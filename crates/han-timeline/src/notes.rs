//! Notes (§23, TL-150 – TL-157): text about a range of a file, kept in the root map under
//! `note:<key>`, attached by anchors so it follows the text as the file changes. A note is
//! data: it never approves, gates or steers (TL-151, TL-155).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::envelope::Signer;
use crate::error::{Error, Reason};
use crate::history::{History, Written};
use crate::ids::{CharId, ObjectId, OpId};
use crate::op::Op;
use crate::state::{Obj, State};
use crate::store::Store;
use crate::text::{Anchor, Bias, TextObj};
use crate::value::Value;

/// The root-key prefix of a note (TL-150).
pub const NOTE_PREFIX: &str = "note:";
/// The root-key prefix of a note's resolved flag (TL-153).
pub const RESOLVED_PREFIX: &str = "note-resolved:";
/// The largest note body, in bytes (TL-150).
pub const MAX_NOTE_BODY: usize = 16_384;
/// The largest note key, in bytes (TL-150).
pub const MAX_NOTE_KEY: usize = 128;
/// The writer limit of notes on one content object (TL-156).
pub const MAX_NOTES_PER_OBJECT: usize = 2_048;
/// The writer limit of notes in one history (TL-156).
pub const MAX_NOTES_PER_HISTORY: usize = 65_536;
/// The largest batch step, in bytes (TL-91).
const MAX_STEP: usize = 256;

/// A note's kind (TL-150). No other kind exists, and neither carries authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NoteKind {
    /// A remark.
    Note,
    /// A question.
    Question,
}

impl NoteKind {
    /// `note` or `question`.
    pub fn as_str(self) -> &'static str {
        match self {
            NoteKind::Note => "note",
            NoteKind::Question => "question",
        }
    }
}

/// Where a note sits at a version (TL-152).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotePlace {
    /// Visible positions `from < to`.
    Placed {
        /// Start position.
        from: usize,
        /// End position (exclusive).
        to: usize,
    },
    /// Both anchors resolve to one position (every covered character may be deleted).
    Collapsed(usize),
    /// The content object does not exist here or is not a text.
    Orphaned,
    /// An anchor names no character of the text (`unknown_anchor`).
    Unresolved,
}

/// What a note says, as written (TL-150).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoteDraft {
    /// The file's content object.
    pub object: OpId,
    /// The start anchor: `start` or a character; never `end`.
    pub from: Anchor,
    /// The end anchor: `end` or a character; never `start`.
    pub to: Anchor,
    /// The kind.
    pub kind: NoteKind,
    /// 1 – 16,384 bytes of UTF-8 without control characters except LF.
    pub body: String,
    /// The note key this one replies to.
    pub reply_to: Option<String>,
}

/// A note read at a version (TL-152 – TL-154).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    /// The note key (the root key without `note:`).
    pub key: String,
    /// The op that wrote the visible value.
    pub writer: OpId,
    /// What the note says.
    pub draft: NoteDraft,
    /// The batch step recorded with it.
    pub step: Option<String>,
    /// Where it sits.
    pub place: NotePlace,
    /// The visible value of its resolved flag; an absent flag is false.
    pub resolved: bool,
    /// The resolved flag has concurrent values.
    pub resolved_conflict: bool,
    /// It replies to a note key that is absent.
    pub parent_missing: bool,
}

/// The notes of a state (TL-150 – TL-154).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Notes {
    /// The notes in ascending key order.
    pub notes: Vec<Note>,
    /// Note keys a reader ignores (TL-151), ascending.
    pub ignored: Vec<String>,
}

// ---------------------------------------------------------------------------------------------
// Encoding

fn no_controls(s: &str, allow_lf: bool) -> bool {
    s.chars()
        .all(|c| !(c < '\u{20}' || c == '\u{7f}') || (allow_lf && c == '\n'))
}

fn valid_key(k: &str) -> bool {
    (1..=MAX_NOTE_KEY).contains(&k.len())
}

fn valid_body(b: &str) -> bool {
    (1..=MAX_NOTE_BODY).contains(&b.len()) && no_controls(b, true)
}

fn anchor_value(a: &Anchor) -> Value {
    match a {
        Anchor::Start => Value::text("start"),
        Anchor::End => Value::text("end"),
        Anchor::Char(c, bias) => Value::Map(BTreeMap::from([
            ("c".into(), Value::text(c.to_string())),
            (
                "b".into(),
                Value::text(match bias {
                    Bias::Left => "left",
                    Bias::Right => "right",
                }),
            ),
        ])),
    }
}

fn parse_anchor(v: &Value) -> Option<Anchor> {
    match v {
        Value::Text(s) if s == "start" => Some(Anchor::Start),
        Value::Text(s) if s == "end" => Some(Anchor::End),
        Value::Map(m) => {
            let c = CharId::parse(m.get("c")?.as_text()?)?;
            let bias = match m.get("b")?.as_text()? {
                "left" => Bias::Left,
                "right" => Bias::Right,
                _ => return None,
            };
            Some(Anchor::Char(c, bias))
        }
        _ => None,
    }
}

impl NoteDraft {
    /// The note's value, with `step` when given (TL-150).
    pub fn value(&self, step: Option<&str>) -> Value {
        let mut m = BTreeMap::from([
            ("object".into(), Value::object_ref(&self.object)),
            ("from".into(), anchor_value(&self.from)),
            ("to".into(), anchor_value(&self.to)),
            ("kind".into(), Value::text(self.kind.as_str())),
            ("body".into(), Value::text(self.body.clone())),
        ]);
        if let Some(r) = &self.reply_to {
            m.insert("reply_to".into(), Value::text(r.clone()));
        }
        if let Some(s) = step {
            m.insert("step".into(), Value::text(s));
        }
        Value::Map(m)
    }

    /// Reads a note value and its step; `None` when a reader ignores it (TL-151), except for
    /// the order of its anchors, which needs the text.
    fn parse(v: &Value) -> Option<(NoteDraft, Option<String>)> {
        let m = v.as_map()?;
        let object = m.get("object")?.obj_field()?;
        let from = parse_anchor(m.get("from")?)?;
        let to = parse_anchor(m.get("to")?)?;
        if from == Anchor::End || to == Anchor::Start {
            return None;
        }
        let kind = match m.get("kind")?.as_text()? {
            "note" => NoteKind::Note,
            "question" => NoteKind::Question,
            _ => return None,
        };
        let body = m.get("body")?.as_text().filter(|b| valid_body(b))?;
        let reply_to = match m.get("reply_to") {
            None => None,
            Some(r) => Some(r.as_text().filter(|r| valid_key(r))?.to_string()),
        };
        let step = match m.get("step") {
            None => None,
            Some(s) => Some(s.as_text().filter(|s| s.len() <= MAX_STEP)?.to_string()),
        };
        Some((
            NoteDraft {
                object,
                from,
                to,
                kind,
                body: body.to_string(),
                reply_to,
            },
            step,
        ))
    }
}

/// Whether `from`'s character comes after `to`'s in the text's order (TL-43, TL-151). Only
/// characters the text holds directly are compared.
fn reversed(t: &TextObj, from: &Anchor, to: &Anchor) -> bool {
    let (Anchor::Char(a, _), Anchor::Char(b, _)) = (from, to) else {
        return false;
    };
    match (t.local(a), t.local(b)) {
        (Some(a), Some(b)) => {
            let lay = t.layout();
            lay.pos(a) > lay.pos(b)
        }
        _ => false,
    }
}

fn place(t: Option<&TextObj>, from: &Anchor, to: &Anchor) -> NotePlace {
    let Some(t) = t else {
        return NotePlace::Orphaned;
    };
    match (t.resolve_anchor(from), t.resolve_anchor(to)) {
        (Some(f), Some(e)) if f < e => NotePlace::Placed { from: f, to: e },
        (Some(f), Some(e)) => NotePlace::Collapsed(f.min(e)),
        _ => NotePlace::Unresolved,
    }
}

// ---------------------------------------------------------------------------------------------
// Reading

impl State {
    /// The notes at this state, resolved (TL-150 – TL-154).
    pub fn notes(&self) -> Notes {
        let mut out = Notes::default();
        let Some(root) = self.map(&ObjectId::Root) else {
            return out;
        };
        let notes = root
            .keys
            .range(NOTE_PREFIX.to_string()..)
            .take_while(|(k, _)| k.starts_with(NOTE_PREFIX));
        for (root_key, values) in notes {
            let key = &root_key[NOTE_PREFIX.len()..];
            let Some((writer, value)) = values.first() else {
                continue;
            };
            let parsed = NoteDraft::parse(value).filter(|_| valid_key(key));
            let text = parsed.as_ref().and_then(|(d, _)| {
                match self.objects.get(&ObjectId::Op(d.object.clone())) {
                    Some(Obj::Text(t)) => Some(&**t),
                    _ => None,
                }
            });
            let parsed = parsed.filter(|(d, _)| !text.is_some_and(|t| reversed(t, &d.from, &d.to)));
            let Some((draft, step)) = parsed else {
                out.ignored.push(key.to_string());
                continue;
            };
            let flag = root.keys.get(&format!("{RESOLVED_PREFIX}{key}"));
            let parent_missing = draft
                .reply_to
                .as_ref()
                .is_some_and(|p| !root.keys.contains_key(&format!("{NOTE_PREFIX}{p}")));
            out.notes.push(Note {
                key: key.to_string(),
                writer: writer.clone(),
                place: place(text, &draft.from, &draft.to),
                draft,
                step,
                resolved: flag.and_then(|v| v.first()).map(|(_, v)| v) == Some(&Value::Bool(true)),
                resolved_conflict: flag.is_some_and(|v| v.len() > 1),
                parent_missing,
            });
        }
        out
    }

    /// Visible note keys and the content object each names.
    fn note_objects(&self) -> impl Iterator<Item = (&str, Option<OpId>)> {
        self.map(&ObjectId::Root).into_iter().flat_map(|root| {
            root.keys
                .range(NOTE_PREFIX.to_string()..)
                .take_while(|(k, _)| k.starts_with(NOTE_PREFIX))
                .filter_map(|(k, v)| {
                    let (_, value) = v.first()?;
                    let object = value.as_map()?.get("object")?.obj_field();
                    Some((&k[NOTE_PREFIX.len()..], object))
                })
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Writing

impl<S: Store> History<S> {
    /// Writes a note on `timeline` under `key`, or under the text form of the writing op's id
    /// when `None` (TL-150), as one `map.set` of the root map. The engine refuses a note a
    /// reader would ignore (`rejected: malformed`, `rejected: too_large`), anchors that name no
    /// character of the text at the head (`unknown_anchor`) or that come in the wrong order
    /// (`out_of_range`), and a note that would exceed 2,048 notes on its content object or
    /// 65,536 in the history (`too_many`, TL-156), counting the notes visible at every
    /// timeline's head.
    pub fn write_note(
        &mut self,
        signer: &dyn Signer,
        lane: u32,
        timeline: &OpId,
        key: Option<&str>,
        draft: &NoteDraft,
        step: Option<String>,
    ) -> Result<Written, Error> {
        let head = self.head(timeline).ok_or(Error::UnknownTimeline)?;
        if draft.from == Anchor::End
            || draft.to == Anchor::Start
            || !no_controls(&draft.body, true)
            || draft.body.is_empty()
            || key.is_some_and(|k| k.is_empty())
            || draft.reply_to.as_ref().is_some_and(|r| r.is_empty())
        {
            return Err(Error::Rejected(Reason::Malformed));
        }
        if draft.body.len() > MAX_NOTE_BODY
            || key.is_some_and(|k| !valid_key(k))
            || draft.reply_to.as_ref().is_some_and(|r| !valid_key(r))
            || step.as_ref().is_some_and(|s| s.len() > MAX_STEP)
        {
            return Err(Error::Rejected(Reason::TooLarge));
        }
        let state = self.state(&crate::history::At::Timeline(timeline.clone()))?;
        match state.objects.get(&ObjectId::Op(draft.object.clone())) {
            None => return Err(Error::UnknownObject),
            Some(Obj::Text(t)) => {
                match (t.resolve_anchor(&draft.from), t.resolve_anchor(&draft.to)) {
                    (Some(f), Some(e)) if f <= e && !reversed(t, &draft.from, &draft.to) => {}
                    (Some(_), Some(_)) => return Err(Error::OutOfRange),
                    _ => return Err(Error::UnknownAnchor),
                }
            }
            Some(_) => {}
        }
        self.check_note_limits(key, &draft.object)?;
        let value_step = step.clone();
        let key = key.map(str::to_string);
        self.write_with(signer, lane, head, Some(timeline.clone()), step, |id| {
            let key = key.unwrap_or_else(|| id.to_string());
            vec![Op::MapSet {
                target: ObjectId::Root,
                key: format!("{NOTE_PREFIX}{key}"),
                value: draft.value(value_step.as_deref()),
            }]
        })
    }

    /// TL-156 over the notes visible at every timeline's head: a new key may not take the
    /// history past 65,536 notes or `object` past 2,048.
    fn check_note_limits(&self, key: Option<&str>, object: &OpId) -> Result<(), Error> {
        let mut all: HashMap<String, BTreeSet<Option<OpId>>> = HashMap::new();
        for t in self.timelines() {
            let state = self.state(&crate::history::At::Timeline(t.id))?;
            for (k, o) in state.note_objects() {
                all.entry(k.to_string()).or_default().insert(o);
            }
        }
        // A key not yet in use (always so for the default key) adds a note.
        let existing = key.and_then(|k| all.get(k));
        let on_object = all
            .values()
            .filter(|objs| objs.contains(&Some(object.clone())))
            .count();
        let adds_to_object = existing.is_none_or(|objs| !objs.contains(&Some(object.clone())));
        if (existing.is_none() && all.len() >= MAX_NOTES_PER_HISTORY)
            || (adds_to_object && on_object >= MAX_NOTES_PER_OBJECT)
        {
            return Err(Error::TooMany);
        }
        Ok(())
    }
}

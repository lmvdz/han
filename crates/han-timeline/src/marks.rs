//! Turn marks and rewind (§22, TL-140 – TL-147): `turn:N` checkpoints on a session's timeline,
//! `turn:N` keys in its transcript history, the turn of a transcript position, and reading or
//! forking a session as it was at the end of a turn. Marks are conventions over checkpoints
//! and a map: they carry no authority, and nothing is fetched because a mark names it.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::envelope::Signer;
use crate::error::Error;
use crate::history::{At, Checkpoint, History, Written};
use crate::ids::{BatchId, CharId, HistoryId, ObjectId, OpId, Version, is_hex64};
use crate::op::Op;
use crate::state::State;
use crate::store::Store;
use crate::text::{Anchor, Bias};
use crate::value::Value;

/// The longest timeline name (TL-20).
const MAX_NAME: usize = 256;

/// The turn number of a `turn:N` label or key: a positive decimal without leading zeros.
fn turn_number(s: &str) -> Option<u64> {
    let n = s.strip_prefix("turn:")?;
    let canonical = !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) && !n.starts_with('0');
    canonical.then(|| n.parse().ok()).flatten()
}

/// A turn mark of a transcript history (TL-142): the root key `turn:N` and its value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnMark {
    /// The turn number `N`.
    pub turn: u64,
    /// `h`: the worktree history.
    pub history: HistoryId,
    /// `t`: the session timeline.
    pub timeline: OpId,
    /// `v`: the checkpoint version.
    pub version: Version,
    /// `end`: the last character of the turn in the transcript.
    pub end: CharId,
}

impl TurnMark {
    /// The transcript root key, `turn:N`.
    pub fn key(&self) -> String {
        format!("turn:{}", self.turn)
    }

    /// The value to write under [`TurnMark::key`].
    pub fn value(&self) -> Value {
        let text = |s: String| Value::Text(s);
        Value::Map(BTreeMap::from([
            ("h".into(), text(self.history.to_hex())),
            ("t".into(), text(self.timeline.to_string())),
            (
                "v".into(),
                Value::Array(
                    self.version
                        .heads()
                        .iter()
                        .map(|h| text(h.to_string()))
                        .collect(),
                ),
            ),
            ("end".into(), text(self.end.to_string())),
        ]))
    }

    /// Reads a mark from its key and value; `None` when either is not of the form of TL-142
    /// (the reader then ignores it, TL-146). Members other than `h`, `t`, `v`, `end` are
    /// ignored.
    pub fn parse(key: &str, value: &Value) -> Option<TurnMark> {
        let turn = turn_number(key)?;
        let m = value.as_map()?;
        let h = m.get("h")?.as_text().filter(|h| is_hex64(h))?;
        let mut history = [0u8; 32];
        for (i, b) in history.iter_mut().enumerate() {
            *b = u8::from_str_radix(&h[2 * i..2 * i + 2], 16).ok()?;
        }
        let timeline = OpId::parse(m.get("t")?.as_text()?)?;
        let Value::Array(v) = m.get("v")? else {
            return None;
        };
        let version = v
            .iter()
            .map(|x| x.as_text().and_then(OpId::parse))
            .collect::<Option<Vec<_>>>()?;
        let end = CharId::parse(m.get("end")?.as_text()?)?;
        Some(TurnMark {
            turn,
            history: BatchId(history),
            timeline,
            version: Version::new(version),
            end,
        })
    }
}

/// The turn marks of a transcript state (TL-142, TL-146).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TranscriptMarks {
    /// The marks that parse, by turn number.
    pub marks: BTreeMap<u64, TurnMark>,
    /// Root keys beginning `turn:` that are not marks of the form of TL-142, ascending.
    pub ignored: Vec<String>,
    /// Marks whose `end` names no character of any text in the state (TL-146): unresolved.
    pub unresolved: Vec<u64>,
}

/// The turn a transcript position belongs to (TL-143).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnOf {
    /// The turn number.
    pub turn: u64,
    /// The turn in progress: after the last mark, which has no mark yet.
    pub in_progress: bool,
}

impl State {
    /// The turn marks of a transcript history's root map at this state (TL-142, TL-146).
    pub fn transcript_marks(&self) -> TranscriptMarks {
        let mut out = TranscriptMarks::default();
        let Some(root) = self.map(&ObjectId::Root) else {
            return out;
        };
        for (key, values) in root.keys.range("turn:".to_string()..) {
            if !key.starts_with("turn:") {
                break;
            }
            let Some((_, value)) = values.first() else {
                continue;
            };
            match TurnMark::parse(key, value) {
                Some(mark) => {
                    if !self.holds_char(&mark.end) {
                        out.unresolved.push(mark.turn);
                    }
                    out.marks.insert(mark.turn, mark);
                }
                None => out.ignored.push(key.clone()),
            }
        }
        out.unresolved.sort_unstable();
        out
    }

    /// Whether some text of this state holds character `c`, visible or not.
    fn holds_char(&self, c: &CharId) -> bool {
        self.objects.values().any(|o| match o {
            crate::state::Obj::Text(t) => t.has_char(c),
            _ => false,
        })
    }

    /// The turn of visible character `pos` of transcript text `text` (TL-143): the least
    /// marked turn `n` with `pos < e(n)`, else the turn in progress. Marks whose `end` this
    /// text does not hold are skipped. `None` when `text` is not a text or `pos` is past its
    /// end.
    pub fn turn_of(&self, text: &ObjectId, pos: usize) -> Option<TurnOf> {
        let t = self.text_obj(text)?;
        if pos >= t.len() {
            return None;
        }
        let mut last = 0;
        for (n, mark) in self.transcript_marks().marks {
            let Some(end) = t.resolve_anchor(&Anchor::Char(mark.end, Bias::Left)) else {
                continue;
            };
            if pos < end {
                return Some(TurnOf {
                    turn: n,
                    in_progress: false,
                });
            }
            last = n;
        }
        Some(TurnOf {
            turn: last + 1,
            in_progress: true,
        })
    }
}

impl<S: Store> History<S> {
    /// The turn marks of a timeline (TL-141): its checkpoints labelled `turn:N`, by turn; for
    /// a label used more than once, the checkpoint with the greatest op id. Only checkpoints
    /// on the timeline itself count: a fork has none of its source's marks.
    pub fn turn_marks(&self, timeline: &OpId) -> BTreeMap<u64, Checkpoint> {
        let mut out = BTreeMap::new();
        // Checkpoints come in op id order, so a later one replaces an earlier one.
        for cp in self.checkpoints(timeline) {
            if let Some(n) = turn_number(&cp.label) {
                out.insert(n, cp);
            }
        }
        out
    }

    /// The mark of turn `n` on a timeline (TL-141).
    pub fn turn_mark(&self, timeline: &OpId, n: u64) -> Option<Checkpoint> {
        self.turn_marks(timeline).remove(&n)
    }

    /// The session as it was at the end of turn `n` (TL-144): the state at the mark's
    /// version, a read that changes nothing. Errors: `unknown_mark`.
    pub fn rewind(&self, timeline: &OpId, n: u64) -> Result<Arc<State>, Error> {
        let mark = self.turn_mark(timeline, n).ok_or(Error::UnknownMark)?;
        self.state(&At::Version(mark.version))
    }

    /// Continues from turn `n` (TL-145): forks `timeline` at the mark's version into a new
    /// timeline named `rewind:<n>:<timeline name>`, cut at a scalar boundary to fit TL-20. The
    /// original timeline is untouched. Errors: `unknown_timeline`, `unknown_mark`.
    pub fn rewind_fork(
        &mut self,
        signer: &dyn Signer,
        lane: u32,
        timeline: &OpId,
        n: u64,
    ) -> Result<Written, Error> {
        let source = self
            .timelines()
            .into_iter()
            .find(|t| t.id == *timeline)
            .ok_or(Error::UnknownTimeline)?;
        let mark = self.turn_mark(timeline, n).ok_or(Error::UnknownMark)?;
        let mut name = format!("rewind:{n}:{}", source.name);
        if name.len() > MAX_NAME {
            let cut = (0..=MAX_NAME)
                .rev()
                .find(|&i| name.is_char_boundary(i))
                .unwrap_or(0);
            name.truncate(cut);
        }
        self.fork(signer, lane, timeline, Some(&mark.version), &name)
    }

    /// Records the mark of turn `n` on `timeline` (TL-141): a checkpoint labelled `turn:<n>`
    /// at its head, to write once every mirror batch of the turn is recorded. `n` starts at 1.
    pub fn mark_turn(
        &mut self,
        signer: &dyn Signer,
        lane: u32,
        timeline: &OpId,
        n: u64,
        step: Option<String>,
    ) -> Result<Written, Error> {
        if n == 0 {
            return Err(Error::OutOfRange);
        }
        let label = format!("turn:{n}");
        self.write(signer, lane, timeline, vec![Op::Checkpoint { label }], step)
    }

    /// Whether this history resolves a transcript mark (TL-146): it is the mark's worktree
    /// history and holds its timeline and every op of its version. A mark that does not
    /// resolve is unresolved, not an error; nothing is fetched because of it.
    pub fn resolves_mark(&self, mark: &TurnMark) -> bool {
        mark.history == self.id()
            && self.head(&mark.timeline).is_some()
            && self.positions(&mark.version).is_ok()
    }
}

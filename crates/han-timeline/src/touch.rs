//! Touch sets and overlap (§21, TL-130 – TL-138): what a timeline changed over a base, and
//! where two timelines meet. Both are pure functions of the held ops: they read only states,
//! write nothing and keep no index (TL-123, TL-135). Overlap is built from the merge planner's
//! own output, so its conflicting entries are the plan's conflicts by construction (TL-134).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::{BitOr, BitOrAssign};
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::cbor::Cbor;
use crate::codec::full_id;
use crate::error::Error;
use crate::history::History;
use crate::ids::{CharId, ObjectId, OpId, Version};
use crate::merge::{Conflict, Lines, Side, clusters, edits_of, region_chars, sequence};
use crate::op::{HistoryKind, Op};
use crate::state::{Obj, State};
use crate::store::Store;
use crate::text::TextObj;
use crate::value::Value;

/// At most this many entries in a touch set (TL-137).
pub const MAX_TOUCH_ENTRIES: usize = 4096;
/// At most this many line ranges per list of a touch-set entry (TL-137).
pub const MAX_TOUCH_RANGES: usize = 1024;

/// What changed about a content object (TL-130): a set of flags whose bits are the digest's
/// `change bits` (TL-136).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TouchChange(u8);

impl TouchChange {
    /// Absent at the base, present at the head.
    pub const CREATED: TouchChange = TouchChange(1);
    /// Present at the base, absent at the head.
    pub const DELETED: TouchChange = TouchChange(2);
    /// Present at both with different path sets.
    pub const RENAMED: TouchChange = TouchChange(4);
    /// A path's mode differs.
    pub const MODE: TouchChange = TouchChange(8);
    /// A path's line-ending attribute differs.
    pub const EOL: TouchChange = TouchChange(16);
    /// A text file with at least one identity-based edit.
    pub const EDITED: TouchChange = TouchChange(32);
    /// A register content object whose value differs.
    pub const BINARY_CHANGED: TouchChange = TouchChange(64);

    const NAMES: [(TouchChange, &'static str); 7] = [
        (Self::CREATED, "created"),
        (Self::DELETED, "deleted"),
        (Self::RENAMED, "renamed"),
        (Self::MODE, "mode"),
        (Self::EOL, "eol"),
        (Self::EDITED, "edited"),
        (Self::BINARY_CHANGED, "binary_changed"),
    ];

    /// The digest's `change bits`.
    pub fn bits(self) -> u8 {
        self.0
    }

    /// Whether every flag of `other` is set.
    pub fn contains(self, other: TouchChange) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether no flag is set.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The names of the set flags, in the order `created, deleted, renamed, mode, eol, edited,
    /// binary_changed`.
    pub fn names(self) -> Vec<&'static str> {
        Self::NAMES
            .iter()
            .filter(|(c, _)| self.contains(*c))
            .map(|(_, n)| *n)
            .collect()
    }
}

impl BitOr for TouchChange {
    type Output = TouchChange;
    fn bitor(self, rhs: TouchChange) -> TouchChange {
        TouchChange(self.0 | rhs.0)
    }
}

impl BitOrAssign for TouchChange {
    fn bitor_assign(&mut self, rhs: TouchChange) {
        self.0 |= rhs.0;
    }
}

/// One content object a timeline changed (TL-130).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TouchEntry {
    /// The content object.
    pub object: OpId,
    /// The least path referring to it at the base, if present there.
    pub base_path: Option<String>,
    /// The least path referring to it at the head, if present there.
    pub head_path: Option<String>,
    /// What changed.
    pub change: TouchChange,
    /// Base line ranges `[a, b)` of its clusters (TL-131), ascending.
    pub base_lines: Vec<(u64, u64)>,
    /// Head line ranges `[a', a' + k)` of its clusters (TL-131), ascending.
    pub head_lines: Vec<(u64, u64)>,
}

/// The touch set of a timeline over a base (TL-130).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TouchSet {
    /// The base version.
    pub base: Version,
    /// The entries in ascending object id order.
    pub entries: Vec<TouchEntry>,
    /// Cut at a reader limit (TL-137): an absent entry is unknown, not untouched.
    pub truncated: bool,
}

impl TouchSet {
    /// The touch digest (TL-136).
    pub fn digest(&self) -> [u8; 32] {
        let ranges = |rs: &[(u64, u64)]| {
            Cbor::Array(
                rs.iter()
                    .map(|(a, b)| Cbor::Array(vec![Cbor::u(*a), Cbor::u(*b)]))
                    .collect(),
            )
        };
        let path = |p: &Option<String>| p.as_ref().map_or(Cbor::Null, |p| Cbor::text(p.clone()));
        let entries = self
            .entries
            .iter()
            .map(|e| {
                Cbor::Array(vec![
                    full_id(&e.object),
                    path(&e.base_path),
                    path(&e.head_path),
                    Cbor::u(e.change.bits().into()),
                    ranges(&e.base_lines),
                    ranges(&e.head_lines),
                ])
            })
            .collect();
        digest(b"han-timeline-touch-v1\n", Cbor::Array(entries))
    }
}

/// A side of an overlap: the first or the second timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OverlapSide {
    /// The first timeline (the merge target of TL-134).
    A,
    /// The second timeline (the incoming side of TL-134).
    B,
}

/// One place where two timelines meet (TL-133).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OverlapEntry {
    /// A cluster both sides edit in one text; `converged` when the region texts are equal.
    Region {
        /// The text.
        object: ObjectId,
        /// The cluster's base line range `[a, b)`.
        lines: (u64, u64),
        /// Both sides made the region read the same.
        converged: bool,
    },
    /// Both sides touch the content object and nothing else applies.
    File {
        /// The content object.
        object: ObjectId,
    },
    /// One side removed a path whose content the other side changed.
    ModifyDelete {
        /// The content object the path referred to at the base.
        object: ObjectId,
        /// The path.
        path: String,
        /// The side that removed it.
        deleted_by: OverlapSide,
    },
    /// The content object would have more paths than on either side.
    Rename {
        /// The content object.
        object: ObjectId,
        /// Its paths at head `A`, ascending.
        a: Vec<String>,
        /// Its paths at head `B`, ascending.
        b: Vec<String>,
    },
    /// A path clash neither side had.
    Path {
        /// The tree map.
        object: ObjectId,
        /// `p` and the path under `p/`.
        paths: (String, String),
    },
    /// A map key all three versions disagree on.
    MapKey {
        /// The map.
        object: ObjectId,
        /// The key.
        key: String,
    },
    /// A register all three versions disagree on.
    Register {
        /// The register.
        object: ObjectId,
    },
}

impl OverlapEntry {
    /// The object the entry is about.
    pub fn object(&self) -> &ObjectId {
        match self {
            OverlapEntry::Region { object, .. }
            | OverlapEntry::File { object }
            | OverlapEntry::ModifyDelete { object, .. }
            | OverlapEntry::Rename { object, .. }
            | OverlapEntry::Path { object, .. }
            | OverlapEntry::MapKey { object, .. }
            | OverlapEntry::Register { object } => object,
        }
    }

    /// The class name of TL-133.
    pub fn class(&self) -> &'static str {
        match self {
            OverlapEntry::Region { .. } => "region",
            OverlapEntry::File { .. } => "file",
            OverlapEntry::ModifyDelete { .. } => "modify_delete",
            OverlapEntry::Rename { .. } => "rename",
            OverlapEntry::Path { .. } => "path",
            OverlapEntry::MapKey { .. } => "map_key",
            OverlapEntry::Register { .. } => "register",
        }
    }

    /// Whether this entry is a conflict of the merge plan (TL-134).
    pub fn is_conflict(&self) -> bool {
        !matches!(
            self,
            OverlapEntry::File { .. }
                | OverlapEntry::Region {
                    converged: true,
                    ..
                }
        )
    }

    /// The class code of TL-136.
    fn code(&self) -> u64 {
        match self {
            OverlapEntry::Region { .. } => 1,
            OverlapEntry::File { .. } => 2,
            OverlapEntry::ModifyDelete { .. } => 3,
            OverlapEntry::Rename { .. } => 4,
            OverlapEntry::Path { .. } => 5,
            OverlapEntry::MapKey { .. } => 6,
            OverlapEntry::Register { .. } => 7,
        }
    }

    /// The sort key of TL-133: object, class order, then lines, key or path.
    fn order(&self) -> (&ObjectId, u8, (u64, u64), &str) {
        let none = (0, 0);
        match self {
            OverlapEntry::Region { object, lines, .. } => (object, 0, *lines, ""),
            OverlapEntry::ModifyDelete { object, path, .. } => (object, 1, none, path),
            OverlapEntry::Rename { object, .. } => (object, 2, none, ""),
            OverlapEntry::File { object } => (object, 3, none, ""),
            OverlapEntry::Path { object, paths } => (object, 4, none, &paths.0),
            OverlapEntry::MapKey { object, key } => (object, 5, none, key),
            OverlapEntry::Register { object } => (object, 6, none, ""),
        }
    }

    fn to_cbor(&self) -> Cbor {
        let texts = |v: &[String]| Cbor::Array(v.iter().map(|s| Cbor::text(s.clone())).collect());
        let (converged, detail) = match self {
            OverlapEntry::Region {
                lines, converged, ..
            } => (
                Cbor::Bool(*converged),
                Cbor::Array(vec![Cbor::u(lines.0), Cbor::u(lines.1)]),
            ),
            OverlapEntry::ModifyDelete {
                path, deleted_by, ..
            } => (
                Cbor::Null,
                Cbor::Array(vec![
                    Cbor::text(path.clone()),
                    Cbor::u(match deleted_by {
                        OverlapSide::A => 1,
                        OverlapSide::B => 2,
                    }),
                ]),
            ),
            OverlapEntry::Rename { a, b, .. } => {
                (Cbor::Null, Cbor::Array(vec![texts(a), texts(b)]))
            }
            OverlapEntry::Path { paths, .. } => (
                Cbor::Null,
                Cbor::Array(vec![
                    Cbor::text(paths.0.clone()),
                    Cbor::text(paths.1.clone()),
                ]),
            ),
            OverlapEntry::MapKey { key, .. } => {
                (Cbor::Null, Cbor::Array(vec![Cbor::text(key.clone())]))
            }
            OverlapEntry::File { .. } | OverlapEntry::Register { .. } => {
                (Cbor::Null, Cbor::Array(Vec::new()))
            }
        };
        Cbor::Array(vec![
            object_cbor(self.object()),
            Cbor::u(self.code()),
            converged,
            detail,
        ])
    }
}

/// The overlap of two timelines (TL-133).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overlap {
    /// Their merge base (TL-70).
    pub base: Version,
    /// The entries in the order of TL-133.
    pub entries: Vec<OverlapEntry>,
    /// Computed from a truncated touch set (TL-137): an absent entry is unknown, not disjoint.
    pub truncated: bool,
}

impl Overlap {
    /// Whether the two timelines are disjoint: no entry, and nothing was cut (TL-137).
    pub fn is_disjoint(&self) -> bool {
        self.entries.is_empty() && !self.truncated
    }

    /// The overlap digest (TL-136).
    pub fn digest(&self) -> [u8; 32] {
        let entries = self.entries.iter().map(OverlapEntry::to_cbor).collect();
        digest(b"han-timeline-overlap-v1\n", Cbor::Array(entries))
    }
}

fn digest(prefix: &[u8], body: Cbor) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(prefix);
    h.update(body.encode());
    h.finalize().into()
}

fn object_cbor(o: &ObjectId) -> Cbor {
    match o {
        ObjectId::Root => Cbor::Null,
        ObjectId::Op(id) => full_id(id),
    }
}

// ---------------------------------------------------------------------------------------------
// Computing touch sets

/// Per content object, the tree paths that refer to it in `state` and their entries.
fn tree_refs(state: &State) -> BTreeMap<OpId, BTreeMap<&str, &Value>> {
    let mut out: BTreeMap<OpId, BTreeMap<&str, &Value>> = BTreeMap::new();
    for obj in state.objects.values() {
        let Obj::Map(m) = obj else { continue };
        if !m.tree {
            continue;
        }
        for (path, values) in &m.keys {
            if let Some((_, entry)) = values.first()
                && let Some(id) = entry.obj_field()
            {
                out.entry(id).or_default().insert(path, entry);
            }
        }
    }
    out
}

/// The number of lines of a text split after each LF (the last may lack one).
fn line_count(chars: impl Iterator<Item = char>) -> u64 {
    let (mut n, mut open) = (0, false);
    for c in chars {
        if c == '\n' {
            n += 1;
            open = false;
        } else {
            open = true;
        }
    }
    n + u64::from(open)
}

fn whole(n: u64) -> Vec<(u64, u64)> {
    if n == 0 { Vec::new() } else { vec![(0, n)] }
}

/// The clusters of `head`'s identity-based edits against `base` (TL-131): base and head line
/// ranges, or `None` when there is no edit.
#[allow(clippy::type_complexity)]
fn text_ranges(base: &TextObj, head: &TextObj) -> Option<(Vec<(u64, u64)>, Vec<(u64, u64)>)> {
    let bseq = sequence(Some(base));
    let hseq = sequence(Some(head));
    let base_index: HashMap<&CharId, usize> = bseq
        .iter()
        .enumerate()
        .map(|(i, e)| (&e.canon, i))
        .collect();
    let lines = Lines::of(&bseq);
    let line = |i: usize| lines.line(i);
    let base_char = |i: usize| bseq[i].ch;
    let (edits, bx) = edits_of(Side::Target, &bseq, &base_index, &hseq, &line, &base_char);
    if edits.is_empty() {
        return None;
    }
    // LFs in the head before each head position.
    let mut lf = Vec::with_capacity(hseq.len() + 1);
    lf.push(0u64);
    for e in &hseq {
        lf.push(lf.last().copied().unwrap_or(0) + u64::from(e.ch == '\n'));
    }
    let mut base_lines = Vec::new();
    let mut head_lines = Vec::new();
    for (a, b, members) in clusters(&edits) {
        base_lines.push((a as u64, b as u64));
        let region = region_chars(&edits, &bx, Side::Target, &lines, (a, b), &members);
        let k = line_count(region.iter().map(|&x| hseq[x].ch));
        let start = match region.first() {
            Some(&x) => lf[x],
            // The gap right after the last kept head character in a base line before `a`.
            None => (0..lines.start(a))
                .rev()
                .find_map(|bi| bx[bi])
                .map_or(0, |x| lf[x + 1]),
        };
        head_lines.push((start, start + k));
    }
    base_lines.sort_unstable();
    head_lines.sort_unstable();
    Some((base_lines, head_lines))
}

/// The attributes of a file entry.
fn attr<'a>(entry: &'a Value, name: &str) -> Option<&'a Value> {
    entry.as_map()?.get(name)
}

/// The touch set of `head` over `base` (TL-130, TL-131), cut at the reader limits (TL-137).
pub(crate) fn touch(base: &State, head: &State) -> (Vec<TouchEntry>, bool) {
    let in_base = tree_refs(base);
    let in_head = tree_refs(head);
    let ids: BTreeSet<&OpId> = in_base.keys().chain(in_head.keys()).collect();
    let mut entries = Vec::new();
    let mut truncated = false;
    for id in ids {
        let object = ObjectId::Op(id.clone());
        let (bo, ho) = (base.objects.get(&object), head.objects.get(&object));
        if [bo, ho]
            .iter()
            .any(|o| matches!(o, Some(Obj::Text(t)) if t.transcript))
        {
            continue; // never in a touch set (TL-130)
        }
        let (pb, ph) = (in_base.get(id), in_head.get(id));
        let mut change = TouchChange::default();
        let mut base_lines = Vec::new();
        let mut head_lines = Vec::new();
        match (pb, ph) {
            (None, Some(_)) => {
                change |= TouchChange::CREATED;
                if let Some(Obj::Text(t)) = ho {
                    head_lines = whole(line_count(t.text().chars()));
                }
            }
            (Some(_), None) => {
                change |= TouchChange::DELETED;
                if let Some(Obj::Text(t)) = bo {
                    base_lines = whole(line_count(t.text().chars()));
                }
            }
            (Some(pb), Some(ph)) => {
                if !pb.keys().eq(ph.keys()) {
                    change |= TouchChange::RENAMED;
                }
                // Attributes of the paths at both ends; with none in common, of the least paths.
                let mut pairs: Vec<(&Value, &Value)> = pb
                    .iter()
                    .filter_map(|(p, eb)| ph.get(p).map(|eh| (*eb, *eh)))
                    .collect();
                if pairs.is_empty()
                    && let (Some(eb), Some(eh)) = (pb.values().next(), ph.values().next())
                {
                    pairs.push((eb, eh));
                }
                for (eb, eh) in pairs {
                    if attr(eb, "mode") != attr(eh, "mode") {
                        change |= TouchChange::MODE;
                    }
                    if attr(eb, "eol") != attr(eh, "eol") {
                        change |= TouchChange::EOL;
                    }
                }
                match (bo, ho) {
                    (Some(Obj::Text(tb)), Some(Obj::Text(th))) if !Arc::ptr_eq(tb, th) => {
                        if let Some((b, h)) = text_ranges(tb, th) {
                            change |= TouchChange::EDITED;
                            (base_lines, head_lines) = (b, h);
                        }
                    }
                    (Some(Obj::Register(rb)), Some(Obj::Register(rh))) if rb.value != rh.value => {
                        change |= TouchChange::BINARY_CHANGED;
                    }
                    _ => {}
                }
            }
            (None, None) => {}
        }
        if change.is_empty() {
            continue;
        }
        if entries.len() == MAX_TOUCH_ENTRIES {
            truncated = true;
            break;
        }
        for list in [&mut base_lines, &mut head_lines] {
            if list.len() > MAX_TOUCH_RANGES {
                list.truncate(MAX_TOUCH_RANGES);
                truncated = true;
            }
        }
        let least = |p: Option<&BTreeMap<&str, &Value>>| {
            p.and_then(|p| p.keys().next()).map(|s| s.to_string())
        };
        entries.push(TouchEntry {
            object: id.clone(),
            base_path: least(pb),
            head_path: least(ph),
            change,
            base_lines,
            head_lines,
        });
    }
    (entries, truncated)
}

// ---------------------------------------------------------------------------------------------
// The history's view

impl<S: Store> History<S> {
    /// The touch set of `timeline` over `base`, or over its default base (TL-132) when `None`.
    /// `Ok(None)` when the history is not a worktree history (TL-138). Errors:
    /// `unknown_timeline`, `unknown_op`, and `needs_base` for main without a base.
    pub fn touch_set(
        &self,
        timeline: &OpId,
        base: Option<&Version>,
    ) -> Result<Option<TouchSet>, Error> {
        let head = self.graph.head(timeline).ok_or(Error::UnknownTimeline)?;
        if self.kind != Some(HistoryKind::Worktree) {
            return Ok(None);
        }
        let base = match base {
            Some(v) => self.positions(v)?,
            None => self.default_base(timeline)?,
        };
        let (closure, sb) = self.materialise(&base);
        let sh = self.materialise(head).1;
        let (entries, truncated) = touch(&sb, &sh);
        Ok(Some(TouchSet {
            base: self.graph.version_of(&self.graph.frontier(&closure)),
            entries,
            truncated,
        }))
    }

    /// The default base of a timeline (TL-132): its fork version or, once it has merged other
    /// work, its merge base with its source.
    fn default_base(&self, timeline: &OpId) -> Result<Vec<crate::graph::Pos>, Error> {
        let create = self.graph.locate(timeline).ok_or(Error::UnknownTimeline)?;
        let Op::TimelineCreate {
            source: Some(source),
            ..
        } = self.graph.op(create)
        else {
            return Err(Error::NeedsBase);
        };
        let merged = self
            .graph
            .nodes
            .iter()
            .any(|n| n.timeline == *timeline && !n.merges.is_empty());
        match (merged, self.graph.head(source)) {
            (true, Some(sh)) => {
                let head = self.graph.head(timeline).ok_or(Error::UnknownTimeline)?;
                Ok(self.merge_base(head, sh))
            }
            _ => Ok(self.graph.op_parents(create)),
        }
    }

    /// The overlap of timelines `a` and `b` (TL-133): the conflicts of the plan that merges `b`
    /// into `a` (TL-134), its converged clusters, and in a worktree history the content objects
    /// both touch over the merge base. When that merge is up to date but the merge of `a` into
    /// `b` can be planned, that plan is read with the sides swapped, so that TL-134 holds for
    /// the pair in whichever order it is asked. Errors: `merge_self`, `unknown_timeline`.
    pub fn overlap(&self, a: &OpId, b: &OpId) -> Result<Overlap, Error> {
        let (planned, swapped) = match self.planned_merge(a, b)? {
            Some(p) => (p, false),
            None => match self.planned_merge(b, a)? {
                Some(p) => (p, true),
                None => (self.plan_core(a, b)?, false),
            },
        };
        let side = |s: Side| {
            if (s == Side::Target) != swapped {
                OverlapSide::A
            } else {
                OverlapSide::B
            }
        };
        let mut entries: Vec<OverlapEntry> = planned
            .plan
            .conflicts
            .iter()
            .map(|c| match c {
                Conflict::Text(h) => OverlapEntry::Region {
                    object: h.object.clone(),
                    lines: h.lines,
                    converged: false,
                },
                Conflict::ModifyDelete {
                    object,
                    key,
                    deleted_by,
                } => OverlapEntry::ModifyDelete {
                    object: planned
                        .base
                        .map_value(object, key)
                        .and_then(Value::obj_field)
                        .map_or_else(|| object.clone(), ObjectId::Op),
                    path: key.clone(),
                    deleted_by: side(*deleted_by),
                },
                Conflict::Rename {
                    content,
                    target,
                    incoming,
                    ..
                } => {
                    let (a, b) = if swapped {
                        (incoming, target)
                    } else {
                        (target, incoming)
                    };
                    OverlapEntry::Rename {
                        object: ObjectId::Op(content.clone()),
                        a: a.clone(),
                        b: b.clone(),
                    }
                }
                Conflict::Path { object, paths } => OverlapEntry::Path {
                    object: object.clone(),
                    paths: paths.clone(),
                },
                Conflict::Map { object, key, .. } => OverlapEntry::MapKey {
                    object: object.clone(),
                    key: key.clone(),
                },
                Conflict::Register { object, .. } => OverlapEntry::Register {
                    object: object.clone(),
                },
            })
            .collect();
        entries.extend(planned.meets.iter().filter(|m| m.converged).map(|m| {
            OverlapEntry::Region {
                object: m.object.clone(),
                lines: m.lines,
                converged: true,
            }
        }));
        let mut truncated = false;
        if self.kind == Some(HistoryKind::Worktree) {
            let sa = self.materialise(&planned.target_heads).1;
            let sb = self.materialise(&planned.incoming_heads).1;
            let (ta, cut_a) = touch(&planned.base, &sa);
            let (tb, cut_b) = touch(&planned.base, &sb);
            truncated = cut_a || cut_b;
            let named: BTreeSet<ObjectId> = entries.iter().map(|e| e.object().clone()).collect();
            let in_b: BTreeSet<&OpId> = tb.iter().map(|e| &e.object).collect();
            entries.extend(
                ta.iter()
                    .map(|e| ObjectId::Op(e.object.clone()))
                    .filter(|o| {
                        matches!(o, ObjectId::Op(id) if in_b.contains(id)) && !named.contains(o)
                    })
                    .map(|object| OverlapEntry::File { object }),
            );
        }
        entries.sort_by(|x, y| x.order().cmp(&y.order()));
        Ok(Overlap {
            base: planned.plan.base,
            entries,
            truncated,
        })
    }
}

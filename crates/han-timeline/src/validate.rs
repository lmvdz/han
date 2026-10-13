//! TL-101 steps 6, 8 and 9: duplicates, causality, genesis, timelines, and every op judged at
//! its parent (TL-17).

use std::collections::HashSet;

use crate::codec::MAX_KEY;
use crate::error::Reason;
use crate::graph::{Closure, Pos};
use crate::history::{Fail, History, Pending};
use crate::ids::{CharId, IdRange, InsertionId, MAX_SAFE, ObjectId, OpId};
use crate::op::{Batch, Change, HistoryKind, MergeOp, Op};
use crate::state::{Seen, State};
use crate::store::Store;
use crate::text::TextObj;
use crate::value::{Value, value_op_id};

/// What validation hands to integration: the closure and state after the batch, the resolved
/// parents, the batch's timeline, and its merges' incoming heads.
pub(crate) type Validated = (Closure, State, Vec<Pos>, OpId, Vec<(u32, Vec<Pos>)>);

const INVALID: Reason = Reason::InvalidOp;

fn ensure(ok: bool, r: Reason) -> Result<(), Reason> {
    if ok { Ok(()) } else { Err(r) }
}

/// A git-style path (TL-82).
pub(crate) fn is_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= crate::codec::MAX_PATH
        && !p.contains('\0')
        && p.split('/').all(|seg| {
            !seg.is_empty() && seg != "." && seg != ".." && !seg.eq_ignore_ascii_case(".git")
        })
}

/// A file entry whose content object exists with the kind its mode requires (TL-83).
pub(crate) fn is_file_entry(v: &Value, state: &State) -> bool {
    let Value::Map(m) = v else { return false };
    if m.keys()
        .any(|k| !matches!(k.as_str(), "obj" | "mode" | "eol"))
    {
        return false;
    }
    let Some(obj) = m.get("obj").and_then(value_op_id) else {
        return false;
    };
    let Some(mode) = m.get("mode").and_then(Value::as_text) else {
        return false;
    };
    let eol = match m.get("eol") {
        None => None,
        Some(Value::Text(e)) if e == "lf" || e == "crlf" => Some(e),
        Some(_) => return false,
    };
    let obj = ObjectId::Op(obj);
    let text = state.text_obj(&obj);
    let register = state.register(&obj).is_some();
    match mode {
        "100644" | "100755" => match text {
            Some(t) => !t.transcript && eol.is_some(),
            None => register && eol.is_none(),
        },
        "120000" | "160000" => register && eol.is_none(),
        _ => false,
    }
}

fn check_key(tree: bool, key: &str) -> Result<(), Reason> {
    if !tree && key.len() > MAX_KEY {
        return Err(Reason::TooLarge);
    }
    Ok(())
}

/// TL-44, TL-48 for one edit (or one text change of a merge, whose inserts start at index
/// `first`).
fn check_edit(
    t: &TextObj,
    op: &OpId,
    deletes: &[IdRange],
    inserts: &[(Option<&CharId>, &str)],
    first: u32,
    allow_empty: bool,
) -> Result<(), Reason> {
    ensure(
        allow_empty || !deletes.is_empty() || !inserts.is_empty(),
        INVALID,
    )?;
    let mut sorted: Vec<&IdRange> = deletes.iter().collect();
    sorted.sort();
    for r in &sorted {
        ensure(t.range_valid(r), INVALID)?;
    }
    for w in sorted.windows(2) {
        ensure(
            w[0].insertion != w[1].insertion || w[0].to <= w[1].from,
            INVALID,
        )?;
    }
    let mut anchors = HashSet::new();
    for (n, (anchor, text)) in inserts.iter().enumerate() {
        ensure(!text.is_empty() && !text.contains('\r'), INVALID)?;
        ensure(anchors.insert(*anchor), INVALID)?;
        if let Some(c) = anchor
            && !t.has_char(c)
        {
            // An earlier insert of the same op (and the same change).
            let i = c.insertion.index;
            let earlier = c.insertion.op == *op
                && i >= first
                && i < first + n as u32
                && (c.offset as usize) < inserts[(i - first) as usize].1.chars().count();
            ensure(earlier, INVALID)?;
        }
    }
    Ok(())
}

/// TL-51.
fn check_seal(t: &TextObj, ranges: &[IdRange]) -> Result<(), Reason> {
    ensure(t.transcript && !ranges.is_empty(), INVALID)?;
    let lay = t.layout();
    let mut seen = HashSet::new();
    let mut prev_last: Option<usize> = None;
    for r in ranges {
        ensure(t.range_valid(r), INVALID)?;
        let mut first = None;
        let mut last = 0;
        for k in r.from..r.to {
            let c = CharId {
                insertion: r.insertion.clone(),
                offset: k,
            };
            let (l, k) = t.local(&c).ok_or(INVALID)?;
            let i = lay.pos[l as usize][k as usize];
            ensure(lay.visible[i] && seen.insert(i), INVALID)?;
            first.get_or_insert(i);
            last = i;
        }
        let first = first.ok_or(INVALID)?;
        // In document order.
        ensure(prev_last.is_none_or(|p| p < first), INVALID)?;
        prev_last = Some(last);
    }
    let lo = *seen.iter().min().ok_or(INVALID)?;
    let hi = *seen.iter().max().ok_or(INVALID)?;
    let visible_between = lay.visible_index.partition_point(|&x| x <= hi)
        - lay.visible_index.partition_point(|&x| x < lo);
    ensure(visible_between == seen.len(), INVALID)
}

struct InsertionInfo<'a> {
    object: ObjectId,
    text: &'a str,
    is_copy: bool,
}

fn insertion_in_op<'a>(op: &'a Op, op_id: &OpId, index: u32) -> Option<InsertionInfo<'a>> {
    match op {
        Op::TextCreate { text, .. } if index == 0 && !text.is_empty() => Some(InsertionInfo {
            object: ObjectId::Op(op_id.clone()),
            text,
            is_copy: false,
        }),
        Op::TextEdit {
            target, inserts, ..
        } => inserts.get(index as usize).map(|i| InsertionInfo {
            object: target.clone(),
            text: &i.text,
            is_copy: false,
        }),
        Op::Merge(m) => m
            .changes
            .iter()
            .filter_map(|c| match c {
                Change::Text {
                    object, inserts, ..
                } => Some(inserts.iter().map(move |i| (object, i))),
                _ => None,
            })
            .flatten()
            .nth(index as usize)
            .map(|(object, i)| InsertionInfo {
                object: object.clone(),
                text: &i.text,
                is_copy: i.origin.is_some(),
            }),
        _ => None,
    }
}

impl<S: Store> History<S> {
    /// Validates a batch whose dependencies are held.
    pub(crate) fn validate(&self, p: &Pending) -> Result<Validated, Fail> {
        let b = &p.batch;
        self.check_collisions(b, &p.raw)?;
        self.check_causality(b)?;
        let genesis = b.parents.is_root();
        let kind = self.check_genesis(p)?;
        let parents: Vec<Pos> = b
            .parents
            .heads()
            .iter()
            .map(|h| self.graph.locate(h).ok_or(Reason::Timeline))
            .collect::<Result<_, _>>()?;
        let timeline = self.check_timeline(b, &parents, genesis, kind)?;

        let (closure, base) = if genesis {
            (Closure(Vec::new()), std::sync::Arc::new(State::default()))
        } else {
            self.materialise(&parents)
        };
        let mut state = (*base).clone();
        let mut merges = Vec::new();
        for (i, op) in b.ops.iter().enumerate() {
            let id = b.op_id(i);
            self.check_op(&state, b, i, &id, kind)?;
            state.apply(&id, op, &Seen::All);
            if let Op::Merge(m) = op {
                // File entries may name objects the same merge imports: judge them after it.
                for c in &m.changes {
                    if let Change::Map {
                        tree: true, sets, ..
                    } = c
                    {
                        for (_, v) in sets {
                            ensure(is_file_entry(v, &state), INVALID)?;
                        }
                    }
                }
                let incoming = m
                    .incoming
                    .heads()
                    .iter()
                    .filter_map(|h| self.graph.locate(h))
                    .collect();
                merges.push((i as u32, incoming));
            }
        }
        Ok((closure, state, parents, timeline, merges))
    }

    /// TL-04, TL-76: counters.
    fn check_causality(&self, b: &Batch) -> Result<(), Reason> {
        let below = |v: &crate::ids::Version| v.heads().iter().all(|h| h.counter < b.start);
        ensure(below(&b.parents), Reason::Lamport)?;
        for op in &b.ops {
            if let Op::Merge(m) = op {
                ensure(below(&m.incoming) && below(&m.base), Reason::Lamport)?;
            }
        }
        ensure(b.end() <= MAX_SAFE, Reason::Lamport)
    }

    /// TL-10, TL-15, TL-93: returns the history's kind.
    fn check_genesis(&self, p: &Pending) -> Result<HistoryKind, Reason> {
        let b = &p.batch;
        if !b.parents.is_root() {
            ensure(!p.empty_history, Reason::Genesis)?;
            return self.kind.ok_or(Reason::Genesis);
        }
        ensure(
            self.graph.len() == 0 && p.empty_history && b.timeline.is_none(),
            Reason::Genesis,
        )?;
        let kind = match b.ops.first() {
            Some(Op::TimelineCreate {
                source: None,
                history_kind: Some(k),
                session,
                ..
            }) if (*k == HistoryKind::Transcript) == session.is_some() => *k,
            _ => return Err(Reason::Genesis),
        };
        ensure(p.id == self.id(), Reason::WrongHistory)?;
        Ok(kind)
    }

    /// TL-20 – TL-23, TL-27: returns the timeline the batch is on.
    fn check_timeline(
        &self,
        b: &Batch,
        parents: &[Pos],
        genesis: bool,
        kind: HistoryKind,
    ) -> Result<OpId, Reason> {
        let creates = |op: &Op| matches!(op, Op::TimelineCreate { .. });
        ensure(!b.ops.iter().skip(1).any(creates), Reason::Timeline)?;
        let is_timeline = |t: &OpId| -> Option<Pos> {
            let pos = self.graph.locate(t)?;
            creates(self.graph.op(pos)).then_some(pos)
        };
        if kind == HistoryKind::Transcript {
            ensure(
                !b.ops.iter().any(|op| matches!(op, Op::Merge(_))),
                Reason::Timeline,
            )?;
        }
        match &b.timeline {
            None => {
                let Some(Op::TimelineCreate {
                    source,
                    history_kind,
                    session,
                    ..
                }) = b.ops.first()
                else {
                    return Err(Reason::Timeline);
                };
                if !genesis {
                    ensure(
                        history_kind.is_none() && session.is_none(),
                        Reason::Timeline,
                    )?;
                    ensure(kind != HistoryKind::Transcript, Reason::Timeline)?;
                    let s = source.as_ref().ok_or(Reason::Timeline)?;
                    let s_pos = is_timeline(s).ok_or(Reason::Timeline)?;
                    // TL-22: every head of the fork version is in the source's reach.
                    for &p in parents {
                        ensure(
                            self.graph.is_on(p, s) || self.graph.happens_before_eq(p, s_pos),
                            Reason::Timeline,
                        )?;
                    }
                }
                Ok(b.op_id(0))
            }
            Some(t) => {
                ensure(!creates(&b.ops[0]), Reason::Timeline)?;
                is_timeline(t).ok_or(Reason::Timeline)?;
                // TL-23: no history joins.
                ensure(
                    parents.iter().all(|&p| self.graph.is_on(p, t)),
                    Reason::Timeline,
                )?;
                Ok(t.clone())
            }
        }
    }

    /// TL-17: op `i` of `b` against the state at its parent.
    fn check_op(
        &self,
        state: &State,
        b: &Batch,
        i: usize,
        id: &OpId,
        kind: HistoryKind,
    ) -> Result<(), Reason> {
        match &b.ops[i] {
            Op::TimelineCreate { .. } | Op::Checkpoint { .. } | Op::RegisterCreate { .. } => Ok(()),
            Op::RegisterSet { target, .. } => ensure(state.register(target).is_some(), INVALID),
            Op::MapCreate { tree } => ensure(!tree || kind == HistoryKind::Worktree, INVALID),
            Op::MapSet { target, key, value } => {
                let m = state.map(target).ok_or(INVALID)?;
                check_key(m.tree, key)?;
                if m.tree {
                    ensure(is_path(key) && is_file_entry(value, state), INVALID)?;
                }
                Ok(())
            }
            Op::MapDelete { target, key } => {
                let m = state.map(target).ok_or(INVALID)?;
                check_key(m.tree, key)
            }
            Op::TextCreate { text, transcript } => {
                ensure(!text.contains('\r'), INVALID)?;
                ensure(!transcript || kind == HistoryKind::Transcript, INVALID)
            }
            Op::TextEdit {
                target,
                deletes,
                inserts,
            } => {
                let t = state.text_obj(target).ok_or(INVALID)?;
                let ins: Vec<(Option<&CharId>, &str)> = inserts
                    .iter()
                    .map(|i| (i.anchor.as_ref(), i.text.as_str()))
                    .collect();
                check_edit(t, id, deletes, &ins, 0, false)
            }
            Op::TextSeal { target, ranges } => {
                let t = state.text_obj(target).ok_or(INVALID)?;
                check_seal(t, ranges)
            }
            Op::Merge(m) => self.check_merge(state, b, i, id, m, kind),
        }
    }

    /// TL-72 – TL-76 for a merge op.
    fn check_merge(
        &self,
        state: &State,
        b: &Batch,
        i: usize,
        id: &OpId,
        m: &MergeOp,
        kind: HistoryKind,
    ) -> Result<(), Reason> {
        let source = self.graph.locate(&m.source).ok_or(INVALID)?;
        ensure(
            matches!(self.graph.op(source), Op::TimelineCreate { .. }),
            INVALID,
        )?;
        let incoming: Vec<Pos> = m
            .incoming
            .heads()
            .iter()
            .map(|h| self.graph.locate(h).ok_or(INVALID))
            .collect::<Result<_, _>>()?;
        // An imported object is created in closure(incoming) by an op of the right kind.
        let imported = |object: &ObjectId, ok: &dyn Fn(&Op) -> bool| -> Result<(), Reason> {
            let ObjectId::Op(oid) = object else {
                return Err(INVALID);
            };
            let pos = self.graph.locate(oid).ok_or(INVALID)?;
            ensure(
                self.graph.in_past_of(pos, &incoming) && ok(self.graph.op(pos)),
                INVALID,
            )
        };
        let mut objects = HashSet::new();
        let mut j = 0u32;
        let empty = TextObj::default();
        for c in &m.changes {
            ensure(objects.insert(c.object().clone()), INVALID)?;
            match c {
                Change::Register { object, create, .. } => {
                    if *create {
                        imported(object, &|op| matches!(op, Op::RegisterCreate { .. }))?;
                        ensure(
                            !state.contains(object) || state.register(object).is_some(),
                            INVALID,
                        )?;
                    } else {
                        ensure(state.register(object).is_some(), INVALID)?;
                    }
                }
                Change::Map {
                    object,
                    create,
                    tree,
                    sets,
                    deletes,
                } => {
                    if *create {
                        imported(
                            object,
                            &|op| matches!(op, Op::MapCreate { tree: t } if t == tree),
                        )?;
                    }
                    match state.map(object) {
                        Some(existing) => ensure(existing.tree == *tree, INVALID)?,
                        None => ensure(*create && !state.contains(object), INVALID)?,
                    }
                    ensure(!tree || kind == HistoryKind::Worktree, INVALID)?;
                    let mut keys = HashSet::new();
                    for (k, _) in sets {
                        ensure(keys.insert(k.as_str()), INVALID)?;
                        check_key(*tree, k)?;
                        ensure(!tree || is_path(k), INVALID)?;
                    }
                    for k in deletes {
                        ensure(keys.insert(k.as_str()), INVALID)?;
                        check_key(*tree, k)?;
                    }
                }
                Change::Text {
                    object,
                    create,
                    transcript,
                    deletes,
                    inserts,
                } => {
                    ensure(!transcript, INVALID)?;
                    if *create {
                        imported(
                            object,
                            &|op| matches!(op, Op::TextCreate { transcript: t, .. } if t == transcript),
                        )?;
                    }
                    let t = match state.text_obj(object) {
                        Some(t) => t,
                        None => {
                            ensure(*create && !state.contains(object), INVALID)?;
                            &empty
                        }
                    };
                    let ins: Vec<(Option<&CharId>, &str)> = inserts
                        .iter()
                        .map(|x| (x.anchor.as_ref(), x.text.as_str()))
                        .collect();
                    check_edit(t, id, deletes, &ins, j, *create)?;
                    for x in inserts {
                        if let Some(o) = &x.origin {
                            self.check_origin(o, &x.text, object, b, i, state, &incoming)?;
                        }
                    }
                    j += inserts.len() as u32;
                }
            }
        }
        Ok(())
    }

    /// An origin is a canonical character of the same text whose canonical id exists at
    /// `incoming` or at the op's parent, and the copy's text equals the origin insertion's text
    /// at those offsets (TL-72, TL-76).
    #[allow(clippy::too_many_arguments)]
    fn check_origin(
        &self,
        o: &CharId,
        text: &str,
        object: &ObjectId,
        b: &Batch,
        i: usize,
        state: &State,
        incoming: &[Pos],
    ) -> Result<(), Reason> {
        let InsertionId { op, index } = &o.insertion;
        let own = *op.replica == *b.replica && op.counter >= b.start && op.counter <= b.end();
        let info = if own {
            let k = (op.counter - b.start) as usize;
            ensure(k < i, INVALID)?;
            insertion_in_op(&b.ops[k], op, *index)
        } else {
            let pos = self.graph.locate(op).ok_or(INVALID)?;
            insertion_in_op(self.graph.op(pos), op, *index)
        };
        let info = info.ok_or(INVALID)?;
        ensure(info.object == *object && !info.is_copy, INVALID)?;
        let from = o.offset as usize;
        let n = text.chars().count();
        let origin: Vec<char> = info.text.chars().collect();
        ensure(
            from + n <= origin.len() && origin[from..from + n].iter().copied().eq(text.chars()),
            INVALID,
        )?;
        // The character (or a copy of it) exists at the op's parent or at `incoming`.
        let at_parent = state.text_obj(object).is_some_and(|t| t.has_canonical(o));
        let at_incoming = || {
            self.materialise(incoming)
                .1
                .text_obj(object)
                .is_some_and(|t| t.has_canonical(o))
        };
        ensure(at_parent || at_incoming(), INVALID)
    }
}

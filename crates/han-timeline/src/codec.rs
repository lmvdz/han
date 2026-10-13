//! The CBOR form of batches and ops (TL-91, TL-92) and the full-id form used by digests (TL-74).

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::cbor::{Cbor, CborError};
use crate::ids::{
    CharId, IdRange, InsertionId, MAX_SAFE, ObjectId, OpId, Replica, ReplicaId, Version,
};
use crate::op::{
    Batch, Change, ConflictKind, ConflictRecord, CopyInsert, HistoryKind, Insert, MergeOp, Op,
    ResolutionKind,
};
use crate::value::{Value, ValueError};

/// Ops per batch (§17).
pub const MAX_OPS: usize = 65_536;
/// Replica table entries (§17).
pub const MAX_REPLICAS: usize = 256;
/// Heads in a version (§17).
pub const MAX_HEADS: usize = 64;
/// Timeline names and checkpoint labels, in bytes (§17).
pub const MAX_NAME: usize = 256;
/// Map keys, in bytes (§17).
pub const MAX_KEY: usize = 1024;
/// Tree paths, in bytes (§17).
pub const MAX_PATH: usize = 4096;
/// The `step` field, in bytes (TL-91).
pub const MAX_STEP: usize = 256;
/// The `time` field, in bytes (informational; generous).
const MAX_TIME: usize = 64;

/// Why bytes are not a valid batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Not deterministic CBOR or not the right shape.
    Malformed,
    /// Over a limit of §17.
    TooLarge,
}

impl From<CborError> for DecodeError {
    fn from(e: CborError) -> Self {
        match e {
            CborError::Malformed => DecodeError::Malformed,
            CborError::TooDeep => DecodeError::TooLarge,
        }
    }
}

impl From<ValueError> for DecodeError {
    fn from(e: ValueError) -> Self {
        match e {
            ValueError::Malformed => DecodeError::Malformed,
            ValueError::TooLarge => DecodeError::TooLarge,
        }
    }
}

type R<T> = Result<T, DecodeError>;

const M: DecodeError = DecodeError::Malformed;

// ---------------------------------------------------------------------------------------------
// Reference styles

/// How references are written: through the batch's replica table (the wire, TL-91), or as full
/// op ids with `this` written as null (digests, TL-74).
pub(crate) enum Refs<'a> {
    Table {
        own: &'a ReplicaId,
        table: &'a [Replica],
    },
    Full {
        this: Option<&'a OpId>,
    },
}

impl Refs<'_> {
    fn id_parts(&self, id: &OpId) -> Vec<Cbor> {
        match self {
            Refs::Table { own, table } => {
                let r = if *id.replica == **own {
                    0
                } else {
                    // Every referenced replica is in the table (encode_batch builds it so).
                    table
                        .binary_search_by(|x| (**x).cmp(&id.replica))
                        .map(|i| i + 1)
                        .unwrap_or(usize::MAX)
                };
                vec![Cbor::u(r as u64), Cbor::u(id.counter)]
            }
            Refs::Full { this } => {
                if Some(id) == *this {
                    vec![Cbor::Null]
                } else {
                    vec![full_id(id)]
                }
            }
        }
    }

    fn op(&self, id: &OpId) -> Cbor {
        match self {
            Refs::Table { .. } => Cbor::Array(self.id_parts(id)),
            Refs::Full { .. } => full_id(id),
        }
    }

    fn object(&self, id: &ObjectId) -> Cbor {
        match id {
            ObjectId::Root => Cbor::Null,
            ObjectId::Op(id) => self.op(id),
        }
    }

    fn version(&self, v: &Version) -> Cbor {
        Cbor::Array(v.heads().iter().map(|h| self.op(h)).collect())
    }

    fn char_ref(&self, c: &CharId) -> Cbor {
        let mut parts = self.id_parts(&c.insertion.op);
        parts.push(Cbor::u(u64::from(c.insertion.index)));
        parts.push(Cbor::u(u64::from(c.offset)));
        Cbor::Array(parts)
    }

    fn anchor(&self, a: &Option<CharId>) -> Cbor {
        a.as_ref().map_or(Cbor::Null, |c| self.char_ref(c))
    }

    fn range(&self, r: &IdRange) -> Cbor {
        let mut parts = self.id_parts(&r.insertion.op);
        parts.push(Cbor::u(u64::from(r.insertion.index)));
        parts.push(Cbor::u(u64::from(r.from)));
        parts.push(Cbor::u(u64::from(r.to)));
        Cbor::Array(parts)
    }
}

pub(crate) fn full_id(id: &OpId) -> Cbor {
    Cbor::Array(vec![
        Cbor::text(id.replica.actor.clone()),
        Cbor::text(id.replica.key.clone()),
        Cbor::u(u64::from(id.replica.lane)),
        Cbor::u(id.counter),
    ])
}

fn map(entries: Vec<(u64, Cbor)>) -> Cbor {
    Cbor::Map(entries.into_iter().map(|(k, v)| (Cbor::u(k), v)).collect())
}

// ---------------------------------------------------------------------------------------------
// Encoding

pub(crate) fn encode_change(c: &Change, refs: &Refs<'_>) -> Cbor {
    match c {
        Change::Register {
            object,
            create,
            value,
        } => map(vec![
            (0, Cbor::u(1)),
            (1, refs.object(object)),
            (2, Cbor::Bool(*create)),
            (3, value.to_cbor()),
        ]),
        Change::Map {
            object,
            create,
            tree,
            sets,
            deletes,
        } => map(vec![
            (0, Cbor::u(2)),
            (1, refs.object(object)),
            (2, Cbor::Bool(*create)),
            (3, Cbor::Bool(*tree)),
            (
                4,
                Cbor::Array(
                    sets.iter()
                        .map(|(k, v)| Cbor::Array(vec![Cbor::text(k.clone()), v.to_cbor()]))
                        .collect(),
                ),
            ),
            (
                5,
                Cbor::Array(deletes.iter().map(|k| Cbor::text(k.clone())).collect()),
            ),
        ]),
        Change::Text {
            object,
            create,
            transcript,
            deletes,
            inserts,
        } => map(vec![
            (0, Cbor::u(3)),
            (1, refs.object(object)),
            (2, Cbor::Bool(*create)),
            (3, Cbor::Bool(*transcript)),
            (
                4,
                Cbor::Array(deletes.iter().map(|r| refs.range(r)).collect()),
            ),
            (
                5,
                Cbor::Array(
                    inserts
                        .iter()
                        .map(|i| {
                            Cbor::Array(vec![
                                refs.anchor(&i.anchor),
                                Cbor::text(i.text.clone()),
                                refs.anchor(&i.origin),
                            ])
                        })
                        .collect(),
                ),
            ),
        ]),
    }
}

pub(crate) fn encode_conflict(c: &ConflictRecord, refs: &Refs<'_>) -> Cbor {
    map(vec![
        (0, Cbor::u(c.kind as u64)),
        (1, refs.object(&c.object)),
        (2, c.key.clone().map_or(Cbor::Null, Cbor::Text)),
        (
            3,
            c.lines.map_or(Cbor::Null, |(a, b)| {
                Cbor::Array(vec![Cbor::u(a), Cbor::u(b)])
            }),
        ),
        (4, Cbor::u(c.resolution as u64)),
    ])
}

fn encode_op(op: &Op, refs: &Refs<'_>) -> Cbor {
    let mut f = vec![(0, Cbor::u(op.kind_number()))];
    match op {
        Op::TimelineCreate {
            name,
            source,
            history_kind,
            session,
        } => {
            f.push((1, Cbor::text(name.clone())));
            f.push((2, source.as_ref().map_or(Cbor::Null, |s| refs.op(s))));
            if let Some(k) = history_kind {
                f.push((3, Cbor::text(k.as_str())));
            }
            if let Some(s) = session {
                f.push((4, Cbor::text(s.clone())));
            }
        }
        Op::Checkpoint { label } => f.push((1, Cbor::text(label.clone()))),
        Op::Merge(m) => {
            f.push((1, refs.op(&m.source)));
            f.push((2, refs.version(&m.incoming)));
            f.push((3, refs.version(&m.base)));
            f.push((
                4,
                Cbor::Array(m.changes.iter().map(|c| encode_change(c, refs)).collect()),
            ));
            f.push((
                5,
                Cbor::Array(
                    m.conflicts
                        .iter()
                        .map(|c| encode_conflict(c, refs))
                        .collect(),
                ),
            ));
            if let Some(e) = &m.effect {
                f.push((6, Cbor::text(e.clone())));
            }
        }
        Op::RegisterCreate { value } => f.push((1, value.to_cbor())),
        Op::RegisterSet { target, value } => {
            f.push((1, refs.object(target)));
            f.push((2, value.to_cbor()));
        }
        Op::MapCreate { tree } => f.push((1, Cbor::Bool(*tree))),
        Op::MapSet { target, key, value } => {
            f.push((1, refs.object(target)));
            f.push((2, Cbor::text(key.clone())));
            f.push((3, value.to_cbor()));
        }
        Op::MapDelete { target, key } => {
            f.push((1, refs.object(target)));
            f.push((2, Cbor::text(key.clone())));
        }
        Op::TextCreate { text, transcript } => {
            f.push((1, Cbor::text(text.clone())));
            f.push((2, Cbor::Bool(*transcript)));
        }
        Op::TextEdit {
            target,
            deletes,
            inserts,
        } => {
            f.push((1, refs.object(target)));
            f.push((
                2,
                Cbor::Array(deletes.iter().map(|r| refs.range(r)).collect()),
            ));
            f.push((
                3,
                Cbor::Array(
                    inserts
                        .iter()
                        .map(|i| {
                            Cbor::Array(vec![refs.anchor(&i.anchor), Cbor::text(i.text.clone())])
                        })
                        .collect(),
                ),
            ));
        }
        Op::TextSeal { target, ranges } => {
            f.push((1, refs.object(target)));
            f.push((
                2,
                Cbor::Array(ranges.iter().map(|r| refs.range(r)).collect()),
            ));
        }
    }
    map(f)
}

/// Every op id a batch references through refs (not through values), for its replica table.
pub(crate) fn visit_refs(batch: &Batch, f: &mut impl FnMut(&OpId)) {
    batch.parents.heads().iter().for_each(&mut *f);
    if let Some(t) = &batch.timeline {
        f(t);
    }
    for op in &batch.ops {
        visit_op_refs(op, f);
    }
}

fn visit_object(o: &ObjectId, f: &mut impl FnMut(&OpId)) {
    if let ObjectId::Op(id) = o {
        f(id);
    }
}

pub(crate) fn visit_op_refs(op: &Op, f: &mut impl FnMut(&OpId)) {
    match op {
        Op::TimelineCreate { source, .. } => {
            if let Some(s) = source {
                f(s);
            }
        }
        Op::Checkpoint { .. }
        | Op::RegisterCreate { .. }
        | Op::MapCreate { .. }
        | Op::TextCreate { .. } => {}
        Op::Merge(m) => {
            f(&m.source);
            m.incoming.heads().iter().for_each(&mut *f);
            m.base.heads().iter().for_each(&mut *f);
            for c in &m.changes {
                visit_object(c.object(), f);
                if let Change::Text {
                    deletes, inserts, ..
                } = c
                {
                    deletes.iter().for_each(|r| f(&r.insertion.op));
                    for i in inserts {
                        if let Some(a) = &i.anchor {
                            f(&a.insertion.op);
                        }
                        if let Some(o) = &i.origin {
                            f(&o.insertion.op);
                        }
                    }
                }
            }
            m.conflicts.iter().for_each(|c| visit_object(&c.object, f));
        }
        Op::RegisterSet { target, .. }
        | Op::MapSet { target, .. }
        | Op::MapDelete { target, .. } => visit_object(target, f),
        Op::TextEdit {
            target,
            deletes,
            inserts,
        } => {
            visit_object(target, f);
            deletes.iter().for_each(|r| f(&r.insertion.op));
            inserts
                .iter()
                .filter_map(|i| i.anchor.as_ref())
                .for_each(|a| f(&a.insertion.op));
        }
        Op::TextSeal { target, ranges } => {
            visit_object(target, f);
            ranges.iter().for_each(|r| f(&r.insertion.op));
        }
    }
}

/// The deterministic CBOR of a batch (TL-91). The batch's own replica is index 0; the table
/// lists every other referenced replica in ascending order.
pub fn encode_batch(batch: &Batch) -> Vec<u8> {
    let mut others = BTreeSet::new();
    visit_refs(batch, &mut |id| {
        if *id.replica != *batch.replica {
            others.insert(id.replica.clone());
        }
    });
    let table: Vec<Replica> = others.into_iter().collect();
    let refs = Refs::Table {
        own: &batch.replica,
        table: &table,
    };
    let mut f = vec![
        (1, Cbor::u(u64::from(batch.replica.lane))),
        (2, Cbor::u(batch.start)),
        (3, refs.version(&batch.parents)),
        (
            4,
            batch.timeline.as_ref().map_or(Cbor::Null, |t| refs.op(t)),
        ),
        (
            5,
            Cbor::Array(
                table
                    .iter()
                    .map(|r| {
                        Cbor::Array(vec![
                            Cbor::text(r.actor.clone()),
                            Cbor::text(r.key.clone()),
                            Cbor::u(u64::from(r.lane)),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            6,
            Cbor::Array(batch.ops.iter().map(|o| encode_op(o, &refs)).collect()),
        ),
    ];
    if let Some(t) = &batch.time {
        f.push((7, Cbor::text(t.clone())));
    }
    if let Some(s) = &batch.step {
        f.push((8, Cbor::text(s.clone())));
    }
    map(f).encode()
}

// ---------------------------------------------------------------------------------------------
// Decoding

/// Checks that `item` is a map whose keys are all unsigned integers in `allowed`, and returns
/// a field getter.
fn fields<'a>(item: &'a Cbor, allowed: &[u64]) -> R<Fields<'a>> {
    let Cbor::Map(entries) = item else {
        return Err(M);
    };
    for (k, _) in entries {
        match k {
            Cbor::Uint(n) if allowed.contains(n) => {}
            _ => return Err(M),
        }
    }
    Ok(Fields(item))
}

struct Fields<'a>(&'a Cbor);

impl<'a> Fields<'a> {
    fn opt(&self, k: u64) -> Option<&'a Cbor> {
        self.0.get(k)
    }
    fn req(&self, k: u64) -> R<&'a Cbor> {
        self.0.get(k).ok_or(M)
    }
}

fn uint(c: &Cbor) -> R<u64> {
    match c {
        Cbor::Uint(n) => Ok(*n),
        _ => Err(M),
    }
}

fn u32_of(c: &Cbor) -> R<u32> {
    u32::try_from(uint(c)?).map_err(|_| M)
}

fn counter(c: &Cbor) -> R<u64> {
    let n = uint(c)?;
    if (1..=MAX_SAFE).contains(&n) {
        Ok(n)
    } else {
        Err(M)
    }
}

fn boolean(c: &Cbor) -> R<bool> {
    match c {
        Cbor::Bool(b) => Ok(*b),
        _ => Err(M),
    }
}

fn text(c: &Cbor) -> R<&str> {
    match c {
        Cbor::Text(s) => Ok(s),
        _ => Err(M),
    }
}

fn array(c: &Cbor) -> R<&[Cbor]> {
    match c {
        Cbor::Array(a) => Ok(a),
        _ => Err(M),
    }
}

/// A timeline name or checkpoint label: 1–256 bytes, no control characters (TL-20, TL-25).
fn label(c: &Cbor) -> R<String> {
    let s = text(c)?;
    if s.len() > MAX_NAME {
        return Err(DecodeError::TooLarge);
    }
    if s.is_empty() || s.chars().any(|ch| ch < ' ' || ch == '\u{7f}') {
        return Err(M);
    }
    Ok(s.to_string())
}

/// A map key: 1 byte up to the tree path limit; the 1,024-byte limit of non-tree maps is
/// checked against the target (TL-38).
fn key(c: &Cbor) -> R<String> {
    let s = text(c)?;
    if s.len() > MAX_PATH {
        return Err(DecodeError::TooLarge);
    }
    if s.is_empty() {
        return Err(M);
    }
    Ok(s.to_string())
}

fn hex64(c: &Cbor) -> R<String> {
    let s = text(c)?;
    if crate::ids::is_hex64(s) {
        Ok(s.to_string())
    } else {
        Err(M)
    }
}

struct Ctx {
    replicas: Vec<Replica>,
    used: Vec<bool>,
}

impl Ctx {
    fn replica(&mut self, c: &Cbor) -> R<Replica> {
        let r = usize::try_from(uint(c)?).map_err(|_| M)?;
        let rep = self.replicas.get(r).ok_or(M)?.clone();
        self.used[r] = true;
        Ok(rep)
    }

    fn op_parts(&mut self, parts: &[Cbor]) -> R<OpId> {
        let [r, c] = parts else { return Err(M) };
        let replica = self.replica(r)?;
        Ok(OpId::new(counter(c)?, replica))
    }

    fn op(&mut self, c: &Cbor) -> R<OpId> {
        self.op_parts(array(c)?)
    }

    fn opt_op(&mut self, c: &Cbor) -> R<Option<OpId>> {
        match c {
            Cbor::Null => Ok(None),
            _ => self.op(c).map(Some),
        }
    }

    fn object(&mut self, c: &Cbor) -> R<ObjectId> {
        Ok(self.opt_op(c)?.map_or(ObjectId::Root, ObjectId::Op))
    }

    fn version(&mut self, c: &Cbor) -> R<Version> {
        let a = array(c)?;
        if a.len() > MAX_HEADS {
            return Err(DecodeError::TooLarge);
        }
        let ids = a.iter().map(|x| self.op(x)).collect::<R<Vec<_>>>()?;
        if ids.windows(2).any(|w| w[0] >= w[1]) {
            return Err(M);
        }
        Ok(Version::new(ids))
    }

    fn char_ref(&mut self, c: &Cbor) -> R<CharId> {
        let a = array(c)?;
        let [r, n, i, k] = a else { return Err(M) };
        let op = self.op_parts(&[r.clone(), n.clone()])?;
        Ok(CharId {
            insertion: InsertionId {
                op,
                index: u32_of(i)?,
            },
            offset: u32_of(k)?,
        })
    }

    fn anchor(&mut self, c: &Cbor) -> R<Option<CharId>> {
        match c {
            Cbor::Null => Ok(None),
            _ => self.char_ref(c).map(Some),
        }
    }

    fn range(&mut self, c: &Cbor) -> R<IdRange> {
        let a = array(c)?;
        let [r, n, i, from, to] = a else {
            return Err(M);
        };
        let op = self.op_parts(&[r.clone(), n.clone()])?;
        Ok(IdRange {
            insertion: InsertionId {
                op,
                index: u32_of(i)?,
            },
            from: u32_of(from)?,
            to: u32_of(to)?,
        })
    }

    fn ranges(&mut self, c: &Cbor) -> R<Vec<IdRange>> {
        array(c)?.iter().map(|x| self.range(x)).collect()
    }
}

fn value(c: &Cbor) -> R<Value> {
    Ok(Value::from_cbor(c)?)
}

fn decode_change(c: &Cbor, ctx: &mut Ctx) -> R<Change> {
    let kind = uint(c.get(0).ok_or(M)?)?;
    match kind {
        1 => {
            let f = fields(c, &[0, 1, 2, 3])?;
            Ok(Change::Register {
                object: ctx.object(f.req(1)?)?,
                create: boolean(f.req(2)?)?,
                value: value(f.req(3)?)?,
            })
        }
        2 => {
            let f = fields(c, &[0, 1, 2, 3, 4, 5])?;
            let sets = array(f.req(4)?)?
                .iter()
                .map(|e| match array(e)? {
                    [k, v] => Ok((key(k)?, value(v)?)),
                    _ => Err(M),
                })
                .collect::<R<Vec<_>>>()?;
            let deletes = array(f.req(5)?)?.iter().map(key).collect::<R<Vec<_>>>()?;
            Ok(Change::Map {
                object: ctx.object(f.req(1)?)?,
                create: boolean(f.req(2)?)?,
                tree: boolean(f.req(3)?)?,
                sets,
                deletes,
            })
        }
        3 => {
            let f = fields(c, &[0, 1, 2, 3, 4, 5])?;
            let object = ctx.object(f.req(1)?)?;
            let deletes = ctx.ranges(f.req(4)?)?;
            let inserts = array(f.req(5)?)?
                .iter()
                .map(|e| match array(e)? {
                    [a, t, o] => Ok(CopyInsert {
                        anchor: ctx.anchor(a)?,
                        text: text(t)?.to_string(),
                        origin: ctx.anchor(o)?,
                    }),
                    _ => Err(M),
                })
                .collect::<R<Vec<_>>>()?;
            Ok(Change::Text {
                object,
                create: boolean(f.req(2)?)?,
                transcript: boolean(f.req(3)?)?,
                deletes,
                inserts,
            })
        }
        _ => Err(M),
    }
}

fn decode_conflict(c: &Cbor, ctx: &mut Ctx) -> R<ConflictRecord> {
    let f = fields(c, &[0, 1, 2, 3, 4])?;
    let kind = match uint(f.req(0)?)? {
        1 => ConflictKind::Register,
        2 => ConflictKind::Map,
        3 => ConflictKind::Text,
        4 => ConflictKind::ModifyDelete,
        5 => ConflictKind::Rename,
        6 => ConflictKind::Path,
        _ => return Err(M),
    };
    let key = match f.req(2)? {
        Cbor::Null => None,
        k => Some(key(k)?),
    };
    let lines = match f.req(3)? {
        Cbor::Null => None,
        l => match array(l)? {
            [a, b] => Some((uint(a)?, uint(b)?)),
            _ => return Err(M),
        },
    };
    let resolution = match uint(f.req(4)?)? {
        1 => ResolutionKind::Target,
        2 => ResolutionKind::Incoming,
        3 => ResolutionKind::Given,
        _ => return Err(M),
    };
    Ok(ConflictRecord {
        kind,
        object: ctx.object(f.req(1)?)?,
        key,
        lines,
        resolution,
    })
}

fn decode_op(c: &Cbor, ctx: &mut Ctx) -> R<Op> {
    let kind = uint(c.get(0).ok_or(M)?)?;
    Ok(match kind {
        1 => {
            let f = fields(c, &[0, 1, 2, 3, 4])?;
            let history_kind = f
                .opt(3)
                .map(|k| HistoryKind::parse(text(k)?).ok_or(M))
                .transpose()?;
            Op::TimelineCreate {
                name: label(f.req(1)?)?,
                source: ctx.opt_op(f.req(2)?)?,
                history_kind,
                session: f.opt(4).map(hex64).transpose()?,
            }
        }
        2 => {
            let f = fields(c, &[0, 1])?;
            Op::Checkpoint {
                label: label(f.req(1)?)?,
            }
        }
        3 => {
            let f = fields(c, &[0, 1, 2, 3, 4, 5, 6])?;
            let source = ctx.op(f.req(1)?)?;
            let incoming = ctx.version(f.req(2)?)?;
            let base = ctx.version(f.req(3)?)?;
            let changes = array(f.req(4)?)?
                .iter()
                .map(|x| decode_change(x, ctx))
                .collect::<R<Vec<_>>>()?;
            let conflicts = array(f.req(5)?)?
                .iter()
                .map(|x| decode_conflict(x, ctx))
                .collect::<R<Vec<_>>>()?;
            Op::Merge(Box::new(MergeOp {
                source,
                incoming,
                base,
                changes,
                conflicts,
                effect: f.opt(6).map(hex64).transpose()?,
            }))
        }
        10 => {
            let f = fields(c, &[0, 1])?;
            Op::RegisterCreate {
                value: value(f.req(1)?)?,
            }
        }
        11 => {
            let f = fields(c, &[0, 1, 2])?;
            Op::RegisterSet {
                target: ctx.object(f.req(1)?)?,
                value: value(f.req(2)?)?,
            }
        }
        20 => {
            let f = fields(c, &[0, 1])?;
            Op::MapCreate {
                tree: boolean(f.req(1)?)?,
            }
        }
        21 => {
            let f = fields(c, &[0, 1, 2, 3])?;
            Op::MapSet {
                target: ctx.object(f.req(1)?)?,
                key: key(f.req(2)?)?,
                value: value(f.req(3)?)?,
            }
        }
        22 => {
            let f = fields(c, &[0, 1, 2])?;
            Op::MapDelete {
                target: ctx.object(f.req(1)?)?,
                key: key(f.req(2)?)?,
            }
        }
        30 => {
            let f = fields(c, &[0, 1, 2])?;
            Op::TextCreate {
                text: text(f.req(1)?)?.to_string(),
                transcript: boolean(f.req(2)?)?,
            }
        }
        31 => {
            let f = fields(c, &[0, 1, 2, 3])?;
            let target = ctx.object(f.req(1)?)?;
            let deletes = ctx.ranges(f.req(2)?)?;
            let inserts = array(f.req(3)?)?
                .iter()
                .map(|e| match array(e)? {
                    [a, t] => Ok(Insert {
                        anchor: ctx.anchor(a)?,
                        text: text(t)?.to_string(),
                    }),
                    _ => Err(M),
                })
                .collect::<R<Vec<_>>>()?;
            Op::TextEdit {
                target,
                deletes,
                inserts,
            }
        }
        32 => {
            let f = fields(c, &[0, 1, 2])?;
            Op::TextSeal {
                target: ctx.object(f.req(1)?)?,
                ranges: ctx.ranges(f.req(2)?)?,
            }
        }
        _ => return Err(M),
    })
}

/// Decodes a batch written by `actor` with `key` (TL-91, TL-92): deterministic CBOR, exact
/// shape, a canonical replica table, and the limits of §17 that need no state.
pub fn decode_batch(bytes: &[u8], actor: &str, key: &str) -> R<Batch> {
    let item = Cbor::decode(bytes)?;
    let f = fields(&item, &[1, 2, 3, 4, 5, 6, 7, 8])?;
    let lane = u32_of(f.req(1)?)?;
    let start = counter(f.req(2)?)?;
    let own: Replica = Arc::new(ReplicaId::new(actor, key, lane));
    let table_items = array(f.req(5)?)?;
    if table_items.len() > MAX_REPLICAS {
        return Err(DecodeError::TooLarge);
    }
    let mut replicas = vec![own.clone()];
    for t in table_items {
        let [a, k, l] = array(t)? else {
            return Err(M);
        };
        let (a, k) = (text(a)?, text(k)?);
        if !han_core::grammar::is_actor_id(a) || !han_core::grammar::is_key_id(k) {
            return Err(M);
        }
        let r = ReplicaId::new(a, k, u32_of(l)?);
        // Ascending, unique, and never the batch's own replica.
        if r == *own || replicas.len() > 1 && *replicas[replicas.len() - 1] >= r {
            return Err(M);
        }
        replicas.push(Arc::new(r));
    }
    let used = vec![false; replicas.len()];
    let mut ctx = Ctx { replicas, used };
    let parents = ctx.version(f.req(3)?)?;
    let timeline = ctx.opt_op(f.req(4)?)?;
    let op_items = array(f.req(6)?)?;
    if op_items.is_empty() {
        return Err(M);
    }
    if op_items.len() > MAX_OPS {
        return Err(DecodeError::TooLarge);
    }
    let ops = op_items
        .iter()
        .map(|o| decode_op(o, &mut ctx))
        .collect::<R<Vec<_>>>()?;
    // The table lists only replicas the batch references.
    if ctx.used.iter().skip(1).any(|u| !u) {
        return Err(M);
    }
    let time = match f.opt(7) {
        Some(t) => {
            let t = text(t)?;
            if t.is_empty() || t.len() > MAX_TIME {
                return Err(M);
            }
            Some(t.to_string())
        }
        None => None,
    };
    let step = match f.opt(8) {
        Some(s) => {
            let s = text(s)?;
            if s.len() > MAX_STEP {
                return Err(DecodeError::TooLarge);
            }
            Some(s.to_string())
        }
        None => None,
    };
    Ok(Batch {
        replica: own,
        start,
        parents,
        timeline,
        ops,
        time,
        step,
    })
}

/// The full-id form of a merge's changes and conflicts, for its digest (TL-74).
pub(crate) fn merge_digest_parts(m: &MergeOp, this: Option<&OpId>) -> (Cbor, Cbor) {
    let refs = Refs::Full { this };
    (
        Cbor::Array(m.changes.iter().map(|c| encode_change(c, &refs)).collect()),
        Cbor::Array(
            m.conflicts
                .iter()
                .map(|c| encode_conflict(c, &refs))
                .collect(),
        ),
    )
}

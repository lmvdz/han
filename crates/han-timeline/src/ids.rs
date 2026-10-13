//! Replica ids, op ids, object ids, character ids and versions (TL-01 – TL-06, TL-13, TL-31, TL-41).

use std::fmt;
use std::sync::Arc;

/// The largest counter (TL-03) and the largest magnitude of an integer value (TL-30).
pub const MAX_SAFE: u64 = (1 << 53) - 1;

/// A replica: one writer, identified by actor, device key and lane (TL-01).
///
/// The derived order is the replica order of TL-02: actor, then key, as UTF-8 bytes (a shorter
/// prefix first), then lane.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReplicaId {
    /// The HAN ActorId of the author.
    pub actor: String,
    /// The KeyId of the device key that signs the replica's batches.
    pub key: String,
    /// The lane the device chose for this writer.
    pub lane: u32,
}

impl ReplicaId {
    /// A replica id from its three parts.
    pub fn new(actor: impl Into<String>, key: impl Into<String>, lane: u32) -> Self {
        ReplicaId {
            actor: actor.into(),
            key: key.into(),
            lane,
        }
    }
}

impl fmt::Display for ReplicaId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.actor, self.key, self.lane)
    }
}

/// A shared replica id; op ids hold one each.
pub type Replica = Arc<ReplicaId>;

/// An op id: counter, then replica (TL-03). The derived order is the op id order.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OpId {
    /// The Lamport counter, in `1 ..= MAX_SAFE`.
    pub counter: u64,
    /// The replica that wrote the op.
    pub replica: Replica,
}

impl OpId {
    /// The op id `counter@replica`.
    pub fn new(counter: u64, replica: Replica) -> Self {
        OpId { counter, replica }
    }
}

impl fmt::Display for OpId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.counter, self.replica)
    }
}

/// An object id: the root map or the op that created the object (TL-31). `Root` sorts first.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObjectId {
    /// The root map.
    Root,
    /// The object created by this op.
    Op(OpId),
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ObjectId::Root => f.write_str("root"),
            ObjectId::Op(id) => id.fmt(f),
        }
    }
}

/// An insertion: one inserted string, named by its op and its index among that op's inserts
/// (TL-41).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InsertionId {
    /// The op that inserted it.
    pub op: OpId,
    /// Its index among the op's inserts.
    pub index: u32,
}

/// A character id: character `offset` of an insertion (TL-41).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CharId {
    /// The insertion the character belongs to.
    pub insertion: InsertionId,
    /// Its offset in the insertion, counted in Unicode scalar values.
    pub offset: u32,
}

/// An identity range `[insertion, from, to)` of characters (TL-44, TL-51).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IdRange {
    /// The insertion.
    pub insertion: InsertionId,
    /// First offset, inclusive.
    pub from: u32,
    /// Last offset, exclusive.
    pub to: u32,
}

/// A version: an antichain of op ids, kept in ascending op id order (TL-13).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Version(Vec<OpId>);

impl Version {
    /// The root version.
    pub fn root() -> Self {
        Version(Vec::new())
    }

    /// A version from op ids in any order (duplicates removed). Whether they form an antichain
    /// is not checked here; the history normalises versions it accepts (TL-13).
    pub fn new(mut ids: Vec<OpId>) -> Self {
        ids.sort();
        ids.dedup();
        Version(ids)
    }

    /// The heads in ascending op id order.
    pub fn heads(&self) -> &[OpId] {
        &self.0
    }

    /// Whether this is the root version.
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[")?;
        for (i, id) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            id.fmt(f)?;
        }
        f.write_str("]")
    }
}

/// A batch id or history id: SHA-256 of the envelope's batch-id preimage (TL-93).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BatchId(pub [u8; 32]);

impl BatchId {
    /// 64 lowercase hex digits.
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }
}

impl fmt::Display for BatchId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// The id of a history: the batch id of its genesis envelope (TL-10).
pub type HistoryId = BatchId;

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[usize::from(b >> 4)] as char);
        s.push(DIGITS[usize::from(b & 15)] as char);
    }
    s
}

/// Whether `s` is 64 lowercase hex digits (an EntryId).
pub(crate) fn is_hex64(s: &str) -> bool {
    han_core::grammar::is_hex(s)
}

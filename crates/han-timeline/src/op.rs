//! Operations and batches, decoded (TL-11, TL-16, TL-73).

use crate::ids::{CharId, IdRange, ObjectId, OpId, Replica, Version};
use crate::value::Value;

/// The kind of a history, fixed by main's create op (TL-27).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HistoryKind {
    /// Mirrors one repository (§12).
    Worktree,
    /// One session's transcript (TL-89).
    Transcript,
    /// Any other use.
    Data,
}

impl HistoryKind {
    /// The text form used on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            HistoryKind::Worktree => "worktree",
            HistoryKind::Transcript => "transcript",
            HistoryKind::Data => "data",
        }
    }

    /// Parses the text form.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "worktree" => Some(HistoryKind::Worktree),
            "transcript" => Some(HistoryKind::Transcript),
            "data" => Some(HistoryKind::Data),
            _ => None,
        }
    }
}

/// One insert of a `text.edit`: text placed after `anchor` (`None` is `start`) (TL-42).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Insert {
    /// The character to the left of the insertion point, or `None` for `start`.
    pub anchor: Option<CharId>,
    /// The inserted text; never empty, never containing CR.
    pub text: String,
}

/// One insert of a merge's text change: like [`Insert`], plus the origin of a copy (TL-72).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyInsert {
    /// The character to the left of the insertion point, or `None` for `start`.
    pub anchor: Option<CharId>,
    /// The inserted text.
    pub text: String,
    /// For a copy, the canonical character its first character came from.
    pub origin: Option<CharId>,
}

/// One object's changes in a merge op (TL-73).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// Set (or import) a register.
    Register {
        /// The register.
        object: ObjectId,
        /// Whether the change imports the object.
        create: bool,
        /// The value written.
        value: Value,
    },
    /// Set and delete map keys (or import a map).
    Map {
        /// The map.
        object: ObjectId,
        /// Whether the change imports the object.
        create: bool,
        /// The map's tree flag.
        tree: bool,
        /// Keys written, in ascending key order.
        sets: Vec<(String, Value)>,
        /// Keys removed, in ascending key order.
        deletes: Vec<String>,
    },
    /// Delete and insert characters (or import a text).
    Text {
        /// The text.
        object: ObjectId,
        /// Whether the change imports the object.
        create: bool,
        /// The text's transcript flag.
        transcript: bool,
        /// Identity ranges deleted.
        deletes: Vec<IdRange>,
        /// Inserts, in index order.
        inserts: Vec<CopyInsert>,
    },
}

impl Change {
    /// The object the change applies to.
    pub fn object(&self) -> &ObjectId {
        match self {
            Change::Register { object, .. }
            | Change::Map { object, .. }
            | Change::Text { object, .. } => object,
        }
    }
}

/// The kind of a merge conflict (TL-75, TL-92).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ConflictKind {
    /// A register both sides set differently.
    Register = 1,
    /// A map key both sides changed differently.
    Map = 2,
    /// A text hunk.
    Text = 3,
    /// A path one side removed and the other modified (TL-87).
    ModifyDelete = 4,
    /// A content object renamed differently on both sides (TL-87).
    Rename = 5,
    /// A file and a directory of one name (TL-87).
    Path = 6,
}

/// Which resolution a merge took for a conflict (TL-75).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResolutionKind {
    /// Kept the target's side.
    Target = 1,
    /// Took the incoming side.
    Incoming = 2,
    /// A value or text of the merge author's own.
    Given = 3,
}

/// The record of one conflict and its resolution in a merge op (TL-73, TL-92).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictRecord {
    /// The conflict kind.
    pub kind: ConflictKind,
    /// The object (the tree map for tree conflicts).
    pub object: ObjectId,
    /// The key or path, when the location has one.
    pub key: Option<String>,
    /// The base line range of a text hunk.
    pub lines: Option<(u64, u64)>,
    /// The resolution taken.
    pub resolution: ResolutionKind,
}

/// A `merge` op (TL-73).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeOp {
    /// The incoming timeline.
    pub source: OpId,
    /// The incoming version that was planned.
    pub incoming: Version,
    /// The merge base.
    pub base: Version,
    /// What to apply, per object.
    pub changes: Vec<Change>,
    /// The plan's conflicts and the resolutions taken.
    pub conflicts: Vec<ConflictRecord>,
    /// The EntryId of the HAN effect that authorised the merge.
    pub effect: Option<String>,
}

/// An operation (TL-16).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Creates a timeline (§4).
    TimelineCreate {
        /// Its name, a label.
        name: String,
        /// The timeline it forks, or `None` for main.
        source: Option<OpId>,
        /// The history's kind, on main only.
        history_kind: Option<HistoryKind>,
        /// The `session.open` EntryId, on the main of a transcript history only.
        session: Option<String>,
    },
    /// Names the version of its parent (TL-25).
    Checkpoint {
        /// Its label.
        label: String,
    },
    /// A three-way content merge (§11).
    Merge(Box<MergeOp>),
    /// Creates a register.
    RegisterCreate {
        /// Its first value.
        value: Value,
    },
    /// Writes a register.
    RegisterSet {
        /// The register.
        target: ObjectId,
        /// The value.
        value: Value,
    },
    /// Creates a map; `tree` makes it a worktree tree.
    MapCreate {
        /// The tree flag.
        tree: bool,
    },
    /// Writes a map key.
    MapSet {
        /// The map.
        target: ObjectId,
        /// The key.
        key: String,
        /// The value.
        value: Value,
    },
    /// Removes the values of a key its author had seen.
    MapDelete {
        /// The map.
        target: ObjectId,
        /// The key.
        key: String,
    },
    /// Creates a text, optionally with initial content.
    TextCreate {
        /// The initial text (insertion 0, anchored at `start`).
        text: String,
        /// The transcript flag.
        transcript: bool,
    },
    /// Deletes and inserts characters.
    TextEdit {
        /// The text.
        target: ObjectId,
        /// Identity ranges deleted.
        deletes: Vec<IdRange>,
        /// Inserts, in index order.
        inserts: Vec<Insert>,
    },
    /// Freezes a range of a transcript (§9).
    TextSeal {
        /// The transcript.
        target: ObjectId,
        /// The sealed identity ranges, in document order.
        ranges: Vec<IdRange>,
    },
}

impl Op {
    /// The kind number of TL-92.
    pub fn kind_number(&self) -> u64 {
        match self {
            Op::TimelineCreate { .. } => 1,
            Op::Checkpoint { .. } => 2,
            Op::Merge(_) => 3,
            Op::RegisterCreate { .. } => 10,
            Op::RegisterSet { .. } => 11,
            Op::MapCreate { .. } => 20,
            Op::MapSet { .. } => 21,
            Op::MapDelete { .. } => 22,
            Op::TextCreate { .. } => 30,
            Op::TextEdit { .. } => 31,
            Op::TextSeal { .. } => 32,
        }
    }
}

/// A batch: a run of ops by one replica with consecutive counters (TL-11, TL-91).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch {
    /// The writer.
    pub replica: Replica,
    /// The first op's counter.
    pub start: u64,
    /// The parents of the first op.
    pub parents: Version,
    /// The timeline tag: `None` when the batch creates its timeline (TL-21).
    pub timeline: Option<OpId>,
    /// The ops, at least one.
    pub ops: Vec<Op>,
    /// The writer's RFC 3339 UTC time (informational).
    pub time: Option<String>,
    /// The tool call or step that caused the batch (informational).
    pub step: Option<String>,
}

impl Batch {
    /// The op id of op `i` of the batch.
    pub fn op_id(&self, i: usize) -> OpId {
        OpId::new(self.start + i as u64, self.replica.clone())
    }

    /// The counter of the batch's last op.
    pub fn end(&self) -> u64 {
        self.start + self.ops.len() as u64 - 1
    }
}

//! HAN work timelines: the engine of profile `han.timeline/1`.
//!
//! A [`History`] holds the signed op batches of one body of work (a repository's worktree, a
//! session's transcript, or other data). It integrates envelopes from peers in any order
//! ([`History::receive`]), writes local batches ([`History::write`], [`History::fork`],
//! [`History::merge`]), answers sync ([`History::hello`], [`History::answer`]), materialises any
//! version ([`History::state`]), and plans local text edits ([`History::plan_edit`]) and
//! three-way merges ([`History::plan_merge`]).
//!
//! The engine does no I/O: storage is a [`Store`], keys and roles come from a [`Directory`],
//! and signing from a [`Signer`].

mod cbor;
pub mod codec;
pub mod envelope;
mod error;
mod graph;
mod history;
mod ids;
mod local;
mod merge;
mod op;
mod state;
mod store;
mod sync;
mod text;
mod validate;
mod value;

pub use envelope::{Compression, Envelope, KeySigner, Signer};
pub use error::{Error, Reason, Report, Verdict};
pub use history::{At, Checkpoint, FaultProof, History, TimelineInfo, Written};
pub use ids::{
    BatchId, CharId, HistoryId, IdRange, InsertionId, ObjectId, OpId, ReplicaId, Version,
};
pub use merge::{Conflict, MergeDraft, MergePlan, Resolution, Side, TextHunk};
pub use op::{
    Batch, Change, ConflictKind, ConflictRecord, CopyInsert, HistoryKind, Insert, MergeOp, Op,
    ResolutionKind,
};
pub use state::{ObjectState, State, TreeConflicts};
pub use store::{Directory, KeyInfo, MemoryStore, Scope, Store};
pub use sync::{Answer, Hello, Inventory};
pub use text::{Anchor, Bias};
pub use value::Value;

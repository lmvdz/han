#![doc = include_str!("../README.md")]

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

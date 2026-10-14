//! The node API (PLAN section 5): two methods, `call(Command)` and `view(Query)`, as serde enums.
//!
//! Everything a face needs is here, and nothing a face must not have: no network, no storage, no keys. The
//! types are Rust-first; `bindings` generates the TypeScript the GUI uses (`apps/han-gui/src/gen/`).
//! New features add variants; the two methods never change.
//!
//! * [`Command`] ends as a [`CallResult`] or a [`NodeError`] from the closed set of the protocol's verdicts.
//! * [`Query`] opens a view; its stream starts with a [`ViewEvent::Snapshot`] of the window, then diffs.
//! * [`ViewMirror`] applies those events: a snapshot plus every later diff equals a fresh read.

mod bindings;
mod command;
mod error;
mod query;
mod rows;
mod stream;

pub use bindings::typescript;
pub use command::{CallResult, Command, CommandClass, HomeRef};
pub use error::NodeError;
pub use query::{Facets, Filter, Group, Query, Scope, Target, Window};
pub use rows::{
    ApprovalRow, BoardRow, CostRow, DiffOp, EffectRow, Fidelity, FilePatchRow, FileTouch, GuardRow,
    InputRow, JoinProgressRow, JoinStage, NodeStatusRow, OutboxRow, OverlapKind, OverlapParty,
    OverlapRow, PatchStatus, PlanItem, RoleInfo, Row, SUMMARY_FIELDS, SUMMARY_TEXT, Sandbox,
    SandboxBackend, SessionRow, SessionState, SpaceLink, SpaceStatusRow, StatusBody, TaskRow,
    Tests, TimelinePointRow, Tokens, ViewEvent, ViewMirror, WhoRow,
};
pub use stream::{BoxFuture, NodeApi, ViewSink, ViewStream};

/// The default size of a window.
pub const DEFAULT_LIMIT: u32 = 200;

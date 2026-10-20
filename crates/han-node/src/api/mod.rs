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
mod items;
mod preset;
mod query;
mod rows;
mod stream;

pub use bindings::typescript;
pub use command::{
    ApprovalEvidence, CallResult, Command, CommandClass, ConnectionClass, EvidenceRef, HomeRef,
};
pub use error::{APPROVAL_NOT_SIGNED, NodeError, Pending};
pub use items::{
    ActorKind, AdmissionRow, ChangeSummaryRow, DeviceRow, DeviceState, InputState, ItemBody,
    ItemRow, MessageRole, RecordRow, Span, ToolKind, ToolLocation, ToolStatus, ToolText,
    TurnDriver, masked_spans,
};
pub use preset::trusted_team_policy;
pub use query::{
    Facets, Filter, Group, MAX_LIMIT, MAX_SESSION_LIMIT, MAX_TIMELINE_LIMIT, Query, Scope,
    SessionPart, Target, Window, glob_match,
};
pub use rows::{
    AGENT_TEXT, AgentText, ApprovalRow, BoardRow, BoardTotals, BudgetStatus, ClaimKind, CostRow,
    DiffOp, EffectEnd, EffectRow, ErrorNote, Fidelity, FilePatchRow, FileTouch, GroupTotal,
    GuardRow, InputRow, JoinProgressRow, JoinStage, Needed, NeededGuard, NodeStatusRow, OutboxRow,
    OverlapClaim, OverlapConflict, OverlapFile, OverlapKind, OverlapParty, OverlapRow,
    OverlapSummary, PatchStatus, PlanItem, RECENT_ERRORS, RoleInfo, Row, SUMMARY_FIELDS,
    SUMMARY_TEXT, Sandbox, SandboxBackend, SessionRights, SessionRow, SessionState, SpaceLink,
    SpaceStatusRow, StateCounts, StatusBody, TaskBudget, TaskRow, Tests, TimelinePointRow, Tokens,
    ViewEvent, ViewMirror, WhoRow,
};
pub use stream::{BoxFuture, ClassApi, MAX_BACKLOG, NodeApi, ViewSink, ViewStream};

/// The default size of a window.
pub const DEFAULT_LIMIT: u32 = 200;

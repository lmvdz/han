//! TypeScript for the node API, generated from the Rust types with ts-rs. The GUI checks the output into
//! `apps/han-gui/src/gen/node-api.ts`; a test fails when the checked-in file drifts from the types.

use ts_rs::{Config, TS};

use super::command::{ApprovalEvidence, EvidenceRef};

use super::{
    ActorKind, AdmissionRow, AgentText, ApprovalRow, BoardRow, BoardTotals, BudgetStatus,
    CallResult, ChangeSummaryRow, ClaimKind, Command, CostRow, DeviceRow, DeviceState, DiffOp,
    EffectEnd, EffectRow, ErrorNote, Fidelity, FilePatchRow, FileTouch, Filter, Group, GroupTotal,
    GuardRow, HomeRef, InputRow, InputState, ItemBody, ItemRow, JoinProgressRow, JoinStage,
    MessageRole, Needed, NeededGuard, NodeError, NodeStatusRow, OutboxRow, OverlapClaim,
    OverlapConflict, OverlapFile, OverlapKind, OverlapParty, OverlapRow, OverlapSummary,
    PatchStatus, Pending, PlanItem, Query, RecordRow, RoleInfo, Row, Sandbox, SandboxBackend,
    Scope, SessionPart, SessionRights, SessionRow, SessionState, SpaceLink, SpaceStatusRow,
    StateCounts, StatusBody, Target, TaskBudget, TaskRow, Tests, TimelinePointRow, Tokens,
    ToolKind, ToolLocation, ToolStatus, ToolText, TrustedSetup, TurnDriver, ViewEvent, WhoRow,
    Window,
};

const HEADER: &str = "// Generated from crates/han-node by `cargo test -p han-node --test parts` (set HAN_UPDATE_GEN=1 to rewrite).\n// Do not edit by hand. These are the node API types: call(Command) and view(Query).\n\n";

/// The whole node API as one TypeScript module.
pub fn typescript() -> String {
    // JSON numbers: every u64 of the API is far below 2^53.
    let cfg = Config::new().with_large_int("number");
    let mut out = String::from(HEADER);
    macro_rules! decls {
        ($($t:ty),* $(,)?) => {
            $(
                out.push_str("export ");
                out.push_str(&<$t as TS>::decl(&cfg));
                out.push_str("\n\n");
            )*
        };
    }
    decls!(
        NodeError,
        Pending,
        HomeRef,
        EvidenceRef,
        ApprovalEvidence,
        Command,
        CallResult,
        Filter,
        Window,
        Group,
        Target,
        Scope,
        Query,
        SessionState,
        PlanItem,
        FileTouch,
        Tests,
        Tokens,
        BudgetStatus,
        TaskBudget,
        StatusBody,
        OverlapKind,
        OverlapParty,
        OverlapSummary,
        BoardRow,
        SessionRow,
        WhoRow,
        OverlapFile,
        ClaimKind,
        OverlapClaim,
        OverlapConflict,
        OverlapRow,
        ApprovalRow,
        EffectEnd,
        EffectRow,
        GuardRow,
        InputRow,
        TaskRow,
        SpaceLink,
        TrustedSetup,
        SpaceStatusRow,
        CostRow,
        SandboxBackend,
        Fidelity,
        Sandbox,
        OutboxRow,
        JoinStage,
        JoinProgressRow,
        RoleInfo,
        NodeStatusRow,
        TimelinePointRow,
        PatchStatus,
        FilePatchRow,
        Row,
        DiffOp,
        StateCounts,
        GroupTotal,
        BoardTotals,
        ViewEvent,
        SessionPart,
        SessionRights,
        Needed,
        NeededGuard,
        AgentText,
        ErrorNote,
        MessageRole,
        ToolKind,
        ToolStatus,
        ToolLocation,
        ToolText,
        TurnDriver,
        InputState,
        ItemBody,
        ItemRow,
        RecordRow,
        ActorKind,
        AdmissionRow,
        DeviceState,
        DeviceRow,
        ChangeSummaryRow,
    );
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

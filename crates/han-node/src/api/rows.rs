//! What a view yields: rows keyed and ordered by `key`, a snapshot of the window and then diffs.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::error::NodeError;

/// The state of a session as the board shows it. The first five are what a host claims in `han.status/1`;
/// `stale` and `failed` are derived by the node from silence (a status item's own `stale` counts as `idle`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum SessionState {
    /// Doing work.
    Working,
    /// An effect waits for an approval.
    WaitingApproval,
    /// Stuck.
    Blocked,
    /// Nothing to do right now.
    Idle,
    /// Finished.
    Done,
    /// No item for a while (derived).
    Stale,
    /// Silent for much longer, or reported failed.
    Failed,
}

impl SessionState {
    /// The state name as written in JSON.
    pub fn name(self) -> &'static str {
        match self {
            SessionState::Working => "working",
            SessionState::WaitingApproval => "waiting-approval",
            SessionState::Blocked => "blocked",
            SessionState::Idle => "idle",
            SessionState::Done => "done",
            SessionState::Stale => "stale",
            SessionState::Failed => "failed",
        }
    }

    /// Done or failed: silence changes nothing.
    pub fn is_terminal(self) -> bool {
        matches!(self, SessionState::Done | SessionState::Failed)
    }
}

/// One step of a plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct PlanItem {
    /// The step.
    pub text: String,
    /// Whether it is done.
    pub done: bool,
}

/// A file a session changed, with the changed line ranges `[start, len]` against `base`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct FileTouch {
    /// The path, relative to the repository.
    pub path: String,
    /// `[start, len]` pairs.
    #[serde(default)]
    pub hunks: Vec<(u32, u32)>,
}

/// Test counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Tests {
    /// Passing.
    pub pass: u32,
    /// Failing.
    pub fail: u32,
}

/// Token use and cost (in the space budget's unit, if any).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Tokens {
    /// Tokens in.
    #[serde(rename = "in")]
    pub input: u64,
    /// Tokens out.
    pub out: u64,
    /// Cost, in integer minor units.
    #[serde(default)]
    pub cost: u64,
}

/// Which sandbox confines a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SandboxBackend {
    /// Microsoft MXC.
    Mxc,
    /// bubblewrap.
    Bwrap,
    /// No sandbox.
    None,
}

/// Whether the sandbox's gates are enforced or only advisory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Fidelity {
    /// The sandbox enforces its limits.
    Enforced,
    /// The limits are a request, not a guarantee.
    Advisory,
}

/// A session's sandbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Sandbox {
    /// The backend.
    pub backend: SandboxBackend,
    /// How far it can be trusted.
    pub fidelity: Fidelity,
}

/// A `han.status/1` item (spec/profiles/han.status-1.md): where a live session is, as its host sees it. It
/// carries no authority. Unknown members are ignored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct StatusBody {
    /// The `session.open` entry id.
    pub session: String,
    /// The session's actor.
    pub agent: String,
    /// The actor whose task drives the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub delegate_of: Option<String>,
    /// The agent program.
    pub harness: String,
    /// The work's external reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub issue: Option<String>,
    /// The repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub repo: Option<String>,
    /// The commit the worktree started from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub base: Option<String>,
    /// The state the host claims.
    pub state: SessionState,
    /// The current step.
    pub step: String,
    /// The tool in use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tool: Option<String>,
    /// The plan.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plan: Vec<PlanItem>,
    /// Files changed so far.
    #[serde(default)]
    pub files: Vec<FileTouch>,
    /// Test counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tests: Option<Tests>,
    /// Token use and cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tokens: Option<Tokens>,
    /// A short summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub summary: Option<String>,
    /// The sandbox the host runs the session in. Not a member of `han.status/1` proper (unknown members are
    /// ignored, so it can ride along): a node-side extension the board shows (SR-99).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sandbox: Option<Sandbox>,
    /// A counter per session, starting at 1.
    pub seq: u64,
    /// The host's time.
    pub at: String,
}

/// Which kind of meeting an overlap is. Tiers 1 and 2 of PLAN 6.6 (tier 3, merge simulation, is later).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum OverlapKind {
    /// Two actors claim or work the same issue (tier 1).
    IssueClaim,
    /// Two live leases cover overlapping paths (tier 1).
    PathLease,
    /// Two sessions changed the same file (tier 2).
    SameFile,
    /// Two sessions changed overlapping line ranges of one file (tier 2).
    HunkRange,
}

impl OverlapKind {
    /// 1 for claims and leases, 2 for touched files and hunks.
    pub fn tier(self) -> u8 {
        match self {
            OverlapKind::IssueClaim | OverlapKind::PathLease => 1,
            OverlapKind::SameFile | OverlapKind::HunkRange => 2,
        }
    }
}

/// One side of an overlap.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
pub struct OverlapParty {
    /// The actor.
    pub actor: String,
    /// Its session, when the party is a session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub session: Option<String>,
}

/// A row of the board: one session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct BoardRow {
    /// Orders and identifies the row: the group, then the session.
    pub key: String,
    /// The space.
    pub space: String,
    /// The group the row is in (issue, person or state, as the query asked).
    pub group: String,
    /// The session's entry id.
    pub session: String,
    /// The session's actor.
    pub agent: String,
    /// The person behind it: the delegating actor, the task's author, or the actor itself.
    pub person: String,
    /// The harness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub harness: Option<String>,
    /// The issue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub issue: Option<String>,
    /// The repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub repo: Option<String>,
    /// The state, with silence applied.
    pub state: SessionState,
    /// The current step.
    pub step: String,
    /// The tool in use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tool: Option<String>,
    /// Files changed so far.
    pub files: Vec<FileTouch>,
    /// Kinds of overlap this session is part of (none: no badge).
    pub overlaps: Vec<OverlapKind>,
    /// Test counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tests: Option<Tests>,
    /// Token use and cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tokens: Option<Tokens>,
    /// A short summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub summary: Option<String>,
    /// The sandbox, when the host reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sandbox: Option<Sandbox>,
    /// This node's steady clock (ms) when the last item arrived; a face computes the age from its own clock.
    pub last_active_ms: u64,
    /// Whether the session is closed.
    pub closed: bool,
    /// The session waits on this device's actor (blocked or waiting for approval on its work).
    pub needs_me: bool,
    /// This device's actor may steer the session. The home's Policy decides when it is tried; this is the
    /// node's reading of the space's `sessions.steer` level.
    pub can_steer: bool,
    /// This device's actor may call the session's work off.
    pub can_cancel: bool,
}

/// A session in full.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionRow {
    /// The session's entry id.
    pub key: String,
    /// The space.
    pub space: String,
    /// The session's actor.
    pub actor: String,
    /// The receipt time of `session.open`.
    pub opened: String,
    /// The task it works.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub task: Option<String>,
    /// The task's external reference, if it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub external: Option<String>,
    /// Whether it is closed.
    pub closed: bool,
    /// The state, with silence applied.
    pub state: SessionState,
    /// The latest status item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub status: Option<StatusBody>,
    /// This node's steady clock (ms) when the last item arrived.
    pub last_active_ms: u64,
}

/// One session that is on an issue or a path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct WhoRow {
    /// Orders and identifies the row.
    pub key: String,
    /// The space.
    pub space: String,
    /// The actor.
    pub actor: String,
    /// The session, if the row is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub session: Option<String>,
    /// How the actor is on it: `claim`, `session` or `lease`.
    pub via: String,
    /// What matched: the issue or the path.
    pub subject: String,
    /// The session's state, for a session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub state: Option<SessionState>,
}

/// Two pieces of work meeting.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct OverlapRow {
    /// Orders and identifies the row.
    pub key: String,
    /// The space.
    pub space: String,
    /// The kind.
    pub kind: OverlapKind,
    /// 1 or 2.
    pub tier: u8,
    /// The issue or path.
    pub subject: String,
    /// The actors involved, in order.
    pub parties: Vec<OverlapParty>,
    /// What overlaps, in words.
    pub detail: String,
}

/// An effect waiting for approval.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ApprovalRow {
    /// Orders and identifies the row: the space and the proposal.
    pub key: String,
    /// The space.
    pub space: String,
    /// The proposal's entry id.
    pub proposal: String,
    /// The proposer.
    pub proposer: String,
    /// The executor.
    pub executor: String,
    /// The action.
    pub action: String,
    /// What the action works on, from the plan (`plan.target`): a repository, a path, an address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub target: Option<String>,
    /// The plan's top-level arguments that are plain values, as `(name, text)`, at most [`SUMMARY_FIELDS`]
    /// and each text cut to [`SUMMARY_TEXT`] characters. Text an agent wrote (a title, a message) is among
    /// them: a face shows it as quoted, not as its own words.
    #[serde(default)]
    pub summary: Vec<(String, String)>,
    /// The plan as JSON text, from the proposal's content (absent when redacted or not delivered).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plan: Option<String>,
    /// The digest of the plan.
    pub plan_digest: String,
    /// The node hashed the plan itself and `sha256(JCS(plan))` equals `plan_digest` (DAT-11). False when the
    /// plan is absent or does not hash to the digest: a face must not offer to sign it.
    pub digest_verified: bool,
    /// Authors of approvals so far.
    pub approvals: Vec<String>,
    /// The cost unit and amount, when the proposal has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cost: Option<(String, u64)>,
    /// No claim at or after this time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub expires_at: Option<String>,
    /// The seq of the proposal.
    pub seq: u64,
    /// The asking actor may give an approval that is worth trying (a member, not the proposer, not yet
    /// approved); Policy has the last word when it is appended.
    pub can_approve: bool,
    /// The asking actor may cancel the effect: its proposer, or an owner.
    pub can_cancel: bool,
    /// Who must approve: the actors and roles of the approval rule that applies, or the owners by default.
    pub approvers: Vec<String>,
    /// Why the effect is gated: which rule or default required an approval.
    pub why_gated: String,
}

/// At most this many arguments are in an [`ApprovalRow`]'s summary.
pub const SUMMARY_FIELDS: usize = 8;

/// Each text of an [`ApprovalRow`]'s summary is cut to this many characters.
pub const SUMMARY_TEXT: usize = 200;

/// A steering input (`Inputs` view).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct InputRow {
    /// Orders and identifies the row: the seq, zero-padded.
    pub key: String,
    /// The space.
    pub space: String,
    /// The session.
    pub session: String,
    /// The input's entry id.
    pub id: String,
    /// The input's seq.
    pub seq: u64,
    /// Its author.
    pub author: String,
    /// Its text (absent when redacted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub text: Option<String>,
}

/// A task (`Tasks` view).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TaskRow {
    /// Orders and identifies the row: the space and the seq, zero-padded.
    pub key: String,
    /// The space.
    pub space: String,
    /// The task's entry id.
    pub task: String,
    /// The seq of the task.
    pub seq: u64,
    /// Its author.
    pub author: String,
    /// Its assignees.
    pub to: Vec<String>,
    /// The work's external reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub external: Option<String>,
    /// The ask (content), when delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub ask: Option<String>,
    /// Whether it is still open.
    pub open: bool,
    /// P-15: the agents whose guarding the task carries, fixed when it was created: its parent's list, then
    /// the guards of its author. An assignee that accepts the task answers to their guards.
    #[serde(default)]
    pub guarded_for: Vec<String>,
    /// The assignees that accepted it.
    #[serde(default)]
    pub accepted: Vec<String>,
}

/// Whether an actor is guarded (`Guard` view).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct GuardRow {
    /// Orders and identifies the row: the space and the actor.
    pub key: String,
    /// The space.
    pub space: String,
    /// The actor.
    pub actor: String,
    /// It chose guarding, or is forced.
    pub guarded: bool,
    /// The owners' `forceGuarded` is on (it applies to non-humans).
    pub forced: bool,
    /// P-15, Z1: the agents whose guard the actor answers to: itself if it chose guarding or is forced, then
    /// the `guardedFor` of the open tasks it accepted, each once. `guarded` is true when this is not empty.
    #[serde(default)]
    pub guards: Vec<String>,
    /// The head the answer is at.
    pub head: u64,
}

/// An effect as an executor reads it (`Effects` view).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct EffectRow {
    /// Orders and identifies the row: the space and the proposal's seq, zero-padded.
    pub key: String,
    /// The space.
    pub space: String,
    /// The proposal's entry id.
    pub proposal: String,
    /// The proposer.
    pub proposer: String,
    /// The executor.
    pub executor: String,
    /// The action.
    pub action: String,
    /// The digest of the plan.
    pub plan_digest: String,
    /// The plan as JSON text, from the proposal's content (absent when redacted or not delivered).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plan: Option<String>,
    /// The task it is charged to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub task: Option<String>,
    /// `open`, `claimed` or `done` (a final phase, as far as this device folds it).
    pub phase: String,
    /// Authors of the open approvals.
    pub approvals: Vec<String>,
    /// How many claims it has had.
    pub claims: u64,
    /// The latest claim's entry id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_claim: Option<String>,
    /// The space's `launchWindow` in seconds.
    pub launch_window: u64,
    /// Whether a failed attempt may be retried.
    pub repeatable: bool,
    /// The cost unit and amount, when the proposal has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cost: Option<(String, u64)>,
    /// No claim at or after this time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub expires_at: Option<String>,
    /// The seq of the proposal.
    pub seq: u64,
    /// The head of the space when the row was made (an executor claims each effect at most once per head).
    pub head: u64,
}

/// How this device is connected to a space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SpaceLink {
    /// Dialing the home.
    Connecting,
    /// Following live; receipts verify.
    Live,
    /// The home cannot be reached; the views show what was last verified.
    Offline,
    /// A receipt did not verify; the device stopped following.
    Rejected,
}

/// How a space is doing on this device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SpaceStatusRow {
    /// The space.
    pub key: String,
    /// The space.
    pub space: String,
    /// The home's ServerName.
    pub home: String,
    /// The link.
    pub link: SpaceLink,
    /// The last verified seq.
    pub head: u64,
    /// Open sessions.
    pub sessions: u32,
    /// Actors holding the role owner or member.
    pub members: u32,
    /// Entries signed here and not yet ordered by the home.
    pub outbox_pending: u32,
    /// Signals received (for example joins), a counter.
    pub signals: u64,
    /// Why the space was rejected or is offline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
}

/// Tokens and cost of a session, or the total of a space.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct CostRow {
    /// Orders and identifies the row: `0` for the total, `1/<session>` for a session.
    pub key: String,
    /// The space.
    pub space: String,
    /// The session, or none for the total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub session: Option<String>,
    /// The session's actor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub agent: Option<String>,
    /// Tokens in.
    pub tokens_in: u64,
    /// Tokens out.
    pub tokens_out: u64,
    /// Cost, in the unit of the space's budget.
    pub cost: u64,
    /// That unit, if the space has a budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub unit: Option<String>,
}

/// An entry signed here and its way to the home (the outbox).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct OutboxRow {
    /// Orders and identifies the row: the outbox id, zero-padded.
    pub key: String,
    /// The outbox id `Queued` returned.
    pub outbox_id: u64,
    /// The space.
    pub space: String,
    /// The entry's id.
    pub entry_id: String,
    /// The entry's kind.
    pub kind: String,
    /// `pending`, `sent` or `rejected`.
    pub state: String,
    /// Delivery attempts so far.
    pub attempts: u32,
    /// The receipt's seq once sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub seq: Option<u64>,
    /// Why it was refused, or the last reason a try failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
}

/// Where joining a space stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum JoinStage {
    /// Dialing the home.
    Connecting,
    /// Connected; the space cannot be read yet: an owner has not admitted this actor.
    WaitingForAdmission,
    /// Admitted; catching up with the log.
    Syncing,
    /// Following live.
    Ready,
    /// A receipt did not verify; the join stopped.
    Rejected,
}

/// Progress of joining one space.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct JoinProgressRow {
    /// The space.
    pub key: String,
    /// The space.
    pub space: String,
    /// The stage.
    pub stage: JoinStage,
    /// The actor an owner must admit (this device's actor).
    pub actor: String,
    /// The last verified seq.
    pub head: u64,
    /// Why it is not further along.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
}

/// A child role the node supervises.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RoleInfo {
    /// The instance name.
    pub name: String,
    /// `running` or `exited`.
    pub state: String,
}

/// The node itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct NodeStatusRow {
    /// Always `node`.
    pub key: String,
    /// The actor this device acts as.
    pub actor: String,
    /// This device's node id (its public key).
    pub node_id: String,
    /// The node's version.
    pub version: String,
    /// Followed spaces.
    pub spaces: u32,
    /// Spaces that are following live.
    pub spaces_live: u32,
    /// Entries waiting in the outbox.
    pub outbox_pending: u32,
    /// Supervised roles.
    pub roles: Vec<RoleInfo>,
    /// This node's steady clock (ms), the one `last_active_ms` counts on, as of the last tick.
    pub now_ms: u64,
}

/// A point in a session's work timeline (backed by han-timeline, M4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TimelinePointRow {
    /// Orders and identifies the row: the point's position, zero-padded.
    pub key: String,
    /// The session.
    pub session: String,
    /// The point's id.
    pub point: String,
    /// A label (a step, a checkpoint, a tool call).
    pub label: String,
    /// When, as the host saw it.
    pub at: String,
    /// Files changed at this point.
    pub files: u32,
}

/// What happened to a file between two points.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum PatchStatus {
    /// New.
    Added,
    /// Changed.
    Modified,
    /// Gone.
    Deleted,
}

/// The patch of one file between two points of a timeline, or one file of a push plan or merge preview.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct FilePatchRow {
    /// Orders and identifies the row: the path.
    pub key: String,
    /// The path.
    pub path: String,
    /// Added, modified or deleted.
    pub status: PatchStatus,
    /// Unified diff text.
    pub patch: String,
    /// Lines added.
    pub added: u32,
    /// Lines removed.
    pub removed: u32,
}

/// A row of any view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "view", content = "row", rename_all = "snake_case")]
pub enum Row {
    /// A board row.
    Board(BoardRow),
    /// A session.
    Session(SessionRow),
    /// A `WhoIsOn` row.
    WhoIsOn(WhoRow),
    /// An overlap.
    Overlap(OverlapRow),
    /// A waiting approval.
    Approval(ApprovalRow),
    /// A space's status.
    SpaceStatus(SpaceStatusRow),
    /// A cost row.
    Cost(CostRow),
    /// An outbox entry.
    Outbox(OutboxRow),
    /// Join progress.
    JoinProgress(JoinProgressRow),
    /// The node's status.
    NodeStatus(NodeStatusRow),
    /// A timeline point.
    TimelinePoint(TimelinePointRow),
    /// A file patch.
    FilePatch(FilePatchRow),
    /// An effect with its plan.
    Effect(EffectRow),
    /// Whether an actor is guarded.
    Guard(GuardRow),
    /// A steering input.
    Input(InputRow),
    /// A task.
    Task(TaskRow),
}

impl Row {
    /// The key rows are ordered and diffed by.
    pub fn key(&self) -> &str {
        match self {
            Row::Board(r) => &r.key,
            Row::Session(r) => &r.key,
            Row::WhoIsOn(r) => &r.key,
            Row::Overlap(r) => &r.key,
            Row::Approval(r) => &r.key,
            Row::SpaceStatus(r) => &r.key,
            Row::Cost(r) => &r.key,
            Row::Outbox(r) => &r.key,
            Row::JoinProgress(r) => &r.key,
            Row::NodeStatus(r) => &r.key,
            Row::TimelinePoint(r) => &r.key,
            Row::FilePatch(r) => &r.key,
            Row::Effect(r) => &r.key,
            Row::Guard(r) => &r.key,
            Row::Input(r) => &r.key,
            Row::Task(r) => &r.key,
        }
    }
}

/// One change to the rows of a window. Rows are ordered by key, so `index` is a position in that order after
/// the earlier ops of the same diff are applied: removes first, then updates, then inserts by ascending index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum DiffOp {
    /// A row entered the window.
    Insert {
        /// Where it goes.
        index: u32,
        /// The row.
        row: Row,
    },
    /// A row changed in place.
    Update {
        /// The new row (same key).
        row: Row,
    },
    /// A row left the window.
    Remove {
        /// Its key.
        key: String,
    },
    /// A row moved without changing; reserved (a changed key is a remove and an insert).
    Move {
        /// Its key.
        key: String,
        /// Its new position.
        index: u32,
    },
}

/// What a view stream yields. The vocabulary is the same for every view: a snapshot, then diffs made of
/// insert, update, remove (and a reserved move) by key, and a resnapshot when a diff is not worth sending.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ViewEvent {
    /// The window as it is now, ordered by key. Always the first event.
    Snapshot {
        /// The rows.
        rows: Vec<Row>,
        /// How many rows match the filter in all (collapsed groups left out).
        total: u32,
        /// How many matching rows come before the first of this window.
        offset: u32,
        /// The key to put in `cursor` for the next page, when rows follow this window.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        next_cursor: Option<String>,
        /// The node's steady clock (ms) when the window was computed: the clock `last_active_ms` counts on,
        /// so a face computes ages as `now_ms - last_active_ms` without a clock of its own.
        #[serde(default)]
        now_ms: u64,
    },
    /// What changed in the window since the last event.
    Diff {
        /// The changes.
        ops: Vec<DiffOp>,
        /// The new total.
        total: u32,
        /// The new offset.
        offset: u32,
        /// The new cursor of the page after this window.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        next_cursor: Option<String>,
        /// The node's steady clock (ms) as of this diff.
        #[serde(default)]
        now_ms: u64,
    },
    /// Too much changed for a diff to be worth it: the window as it is now, replacing what the face holds.
    Resnapshot {
        /// The rows.
        rows: Vec<Row>,
        /// How many rows match the filter in all.
        total: u32,
        /// How many matching rows come before the first of this window.
        offset: u32,
        /// The key for the next page.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        next_cursor: Option<String>,
        /// The node's steady clock (ms) as of this window.
        #[serde(default)]
        now_ms: u64,
    },
    /// The view cannot be served; the stream ends.
    Failed {
        /// Why.
        error: NodeError,
    },
}

/// The rows a face holds for a view: apply every event in order, and `rows()` is what a fresh read gives.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewMirror {
    rows: Vec<Row>,
    total: u32,
    offset: u32,
    next_cursor: Option<String>,
    now_ms: u64,
    failed: Option<NodeError>,
}

impl ViewMirror {
    /// An empty mirror.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one event.
    pub fn apply(&mut self, event: ViewEvent) {
        match event {
            ViewEvent::Snapshot {
                rows,
                total,
                offset,
                next_cursor,
                now_ms,
            }
            | ViewEvent::Resnapshot {
                rows,
                total,
                offset,
                next_cursor,
                now_ms,
            } => {
                self.now_ms = now_ms;
                self.rows = rows;
                self.total = total;
                self.offset = offset;
                self.next_cursor = next_cursor;
            }
            ViewEvent::Diff {
                ops,
                total,
                offset,
                next_cursor,
                now_ms,
            } => {
                self.now_ms = now_ms;
                for op in ops {
                    match op {
                        DiffOp::Remove { key } => self.rows.retain(|r| r.key() != key),
                        DiffOp::Update { row } => {
                            if let Some(slot) = self.rows.iter_mut().find(|r| r.key() == row.key())
                            {
                                *slot = row;
                            }
                        }
                        DiffOp::Insert { index, row } => {
                            let at = (index as usize).min(self.rows.len());
                            self.rows.insert(at, row);
                        }
                        DiffOp::Move { key, index } => {
                            if let Some(from) = self.rows.iter().position(|r| r.key() == key) {
                                let row = self.rows.remove(from);
                                let at = (index as usize).min(self.rows.len());
                                self.rows.insert(at, row);
                            }
                        }
                    }
                }
                self.total = total;
                self.offset = offset;
                self.next_cursor = next_cursor;
            }
            ViewEvent::Failed { error } => self.failed = Some(error),
        }
    }

    /// The rows, in order.
    pub fn rows(&self) -> Vec<Row> {
        self.rows.clone()
    }

    /// How many rows match in all.
    pub fn total(&self) -> u32 {
        self.total
    }

    /// How many matching rows come before the window.
    pub fn offset(&self) -> u32 {
        self.offset
    }

    /// The node's clock (ms) as of the last event.
    pub fn now_ms(&self) -> u64 {
        self.now_ms
    }

    /// The cursor of the next page.
    pub fn next_cursor(&self) -> Option<&str> {
        self.next_cursor.as_deref()
    }

    /// The error the stream failed with, if it did.
    pub fn failed(&self) -> Option<&NodeError> {
        self.failed.as_ref()
    }
}

//! The landing lane (R33 §3, §4, §8): the first slice, a lane of one per `(repo, ref)`, through pull requests.
//!
//! The lane is a service of the executor that holds the repository's write credential. It takes the approved
//! `vcs.land` effects of that executor, one at a time per `(repo, ref)`, in the order of the entry that
//! completed their approval ([`han_node::api::EffectRow::approved_seq`]), and moves each through the states of
//! R33 §3:
//!
//! ```text
//! queued ─ prepare ─▶ testing ─ checks green ─▶ landing ─ claim + compare-and-swap ─▶ landed ─▶ confirmed
//!    ▲                   │                          │                                    │
//!    └── ref moved ──────┴──── ref moved ──────────┘                          post-merge red:
//!                        │                                                    stays landed, escalated
//!                        └─▶ rejected (conflict | test | dependency | policy)
//!
//! landed ─ post-merge red, `checks.auto_revert` ─▶ bisect ─▶ culprit ─▶ revert proposed ─▶ ... landed ─▶ reverted
//! ```
//!
//! * **queued**: the change waits for the lane's slot and for every `dependsOn` proposal to be `landed` or
//!   `confirmed` (a rejected or cancelled dependency rejects it, `dependency`; a cycle rejects its members).
//! * **prepare** (queued → testing): the lane imports the plan's bundle, fetches the ref's tip and
//!   squash-rebases the approved change onto it ([`han_vcs::Vcs::squash_rebase`]), deterministically: the
//!   proposer is author and committer, the commit time is the time of the approval entry, the message is the
//!   plan's `title` and the proposal id. A change that is not path-disjoint from the tip, or does not merge,
//!   is rejected (`conflict`) with the conflict record as a JSON blob (it is gated again, §2; the lane resolves
//!   nothing in this slice). Otherwise the lane pushes the commit to its lane branch
//!   (`<lane_branch_prefix><ref>/<proposal>`) and opens or finds the branch's pull request, so that CI runs on
//!   that exact commit.
//! * **testing**: the lane reads the commit's check runs through the [`Checks`] seam every `checks.poll_secs`.
//!   All of `checks.required` green moves on. One red is rejected (`test`) with the runs as evidence, after
//!   `checks.rerun` asks for a re-run. A required check that does not exist after `checks.appear_secs` makes the
//!   change *stalled*: GitHub runs no `pull_request` CI on a pull request that cannot merge (PR #99), so nothing
//!   is pending and nothing will happen.
//! * **landing**: the lane checks that the ref is still at the commit's parent (if not, it prepares again on the
//!   new tip), then claims the effect through the executor's normal claim path ([`Executor::claim_for_lane`]).
//!   The [`LaneInstruction`] is journalled before the claim's first send, so a restart can answer for it
//!   (X-06). The claim runs `vcs.land`, the compare-and-swap ref move. A lost compare-and-swap is reported
//!   `failed`/`no` (the ref was not touched, the effect opens again with its approvals, P-09) and the lane
//!   prepares again on the new tip.
//! * **landed → confirmed**: the lane reads the landed commit's `checks.full` runs. All green: `confirmed`. A
//!   repository with no `full` set confirms at once. A red run leaves the change `landed` and reports it
//!   `post_merge_failed` with the evidence, escalated.
//! * **landed → reverted** (A2, `checks.auto_revert`, off by default): a red run starts a bisect over the
//!   changes it covered, the landed ones since the last green run, in the order they landed. The lane asks the
//!   checks backend for `full` on the middle candidate and narrows to the first red one. Before that candidate
//!   is reverted the lane runs `full` on it once more: only a red that reproduces makes it the culprit. A green
//!   re-run marks it `flaky` (the item's `reason` and `flaky`), escalated and not reverted, and so does a
//!   backend that cannot run it again (the red is unconfirmed); the changes behind it wait with it. The re-run
//!   is journalled before it is asked for, so a restart reads it. The culprit's proposer gets a fix-forward
//!   `task` with the failing run as evidence (the first line of its ask names the culprit, so a task whose
//!   reply was lost is adopted, not made twice). The lane builds the
//!   revert as a squash commit whose tree is the tree the culprit landed on ([`han_vcs::Vcs::tree_commit`]), so
//!   its delta is the exact inverse of the culprit's, and proposes it as a `vcs.land` whose `change.revertOf`
//!   names the culprit (architecture.md P-17), with a bundle of that one commit. It approves the proposal itself
//!   with the failing run as evidence, through the node's narrow executor command `RevertApprove` (the node
//!   signs only for an open revert this actor proposed, on `han.ci/1` evidence over the reverted delta;
//!   `EffectApprove` stays closed to an executor); Policy accepts a machine's approval only at tier 0 and 1
//!   (the tier of a revert is its culprit's), so a revert of a tier 2 or 3 change, or of a never-auto path,
//!   waits for a human. In a budgeted space the revert names the task the culprit named, charged at the culprit's
//!   cost, while that task is open and budgeted (P-06, round 33). A proposal Policy refuses (the culprit's task is
//!   closed or absent, in a budgeted space) is not retried: the culprit is escalated with the reason and a
//!   person reverts it.
//!   The revert then goes the normal way (fast checks, compare-and-swap). If later changes touched its paths it
//!   conflicts: it stays queued, *stalled*, with the conflict record, and nothing is forced. When it lands the
//!   culprit becomes `reverted`; when its own post-merge run is green, the changes the red run covered that
//!   landed before it become `confirmed`. A revert whose own post-merge run is red is escalated, never reverted.
//!
//! **Check-only tries** (`vcs.try`, architecture.md P-17). The same machine runs a try: the lane squash-rebases the
//! approved change onto the ref's tip, pushes the commit to `han/try/<proposal>`, opens a *draft* pull request from
//! it (so that CI runs, as for a land) and tests it. A try has a group of its own, so it never takes the land slot of
//! its ref and a land never waits for it. Red or a conflict ends in the executor's `effect.cancel` with a `test:` or
//! `conflict:` reason and the evidence, as for a land. Green claims the effect through [`Executor::claim_for_lane`];
//! `vcs.try` verifies the lane's instruction, moves no ref, and the outcome is `succeeded` with the checks' record
//! as evidence (the state is `tried`). Whatever the end, the lane then closes the pull request and deletes the
//! branch; the record says so until it has (a restart carries on).
//!
//! **Liveness.** Every state has a deadline ([`crate::Deadlines`]). A change past it, or with nothing
//! pending, is *stalled* with its reason, and the item says so. The lane never waits silently.
//!
//! **The status stream.** The lane publishes `han.land/1` items (R33 §8) as profile stream items on a stream of
//! its own: one `session.open` of the executor's actor per space, titled `han.land lane`, opened the first
//! time the lane has something to say and remembered in the journal (`meta`, key `lane-session|<space>`).
//! There is no executor stream in the core, and a stream item needs a session (T-07). Two kinds of item travel
//! on it, told apart by `item`:
//!
//! * `change`: `{item, proposal, repo, ref, lane, state, position?, deadline?, stalled?, reason?, detail?,
//!   commit?, onto?, branch?, pr?, conflict?, evidence?, postMergeFailed?, escalated?, revertOf?, revertedBy?,
//!   revertProposal?, fixTask?, needsHuman?, flaky?, seq, at}`; `seq` counts one change's items from 1; `conflict` and
//!   `evidence` are `BlobRef`s;
//! * `lane`: `{item, repo, ref, instance, depth, batchSize, parallel, running: [{n, members}], seq, at}`.
//!
//! Records of the lane live in the executor's journal file (`lane_changes`), so a restart carries on.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use han_node::api::{ApprovalEvidence, CallResult, Command, EffectRow, EvidenceRef, NodeError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::actions::{REF_MOVED, check_branch, depends_cycle, write_auth};
use crate::checks::{CheckRequest, Checks, ChecksEvidence, Stage, Verdict, verdict_of};
use crate::executor::{LaneClaim, check_plan};
use crate::ledger::Report;
use crate::{ExecError, Executor, IdempotencyKey, LaneInstruction, PlanError, RepoTarget, Wall};

/// The profile the lane's items carry.
pub const PROFILE: &str = "han.land/1";

/// The branch prefix the lane pushes a check-only try's commit to: `han/try/<first 12 characters of the proposal id>`.
pub const TRY_BRANCH_PREFIX: &str = "han/try/";

/// How long a final change's item is sent again after a restart, in seconds (a week).
const FINAL_REPUBLISH_SECS: i64 = 7 * 86_400;

/// How often a change that is not final has its item sent again when its repository does not say.
pub const DEFAULT_REPUBLISH_SECS: u64 = 60;

/// The kind of record a revert's approval cites: the failing run, over the delta of the change reverted (A2).
pub const REVERT_EVIDENCE_KIND: &str = "han.ci/1";

/// A revert that conflicts with later changes is prepared again after this many seconds at the soonest.
const REVERT_RETRY_SECS: i64 = 300;

/// A lost claim attempt that did not start (not a lost compare-and-swap) is tried again this many times.
const MAX_NOT_STARTED: u32 = 3;

/// A change's state in the lane (R33 §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Waiting for the lane's slot or for a dependency.
    Queued,
    /// Prepared, pushed, and waiting for its required checks.
    Testing,
    /// Being claimed and landed.
    Landing,
    /// On the target ref.
    Landed,
    /// Landed, and the post-merge checks are green (or there are none).
    Confirmed,
    /// Landed, broke the post-merge run, and a revert of it has landed (A2).
    Reverted,
    /// Out, with a reason.
    Rejected,
    /// The proposal was cancelled before the lane landed it.
    Cancelled,
    /// A check-only try (`vcs.try`) whose checks were green and whose effect succeeded; nothing landed.
    Tried,
}

impl State {
    /// The state as the items name it.
    pub fn as_str(self) -> &'static str {
        match self {
            State::Queued => "queued",
            State::Testing => "testing",
            State::Landing => "landing",
            State::Landed => "landed",
            State::Confirmed => "confirmed",
            State::Reverted => "reverted",
            State::Rejected => "rejected",
            State::Cancelled => "cancelled",
            State::Tried => "tried",
        }
    }

    /// Whether nothing more happens to a change in this state.
    pub fn is_final(self) -> bool {
        matches!(
            self,
            State::Confirmed | State::Reverted | State::Rejected | State::Cancelled | State::Tried
        )
    }
}

/// What the lane prepared for a change: the commit it tests and lands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Prepared {
    /// The tip of the ref the commit sits on.
    onto: String,
    /// The squashed commit.
    commit: String,
    /// The lane branch it was pushed to (short name).
    branch: String,
    /// The repository that holds both.
    repository: String,
    /// The pull request, once there is one.
    #[serde(default)]
    pr: Option<String>,
}

/// A change a red post-merge run covered: a landed change, with the commit it landed as.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Cand {
    proposal: String,
    commit: String,
}

/// The re-run of `full` on the candidate culprit before it is reverted (A2): the red result has to reproduce.
/// Journalled before the backend is asked, so a restart reads the answer instead of asking twice.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Rerun {
    /// The candidate's commit.
    commit: String,
    /// The backend is being (or was) asked to run it again.
    #[serde(default)]
    asked: bool,
    /// What the backend answered: `false` when it cannot run it again. `None` until it answered (also after a
    /// restart in between: the run is then read as it is).
    #[serde(default)]
    accepted: Option<bool>,
    /// When it was asked (wall seconds).
    #[serde(default)]
    since: i64,
}

/// The search for the change that broke a red post-merge run (A2). The candidates are the landed changes up to
/// the red one, in the order they landed; the last is known red. `lo..=hi` holds the first red one: the middle
/// candidate is run, red narrows to `lo..=mid`, green to `mid+1..=hi`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Bisect {
    candidates: Vec<Cand>,
    lo: usize,
    hi: usize,
    /// The failing run (canonical JSON of a [`ChecksEvidence`]) of each commit known red.
    red: BTreeMap<String, String>,
    /// The commit the checks backend was last asked to run.
    #[serde(default)]
    started: Option<String>,
    /// When the current candidate's run was asked for (wall seconds).
    #[serde(default)]
    since: i64,
    /// The search is down to one candidate and `full` is run on it once more; only a red that reproduces makes
    /// it the culprit.
    #[serde(default)]
    rerun: Option<Rerun>,
    /// The culprit is found.
    #[serde(default)]
    done: bool,
}

/// What the lane has done about a change that broke the post-merge run (A2). Each step is kept as soon as it
/// is done, so a restart carries on and nothing is proposed or sent twice.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Culprit {
    /// When the culprit was found (wall seconds): the time of the revert commit.
    at: i64,
    /// The fix-forward task for the proposer.
    #[serde(default)]
    fix_task: Option<String>,
    /// The revert's proposal.
    #[serde(default)]
    revert: Option<String>,
    /// The lane approved the revert.
    #[serde(default)]
    approved: bool,
    /// The lane could not approve it (Policy needs a person: tier 2 or more, a never-auto path): it waits.
    #[serde(default)]
    needs_human: bool,
    /// Why the lane does not act, a person has to: the red did not reproduce (`flaky`), it could not be
    /// re-run, or the revert cannot be proposed (for example in a budgeted space). Nothing is dropped.
    #[serde(default)]
    held: Option<String>,
    /// The red did not reproduce on the re-run: the change is marked `flaky` and not reverted.
    #[serde(default)]
    flaky: bool,
}

/// The lane's record of one change, kept in the journal file.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Change {
    space: String,
    proposal: String,
    repo: String,
    refname: String,
    /// The seq of the entry that completed the approval.
    order: u64,
    proposer: String,
    depends_on: Vec<String>,
    launch_window: u64,
    state: State,
    /// When the change entered the state (wall seconds).
    entered: i64,
    /// Counts the change's items; one more on every change of what an item shows.
    seq: u64,
    /// The last `seq` that reached the stream.
    published: u64,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    detail: Option<String>,
    /// Why the change is stalled for a cause other than a deadline (nothing is pending).
    #[serde(default)]
    problem: Option<String>,
    /// Why the change is stalled now: `problem`, or the deadline.
    #[serde(default)]
    stalled: Option<String>,
    #[serde(default)]
    position: Option<u32>,
    #[serde(default)]
    prepared: Option<Prepared>,
    /// The evidence (canonical JSON of a [`ChecksEvidence`]) and, once uploaded, its `BlobRef`.
    #[serde(default)]
    evidence_json: Option<String>,
    #[serde(default)]
    evidence: Option<Value>,
    /// The conflict record (JSON) and, once uploaded, its `BlobRef`.
    #[serde(default)]
    conflict_json: Option<String>,
    #[serde(default)]
    conflict: Option<Value>,
    #[serde(default)]
    post_merge_failed: bool,
    #[serde(default)]
    reruns: u32,
    #[serde(default)]
    not_started: u32,
    #[serde(default)]
    retry_after: i64,
    /// When the change became stalled (wall seconds), while it is.
    #[serde(default)]
    stalled_since: Option<i64>,
    /// The rejection has not reached the log yet: the executor's `effect.cancel` is still to be sent.
    #[serde(default)]
    cancel_pending: bool,
    /// The order the change landed in, per lane (1, 2, ...): the order a red run's candidates are in.
    #[serde(default)]
    landed_seq: u64,
    /// A revert (A2): the proposal it undoes.
    #[serde(default)]
    revert_of: Option<String>,
    /// The search this change's red run started (A2).
    #[serde(default)]
    bisect: Option<Bisect>,
    /// This change broke the post-merge run (A2).
    #[serde(default)]
    culprit: Option<Culprit>,
    /// This change's red run is the culprit's doing: the proposal it is waiting on (A2).
    #[serde(default)]
    covered_by: Option<String>,
    /// The revert that undid this change, once it landed (A2).
    #[serde(default)]
    reverted_by: Option<String>,
    /// A check-only try (`vcs.try`): it runs the checks and lands nothing, in a group of its own.
    #[serde(default)]
    check_only: bool,
    /// The try is over and its pull request and branch are still to be removed.
    #[serde(default)]
    cleanup: bool,
    /// The record as last written (not itself written).
    #[serde(skip)]
    saved: String,
}

/// What a change looks like from outside, for tests and logs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeView {
    /// The proposal.
    pub proposal: String,
    /// The state.
    pub state: State,
    /// The rejection reason (`conflict`, `test`, `dependency`, `policy`) or `post_merge_failed`.
    pub reason: Option<String>,
    /// Why it is stalled, if it is.
    pub stalled: Option<String>,
    /// How many items it has published.
    pub seq: u64,
}

/// What one pass did that someone has to be told about.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PassReport {
    /// `(proposal, why)` changes that became stalled in this pass.
    pub stalled: Vec<(String, String)>,
    /// `(proposal, why)` changes escalated in this pass (a red post-merge run).
    pub escalated: Vec<(String, String)>,
}

#[derive(Default)]
struct Mem {
    /// Proposal (and stage) → wall time of its last backend read.
    last_poll: HashMap<String, i64>,
    /// Lane key → the last lane item's content, to publish on change only.
    lane_items: HashMap<String, String>,
    /// Space → the lane's stream (session entry id).
    sessions: HashMap<String, String>,
    /// A claim was made since the effects were read.
    rows_stale: bool,
    /// Proposal → wall time its item last reached the stream in this process. Empty after a restart, which is
    /// what makes every change's item go out once more.
    last_item: HashMap<String, i64>,
}

/// The landing lane of an executor.
pub struct Lane {
    exec: Arc<Executor>,
    checks: Arc<dyn Checks>,
    wall: Arc<dyn Wall>,
    mem: Mutex<Mem>,
}

/// The longest `reason` an `effect.cancel` takes (UTF-8 bytes, architecture.md P-07a).
const MAX_REASON_BYTES: usize = 1024;

/// A `BlobRef` as the node API takes it.
fn evidence_ref(r: &Value) -> Option<EvidenceRef> {
    Some(EvidenceRef {
        hash: r.get("hash")?.as_str()?.to_owned(),
        size: r.get("size")?.as_u64()?,
        media_type: r.get("type")?.as_str()?.to_owned(),
    })
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A HAN Time from epoch seconds.
fn format_time(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn canonical(v: &Value) -> Result<String, String> {
    let core = han_core::json::parse(v.to_string().as_bytes()).map_err(|e| e.to_string())?;
    let bytes = han_core::json::canonicalize(&core).map_err(|e| e.to_string())?;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

fn arg<'a>(plan: &'a Value, k: &str) -> &'a str {
    plan.get("args")
        .and_then(|a| a.get(k))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn short(s: &str) -> &str {
    &s[..s.len().min(8)]
}

/// `Name <email>` for a proposer: its actor id, and that id as an address.
fn author_of(proposer: &str) -> String {
    let clean: String = proposer.chars().filter(|c| !"<>\n".contains(*c)).collect();
    let email = clean.trim_start_matches('@').replacen(':', "@", 1);
    format!("{clean} <{email}>")
}

/// The lane branch of a change: `<prefix><ref, slashes as dashes>/<first 12 characters of the proposal id>`.
fn lane_branch(repo: &RepoTarget, refname: &str, proposal: &str) -> String {
    let r: String = refname
        .strip_prefix("refs/heads/")
        .unwrap_or(refname)
        .chars()
        .map(|c| if c == '/' { '-' } else { c })
        .collect();
    let p: String = proposal
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(12)
        .collect();
    format!("{}{r}/{p}", repo.lane_branch_prefix)
}

/// The branch of a check-only try: `han/try/<first 12 characters of the proposal id>`.
fn try_branch(proposal: &str) -> String {
    let p: String = proposal
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(12)
        .collect();
    format!("{TRY_BRANCH_PREFIX}{p}")
}

/// The number of the pull request in `pull request #N URL`.
fn pr_number(pr: &str) -> Option<u64> {
    let digits: String = pr
        .split_once('#')?
        .1
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// What a plan says the lane needs: the repository, the ref, the proposals it depends on and, for a revert, the
/// proposal it undoes.
fn plan_info(row: &EffectRow) -> Result<(String, String, Vec<String>, Option<String>), String> {
    let plan: Value =
        serde_json::from_str(row.plan.as_deref().ok_or("the proposal carries no plan")?)
            .map_err(|e| format!("the plan is not JSON: {e}"))?;
    let repo = plan
        .get("target")
        .and_then(Value::as_str)
        .ok_or("the plan names no target")?
        .to_owned();
    let refname = arg(&plan, "ref");
    if refname.is_empty() {
        return Err("the plan names no ref".into());
    }
    let deps = plan
        .get("args")
        .and_then(|a| a.get("dependsOn"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|d| d.get("proposal").and_then(Value::as_str).map(str::to_owned))
        .collect();
    let revert_of = plan
        .pointer("/args/change/revertOf")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok((repo, refname.to_owned(), deps, revert_of))
}

enum Prep {
    Ready,
    Done,
    Retry(String),
    /// A revert that cannot be applied to the tip: stalled with the conflict record, the lane goes on.
    Stalled(String),
}

/// Why the lane's proposal of a revert did not go through.
enum Proposal {
    /// Policy refuses it (or finds it invalid), and will again: a person has to revert the change.
    Refused(String),
    /// Not now: try again.
    Retry(String),
}

impl From<String> for Proposal {
    fn from(s: String) -> Self {
        Proposal::Retry(s)
    }
}

impl From<&str> for Proposal {
    fn from(s: &str) -> Self {
        Proposal::Retry(s.to_owned())
    }
}

/// Why a candidate culprit is not reverted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    /// The red did not reproduce.
    Flaky,
    /// The backend could not run it again, so the red was not confirmed.
    Unconfirmed,
}

/// Why the lane's approval of its own revert did not go through.
enum Approval {
    /// Policy (or the node) refuses a machine's approval: a person has to approve.
    Human(String),
    /// Not now: try again.
    Retry(String),
}

enum Deps {
    Ready,
    Waiting(String),
    Rejected(String),
}

impl Lane {
    /// A lane for `exec`'s `vcs.land` effects, reading checks through `checks`. It does nothing for a
    /// repository with no `landing_refs`.
    pub fn new(exec: Arc<Executor>, checks: Arc<dyn Checks>, wall: Arc<dyn Wall>) -> Lane {
        Lane {
            exec,
            checks,
            wall,
            mem: Mutex::new(Mem::default()),
        }
    }

    /// Whether the executor's configuration turns any lane on.
    pub fn configured(config: &crate::ExecConfig) -> bool {
        config.repos.iter().any(|r| !r.landing_refs.is_empty())
    }

    fn repo(&self, name: &str) -> Option<RepoTarget> {
        self.exec
            .config()
            .repo(name)
            .filter(|r| !r.landing_refs.is_empty())
            .cloned()
    }

    // ───────────────────────── persistence and publishing ─────────────────────────

    fn load(&self) -> Result<Vec<Change>, ExecError> {
        let rows = self.exec.with_ledger(|l| l.lane_all())?;
        let mut out: Vec<Change> = rows
            .into_iter()
            .filter_map(|(_, _, text)| {
                let mut c: Change = serde_json::from_str(&text).ok()?;
                c.saved = serde_json::to_string(&c).ok()?;
                Some(c)
            })
            .collect();
        out.sort_by(|a, b| (a.order, &a.proposal).cmp(&(b.order, &b.proposal)));
        Ok(out)
    }

    fn put(&self, c: &mut Change) -> Result<(), ExecError> {
        let text = serde_json::to_string(c).map_err(|e| crate::LedgerError::Db(e.to_string()))?;
        self.exec
            .with_ledger(|l| l.lane_put(&c.space, &c.proposal, &text))?;
        c.saved = text;
        Ok(())
    }

    /// Writes the change if it differs from what was written, with the next `seq`, and publishes it.
    async fn checkpoint(&self, c: &mut Change) -> Result<(), ExecError> {
        let now = serde_json::to_string(c).map_err(|e| crate::LedgerError::Db(e.to_string()))?;
        if now != c.saved {
            c.seq += 1;
            self.put(c)?;
        }
        self.publish(c).await?;
        self.send_cancel(c).await?;
        self.clean_try(c).await
    }

    /// Removes what a finished try left on the remote: its pull request is closed and its branch deleted. It is
    /// kept in the record (`cleanup`) until both are done, so a failure or a restart tries again; a branch that
    /// is not there is already gone.
    async fn clean_try(&self, c: &mut Change) -> Result<(), ExecError> {
        if !c.cleanup {
            return Ok(());
        }
        let (Some(p), Some(repo)) = (
            c.prepared.clone(),
            self.exec.config().repo(&c.repo).cloned(),
        ) else {
            c.cleanup = false;
            return self.put(c);
        };
        let mut done = true;
        if let (Some(pr), Some(gh), Some(minter)) =
            (p.pr.as_deref(), repo.github.as_deref(), self.exec.github())
            && let Some(n) = pr_number(pr)
            && let Err(e) = crate::actions::close_pr(minter, gh, n).await
        {
            tracing::warn!(proposal = %c.proposal, "lane: closing the try's pull request: {e}");
            done = false;
        }
        let key = IdempotencyKey {
            space: c.space.clone(),
            proposal: c.proposal.clone(),
            attempt: 0,
        };
        let ctx = self.exec.plain_ctx(&key);
        let plan = json!({"target": c.repo});
        match write_auth(&plan, &ctx).await {
            Err(e) => {
                tracing::warn!(proposal = %c.proposal, "lane: the try's branch cannot be deleted: {e}");
                done = false;
            }
            Ok(auth) => {
                let vcs = ctx.vcs.clone();
                let mut dir = PathBuf::from(&p.repository);
                if !dir.exists() {
                    match vcs.fetch_ref(&repo.remote, auth.as_ref(), &c.refname) {
                        Ok(Some((d, _))) => dir = d,
                        _ => dir = PathBuf::new(),
                    }
                }
                let full = format!("refs/heads/{}", p.branch);
                if dir.as_os_str().is_empty() {
                    tracing::warn!(proposal = %c.proposal, "lane: no repository to delete {full} from; it stays");
                } else if let Err(e) = vcs.delete_branch(&dir, &repo.remote, &full, auth.as_ref()) {
                    tracing::warn!(proposal = %c.proposal, "lane: deleting {full}: {e}");
                    done = false;
                }
            }
        }
        if done {
            c.cleanup = false;
            self.put(c)?;
        }
        Ok(())
    }

    async fn publish(&self, c: &mut Change) -> Result<(), ExecError> {
        if c.published >= c.seq {
            return Ok(());
        }
        // Content the item refers to goes up first; if it cannot, the item waits.
        if c.evidence.is_none()
            && let Some(text) = c.evidence_json.clone()
        {
            match self.upload(&c.space, &text).await {
                Some(r) => c.evidence = Some(r),
                None => return Ok(()),
            }
        }
        if c.conflict.is_none()
            && let Some(text) = c.conflict_json.clone()
        {
            match self.upload(&c.space, &text).await {
                Some(r) => c.conflict = Some(r),
                None => return Ok(()),
            }
        }
        let body = self.body(c);
        if self.stream(&c.space, body).await {
            c.published = c.seq;
            lock(&self.mem)
                .last_item
                .insert(c.proposal.clone(), self.wall.now_secs());
        }
        self.put(c)
    }

    /// Sends the change's current item again, with its unchanged `seq`: the home does not replay stream items,
    /// so a host that was away when the item went out, or that never saw it, learns the state from the next
    /// one. A change that is not final is sent again every `republish_secs`; every change, final ones too, is
    /// sent once after a restart (a final one only while it is younger than [`FINAL_REPUBLISH_SECS`]: older
    /// ones are in the log, which the host reads as well).
    async fn republish(&self, c: &Change, now: i64) {
        if c.seq == 0 || c.published < c.seq {
            return; // `publish` is still to send it
        }
        let last = lock(&self.mem).last_item.get(&c.proposal).copied();
        let every = self
            .exec
            .config()
            .repo(&c.repo)
            .map_or(DEFAULT_REPUBLISH_SECS, |r| r.republish_secs);
        let due = match last {
            None => !c.state.is_final() || now - c.entered <= FINAL_REPUBLISH_SECS,
            Some(t) => {
                !c.state.is_final()
                    && every > 0
                    && now - t >= i64::try_from(every).unwrap_or(i64::MAX)
            }
        };
        if due && self.stream(&c.space, self.body(c)).await {
            lock(&self.mem).last_item.insert(c.proposal.clone(), now);
        }
    }

    async fn upload(&self, space: &str, text: &str) -> Option<Value> {
        self.upload_bytes(space, "application/json", text.as_bytes())
            .await
    }

    async fn upload_bytes(&self, space: &str, media_type: &str, bytes: &[u8]) -> Option<Value> {
        let r = self
            .exec
            .node()
            .call(Command::BlobPut {
                space: space.to_owned(),
                media_type: media_type.to_owned(),
                data: han_core::base64url::encode(bytes),
            })
            .await;
        match r {
            Ok(CallResult::BlobRef {
                hash,
                size,
                media_type,
            }) => Some(json!({"hash": hash, "size": size, "type": media_type})),
            Ok(_) => None,
            Err(e) => {
                tracing::warn!("lane: uploading a blob: {e}");
                None
            }
        }
    }

    /// The lane's stream in a space: its `session.open`, made the first time and remembered.
    async fn session(&self, space: &str) -> Option<String> {
        if let Some(s) = lock(&self.mem).sessions.get(space) {
            return Some(s.clone());
        }
        let key = format!("lane-session|{space}");
        let stored = self.exec.with_ledger(|l| l.meta_get(&key)).ok().flatten();
        let id = match stored {
            Some(id) => id,
            None => {
                let r = self
                    .exec
                    .node()
                    .call(Command::SessionOpen {
                        space: space.to_owned(),
                        steer_mode: None,
                        task: None,
                        title: Some("han.land lane".into()),
                    })
                    .await;
                let CallResult::Receipt { id, .. } = r.ok()? else {
                    return None;
                };
                self.exec.with_ledger(|l| l.meta_put(&key, &id)).ok()?;
                id
            }
        };
        lock(&self.mem)
            .sessions
            .insert(space.to_owned(), id.clone());
        Some(id)
    }

    async fn stream(&self, space: &str, body: Value) -> bool {
        let Some(session) = self.session(space).await else {
            return false;
        };
        let r = self
            .exec
            .node()
            .call(Command::StreamPublish {
                space: space.to_owned(),
                session,
                name: PROFILE.into(),
                body: body.to_string(),
            })
            .await;
        match r {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!("lane: publishing a {PROFILE} item: {e}");
                false
            }
        }
    }

    fn deadline_of(&self, c: &Change) -> Option<i64> {
        let repo = self.exec.config().repo(&c.repo)?;
        let d = &repo.deadlines;
        let secs = match c.state {
            State::Queued => d
                .queued_secs
                .saturating_mul(u64::from(c.position.unwrap_or(1).max(1))),
            State::Testing => d.testing_secs,
            State::Landing => d.landing_secs.unwrap_or(c.launch_window),
            State::Landed if !c.post_merge_failed => d.landed_secs,
            _ => return None,
        };
        Some(
            c.entered
                .saturating_add(i64::try_from(secs).unwrap_or(i64::MAX)),
        )
    }

    /// The `han.land/1` item of a change.
    fn body(&self, c: &Change) -> Value {
        let mut b = json!({
            "item": "change",
            "proposal": c.proposal,
            "repo": c.repo,
            "ref": c.refname,
            "lane": format!("{}#{}", c.repo, c.refname),
            "state": c.state.as_str(),
            "seq": c.seq,
            "at": format_time(self.wall.now_secs()),
        });
        let o = b.as_object_mut().expect("an object");
        if c.check_only {
            o.insert("try".into(), json!(true));
        }
        if let Some(p) = c.position {
            o.insert("position".into(), json!(p));
        }
        if let Some(d) = self.deadline_of(c) {
            o.insert("deadline".into(), json!(format_time(d)));
        }
        if let Some(s) = &c.stalled {
            o.insert("stalled".into(), json!(s));
        }
        if let Some(r) = &c.reason {
            o.insert("reason".into(), json!(r));
        }
        if let Some(d) = &c.detail {
            o.insert("detail".into(), json!(d));
        }
        if let Some(p) = &c.prepared {
            o.insert("commit".into(), json!(p.commit));
            o.insert("onto".into(), json!(p.onto));
            o.insert("branch".into(), json!(p.branch));
            if let Some(pr) = &p.pr {
                o.insert("pr".into(), json!(pr));
            }
        }
        if let Some(r) = &c.conflict {
            o.insert("conflict".into(), r.clone());
        }
        if let Some(r) = &c.evidence {
            o.insert("evidence".into(), r.clone());
        }
        if c.post_merge_failed {
            o.insert("postMergeFailed".into(), json!(true));
            o.insert("escalated".into(), json!(true));
        }
        if let Some(r) = &c.revert_of {
            o.insert("revertOf".into(), json!(r));
        }
        if let Some(r) = &c.reverted_by {
            o.insert("revertedBy".into(), json!(r));
        }
        if let Some(cu) = &c.culprit {
            if let Some(r) = &cu.revert {
                o.insert("revertProposal".into(), json!(r));
            }
            if let Some(t) = &cu.fix_task {
                o.insert("fixTask".into(), json!(t));
            }
            if cu.needs_human || cu.held.is_some() {
                o.insert("needsHuman".into(), json!(true));
            }
            if cu.flaky {
                o.insert("flaky".into(), json!(true));
            }
        }
        b
    }

    // ───────────────────────── transitions ─────────────────────────

    async fn enter(&self, c: &mut Change, state: State, now: i64) -> Result<(), ExecError> {
        c.state = state;
        if c.check_only && state.is_final() && c.prepared.is_some() {
            c.cleanup = true;
        }
        c.entered = now;
        c.problem = None;
        c.stalled = None;
        c.stalled_since = None;
        c.position = None;
        c.retry_after = 0;
        self.checkpoint(c).await
    }

    async fn reject(
        &self,
        c: &mut Change,
        reason: &str,
        detail: &str,
        evidence: Option<String>,
        conflict: Option<String>,
        now: i64,
    ) -> Result<(), ExecError> {
        tracing::warn!(proposal = %c.proposal, reason, detail, "lane: rejected");
        c.reason = Some(reason.to_owned());
        c.detail = Some(detail.to_owned());
        if let Some(e) = evidence {
            c.evidence_json = Some(e);
            c.evidence = None;
        }
        if let Some(e) = conflict {
            c.conflict_json = Some(e);
            c.conflict = None;
        }
        lock(&self.mem).last_poll.remove(&c.proposal);
        c.cancel_pending = true;
        self.enter(c, State::Rejected, now).await
    }

    /// Ends a rejected change's effect in the log: the executor's own `effect.cancel` (P-07a) with the
    /// reason and the proof (the conflict record, or the checks that failed). It waits while the node is
    /// unavailable and is dropped when the log refuses it (the effect is no longer open, or no longer ours):
    /// the lane's record is final either way.
    async fn send_cancel(&self, c: &mut Change) -> Result<(), ExecError> {
        if !c.cancel_pending || c.state != State::Rejected {
            return Ok(());
        }
        if c.conflict.is_none()
            && let Some(text) = c.conflict_json.clone()
        {
            match self.upload(&c.space, &text).await {
                Some(r) => c.conflict = Some(r),
                None => return Ok(()),
            }
        }
        if c.evidence.is_none()
            && let Some(text) = c.evidence_json.clone()
        {
            match self.upload(&c.space, &text).await {
                Some(r) => c.evidence = Some(r),
                None => return Ok(()),
            }
        }
        let proof = c
            .conflict
            .as_ref()
            .or(c.evidence.as_ref())
            .and_then(evidence_ref);
        let mut reason = format!(
            "{}: {}",
            c.reason.as_deref().unwrap_or("rejected"),
            c.detail.as_deref().unwrap_or("")
        );
        if reason.len() > MAX_REASON_BYTES {
            let mut end = MAX_REASON_BYTES;
            while !reason.is_char_boundary(end) {
                end -= 1;
            }
            reason.truncate(end);
        }
        let sent = self
            .exec
            .node()
            .call(Command::EffectCancel {
                space: c.space.clone(),
                proposal: c.proposal.clone(),
                reason: Some(reason),
                evidence: proof,
            })
            .await;
        match sent {
            Err(NodeError::Unavailable { .. }) => {
                tracing::warn!(proposal = %c.proposal, "lane: the cancel is not sent yet; retrying");
                return Ok(());
            }
            Err(e) => {
                tracing::warn!(proposal = %c.proposal, "lane: the cancel was refused: {e}");
            }
            Ok(_) => {}
        }
        c.cancel_pending = false;
        self.put(c)
    }

    /// Whether a change in `testing` that is stalled and past its deadline steps aside, so that the changes
    /// behind it go on. It keeps its place in the log and its checks; it holds the slot again if it unstalls.
    fn yielding(&self, c: &Change, now: i64) -> bool {
        c.state == State::Testing
            && c.stalled.is_some()
            && self.deadline_of(c).is_some_and(|d| now > d)
    }

    /// Back to the queue, to be prepared again on the ref's new tip.
    async fn requeue(&self, c: &mut Change, detail: &str, now: i64) -> Result<(), ExecError> {
        tracing::info!(proposal = %c.proposal, detail, "lane: prepared again");
        c.prepared = None;
        c.evidence_json = None;
        c.evidence = None;
        c.reruns = 0;
        c.detail = Some(detail.to_owned());
        lock(&self.mem).last_poll.remove(&c.proposal);
        self.enter(c, State::Queued, now).await
    }

    // ───────────────────────── the pass ─────────────────────────

    /// What the changes look like now.
    pub fn changes(&self) -> Result<Vec<ChangeView>, ExecError> {
        Ok(self
            .load()?
            .into_iter()
            .map(|c| ChangeView {
                proposal: c.proposal,
                state: c.state,
                reason: c.reason,
                stalled: c.stalled,
                seq: c.seq,
            })
            .collect())
    }

    /// Runs passes every second until `shutdown` turns true.
    pub async fn run(&self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        loop {
            if let Err(e) = self.pass().await {
                tracing::warn!("lane pass: {e}");
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                _ = shutdown.changed() => {}
            }
            if *shutdown.borrow() {
                return;
            }
        }
    }

    /// One pass over the effects the node shows now and the changes the lane keeps.
    pub async fn pass(&self) -> Result<PassReport, ExecError> {
        let mut rows = self.exec.effects().await?;
        lock(&self.mem).rows_stale = false;
        let now = self.wall.now_secs();
        let mut rep = PassReport::default();
        self.discover(&rows, now).await?;
        let mut all = self.load()?;

        // Proposals cancelled before the lane claimed them.
        for item in &mut all {
            if item.state.is_final() {
                continue;
            }
            let Some(row) = rows
                .iter()
                .find(|r| r.space == item.space && r.proposal == item.proposal)
            else {
                continue;
            };
            if row.phase == "done" && row.claims == 0 {
                let mut c = item.clone();
                c.detail = Some("the proposal was cancelled".into());
                self.enter(&mut c, State::Cancelled, now).await?;
                *item = c;
            }
        }

        // A dependsOn cycle rejects everything on it.
        loop {
            let edges: BTreeMap<String, Vec<String>> = all
                .iter()
                .filter(|c| !c.state.is_final())
                .map(|c| (c.proposal.clone(), c.depends_on.clone()))
                .collect();
            let Some(cycle) = depends_cycle(&edges) else {
                break;
            };
            let text = cycle.join(" -> ");
            for item in &mut all {
                if cycle.contains(&item.proposal) && !item.state.is_final() {
                    let mut c = item.clone();
                    self.reject(
                        &mut c,
                        "dependency",
                        &format!("dependsOn cycle: {text}"),
                        None,
                        None,
                        now,
                    )
                    .await?;
                    *item = c;
                }
            }
        }

        // A check-only try has a group of its own, so it never takes the land slot of its ref.
        let mut groups: BTreeMap<(String, String, String, bool), Vec<usize>> = BTreeMap::new();
        for (i, c) in all.iter().enumerate() {
            if !c.state.is_final() {
                groups
                    .entry((
                        c.space.clone(),
                        c.repo.clone(),
                        c.refname.clone(),
                        c.check_only,
                    ))
                    .or_default()
                    .push(i);
            }
        }
        for idxs in groups.values() {
            // A change that moved on (a lost compare-and-swap prepared again, a landing that freed the slot)
            // lets the next step run in the same pass; a few rounds bound it.
            for _ in 0..4 {
                let before: u64 = idxs.iter().map(|&i| all[i].seq).sum();
                self.group(&mut all, idxs, &mut rows, now, &mut rep).await?;
                if idxs.iter().map(|&i| all[i].seq).sum::<u64>() == before {
                    break;
                }
            }
        }

        // Items that did not get through, and the lane items.
        for c in &mut all {
            if c.published < c.seq {
                self.publish(c).await?;
            }
            self.send_cancel(c).await?;
            self.clean_try(c).await?;
            self.republish(c, now).await;
        }
        self.lane_items(&all, now).await;
        Ok(rep)
    }

    /// Makes a record for every approved `vcs.land` effect of a configured repository that has none, and for
    /// every open `vcs.try` that needs no approval (Policy says the proposer suffices) or has one.
    async fn discover(&self, rows: &[EffectRow], now: i64) -> Result<(), ExecError> {
        let known: Vec<(String, String)> = self
            .exec
            .with_ledger(|l| l.lane_all())?
            .into_iter()
            .map(|(s, p, _)| (s, p))
            .collect();
        for row in rows {
            if !matches!(row.action.as_str(), "vcs.land" | "vcs.try")
                || row.phase != "open"
                || (row.approvals.is_empty() && !row.proposer_suffices)
                || known.contains(&(row.space.clone(), row.proposal.clone()))
            {
                continue;
            }
            let info = plan_info(row);
            let (repo, refname, deps, revert_of) = match &info {
                Ok(i) => i.clone(),
                Err(_) => (String::new(), String::new(), Vec::new(), None),
            };
            // Default-off: a repository with no landing refs has no lane.
            if info.is_ok() && self.repo(&repo).is_none() {
                continue;
            }
            let mut c = Change {
                space: row.space.clone(),
                proposal: row.proposal.clone(),
                repo,
                refname,
                order: row.approved_seq.unwrap_or(row.seq),
                proposer: row.proposer.clone(),
                depends_on: deps,
                launch_window: row.launch_window,
                state: State::Queued,
                entered: now,
                seq: 0,
                published: 0,
                reason: None,
                detail: None,
                problem: None,
                stalled: None,
                position: None,
                prepared: None,
                evidence_json: None,
                evidence: None,
                conflict_json: None,
                conflict: None,
                post_merge_failed: false,
                reruns: 0,
                not_started: 0,
                retry_after: 0,
                stalled_since: None,
                cancel_pending: false,
                landed_seq: 0,
                revert_of,
                bisect: None,
                culprit: None,
                covered_by: None,
                reverted_by: None,
                check_only: row.action == "vcs.try",
                cleanup: false,
                saved: String::new(),
            };
            if let Err(why) = info {
                self.reject(&mut c, "policy", &why, None, None, now).await?;
            } else {
                self.checkpoint(&mut c).await?;
            }
        }
        Ok(())
    }

    /// One `(space, repo, ref)`: what is in flight, then the next change.
    async fn group(
        &self,
        all: &mut [Change],
        idxs: &[usize],
        rows: &mut Vec<EffectRow>,
        now: i64,
        rep: &mut PassReport,
    ) -> Result<(), ExecError> {
        let Some(repo) = self.repo(&all[idxs[0]].repo) else {
            return Ok(()); // the configuration changed under a record: leave it be
        };
        // Landed changes watch their post-merge run, in parallel with everything else.
        for &i in idxs {
            if all[i].state == State::Landed && !all[i].post_merge_failed {
                let mut c = all[i].clone();
                self.watch_post_merge(&mut c, &repo, now, rep).await?;
                self.settle_stall(&mut c, now, rep).await?;
                all[i] = c;
            }
        }
        // A red post-merge run: find the culprit, revert it (A2).
        self.revert_flow(all, idxs, rows, &repo, now, rep).await?;
        // The change in the slot.
        for &i in idxs {
            if !matches!(all[i].state, State::Testing | State::Landing) {
                continue;
            }
            let mut c = all[i].clone();
            match self.row_of(rows, &c.space, &c.proposal).await? {
                Some(row) => {
                    if c.state == State::Landing {
                        self.resolve_landing(&mut c, &repo, now, rep).await?;
                    } else {
                        self.advance_testing(&mut c, &row, &repo, now, rep).await?;
                    }
                }
                None => c.problem = Some("the effect is not in the node's view".into()),
            }
            self.settle_stall(&mut c, now, rep).await?;
            all[i] = c;
        }
        // The queue, in the order of the approvals.
        let mut position = 0u32;
        let mut blocked = false;
        for &i in idxs {
            if all[i].state != State::Queued {
                continue;
            }
            let mut c = all[i].clone();
            let Some(row) = self.row_of(rows, &c.space, &c.proposal).await? else {
                continue;
            };
            match Self::deps(all, &c) {
                Deps::Rejected(dep) => {
                    self.reject(
                        &mut c,
                        "dependency",
                        &format!("dependency {dep} did not land"),
                        None,
                        None,
                        now,
                    )
                    .await?;
                    all[i] = c;
                    continue;
                }
                Deps::Waiting(why) => {
                    position += 1;
                    c.position = Some(position);
                    c.detail = Some(why);
                    c.problem = None;
                    self.settle_stall(&mut c, now, rep).await?;
                    all[i] = c;
                    continue;
                }
                Deps::Ready => {}
            }
            position += 1;
            let slot_taken = idxs.iter().any(|&j| {
                matches!(all[j].state, State::Testing | State::Landing)
                    && !self.yielding(&all[j], now)
            });
            if slot_taken || blocked {
                c.position = Some(position);
                c.detail = Some(if slot_taken {
                    "waiting for the lane's slot".into()
                } else {
                    "waiting for the change ahead".into()
                });
                c.problem = None;
                self.settle_stall(&mut c, now, rep).await?;
                all[i] = c;
                continue;
            }
            c.position = Some(position);
            if now < c.retry_after {
                // a stalled revert does not hold up the changes behind it
                blocked = c.revert_of.is_none();
                self.settle_stall(&mut c, now, rep).await?;
                all[i] = c;
                continue;
            }
            match self.prepare(&mut c, &row, &repo, now).await? {
                Prep::Ready => {
                    position -= 1;
                    self.advance_testing(&mut c, &row, &repo, now, rep).await?;
                }
                Prep::Done => position -= 1,
                Prep::Retry(why) => {
                    c.problem = Some(why);
                    c.retry_after = now + i64::try_from(repo.checks.poll_secs).unwrap_or(30);
                    blocked = true;
                }
                Prep::Stalled(why) => {
                    c.problem = Some(why);
                    c.retry_after = now + REVERT_RETRY_SECS;
                }
            }
            self.settle_stall(&mut c, now, rep).await?;
            all[i] = c;
        }
        Ok(())
    }

    /// The effect row of a change, from the pass's snapshot, or from a new one after a claim (which changes
    /// the row's claims and approvals).
    async fn row_of(
        &self,
        rows: &mut Vec<EffectRow>,
        space: &str,
        proposal: &str,
    ) -> Result<Option<EffectRow>, ExecError> {
        if std::mem::take(&mut lock(&self.mem).rows_stale) {
            *rows = self.exec.effects().await?;
        }
        Ok(rows
            .iter()
            .find(|r| r.space == space && r.proposal == proposal)
            .cloned())
    }

    fn deps(all: &[Change], c: &Change) -> Deps {
        let mut waiting = Vec::new();
        for dep in &c.depends_on {
            match all
                .iter()
                .find(|d| d.space == c.space && &d.proposal == dep)
            {
                None => waiting.push(format!("dependency {dep} is not known to the lane")),
                Some(d) => match d.state {
                    State::Rejected | State::Cancelled | State::Reverted => {
                        return Deps::Rejected(dep.clone());
                    }
                    State::Landed | State::Confirmed => {}
                    s => waiting.push(format!("waiting for dependency {dep} ({})", s.as_str())),
                },
            }
        }
        if waiting.is_empty() {
            Deps::Ready
        } else {
            Deps::Waiting(waiting.join("; "))
        }
    }

    /// Sets `stalled` from `problem` or the state's deadline, and writes the change when anything shows.
    async fn settle_stall(
        &self,
        c: &mut Change,
        now: i64,
        rep: &mut PassReport,
    ) -> Result<(), ExecError> {
        let before = c.stalled.clone();
        let late = self.deadline_of(c).filter(|d| now > *d).map(|_| {
            format!(
                "past the {} deadline of {} s",
                c.state.as_str(),
                self.deadline_of(c).map_or(0, |d| d - c.entered)
            )
        });
        c.stalled = c.problem.clone().or(late);
        if c.stalled != before
            && let Some(why) = &c.stalled
        {
            tracing::warn!(proposal = %c.proposal, why = %why, "lane: stalled");
            rep.stalled.push((c.proposal.clone(), why.clone()));
        }
        if c.stalled.is_none() {
            c.stalled_since = None;
        } else if c.stalled_since.is_none() {
            c.stalled_since = Some(now);
        }
        // A head that stays stalled is not kept forever: it has stepped aside (see `yielding`) and is rejected
        // after the hard bound, unless it unstalls first.
        if c.state == State::Testing
            && let (Some(since), Some(repo)) = (c.stalled_since, self.exec.config().repo(&c.repo))
            && now.saturating_sub(since)
                >= i64::try_from(repo.deadlines.stall_secs).unwrap_or(i64::MAX)
        {
            let detail = format!(
                "stalled for {} s: {}",
                now - since,
                c.stalled.as_deref().unwrap_or("")
            );
            return self.reject(c, "timeout", &detail, None, None, now).await;
        }
        self.checkpoint(c).await
    }

    // ───────────────────────── prepare ─────────────────────────

    async fn prepare(
        &self,
        c: &mut Change,
        row: &EffectRow,
        repo: &RepoTarget,
        now: i64,
    ) -> Result<Prep, ExecError> {
        let kind = if c.check_only { "vcs.try" } else { "vcs.land" };
        let Some(action) = self.exec.action_of(kind) else {
            return Ok(Prep::Retry(format!(
                "{kind} is not an action of this executor"
            )));
        };
        let (_, plan) = match check_plan(row, action.as_ref(), self.exec.config()) {
            Ok(p) => p,
            Err(PlanError::Preflight(m)) => return Ok(Prep::Retry(m)),
            Err(e) => {
                self.reject(c, "policy", &e.to_string(), None, None, now)
                    .await?;
                return Ok(Prep::Done);
            }
        };
        // A2: a revert is the exact inverse of a change this executor landed (X-13), or it is refused here
        if let Err(why) = self.exec.check_revert(row, &plan) {
            self.reject(c, "policy", &why, None, None, now).await?;
            return Ok(Prep::Done);
        }
        let key = IdempotencyKey {
            space: row.space.clone(),
            proposal: row.proposal.clone(),
            attempt: row.claims + 1,
        };
        let mut ctx = self.exec.plain_ctx(&key);
        ctx.args_blob = match self.exec.fetch_args_blob(&row.space, &plan).await {
            Ok(b) => b,
            Err(PlanError::Preflight(m)) => return Ok(Prep::Retry(m)),
            Err(e) => {
                self.reject(c, "policy", &e.to_string(), None, None, now)
                    .await?;
                return Ok(Prep::Done);
            }
        };
        // The bundle is the approved changeset (the same check a claim makes).
        match action.preflight(&plan, &ctx).await {
            Ok(()) => {}
            Err(PlanError::Preflight(m)) => return Ok(Prep::Retry(m)),
            Err(e) => {
                self.reject(c, "policy", &e.to_string(), None, None, now)
                    .await?;
                return Ok(Prep::Done);
            }
        }
        let auth = match write_auth(&plan, &ctx).await {
            Ok(a) => a,
            Err(e) => return Ok(Prep::Retry(format!("credential: {e}"))),
        };
        let vcs = ctx.vcs.clone();
        let (base, to) = (arg(&plan, "base"), arg(&plan, "to"));
        let refname = arg(&plan, "ref");
        let Some(bytes) = ctx.args_blob.clone() else {
            return Ok(Prep::Retry("the changeset's bundle is not at hand".into()));
        };
        // The bundle needs a commit the remote holds (a dependent proposal's `base` is its predecessor's `to`,
        // which the lane squashed and never pushed).
        let needs = crate::actions::bundle_base(&plan);
        let dir = match vcs.import_bundle(&repo.remote, auth.as_ref(), &bytes, &needs, to) {
            Ok(d) => d,
            Err(e) => return Ok(Prep::Retry(format!("importing the bundle: {e}"))),
        };
        let onto = match vcs.fetch_ref(&repo.remote, auth.as_ref(), refname) {
            Ok(Some((_, tip))) => tip,
            Ok(None) => {
                return Ok(Prep::Retry(format!(
                    "{refname} does not exist on the remote"
                )));
            }
            Err(e) => return Ok(Prep::Retry(format!("fetching {refname}: {e}"))),
        };
        let message = if c.check_only {
            format!(
                "{}\n\nHAN-Try: {}\n",
                crate::actions::try_title(&plan),
                row.proposal
            )
        } else {
            format!(
                "{}\n\nHAN-Proposal: {}\n",
                crate::actions::commit_title(&plan),
                row.proposal
            )
        };
        let time = row
            .approved_at
            .as_deref()
            .and_then(han_core::grammar::epoch_seconds)
            .unwrap_or(now);
        let rebased = match vcs.squash_rebase(
            &dir,
            base,
            to,
            &onto,
            &message,
            &author_of(&row.proposer),
            time,
        ) {
            Ok(r) => r,
            Err(e) => return Ok(Prep::Retry(format!("squash-rebase: {e}"))),
        };
        let commit = match rebased {
            han_vcs::RebaseOutcome::Rebased(r) if r.path_disjoint => r.commit,
            han_vcs::RebaseOutcome::Rebased(_) => {
                // It merges, but not as approved: gated again (R33 §2).
                let touched = vcs
                    .delta(&dir, base, to)
                    .and_then(|d| vcs.path_disjoint(&dir, &d, &onto))
                    .map(|p| p.touched)
                    .unwrap_or_default();
                let (clean, conflicts) = vcs
                    .merge_record(&dir, &onto, to)
                    .map(|r| {
                        (
                            r.clean,
                            serde_json::to_value(r.conflicts).unwrap_or_default(),
                        )
                    })
                    .unwrap_or_default();
                let record = json!({"changeset": c.proposal, "onto": onto, "clean": clean,
                    "conflicts": conflicts, "against": [], "reason": "not_path_disjoint",
                    "touched": touched});
                let detail = format!(
                    "not path-disjoint from {} ({}): gated again",
                    short(&onto),
                    touched.join(", ")
                );
                let text = canonical(&record).unwrap_or_else(|_| record.to_string());
                return self.conflicted(c, &detail, text, now).await;
            }
            han_vcs::RebaseOutcome::Conflict(r) => {
                let mut record = serde_json::to_value(&r).unwrap_or_default();
                if let Some(o) = record.as_object_mut() {
                    o.insert("changeset".into(), json!(c.proposal));
                    o.insert("against".into(), json!([]));
                    o.insert("reason".into(), json!("merge_conflict"));
                }
                let paths: Vec<&str> = r.conflicts.iter().map(|x| x.path.as_str()).collect();
                let detail = format!("conflicts with {}: {}", short(&onto), paths.join(", "));
                let text = canonical(&record).unwrap_or_else(|_| record.to_string());
                return self.conflicted(c, &detail, text, now).await;
            }
        };

        // The commit under test goes to the lane branch, and CI runs on that exact commit.
        let (branch, prefix) = if c.check_only {
            (try_branch(&row.proposal), TRY_BRANCH_PREFIX)
        } else {
            (
                lane_branch(repo, refname, &row.proposal),
                repo.lane_branch_prefix.as_str(),
            )
        };
        if let Err(e) = check_branch(&branch, &[prefix.to_owned()]) {
            self.reject(c, "policy", &e.to_string(), None, None, now)
                .await?;
            return Ok(Prep::Done);
        }
        let full = format!("refs/heads/{branch}");
        let mut expected = match vcs.remote_ref(&dir, &repo.remote, &full, auth.as_ref()) {
            Ok(r) => r,
            Err(e) => return Ok(Prep::Retry(format!("reading {full}: {e}"))),
        };
        let mut pushed = false;
        for _ in 0..2 {
            let spec = han_vcs::LandSpec {
                remote: repo.remote.clone(),
                refname: full.clone(),
                expected: expected.clone(),
                new: commit.clone(),
            };
            match vcs.land(&dir, &spec, auth.as_ref()) {
                Ok(han_vcs::LandOutcome::Landed | han_vcs::LandOutcome::AlreadyThere) => {
                    pushed = true;
                    break;
                }
                Ok(han_vcs::LandOutcome::Conflict { actual }) => expected = actual,
                Err(e) => return Ok(Prep::Retry(format!("pushing {full}: {e}"))),
            }
        }
        if !pushed {
            return Ok(Prep::Retry(format!("{full} keeps moving")));
        }
        let mut prepared = Prepared {
            onto,
            commit,
            branch,
            repository: dir.to_string_lossy().into_owned(),
            pr: None,
        };
        prepared.pr = self
            .open_pr(repo, &prepared, &plan, &row.proposal, c.check_only)
            .await;
        c.prepared = Some(prepared.clone());
        c.detail = None;
        c.reruns = 0;
        let req = self.request(repo, &prepared, Stage::Fast);
        if let Err(e) = self.checks.start(&req).await {
            tracing::warn!(proposal = %c.proposal, "lane: starting the checks: {e}");
        }
        self.enter(c, State::Testing, now).await?;
        Ok(Prep::Ready)
    }

    /// A change that does not apply to the tip. It is rejected (`conflict`), gated again, with the record. A
    /// revert (A2) is not: nothing is cancelled and nothing is forced, it stays queued, stalled, with the
    /// record, and the lane goes on with the changes behind it.
    async fn conflicted(
        &self,
        c: &mut Change,
        detail: &str,
        record: String,
        now: i64,
    ) -> Result<Prep, ExecError> {
        if c.revert_of.is_none() {
            self.reject(c, "conflict", detail, None, Some(record), now)
                .await?;
            return Ok(Prep::Done);
        }
        if c.conflict_json.as_deref() != Some(record.as_str()) {
            c.conflict_json = Some(record);
            c.conflict = None;
        }
        Ok(Prep::Stalled(format!(
            "the revert does not apply: {detail}; later changes touched its paths"
        )))
    }

    /// The pull request of a lane branch, opened or found; `None` when the repository has no GitHub side or
    /// GitHub did not answer (the lane tries again while the change is testing).
    async fn open_pr(
        &self,
        repo: &RepoTarget,
        p: &Prepared,
        plan: &Value,
        proposal: &str,
        check_only: bool,
    ) -> Option<String> {
        let gh = repo.github.as_deref()?;
        let minter = self.exec.github()?;
        let refname = arg(plan, "ref");
        let base = refname.strip_prefix("refs/heads/").unwrap_or(refname);
        // A try's pull request is a draft, only there so that CI runs; the lane closes it afterwards.
        let (title, body) = if check_only {
            (
                format!("try: {}", crate::actions::try_title(plan)),
                format!(
                    "HAN check-only try for proposal {proposal}. The lane runs the checks on this exact commit and lands nothing; it closes this pull request and deletes the branch afterwards."
                ),
            )
        } else {
            (
                crate::actions::commit_title(plan),
                format!(
                    "HAN lane branch for proposal {proposal}. The lane tests this exact commit and lands it on {base} by compare-and-swap."
                ),
            )
        };
        match crate::actions::ensure_pr(minter, gh, &p.branch, base, &title, &body, check_only)
            .await
        {
            Ok(d) => Some(d),
            Err(e) => {
                tracing::warn!(proposal, "lane: opening the pull request: {e}");
                None
            }
        }
    }

    fn request(&self, repo: &RepoTarget, p: &Prepared, stage: Stage) -> CheckRequest {
        CheckRequest {
            repo: repo.clone(),
            commit: p.commit.clone(),
            branch: Some(p.branch.clone()),
            stage,
            names: match stage {
                Stage::Fast => repo.checks.required.clone(),
                Stage::Full => repo.checks.full.clone(),
            },
        }
    }

    // ───────────────────────── testing ─────────────────────────

    fn due(&self, key: &str, now: i64, every: u64) -> bool {
        let mut mem = lock(&self.mem);
        let due = mem
            .last_poll
            .get(key)
            .is_none_or(|t| now - t >= i64::try_from(every).unwrap_or(i64::MAX));
        if due {
            mem.last_poll.insert(key.to_owned(), now);
        }
        due
    }

    async fn advance_testing(
        &self,
        c: &mut Change,
        row: &EffectRow,
        repo: &RepoTarget,
        now: i64,
        rep: &mut PassReport,
    ) -> Result<(), ExecError> {
        let Some(p) = c.prepared.clone() else {
            return self.requeue(c, "nothing was prepared", now).await;
        };
        if now < c.retry_after || !self.due(&c.proposal, now, repo.checks.poll_secs) {
            return Ok(());
        }
        c.problem = None;
        if p.pr.is_none()
            && repo.github.is_some()
            && let Ok(plan) = serde_json::from_str::<Value>(row.plan.as_deref().unwrap_or(""))
        {
            let pr = self
                .open_pr(repo, &p, &plan, &c.proposal, c.check_only)
                .await;
            if let Some(prepared) = c.prepared.as_mut() {
                prepared.pr = pr;
            }
        }
        let req = self.request(repo, &p, Stage::Fast);
        let runs = match self.checks.poll(&req).await {
            Ok(r) => r,
            Err(e) => {
                c.problem = Some(format!("the checks backend could not be read: {e}"));
                return Ok(());
            }
        };
        let verdicts: Vec<(&String, Verdict)> = repo
            .checks
            .required
            .iter()
            .map(|n| (n, verdict_of(&runs, n)))
            .collect();
        let names = |v: Verdict| -> Vec<String> {
            verdicts
                .iter()
                .filter(|(_, x)| *x == v)
                .map(|(n, _)| (*n).clone())
                .collect()
        };
        let evidence = ChecksEvidence::new(&p.commit, self.checks.name_for(repo), &runs);
        let evidence_text = evidence
            .canonical()
            .ok()
            .and_then(|b| String::from_utf8(b).ok());
        let red = names(Verdict::Red);
        if !red.is_empty() {
            if c.reruns < repo.checks.rerun && self.checks.rerun(&req, &red).await.unwrap_or(false)
            {
                c.reruns += 1;
                c.detail = Some(format!(
                    "running {} again ({}/{})",
                    red.join(", "),
                    c.reruns,
                    repo.checks.rerun
                ));
                return Ok(());
            }
            let detail = format!("required checks failed: {}", red.join(", "));
            return self
                .reject(c, "test", &detail, evidence_text, None, now)
                .await;
        }
        let missing = names(Verdict::Missing);
        let pending = names(Verdict::Pending);
        if !missing.is_empty() || !pending.is_empty() {
            if !missing.is_empty()
                && now - c.entered > i64::try_from(repo.checks.appear_secs).unwrap_or(i64::MAX)
            {
                c.problem = Some(format!(
                    "required check {} has not appeared after {} s: nothing is pending",
                    missing.join(", "),
                    repo.checks.appear_secs
                ));
            }
            return Ok(());
        }
        // All green. Has the ref moved? Then this commit is not the one that would land: prepare again. (A try
        // lands nothing: what it reports is that this commit passed.)
        if !c.check_only {
            let tip = self.tip(row, repo, &p.repository).await;
            match tip {
                Ok(t) if t.as_deref() != Some(p.onto.as_str()) => {
                    return self.requeue(c, "the ref moved while testing", now).await;
                }
                Ok(_) => {}
                Err(e) => {
                    c.problem = Some(format!("reading the ref: {e}"));
                    return Ok(());
                }
            }
        }
        if row.approvals.is_empty() && !row.proposer_suffices {
            // The approvals lapsed or were withdrawn: nothing can claim, and someone has to approve again.
            c.problem = Some("waiting for an approval: the checks are green".into());
            return Ok(());
        }
        if let Some(text) = evidence_text {
            c.evidence_json = Some(text);
            c.evidence = None;
        }
        // The checks that justify the landing go up first, so that the outcome can carry them (G-01).
        if c.evidence.is_none()
            && let Some(text) = c.evidence_json.clone()
        {
            match self.upload(&c.space, &text).await {
                Some(r) => c.evidence = Some(r),
                None => {
                    c.problem = Some("the checks evidence could not be uploaded".into());
                    return Ok(());
                }
            }
        }
        let proof = c.evidence.as_ref().and_then(evidence_ref);
        c.detail = None;
        self.enter(c, State::Landing, now).await?;
        lock(&self.mem).rows_stale = true;
        let claim = self
            .exec
            .claim_for_lane(
                row,
                LaneInstruction {
                    onto: p.onto.clone(),
                    commit: p.commit.clone(),
                    repository: PathBuf::from(&p.repository),
                    evidence: proof,
                },
            )
            .await?;
        self.after_claim(c, claim, repo, now, rep).await
    }

    /// Where the ref is on the remote now.
    async fn tip(
        &self,
        row: &EffectRow,
        repo: &RepoTarget,
        dir: &str,
    ) -> Result<Option<String>, String> {
        let plan: Value =
            serde_json::from_str(row.plan.as_deref().unwrap_or("")).map_err(|e| e.to_string())?;
        let key = IdempotencyKey {
            space: row.space.clone(),
            proposal: row.proposal.clone(),
            attempt: row.claims + 1,
        };
        let ctx = self.exec.plain_ctx(&key);
        let auth = write_auth(&plan, &ctx).await?;
        ctx.vcs
            .remote_ref(
                std::path::Path::new(dir),
                &repo.remote,
                arg(&plan, "ref"),
                auth.as_ref(),
            )
            .map_err(|e| e.to_string())
    }

    // ───────────────────────── landing ─────────────────────────

    async fn after_claim(
        &self,
        c: &mut Change,
        claim: LaneClaim,
        repo: &RepoTarget,
        now: i64,
        rep: &mut PassReport,
    ) -> Result<(), ExecError> {
        let poll = i64::try_from(repo.checks.poll_secs).unwrap_or(30);
        match claim {
            LaneClaim::Refused {
                permanent: true,
                why,
            } => self.reject(c, "policy", &why, None, None, now).await,
            LaneClaim::Refused { why, .. }
            | LaneClaim::ClaimRefused(why)
            | LaneClaim::NotLaunched(why) => {
                self.enter(c, State::Testing, now).await?;
                c.problem = Some(format!("the claim did not go through: {why}"));
                c.retry_after = now + poll;
                Ok(())
            }
            LaneClaim::NeedsApproval => {
                self.enter(c, State::Testing, now).await?;
                c.problem = Some("waiting for an approval".into());
                c.retry_after = now + poll;
                Ok(())
            }
            // The journal settles it (the next pass, or after a restart).
            LaneClaim::Busy | LaneClaim::Unacknowledged => Ok(()),
            LaneClaim::Ran { outcome, report } => {
                self.settle(c, &report, outcome.detail(), repo, now, rep)
                    .await
            }
        }
    }

    /// A claim's result, from the executor just now or from the journal after a restart.
    async fn settle(
        &self,
        c: &mut Change,
        report: &Report,
        detail: &str,
        repo: &RepoTarget,
        now: i64,
        rep: &mut PassReport,
    ) -> Result<(), ExecError> {
        if report.result == "succeeded" && c.check_only {
            c.not_started = 0;
            c.detail = Some(detail.to_owned());
            return self.enter(c, State::Tried, now).await;
        }
        if report.result == "succeeded" {
            c.not_started = 0;
            c.detail = Some(detail.to_owned());
            c.landed_seq = self.next_landed_seq(c)?;
            self.enter(c, State::Landed, now).await?;
            return self.watch_post_merge(c, repo, now, rep).await;
        }
        if *report == Report::not_started() {
            if detail.starts_with(REF_MOVED) {
                return self
                    .requeue(c, "the compare-and-swap lost: the ref moved", now)
                    .await;
            }
            c.not_started += 1;
            if c.not_started >= MAX_NOT_STARTED {
                return self.reject(c, "policy", detail, None, None, now).await;
            }
            return self.requeue(c, detail, now).await;
        }
        c.problem = Some(format!(
            "the claim ended {}/{}: the ref may or may not have moved; look at it and at the journal ({detail})",
            report.result, report.started
        ));
        Ok(())
    }

    /// A change in `landing` at the start of a pass: the journal says how its claim ended.
    async fn resolve_landing(
        &self,
        c: &mut Change,
        repo: &RepoTarget,
        now: i64,
        rep: &mut PassReport,
    ) -> Result<(), ExecError> {
        let Some(p) = c.prepared.clone() else {
            return self.requeue(c, "nothing was prepared", now).await;
        };
        let attempts = self
            .exec
            .with_ledger(|l| l.attempts_of(&c.space, &c.proposal))?;
        let ours = attempts
            .into_iter()
            .rev()
            .find(|a| a.lane.as_ref().is_some_and(|l| l.commit == p.commit));
        let Some(a) = ours else {
            // No claim reached the journal: nothing happened. Test again from the start.
            c.prepared = Some(p);
            return self.enter(c, State::Testing, now).await;
        };
        match (a.state.as_str(), a.report.as_ref()) {
            ("result" | "reported", Some(report)) => {
                self.settle(c, report, &a.detail, repo, now, rep).await
            }
            ("refused", _) => self.enter(c, State::Testing, now).await,
            _ => {
                c.problem = Some(format!(
                    "waiting for the executor to settle attempt {} ({})",
                    a.key.attempt, a.state
                ));
                Ok(())
            }
        }
    }

    // ───────────────────────── post-merge ─────────────────────────

    async fn watch_post_merge(
        &self,
        c: &mut Change,
        repo: &RepoTarget,
        now: i64,
        rep: &mut PassReport,
    ) -> Result<(), ExecError> {
        if c.post_merge_failed {
            return Ok(());
        }
        if repo.checks.full.is_empty() {
            // A repository with no full set treats landed as confirmed (R33 §4).
            c.detail = None;
            return self.enter(c, State::Confirmed, now).await;
        }
        let Some(p) = c.prepared.clone() else {
            return Ok(());
        };
        if !self.due(&format!("{}#full", c.proposal), now, repo.checks.poll_secs) {
            return Ok(());
        }
        c.problem = None;
        let req = self.request(repo, &p, Stage::Full);
        let runs = match self.checks.poll(&req).await {
            Ok(r) => r,
            Err(e) => {
                c.problem = Some(format!("the checks backend could not be read: {e}"));
                return Ok(());
            }
        };
        let evidence = ChecksEvidence::new(&p.commit, self.checks.name_for(repo), &runs)
            .canonical()
            .ok()
            .and_then(|b| String::from_utf8(b).ok());
        let verdicts: Vec<(&String, Verdict)> = repo
            .checks
            .full
            .iter()
            .map(|n| (n, verdict_of(&runs, n)))
            .collect();
        let names = |v: Verdict| -> Vec<String> {
            verdicts
                .iter()
                .filter(|(_, x)| *x == v)
                .map(|(n, _)| (*n).clone())
                .collect()
        };
        let red = names(Verdict::Red);
        if !red.is_empty() {
            let detail = format!(
                "the post-merge run failed on the landed commit {}: {}",
                short(&p.commit),
                red.join(", ")
            );
            tracing::error!(proposal = %c.proposal, "lane: {detail}");
            c.post_merge_failed = true;
            c.reason = Some("post_merge_failed".into());
            c.detail = Some(detail.clone());
            c.problem = None;
            if let Some(e) = evidence {
                c.evidence_json = Some(e);
                c.evidence = None;
            }
            rep.escalated.push((c.proposal.clone(), detail));
            return self.checkpoint(c).await;
        }
        let missing = names(Verdict::Missing);
        if !missing.is_empty() || !names(Verdict::Pending).is_empty() {
            if !missing.is_empty()
                && now - c.entered > i64::try_from(repo.checks.appear_secs).unwrap_or(i64::MAX)
            {
                c.problem = Some(format!(
                    "post-merge check {} has not appeared after {} s: nothing is pending",
                    missing.join(", "),
                    repo.checks.appear_secs
                ));
            }
            return Ok(());
        }
        if let Some(e) = evidence {
            c.evidence_json = Some(e);
            c.evidence = None;
        }
        c.detail = None;
        self.enter(c, State::Confirmed, now).await
    }

    // ───────────────────────── revert (A2) ─────────────────────────

    /// The next landing number of a lane: 1, 2, ... for the changes of one `(space, repo, ref)` in the order they
    /// landed. It is kept in the journal, so it does not repeat after a restart.
    fn next_landed_seq(&self, c: &Change) -> Result<u64, ExecError> {
        let key = format!("lane-landed|{}|{}|{}", c.space, c.repo, c.refname);
        let n = self
            .exec
            .with_ledger(|l| l.meta_get(&key))?
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
            + 1;
        self.exec
            .with_ledger(|l| l.meta_put(&key, &n.to_string()))?;
        Ok(n)
    }

    /// After a red post-merge run, for a repository that reverts (A2): finds the change that broke it (a
    /// bisect over the changes the run covered), tells its proposer, proposes the revert and, where Policy lets
    /// a machine, approves it. Then settles what a landed revert means for the changes around it.
    async fn revert_flow(
        &self,
        all: &mut [Change],
        idxs: &[usize],
        rows: &mut Vec<EffectRow>,
        repo: &RepoTarget,
        now: i64,
        rep: &mut PassReport,
    ) -> Result<(), ExecError> {
        if repo.checks.auto_revert {
            self.start_bisect(all, idxs).await?;
            for &t in idxs {
                if all[t].bisect.as_ref().is_some_and(|b| !b.done) {
                    self.probe(all, idxs, t, repo, now, rep).await?;
                }
            }
            for &k in idxs {
                if all[k].state == State::Landed && all[k].culprit.is_some() {
                    self.act_on_culprit(all, k, rows, repo, now, rep).await?;
                }
            }
        }
        self.reconcile(all, idxs, now).await
    }

    /// Takes the first change whose post-merge run is red and has not been looked into, and makes the list
    /// of the changes that run covered: the changes that landed since the last green one (the ones still
    /// `landed`), up to and including it, in the order they landed. One search runs at a time per lane.
    async fn start_bisect(&self, all: &mut [Change], idxs: &[usize]) -> Result<(), ExecError> {
        if idxs
            .iter()
            .any(|&i| all[i].bisect.as_ref().is_some_and(|b| !b.done))
        {
            return Ok(());
        }
        let mut reds: Vec<usize> = idxs
            .iter()
            .copied()
            .filter(|&i| {
                let c = &all[i];
                c.state == State::Landed
                    && c.post_merge_failed
                    && c.revert_of.is_none()
                    && c.bisect.is_none()
                    && c.culprit.is_none()
                    && c.covered_by.is_none()
                    && c.prepared.is_some()
            })
            .collect();
        reds.sort_by_key(|&i| (all[i].landed_seq, all[i].order));
        let Some(&t) = reds.first() else {
            return Ok(());
        };
        // Red because a culprit that landed before it is still in: its revert settles it.
        let culprit = idxs.iter().copied().find(|&j| {
            all[j].state == State::Landed
                && all[j].culprit.is_some()
                && all[j].landed_seq < all[t].landed_seq
        });
        if let Some(k) = culprit {
            let by = all[k].proposal.clone();
            let mut c = all[t].clone();
            c.detail = Some(
                match all[k].culprit.as_ref().and_then(|cu| cu.held.as_ref()) {
                    Some(why) => format!(
                        "the post-merge run is red; {} is the suspect and is not reverted: {why}",
                        short(&by)
                    ),
                    None => format!(
                        "the post-merge run is red because of {}, which is being reverted",
                        short(&by)
                    ),
                },
            );
            c.covered_by = Some(by);
            self.checkpoint(&mut c).await?;
            all[t] = c;
            return Ok(());
        }
        let key = |j: usize| (all[j].landed_seq, all[j].order);
        let mut cands: Vec<usize> = idxs
            .iter()
            .copied()
            .filter(|&j| {
                let c = &all[j];
                c.state == State::Landed
                    && c.revert_of.is_none()
                    && c.culprit.is_none()
                    && c.covered_by.is_none()
                    && c.prepared.is_some()
                    && key(j) <= key(t)
            })
            .collect();
        cands.sort_by_key(|&j| key(j));
        let candidates: Vec<Cand> = cands
            .iter()
            .map(|&j| Cand {
                proposal: all[j].proposal.clone(),
                commit: all[j]
                    .prepared
                    .as_ref()
                    .map(|p| p.commit.clone())
                    .unwrap_or_default(),
            })
            .collect();
        let hi = candidates
            .iter()
            .position(|c| c.proposal == all[t].proposal)
            .unwrap_or(0);
        let mut candidates = candidates;
        candidates.truncate(hi + 1);
        let commit = all[t]
            .prepared
            .as_ref()
            .map(|p| p.commit.clone())
            .unwrap_or_default();
        let red: BTreeMap<String, String> = all[t]
            .evidence_json
            .clone()
            .map(|e| (commit, e))
            .into_iter()
            .collect();
        let n = candidates.len();
        let mut c = all[t].clone();
        c.bisect = Some(Bisect {
            candidates,
            lo: 0,
            hi,
            red,
            ..Bisect::default()
        });
        c.detail = Some(format!(
            "bisecting the red post-merge run over {n} landed change(s)"
        ));
        self.checkpoint(&mut c).await?;
        all[t] = c;
        Ok(())
    }

    /// One step of the search of `all[t]`: runs `full` on the middle candidate (through the checks backend)
    /// and reads it. Red narrows to the candidates up to it, green to the ones after it; one left is the
    /// culprit.
    async fn probe(
        &self,
        all: &mut [Change],
        idxs: &[usize],
        t: usize,
        repo: &RepoTarget,
        now: i64,
        rep: &mut PassReport,
    ) -> Result<(), ExecError> {
        let mut c = all[t].clone();
        let Some(mut b) = c.bisect.clone() else {
            return Ok(());
        };
        if b.done {
            return Ok(());
        }
        if b.lo >= b.hi {
            return self.confirm(all, idxs, t, repo, now, rep).await;
        }
        let mid = b.lo + (b.hi - b.lo) / 2;
        let cand = b.candidates[mid].clone();
        let req = CheckRequest {
            repo: repo.clone(),
            commit: cand.commit.clone(),
            branch: c.refname.strip_prefix("refs/heads/").map(str::to_owned),
            stage: Stage::Full,
            names: repo.checks.full.clone(),
        };
        if b.started.as_deref() != Some(cand.commit.as_str()) {
            if let Err(e) = self.checks.start(&req).await {
                tracing::warn!(proposal = %c.proposal, "lane: starting the post-merge run of a candidate: {e}");
            }
            b.started = Some(cand.commit.clone());
            b.since = now;
        }
        let key = format!("{}#bisect#{}", c.proposal, cand.commit);
        if self.due(&key, now, repo.checks.poll_secs) {
            match self.checks.poll(&req).await {
                Err(e) => {
                    c.problem = Some(format!("the checks backend could not be read: {e}"));
                }
                Ok(runs) => {
                    c.problem = None;
                    let verdicts: Vec<Verdict> = repo
                        .checks
                        .full
                        .iter()
                        .map(|n| verdict_of(&runs, n))
                        .collect();
                    if verdicts.contains(&Verdict::Red) {
                        let ev =
                            ChecksEvidence::new(&cand.commit, self.checks.name_for(repo), &runs)
                                .canonical()
                                .ok()
                                .and_then(|bytes| String::from_utf8(bytes).ok())
                                .unwrap_or_default();
                        b.red.insert(cand.commit.clone(), ev);
                        b.hi = mid;
                        b.started = None;
                    } else if verdicts.iter().all(|v| *v == Verdict::Green) {
                        b.lo = mid + 1;
                        b.started = None;
                    } else if verdicts.contains(&Verdict::Missing)
                        && now - b.since
                            > i64::try_from(repo.checks.appear_secs).unwrap_or(i64::MAX)
                    {
                        c.problem = Some(format!(
                            "the post-merge run of {} has not appeared after {} s: nothing is pending",
                            short(&cand.commit),
                            repo.checks.appear_secs
                        ));
                    }
                }
            }
        }
        let n = b.hi - b.lo + 1;
        let finished = b.lo >= b.hi;
        c.bisect = Some(b);
        if finished {
            c.problem = None;
            all[t] = c;
            return self.confirm(all, idxs, t, repo, now, rep).await;
        }
        c.detail = Some(format!(
            "bisecting the red post-merge run: {n} candidate(s) left, running {}",
            short(&cand.commit)
        ));
        self.settle_stall(&mut c, now, rep).await?;
        all[t] = c;
        Ok(())
    }

    /// The search of `all[t]` is down to one candidate. Before it is reverted, `full` is run on it once more
    /// (the backend is asked to run it again, and the answer is read): a red that reproduces makes it the
    /// culprit; a green is a flake, so the change is marked `flaky` and escalated, not reverted; a backend that
    /// cannot run it again leaves the red unconfirmed, which is escalated too. The state is journalled before
    /// the backend is asked, so a restart reads the run instead of asking again.
    async fn confirm(
        &self,
        all: &mut [Change],
        idxs: &[usize],
        t: usize,
        repo: &RepoTarget,
        now: i64,
        rep: &mut PassReport,
    ) -> Result<(), ExecError> {
        let mut c = all[t].clone();
        let Some(mut b) = c.bisect.clone() else {
            return Ok(());
        };
        if b.done {
            return Ok(());
        }
        let cand = b.candidates[b.hi].clone();
        let req = CheckRequest {
            repo: repo.clone(),
            commit: cand.commit.clone(),
            branch: c.refname.strip_prefix("refs/heads/").map(str::to_owned),
            stage: Stage::Full,
            names: repo.checks.full.clone(),
        };
        let mut rr = b
            .rerun
            .clone()
            .filter(|r| r.commit == cand.commit)
            .unwrap_or_else(|| Rerun {
                commit: cand.commit.clone(),
                ..Rerun::default()
            });
        if !rr.asked {
            rr.asked = true;
            rr.since = now;
            b.rerun = Some(rr.clone());
            c.bisect = Some(b.clone());
            c.detail = Some(format!(
                "running the post-merge run of {} again to see whether the red reproduces",
                short(&cand.commit)
            ));
            self.checkpoint(&mut c).await?;
            let reds: Vec<String> = b
                .red
                .get(&cand.commit)
                .and_then(|e| serde_json::from_str::<ChecksEvidence>(e).ok())
                .into_iter()
                .flat_map(|e| e.checks)
                .filter(|r| r.verdict() == Verdict::Red)
                .map(|r| r.name)
                .collect();
            let names = if reds.is_empty() {
                repo.checks.full.clone()
            } else {
                reds
            };
            match self.checks.rerun(&req, &names).await {
                Ok(accepted) => rr.accepted = Some(accepted),
                Err(e) => {
                    // Not known to have been asked: ask again at the next pass.
                    rr.asked = false;
                    c.problem = Some(format!("the post-merge re-run could not be asked for: {e}"));
                }
            }
            b.rerun = Some(rr);
            c.bisect = Some(b);
            self.settle_stall(&mut c, now, rep).await?;
            all[t] = c;
            return Ok(());
        }
        let key = format!("{}#rerun#{}", c.proposal, cand.commit);
        if !self.due(&key, now, repo.checks.poll_secs) {
            return Ok(());
        }
        let runs = match self.checks.poll(&req).await {
            Ok(r) => r,
            Err(e) => {
                c.problem = Some(format!("the checks backend could not be read: {e}"));
                self.settle_stall(&mut c, now, rep).await?;
                all[t] = c;
                return Ok(());
            }
        };
        c.problem = None;
        let verdicts: Vec<Verdict> = repo
            .checks
            .full
            .iter()
            .map(|n| verdict_of(&runs, n))
            .collect();
        let evidence = ChecksEvidence::new(&cand.commit, self.checks.name_for(repo), &runs)
            .canonical()
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .unwrap_or_default();
        let red = verdicts.contains(&Verdict::Red);
        if !red && !verdicts.iter().all(|v| *v == Verdict::Green) {
            if verdicts.contains(&Verdict::Missing)
                && now - rr.since > i64::try_from(repo.checks.appear_secs).unwrap_or(i64::MAX)
            {
                c.problem = Some(format!(
                    "the post-merge re-run of {} has not appeared after {} s: nothing is pending",
                    short(&cand.commit),
                    repo.checks.appear_secs
                ));
            }
            c.detail = Some(format!(
                "waiting for the post-merge run of {} to finish again",
                short(&cand.commit)
            ));
            self.settle_stall(&mut c, now, rep).await?;
            all[t] = c;
            return Ok(());
        }
        if rr.accepted == Some(false) {
            // Whatever the backend shows is the old run: the red is not confirmed.
            let why = format!(
                "the checks backend cannot run the post-merge run of {} again, so its red was not confirmed",
                short(&cand.commit)
            );
            return self
                .park(all, idxs, t, c, b, &cand, Held::Unconfirmed, why, now, rep)
                .await;
        }
        if red {
            // It reproduced: the new run is the evidence.
            b.red.insert(cand.commit.clone(), evidence);
            c.bisect = Some(b);
            self.checkpoint(&mut c).await?;
            all[t] = c;
            return self.finish_bisect(all, idxs, t, now, rep).await;
        }
        let why = format!(
            "the post-merge run of {} was red, and green when run again: flaky, not reverted",
            short(&cand.commit)
        );
        self.park(all, idxs, t, c, b, &cand, Held::Flaky, why, now, rep)
            .await
    }

    /// The candidate is not reverted: the search ends, the candidate is `flaky` (or its red was unconfirmed),
    /// escalated with the reason, and the changes behind it wait with it. Nobody gets a fix-forward task for
    /// a change nothing proved broken; a person decides.
    #[allow(clippy::too_many_arguments)]
    async fn park(
        &self,
        all: &mut [Change],
        idxs: &[usize],
        t: usize,
        mut tc: Change,
        mut b: Bisect,
        cand: &Cand,
        held: Held,
        why: String,
        now: i64,
        rep: &mut PassReport,
    ) -> Result<(), ExecError> {
        b.done = true;
        let behind: Vec<String> = b.candidates[b.hi + 1..]
            .iter()
            .map(|c| c.proposal.clone())
            .collect();
        let red = b.red.get(&cand.commit).cloned();
        tc.bisect = Some(b);
        tc.problem = None;
        self.settle_stall(&mut tc, now, rep).await?;
        all[t] = tc;
        let Some(k) = idxs
            .iter()
            .copied()
            .find(|&j| all[j].proposal == cand.proposal)
        else {
            return Ok(());
        };
        let mut kc = all[k].clone();
        let told = kc.post_merge_failed;
        kc.culprit = Some(Culprit {
            at: now,
            held: Some(why.clone()),
            flaky: held == Held::Flaky,
            needs_human: true,
            ..Culprit::default()
        });
        kc.post_merge_failed = true;
        kc.reason = Some(
            match held {
                Held::Flaky => "flaky",
                Held::Unconfirmed => "post_merge_failed",
            }
            .into(),
        );
        kc.detail = Some(why.clone());
        kc.problem = None;
        if let Some(e) = red {
            kc.evidence_json = Some(e);
            kc.evidence = None;
        }
        tracing::error!(proposal = %kc.proposal, "lane: {why}");
        if !told || held == Held::Flaky {
            rep.escalated.push((kc.proposal.clone(), why.clone()));
        }
        self.settle_stall(&mut kc, now, rep).await?;
        let suspect = kc.proposal.clone();
        all[k] = kc;
        for p in behind {
            let Some(j) = idxs.iter().copied().find(|&j| all[j].proposal == p) else {
                continue;
            };
            let mut c = all[j].clone();
            c.detail = Some(format!(
                "the post-merge run is red; {} is the suspect and is not reverted: {why}",
                short(&suspect)
            ));
            c.covered_by = Some(suspect.clone());
            self.checkpoint(&mut c).await?;
            all[j] = c;
        }
        Ok(())
    }

    /// The search of `all[t]` is down to one candidate: it is the culprit. Its proposer is told (the lane acts
    /// on it next), the changes that landed after it, whose runs it made red, wait for its revert.
    async fn finish_bisect(
        &self,
        all: &mut [Change],
        idxs: &[usize],
        t: usize,
        now: i64,
        rep: &mut PassReport,
    ) -> Result<(), ExecError> {
        let mut tc = all[t].clone();
        let Some(mut b) = tc.bisect.clone() else {
            return Ok(());
        };
        b.done = true;
        let cand = b.candidates[b.hi].clone();
        let behind: Vec<String> = b.candidates[b.hi + 1..]
            .iter()
            .map(|c| c.proposal.clone())
            .collect();
        let evidence = b.red.get(&cand.commit).cloned();
        let n = b.candidates.len();
        tc.bisect = Some(b);
        self.settle_stall(&mut tc, now, rep).await?;
        all[t] = tc;
        let Some(k) = idxs
            .iter()
            .copied()
            .find(|&j| all[j].proposal == cand.proposal)
        else {
            return Ok(());
        };
        let mut kc = all[k].clone();
        let told = kc.post_merge_failed;
        kc.culprit = Some(Culprit {
            at: now,
            ..Culprit::default()
        });
        kc.post_merge_failed = true;
        kc.reason = Some("post_merge_failed".into());
        let detail = format!(
            "broke the post-merge run: the first red of {n} landed change(s), on {}",
            short(&cand.commit)
        );
        kc.detail = Some(detail.clone());
        kc.problem = None;
        if let Some(e) = evidence {
            kc.evidence_json = Some(e);
            kc.evidence = None;
        }
        tracing::error!(proposal = %kc.proposal, "lane: {detail}");
        if !told {
            rep.escalated.push((kc.proposal.clone(), detail));
        }
        self.settle_stall(&mut kc, now, rep).await?;
        let culprit = kc.proposal.clone();
        all[k] = kc;
        for p in behind {
            let Some(j) = idxs.iter().copied().find(|&j| all[j].proposal == p) else {
                continue;
            };
            let mut c = all[j].clone();
            c.detail = Some(format!(
                "the post-merge run is red because of {}, which is being reverted",
                short(&culprit)
            ));
            c.covered_by = Some(culprit.clone());
            self.checkpoint(&mut c).await?;
            all[j] = c;
        }
        Ok(())
    }

    /// What the lane does about a culprit, one step at a time, each kept before the next: the failing run goes
    /// up as a blob, the proposer gets the fix-forward task, the revert is proposed, and the lane approves it
    /// as far as Policy lets a machine (tier 0 and 1 with the failing run as evidence). The rest waits for a
    /// person, and the item says so.
    async fn act_on_culprit(
        &self,
        all: &mut [Change],
        k: usize,
        rows: &mut Vec<EffectRow>,
        repo: &RepoTarget,
        now: i64,
        rep: &mut PassReport,
    ) -> Result<(), ExecError> {
        let mut c = all[k].clone();
        let Some(mut cu) = c.culprit.clone() else {
            return Ok(());
        };
        if cu.held.is_some() || now < c.retry_after {
            return Ok(());
        }
        let retry = i64::try_from(repo.checks.poll_secs).unwrap_or(30).max(1);
        let stop = |c: &mut Change, why: String| {
            c.problem = Some(why);
            c.retry_after = now + retry;
        };
        c.problem = None;
        if c.evidence.is_none()
            && let Some(text) = c.evidence_json.clone()
        {
            match self.upload(&c.space, &text).await {
                Some(r) => c.evidence = Some(r),
                None => {
                    stop(
                        &mut c,
                        "the failing run's evidence could not be uploaded".into(),
                    );
                    self.settle_stall(&mut c, now, rep).await?;
                    all[k] = c;
                    return Ok(());
                }
            }
        }
        // The proposer's fix-forward task.
        if cu.fix_task.is_none() {
            match self.fix_task(&c).await {
                Ok(id) => cu.fix_task = Some(id),
                Err(why) => {
                    stop(
                        &mut c,
                        format!("the fix-forward task was not created: {why}"),
                    );
                    self.settle_stall(&mut c, now, rep).await?;
                    all[k] = c;
                    return Ok(());
                }
            }
            c.culprit = Some(cu.clone());
            self.checkpoint(&mut c).await?;
        }
        // The revert, proposed once.
        let revert = match cu.revert.clone() {
            Some(id) => id,
            None => match self.propose_revert(&c, &cu, rows).await {
                Ok(id) => {
                    cu.revert = Some(id.clone());
                    c.culprit = Some(cu.clone());
                    lock(&self.mem).rows_stale = true;
                    self.checkpoint(&mut c).await?;
                    id
                }
                Err(Proposal::Refused(why)) => {
                    // Policy refuses the lane's proposal and will again (a budgeted space asks for a task the
                    // lane was not given, for example): it is not retried, a person reverts the change.
                    let why = format!("the revert cannot be proposed: {why}");
                    tracing::warn!(proposal = %c.proposal, "lane: {why}");
                    cu.held = Some(why.clone());
                    cu.needs_human = true;
                    c.culprit = Some(cu);
                    c.detail = Some(format!(
                        "post-merge run red: {why}; a person has to revert {}",
                        short(&c.proposal)
                    ));
                    rep.escalated.push((c.proposal.clone(), why));
                    self.settle_stall(&mut c, now, rep).await?;
                    all[k] = c;
                    return Ok(());
                }
                Err(Proposal::Retry(why)) => {
                    stop(&mut c, format!("the revert was not proposed: {why}"));
                    self.settle_stall(&mut c, now, rep).await?;
                    all[k] = c;
                    return Ok(());
                }
            },
        };
        // Where the revert stands.
        let record = all
            .iter()
            .find(|r| r.space == c.space && r.proposal == revert)
            .map(|r| (r.state, r.reason.clone(), r.detail.clone()));
        if let Some((State::Rejected | State::Cancelled, reason, detail)) = record {
            let why = format!(
                "the revert {} ended {}: {}",
                short(&revert),
                reason.unwrap_or_default(),
                detail.unwrap_or_default()
            );
            c.retry_after = 0;
            c.problem = Some(why);
            self.settle_stall(&mut c, now, rep).await?;
            all[k] = c;
            return Ok(());
        }
        let row = self.row_of(rows, &c.space, &revert).await?;
        if let Some(row) = row
            && row.phase == "open"
            && row.approvals.is_empty()
            && !cu.needs_human
            && !cu.approved
        {
            match self.approve_revert(&c, &revert).await {
                Ok(()) => {
                    cu.approved = true;
                    lock(&self.mem).rows_stale = true;
                }
                Err(Approval::Human(why)) => {
                    tracing::warn!(proposal = %c.proposal, "lane: the revert {revert} waits for a person: {why}");
                    cu.needs_human = true;
                }
                Err(Approval::Retry(why)) => stop(&mut c, format!("approving the revert: {why}")),
            }
            c.culprit = Some(cu.clone());
        }
        c.detail = Some(if cu.needs_human {
            format!(
                "post-merge run red: revert {} proposed, waiting for a person to approve it",
                short(&revert)
            )
        } else {
            format!("post-merge run red: reverting through {}", short(&revert))
        });
        self.settle_stall(&mut c, now, rep).await?;
        all[k] = c;
        Ok(())
    }

    /// The fix-forward task for the culprit's proposer, with the failing run as evidence (its blob's hash and
    /// the failing checks, named in the ask).
    async fn fix_task(&self, c: &Change) -> Result<String, String> {
        // The first line of the ask names the culprit, so a task this lane created and lost the answer of (a crash
        // after the send) is found again and taken instead of made twice.
        let marker = format!("[han-lane fix-forward {}]", c.proposal);
        let actor = self.exec.actor();
        let existing = self
            .exec
            .tasks(&c.space)
            .await
            .map_err(|e| format!("reading the tasks: {e}"))?;
        if let Some(t) = existing.iter().find(|t| {
            t.author == actor
                && t.to == [c.proposer.clone()]
                && t.ask.as_deref().is_some_and(|a| a.starts_with(&marker))
        }) {
            return Ok(t.task.clone());
        }
        let evidence: Option<ChecksEvidence> = c
            .evidence_json
            .as_deref()
            .and_then(|t| serde_json::from_str(t).ok());
        let reds: Vec<String> = evidence
            .iter()
            .flat_map(|e| &e.checks)
            .filter(|r| r.verdict() == Verdict::Red)
            .map(|r| match &r.url {
                Some(u) => format!("{} ({}, {u})", r.name, r.conclusion),
                None => format!("{} ({})", r.name, r.conclusion),
            })
            .collect();
        let blob = c
            .evidence
            .as_ref()
            .map(|r| {
                format!(
                    "{} ({} bytes)",
                    r["hash"].as_str().unwrap_or_default(),
                    r["size"]
                )
            })
            .unwrap_or_default();
        let commit = c
            .prepared
            .as_ref()
            .map(|p| p.commit.clone())
            .unwrap_or_default();
        let ask = format!(
            "{marker}\nFix forward: your change {} landed on {} of {} as {commit}, and the post-merge run failed on it.\n\
             Failing: {}\nEvidence: the lane's record of the failing run, blob {blob}.\n\
             The lane reverts the change (a revert of a change of tier 2 or 3, or on a path no machine may approve, \
             waits for a person's approval). Land a new change that does what you meant and passes the post-merge run.",
            c.proposal,
            c.refname,
            c.repo,
            if reds.is_empty() {
                "(see the evidence)".to_owned()
            } else {
                reds.join(", ")
            }
        );
        let r = self
            .exec
            .node()
            .call(Command::TaskCreate {
                space: c.space.clone(),
                to: vec![c.proposer.clone()],
                ask,
                external: None,
            })
            .await
            .map_err(|e| e.to_string())?;
        match r {
            CallResult::Receipt { id, .. } => Ok(id),
            other => Err(format!("unexpected answer {other:?}")),
        }
    }

    /// Proposes the revert of `c` as a `vcs.land`: a squash commit on the commit `c` landed as, with the tree
    /// `c` landed on, so its delta is the exact inverse of `c`'s (X-13 checks that again before a claim). A
    /// proposal this executor already made for `c` (it restarted after the send) is taken instead.
    async fn propose_revert(
        &self,
        c: &Change,
        cu: &Culprit,
        rows: &[EffectRow],
    ) -> Result<String, Proposal> {
        let actor = self.exec.actor().to_owned();
        let plan_of = |r: &EffectRow| {
            r.plan
                .as_deref()
                .and_then(|t| serde_json::from_str::<Value>(t).ok())
        };
        for r in rows {
            if r.space == c.space
                && r.action == "vcs.land"
                && r.proposer == actor
                && plan_of(r).is_some_and(|p| {
                    p.pointer("/args/change/revertOf").and_then(Value::as_str)
                        == Some(c.proposal.as_str())
                })
            {
                return Ok(r.proposal.clone());
            }
        }
        let p = c.prepared.as_ref().ok_or("nothing was prepared")?;
        let key = IdempotencyKey {
            space: c.space.clone(),
            proposal: c.proposal.clone(),
            attempt: 0,
        };
        let vcs = self.exec.plain_ctx(&key).vcs;
        let dir = PathBuf::from(&p.repository);
        let title = rows
            .iter()
            .find(|r| r.space == c.space && r.proposal == c.proposal)
            .and_then(plan_of)
            .map_or_else(
                || format!("change {}", short(&c.proposal)),
                |plan| crate::actions::commit_title(&plan),
            );
        let subject: String = format!("Revert \"{title}\"")
            .chars()
            .filter(|ch| !ch.is_control())
            .take(200)
            .collect();
        let message = format!(
            "{subject}\n\nHAN-Reverts: {}\nHAN-Reason: the post-merge run failed on {}\n",
            c.proposal, p.commit
        );
        let revert = vcs
            .tree_commit(
                &dir,
                &p.commit,
                &p.onto,
                &message,
                &author_of(&actor),
                cu.at,
            )
            .map_err(|e| format!("building the revert commit: {e}"))?;
        let delta = vcs
            .delta(&dir, &p.commit, &revert)
            .map_err(|e| format!("the delta of the revert: {e}"))?;
        let lines = vcs
            .delta_lines(&dir, &p.commit, &revert, &delta)
            .map_err(|e| format!("the size of the revert: {e}"))?;
        let change = han_vcs::land_change_of(&c.repo, &delta, &lines, Some(&c.proposal))
            .map_err(|e| format!("the revert's change summary: {e}"))?;
        let digest = delta.digest().map_err(|e| e.to_string())?;
        let bundle = vcs
            .bundle(&dir, &p.commit, &revert)
            .map_err(|e| format!("bundling the revert: {e}"))?;
        let blob = self
            .upload_bytes(&c.space, han_vcs::BUNDLE_MEDIA_TYPE, &bundle)
            .await
            .ok_or("the revert's bundle could not be uploaded")?;
        let plan = json!({
            "action": "vcs.land",
            "target": c.repo,
            "args": {
                "ref": c.refname,
                "base": p.commit,
                "to": revert,
                "deltaDigest": digest,
                "tier": 1,
                "rebase": "path-disjoint",
                "title": subject,
                "change": serde_json::to_value(&change).map_err(|e| e.to_string())?,
            },
            "argsBlob": blob,
        });
        // P-06 (round 33): in a budgeted space a revert may name the task its culprit named, while that task
        // is open and budgeted, and is charged there at the culprit's cost.
        let culprit = rows
            .iter()
            .find(|r| r.space == c.space && r.proposal == c.proposal);
        let mut charge = None;
        if let Some((task, cost)) = culprit.and_then(|r| r.task.clone().zip(r.cost.clone())) {
            let tasks = self
                .exec
                .tasks(&c.space)
                .await
                .map_err(|e| Proposal::Retry(format!("reading the culprit's task: {e}")))?;
            if tasks
                .iter()
                .any(|t| t.task == task && t.open && t.budget.is_some())
            {
                charge = Some((task, cost));
            }
        }
        let named = charge.is_some();
        let (task, cost) = charge.unzip();
        let r = self
            .exec
            .node()
            .call(Command::EffectPropose {
                space: c.space.clone(),
                executor: actor,
                plan: plan.to_string(),
                task,
                cost,
                repeatable: false,
                expires_at: None,
            })
            .await
            .map_err(|e| match e {
                // Without the culprit's task a budgeted space refuses the revert, and will again.
                NodeError::Denied { message } | NodeError::Invalid { message }
                    if !named && message.contains("budgeted space") =>
                {
                    Proposal::Refused(format!(
                        "the culprit's task is closed or absent ({message})"
                    ))
                }
                NodeError::Denied { message } | NodeError::Invalid { message } => {
                    Proposal::Refused(message)
                }
                e => Proposal::Retry(e.to_string()),
            })?;
        match r {
            CallResult::Receipt { id, .. } => Ok(id),
            other => Err(Proposal::Retry(format!("unexpected answer {other:?}"))),
        }
    }

    /// The lane's own approval of the revert it proposed, with the failing run over the culprit's delta as
    /// evidence, through the node's narrow executor command (`Command::RevertApprove`: no person is asked; the
    /// node checks that the proposal is open, this actor's, a revert, and the evidence). Policy decides whether
    /// it counts: a machine's approval counts at tier 0 and 1 under a rule that names this executor
    /// (`revert: true`), and never above; a refusal means a person has to approve.
    async fn approve_revert(&self, c: &Change, revert: &str) -> Result<(), Approval> {
        let p = c
            .prepared
            .as_ref()
            .ok_or_else(|| Approval::Retry("nothing was prepared".into()))?;
        let record =
            c.evidence.as_ref().and_then(evidence_ref).ok_or_else(|| {
                Approval::Retry("the failing run's evidence is not uploaded".into())
            })?;
        let key = IdempotencyKey {
            space: c.space.clone(),
            proposal: c.proposal.clone(),
            attempt: 0,
        };
        let over = self
            .exec
            .plain_ctx(&key)
            .vcs
            .delta(std::path::Path::new(&p.repository), &p.onto, &p.commit)
            .and_then(|d| d.digest())
            .map_err(|e| Approval::Retry(format!("the culprit's delta: {e}")))?;
        let sent = self
            .exec
            .node()
            .call(Command::RevertApprove {
                space: c.space.clone(),
                proposal: revert.to_owned(),
                note: Some(format!(
                    "the post-merge run failed on {}: reverting {}",
                    short(&p.commit),
                    short(&c.proposal)
                )),
                evidence: ApprovalEvidence {
                    kind: REVERT_EVIDENCE_KIND.into(),
                    over,
                    record,
                },
            })
            .await;
        match sent {
            Ok(_) => Ok(()),
            Err(NodeError::Denied { message }) => Err(Approval::Human(message)),
            Err(e) => Err(Approval::Retry(e.to_string())),
        }
    }

    /// What a landed revert means: its culprit is `reverted`; and once the revert's own post-merge run is green,
    /// the changes that landed before it and wait on a red run are `confirmed` (the run contains them).
    async fn reconcile(
        &self,
        all: &mut [Change],
        idxs: &[usize],
        now: i64,
    ) -> Result<(), ExecError> {
        for &r in idxs {
            let Some(of) = all[r].revert_of.clone() else {
                continue;
            };
            if !matches!(all[r].state, State::Landed | State::Confirmed) {
                continue;
            }
            let by = all[r].proposal.clone();
            let space = all[r].space.clone();
            let culprit = idxs
                .iter()
                .copied()
                .find(|&k| all[k].space == space && all[k].proposal == of);
            if let Some(k) = culprit
                && all[k].state == State::Landed
            {
                let mut c = all[k].clone();
                c.reverted_by = Some(by.clone());
                c.detail = Some(format!("reverted by {}", short(&by)));
                c.problem = None;
                self.enter(&mut c, State::Reverted, now).await?;
                all[k] = c;
            }
            if all[r].state != State::Confirmed {
                continue;
            }
            for &j in idxs {
                if all[j].state == State::Landed
                    && all[j].revert_of.is_none()
                    && all[j].landed_seq > 0
                    && all[j].landed_seq < all[r].landed_seq
                {
                    let mut c = all[j].clone();
                    c.detail = Some(format!(
                        "confirmed by the green post-merge run of the revert {}",
                        short(&by)
                    ));
                    c.post_merge_failed = false;
                    c.reason = None;
                    self.enter(&mut c, State::Confirmed, now).await?;
                    all[j] = c;
                }
            }
        }
        Ok(())
    }

    // ───────────────────────── lane items ─────────────────────────

    /// One `lane` item per `(space, repo, ref)` whose depth or running set changed since the last one.
    async fn lane_items(&self, all: &[Change], now: i64) {
        let mut lanes: BTreeMap<(String, String, String), Vec<&Change>> = BTreeMap::new();
        for c in all.iter().filter(|c| !c.check_only) {
            lanes
                .entry((c.space.clone(), c.repo.clone(), c.refname.clone()))
                .or_default()
                .push(c);
        }
        for ((space, repo, refname), changes) in lanes {
            let live: Vec<&&Change> = changes.iter().filter(|c| !c.state.is_final()).collect();
            let running: Vec<&str> = live
                .iter()
                .filter(|c| matches!(c.state, State::Testing | State::Landing))
                .map(|c| c.proposal.as_str())
                .collect();
            let content = json!({
                "item": "lane", "repo": repo, "ref": refname, "instance": self.exec.actor(),
                "depth": live.len(), "batchSize": 1, "parallel": 1,
                "running": running.iter().enumerate()
                    .map(|(i, m)| json!({"n": i + 1, "members": [m]})).collect::<Vec<_>>(),
            });
            let key = format!("{space}|{repo}|{refname}");
            let text = content.to_string();
            if lock(&self.mem).lane_items.get(&key) == Some(&text) {
                continue;
            }
            let seq_key = format!("lane-seq|{key}");
            let seq = self
                .exec
                .with_ledger(|l| l.meta_get(&seq_key))
                .ok()
                .flatten()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0)
                + 1;
            let mut body = content;
            if let Some(o) = body.as_object_mut() {
                o.insert("seq".into(), json!(seq));
                o.insert("at".into(), json!(format_time(now)));
            }
            if self.stream(&space, body).await {
                let _ = self
                    .exec
                    .with_ledger(|l| l.meta_put(&seq_key, &seq.to_string()));
                lock(&self.mem).lane_items.insert(key, text);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_format_as_han_times() {
        assert_eq!(format_time(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_time(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(format_time(951_782_400), "2000-02-29T00:00:00Z");
        let t = format_time(1_792_000_000);
        assert_eq!(han_core::grammar::epoch_seconds(&t), Some(1_792_000_000));
    }

    #[test]
    fn branches_and_authors_are_derived_from_the_change() {
        let repo = RepoTarget::new("r", "x");
        assert_eq!(
            lane_branch(&repo, "refs/heads/release/1.0", "abc123DEF456xyz"),
            "han/lane/release-1.0/abc123DEF456"
        );
        assert_eq!(
            author_of("@agent:a.localhost"),
            "@agent:a.localhost <agent@a.localhost>"
        );
    }
}

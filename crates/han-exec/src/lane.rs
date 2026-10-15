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
//!   `post_merge_failed` with the evidence, escalated (an automatic revert is a later slice).
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
//!   commit?, onto?, branch?, pr?, conflict?, evidence?, postMergeFailed?, escalated?, seq, at}`; `seq`
//!   counts one change's items from 1; `conflict` and `evidence` are `BlobRef`s;
//! * `lane`: `{item, repo, ref, instance, depth, batchSize, parallel, running: [{n, members}], seq, at}`.
//!
//! Records of the lane live in the executor's journal file (`lane_changes`), so a restart carries on.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use han_node::api::{CallResult, Command, EffectRow, EvidenceRef, NodeError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::actions::{REF_MOVED, check_branch, depends_cycle, write_auth};
use crate::checks::{CheckRequest, Checks, ChecksEvidence, Stage, Verdict, verdict_of};
use crate::executor::{LaneClaim, check_plan};
use crate::ledger::Report;
use crate::{ExecError, Executor, IdempotencyKey, LaneInstruction, PlanError, RepoTarget, Wall};

/// The profile the lane's items carry.
pub const PROFILE: &str = "han.land/1";

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
    /// Out, with a reason.
    Rejected,
    /// The proposal was cancelled before the lane landed it.
    Cancelled,
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
            State::Rejected => "rejected",
            State::Cancelled => "cancelled",
        }
    }

    /// Whether nothing more happens to a change in this state.
    pub fn is_final(self) -> bool {
        matches!(self, State::Confirmed | State::Rejected | State::Cancelled)
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

/// What a plan says the lane needs: the repository, the ref and the proposals it depends on.
fn plan_info(row: &EffectRow) -> Result<(String, String, Vec<String>), String> {
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
    Ok((repo, refname.to_owned(), deps))
}

enum Prep {
    Ready,
    Done,
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
        self.send_cancel(c).await
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
        }
        self.put(c)
    }

    async fn upload(&self, space: &str, text: &str) -> Option<Value> {
        let r = self
            .exec
            .node()
            .call(Command::BlobPut {
                space: space.to_owned(),
                media_type: "application/json".into(),
                data: han_core::base64url::encode(text.as_bytes()),
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
        b
    }

    // ───────────────────────── transitions ─────────────────────────

    async fn enter(&self, c: &mut Change, state: State, now: i64) -> Result<(), ExecError> {
        c.state = state;
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

        let mut groups: BTreeMap<(String, String, String), Vec<usize>> = BTreeMap::new();
        for (i, c) in all.iter().enumerate() {
            if !c.state.is_final() {
                groups
                    .entry((c.space.clone(), c.repo.clone(), c.refname.clone()))
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
        }
        self.lane_items(&all, now).await;
        Ok(rep)
    }

    /// Makes a record for every approved `vcs.land` effect of a configured repository that has none.
    async fn discover(&self, rows: &[EffectRow], now: i64) -> Result<(), ExecError> {
        let known: Vec<(String, String)> = self
            .exec
            .with_ledger(|l| l.lane_all())?
            .into_iter()
            .map(|(s, p, _)| (s, p))
            .collect();
        for row in rows {
            if row.action != "vcs.land"
                || row.phase != "open"
                || row.approvals.is_empty()
                || known.contains(&(row.space.clone(), row.proposal.clone()))
            {
                continue;
            }
            let info = plan_info(row);
            let (repo, refname, deps) = match &info {
                Ok(i) => i.clone(),
                Err(_) => (String::new(), String::new(), Vec::new()),
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
                blocked = true;
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
                    State::Rejected | State::Cancelled => return Deps::Rejected(dep.clone()),
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
        let Some(action) = self.exec.action_of("vcs.land") else {
            return Ok(Prep::Retry(
                "vcs.land is not an action of this executor".into(),
            ));
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
        let dir = match vcs.import_bundle(&repo.remote, auth.as_ref(), &bytes, base, to) {
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
        let message = format!(
            "{}\n\nHAN-Proposal: {}\n",
            crate::actions::commit_title(&plan),
            row.proposal
        );
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
                self.reject(c, "conflict", &detail, None, Some(text), now)
                    .await?;
                return Ok(Prep::Done);
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
                self.reject(c, "conflict", &detail, None, Some(text), now)
                    .await?;
                return Ok(Prep::Done);
            }
        };

        // The commit under test goes to the lane branch, and CI runs on that exact commit.
        let branch = lane_branch(repo, refname, &row.proposal);
        if let Err(e) = check_branch(&branch, std::slice::from_ref(&repo.lane_branch_prefix)) {
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
        prepared.pr = self.open_pr(repo, &prepared, &plan, &row.proposal).await;
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

    /// The pull request of a lane branch, opened or found; `None` when the repository has no GitHub side or
    /// GitHub did not answer (the lane tries again while the change is testing).
    async fn open_pr(
        &self,
        repo: &RepoTarget,
        p: &Prepared,
        plan: &Value,
        proposal: &str,
    ) -> Option<String> {
        let gh = repo.github.as_deref()?;
        let minter = self.exec.github()?;
        let refname = arg(plan, "ref");
        let base = refname.strip_prefix("refs/heads/").unwrap_or(refname);
        let title = crate::actions::commit_title(plan);
        let body = format!(
            "HAN lane branch for proposal {proposal}. The lane tests this exact commit and lands it on {base} by compare-and-swap."
        );
        match crate::actions::ensure_pr(minter, gh, &p.branch, base, &title, &body).await {
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
            let pr = self.open_pr(repo, &p, &plan, &c.proposal).await;
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
        let evidence = ChecksEvidence::new(&p.commit, self.checks.name(), &runs);
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
        // All green. Has the ref moved? Then this commit is not the one that would land: prepare again.
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
        if row.approvals.is_empty() {
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
        if report.result == "succeeded" {
            c.not_started = 0;
            c.detail = Some(detail.to_owned());
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
        let evidence = ChecksEvidence::new(&p.commit, self.checks.name(), &runs)
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

    // ───────────────────────── lane items ─────────────────────────

    /// One `lane` item per `(space, repo, ref)` whose depth or running set changed since the last one.
    async fn lane_items(&self, all: &[Change], now: i64) {
        let mut lanes: BTreeMap<(String, String, String), Vec<&Change>> = BTreeMap::new();
        for c in all {
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

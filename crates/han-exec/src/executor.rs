//! The executor loop over the node API (X-01…X-11).
//!
//! For every `open` effect of this executor in the `Effects` view, at most once per head (X-09):
//! 1. the plan from the proposal's content must hash to `planDigest` and pass the action's closed schema and
//!    target checks (SR-70), and the action's preflight (for `git_push`: the approved changeset);
//! 2. the local [`Gate`] must let it run (until spec round 33: an approval is in hand);
//! 3. journal `claiming` with the elapsed clock's reading (X-02), then append `effect.claim` with
//!    `attempt = claims + 1` and the space's `launchWindow` (X-03);
//! 4. with the receipt in hand, check the plan again (X-09), journal `launching`, and as the last step check
//!    the deadline `W − stallBound` (X-04): past it, journal `expired` and report `failed`/`no`;
//! 5. run, journal the result, report `effect.outcome` (X-05), journal `reported`.
//!
//! After a restart nothing launches (X-06): [`Executor::recover`] reports what the journal holds.
//!
//! Actions that are [`Action::lane_only`] (`vcs.land`) are never claimed by this loop: the landing lane
//! ([`crate::Lane`]) claims them at push time through [`Executor::claim_for_lane`], which is steps 3 to 5 with
//! the lane's instruction journalled in step 3 and handed to the action in step 5.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use han_node::api::{
    CallResult, Command, EffectRow, NodeApi, NodeError, Query, Row, ViewEvent, ViewMirror, Window,
};
use serde_json::Value;

use crate::gate::{GateDecision, GateInput};
use crate::ledger::{Attempt, InstanceLock, Ledger, LedgerError, Report};
use crate::{
    Action, ActionCtx, Elapsed, ExecConfig, Gate, IdempotencyKey, LaneInstruction, Outcome,
    PlanError,
};

/// How the executor loop is set up.
pub struct ExecutorConfig {
    /// The executor actor (the node API's actor).
    pub actor: String,
    /// The actions it runs.
    pub actions: Vec<Arc<dyn Action>>,
    /// The local gate.
    pub gate: Arc<dyn Gate>,
    /// Targets and credentials.
    pub config: Arc<ExecConfig>,
    /// The version-control backend.
    pub vcs: Arc<dyn han_vcs::Vcs>,
    /// The GitHub token minter, when a GitHub App is configured.
    pub github: Option<Arc<crate::github::GithubMinter>>,
    /// The state directory (ledger, lock, scratch).
    pub state_dir: std::path::PathBuf,
    /// `stallBound` (X-04), 10 s by the spec.
    pub stall_bound: Duration,
}

/// Why the executor could not start.
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    /// The journal.
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    /// The node API.
    #[error("node: {0}")]
    Node(#[from] NodeError),
}

/// What one pass did, for tests and logs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StepReport {
    /// Proposals claimed in this pass.
    pub claimed: Vec<String>,
    /// Proposals launched in this pass.
    pub launched: Vec<String>,
    /// `(proposal, report)` outcomes appended in this pass.
    pub reported: Vec<(String, Report)>,
    /// `(proposal, why)` plans refused before claiming.
    pub refused: Vec<(String, String)>,
    /// Proposals waiting for an approval.
    pub waiting: Vec<String>,
}

/// How [`Executor::claim_for_lane`] ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaneClaim {
    /// Not claimed: the plan, its changeset or its preflight was refused. `permanent` says that no later
    /// attempt can pass (digest, schema, scope, changeset); otherwise the world could not be read, and a later
    /// attempt may.
    Refused {
        /// Whether trying again is pointless.
        permanent: bool,
        /// Why.
        why: String,
    },
    /// Not claimed: the gate still wants an approval.
    NeedsApproval,
    /// Not claimed: an attempt of this effect is still in flight, or the node's view lags the journal.
    Busy,
    /// The claim got no receipt (it is queued, or the reply was lost); the journal settles it.
    Unacknowledged,
    /// The home refused the claim.
    ClaimRefused(String),
    /// Claimed, but not launched (the plan check failed again, or the launch deadline passed): reported
    /// `failed`/`no`, so the effect opens again.
    NotLaunched(String),
    /// Launched. The outcome is journalled and, when the node answered, reported; `report` is what was
    /// reported.
    Ran {
        /// What the action returned.
        outcome: Outcome,
        /// The `(result, started)` journalled for `effect.outcome`.
        report: Report,
    },
}

/// What a claim ended with, inside the executor.
enum Flow {
    Unacknowledged,
    ClaimRefused(String),
    NotLaunched(String),
    Ran(Outcome, Report),
}

#[derive(Default)]
struct LoopState {
    /// Proposal → the head at which its last claim was refused or its plan was refused (X-09).
    tried_at_head: HashMap<String, u64>,
    /// Proposal → (not before this elapsed reading, the current delay in ms) (X-11).
    backoff: HashMap<String, (u64, u64)>,
    /// Proposals whose plan can never pass (digest, schema, scope): refused once, never looked at again.
    bad_plans: HashMap<String, String>,
}

/// The executor.
pub struct Executor {
    cfg: ExecutorConfig,
    api: Arc<dyn NodeApi>,
    elapsed: Arc<dyn Elapsed>,
    ledger: Mutex<Ledger>,
    state: Mutex<LoopState>,
    _lock: InstanceLock,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Executor {
    /// Takes the instance lock (X-07) and opens the journal.
    pub fn open(
        cfg: ExecutorConfig,
        api: Arc<dyn NodeApi>,
        elapsed: Arc<dyn Elapsed>,
    ) -> Result<Executor, ExecError> {
        let lock_ = InstanceLock::acquire(&cfg.state_dir)?;
        let ledger = Ledger::open(&cfg.state_dir.join("ledger.db"))?;
        tracing::info!(clock = elapsed.name(), actor = %cfg.actor, "han-exec started");
        Ok(Executor {
            cfg,
            api,
            elapsed,
            ledger: Mutex::new(ledger),
            state: Mutex::new(LoopState::default()),
            _lock: lock_,
        })
    }

    /// The journal (read access for tests and evidence).
    pub fn with_ledger<R>(&self, f: impl FnOnce(&Ledger) -> R) -> R {
        f(&lock(&self.ledger))
    }

    fn action(&self, kind: &str) -> Option<Arc<dyn Action>> {
        self.cfg.actions.iter().find(|a| a.kind() == kind).cloned()
    }

    /// The config the executor runs with.
    pub fn config(&self) -> &Arc<ExecConfig> {
        &self.cfg.config
    }

    /// The executor actor.
    pub fn actor(&self) -> &str {
        &self.cfg.actor
    }

    pub(crate) fn github(&self) -> Option<&Arc<crate::github::GithubMinter>> {
        self.cfg.github.as_ref()
    }

    pub(crate) fn node(&self) -> &Arc<dyn NodeApi> {
        &self.api
    }

    /// The context an action gets for `key`, without a lane instruction or the plan's blob.
    pub(crate) fn plain_ctx(&self, key: &IdempotencyKey) -> ActionCtx {
        self.ctx(key)
    }

    /// The action of a kind.
    pub(crate) fn action_of(&self, kind: &str) -> Option<Arc<dyn Action>> {
        self.action(kind)
    }

    /// The effects of this executor as the node shows them now (open, claimed and done).
    pub async fn effects(&self) -> Result<Vec<EffectRow>, NodeError> {
        self.snapshot().await
    }

    /// The context for an attempt read back from the journal: the lane's instruction is re-attached, so
    /// `lookup` can answer for it after a restart.
    fn ctx_for(&self, a: &Attempt) -> ActionCtx {
        let mut ctx = self.ctx(&a.key);
        ctx.lane = a.lane.clone();
        ctx
    }

    fn ctx(&self, key: &IdempotencyKey) -> ActionCtx {
        let scratch = self.cfg.state_dir.join("scratch").join(
            key.as_string()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect::<String>(),
        );
        ActionCtx {
            config: Arc::clone(&self.cfg.config),
            vcs: Arc::clone(&self.cfg.vcs),
            scratch,
            github: self.cfg.github.clone(),
            args_blob: None,
            lane: None,
        }
    }

    /// The bytes of the plan's `argsBlob` from the space (the node checks their hash and size). A blob the
    /// home doesn't serve yet (the proposer's upload is still on its way) is a preflight failure, tried again
    /// at the next head.
    pub(crate) async fn fetch_args_blob(
        &self,
        space: &str,
        plan: &Value,
    ) -> Result<Option<Arc<Vec<u8>>>, PlanError> {
        let Some(b) = crate::args_blob(plan)? else {
            return Ok(None);
        };
        let r = self
            .api
            .call(Command::BlobFetch {
                space: space.to_owned(),
                hash: b.hash.clone(),
                size: b.size,
                media_type: b.media_type.clone(),
            })
            .await
            .map_err(|e| PlanError::Preflight(format!("argsBlob: {e}")))?;
        let CallResult::Blob { hash, data, .. } = r else {
            return Err(PlanError::Preflight("argsBlob: unexpected answer".into()));
        };
        let bytes = han_core::base64url::decode(&data)
            .map_err(|_| PlanError::Preflight("argsBlob: not base64url".into()))?;
        if hash != b.hash || han_core::ids::sha256_hex(&bytes) != b.hash {
            return Err(PlanError::Preflight(
                "argsBlob: the bytes do not hash to the plan's".into(),
            ));
        }
        Ok(Some(Arc::new(bytes)))
    }

    fn query(&self) -> Query {
        Query::Effects {
            space: None,
            executor: Some(self.cfg.actor.clone()),
            include_done: true,
            window: Window {
                limit: 10_000,
                ..Window::default()
            },
        }
    }

    /// The effects as the node shows them now.
    async fn snapshot(&self) -> Result<Vec<EffectRow>, NodeError> {
        let mut stream = self.api.view(self.query());
        match stream.next().await {
            Some(ViewEvent::Snapshot { rows, .. } | ViewEvent::Resnapshot { rows, .. }) => {
                Ok(effect_rows(rows))
            }
            Some(ViewEvent::Failed { error, .. }) => Err(error),
            _ => Err(NodeError::unavailable("the effects view ended")),
        }
    }

    // ───────────────────────── restart (X-06) ─────────────────────────

    /// Settles what the journal holds after a restart, without launching anything (X-06): an attempt that
    /// journalled `claiming` but not `launching` is reported `failed`/`no` (or `refused` when its claim never
    /// landed); one that journalled `launching` with no result is looked up in the outside system and reported
    /// `succeeded`/`yes` only when the desired state is found, otherwise `unknown`/`unknown`; an `expired` one is
    /// reported `failed`/`no`; a result not yet reported is reported.
    pub async fn recover(&self) -> Result<StepReport, ExecError> {
        let rows = self.snapshot().await?;
        let pending = lock(&self.ledger).pending()?;
        let mut report = StepReport::default();
        for a in pending {
            let row = rows
                .iter()
                .find(|r| r.space == a.key.space && r.proposal == a.key.proposal);
            match a.state.as_str() {
                "claiming" => {
                    let claim = a.claim_id.clone().or_else(|| our_claim(row, &a));
                    let ledger = lock(&self.ledger);
                    match claim {
                        Some(c) => {
                            ledger.claimed(&a.key, &c)?;
                            ledger.result(
                                &a.key,
                                &Report::not_started(),
                                "restart before launching",
                            )?;
                        }
                        None => ledger.refused(&a.key, "restart: the claim never landed")?,
                    }
                }
                "launching" => {
                    let found = match (
                        self.action(&a.action),
                        serde_json::from_str::<Value>(&a.plan),
                    ) {
                        (Some(action), Ok(plan)) => {
                            action.lookup(&plan, &a.key, &self.ctx_for(&a)).await
                        }
                        _ => None,
                    };
                    let (r, d) = match found {
                        Some(Outcome::Succeeded { detail }) => (
                            Report::new("succeeded", "yes"),
                            format!("restart lookup: {detail}"),
                        ),
                        _ => (
                            Report::unknown(),
                            "restart: launched, result not found".to_owned(),
                        ),
                    };
                    lock(&self.ledger).result(&a.key, &r, &d)?;
                }
                "expired" => {
                    lock(&self.ledger).result(&a.key, &Report::not_started(), "expired")?;
                }
                _ => {}
            }
        }
        self.report_pending(&rows, &mut report).await?;
        Ok(report)
    }

    // ───────────────────────── one pass ─────────────────────────

    /// One pass over the effects the node shows now.
    pub async fn step(&self) -> Result<StepReport, ExecError> {
        let rows = self.snapshot().await?;
        self.step_rows(&rows).await
    }

    async fn step_rows(&self, rows: &[EffectRow]) -> Result<StepReport, ExecError> {
        let mut report = StepReport::default();
        self.settle_stuck(rows)?;
        self.report_pending(rows, &mut report).await?;
        for row in rows.iter().filter(|r| r.phase == "open") {
            self.consider(row, &mut report).await?;
        }
        Ok(report)
    }

    /// Attempts whose claim landed late (after a lost reply or a queued send) are never launched: once the
    /// node shows the claim, they are reported `failed`/`no` so the effect opens again (X-05).
    fn settle_stuck(&self, rows: &[EffectRow]) -> Result<(), ExecError> {
        // A claim given up on that landed after all: the effect waits on it, so report it not started.
        for r in rows.iter().filter(|r| r.phase == "claimed") {
            let attempts = lock(&self.ledger).attempts_of(&r.space, &r.proposal)?;
            if let Some(a) = attempts
                .iter()
                .find(|a| a.key.attempt == r.claims && a.state == "refused" && a.claim_id.is_none())
                && let Some(c) = r.last_claim.clone()
            {
                let ledger = lock(&self.ledger);
                ledger.claimed(&a.key, &c)?;
                ledger.result(
                    &a.key,
                    &Report::not_started(),
                    "a given-up claim landed late",
                )?;
            }
        }
        let pending = lock(&self.ledger).pending()?;
        for a in pending
            .iter()
            .filter(|a| a.state == "claiming" && a.claim_id.is_none())
        {
            let row = rows
                .iter()
                .find(|r| r.space == a.key.space && r.proposal == a.key.proposal);
            if let Some(c) = our_claim(row, a) {
                let ledger = lock(&self.ledger);
                ledger.claimed(&a.key, &c)?;
                ledger.result(&a.key, &Report::not_started(), "claim receipt arrived late")?;
            } else if let Some(r) = row
                && (r.claims >= a.key.attempt || r.phase == "done")
            {
                lock(&self.ledger).refused(&a.key, "another claim took this attempt")?;
            } else if self.elapsed.now_ms().saturating_sub(a.sent_ms) > a.window * 1000 {
                lock(&self.ledger).refused(&a.key, "the claim did not land within its window")?;
            }
        }
        Ok(())
    }

    /// Appends every journalled result that is not reported yet.
    async fn report_pending(
        &self,
        rows: &[EffectRow],
        out: &mut StepReport,
    ) -> Result<(), ExecError> {
        let pending = lock(&self.ledger).pending()?;
        for a in pending.into_iter().filter(|a| a.state == "result") {
            let (Some(claim), Some(rep)) = (a.claim_id.clone(), a.report.clone()) else {
                continue;
            };
            let row = rows
                .iter()
                .find(|r| r.space == a.key.space && r.proposal == a.key.proposal);
            // The home has settled this claim already (a lost reply that did land): nothing to send.
            if row.is_some_and(|r| {
                r.last_claim.as_deref() == Some(claim.as_str()) && r.phase != "claimed"
            }) || row.is_some_and(|r| r.claims > a.key.attempt)
            {
                lock(&self.ledger).reported(&a.key, "already settled at the home")?;
                continue;
            }
            let sent = self
                .api
                .call(Command::EffectOutcome {
                    space: a.key.space.clone(),
                    claim,
                    result: rep.result.clone(),
                    started: rep.started.clone(),
                    cost: None,
                    evidence: a.lane.as_ref().and_then(|l| l.evidence.clone()),
                })
                .await;
            match sent {
                Ok(_) => {
                    lock(&self.ledger).reported(&a.key, "outcome appended")?;
                    if rep.started == "no" {
                        self.back_off(&a.key.proposal);
                    }
                    out.reported.push((a.key.proposal.clone(), rep));
                }
                Err(NodeError::Unavailable { .. }) => {
                    tracing::warn!(proposal = %a.key.proposal, "outcome not sent yet; retrying");
                }
                Err(e) => {
                    lock(&self.ledger)
                        .reported(&a.key, &format!("home refused the outcome: {}", e.code()))?;
                }
            }
        }
        Ok(())
    }

    fn back_off(&self, proposal: &str) {
        let now = self.elapsed.now_ms();
        let mut st = lock(&self.state);
        let delay = st
            .backoff
            .get(proposal)
            .map(|(_, d)| (*d * 2).min(60_000))
            .unwrap_or(1_000);
        st.backoff.insert(proposal.to_owned(), (now + delay, delay));
    }

    fn refuse(&self, row: &EffectRow, e: &PlanError, out: &mut StepReport) {
        let why = e.to_string();
        let mut st = lock(&self.state);
        let permanent = matches!(
            e,
            PlanError::Digest
                | PlanError::Schema(_)
                | PlanError::OutOfScope(_)
                | PlanError::Unsupported(_)
        );
        if permanent {
            st.bad_plans.insert(row.proposal.clone(), why.clone());
        }
        if st.tried_at_head.get(&row.proposal) != Some(&row.head) {
            tracing::warn!(proposal = %row.proposal, why = %why, "plan refused before claiming");
            out.refused.push((row.proposal.clone(), why));
        }
        st.tried_at_head.insert(row.proposal.clone(), row.head);
    }

    async fn consider(&self, row: &EffectRow, out: &mut StepReport) -> Result<(), ExecError> {
        let Some(action) = self.action(&row.action) else {
            return Ok(()); // X-11: not ours to run; never claimed
        };
        if action.lane_only() {
            return Ok(()); // the landing lane claims it, at push time, after its checks are green
        }
        {
            let st = lock(&self.state);
            if st.bad_plans.contains_key(&row.proposal) {
                return Ok(());
            }
            if st
                .tried_at_head
                .get(&row.proposal)
                .is_some_and(|h| *h >= row.head)
            {
                return Ok(());
            }
            if st
                .backoff
                .get(&row.proposal)
                .is_some_and(|(until, _)| self.elapsed.now_ms() < *until)
            {
                return Ok(());
            }
        }
        let attempts = lock(&self.ledger).attempts_of(&row.space, &row.proposal)?;
        if attempts.iter().any(|a| !a.is_final()) {
            return Ok(()); // an attempt is in flight
        }
        let attempt = row.claims + 1;
        let reusable = |a: &Attempt| a.state == "refused" && a.claim_id.is_none();
        if attempts
            .iter()
            .any(|a| a.key.attempt > attempt || (a.key.attempt == attempt && !reusable(a)))
        {
            return Ok(()); // the view lags our own journal: wait for it
        }
        // 1. The plan.
        let (plan_text, plan) = match check_plan(row, action.as_ref(), &self.cfg.config) {
            Ok(p) => p,
            Err(e) => {
                self.refuse(row, &e, out);
                return Ok(());
            }
        };
        // 2. The gate.
        let decision = self.cfg.gate.decide(&GateInput {
            action: row.action.clone(),
            target: crate::plan_target(&plan).to_owned(),
            changes: plan
                .get("args")
                .and_then(|a| a.get("changes"))
                .and_then(|c| serde_json::from_value(c.clone()).ok()),
            approvals: row.approvals.clone(),
        });
        if let GateDecision::NeedsApproval { .. } = decision {
            out.waiting.push(row.proposal.clone());
            return Ok(());
        }
        let key = IdempotencyKey {
            space: row.space.clone(),
            proposal: row.proposal.clone(),
            attempt,
        };
        let mut ctx = self.ctx(&key);
        match self.fetch_args_blob(&row.space, &plan).await {
            Ok(b) => ctx.args_blob = b,
            Err(e) => {
                self.refuse(row, &e, out);
                return Ok(());
            }
        }
        if let Err(e) = action.preflight(&plan, &ctx).await {
            self.refuse(row, &e, out);
            return Ok(());
        }
        self.claim_and_run(row, action.as_ref(), &plan, &plan_text, &key, &ctx, out)
            .await?;
        Ok(())
    }

    /// Steps 3 to 5: claim (X-02, X-03), check again (X-09), launch within the deadline (X-04), report (X-05).
    /// `ctx.lane`, when set, is journalled with the claim.
    #[allow(clippy::too_many_arguments)]
    async fn claim_and_run(
        &self,
        row: &EffectRow,
        action: &dyn Action,
        plan: &Value,
        plan_text: &str,
        key: &IdempotencyKey,
        ctx: &ActionCtx,
        out: &mut StepReport,
    ) -> Result<Flow, ExecError> {
        // 3. Claim (X-02, X-03).
        let sent_ms = self.elapsed.now_ms();
        let window = row.launch_window;
        let attempt = key.attempt;
        match lock(&self.ledger).claiming_with(
            key,
            &row.action,
            plan_text,
            self.elapsed.name(),
            sent_ms,
            window,
            ctx.lane.as_ref(),
        ) {
            Ok(()) => {}
            Err(LedgerError::Exists(_)) => return Ok(Flow::Unacknowledged),
            Err(e) => return Err(e.into()),
        }
        let claim = self
            .api
            .call(Command::EffectClaim {
                space: row.space.clone(),
                proposal: row.proposal.clone(),
                attempt,
                window,
            })
            .await;
        let claim_id = match claim {
            Ok(CallResult::Receipt { id, .. }) => id,
            // Queued, or the reply was lost: no receipt in hand. The claim may still land; settle_stuck
            // reports it not started once the node shows it, or gives it up after its window.
            Ok(_) | Err(NodeError::Unavailable { .. }) => return Ok(Flow::Unacknowledged),
            Err(e) => {
                let why = format!("claim {}", e.code());
                lock(&self.ledger).refused(key, &why)?;
                lock(&self.state)
                    .tried_at_head
                    .insert(row.proposal.clone(), row.head);
                return Ok(Flow::ClaimRefused(why));
            }
        };
        lock(&self.ledger).claimed(key, &claim_id)?;
        out.claimed.push(row.proposal.clone());
        // 4. Check again, then launch (X-09, X-04).
        if let Err(e) = action.preflight(plan, ctx).await {
            let why = format!("plan check: {e}");
            lock(&self.ledger).result(key, &Report::not_started(), &why)?;
            self.report_pending(&[], out).await?;
            return Ok(Flow::NotLaunched(why));
        }
        lock(&self.ledger).launching(key)?;
        let deadline_ms = window
            .saturating_sub(self.cfg.stall_bound.as_secs())
            .saturating_mul(1000);
        if self.elapsed.now_ms().saturating_sub(sent_ms) >= deadline_ms {
            {
                let ledger = lock(&self.ledger);
                ledger.expired(key, "launch deadline passed (X-04)")?;
                ledger.result(key, &Report::not_started(), "expired")?;
            }
            self.report_pending(&[], out).await?;
            return Ok(Flow::NotLaunched("launch deadline passed (X-04)".into()));
        }
        out.launched.push(row.proposal.clone());
        let outcome = action.run(plan, key, ctx).await;
        // A landing lane's lost compare-and-swap moved nothing: the ref is where it was, so it is reported
        // not started, the effect opens again with its approvals (P-09), and the lane tries on the new tip.
        let report = match &outcome {
            Outcome::Failed {
                started: true,
                detail,
            } if ctx.lane.is_some() && detail.starts_with(crate::actions::REF_MOVED) => {
                Report::not_started()
            }
            other => other.report(),
        };
        lock(&self.ledger).result(key, &report, outcome.detail())?;
        // 5. Report.
        self.report_pending(&[], out).await?;
        Ok(Flow::Ran(outcome, report))
    }

    /// Claims one effect for the landing lane (R33 §4 "Land"), at push time: the plan is checked as for any
    /// claim, and `lane` is journalled with the claim's `claiming` entry and handed to the action as
    /// [`ActionCtx::lane`]. After a restart [`Executor::recover`] re-attaches it for `lookup`.
    pub async fn claim_for_lane(
        &self,
        row: &EffectRow,
        lane: LaneInstruction,
    ) -> Result<LaneClaim, ExecError> {
        let Some(action) = self.action(&row.action) else {
            return Ok(LaneClaim::Refused {
                permanent: true,
                why: format!("unsupported action {}", row.action),
            });
        };
        let attempts = lock(&self.ledger).attempts_of(&row.space, &row.proposal)?;
        if attempts.iter().any(|a| !a.is_final()) {
            return Ok(LaneClaim::Busy);
        }
        let attempt = row.claims + 1;
        let reusable = |a: &Attempt| a.state == "refused" && a.claim_id.is_none();
        if attempts
            .iter()
            .any(|a| a.key.attempt > attempt || (a.key.attempt == attempt && !reusable(a)))
        {
            return Ok(LaneClaim::Busy);
        }
        let refused = |e: PlanError| LaneClaim::Refused {
            permanent: !matches!(e, PlanError::Preflight(_)),
            why: e.to_string(),
        };
        let (plan_text, plan) = match check_plan(row, action.as_ref(), &self.cfg.config) {
            Ok(p) => p,
            Err(e) => return Ok(refused(e)),
        };
        if let Err(why) = self.check_revert(row, &plan) {
            return Ok(LaneClaim::Refused {
                permanent: true,
                why,
            });
        }
        let decision = self.cfg.gate.decide(&GateInput {
            action: row.action.clone(),
            target: crate::plan_target(&plan).to_owned(),
            changes: plan
                .get("args")
                .and_then(|a| a.get("changes"))
                .and_then(|c| serde_json::from_value(c.clone()).ok()),
            approvals: row.approvals.clone(),
        });
        if let GateDecision::NeedsApproval { .. } = decision {
            return Ok(LaneClaim::NeedsApproval);
        }
        let key = IdempotencyKey {
            space: row.space.clone(),
            proposal: row.proposal.clone(),
            attempt,
        };
        let mut ctx = self.ctx(&key);
        ctx.lane = Some(lane);
        match self.fetch_args_blob(&row.space, &plan).await {
            Ok(b) => ctx.args_blob = b,
            Err(e) => return Ok(refused(e)),
        }
        if let Err(e) = action.preflight(&plan, &ctx).await {
            return Ok(refused(e));
        }
        let mut out = StepReport::default();
        Ok(
            match self
                .claim_and_run(
                    row,
                    action.as_ref(),
                    &plan,
                    &plan_text,
                    &key,
                    &ctx,
                    &mut out,
                )
                .await?
            {
                Flow::Unacknowledged => LaneClaim::Unacknowledged,
                Flow::ClaimRefused(why) => LaneClaim::ClaimRefused(why),
                Flow::NotLaunched(why) => LaneClaim::NotLaunched(why),
                Flow::Ran(outcome, report) => LaneClaim::Ran { outcome, report },
            },
        )
    }

    /// X-13, A2: a plan whose `args.change.revertOf` names a land is a revert only if that land is one this
    /// executor made (its journal holds a succeeded lane attempt of the proposal), and the plan's delta is the
    /// exact inverse of what that land changed: the delta from the landed commit back to the tip it landed on
    /// has the plan's `deltaDigest`. `Err` is the reason; no claim is made.
    pub(crate) fn check_revert(&self, row: &EffectRow, plan: &Value) -> Result<(), String> {
        let Some(of) = plan
            .pointer("/args/change/revertOf")
            .and_then(Value::as_str)
        else {
            return Ok(());
        };
        let attempts = lock(&self.ledger)
            .attempts_of(&row.space, of)
            .map_err(|e| e.to_string())?;
        let landed = attempts
            .iter()
            .rev()
            .find_map(|a| {
                let succeeded = a.report.as_ref().is_some_and(|r| r.result == "succeeded");
                a.lane.as_ref().filter(|_| succeeded)
            })
            .ok_or_else(|| format!("revertOf {of} is no land this executor made"))?;
        let want = plan
            .pointer("/args/deltaDigest")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let inverse = self
            .cfg
            .vcs
            .delta(&landed.repository, &landed.commit, &landed.onto)
            .and_then(|d| d.digest())
            .map_err(|e| format!("the inverse of {of}: {e}"))?;
        if inverse != want {
            return Err(format!(
                "the plan's delta is not the inverse of the change {of} landed (delta {} is not {})",
                &want[..want.len().min(8)],
                &inverse[..inverse.len().min(8)]
            ));
        }
        Ok(())
    }

    // ───────────────────────── the loop ─────────────────────────

    /// Recovers (X-06), then passes over the effects on every change of the view and at least once a second,
    /// until `shutdown` turns true.
    pub async fn run(
        &self,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), ExecError> {
        self.recover().await?;
        let mut stream = self.api.view(self.query());
        let mut mirror = ViewMirror::new();
        loop {
            let rows = effect_rows(mirror.rows());
            if let Err(e) = self.step_rows(&rows).await {
                tracing::warn!("executor pass: {e}");
            }
            tokio::select! {
                ev = stream.next() => match ev {
                    Some(ev) => {
                        mirror.apply(ev);
                        while let Some(more) = stream.try_next() {
                            mirror.apply(more);
                        }
                    }
                    None => {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        stream = self.api.view(self.query());
                        mirror = ViewMirror::new();
                    }
                },
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                _ = shutdown.changed() => {}
            }
            if *shutdown.borrow() {
                return Ok(());
            }
        }
    }
}

fn effect_rows(rows: Vec<Row>) -> Vec<EffectRow> {
    rows.into_iter()
        .filter_map(|r| match r {
            Row::Effect(e) => Some(e),
            _ => None,
        })
        .collect()
}

/// The claim the node shows for this attempt, if it is the latest one.
fn our_claim(row: Option<&EffectRow>, a: &Attempt) -> Option<String> {
    row.filter(|r| r.claims == a.key.attempt)
        .and_then(|r| r.last_claim.clone())
}

/// The plan of a row: present, hashing to `planDigest` (X-09), and passing the action's checks (SR-70).
pub(crate) fn check_plan(
    row: &EffectRow,
    action: &dyn Action,
    config: &ExecConfig,
) -> Result<(String, Value), PlanError> {
    let text = row
        .plan
        .as_deref()
        .ok_or_else(|| PlanError::Schema("the proposal carries no plan".into()))?;
    let parsed = han_core::json::parse(text.as_bytes()).map_err(|_| PlanError::Digest)?;
    let digest = han_core::ids::json_digest(&parsed).map_err(|_| PlanError::Digest)?;
    if digest != row.plan_digest {
        return Err(PlanError::Digest);
    }
    let plan: Value = serde_json::from_str(text).map_err(|e| PlanError::Schema(e.to_string()))?;
    if plan.get("action").and_then(Value::as_str) != Some(row.action.as_str()) {
        return Err(PlanError::Schema("the plan names another action".into()));
    }
    // X-13: Policy read the proposal's `change`; it must be the summary the plan carries, which the action
    // checks against the bundle. Both absent is fine (a policy with gates then makes it tier 3).
    if row.action == "vcs.land" && row.change_digest != crate::actions::change_digest(&plan) {
        return Err(PlanError::Schema(
            "the proposal's change summary is not the plan's".into(),
        ));
    }
    action.plan_check(&plan, config)?;
    Ok((text.to_owned(), plan))
}

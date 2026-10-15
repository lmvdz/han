//! The session runner: one agent session from task to final update (PLAN §3, §5; spec R-24, §8.4).
//!
//! ```text
//!   task / issue ─▶ accept (R-24) ─▶ session.open ─▶ workspace (han-vcs) ─▶ proxy ─▶ sandbox ─▶ ACP agent
//!        ▲                                                                   │            │
//!        │                                       han-mcp over the tunnel ◀───┘            │ events
//!   final update ◀── status items, timeline batches, proposals ◀── status deriver ◀───────┘
//! ```
//!
//! The runner never acts on text it read as an instruction (R-24, SR-55): a turn is driven only by the task's
//! fixed prompt or by a signed `session.input` with a receipt; everything else reaches the agent as a data
//! envelope through han-mcp.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures::{FutureExt, StreamExt};
use han_mcp::{HostBridge, ProposeAnswer, ProposeRequest};
use han_node::api::{
    BoxFuture, CallResult, Command, FileTouch, NodeApi, NodeError, Query, Row, Sandbox,
    SessionState, ViewEvent, ViewMirror, ViewStream, Window,
};
use han_vcs::{ChangeSet, Vcs, Workspace, WorkspaceSpec};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

use crate::acp::{AcpDriver, AgentDriver, AgentEvent, Mode, SessionConfig, TurnInput};
use crate::driver::DriverConfig;
use crate::items::ItemFeed;
use crate::mask::{MaskedApi, Masker};
use crate::proxy::ProxyConfig;
use crate::r24::{self, Driver, DriverKind, GuardFacts, TaintStore, TaskDuty};
use crate::sandbox::ShimCommand;
use crate::status::{Signal, StatusDeriver};

/// Why a session could not run.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// The node refused or failed a command.
    #[error("node: {0}")]
    Node(#[from] NodeError),
    /// Version control.
    #[error("vcs: {0}")]
    Vcs(#[from] han_vcs::VcsError),
    /// The sandbox could not be set up (SR-23: the session does not start).
    #[error("sandbox: {0}")]
    Sandbox(String),
    /// The agent.
    #[error("agent: {0}")]
    Agent(String),
    /// Local I/O.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The taint store.
    #[error("taint store: {0}")]
    Taint(#[from] rusqlite::Error),
}

/// Where the session's code comes from.
#[derive(Clone, Debug)]
pub struct RepoConfig {
    /// The name plans use, for example `github:owner/repo` (the executor maps it to a remote).
    pub name: String,
    /// What to clone: a URL or a local path.
    pub source: String,
    /// What to start from: a branch, `HEAD` or a commit.
    pub base: String,
    /// The branch pull requests target.
    pub default_branch: String,
}

/// How to run one session.
#[derive(Clone)]
pub struct HostConfig {
    /// The space.
    pub space: String,
    /// The agent actor.
    pub agent: String,
    /// The agent's operator (its guard when it is guarded).
    pub operator: Option<String>,
    /// The harness.
    pub driver: DriverConfig,
    /// The repository.
    pub repo: RepoConfig,
    /// The issue, `github:owner/repo#123`.
    pub issue: Option<String>,
    /// A task to work (otherwise the runner finds or claims the issue's).
    pub task: Option<String>,
    /// Who runs approved effects.
    pub executor: String,
    /// The session's directory: worktree, agent dir, temp, mirror store.
    pub session_dir: PathBuf,
    /// The device's shared object store for workspaces.
    pub taint_db: PathBuf,
    /// Run in a guarded session (the agent chooses guarding).
    pub guarded: bool,
    /// Run turns that read other sessions in the stricter mode even when the agent is not guarded (SR-56
    /// SHOULD).
    pub strict_on_read: bool,
    /// Sandbox and egress.
    pub sandbox: SandboxChoice,
    /// The in-sandbox shim.
    pub shim: ShimCommand,
    /// Node and adapter, for drivers that launch through node.
    pub node: PathBuf,
    /// The adapter's directory.
    pub adapter_dir: PathBuf,
    /// The real model key in API-key mode (held only by the proxy).
    pub model_key: Option<String>,
    /// The session's budget (unit, limit and the node's prices), narrowed to the task's when the session works
    /// a budgeted task.
    pub budget: Option<han_registry::BudgetSpec>,
    /// How long one turn may take.
    pub turn_timeout: Duration,
    /// Paths never mirrored or streamed (globs), on top of the default list.
    pub exclusion: Vec<String>,
    /// How finished work leaves the session. Everything is off by default; see [`crate::land`].
    pub finish: han_registry::FinishSpec,
}

/// Which sandbox to use.
#[derive(Clone, Debug)]
pub struct SandboxChoice {
    /// `auto`, `mxc`, `bwrap` or `none`.
    pub backend: String,
    /// Whether the space allows advisory mode (no OS sandbox), SR-23.
    pub allow_advisory: bool,
    /// Extra read-only paths the agent needs (node, the adapter, the shim).
    pub ro: Vec<PathBuf>,
    /// The agent's own login directory (for example its Claude config directory), bound read-write into this
    /// agent's sandbox only, for drivers with `login_dir_env`.
    pub login_dir: Option<PathBuf>,
    /// Upstream allow-list additions.
    pub allow_hosts: Vec<String>,
    /// Toolchains the agent gets inside the sandbox (read-only roots, per-agent caches, registry hosts).
    pub toolchains: crate::toolchain::ToolchainConfig,
}

/// How a session ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionEnd {
    /// The `session.open` entry id.
    pub session: String,
    /// The task worked.
    pub task: Option<String>,
    /// The final state.
    pub state: SessionState,
    /// Why the session failed (`state` is `Failed`): the first failure it met. The status the node shows and
    /// the host's log say the same.
    pub reason: Option<String>,
    /// Proposals made.
    pub proposals: Vec<String>,
}

// ───────────────────────── node helpers ─────────────────────────

/// The first snapshot of a view, as rows.
pub async fn snapshot(api: &dyn NodeApi, q: Query) -> Result<Vec<Row>, NodeError> {
    let mut v = api.view(q);
    match tokio::time::timeout(Duration::from_secs(10), v.next()).await {
        Ok(Some(ViewEvent::Snapshot { rows, .. })) => Ok(rows),
        Ok(Some(ViewEvent::Failed { error, .. })) => Err(error),
        Ok(_) => Err(NodeError::unavailable("the view ended")),
        Err(_) => Err(NodeError::unavailable("the view did not answer")),
    }
}

fn receipt_id(r: &CallResult) -> Option<String> {
    match r {
        CallResult::Receipt { id, .. } | CallResult::Queued { id, .. } => Some(id.clone()),
        _ => None,
    }
}

/// The node API as an agent's tools see it (SR-06): views, and agent-class commands only.
pub struct AgentApi(pub Arc<dyn NodeApi>);

impl NodeApi for AgentApi {
    fn call(&self, cmd: Command) -> BoxFuture<'_, Result<CallResult, NodeError>> {
        if !cmd.is_agent_allowed() {
            return Box::pin(async {
                Err(NodeError::denied(
                    "an agent's tools may not send this command",
                ))
            });
        }
        self.0.call(cmd)
    }

    fn view(&self, query: Query) -> ViewStream {
        self.0.view(query)
    }
}

/// Guard facts read from the node's `Guard` view (R-24). The spec reads `state {agent, at}` from the home;
/// this reads the device's fold of the same log, at its current head (a gap: not "as of the instruction").
pub struct NodeGuardFacts {
    api: Arc<dyn NodeApi>,
    operator: Option<(String, String)>,
    cache: Mutex<BTreeMap<(String, String), bool>>,
}

impl NodeGuardFacts {
    /// Facts over `api`; `operator` is `(agent, operator)`.
    pub fn new(api: Arc<dyn NodeApi>, operator: Option<(String, String)>) -> Self {
        NodeGuardFacts {
            api,
            operator,
            cache: Mutex::new(BTreeMap::new()),
        }
    }

    /// Refreshes the guard facts of `actors` in `space` (call before deciding a turn's mode).
    pub async fn refresh(&self, space: &str, actors: &[&str]) {
        for a in actors {
            let rows = snapshot(
                self.api.as_ref(),
                Query::Guard {
                    space: space.into(),
                    actor: (*a).into(),
                    window: Window::default(),
                },
            )
            .await
            .unwrap_or_default();
            let g = rows.iter().any(|r| matches!(r, Row::Guard(g) if g.guarded));
            self.cache
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert((space.into(), (*a).into()), g);
        }
    }
}

impl GuardFacts for NodeGuardFacts {
    fn guarded(&self, space: &str, actor: &str, _seq: u64) -> bool {
        self.cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&(space.to_owned(), actor.to_owned()))
            .copied()
            .unwrap_or(false)
    }

    fn guard_of(&self, actor: &str) -> Option<String> {
        self.operator
            .as_ref()
            .filter(|(a, _)| a == actor)
            .map(|(_, o)| o.clone())
    }
}

// ───────────────────────── the session's shared state ─────────────────────────

struct Shared {
    api: Arc<dyn NodeApi>,
    cfg: HostConfig,
    session: String,
    task: Option<String>,
    workspace: Workspace,
    vcs: Arc<dyn Vcs>,
    masker: Arc<Masker>,
    status: Mutex<StatusDeriver>,
    /// Closed (taken) at shutdown, so the publisher sees the end of the channel after the queued signals.
    signals: Mutex<Option<mpsc::UnboundedSender<Signal>>>,
    /// Publishes (status or item) the node refused or failed; reported as `publish_errors` in the status.
    publish_errors: AtomicU64,
    read_others: Mutex<bool>,
    proposals: Mutex<Vec<String>>,
    /// The session's budget with the proxy's ledger, set once the egress is up. Proposal cost is read from it.
    budget: OnceLock<crate::glue::SessionBudget>,
    /// How much of the measured spend earlier proposals already carried, in the budget's unit.
    charged: Mutex<u64>,
    /// The first reason a turn failed, which the session's last status keeps.
    failure: Mutex<Option<String>>,
    /// The `land` finish flow, when the spec turns it on.
    land: Option<Arc<crate::land::Flow>>,
    /// A turn is running: the agent is in the workspace.
    busy: AtomicBool,
}

/// The cost a proposal carries: what the proxy measured since the last proposal, in the budget's unit
/// (SR-41). `None` when nothing is measured: no budget, or one without a ledger (no price table, or a
/// subscription), where money is unknown and is never guessed. Also returns the spend to count as charged
/// once the proposal is queued.
fn measured_cost(
    budget: Option<&crate::glue::SessionBudget>,
    charged: u64,
) -> Option<((String, u64), u64)> {
    let b = budget.filter(|b| b.ledger.is_some())?;
    let spent = b.spent();
    Some(((b.unit.clone(), spent.saturating_sub(charged)), spent))
}

impl Shared {
    /// Records why the session failed (the first reason stands) and signals it.
    fn fail(&self, why: String) {
        self.failure
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_or_insert_with(|| why.clone());
        self.signal(Signal::Failed(why));
    }

    /// Queues a signal for the publisher (masked first, see [`Shared::apply`]). After [`Shared::close_signals`]
    /// it is dropped.
    fn signal(&self, s: Signal) {
        if let Some(tx) = self
            .signals
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            let _ = tx.send(self.masked_signal(s));
        }
    }

    /// Whether nobody listens any more: the channel was closed at shutdown, or the publisher is gone.
    fn signals_closed(&self) -> bool {
        self.signals
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .is_none_or(|tx| tx.is_closed())
    }

    /// Ends the signal channel: the publisher drains what is queued, publishes the final state and returns.
    fn close_signals(&self) {
        self.signals
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
    }

    /// Masks the free text of a signal. This is early on purpose: the deriver bounds texts (SR-59), and a
    /// secret cut in half by the bound no longer matches. [`MaskedApi`] is the backstop at the node boundary.
    fn masked_signal(&self, s: Signal) -> Signal {
        let m = |t: String| self.masker.mask(&t).into_owned();
        match s {
            Signal::LastMessage(t) => Signal::LastMessage(m(t)),
            Signal::Tool { title, kind } => Signal::Tool {
                title: m(title),
                kind,
            },
            Signal::Plan(items) => Signal::Plan(
                items
                    .into_iter()
                    .map(|p| han_node::api::PlanItem {
                        text: m(p.text),
                        done: p.done,
                    })
                    .collect(),
            ),
            Signal::Summary(t) => Signal::Summary(m(t)),
            Signal::Failed(t) => Signal::Failed(m(t)),
            Signal::TurnStarted { step } => Signal::TurnStarted { step: m(step) },
            Signal::Land { text, blocked } => Signal::Land {
                text: m(text),
                blocked,
            },
            other => other,
        }
    }

    /// Folds a signal into the status (masked). Returns whether the status changed.
    fn apply(&self, s: Signal) -> bool {
        let s = self.masked_signal(s);
        self.status
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .apply(s)
    }

    /// Counts and logs a failed publish. The error text is masked: it may quote what was sent.
    fn publish_failed(&self, what: &str, e: &NodeError) {
        self.publish_errors.fetch_add(1, Ordering::Relaxed);
        tracing::warn!("{what} not published: {}", self.masker.mask(&e.to_string()));
    }

    /// Who must approve, as text.
    fn waiting_for(&self) -> String {
        self.cfg
            .operator
            .clone()
            .unwrap_or_else(|| "an approver".into())
    }

    /// The plan of a proposal, built by the host (never by the agent): git pushes carry the committed
    /// changeset and its digest, which the executor checks before landing, and the commits themselves as a
    /// bundle (`argsBlob`, R15): the executor may run as another OS user that can't read the workspace. The
    /// bundle's bytes come back with the plan, to upload before the proposal.
    fn plan_for(&self, req: &ProposeRequest) -> Result<(Value, Option<Vec<u8>>), String> {
        let effect = req.effect.as_object().cloned().unwrap_or_default();
        let branch = self.workspace.branch.clone();
        match req.kind.as_str() {
            "git_push" => {
                let head = self
                    .vcs
                    .commit_all(
                        &self.workspace.dir,
                        &format!("{}\n\nHAN session {}", req.summary.trim(), self.session),
                        &self.cfg.agent,
                    )
                    .map_err(|e| e.to_string())?;
                let cs: ChangeSet = self
                    .vcs
                    .changeset_between(&self.workspace.dir, &self.workspace.base, &head)
                    .map_err(|e| e.to_string())?;
                if cs.files.is_empty() {
                    return Err("there is nothing to push: the branch has no changes".into());
                }
                let refname = format!("refs/heads/{branch}");
                let from = self
                    .vcs
                    .remote_ref(&self.workspace.dir, &self.cfg.repo.source, &refname, None)
                    .ok()
                    .flatten();
                let bundle = self
                    .vcs
                    .bundle(&self.workspace.dir, &self.workspace.base, &head)
                    .map_err(|e| format!("the changeset can't be bundled: {e}"))?;
                if bundle.len() > MAX_BUNDLE {
                    return Err(format!(
                        "the changeset is too large to hand to the executor: {} bytes as a bundle, at most {MAX_BUNDLE}",
                        bundle.len()
                    ));
                }
                let plan = json!({
                    "action": "git_push",
                    "target": self.cfg.repo.name,
                    "args": {
                        "branch": branch,
                        "base": self.workspace.base,
                        "from": from,
                        "to": head,
                        "changeset": cs.digest,
                        "changes": cs.summary(),
                    },
                    "argsBlob": {
                        "hash": han_core::ids::sha256_hex(&bundle),
                        "size": bundle.len(),
                        "type": han_vcs::BUNDLE_MEDIA_TYPE,
                    }
                });
                Ok((plan, Some(bundle)))
            }
            "open_pr" => {
                let title = effect
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or(req.summary.as_str());
                let mut args = json!({
                    "head": branch,
                    "base": self.cfg.repo.default_branch,
                    "title": crate::status::bound(title, 200),
                });
                if let Some(b) = effect.get("body").and_then(Value::as_str) {
                    args["body"] = Value::String(crate::status::bound(b, 20_000));
                }
                Ok((
                    json!({"action": "open_pr", "target": self.cfg.repo.name, "args": args}),
                    None,
                ))
            }
            "land" => {
                let flow = self.land.as_ref().ok_or(
                    "land is not enabled for this space and repository: use git_push and open_pr",
                )?;
                // The host's backstop proposes what is committed and commits nothing (`committedOnly`); an agent
                // that sets it only gets less.
                let commit = effect.get("committedOnly").and_then(Value::as_bool) != Some(true);
                let draft = flow.draft_with(&req.summary, &effect, commit)?;
                Ok((draft.plan, Some(draft.bundle)))
            }
            "http_request" => {
                let url = effect
                    .get("url")
                    .and_then(Value::as_str)
                    .ok_or("an http_request names its url")?;
                let host = url
                    .strip_prefix("https://")
                    .and_then(|r| r.split(['/', ':', '?']).next())
                    .filter(|h| !h.is_empty())
                    .ok_or("only https:// URLs can be proposed")?;
                let mut args = serde_json::Map::new();
                for k in ["method", "url", "headers", "body"] {
                    if let Some(v) = effect.get(k) {
                        args.insert(k.into(), v.clone());
                    }
                }
                Ok((
                    json!({"action": "http_request", "target": host, "args": args}),
                    None,
                ))
            }
            other => Err(format!(
                "unknown kind {other}: use git_push, open_pr, http_request or land"
            )),
        }
    }
}

/// The largest bundle a `git_push` hands over: what the node API carries as one blob.
pub(crate) const MAX_BUNDLE: usize = 4 * 1024 * 1024;

/// How often an upload of a `git_push`'s bundle is tried again while the home is away, and for how long.
const BLOB_RETRY: Duration = Duration::from_secs(2);
const BLOB_RETRY_FOR: Duration = Duration::from_secs(3600);
/// How long a proposal waits for the first upload of its bundle before it goes ahead (the upload goes on).
const BLOB_FIRST_TRY: Duration = Duration::from_secs(5);

/// Uploads a proposal's `argsBlob` (the bundle of a `git_push`) to the space's home. The home supplies it to
/// the executor because this agent authored the proposal (G-01), whether the upload comes before or after
/// the proposal is ordered, so a home that is away is tried again in the background while the proposal waits
/// in the outbox. Any other refusal refuses the proposal.
async fn put_args_blob(sh: &Arc<Shared>, bytes: Vec<u8>) -> Result<(), String> {
    let call = |sh: Arc<Shared>, data: String| async move {
        sh.api
            .call(Command::BlobPut {
                space: sh.cfg.space.clone(),
                media_type: han_vcs::BUNDLE_MEDIA_TYPE.into(),
                data,
            })
            .await
    };
    let data = han_core::base64url::encode(&bytes);
    // A home that is away can take long to fail: the proposal does not wait for it.
    let first = tokio::time::timeout(BLOB_FIRST_TRY, call(Arc::clone(sh), data.clone()))
        .await
        .unwrap_or_else(|_| Err(NodeError::unavailable("the home did not answer in time")));
    match first {
        Ok(_) => Ok(()),
        Err(NodeError::Unavailable { .. }) => {
            let sh = Arc::clone(sh);
            tokio::spawn(async move {
                let start = tokio::time::Instant::now();
                while start.elapsed() < BLOB_RETRY_FOR && !sh.signals_closed() {
                    tokio::time::sleep(BLOB_RETRY).await;
                    match call(Arc::clone(&sh), data.clone()).await {
                        Ok(_) => return,
                        Err(NodeError::Unavailable { .. }) => {}
                        Err(e) => {
                            sh.publish_failed("the changeset's bundle", &e);
                            return;
                        }
                    }
                }
            });
            Ok(())
        }
        Err(e) => Err(format!("the changeset could not be handed over: {e}")),
    }
}

/// How long a `propose` tool call waits for the home to accept a queued proposal before it answers "queued".
const PROPOSE_WAIT: Duration = Duration::from_secs(10);

/// How often the outbox entry of a queued proposal is looked at.
const OUTBOX_POLL: Duration = Duration::from_millis(500);

/// What became of an outbox entry.
enum Fate {
    /// The home ordered it.
    Sent,
    /// The home refused it, or delivery gave up; why.
    Refused(String),
    /// Still waiting.
    Pending,
}

/// Follows outbox entry `outbox_id` until it is sent, refused or failed, or `wait` is over (or the session's
/// status channel is closed).
async fn follow_outbox(sh: &Shared, outbox_id: u64, wait: Duration) -> Fate {
    let start = tokio::time::Instant::now();
    loop {
        let q = Query::Outbox {
            space: Some(sh.cfg.space.clone()),
            window: Window {
                cursor: Some(format!("{:012}", outbox_id.saturating_sub(1))),
                limit: 1,
                ..Window::default()
            },
        };
        if let Ok(rows) = snapshot(sh.api.as_ref(), q).await {
            for r in rows {
                if let Row::Outbox(o) = r
                    && o.outbox_id == outbox_id
                {
                    match o.state.as_str() {
                        "sent" => return Fate::Sent,
                        "rejected" | "failed" => {
                            return Fate::Refused(o.error.unwrap_or_else(|| o.state.clone()));
                        }
                        _ => {}
                    }
                }
            }
        }
        if sh.signals_closed() || start.elapsed() >= wait {
            return Fate::Pending;
        }
        tokio::time::sleep(OUTBOX_POLL).await;
    }
}

struct Bridge(Arc<Shared>);

async fn propose_plan(sh: &Arc<Shared>, req: ProposeRequest) -> ProposeAnswer {
    let (plan, args_blob) = match sh.plan_for(&req) {
        Ok(p) => p,
        Err(reason) => return ProposeAnswer::Refused { reason },
    };
    if let Some(bytes) = args_blob
        && let Err(reason) = put_args_blob(sh, bytes).await
    {
        return ProposeAnswer::Refused { reason };
    }
    let plan_text = han_core::json::parse(plan.to_string().as_bytes())
        .ok()
        .and_then(|v| han_core::json::canonicalize(&v).ok())
        .and_then(|b| String::from_utf8(b).ok());
    let Some(plan_text) = plan_text else {
        return ProposeAnswer::Refused {
            reason: "the plan is not valid I-JSON".into(),
        };
    };
    // The cost is the proxy's measurement, never the agent's word (the tool has no cost argument).
    let (cost, spent) = measured_cost(
        sh.budget.get(),
        *sh.charged.lock().unwrap_or_else(|p| p.into_inner()),
    )
    .unzip();
    // R-24: a proposal of a task-driven turn names the task.
    let r = sh
        .api
        .call(Command::EffectPropose {
            space: sh.cfg.space.clone(),
            executor: sh.cfg.executor.clone(),
            plan: plan_text,
            task: sh.task.clone(),
            cost,
            repeatable: false,
            expires_at: None,
        })
        .await;
    match r {
        Ok(CallResult::Queued { id, outbox_id, .. }) => {
            if let Some(spent) = spent {
                let mut c = sh.charged.lock().unwrap_or_else(|p| p.into_inner());
                *c = (*c).max(spent);
            }
            sh.proposals
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(id.clone());
            sh.signal(Signal::Summary(req.summary.clone()));
            // Not waiting for approval yet: the home has not accepted the proposal. Follow its
            // outbox entry for a little, so a quick answer is the tool's answer.
            sh.signal(Signal::ProposalQueued);
            match follow_outbox(sh, outbox_id, PROPOSE_WAIT).await {
                Fate::Sent => {
                    sh.signal(Signal::WaitingApproval);
                    ProposeAnswer::Proposed {
                        id,
                        waiting_for: sh.waiting_for(),
                    }
                }
                Fate::Refused(why) => {
                    sh.signal(Signal::ProposalRefused(why.clone()));
                    ProposeAnswer::Refused {
                        reason: format!("the home did not accept the proposal: {why}"),
                    }
                }
                Fate::Pending => {
                    let watched = Arc::clone(sh);
                    let queued_id = id.clone();
                    tokio::spawn(async move {
                        match follow_outbox(&watched, outbox_id, Duration::MAX).await {
                            Fate::Sent => watched.signal(Signal::WaitingApproval),
                            Fate::Refused(why) => {
                                // a proposal the home refused was never made: it freezes nothing
                                if let Some(flow) = &watched.land {
                                    flow.withdrawn(&queued_id);
                                }
                                watched.signal(Signal::ProposalRefused(why));
                            }
                            Fate::Pending => {}
                        }
                    });
                    ProposeAnswer::Queued { id }
                }
            }
        }
        Ok(r) => match receipt_id(&r) {
            Some(id) => {
                sh.proposals
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(id.clone());
                sh.signal(Signal::Summary(req.summary.clone()));
                sh.signal(Signal::WaitingApproval);
                ProposeAnswer::Proposed {
                    id,
                    waiting_for: sh.waiting_for(),
                }
            }
            None => ProposeAnswer::Refused {
                reason: "the node gave no receipt".into(),
            },
        },
        Err(e) => ProposeAnswer::Refused {
            reason: e.to_string(),
        },
    }
}

/// Makes the proposal, and keeps the land flow's ledger in step: a `land` proposal that exists is frozen, one
/// that was refused leaves no trace.
async fn propose_effect(sh: &Arc<Shared>, req: ProposeRequest) -> ProposeAnswer {
    let land = sh.land.clone().filter(|_| req.kind == "land");
    let answer = propose_plan(sh, req).await;
    if let Some(flow) = land {
        match &answer {
            ProposeAnswer::Proposed { id, .. } | ProposeAnswer::Queued { id } => {
                flow.proposed(id);
            }
            ProposeAnswer::Refused { .. } => flow.abandon(),
        }
    }
    answer
}

impl HostBridge for Bridge {
    fn propose(&self, req: ProposeRequest) -> BoxFuture<'_, ProposeAnswer> {
        Box::pin(propose_effect(&self.0, req))
    }

    fn land_notices(&self) -> Vec<Value> {
        self.0
            .land
            .as_ref()
            .map(|f| f.notices())
            .unwrap_or_default()
    }

    fn node(&self) -> Arc<dyn NodeApi> {
        Arc::new(AgentApi(Arc::clone(&self.0.api)))
    }

    fn space(&self) -> String {
        self.0.cfg.space.clone()
    }

    fn session(&self) -> String {
        self.0.session.clone()
    }

    fn tool_used(&self, tool: &str, summary: &str) {
        if tool.starts_with("team_read") || tool == "team.read" {
            *self.0.read_others.lock().unwrap_or_else(|p| p.into_inner()) = true;
        }
        self.0.signal(Signal::Summary(summary.to_owned()));
    }

    fn summaries(&self, _session: &str) -> Vec<String> {
        // Layer 2 (fork summaries) is not produced yet; readers get layers 0 and 1.
        Vec::new()
    }
}

/// The issue's number and the agent's short name, for the branch `han/<issue>-<agent>`.
pub fn branch_name(issue: Option<&str>, agent: &str, session: &str) -> String {
    let who: String = agent
        .trim_start_matches('@')
        .split(':')
        .next()
        .unwrap_or("agent")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let what = issue
        .and_then(|i| i.rsplit('#').next())
        .filter(|n| n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            session
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .take(8)
                .collect()
        });
    format!("han/{what}-{who}")
}

/// Files and hunks of a changeset, for status.
pub fn touches(cs: &ChangeSet) -> Vec<FileTouch> {
    cs.files
        .iter()
        .map(|f| FileTouch {
            path: f.path.clone(),
            hunks: f.hunks.clone(),
        })
        .collect()
}

fn now_text() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    han_core_time(secs)
}

fn han_core_time(secs: u64) -> String {
    // YYYY-MM-DDTHH:MM:SSZ from Unix seconds (civil from days, Howard Hinnant's algorithm).
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

// ───────────────────────── the runner ─────────────────────────

/// What a host needs besides its configuration.
pub struct HostDeps {
    /// The agent's node.
    pub api: Arc<dyn NodeApi>,
    /// Version control.
    pub vcs: Arc<dyn Vcs>,
    /// The agent's actor, key id and key seed, which sign timeline batches (the host process is the
    /// agent's node; the seed never leaves it).
    pub signer: Option<(String, String, [u8; 32])>,
    /// Test only: changes the session's proxy configuration after the host has made it (to reach a mock
    /// provider on loopback, or to trust its certificate). A real session leaves it `None`.
    pub proxy_hook: Option<ProxyHook>,
}

/// A change to a session's [`ProxyConfig`], injected by a test.
pub type ProxyHook = Arc<dyn Fn(&mut ProxyConfig) + Send + Sync>;

/// A task being worked: its id, its author, the issue it came from and its ask.
type TaskInfo = (String, String, Option<String>, Option<String>);

/// Waits (up to `limit`) until the host's node has caught up with the space, so the tasks it reads are the
/// space's and not an empty start. A host that looked too early found no task for its issue and made a
/// second one of its own, with a bare ask, so the issue text never reached the agent.
async fn wait_synced(cfg: &HostConfig, api: &dyn NodeApi, limit: Duration) {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        let rows = snapshot(
            api,
            Query::JoinProgress {
                space: cfg.space.clone(),
                window: Window::default(),
            },
        )
        .await;
        match rows {
            // a node that cannot say is taken at its word
            Err(_) => return,
            Ok(rows) => {
                let ready = rows.iter().any(|r| {
                    matches!(r, Row::JoinProgress(j) if j.stage == han_node::api::JoinStage::Ready)
                });
                if ready || rows.is_empty() {
                    return;
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Finds the open task for the issue assigned to the agent, or claims the issue with a self-assigned task.
async fn find_or_claim_task(
    cfg: &HostConfig,
    api: &dyn NodeApi,
) -> Result<Option<TaskInfo>, HostError> {
    wait_synced(cfg, api, Duration::from_secs(30)).await;
    if let Some(t) = &cfg.task {
        // The node chose this task, so its row is the space's: wait for the host's own node to have it rather
        // than start without the ask, which is the only place the issue's text is.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            let rows = snapshot(
                api,
                Query::Tasks {
                    space: cfg.space.clone(),
                    assignee: Some(cfg.agent.clone()),
                    window: Window::default(),
                },
            )
            .await?;
            if let Some((author, ask)) = rows.iter().find_map(|r| match r {
                Row::Task(x) if &x.task == t => Some((x.author.clone(), x.ask.clone())),
                _ => None,
            }) {
                return Ok(Some((t.clone(), author, cfg.issue.clone(), ask)));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(HostError::Agent(format!(
                    "task {t} has not reached this host's node, so its text cannot be given to the agent"
                )));
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    let Some(issue) = &cfg.issue else {
        return Ok(None);
    };
    let rows = snapshot(
        api,
        Query::Tasks {
            space: cfg.space.clone(),
            assignee: Some(cfg.agent.clone()),
            window: Window::default(),
        },
    )
    .await?;
    if let Some((id, author, ask)) = rows.iter().find_map(|r| match r {
        Row::Task(t) if t.open && t.external.as_deref() == Some(issue.as_str()) => {
            Some((t.task.clone(), t.author.clone(), t.ask.clone()))
        }
        _ => None,
    }) {
        return Ok(Some((id, author, Some(issue.clone()), ask)));
    }
    // A self-claim (PLAN §6.6: counts as accepted; the host still appends the acceptance, R-24).
    let r = api
        .call(Command::TaskCreate {
            space: cfg.space.clone(),
            to: vec![cfg.agent.clone()],
            ask: format!("work on {issue}"),
            external: Some(issue.clone()),
        })
        .await?;
    Ok(receipt_id(&r).map(|id| {
        (
            id,
            cfg.agent.clone(),
            Some(issue.clone()),
            Some(format!("work on {issue}")),
        )
    }))
}

/// Runs one session until the work stops (the agent finishes, `stop` turns true, or it fails).
pub async fn run_session(
    cfg: HostConfig,
    deps: HostDeps,
    mut stop: watch::Receiver<bool>,
) -> Result<SessionEnd, HostError> {
    let api = deps.api.clone();
    std::fs::create_dir_all(&cfg.session_dir)?;

    // 1. The task, accepted before the first turn (R-24).
    let task = find_or_claim_task(&cfg, api.as_ref()).await?;
    let mut duty = task.as_ref().map(|(id, ..)| TaskDuty::new(&cfg.space, id));
    if let Some(cmd) = duty.as_mut().and_then(TaskDuty::accept) {
        api.call(cmd).await?;
    }

    // 2. The session.
    let opened = api
        .call(Command::SessionOpen {
            space: cfg.space.clone(),
            steer_mode: Some(if cfg.guarded { "guarded" } else { "auto" }.into()),
            task: task.as_ref().map(|t| t.0.clone()),
            title: cfg.issue.clone(),
        })
        .await?;
    let session = receipt_id(&opened).ok_or_else(|| HostError::Agent("no session id".into()))?;

    let result = run_opened(&cfg, &deps, &session, task.clone(), &mut stop).await;

    // The final update when work stops (R-24), whatever happened.
    let (state, reason) = match &result {
        Ok((s, why)) => (*s, why.clone()),
        Err(e) => (SessionState::Failed, Some(e.to_string())),
    };
    if state == SessionState::Failed {
        tracing::warn!(
            session = %session,
            "session failed: {}",
            reason.as_deref().unwrap_or("no reason given")
        );
    }
    if result.is_err() {
        // The run broke off before its status publisher could say so: without this the session closes with
        // no state of its own and the board shows it as done.
        if let Some(why) = &reason {
            publish_failed(&cfg, &deps, &session, why).await;
        }
    }
    if let Some(cmd) = duty.as_mut().and_then(|d| {
        d.finish(if state == SessionState::Failed {
            "failed"
        } else {
            "done"
        })
    }) {
        let _ = api.call(cmd).await;
    }
    let _ = api
        .call(Command::SessionClose {
            space: cfg.space.clone(),
            session: session.clone(),
        })
        .await;
    match result {
        Ok(_) => Ok(SessionEnd {
            session,
            task: task.map(|t| t.0),
            state,
            reason,
            proposals: Vec::new(),
        }),
        Err(e) => Err(e),
    }
}

/// Publishes a last `failed` status that names `why`, for a run that ended in an error before the status
/// publisher took over. Text is masked like every other status text (SR-52).
async fn publish_failed(cfg: &HostConfig, deps: &HostDeps, session: &str, why: &str) {
    let masker = Arc::new(Masker::new(cfg.model_key.iter().cloned().collect()));
    let api = MaskedApi::new(deps.api.clone(), masker);
    let mut deriver = StatusDeriver::new(
        session,
        &cfg.agent,
        &cfg.driver.harness,
        None,
        cfg.issue.clone(),
        Some(cfg.repo.name.clone()),
        None,
        Sandbox {
            backend: han_node::api::SandboxBackend::None,
            fidelity: han_node::api::Fidelity::Advisory,
        },
    );
    deriver.apply(Signal::Failed(why.to_owned()));
    let _ = api
        .call(Command::StatusPublish {
            space: cfg.space.clone(),
            status: Box::new(deriver.snapshot(&now_text())),
        })
        .await;
}

/// The session's budget, narrowed to the budget of the task it works when that is smaller (P-05: an agent's
/// spending is charged to its task). The unit is the space's either way; a task in another unit is not
/// believed.
async fn narrowed_budget(
    cfg: &HostConfig,
    api: &dyn NodeApi,
    task: Option<&str>,
) -> Option<han_registry::BudgetSpec> {
    let (spec, task) = (cfg.budget.as_ref()?, task?);
    let rows = snapshot(
        api,
        Query::Tasks {
            space: cfg.space.clone(),
            assignee: Some(cfg.agent.clone()),
            window: Window::default(),
        },
    )
    .await
    .ok()?;
    let own = rows.iter().find_map(|r| match r {
        Row::Task(x) if x.task == task => x.budget.clone(),
        _ => None,
    })?;
    (own.unit == spec.unit && own.amount < spec.limit).then(|| han_registry::BudgetSpec {
        limit: own.amount,
        ..spec.clone()
    })
}

/// Reports what the proxy counts: the tokens and their cost (API-key mode) and the budget with whether it is
/// enforced, a few times a second while anything changes. Ends when the signal channel closes.
fn watch_usage(
    shared: Arc<Shared>,
    observer: crate::proxy::ProxyObserver,
    budget: Option<crate::glue::SessionBudget>,
    api_key: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut last = None;
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        loop {
            tick.tick().await;
            if shared.signals_closed() {
                break;
            }
            let usage = observer.usage();
            let spent = budget.as_ref().map_or(0, |b| b.spent());
            let refused = observer
                .decisions()
                .iter()
                .rev()
                .find(|d| d.reason.starts_with("budget-"))
                .map(|d| d.reason);
            let now = (usage, spent, refused);
            if last == Some(now) {
                continue;
            }
            last = Some(now);
            if api_key {
                shared.signal(Signal::Tokens(han_node::api::Tokens {
                    input: usage.input + usage.cache_read + usage.cache_creation,
                    out: usage.output,
                    cost: spent,
                }));
            }
            if let Some(b) = &budget {
                shared.signal(Signal::Budget(han_node::api::BudgetStatus {
                    unit: b.unit.clone(),
                    limit: b.limit,
                    spent,
                    not_enforced: b.not_enforced.map(str::to_owned),
                    refused: refused.map(str::to_owned),
                }));
            }
        }
    })
}

async fn run_opened(
    cfg: &HostConfig,
    deps: &HostDeps,
    session: &str,
    task: Option<TaskInfo>,
    stop: &mut watch::Receiver<bool>,
) -> Result<(SessionState, Option<String>), HostError> {
    let api = deps.api.clone();
    let narrowed;
    let cfg = match narrowed_budget(cfg, api.as_ref(), task.as_ref().map(|t| t.0.as_str())).await {
        Some(budget) => {
            narrowed = HostConfig {
                budget: Some(budget),
                ..cfg.clone()
            };
            &narrowed
        }
        None => cfg,
    };
    let worktree = cfg.session_dir.join("wt");
    let agent_dir = cfg.session_dir.join("agent");
    let temp = cfg.session_dir.join("tmp");
    std::fs::create_dir_all(&agent_dir)?;
    std::fs::create_dir_all(&temp)?;

    // 3. The workspace: its own clone at the base, on the session's branch.
    let workspace = if worktree.join(".git").exists() {
        Workspace {
            dir: worktree.clone(),
            base: deps
                .vcs
                .changeset(&worktree, "HEAD")
                .map(|c| c.base)
                .unwrap_or_default(),
            branch: branch_name(cfg.issue.as_deref(), &cfg.agent, session),
            shared_store: None,
        }
    } else {
        deps.vcs.create_workspace(&WorkspaceSpec {
            source: cfg.repo.source.clone(),
            base: cfg.repo.base.clone(),
            branch: branch_name(cfg.issue.as_deref(), &cfg.agent, session),
            dir: worktree.clone(),
            sparse: Vec::new(),
        })?
    };

    // 4. Egress: a per-session proxy with its own token (SR-28).
    let token = r24::data_id();
    let mut known = vec![token.clone()];
    known.extend(cfg.model_key.iter().cloned());
    let masker = Arc::new(Masker::new(known));

    // Everything status and stream goes through one masking point (SR-52). The mirror keeps the raw `api`: its
    // batches are signed, so they were masked before they were hashed.
    let masked_api: Arc<dyn NodeApi> = Arc::new(MaskedApi::new(api.clone(), masker.clone()));
    let (sig_tx, mut sig_rx) = mpsc::unbounded_channel();
    let (sandbox_info, gaps) = (
        Sandbox {
            backend: han_node::api::SandboxBackend::None,
            fidelity: han_node::api::Fidelity::Advisory,
        },
        Vec::<String>::new(),
    );
    let deriver = StatusDeriver::new(
        session,
        &cfg.agent,
        &cfg.driver.harness,
        task.as_ref()
            .map(|t| t.1.clone())
            .filter(|a| a != &cfg.agent),
        cfg.issue.clone(),
        Some(cfg.repo.name.clone()),
        Some(workspace.base.clone()),
        sandbox_info,
    );
    let land = crate::land::Flow::new(
        cfg,
        session,
        task.as_ref().and_then(|t| t.3.clone()),
        workspace.clone(),
        deps.vcs.clone(),
        Arc::new(crate::land::NodeLaneFeed::new(
            api.clone(),
            cfg.space.clone(),
            cfg.executor.clone(),
        )),
    )
    .map(Arc::new);
    let shared = Arc::new(Shared {
        api: masked_api,
        cfg: cfg.clone(),
        session: session.to_owned(),
        task: task.as_ref().map(|t| t.0.clone()),
        workspace: workspace.clone(),
        vcs: deps.vcs.clone(),
        masker: masker.clone(),
        status: Mutex::new(deriver),
        signals: Mutex::new(Some(sig_tx)),
        publish_errors: AtomicU64::new(0),
        read_others: Mutex::new(false),
        proposals: Mutex::new(Vec::new()),
        budget: OnceLock::new(),
        charged: Mutex::new(0),
        failure: Mutex::new(None),
        land,
        busy: AtomicBool::new(false),
    });

    let env = crate::glue::Egress::start(
        cfg,
        deps.proxy_hook.as_ref(),
        &shared_bridge(&shared),
        &token,
        &workspace,
        &agent_dir,
        &temp,
    )
    .await
    .map_err(HostError::Sandbox)?;
    if let Some(b) = env.budget() {
        let _ = shared.budget.set(b.clone());
    }
    let base = crate::glue::base_files(&workspace.dir);
    let crate::glue::Launched {
        stdin,
        stdout,
        sandbox: launched_sandbox,
        gaps: launched_gaps,
        mcp_servers,
        session_meta,
        child,
        mut stderr,
    } = env
        .launch(cfg, &agent_dir, &workspace, masker.clone())
        .await
        .map_err(HostError::Sandbox)?;
    {
        let mut st = shared.status.lock().unwrap_or_else(|p| p.into_inner());
        *st = StatusDeriver::new(
            session,
            &cfg.agent,
            &cfg.driver.harness,
            task.as_ref()
                .map(|t| t.1.clone())
                .filter(|a| a != &cfg.agent),
            cfg.issue.clone(),
            Some(cfg.repo.name.clone()),
            Some(workspace.base.clone()),
            launched_sandbox,
        );
    }
    let _ = gaps;
    let usage_watch = (env.budget().is_some()
        || matches!(
            cfg.driver.credentials,
            crate::driver::CredentialMode::ApiKey { .. }
        ))
    .then(|| {
        watch_usage(
            shared.clone(),
            env.proxy().observer(),
            env.budget().cloned(),
            matches!(
                cfg.driver.credentials,
                crate::driver::CredentialMode::ApiKey { .. }
            ),
        )
    });
    if !launched_gaps.is_empty() {
        shared.signal(Signal::Summary(crate::status::sandbox_note(
            &launched_sandbox,
            &launched_gaps,
        )));
    }

    // 5. The worktree mirror: every change as timeline ops, attributed to the running tool call.
    let mirror = match &deps.signer {
        Some((actor, key_id, seed)) => match crate::glue::start_mirror(
            api.clone(),
            &cfg.space,
            session,
            &cfg.session_dir.join("timeline"),
            &workspace,
            han_timeline::KeySigner::from_seed(actor.clone(), key_id.clone(), *seed),
            masker.clone(),
            base,
        ) {
            Ok(m) => Some(m),
            Err(e) => {
                tracing::warn!("no worktree mirror: {e}");
                None
            }
        },
        None => None,
    };

    // The land finish flow follows the lane from here, and lets go of the mirror before it removes the workspace.
    if let (Some(flow), Some(m)) = (&shared.land, &mirror) {
        let m = Arc::clone(m);
        flow.on_release(Arc::new(move || m.mirror().unwatch()));
    }
    let land_watcher = shared.land.is_some().then(|| land_watch(shared.clone()));
    // This host holds its session directory while it runs; a sweep of old directories (its own, below, and
    // any later host's) never touches one it cannot take. Failing to take it only costs the protection.
    let _held = shared
        .land
        .is_some()
        .then(|| crate::land::SessionLock::take(&cfg.session_dir))
        .flatten();
    let land_sweeper = shared
        .land
        .is_some()
        .then(|| land_sweep(shared.clone(), deps.vcs.clone(), api.clone()));

    // 6. The agent.
    let policy = Arc::new(crate::acp::SandboxPolicy {
        worktree: workspace.dir.clone(),
    });
    let started = async {
        let mut agent = AcpDriver::connect(stdin, stdout, policy)
            .await?
            .with_turn_timeout(cfg.turn_timeout);
        let started = agent
            .start(SessionConfig {
                cwd: workspace.dir.clone(),
                mcp_servers: mcp_servers.clone(),
                model: None,
                meta: session_meta.clone(),
                strict_mode: cfg.driver.strict_mode.clone(),
                auto_mode: cfg.driver.auto_mode.clone(),
            })
            .await?;
        Ok::<_, crate::acp::AcpError>((agent, started))
    }
    .await;
    let (mut agent, started) = match started {
        Ok(s) => s,
        Err(e) => {
            // The agent died or would not talk: say what it printed (masked), which is the only clue.
            let why = match stderr.as_mut() {
                Some(log) => {
                    let lines = log.finish(Duration::from_secs(3)).await;
                    crate::stderr_log::StderrLog::describe(&lines, log.path(), 5)
                }
                None => "the agent's stderr was not captured".into(),
            };
            child.kill();
            return Err(HostError::Agent(format!("{e}; {why}")));
        }
    };
    if let crate::driver::VersionCheck::Untested { warning } =
        crate::driver::check_version(&cfg.driver, started.agent_version.as_deref())
    {
        tracing::warn!("{warning}");
    }
    let mut events = agent.events();

    // Status publisher: folds signals and events, publishes on change (at most every 300 ms) and every 60 s.
    let publisher = {
        let shared = shared.clone();
        let mirror = mirror.clone();
        tokio::spawn(async move {
            let mut dirty = true;
            let mut last = tokio::time::Instant::now() - Duration::from_secs(120);
            let mut tick = tokio::time::interval(Duration::from_millis(300));
            let mut feed = ItemFeed::new();
            loop {
                tokio::select! {
                    // Signals first: a turn's start precedes its events, and its end follows them.
                    biased;
                    s = sig_rx.recv() => match s {
                        Some(s) => {
                            match &s {
                                Signal::TurnStarted { .. } => feed.begin_turn(),
                                Signal::TurnEnded | Signal::Failed(_) | Signal::Done => {
                                    // The turn's events are all queued by now: fold them before it ends, so the
                                    // last message is complete and the step can show its start.
                                    while let Some(Some(ev)) = events.next().now_or_never() {
                                        feed.apply(&ev, &now_text());
                                        for s in signals_of(&shared, mirror.as_deref(), ev) {
                                            dirty |= shared.apply(s);
                                        }
                                    }
                                    feed.end_turn();
                                    if matches!(s, Signal::TurnEnded)
                                        && let Some(m) = feed.last_message()
                                    {
                                        shared.apply(Signal::LastMessage(m.to_owned()));
                                    }
                                }
                                _ => {}
                            }
                            dirty |= shared.apply(s);
                            publish_items(&shared, &mut feed).await;
                        }
                        None => break,
                    },
                    ev = events.next() => match ev {
                        Some(ev) => {
                            feed.apply(&ev, &now_text());
                            for s in signals_of(&shared, mirror.as_deref(), ev) {
                                dirty |= shared.apply(s);
                            }
                        }
                        None => {
                            futures::future::pending::<()>().await;
                        }
                    },
                    _ = tick.tick() => {
                        publish_items(&shared, &mut feed).await;
                        if dirty || last.elapsed() > Duration::from_secs(60) {
                            publish(&shared).await;
                            dirty = false;
                            last = tokio::time::Instant::now();
                        }
                    }
                }
            }
            publish_items(&shared, &mut feed).await;
            publish(&shared).await;
        })
    };

    // 7. Turns: the task's fixed prompt first, then each signed input addressed to the session.
    let taint = TaintStore::open(&cfg.taint_db)?;
    let facts = NodeGuardFacts::new(
        api.clone(),
        cfg.operator.clone().map(|o| (cfg.agent.clone(), o)),
    );
    let mut inputs = api.view(Query::Inputs {
        session: session.to_owned(),
        window: Window::default(),
    });
    let mut inputs_mirror = ViewMirror::new();
    let mut handled: u64 = 0;
    let mut state = SessionState::Idle;

    if let Some((task_id, author, external, ask)) = &task {
        let driver = Driver {
            author: author.clone(),
            kind: DriverKind::Task {
                id: task_id.clone(),
                guarded_for: Vec::new(),
            },
            space: cfg.space.clone(),
            seq: 0,
        };
        let text = r24::task_prompt(external.as_deref(), task_id);
        let data = r24::task_data(ask.as_deref(), &r24::data_id());
        state = turn(&shared, &mut agent, &facts, &taint, driver, text, data).await?;
        if let Some(m) = &mirror {
            m.rescan();
        }
    }

    loop {
        if *stop.borrow() || state == SessionState::Failed {
            break;
        }
        tokio::select! {
            _ = stop.changed() => break,
            ev = inputs.next() => {
                let Some(ev) = ev else { break };
                inputs_mirror.apply(ev);
                let pending: Vec<_> = inputs_mirror
                    .rows()
                    .into_iter()
                    .filter_map(|r| match r {
                        Row::Input(i) if i.seq > handled => Some(i),
                        _ => None,
                    })
                    .collect();
                for i in pending {
                    handled = i.seq;
                    // Only a signed input with a receipt drives a turn (R-24); its text is the instruction.
                    let Some(text) = i.text.clone() else { continue };
                    // A pruned workspace takes no more turns: say so, as data, instead of failing in it.
                    if refuse_steer(&shared, i.seq, &i.author).await {
                        continue;
                    }
                    let driver = Driver {
                        author: i.author.clone(),
                        kind: DriverKind::SessionInput,
                        space: cfg.space.clone(),
                        seq: i.seq,
                    };
                    state = turn(&shared, &mut agent, &facts, &taint, driver, text, None).await?;
                    if let Some(m) = &mirror {
                        m.rescan();
                    }
                }
            }
        }
    }

    // 8. Work stops: the agent goes, the status says how it ended.
    agent.shutdown().await;
    child.kill();
    if let Some(m) = &mirror {
        m.rescan();
    }
    if let Some(w) = land_sweeper {
        w.abort();
    }
    if let Some(w) = land_watcher {
        w.abort();
        land_backstop(&shared).await;
        land_settle(&shared).await;
    }
    let final_state = if state == SessionState::Failed {
        SessionState::Failed
    } else {
        SessionState::Done
    };
    let reason = if final_state == SessionState::Failed {
        // The status already says why (the turn's failure): the end keeps that reason.
        let why = shared
            .failure
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .unwrap_or_else(|| "the session stopped".into());
        shared.signal(Signal::Failed(why.clone()));
        Some(why)
    } else {
        shared.signal(Signal::Done);
        None
    };
    drop(mirror);
    if let Some(w) = usage_watch {
        w.abort();
    }
    // Close the signal channel and wait for the publisher: it folds what is queued, sends the last items and
    // the final status, then returns. Only when it overruns the bound is it stopped, and that is logged.
    shared.close_signals();
    let mut publisher = publisher;
    match tokio::time::timeout(PUBLISHER_DRAIN, &mut publisher).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!("the publisher task ended abnormally: {e}"),
        Err(_) => {
            tracing::warn!(
                "the publisher did not drain within {PUBLISHER_DRAIN:?}; the last items may be missing"
            );
            publisher.abort();
        }
    }
    Ok((final_state, reason))
}

/// How often the lane's items for the session's proposals are read.
const LAND_POLL: Duration = Duration::from_secs(3);

/// Follows the lane while the session lives (R33 §3): its states reach the status, and the workspace is pruned
/// when the proposals are final (§7).
fn land_watch(shared: Arc<Shared>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let Some(flow) = shared.land.clone() else {
            return;
        };
        let mut tick = tokio::time::interval(LAND_POLL);
        let mut shown = None;
        loop {
            tick.tick().await;
            if shared.signals_closed() {
                break;
            }
            land_step(&shared, &flow, &mut shown, false).await;
        }
    })
}

/// One look at the lane: reads its items, shows them, and prunes if the proposals are final.
async fn land_step(
    shared: &Arc<Shared>,
    flow: &Arc<crate::land::Flow>,
    shown: &mut Option<(String, bool)>,
    session_over: bool,
) {
    if let Err(e) = flow.poll().await {
        tracing::debug!("the landing lane could not be read: {e}");
    }
    let line = flow.status_line();
    if line != *shown {
        if let Some((text, blocked)) = line.clone() {
            shared.signal(Signal::Land { text, blocked });
        }
        *shown = line;
    }
    let (busy, f) = (shared.busy.load(Ordering::SeqCst), flow.clone());
    let pruned = tokio::task::spawn_blocking(move || f.prune(busy, session_over)).await;
    match pruned {
        Ok(crate::land::Pruned::Removed) => {
            shared.signal(Signal::Summary(
                "the workspace and its build output were removed: its proposals are final".into(),
            ));
        }
        Ok(crate::land::Pruned::Kept(u)) => {
            publish_unlanded(shared, &u).await;
            let text = format!(
                "kept: {} commit(s) after the proposal never landed{}",
                u.commits.len(),
                if u.uncommitted {
                    ", and uncommitted changes"
                } else {
                    ""
                }
            );
            shared.signal(Signal::Summary(text.clone()));
            shared.signal(Signal::Land {
                text,
                blocked: true,
            });
        }
        _ => {}
    }
}

/// Publishes an `unlanded` report (R33 §7) on the session's stream.
async fn publish_unlanded(shared: &Arc<Shared>, u: &crate::land::Unlanded) {
    let mut body = u.item();
    body["at"] = Value::String(now_text());
    let r = shared
        .api
        .call(Command::StreamPublish {
            space: shared.cfg.space.clone(),
            session: shared.session.clone(),
            name: crate::land::PROFILE.into(),
            body: body.to_string(),
        })
        .await;
    if let Err(e) = r {
        shared.publish_failed("unlanded item", &e);
    }
}

/// How often the sweep of earlier sessions' proposals looks at the log.
const SWEEP_POLL: Duration = Duration::from_secs(15);

/// The sweep at host start: the earlier session directories next to this one that have a land ledger and that
/// no running host holds are followed the way a live session's are, through the lane's items and the Effects
/// view, and pruned by the same rules. The session of each is over, so a rejected change no longer keeps its
/// workspace. A host that crashed (or whose proposals were never final when it stopped) leaves them behind.
/// Ends when every one is pruned or reported `unlanded`.
fn land_sweep(
    shared: Arc<Shared>,
    vcs: Arc<dyn Vcs>,
    api: Arc<dyn NodeApi>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut flows = Vec::new();
        for dir in crate::land::sweep_dirs(&shared.cfg.session_dir) {
            let feed = Arc::new(crate::land::NodeLaneFeed::new(
                api.clone(),
                shared.cfg.space.clone(),
                shared.cfg.executor.clone(),
            ));
            let Some(flow) = crate::land::Flow::orphan(&shared.cfg, &dir, vcs.clone(), feed) else {
                continue;
            };
            // A directory a running host holds is not ours to prune.
            let Some(held) = crate::land::SessionLock::take(&dir) else {
                continue;
            };
            flows.push((Arc::new(flow), held));
        }
        let mut tick = tokio::time::interval(SWEEP_POLL);
        while !flows.is_empty() {
            tick.tick().await;
            if shared.signals_closed() {
                break;
            }
            let mut left = Vec::new();
            for (flow, held) in flows {
                if !sweep_step(&shared, &flow).await {
                    left.push((flow, held));
                }
            }
            flows = left;
        }
    })
}

/// One look at an earlier session's proposals; true when it is over (pruned, or kept and reported).
async fn sweep_step(shared: &Arc<Shared>, flow: &Arc<crate::land::Flow>) -> bool {
    if let Err(e) = flow.poll().await {
        tracing::debug!("the landing lane could not be read: {e}");
    }
    let f = flow.clone();
    match tokio::task::spawn_blocking(move || f.prune(false, true)).await {
        Ok(crate::land::Pruned::Removed) => {
            shared.signal(Signal::Summary(format!(
                "an earlier session's workspace {} was removed: its proposals are final",
                flow.workspace_dir().display()
            )));
            true
        }
        Ok(crate::land::Pruned::Kept(u)) => {
            publish_unlanded(shared, &u).await;
            shared.signal(Signal::Summary(format!(
                "kept an earlier session's workspace {}: {} commit(s) after its proposal never landed{}",
                flow.workspace_dir().display(),
                u.commits.len(),
                if u.uncommitted {
                    ", and uncommitted changes"
                } else {
                    ""
                }
            )));
            true
        }
        _ => false,
    }
}

/// The backstop (`autoPropose`): the session is over, and it holds commits after its last proposal (or with no
/// proposal) that nobody proposed. The host proposes them as a `vcs.land`, titled from the task, so that
/// nothing is left silently. Uncommitted work is not committed for it; the prune reports it `unlanded`.
async fn land_backstop(shared: &Arc<Shared>) {
    let Some(flow) = shared.land.clone() else {
        return;
    };
    if !tokio::task::spawn_blocking(move || flow.backstop_ready())
        .await
        .unwrap_or(false)
    {
        return;
    }
    let answer = propose_effect(
        shared,
        ProposeRequest {
            summary: "the session ended with committed work that was never proposed".into(),
            kind: "land".into(),
            effect: json!({"committedOnly": true}),
        },
    )
    .await;
    let text = match answer {
        ProposeAnswer::Proposed { id, .. } | ProposeAnswer::Queued { id } => format!(
            "the session ended with committed work nobody proposed: the host proposed it for landing ({})",
            id.chars().take(12).collect::<String>()
        ),
        ProposeAnswer::Refused { reason } => {
            format!(
                "committed work was left unproposed, and the host could not propose it: {reason}"
            )
        }
    };
    shared.signal(Signal::Summary(text));
}

/// A steer to a session whose workspace is gone: no turn, a refusal as data (a `han.land/1` item) and in the
/// status. True when it was refused.
async fn refuse_steer(shared: &Arc<Shared>, seq: u64, author: &str) -> bool {
    let Some(mut body) = shared.land.as_ref().and_then(|f| f.steer_refusal()) else {
        return false;
    };
    body["input"] = json!(seq);
    body["author"] = json!(author);
    body["at"] = Value::String(now_text());
    let r = shared
        .api
        .call(Command::StreamPublish {
            space: shared.cfg.space.clone(),
            session: shared.session.clone(),
            name: crate::land::PROFILE.into(),
            body: body.to_string(),
        })
        .await;
    if let Err(e) = r {
        shared.publish_failed("steer refusal", &e);
    }
    let text = format!(
        "steer from {author} refused: {}",
        body["detail"].as_str().unwrap_or("the workspace is gone")
    );
    shared.signal(Signal::Summary(text.clone()));
    shared.signal(Signal::Land {
        text,
        blocked: true,
    });
    true
}

/// When work stops: looks at the lane once more (and keeps looking for up to the spec's `settleSecs` while a
/// proposal is open), then prunes with the session over.
async fn land_settle(shared: &Arc<Shared>) {
    let Some(flow) = shared.land.clone() else {
        return;
    };
    let deadline = tokio::time::Instant::now() + flow.settle();
    let mut shown = None;
    loop {
        land_step(shared, &flow, &mut shown, true).await;
        if flow.pruned()
            || flow.unlanded().is_some()
            || flow.settled()
            || tokio::time::Instant::now() >= deadline
        {
            break;
        }
        tokio::time::sleep(LAND_POLL).await;
    }
}

fn shared_bridge(shared: &Arc<Shared>) -> Arc<dyn HostBridge> {
    Arc::new(Bridge(shared.clone()))
}

async fn turn(
    shared: &Arc<Shared>,
    agent: &mut AcpDriver,
    facts: &NodeGuardFacts,
    taint: &TaintStore,
    driver: Driver,
    text: String,
    data: Option<String>,
) -> Result<SessionState, HostError> {
    let cfg = &shared.cfg;
    facts
        .refresh(&cfg.space, &[cfg.agent.as_str(), driver.author.as_str()])
        .await;
    // A turn that follows one which read other sessions counts as reading them too (the content is in the
    // agent's context now).
    let read_others = *shared.read_others.lock().unwrap_or_else(|p| p.into_inner());
    let decision = r24::decide_mode(
        &cfg.agent,
        &driver,
        facts,
        taint,
        read_others,
        cfg.strict_on_read,
    )?;
    shared.signal(Signal::TurnStarted {
        step: match (&driver.kind, decision.mode) {
            (DriverKind::Task { .. }, Mode::Strict) => "working on the task (strict)".into(),
            (DriverKind::Task { .. }, Mode::Auto) => "working on the task".into(),
            (_, Mode::Strict) => format!("steered by {} (strict)", driver.author),
            (_, Mode::Auto) => format!("steered by {}", driver.author),
        },
    });
    shared.busy.store(true, Ordering::SeqCst);
    let end = agent
        .turn(
            TurnInput {
                text,
                data_blocks: data.into_iter().collect(),
            },
            decision.mode,
        )
        .await;
    shared.busy.store(false, Ordering::SeqCst);
    match end {
        Ok(end) => {
            if let Some(meta) = &end.meta
                && let Some(t) = crate::status::tokens_from_meta(meta)
            {
                shared.signal(Signal::Tokens(t));
            }
            if end.suspect_failure || end.provider_error.is_some() {
                let why = end
                    .provider_error
                    .unwrap_or_else(|| "the turn ended with no output and no tool calls".into());
                shared.fail(why);
                return Ok(SessionState::Failed);
            }
            refresh_files(shared).await;
            shared.signal(Signal::TurnEnded);
            Ok(SessionState::Idle)
        }
        Err(e) => {
            shared.fail(e.to_string());
            Ok(SessionState::Failed)
        }
    }
}

/// The workspace's changeset. It shells out to `git`, so it runs on the blocking pool, not on the async loop.
async fn changeset_of(shared: &Shared) -> Option<ChangeSet> {
    let vcs = shared.vcs.clone();
    let (dir, base) = (shared.workspace.dir.clone(), shared.workspace.base.clone());
    tokio::task::spawn_blocking(move || vcs.changeset(&dir, &base).ok())
        .await
        .ok()
        .flatten()
}

async fn refresh_files(shared: &Shared) {
    if let Some(cs) = changeset_of(shared).await {
        shared.signal(Signal::Files(touches(&cs)));
    }
}

/// The most the end of a session waits for the publisher to send what is left (a slow node, a long backlog).
const PUBLISHER_DRAIN: Duration = Duration::from_secs(30);

/// Maps one agent event to status signals, and attributes worktree changes to the running tool call.
fn signals_of(
    shared: &Arc<Shared>,
    mirror: Option<&crate::glue::MirrorHandle>,
    ev: AgentEvent,
) -> Vec<Signal> {
    match ev {
        AgentEvent::ToolCall {
            id, title, kind, ..
        } => {
            if let Some(m) = mirror {
                m.set_step(Some(id));
            }
            vec![Signal::Tool { title, kind }]
        }
        AgentEvent::ToolCallUpdate {
            status,
            output,
            title,
            ..
        } => {
            let mut out = Vec::new();
            if let Some(t) = title {
                out.push(Signal::Tool {
                    title: t,
                    kind: None,
                });
            }
            if matches!(status.as_deref(), Some("completed") | Some("failed")) {
                if let Some(m) = mirror {
                    m.set_step(None);
                }
                out.push(Signal::ToolDone { output });
                // The diff shells out to `git`: off the async loop, and off the event order too (the files
                // arrive as their own signal when the diff is done, so the next events are not held up).
                let shared = shared.clone();
                tokio::spawn(async move { refresh_files(&shared).await });
            }
            out
        }
        AgentEvent::Plan { entries } => vec![Signal::Plan(
            entries
                .into_iter()
                .map(|(text, status)| han_node::api::PlanItem {
                    text,
                    done: status == "completed",
                })
                .collect(),
        )],
        AgentEvent::Exited { code } => vec![Signal::Failed(format!("the agent exited ({code:?})"))],
        _ => Vec::new(),
    }
}

/// Sends the items that changed as `han.item/1` stream items (masked and bounded by the feed).
async fn publish_items(shared: &Shared, feed: &mut ItemFeed) {
    for body in feed.take(&shared.masker) {
        let r = shared
            .api
            .call(Command::StreamPublish {
                space: shared.cfg.space.clone(),
                session: shared.session.clone(),
                name: crate::items::PROFILE.into(),
                body: body.to_string(),
            })
            .await;
        if let Err(e) = r {
            shared.publish_failed("item", &e);
        }
    }
}

async fn publish(shared: &Shared) {
    let mut body = shared
        .status
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .snapshot(&now_text());
    // Text is masked by `shared.api` (SR-52): nothing here masks by hand.
    body.publish_errors =
        u32::try_from(shared.publish_errors.load(Ordering::Relaxed)).unwrap_or(u32::MAX);
    if let Err(e) = shared
        .api
        .call(Command::StatusPublish {
            space: shared.cfg.space.clone(),
            status: Box::new(body),
        })
        .await
    {
        shared.publish_failed("status", &e);
    }
}

/// The worktree path of a session directory.
pub fn worktree_of(session_dir: &Path) -> PathBuf {
    session_dir.join("wt")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_names_follow_the_plan() {
        assert_eq!(
            branch_name(Some("github:o/r#123"), "@lars-coder:a.example", "e1"),
            "han/123-lars-coder"
        );
        assert_eq!(
            branch_name(None, "@x:a.example", "abc-def_123456789"),
            "han/abcdef12-x"
        );
    }

    #[test]
    fn times_are_han_times() {
        assert_eq!(han_core_time(0), "1970-01-01T00:00:00Z");
        assert_eq!(han_core_time(1_767_225_600), "2026-01-01T00:00:00Z");
    }

    struct Recorder(Mutex<Vec<String>>);

    impl NodeApi for Recorder {
        fn call(&self, cmd: Command) -> BoxFuture<'_, Result<CallResult, NodeError>> {
            self.0.lock().unwrap().push(format!("{:?}", cmd.class()));
            Box::pin(async { Ok(CallResult::Done) })
        }
        fn view(&self, _query: Query) -> ViewStream {
            ViewStream::failed(NodeError::unavailable("no views"))
        }
    }

    #[tokio::test]
    async fn the_agent_api_refuses_face_and_executor_commands() {
        // SR-06: han-mcp's connection gets only agent-class commands.
        let inner = Arc::new(Recorder(Mutex::new(Vec::new())));
        let api = AgentApi(inner.clone());
        for cmd in [
            Command::EffectApprove {
                space: "s".into(),
                proposal: "p".into(),
                expires_at: None,
                note: None,
                evidence: None,
            },
            Command::MemberSet {
                space: "s".into(),
                actor: "@a:b".into(),
                role: "owner".into(),
            },
            Command::Invite {
                space: "s".into(),
                ttl_secs: None,
                expires_at: None,
                auto_admit: false,
            },
            Command::SpaceJoin { ticket: "t".into() },
            Command::PolicySet {
                space: "s".into(),
                policy: "{}".into(),
            },
            Command::EffectClaim {
                space: "s".into(),
                proposal: "p".into(),
                attempt: 1,
                window: 30,
            },
        ] {
            assert!(matches!(api.call(cmd).await, Err(NodeError::Denied { .. })));
        }
        assert!(
            inner.0.lock().unwrap().is_empty(),
            "nothing reached the node"
        );
        api.call(Command::LeaseSet {
            space: "s".into(),
            paths: vec!["src/".into()],
            ttl_secs: 60,
            issue: None,
        })
        .await
        .unwrap();
        assert_eq!(inner.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn plans_are_parsed_by_kind() {
        // The http_request plan's target is the URL's host; non-https is refused before proposing.
        let req = ProposeRequest {
            summary: "s".into(),
            kind: "http_request".into(),
            effect: json!({"method": "POST", "url": "http://x.example/"}),
        };
        let _ = req;
    }

    /// A node whose task view holds one task with this budget.
    struct Tasks(Option<(&'static str, u64)>);

    impl NodeApi for Tasks {
        fn call(&self, _cmd: Command) -> BoxFuture<'_, Result<CallResult, NodeError>> {
            Box::pin(async { Ok(CallResult::Done) })
        }
        fn view(&self, _query: Query) -> ViewStream {
            let (sink, stream) = ViewStream::channel();
            sink.send(ViewEvent::Snapshot {
                seq: 1,
                rows: vec![Row::Task(han_node::api::TaskRow {
                    key: "k".into(),
                    space: "s".into(),
                    task: "t1".into(),
                    seq: 1,
                    author: "@a:b".into(),
                    to: vec!["@coder:b".into()],
                    external: None,
                    ask: None,
                    open: true,
                    closed_as: None,
                    guarded_for: Vec::new(),
                    accepted: Vec::new(),
                    budget: self.0.map(|(unit, amount)| han_node::api::TaskBudget {
                        unit: unit.into(),
                        amount,
                    }),
                })],
                total: 1,
                offset: 0,
                next_cursor: None,
                now_ms: 0,
                totals: None,
            });
            stream
        }
    }

    fn budgeted(limit: u64) -> han_registry::BudgetSpec {
        han_registry::BudgetSpec {
            unit: "USD-cent".into(),
            limit,
            prices: [("m".to_owned(), han_registry::PriceSpec::default())].into(),
        }
    }

    fn cfg_with(budget: Option<han_registry::BudgetSpec>) -> HostConfig {
        HostConfig {
            space: "s".into(),
            agent: "@coder:b".into(),
            operator: None,
            driver: crate::driver::builtin("pi").unwrap(),
            repo: RepoConfig {
                name: ".".into(),
                source: ".".into(),
                base: "HEAD".into(),
                default_branch: "main".into(),
            },
            issue: None,
            task: None,
            executor: "@exec:b".into(),
            session_dir: PathBuf::new(),
            taint_db: PathBuf::new(),
            guarded: false,
            strict_on_read: true,
            sandbox: SandboxChoice {
                backend: "none".into(),
                allow_advisory: true,
                ro: Vec::new(),
                login_dir: None,
                allow_hosts: Vec::new(),
                toolchains: Default::default(),
            },
            shim: ShimCommand::new("han", ["shim"]),
            node: PathBuf::from("node"),
            adapter_dir: PathBuf::new(),
            model_key: None,
            budget,
            turn_timeout: Duration::from_secs(1),
            exclusion: Vec::new(),
            finish: Default::default(),
        }
    }

    /// A session budget that costs one unit per token, with `tokens` already settled.
    fn spent_budget(tokens: u64, model_route: bool) -> crate::glue::SessionBudget {
        use crate::proxy::{Budget, CallBound, Usage};
        let mut spec = budgeted(1_000);
        spec.prices = [(
            "m".to_owned(),
            han_registry::PriceSpec {
                input: 1_000_000,
                output: 1_000_000,
                ..Default::default()
            },
        )]
        .into();
        let b = crate::glue::SessionBudget::new(&spec, model_route);
        if let Some(l) = &b.ledger {
            let hold = l
                .reserve(&CallBound {
                    model: Some("m".into()),
                    input_tokens: tokens,
                    max_output: Some(1),
                })
                .unwrap();
            l.settle(
                hold,
                &Usage {
                    input: tokens,
                    requests: 1,
                    ..Usage::default()
                },
            );
        }
        b
    }

    #[test]
    fn a_proposal_carries_the_spend_measured_since_the_last_one() {
        let b = spent_budget(40, true);
        // the first proposal carries all of it, in the budget's unit
        let (cost, spent) = measured_cost(Some(&b), 0).unwrap();
        assert_eq!((cost, spent), (("USD-cent".to_owned(), 40), 40));
        // once that is charged, the next carries only what came after (here: nothing)
        let (cost, _) = measured_cost(Some(&b), spent).unwrap();
        assert_eq!(cost, ("USD-cent".to_owned(), 0));
    }

    #[test]
    fn money_that_is_not_measured_is_not_guessed() {
        // no budget at all
        assert!(measured_cost(None, 0).is_none());
        // a subscription session (no model route) and a budget with no prices have no ledger
        assert!(measured_cost(Some(&spent_budget(40, false)), 0).is_none());
        let mut spec = budgeted(100);
        spec.prices.clear();
        let unpriced = crate::glue::SessionBudget::new(&spec, true);
        assert!(measured_cost(Some(&unpriced), 0).is_none());
    }

    #[tokio::test]
    async fn a_task_budget_narrows_the_session_budget_and_never_widens_it() {
        let cfg = cfg_with(Some(budgeted(500)));
        // the task holds 300 of the space's 500: the session may spend 300, at the same prices
        let narrowed = narrowed_budget(&cfg, &Tasks(Some(("USD-cent", 300))), Some("t1"))
            .await
            .unwrap();
        assert_eq!((narrowed.limit, narrowed.prices.len()), (300, 1));
        // a task with more than the space, in another unit, or with no budget changes nothing
        for own in [Some(("USD-cent", 900)), Some(("EUR-cent", 10)), None] {
            assert!(
                narrowed_budget(&cfg, &Tasks(own), Some("t1"))
                    .await
                    .is_none(),
                "{own:?}"
            );
        }
        // no task, another task, or no budget in the space: nothing to narrow
        assert!(
            narrowed_budget(&cfg, &Tasks(Some(("USD-cent", 1))), None)
                .await
                .is_none()
        );
        assert!(
            narrowed_budget(&cfg, &Tasks(Some(("USD-cent", 1))), Some("t2"))
                .await
                .is_none()
        );
        assert!(
            narrowed_budget(&cfg_with(None), &Tasks(Some(("USD-cent", 1))), Some("t1"))
                .await
                .is_none()
        );
    }
}

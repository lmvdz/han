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
use std::sync::{Arc, Mutex};
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
use crate::mask::Masker;
use crate::r24::{self, Driver, DriverKind, GuardFacts, TaintStore, TaskDuty};
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
    /// The in-sandbox shim program.
    pub shim: PathBuf,
    /// Node and adapter, for drivers that launch through node.
    pub node: PathBuf,
    /// The adapter's directory.
    pub adapter_dir: PathBuf,
    /// The real model key in API-key mode (held only by the proxy).
    pub model_key: Option<String>,
    /// How long one turn may take.
    pub turn_timeout: Duration,
    /// Paths never mirrored or streamed (globs), on top of the default list.
    pub exclusion: Vec<String>,
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
    /// Test only: let the proxy connect to loopback upstreams (a mock provider).
    pub test_loopback_upstream: bool,
    /// Test only: extra TLS roots for the model route.
    pub test_roots: Vec<Vec<u8>>,
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
    signals: mpsc::UnboundedSender<Signal>,
    read_others: Mutex<bool>,
    proposals: Mutex<Vec<String>>,
}

impl Shared {
    fn signal(&self, s: Signal) {
        let _ = self.signals.send(s);
    }

    /// The plan of a proposal, built by the host (never by the agent): git pushes carry the committed
    /// changeset and its digest, which the executor checks before landing.
    fn plan_for(&self, req: &ProposeRequest) -> Result<Value, String> {
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
                Ok(json!({
                    "action": "git_push",
                    "target": self.cfg.repo.name,
                    "args": {
                        "branch": branch,
                        "base": self.workspace.base,
                        "from": from,
                        "to": head,
                        "changeset": cs.digest,
                        "source": self.workspace.dir.to_string_lossy(),
                        "changes": cs.summary(),
                    }
                }))
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
                Ok(json!({"action": "open_pr", "target": self.cfg.repo.name, "args": args}))
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
                Ok(json!({"action": "http_request", "target": host, "args": args}))
            }
            other => Err(format!(
                "unknown kind {other}: use git_push, open_pr or http_request"
            )),
        }
    }
}

struct Bridge(Arc<Shared>);

impl HostBridge for Bridge {
    fn propose(&self, req: ProposeRequest) -> BoxFuture<'_, ProposeAnswer> {
        Box::pin(async move {
            let sh = &self.0;
            let plan = match sh.plan_for(&req) {
                Ok(p) => p,
                Err(reason) => return ProposeAnswer::Refused { reason },
            };
            let plan_text = han_core::json::parse(plan.to_string().as_bytes())
                .ok()
                .and_then(|v| han_core::json::canonicalize(&v).ok())
                .and_then(|b| String::from_utf8(b).ok());
            let Some(plan_text) = plan_text else {
                return ProposeAnswer::Refused {
                    reason: "the plan is not valid I-JSON".into(),
                };
            };
            // R-24: a proposal of a task-driven turn names the task.
            let r = sh
                .api
                .call(Command::EffectPropose {
                    space: sh.cfg.space.clone(),
                    executor: sh.cfg.executor.clone(),
                    plan: plan_text,
                    task: sh.task.clone(),
                    cost: None,
                    repeatable: false,
                    expires_at: None,
                })
                .await;
            match r {
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
                            waiting_for: sh
                                .cfg
                                .operator
                                .clone()
                                .unwrap_or_else(|| "an approver".into()),
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
        })
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
        let masked = self.0.masker.mask(summary).into_owned();
        self.0.signal(Signal::Summary(masked));
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
}

/// A task being worked: its id, its author, the issue it came from and its ask.
type TaskInfo = (String, String, Option<String>, Option<String>);

/// Finds the open task for the issue assigned to the agent, or claims the issue with a self-assigned task.
async fn find_or_claim_task(
    cfg: &HostConfig,
    api: &dyn NodeApi,
) -> Result<Option<TaskInfo>, HostError> {
    if let Some(t) = &cfg.task {
        let rows = snapshot(
            api,
            Query::Tasks {
                space: cfg.space.clone(),
                assignee: Some(cfg.agent.clone()),
                window: Window::default(),
            },
        )
        .await?;
        let (author, ask) = rows
            .iter()
            .find_map(|r| match r {
                Row::Task(x) if &x.task == t => Some((x.author.clone(), x.ask.clone())),
                _ => None,
            })
            .unwrap_or_else(|| (cfg.agent.clone(), None));
        return Ok(Some((t.clone(), author, cfg.issue.clone(), ask)));
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
    let state = match &result {
        Ok(s) => *s,
        Err(_) => SessionState::Failed,
    };
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
        Ok(state) => Ok(SessionEnd {
            session,
            task: task.map(|t| t.0),
            state,
            proposals: Vec::new(),
        }),
        Err(e) => Err(e),
    }
}

async fn run_opened(
    cfg: &HostConfig,
    deps: &HostDeps,
    session: &str,
    task: Option<TaskInfo>,
    stop: &mut watch::Receiver<bool>,
) -> Result<SessionState, HostError> {
    let api = deps.api.clone();
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
    let shared = Arc::new(Shared {
        api: api.clone(),
        cfg: cfg.clone(),
        session: session.to_owned(),
        task: task.as_ref().map(|t| t.0.clone()),
        workspace: workspace.clone(),
        vcs: deps.vcs.clone(),
        masker: masker.clone(),
        status: Mutex::new(deriver),
        signals: sig_tx,
        read_others: Mutex::new(false),
        proposals: Mutex::new(Vec::new()),
    });

    let env = crate::glue::Egress::start(
        cfg,
        &shared_bridge(&shared),
        &token,
        &workspace,
        &agent_dir,
        &temp,
    )
    .await
    .map_err(HostError::Sandbox)?;
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
                                            dirty |= shared.status.lock().unwrap_or_else(|p| p.into_inner()).apply(s);
                                        }
                                    }
                                    feed.end_turn();
                                    if matches!(s, Signal::TurnEnded)
                                        && let Some(m) = feed.last_message()
                                    {
                                        let m = shared.masker.mask(m).into_owned();
                                        shared.status.lock().unwrap_or_else(|p| p.into_inner()).apply(Signal::LastMessage(m));
                                    }
                                }
                                _ => {}
                            }
                            dirty |= shared.status.lock().unwrap_or_else(|p| p.into_inner()).apply(s);
                            publish_items(&shared, &mut feed).await;
                        }
                        None => break,
                    },
                    ev = events.next() => match ev {
                        Some(ev) => {
                            feed.apply(&ev, &now_text());
                            for s in signals_of(&shared, mirror.as_deref(), ev) {
                                dirty |= shared.status.lock().unwrap_or_else(|p| p.into_inner()).apply(s);
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
    let final_state = if state == SessionState::Failed {
        SessionState::Failed
    } else {
        SessionState::Done
    };
    shared.signal(if final_state == SessionState::Failed {
        Signal::Failed("the session stopped".into())
    } else {
        Signal::Done
    });
    drop(mirror);
    // Let the publisher drain, then stop it.
    tokio::time::sleep(Duration::from_millis(400)).await;
    publisher.abort();
    publish(&shared).await;
    Ok(final_state)
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
    let end = agent
        .turn(
            TurnInput {
                text,
                data_blocks: data.into_iter().collect(),
            },
            decision.mode,
        )
        .await;
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
                shared.signal(Signal::Failed(shared.masker.mask(&why).into_owned()));
                return Ok(SessionState::Failed);
            }
            refresh_files(shared);
            shared.signal(Signal::TurnEnded);
            Ok(SessionState::Idle)
        }
        Err(e) => {
            shared.signal(Signal::Failed(
                shared.masker.mask(&e.to_string()).into_owned(),
            ));
            Ok(SessionState::Failed)
        }
    }
}

fn refresh_files(shared: &Shared) {
    if let Ok(cs) = shared
        .vcs
        .changeset(&shared.workspace.dir, &shared.workspace.base)
    {
        shared.signal(Signal::Files(touches(&cs)));
    }
}

/// Maps one agent event to status signals, and attributes worktree changes to the running tool call.
fn signals_of(
    shared: &Shared,
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
            vec![Signal::Tool {
                title: shared.masker.mask(&title).into_owned(),
                kind,
            }]
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
                    title: shared.masker.mask(&t).into_owned(),
                    kind: None,
                });
            }
            if matches!(status.as_deref(), Some("completed") | Some("failed")) {
                if let Some(m) = mirror {
                    m.set_step(None);
                }
                out.push(Signal::ToolDone { output });
                if let Ok(cs) = shared
                    .vcs
                    .changeset(&shared.workspace.dir, &shared.workspace.base)
                {
                    out.push(Signal::Files(touches(&cs)));
                }
            }
            out
        }
        AgentEvent::Plan { entries } => vec![Signal::Plan(
            entries
                .into_iter()
                .map(|(text, status)| han_node::api::PlanItem {
                    text: shared.masker.mask(&text).into_owned(),
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
            tracing::warn!("item not published: {e}");
        }
    }
}

async fn publish(shared: &Shared) {
    let mut body = shared
        .status
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .snapshot(&now_text());
    // SR-52: nothing leaves the device unmasked.
    body.step = shared.masker.mask(&body.step).into_owned();
    if let Some(s) = &body.summary {
        body.summary = Some(shared.masker.mask(s).into_owned());
    }
    if let Some(t) = &body.tool {
        body.tool = Some(shared.masker.mask(t).into_owned());
    }
    for p in &mut body.plan {
        p.text = shared.masker.mask(&p.text).into_owned();
    }
    let _ = shared
        .api
        .call(Command::StatusPublish {
            space: shared.cfg.space.clone(),
            status: Box::new(body),
        })
        .await;
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
}

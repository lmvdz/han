//! The one ACP client (PLAN §5 AgentDriver; `agent-client-protocol` =3.3.0).
//!
//! [`AcpDriver`] speaks ACP to any agent over its stdio. A harness is an ACP agent command plus its sandbox
//! profile ([`crate::driver`]); there is no other implementation of [`AgentDriver`].
//!
//! * **Events.** Everything the agent reports becomes an [`AgentEvent`] on one channel. [`AgentDriver::events`]
//!   hands that channel out **once**; a second call returns an empty stream. Within one connection the events
//!   are in the order the agent sent them, and a turn's [`AgentEvent::TurnEnded`] follows every update of that
//!   turn (ACP's dispatch loop is sequential).
//! * **Permission requests are UX** (PLAN §5). The host answers them by policy ([`PermissionPolicy`]) with the
//!   turn's [`Mode`]. ACP rejects carry no reason (M4 spike Q4), so the driver queues each reject's reason and
//!   prepends it to the next prompt as a fixed sentence ([`reject_notice`]).
//! * **Failed turns.** pi-acp hides provider errors: the turn ends `end_turn` with nothing in it. [`TurnEnd`]
//!   flags that as `suspect_failure`, and an optional failure probe (for example
//!   [`crate::driver::pi_session_error`]) supplies the provider's error text.
//! * **Strict turns.** Before a [`Mode::Strict`] turn the driver switches the session to the agent's
//!   ask-before-acting mode when the agent offers one (claude-agent-acp: `default`), and back afterwards.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    CancelNotification, InitializeRequest, NewSessionRequest, PromptRequest, PromptResponse,
    RequestPermissionRequest, RequestPermissionResponse, SessionNotification,
    SetSessionModeRequest,
};
use agent_client_protocol::{Agent, ByteStreams, Client, ConnectionTo};
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// The most text one tool-call update carries in [`AgentEvent::ToolCallUpdate::output`].
pub const MAX_TOOL_OUTPUT: usize = 4096;
/// The longest stderr line kept in [`AgentEvent::Stderr`].
pub const MAX_STDERR_LINE: usize = 1024;
/// At most this many stderr lines are forwarded per session; the rest are counted and dropped.
pub const MAX_STDERR_LINES: usize = 2000;

/// How a turn runs (R-24).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// The session's own mode (for example a harness's auto mode).
    Auto,
    /// The stricter mode: no allow rules, every world-reaching call becomes a proposal.
    Strict,
}

/// Whether the harness's permission prompts can see every tool call (SR-34).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateFidelity {
    /// Every tool call can be gated (an ask-before-acting mode exists and is used in strict turns).
    EveryCall,
    /// Only the prompt is controlled; the harness never asks (pi). The sandbox is the only gate.
    PromptOnly,
}

/// An MCP server to give the agent through ACP `session/new.mcpServers`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpServerSpec {
    /// A server the agent starts as a child process.
    Stdio {
        /// The server's name (tools appear as `mcp__<name>__<tool>`).
        name: String,
        /// The program.
        command: PathBuf,
        /// Its arguments.
        #[serde(default)]
        args: Vec<String>,
        /// Its environment.
        #[serde(default)]
        env: Vec<(String, String)>,
    },
    /// A streamable-HTTP server (only for agents whose `mcpCapabilities.http` is true).
    Http {
        /// The server's name.
        name: String,
        /// The URL.
        url: String,
        /// Headers to send.
        #[serde(default)]
        headers: Vec<(String, String)>,
    },
}

impl McpServerSpec {
    /// The ACP JSON form.
    pub fn to_acp(&self) -> Value {
        let pairs = |v: &[(String, String)]| -> Vec<Value> {
            v.iter()
                .map(|(k, x)| json!({"name": k, "value": x}))
                .collect()
        };
        match self {
            McpServerSpec::Stdio {
                name,
                command,
                args,
                env,
            } => json!({"name": name, "command": command, "args": args, "env": pairs(env)}),
            McpServerSpec::Http { name, url, headers } => {
                json!({"type": "http", "name": name, "url": url, "headers": pairs(headers)})
            }
        }
    }
}

/// How a session starts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionConfig {
    /// The session's worktree (the agent's working directory).
    pub cwd: PathBuf,
    /// HAN's MCP servers, for agents that take them over ACP.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerSpec>,
    /// The model to select through the `model` config option, if the agent offers it.
    #[serde(default)]
    pub model: Option<String>,
    /// `_meta` for `session/new` (claude-agent-acp reads `claudeCode.options` here).
    #[serde(default)]
    pub meta: Option<Value>,
    /// The session mode to use for strict turns when the agent offers it (default: `default`).
    #[serde(default)]
    pub strict_mode: Option<String>,
    /// The session mode for auto turns (default: whatever the session started in).
    #[serde(default)]
    pub auto_mode: Option<String>,
}

/// What `start` learnt about the agent and its session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionStarted {
    /// The ACP session id.
    pub session_id: String,
    /// `agentInfo.name`.
    pub agent_name: Option<String>,
    /// `agentInfo.version`.
    pub agent_version: Option<String>,
    /// Session modes the agent offers.
    pub modes: Vec<String>,
    /// The mode it started in.
    pub current_mode: Option<String>,
    /// Whether strict turns can switch to an ask-before-acting mode.
    pub gate_fidelity: GateFidelity,
    /// Whether the agent takes HTTP MCP servers.
    pub mcp_http: bool,
    /// The raw initialize response.
    pub initialize: Value,
    /// The raw `session/new` response.
    pub new_session: Value,
}

/// One turn's input. `text` is the instruction; `data_blocks` are already-wrapped `<han-data>` blocks (SR-54)
/// sent as separate text content blocks after it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnInput {
    /// The instruction.
    pub text: String,
    /// Data blocks, each its own content block.
    #[serde(default)]
    pub data_blocks: Vec<String>,
}

/// How a turn ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TurnEnd {
    /// `end_turn`, `max_tokens`, `max_turn_requests`, `refusal` or `cancelled`.
    pub stop_reason: String,
    /// Whether the agent sent any message text.
    pub produced_output: bool,
    /// How many tool calls it started.
    pub tool_calls: u32,
    /// `end_turn` with no output and no tool calls: probably a hidden provider error (pi-acp).
    pub suspect_failure: bool,
    /// The provider error a failure probe found, if any.
    pub provider_error: Option<String>,
    /// The prompt response's `_meta`.
    pub meta: Option<Value>,
}

/// Token counts from a prompt response's `_meta.quota.token_count` (claude-agent-acp).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenCounts {
    /// Uncached input tokens.
    pub input: u64,
    /// Input tokens read from the prompt cache.
    pub cached_input: u64,
    /// Input tokens written to the prompt cache.
    pub cached_write: u64,
    /// Output tokens.
    pub output: u64,
    /// The total the agent reports.
    pub total: u64,
}

impl TokenCounts {
    /// Sums the per-model counts of `_meta.quota.model_usage` when present, else reads
    /// `_meta.quota.token_count`. `None` when neither is there.
    pub fn from_meta(meta: &Value) -> Option<TokenCounts> {
        let quota = meta.get("quota")?;
        let one = |tc: &Value| TokenCounts {
            input: tc["inputTokens"].as_u64().unwrap_or(0),
            cached_input: tc["cachedInputTokens"].as_u64().unwrap_or(0),
            cached_write: tc["cachedWriteTokens"].as_u64().unwrap_or(0),
            output: tc["outputTokens"].as_u64().unwrap_or(0),
            total: tc["totalTokens"].as_u64().unwrap_or(0),
        };
        if let Some(models) = quota.get("model_usage").and_then(Value::as_array)
            && !models.is_empty()
        {
            let mut sum = TokenCounts::default();
            for m in models {
                let c = one(&m["token_count"]);
                sum.input += c.input;
                sum.cached_input += c.cached_input;
                sum.cached_write += c.cached_write;
                sum.output += c.output;
                sum.total += c.total;
            }
            return Some(sum);
        }
        quota.get("token_count").map(one)
    }
}

/// Everything the agent reports.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AgentEvent {
    /// Message text.
    Output {
        /// The chunk.
        text: String,
    },
    /// Thinking text.
    Thought {
        /// The chunk.
        text: String,
    },
    /// A tool call started.
    ToolCall {
        /// Its id.
        id: String,
        /// Its title.
        title: String,
        /// ACP kind (`read`, `edit`, `execute`, `fetch`, …).
        kind: Option<String>,
        /// Its status.
        status: Option<String>,
        /// Its input.
        raw_input: Option<Value>,
        /// Paths it names.
        locations: Vec<String>,
    },
    /// A tool call changed.
    ToolCallUpdate {
        /// Its id.
        id: String,
        /// The new status.
        status: Option<String>,
        /// A new title.
        title: Option<String>,
        /// Text output (content text, terminal output), at most [`MAX_TOOL_OUTPUT`] bytes.
        output: Option<String>,
        /// Raw output.
        raw_output: Option<Value>,
        /// Paths it names.
        locations: Vec<String>,
    },
    /// The agent's plan.
    Plan {
        /// `(content, status)` per entry.
        entries: Vec<(String, String)>,
    },
    /// Context occupancy (`usage_update`).
    Usage {
        /// Tokens in the context.
        used: u64,
        /// The context's size.
        size: u64,
        /// A running cost estimate `(amount, currency)`, when given.
        cost: Option<(f64, String)>,
        /// The update's `_meta` (claude-agent-acp: `_claude/rateLimit`, `_claude/model`).
        meta: Option<Value>,
    },
    /// A turn ended.
    TurnEnded {
        /// The stop reason.
        stop_reason: String,
        /// The prompt response's `_meta`.
        meta: Option<Value>,
    },
    /// The host answered a permission request.
    PermissionDecided {
        /// The tool call.
        tool_call_id: String,
        /// Its title.
        title: String,
        /// Whether it was allowed.
        allowed: bool,
        /// Why not, if rejected.
        reason: Option<String>,
    },
    /// A line the agent wrote to stderr (bounded).
    Stderr {
        /// The line.
        line: String,
    },
    /// The agent process exited.
    Exited {
        /// Its exit code, when known.
        code: Option<i32>,
    },
    /// A protocol or transport error.
    Error {
        /// What went wrong.
        message: String,
    },
}

/// A permission request, as the policy sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PermissionAsk {
    /// The tool call.
    pub tool_call_id: String,
    /// Its title.
    pub title: String,
    /// Its ACP kind.
    pub kind: Option<String>,
    /// Its input.
    pub raw_input: Option<Value>,
    /// Paths it names.
    pub locations: Vec<String>,
    /// The options: `(option id, kind)`.
    pub options: Vec<(String, String)>,
}

/// The policy's answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PermissionDecision {
    /// Let it run.
    Allow,
    /// Do not; the reason reaches the agent with its next prompt.
    Reject {
        /// Why.
        reason: String,
    },
}

/// Answers permission requests by policy (PLAN §5: "allowed inside the sandbox, otherwise use propose").
pub trait PermissionPolicy: Send + Sync + 'static {
    /// The decision for one request in a turn of `mode`.
    fn decide(&self, ask: &PermissionAsk, mode: Mode) -> PermissionDecision;
}

/// The default policy. In an auto turn everything is allowed: the sandbox is the gate. In a strict turn
/// (R-24, §8.4) only reads, searches, thinking and edits whose paths are all inside the worktree are allowed;
/// everything else is rejected and pointed at `propose`.
#[derive(Clone, Debug)]
pub struct SandboxPolicy {
    /// The session's worktree.
    pub worktree: PathBuf,
}

/// The reason a strict turn rejects with.
pub const STRICT_REJECT: &str = "this turn runs in HAN's stricter mode: actions that reach outside the session's worktree are not run";

impl PermissionPolicy for SandboxPolicy {
    fn decide(&self, ask: &PermissionAsk, mode: Mode) -> PermissionDecision {
        if mode == Mode::Auto {
            return PermissionDecision::Allow;
        }
        let local_kind = matches!(
            ask.kind.as_deref(),
            Some("read" | "edit" | "search" | "think" | "delete" | "move")
        );
        let inside = |p: &String| {
            let p = std::path::Path::new(p);
            let abs = if p.is_absolute() {
                p.to_path_buf()
            } else {
                self.worktree.join(p)
            };
            !abs.components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
                && abs.starts_with(&self.worktree)
        };
        if local_kind && ask.locations.iter().all(inside) {
            PermissionDecision::Allow
        } else {
            PermissionDecision::Reject {
                reason: STRICT_REJECT.into(),
            }
        }
    }
}

/// Allows everything (tests).
pub struct AllowAll;

impl PermissionPolicy for AllowAll {
    fn decide(&self, _: &PermissionAsk, _: Mode) -> PermissionDecision {
        PermissionDecision::Allow
    }
}

/// The fixed sentence a rejected call's reason travels in, prepended to the next prompt.
pub fn reject_notice(title: &str, reason: &str) -> String {
    format!(
        "HAN: the tool call '{title}' was not run: {reason}. Use the propose tool for actions outside the sandbox."
    )
}

/// Why the driver failed.
#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    /// The agent answered with an error, or sent something that is not ACP.
    #[error("protocol: {0}")]
    Protocol(String),
    /// The connection is gone (the agent exited or closed stdout).
    #[error("the agent connection is closed")]
    Closed,
    /// `turn` before `start`.
    #[error("no session: start first")]
    NotStarted,
    /// The turn took longer than the configured timeout; it was cancelled.
    #[error("the turn timed out")]
    Timeout,
    /// Spawning the agent failed.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// The driver interface (PLAN §5).
#[async_trait::async_trait]
pub trait AgentDriver: Send {
    /// Initializes the agent and opens a session.
    async fn start(&mut self, cfg: SessionConfig) -> Result<SessionStarted, AcpError>;
    /// Runs one turn.
    async fn turn(&mut self, input: TurnInput, mode: Mode) -> Result<TurnEnd, AcpError>;
    /// Cancels the running turn, if any.
    async fn cancel(&mut self);
    /// The event stream (handed out once; later calls get an empty stream).
    fn events(&mut self) -> BoxStream<'static, AgentEvent>;
}

/// A probe that looks for a provider error after a suspect turn (for example pi's session JSONL).
pub type FailureProbe = Arc<dyn Fn() -> Option<String> + Send + Sync>;

#[derive(Default)]
struct TurnStats {
    output: bool,
    tool_calls: u32,
}

struct Shared {
    mode: Mutex<Mode>,
    rejected: Mutex<Vec<String>>,
    stats: Mutex<TurnStats>,
    policy: Arc<dyn PermissionPolicy>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A handle that cancels the running turn from another task.
#[derive(Clone)]
pub struct CancelHandle {
    conn: ConnectionTo<Agent>,
    session: Arc<Mutex<Option<String>>>,
}

impl CancelHandle {
    /// Sends `session/cancel`.
    pub fn cancel(&self) {
        if let Some(id) = lock(&self.session).clone()
            && let Ok(n) = serde_json::from_value::<CancelNotification>(json!({"sessionId": id}))
        {
            let _ = self.conn.send_notification(n);
        }
    }
}

/// The ACP client.
pub struct AcpDriver {
    conn: ConnectionTo<Agent>,
    shared: Arc<Shared>,
    session: Arc<Mutex<Option<String>>>,
    events: Option<mpsc::UnboundedReceiver<AgentEvent>>,
    tx: mpsc::UnboundedSender<AgentEvent>,
    shutdown: Option<oneshot::Sender<()>>,
    child: Option<tokio::process::Child>,
    modes: Vec<String>,
    current_mode: Option<String>,
    strict_mode: Option<String>,
    auto_mode: Option<String>,
    turn_timeout: Option<Duration>,
    probe: Option<FailureProbe>,
    closed: Arc<std::sync::atomic::AtomicBool>,
}

fn typed<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, AcpError> {
    serde_json::from_value(v).map_err(|e| AcpError::Protocol(e.to_string()))
}

fn truncate(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut cut = max;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push('…');
    }
    s
}

fn locations(v: &Value) -> Vec<String> {
    v["locations"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|l| l["path"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn opt_str(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_owned)
}

/// Text from a tool call update: content text blocks, then terminal output in `_meta`, then a string
/// `rawOutput`.
fn tool_output(v: &Value) -> Option<String> {
    let mut out = String::new();
    if let Some(content) = v["content"].as_array() {
        for c in content {
            if let Some(t) = c["content"]["text"].as_str() {
                out.push_str(t);
            }
        }
    }
    if let Some(t) = v["_meta"]["terminal_output"]["data"].as_str() {
        out.push_str(t);
    }
    if out.is_empty()
        && let Some(t) = v["rawOutput"].as_str()
    {
        out.push_str(t);
    }
    (!out.is_empty()).then(|| truncate(out, MAX_TOOL_OUTPUT))
}

/// Maps one `session/update` to events (several updates carry nothing HAN uses: they are dropped).
pub fn map_update(update: &Value) -> Vec<AgentEvent> {
    let kind = update["sessionUpdate"].as_str().unwrap_or("");
    match kind {
        "agent_message_chunk" => update["content"]["text"]
            .as_str()
            .map(|t| AgentEvent::Output { text: t.into() })
            .into_iter()
            .collect(),
        "agent_thought_chunk" => update["content"]["text"]
            .as_str()
            .map(|t| AgentEvent::Thought { text: t.into() })
            .into_iter()
            .collect(),
        "tool_call" => vec![AgentEvent::ToolCall {
            id: update["toolCallId"].as_str().unwrap_or("").into(),
            title: update["title"].as_str().unwrap_or("").into(),
            kind: opt_str(update, "kind"),
            status: opt_str(update, "status"),
            raw_input: update.get("rawInput").cloned(),
            locations: locations(update),
        }],
        "tool_call_update" => vec![AgentEvent::ToolCallUpdate {
            id: update["toolCallId"].as_str().unwrap_or("").into(),
            status: opt_str(update, "status"),
            title: opt_str(update, "title"),
            output: tool_output(update),
            raw_output: update.get("rawOutput").cloned(),
            locations: locations(update),
        }],
        "plan" => vec![AgentEvent::Plan {
            entries: update["entries"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|e| {
                            (
                                e["content"].as_str().unwrap_or("").to_owned(),
                                e["status"].as_str().unwrap_or("").to_owned(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }],
        "usage_update" => vec![AgentEvent::Usage {
            used: update["used"].as_u64().unwrap_or(0),
            size: update["size"].as_u64().unwrap_or(0),
            cost: update["cost"]["amount"].as_f64().map(|a| {
                (
                    a,
                    update["cost"]["currency"]
                        .as_str()
                        .unwrap_or("USD")
                        .to_owned(),
                )
            }),
            meta: update.get("_meta").cloned(),
        }],
        _ => Vec::new(),
    }
}

impl AcpDriver {
    /// Connects to an agent whose stdin and stdout are given. Runs the ACP connection on a tokio task.
    pub async fn connect<W, R>(
        stdin: W,
        stdout: R,
        policy: Arc<dyn PermissionPolicy>,
    ) -> Result<AcpDriver, AcpError>
    where
        W: AsyncWrite + Send + Unpin + 'static,
        R: AsyncRead + Send + Unpin + 'static,
    {
        let (tx, rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            mode: Mutex::new(Mode::Auto),
            rejected: Mutex::new(Vec::new()),
            stats: Mutex::new(TurnStats::default()),
            policy,
        });
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (conn_tx, conn_rx) = oneshot::channel::<ConnectionTo<Agent>>();
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());
        let (ntx, ptx) = (tx.clone(), tx.clone());
        let (nsh, psh) = (Arc::clone(&shared), Arc::clone(&shared));
        let etx = tx.clone();
        let eclosed = Arc::clone(&closed);
        tokio::spawn(async move {
            let result = Client
                .builder()
                .name("han-host")
                .on_receive_notification(
                    async move |n: SessionNotification, _cx| {
                        let v = serde_json::to_value(&n.update).unwrap_or_default();
                        for ev in map_update(&v) {
                            {
                                let mut st = lock(&nsh.stats);
                                match &ev {
                                    AgentEvent::Output { text } if !text.is_empty() => {
                                        st.output = true;
                                    }
                                    AgentEvent::ToolCall { .. } => st.tool_calls += 1,
                                    _ => {}
                                }
                            }
                            let _ = ntx.send(ev);
                        }
                        Ok(())
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                .on_receive_request(
                    async move |r: RequestPermissionRequest, responder, _cx| {
                        let v = serde_json::to_value(&r).unwrap_or_default();
                        let tc = &v["toolCall"];
                        let options: Vec<(String, String)> = v["options"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .map(|o| {
                                        (
                                            o["optionId"].as_str().unwrap_or("").to_owned(),
                                            o["kind"].as_str().unwrap_or("").to_owned(),
                                        )
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        let ask = PermissionAsk {
                            tool_call_id: tc["toolCallId"].as_str().unwrap_or("").into(),
                            title: tc["title"].as_str().unwrap_or("").into(),
                            kind: opt_str(tc, "kind"),
                            raw_input: tc.get("rawInput").cloned(),
                            locations: locations(tc),
                            options: options.clone(),
                        };
                        let mode = *lock(&psh.mode);
                        let decision = psh.policy.decide(&ask, mode);
                        let (want, reason): (&[&str], Option<String>) = match &decision {
                            PermissionDecision::Allow => (&["allow_once", "allow_always"], None),
                            PermissionDecision::Reject { reason } => {
                                (&["reject_once", "reject_always"], Some(reason.clone()))
                            }
                        };
                        let chosen = want
                            .iter()
                            .find_map(|k| options.iter().find(|(_, kind)| kind == k))
                            .map(|(id, _)| id.clone());
                        if let Some(r) = &reason {
                            lock(&psh.rejected).push(reject_notice(&ask.title, r));
                        }
                        let _ = ptx.send(AgentEvent::PermissionDecided {
                            tool_call_id: ask.tool_call_id.clone(),
                            title: ask.title.clone(),
                            allowed: reason.is_none(),
                            reason: reason.clone(),
                        });
                        let outcome = match chosen {
                            Some(id) => json!({"outcome": "selected", "optionId": id}),
                            None => json!({"outcome": "cancelled"}),
                        };
                        let mut resp = json!({ "outcome": outcome });
                        if let Some(r) = reason {
                            resp["_meta"] = json!({"han": {"reason": r}});
                        }
                        match serde_json::from_value::<RequestPermissionResponse>(resp) {
                            Ok(resp) => responder.respond(resp),
                            Err(e) => {
                                Err(agent_client_protocol::Error::internal_error()
                                    .data(e.to_string()))
                            }
                        }
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_with(transport, async move |conn: ConnectionTo<Agent>| {
                    let _ = conn_tx.send(conn);
                    let _ = stop_rx.await;
                    Ok(())
                })
                .await;
            eclosed.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Err(e) = result {
                let _ = etx.send(AgentEvent::Error {
                    message: e.to_string(),
                });
            }
        });
        let conn = conn_rx.await.map_err(|_| AcpError::Closed)?;
        Ok(AcpDriver {
            conn,
            shared,
            session: Arc::new(Mutex::new(None)),
            events: Some(rx),
            tx,
            shutdown: Some(stop_tx),
            child: None,
            modes: Vec::new(),
            current_mode: None,
            strict_mode: None,
            auto_mode: None,
            turn_timeout: None,
            probe: None,
            closed,
        })
    }

    /// Spawns `cmd` with piped stdio and connects to it. The driver owns the child: its stderr becomes
    /// [`AgentEvent::Stderr`] lines, its exit an [`AgentEvent::Exited`] with the real code, and dropping the
    /// driver kills it.
    pub async fn spawn(
        mut cmd: tokio::process::Command,
        policy: Arc<dyn PermissionPolicy>,
    ) -> Result<AcpDriver, AcpError> {
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().ok_or(AcpError::Closed)?;
        let stdout = child.stdout.take().ok_or(AcpError::Closed)?;
        let stderr = child.stderr.take();
        let mut driver = AcpDriver::connect(stdin, stdout, policy).await?;
        if let Some(stderr) = stderr {
            let tx = driver.tx.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                let mut n = 0usize;
                while let Ok(Some(line)) = lines.next_line().await {
                    n += 1;
                    if n <= MAX_STDERR_LINES {
                        let _ = tx.send(AgentEvent::Stderr {
                            line: truncate(line, MAX_STDERR_LINE),
                        });
                    }
                }
            });
        }
        // Watch for the exit without holding the child: poll its status.
        driver.child = Some(child);
        Ok(driver)
    }

    /// Cancels a turn that runs longer than `t` (and fails it with [`AcpError::Timeout`]).
    pub fn with_turn_timeout(mut self, t: Duration) -> Self {
        self.turn_timeout = Some(t);
        self
    }

    /// Runs `probe` after a suspect turn; its text becomes [`TurnEnd::provider_error`].
    pub fn with_failure_probe(mut self, probe: FailureProbe) -> Self {
        self.probe = Some(probe);
        self
    }

    /// A handle that cancels the running turn from another task.
    pub fn cancel_handle(&self) -> CancelHandle {
        CancelHandle {
            conn: self.conn.clone(),
            session: Arc::clone(&self.session),
        }
    }

    /// Kills the agent process (if this driver spawned it) and closes the connection.
    pub async fn shutdown(&mut self) {
        if let Some(stop) = self.shutdown.take() {
            let _ = stop.send(());
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            let code = child.wait().await.ok().and_then(|s| s.code());
            let _ = self.tx.send(AgentEvent::Exited { code });
        }
    }

    /// Whether the connection has closed.
    pub fn is_closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::SeqCst)
    }

    async fn reap_if_exited(&mut self) {
        if let Some(child) = self.child.as_mut()
            && let Ok(Some(status)) = child.try_wait()
        {
            let _ = self.tx.send(AgentEvent::Exited {
                code: status.code(),
            });
            self.child = None;
        }
    }

    async fn set_mode(&mut self, want: Option<String>) -> Result<(), AcpError> {
        let Some(want) = want else { return Ok(()) };
        if self.current_mode.as_deref() == Some(want.as_str()) || !self.modes.contains(&want) {
            return Ok(());
        }
        let session = lock(&self.session).clone().ok_or(AcpError::NotStarted)?;
        let req: SetSessionModeRequest =
            typed(json!({"sessionId": session, "modeId": want.clone()}))?;
        self.conn
            .send_request(req)
            .block_task()
            .await
            .map_err(|e| AcpError::Protocol(e.to_string()))?;
        self.current_mode = Some(want);
        Ok(())
    }

    async fn wait_for_exit(&mut self) -> Option<i32> {
        let child = self.child.as_mut()?;
        let code = tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .ok()
            .and_then(Result::ok)
            .and_then(|s| s.code());
        self.child = None;
        let _ = self.tx.send(AgentEvent::Exited { code });
        code
    }
}

#[async_trait::async_trait]
impl AgentDriver for AcpDriver {
    async fn start(&mut self, cfg: SessionConfig) -> Result<SessionStarted, AcpError> {
        let init: InitializeRequest = typed(json!({
            "protocolVersion": serde_json::to_value(ProtocolVersion::V1).unwrap_or(json!(1)),
            "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
            "clientInfo": {"name": "han-host", "version": env!("CARGO_PKG_VERSION")}
        }))?;
        let init = self
            .conn
            .send_request(init)
            .block_task()
            .await
            .map_err(|e| AcpError::Protocol(e.to_string()))?;
        let init = serde_json::to_value(&init).unwrap_or_default();
        let mut new = json!({
            "cwd": cfg.cwd,
            "mcpServers": cfg.mcp_servers.iter().map(McpServerSpec::to_acp).collect::<Vec<_>>(),
        });
        if let Some(m) = &cfg.meta {
            new["_meta"] = m.clone();
        }
        let new: NewSessionRequest = typed(new)?;
        let ns = self
            .conn
            .send_request(new)
            .block_task()
            .await
            .map_err(|e| AcpError::Protocol(e.to_string()))?;
        let ns = serde_json::to_value(&ns).unwrap_or_default();
        let session_id = ns["sessionId"].as_str().unwrap_or("").to_owned();
        *lock(&self.session) = Some(session_id.clone());
        self.modes = ns["modes"]["availableModes"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| m["id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        self.current_mode = opt_str(&ns["modes"], "currentModeId");
        let strict = cfg.strict_mode.clone().unwrap_or_else(|| "default".into());
        self.strict_mode = self.modes.contains(&strict).then_some(strict);
        self.auto_mode = cfg.auto_mode.clone().or_else(|| self.current_mode.clone());
        if let Some(model) = &cfg.model {
            let offers_model = ns["configOptions"]
                .as_array()
                .is_some_and(|a| a.iter().any(|o| o["id"] == "model"));
            if offers_model {
                let req: agent_client_protocol::schema::v1::SetSessionConfigOptionRequest =
                    typed(json!({"sessionId": session_id, "configId": "model", "value": model}))?;
                self.conn
                    .send_request(req)
                    .block_task()
                    .await
                    .map_err(|e| AcpError::Protocol(e.to_string()))?;
            }
        }
        Ok(SessionStarted {
            session_id,
            agent_name: init["agentInfo"]["name"].as_str().map(str::to_owned),
            agent_version: init["agentInfo"]["version"].as_str().map(str::to_owned),
            modes: self.modes.clone(),
            current_mode: self.current_mode.clone(),
            gate_fidelity: if self.strict_mode.is_some() {
                GateFidelity::EveryCall
            } else {
                GateFidelity::PromptOnly
            },
            mcp_http: init["agentCapabilities"]["mcpCapabilities"]["http"]
                .as_bool()
                .unwrap_or(false),
            initialize: init,
            new_session: ns,
        })
    }

    async fn turn(&mut self, input: TurnInput, mode: Mode) -> Result<TurnEnd, AcpError> {
        let session = lock(&self.session).clone().ok_or(AcpError::NotStarted)?;
        if self.is_closed() {
            self.reap_if_exited().await;
            return Err(AcpError::Closed);
        }
        let want = match mode {
            Mode::Strict => self.strict_mode.clone(),
            Mode::Auto => self.auto_mode.clone(),
        };
        self.set_mode(want).await?;
        *lock(&self.shared.mode) = mode;
        *lock(&self.shared.stats) = TurnStats::default();
        let notices: Vec<String> = std::mem::take(&mut *lock(&self.shared.rejected));
        let mut text = String::new();
        for n in &notices {
            text.push_str(n);
            text.push_str("\n\n");
        }
        text.push_str(&input.text);
        let mut blocks = vec![json!({"type": "text", "text": text})];
        blocks.extend(
            input
                .data_blocks
                .iter()
                .map(|b| json!({"type": "text", "text": b})),
        );
        let req: PromptRequest = typed(json!({"sessionId": session, "prompt": blocks}))?;
        // The response is handled in the dispatch loop's order (a prepared request's callback), so every
        // update of the turn has been turned into an event before the turn ends.
        let (rtx, rrx) = oneshot::channel::<Result<PromptResponse, String>>();
        self.conn
            .prepare_request(req)
            .on_receiving_result(async move |r| {
                let _ = rtx.send(r.map_err(|e| e.to_string()));
                Ok(())
            })
            .map_err(|e| AcpError::Protocol(e.to_string()))?;
        let result: Result<PromptResponse, AcpError> = match self.turn_timeout {
            Some(t) => match tokio::time::timeout(t, rrx).await {
                Ok(Ok(r)) => r.map_err(AcpError::Protocol),
                Ok(Err(_)) => Err(AcpError::Closed),
                Err(_) => {
                    self.cancel_handle().cancel();
                    Err(AcpError::Timeout)
                }
            },
            None => match rrx.await {
                Ok(r) => r.map_err(AcpError::Protocol),
                Err(_) => Err(AcpError::Closed),
            },
        };
        let resp = match result {
            Ok(r) => r,
            Err(e @ AcpError::Timeout) => return Err(e),
            Err(e) => {
                let gone = matches!(e, AcpError::Closed)
                    || e.to_string().contains("transport closed")
                    || self.is_closed();
                if gone {
                    self.wait_for_exit().await;
                    return Err(AcpError::Closed);
                }
                return Err(e);
            }
        };
        let v = serde_json::to_value(&resp).unwrap_or_default();
        let stop_reason = v["stopReason"].as_str().unwrap_or("end_turn").to_owned();
        let meta = v.get("_meta").cloned();
        let (produced_output, tool_calls) = {
            let st = lock(&self.shared.stats);
            (st.output, st.tool_calls)
        };
        let suspect_failure = stop_reason == "end_turn" && !produced_output && tool_calls == 0;
        let provider_error = if suspect_failure {
            self.probe.as_ref().and_then(|p| p())
        } else {
            None
        };
        let _ = self.tx.send(AgentEvent::TurnEnded {
            stop_reason: stop_reason.clone(),
            meta: meta.clone(),
        });
        Ok(TurnEnd {
            stop_reason,
            produced_output,
            tool_calls,
            suspect_failure,
            provider_error,
            meta,
        })
    }

    async fn cancel(&mut self) {
        self.cancel_handle().cancel();
    }

    fn events(&mut self) -> BoxStream<'static, AgentEvent> {
        match self.events.take() {
            Some(rx) => tokio_stream_from(rx).boxed(),
            None => futures::stream::empty().boxed(),
        }
    }
}

impl Drop for AcpDriver {
    fn drop(&mut self) {
        if let Some(stop) = self.shutdown.take() {
            let _ = stop.send(());
        }
    }
}

fn tokio_stream_from(
    mut rx: mpsc::UnboundedReceiver<AgentEvent>,
) -> impl futures::Stream<Item = AgentEvent> {
    // `try_recv` first: tokio's cooperative budget can make `poll_recv` report Pending with items queued,
    // which would end a `now_or_never` drain early.
    futures::stream::poll_fn(move |cx| match rx.try_recv() {
        Ok(e) => std::task::Poll::Ready(Some(e)),
        Err(mpsc::error::TryRecvError::Disconnected) => std::task::Poll::Ready(None),
        Err(mpsc::error::TryRecvError::Empty) => rx.poll_recv(cx),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_counts_sum_models() {
        let meta = json!({"quota": {"token_count": {"totalTokens": 30341, "inputTokens": 2, "cachedInputTokens": 30065, "cachedWriteTokens": 235, "outputTokens": 39},
            "model_usage": [
                {"model": "a", "token_count": {"totalTokens": 30341, "inputTokens": 2, "cachedInputTokens": 30065, "cachedWriteTokens": 235, "outputTokens": 39}},
                {"model": "b", "token_count": {"totalTokens": 1533, "inputTokens": 1434, "cachedInputTokens": 0, "cachedWriteTokens": 0, "outputTokens": 99}}]}});
        let c = TokenCounts::from_meta(&meta).unwrap();
        assert_eq!(c.input, 1436);
        assert_eq!(c.output, 138);
        assert_eq!(c.cached_input, 30065);
        assert_eq!(TokenCounts::from_meta(&json!({})), None);
    }

    #[test]
    fn strict_policy_allows_only_local_edits() {
        let p = SandboxPolicy {
            worktree: PathBuf::from("/w"),
        };
        let ask = |kind: &str, loc: &str| PermissionAsk {
            tool_call_id: "t".into(),
            title: "x".into(),
            kind: Some(kind.into()),
            raw_input: None,
            locations: vec![loc.into()],
            options: vec![],
        };
        assert_eq!(
            p.decide(&ask("execute", "a"), Mode::Auto),
            PermissionDecision::Allow
        );
        assert_eq!(
            p.decide(&ask("edit", "a.txt"), Mode::Strict),
            PermissionDecision::Allow
        );
        assert!(matches!(
            p.decide(&ask("edit", "../x"), Mode::Strict),
            PermissionDecision::Reject { .. }
        ));
        assert!(matches!(
            p.decide(&ask("execute", "a"), Mode::Strict),
            PermissionDecision::Reject { .. }
        ));
        assert!(matches!(
            p.decide(&ask("fetch", "a"), Mode::Strict),
            PermissionDecision::Reject { .. }
        ));
    }

    #[test]
    fn map_update_reads_tool_output() {
        let v = json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed",
            "content": [{"type": "content", "content": {"type": "text", "text": "hi"}}]});
        match &map_update(&v)[0] {
            AgentEvent::ToolCallUpdate { output, status, .. } => {
                assert_eq!(output.as_deref(), Some("hi"));
                assert_eq!(status.as_deref(), Some("completed"));
            }
            other => panic!("{other:?}"),
        }
    }
}

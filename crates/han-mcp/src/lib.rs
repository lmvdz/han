//! `han-mcp`: the agent tools (PLAN §1.5, §6.4; SR-29, SR-54…SR-60), an MCP server over `rmcp`.
//!
//! The server runs **outside** the sandbox, in han-host. The agent reaches it over stdio through a stub inside
//! the sandbox, which tunnels the bytes over the session's proxy (SR-29); [`serve`] takes whatever byte stream
//! the host hands it. Tools (PLAN names → MCP names):
//!
//! | PLAN name | MCP tool | does |
//! |---|---|---|
//! | `propose` | `propose` | the only route to the outside world: the host turns it into a signed `effect.propose` |
//! | `team.sessions` | `team_sessions` | the live board of the space |
//! | `team.who_is_on(issue\|path)` | `team_who_is_on` | who works on an issue or a path |
//! | `team.read(session, budget_tokens)` | `team_read` | one session, layered and cheapest first, within a budget |
//! | `team.overlaps()` | `team_overlaps` | where work meets |
//!
//! MCP allows dots in tool names, but model APIs do not (Anthropic's tool names match
//! `^[a-zA-Z0-9_-]{1,128}$`, and harnesses prefix them, as in `mcp__han__team_read`), so the MCP names use
//! underscores. pi and Claude Code show them as `mcp__han__<name>`.
//!
//! Every tool takes a required `summary` argument (PLAN §6.4 Summaries, layer 1). Every `team_*` answer is a
//! data envelope (SR-54): `{"han_data": {kind, source, trust: "untrusted", text, data}}`, structured fields in
//! `data`, free text only in `text`. There is no tool to approve, govern (`member.set`, `policy.set`) or steer
//! another session (R-24): those are refused by construction.

mod tools;

use std::sync::Arc;
use std::time::Duration;

use han_node::api::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use tools::{HanTools, tool_list};

/// The MCP names of the tools, in the order they are listed. A snapshot test pins this list (SR-29).
pub const TOOL_NAMES: [&str; 5] = [
    "propose",
    "team_sessions",
    "team_who_is_on",
    "team_read",
    "team_overlaps",
];

/// The longest `summary` an agent may give, in characters.
pub const MAX_SUMMARY_CHARS: usize = 500;

/// The largest `budget_tokens` `team_read` honours.
pub const MAX_BUDGET_TOKENS: u64 = 20_000;

/// How long a tool waits for the node's first snapshot of a view.
pub const VIEW_TIMEOUT: Duration = Duration::from_secs(5);

/// A `propose` call as the host receives it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProposeRequest {
    /// What the agent says it is doing and why (layer-1 summary).
    pub summary: String,
    /// The action kind, for example `git_push`, `open_pr` or `http_request`.
    pub kind: String,
    /// The action's arguments as the agent wrote them.
    pub effect: Value,
}

/// How the host answered a proposal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProposeAnswer {
    /// Proposed: an `effect.propose` with this id is in the log (or the outbox).
    Proposed {
        /// The proposal's entry id.
        id: String,
        /// Who must approve, as text.
        waiting_for: String,
    },
    /// Refused before proposing (unknown action, invalid arguments, no executor, over budget).
    Refused {
        /// Why.
        reason: String,
    },
}

/// What the tools need from the host that runs them. han-host implements it over its node API; tests use a
/// fake.
pub trait HostBridge: Send + Sync + 'static {
    /// Turns a proposal into a signed `effect.propose`.
    fn propose(&self, req: ProposeRequest) -> BoxFuture<'_, ProposeAnswer>;

    /// The node API, restricted to what an agent may do (SR-06): views, and agent-class commands only. The
    /// tools here only open views.
    fn node(&self) -> Arc<dyn han_node::api::NodeApi>;

    /// The space the session works in.
    fn space(&self) -> String;

    /// The session the tools serve.
    fn session(&self) -> String;

    /// Called on every tool call, with the tool's MCP name and the `summary` the agent gave: status layer 1,
    /// and taint (a `team_read` makes the turn read another session's content, SR-56).
    fn tool_used(&self, tool: &str, summary: &str);

    /// Layer-2 summaries the host holds for a session (fork summaries), newest first; may be empty. The host
    /// returns only summaries the reader may see (SR-66).
    fn summaries(&self, session: &str) -> Vec<String>;
}

/// Why [`serve`] ended with an error.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    /// The MCP handshake failed.
    #[error("mcp initialize: {0}")]
    Initialize(String),
    /// The session ended abnormally.
    #[error("mcp session: {0}")]
    Session(String),
}

/// Serves the HAN tools on one byte stream until the client goes away.
pub async fn serve<IO>(io: IO, bridge: Arc<dyn HostBridge>) -> Result<(), McpError>
where
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    use rmcp::ServiceExt;
    let running = HanTools::new(bridge)
        .serve(io)
        .await
        .map_err(|e| McpError::Initialize(e.to_string()))?;
    running
        .waiting()
        .await
        .map(|_| ())
        .map_err(|e| McpError::Session(e.to_string()))
}

/// The source of a delivered piece of data (SR-54).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// The space.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<String>,
    /// The actor whose data it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// The session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The item's seq, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// `status`, `fork-summary`, `transcript` or `github`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

/// A data envelope (SR-54): `{"han_data": {kind, source, trust: "untrusted", text, data?}}`.
pub fn envelope(kind: &str, source: &Source, text: &str, data: Value) -> Value {
    let mut inner = serde_json::Map::new();
    inner.insert("kind".into(), Value::String(kind.into()));
    inner.insert(
        "source".into(),
        serde_json::to_value(source).unwrap_or(Value::Null),
    );
    inner.insert("trust".into(), Value::String("untrusted".into()));
    inner.insert("text".into(), Value::String(text.into()));
    if !data.is_null() {
        inner.insert("data".into(), data);
    }
    let mut outer = serde_json::Map::new();
    outer.insert("han_data".into(), Value::Object(inner));
    Value::Object(outer)
}

/// Cuts `s` to at most `max` characters, marking a cut with `…` (SR-59).
pub fn bound(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The token estimate `team_read` budgets with: one token per four characters, rounded up.
pub fn estimate_tokens(s: &str) -> u64 {
    (s.chars().count() as u64).div_ceil(4)
}

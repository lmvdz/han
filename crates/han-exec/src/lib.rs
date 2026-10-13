//! `han-exec`: the executor (PLAN §5 Action, §6.7; spec architecture.md §4.5 X-01…X-12). The only process
//! that touches the world, and the only holder of the credentials actions need.
//!
//! * [`Action`]: one kind of effect (`git_push`, `open_pr`, `http_request`), desired-state, with a closed plan
//!   schema checked before claiming and again before launching (X-09, SR-70).
//! * [`ledger::Ledger`]: the SQLite launch journal keyed by [`IdempotencyKey`] (X-02…X-06, SR-68). "Unknown"
//!   is never success; invocation ids (attempt numbers) are never reused.
//! * [`gate::Gate`]: the local gate seam. Until spec round 33 (risk tiers, semantic gates) every action outside
//!   the sandbox needs an approval; the gate reads the plan's [`han_vcs::ChangeSummary`] when there is one.
//! * [`Executor`]: the loop over the node API's `Effects` view: gate, plan check, claim, launch, report.
//! * [`github::GithubMinter`]: the single minter of GitHub App installation tokens (SR-49), each scoped to one
//!   repository and the permissions of one action (SR-67).

pub mod actions;
pub mod clock;
mod executor;
pub mod gate;
pub mod github;
pub mod ledger;
pub mod secret;

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use clock::{Elapsed, SystemElapsed};
pub use executor::{ExecError, Executor, ExecutorConfig, StepReport};
pub use gate::{Gate, GateDecision, GateInput, NeedsApproval};
pub use ledger::{Ledger, LedgerError, Report};
pub use secret::Secret;

/// The key an attempt is journalled and deduplicated by: the space, the proposal and the attempt number of the
/// claim (X-01). An attempt number is used for at most one claim, ever.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct IdempotencyKey {
    /// The space.
    pub space: String,
    /// The proposal's entry id.
    pub proposal: String,
    /// The attempt number (the claim's `attempt`).
    pub attempt: u64,
}

impl IdempotencyKey {
    /// The key as one string, `<space>|<proposal>|<attempt>`.
    pub fn as_string(&self) -> String {
        format!("{}|{}|{}", self.space, self.proposal, self.attempt)
    }
}

/// Why a plan is refused (SR-70).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    /// `sha256(JCS(plan))` is not the proposal's `planDigest`.
    #[error("the plan does not hash to the approved digest")]
    Digest,
    /// The plan does not match the action's closed schema.
    #[error("plan schema: {0}")]
    Schema(String),
    /// A target (repository, branch, host) is outside the space's configuration.
    #[error("out of scope: {0}")]
    OutOfScope(String),
    /// The change the plan would land is not the approved changeset (digest mismatch).
    #[error("changeset: {0}")]
    Changeset(String),
    /// No action of this kind.
    #[error("unsupported action {0}")]
    Unsupported(String),
    /// The world could not be read to check the plan (for example the source workspace is missing).
    #[error("preflight: {0}")]
    Preflight(String),
}

/// How an attempt ended (X-05). `Unknown` is never treated as success.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Outcome {
    /// The desired state holds, and this attempt made it so or found it so.
    Succeeded {
        /// What happened, for the journal (never a credential).
        detail: String,
    },
    /// It did not happen. `started` says whether anything was sent to the outside system.
    Failed {
        /// Whether the action started.
        started: bool,
        /// Why.
        detail: String,
    },
    /// The executor cannot tell whether it happened.
    Unknown {
        /// Why.
        detail: String,
    },
}

impl Outcome {
    /// The `(result, started)` pair reported in `effect.outcome` for an attempt that was launched.
    pub fn report(&self) -> Report {
        match self {
            Outcome::Succeeded { .. } => Report::new("succeeded", "yes"),
            Outcome::Failed { started: true, .. } => Report::new("failed", "yes"),
            Outcome::Failed { started: false, .. } => Report::new("failed", "no"),
            Outcome::Unknown { .. } => Report::new("unknown", "yes"),
        }
    }

    /// The detail text.
    pub fn detail(&self) -> &str {
        match self {
            Outcome::Succeeded { detail }
            | Outcome::Failed { detail, .. }
            | Outcome::Unknown { detail } => detail,
        }
    }
}

/// What an action may use while it runs.
pub struct ActionCtx {
    /// The space's configuration of targets and credentials.
    pub config: Arc<ExecConfig>,
    /// The version-control backend.
    pub vcs: Arc<dyn han_vcs::Vcs>,
    /// Where the action may keep scratch state (a per-attempt directory under the executor's state).
    pub scratch: std::path::PathBuf,
    /// The GitHub token minter, when a GitHub App is configured.
    pub github: Option<Arc<github::GithubMinter>>,
}

/// The executor's configuration: which targets each action may reach, and how to get credentials for them.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecConfig {
    /// Repositories `git_push` and `open_pr` may write.
    #[serde(default)]
    pub repos: Vec<RepoTarget>,
    /// Hosts `http_request` may reach (exact names, compared case-insensitively; HTTPS only).
    #[serde(default)]
    pub http_hosts: Vec<String>,
    /// Branch prefixes a push may move (default `han/`). A ref outside them is out of scope.
    #[serde(default = "default_prefixes")]
    pub branch_prefixes: Vec<String>,
    /// The GitHub App, when GitHub actions are configured.
    #[serde(default)]
    pub github: Option<GithubApp>,
    /// TEST ONLY: let `http_request` use `http://` and loopback or private destinations. Never read from a
    /// configuration file.
    #[serde(skip)]
    pub test_allow_private_http: bool,
}

fn default_prefixes() -> Vec<String> {
    vec!["han/".into()]
}

impl ExecConfig {
    /// The repository a plan's `target` names.
    pub fn repo(&self, name: &str) -> Option<&RepoTarget> {
        self.repos.iter().find(|r| r.name == name)
    }
}

/// A repository the executor may write.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoTarget {
    /// The name plans use, `github:owner/repo` or a local name.
    pub name: String,
    /// The remote to push to (an `https://` URL, or a path for tests).
    pub remote: String,
    /// The GitHub `owner/repo`, when credentials come from the GitHub App (an installation token per action,
    /// SR-67) and `open_pr` may run on it.
    #[serde(default)]
    pub github: Option<String>,
}

/// A GitHub App: the private key is loaded by the executor itself (keychain or file) and never leaves it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GithubApp {
    /// The App id.
    pub app_id: u64,
    /// The installation id.
    pub installation_id: u64,
    /// The API base (default `https://api.github.com`; a mock in tests).
    #[serde(default)]
    pub api_base: Option<String>,
    /// Where the PEM private key is: a file path (the keychain loader lives in han-node).
    pub private_key: String,
}

/// One kind of effect (PLAN §5). Actions are desired state: running one twice leaves the world as running it
/// once, and [`Action::lookup`] tells whether the desired state already holds (X-06 after a restart).
#[async_trait::async_trait]
pub trait Action: Send + Sync + 'static {
    /// The action name plans use, for example `git_push`.
    fn kind(&self) -> &'static str;

    /// Checks the plan against the action's closed schema and the configured targets (SR-70). Pure: no I/O.
    fn plan_check(&self, plan: &Value, config: &ExecConfig) -> Result<(), PlanError>;

    /// Checks what the plan would do against the world, for example that the commit to push has exactly the
    /// approved changeset. Runs before claiming and again before launching (X-09). May read, never write.
    async fn preflight(&self, plan: &Value, ctx: &ActionCtx) -> Result<(), PlanError>;

    /// Runs it.
    async fn run(&self, plan: &Value, key: &IdempotencyKey, ctx: &ActionCtx) -> Outcome;

    /// Whether the desired state already holds (`Some(Succeeded)`), or cannot be told (`None`). Only
    /// `Succeeded` is trusted after a launch; anything else is reported `unknown`.
    async fn lookup(&self, plan: &Value, key: &IdempotencyKey, ctx: &ActionCtx) -> Option<Outcome>;
}

/// The plan's `args` object, or a schema error.
pub(crate) fn plan_args(plan: &Value, kind: &str) -> Result<Value, PlanError> {
    let obj = plan
        .as_object()
        .ok_or_else(|| PlanError::Schema("a plan is an object".into()))?;
    for k in obj.keys() {
        if !["action", "target", "args"].contains(&k.as_str()) {
            return Err(PlanError::Schema(format!("unknown plan member {k}")));
        }
    }
    if obj.get("action").and_then(Value::as_str) != Some(kind) {
        return Err(PlanError::Schema(format!("the action is not {kind}")));
    }
    if obj.get("target").and_then(Value::as_str).is_none() {
        return Err(PlanError::Schema("a plan names its target".into()));
    }
    obj.get("args")
        .cloned()
        .ok_or_else(|| PlanError::Schema("a plan has args".into()))
}

/// The plan's target.
pub(crate) fn plan_target(plan: &Value) -> &str {
    plan.get("target").and_then(Value::as_str).unwrap_or("")
}

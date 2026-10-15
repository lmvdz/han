//! `han-exec`: the executor (PLAN §5 Action, §6.7; spec architecture.md §4.5 X-01…X-12). The only process
//! that touches the world, and the only holder of the credentials actions need.
//!
//! * [`Action`]: one kind of effect (`git_push`, `vcs.land`, `open_pr`, `http_request`), desired-state, with a closed plan
//!   schema checked before claiming and again before launching (X-09, SR-70).
//! * [`ledger::Ledger`]: the SQLite launch journal keyed by [`IdempotencyKey`] (X-02…X-06, SR-68). "Unknown"
//!   is never success; invocation ids (attempt numbers) are never reused.
//! * [`gate::Gate`]: the local gate seam. Until spec round 33 (risk tiers, semantic gates) every action outside
//!   the sandbox needs an approval; the gate reads the plan's [`han_vcs::ChangeSummary`] when there is one.
//! * [`Executor`]: the loop over the node API's `Effects` view: gate, plan check, claim, launch, report.
//! * [`Lane`]: the landing lane (R33), a service of the executor that claims `vcs.land` effects at push time,
//!   after the commit it squash-rebased onto the target ref has passed the repository's [`Checks`].
//! * [`github::GithubMinter`]: the single minter of GitHub App installation tokens (SR-49), each scoped to one
//!   repository and the permissions of one action (SR-67).

pub mod actions;
pub mod checks;
pub mod clock;
mod executor;
pub mod gate;
pub mod github;
pub mod lane;
pub mod ledger;
pub mod secret;

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use checks::Checks;
pub use clock::{Elapsed, SystemElapsed, SystemWall, Wall};
pub use executor::{ExecError, Executor, ExecutorConfig, LaneClaim, StepReport};
pub use gate::{Gate, GateDecision, GateInput, NeedsApproval};
pub use lane::Lane;
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
    /// The bytes of the plan's `argsBlob`, fetched from the space and checked against its hash and size
    /// before the action sees them (`None` when the plan has none, or after a restart, where only `lookup`
    /// runs).
    pub args_blob: Option<Arc<Vec<u8>>>,
    /// The landing lane's instruction for this attempt (see [`LaneInstruction`]); `None` for every effect
    /// that is not claimed by a lane. The executor journals it with the claim and re-attaches it for `lookup`
    /// after a restart.
    pub lane: Option<LaneInstruction>,
}

/// What a landing lane tells `vcs.land` at claim time, outside the plan (the plan is fixed at approval, the
/// lane's rebase happens after it).
///
/// The executor attaches it to [`ActionCtx::lane`] when a lane claims the effect, after the lane has
/// squash-rebased the approved change onto the target ref's tip and tested exactly that commit. The lane
/// can sign its evidence ([`checks::ChecksEvidence`]) over the same values, so the claim, the evidence and the
/// push name one commit.
/// It is data, not authority: `vcs.land` re-checks all of it against the plan before the ref moves, and
/// refuses to run without it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LaneInstruction {
    /// The tip of the target ref the lane tested on: the value the ref must have for the move to happen.
    pub onto: String,
    /// The squashed commit the lane tested, a single commit directly on `onto`.
    pub commit: String,
    /// A repository of the executor's own that holds `onto` and `commit` (and the plan's `base..to`), as
    /// the lane's squash-rebase left it. Never the agent's workspace.
    pub repository: std::path::PathBuf,
    /// The checks that justified the landing, a blob the lane already uploaded: the attempt's
    /// `effect.outcome` carries it as `evidence`, so a reader of the log finds it under G-01.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<han_node::api::EvidenceRef>,
}

/// A plan's `argsBlob` (a `BlobRef`): the bytes an action needs beyond `args`, carried by the space.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArgsBlob {
    /// `sha256` of the bytes, lowercase hex.
    pub hash: String,
    /// The size in bytes.
    pub size: u64,
    /// The media type.
    #[serde(rename = "type")]
    pub media_type: String,
    /// A transport locator (ignored: the bytes come from the space's home).
    #[serde(default)]
    pub uri: Option<String>,
}

/// The plan's `argsBlob`, if it has one.
pub fn args_blob(plan: &Value) -> Result<Option<ArgsBlob>, PlanError> {
    match plan.get("argsBlob") {
        None => Ok(None),
        Some(v) => {
            let b: ArgsBlob = serde_json::from_value(v.clone())
                .map_err(|e| PlanError::Schema(format!("argsBlob: {e}")))?;
            if b.hash.len() != 64
                || !b
                    .hash
                    .bytes()
                    .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
            {
                return Err(PlanError::Schema("argsBlob: hash is not a digest".into()));
            }
            Ok(Some(b))
        }
    }
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    /// The full refs `vcs.land` may move on this repository, listed explicitly (for example
    /// `refs/heads/main`); separate from [`ExecConfig::branch_prefixes`], which only `git_push` and `open_pr`
    /// use. Default empty: nothing lands, and no landing lane runs, until a ref is configured.
    #[serde(default)]
    pub landing_refs: Vec<String>,
    /// What the repository's landing lane reads from its checks.
    #[serde(default)]
    pub checks: ChecksConfig,
    /// How long a change may stay in each lane state before it is reported stalled.
    #[serde(default)]
    pub deadlines: Deadlines,
    /// The branch prefix the lane pushes the commits it tests to (`<prefix><ref>/<proposal>`); default
    /// `han/lane/`.
    #[serde(default = "default_lane_prefix")]
    pub lane_branch_prefix: String,
}

fn default_lane_prefix() -> String {
    "han/lane/".into()
}

impl RepoTarget {
    /// A repository with nothing configured for landing.
    pub fn new(name: impl Into<String>, remote: impl Into<String>) -> RepoTarget {
        RepoTarget {
            name: name.into(),
            remote: remote.into(),
            github: None,
            landing_refs: Vec::new(),
            checks: ChecksConfig::default(),
            deadlines: Deadlines::default(),
            lane_branch_prefix: default_lane_prefix(),
        }
    }
}

/// The checks a repository's landing lane gates on (R33 §4 "Two stages").
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChecksConfig {
    /// Names of the checks that must be green on the exact commit the lane lands (`fast`, pre-merge).
    #[serde(default = "default_required")]
    pub required: Vec<String>,
    /// Names of the checks that must be green on the landed commit before the change is `confirmed` (`full`,
    /// post-merge). Default empty: a repository with no `full` set treats `landed` as `confirmed`.
    #[serde(default)]
    pub full: Vec<String>,
    /// Seconds between two reads of a backend (default 30).
    #[serde(default = "default_poll_secs")]
    pub poll_secs: u64,
    /// Seconds a required check may be missing before the change is reported stalled (default 300): GitHub
    /// runs nothing on a pull request that cannot merge, so a check that never appears is not a slow one.
    #[serde(default = "default_appear_secs")]
    pub appear_secs: u64,
    /// How many times the lane asks for a red required check to run again before it rejects the change
    /// (default 0).
    #[serde(default)]
    pub rerun: u32,
}

fn default_required() -> Vec<String> {
    vec!["quick".into()]
}

fn default_poll_secs() -> u64 {
    30
}

fn default_appear_secs() -> u64 {
    300
}

impl Default for ChecksConfig {
    fn default() -> Self {
        ChecksConfig {
            required: default_required(),
            full: Vec::new(),
            poll_secs: default_poll_secs(),
            appear_secs: default_appear_secs(),
            rerun: 0,
        }
    }
}

/// The deadline of each lane state, in seconds (R33 §3). A change past its state's deadline is reported
/// stalled, with the reason; nothing else happens to it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deadlines {
    /// `queued`: default 1800 (30 min, for a lane of one).
    #[serde(default = "default_queued_secs")]
    pub queued_secs: u64,
    /// `testing`: the repository's CI timeout plus ten minutes; default 2400.
    #[serde(default = "default_testing_secs")]
    pub testing_secs: u64,
    /// `landing`: the claim's `launchWindow` when absent.
    #[serde(default)]
    pub landing_secs: Option<u64>,
    /// `landed`: the full suite's timeout plus 30 minutes; default 3600.
    #[serde(default = "default_landed_secs")]
    pub landed_secs: u64,
    /// A change in `testing` that stays stalled this long is rejected (`timeout`); default 86400 (24 h). Past its
    /// deadline a stalled change already steps aside for the changes behind it.
    #[serde(default = "default_stall_secs")]
    pub stall_secs: u64,
}

fn default_queued_secs() -> u64 {
    1800
}

fn default_testing_secs() -> u64 {
    2400
}

fn default_landed_secs() -> u64 {
    3600
}

fn default_stall_secs() -> u64 {
    86_400
}

impl Default for Deadlines {
    fn default() -> Self {
        Deadlines {
            queued_secs: default_queued_secs(),
            testing_secs: default_testing_secs(),
            landing_secs: None,
            landed_secs: default_landed_secs(),
            stall_secs: default_stall_secs(),
        }
    }
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

    /// Whether only a landing lane may claim this action's effects. The executor's own loop never claims one
    /// (the lane claims at push time, after its checks are green); default `false`.
    fn lane_only(&self) -> bool {
        false
    }

    /// Runs it.
    async fn run(&self, plan: &Value, key: &IdempotencyKey, ctx: &ActionCtx) -> Outcome;

    /// Whether the desired state already holds (`Some(Succeeded)`), or cannot be told (`None`). Only
    /// `Succeeded` is trusted after a launch; anything else is reported `unknown`.
    async fn lookup(&self, plan: &Value, key: &IdempotencyKey, ctx: &ActionCtx) -> Option<Outcome>;
}

/// The plan's `args` object, or a schema error.
pub(crate) fn plan_args(plan: &Value, kind: &str) -> Result<Value, PlanError> {
    plan_args_with(plan, kind, false)
}

/// Like [`plan_args`]; with `blob`, the plan may also carry an `argsBlob`.
pub(crate) fn plan_args_with(plan: &Value, kind: &str, blob: bool) -> Result<Value, PlanError> {
    let obj = plan
        .as_object()
        .ok_or_else(|| PlanError::Schema("a plan is an object".into()))?;
    for k in obj.keys() {
        if !(["action", "target", "args"].contains(&k.as_str()) || (blob && k == "argsBlob")) {
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

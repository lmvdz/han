//! The configuration of the review service (`han review`): `review.json` in the reviewer's directory, written
//! by `han review set`. Off unless configured: a missing file or `enabled: false` starts nothing.
//!
//! The model API key is never in the file, only the name of the environment variable that holds it
//! ([`Route::key_env`]); the service unit's environment file sets the variable.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::backend::KeyStyle;
use crate::independence::{AuthorInfo, Independence, ReviewerInfo};
use crate::review::ReviewerConfig;

/// The file's name inside the reviewer's directory.
pub const FILE: &str = "review.json";

/// How the key goes on the request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeyHeader {
    /// `Authorization: Bearer <key>`.
    #[default]
    Bearer,
    /// `x-api-key: <key>`.
    XApiKey,
}

impl From<KeyHeader> for KeyStyle {
    fn from(h: KeyHeader) -> KeyStyle {
        match h {
            KeyHeader::Bearer => KeyStyle::Bearer,
            KeyHeader::XApiKey => KeyStyle::XApiKey,
        }
    }
}

fn default_path() -> String {
    "/v1/chat/completions".into()
}

fn default_backend() -> String {
    "omniroute".into()
}

fn default_timeout() -> u64 {
    120
}

/// The model route: an OpenAI-compatible chat endpoint such as OmniRoute's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    /// The backend's name in the record (`omniroute`).
    #[serde(default = "default_backend")]
    pub name: String,
    /// The base URL: `https://…`, or `http://` to a loopback host.
    pub base: String,
    /// The path of the chat endpoint.
    #[serde(default = "default_path")]
    pub path: String,
    /// The name of the environment variable that holds the API key. The key itself is never in this file.
    pub key_env: String,
    /// How the key is sent.
    #[serde(default)]
    pub key_header: KeyHeader,
    /// Seconds one model call may take.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

/// What the reviewer knows of an author: the node does not say which harness and model ran an actor, so the
/// configuration declares it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorSpec {
    /// The harness (`claude-code`, `pi`).
    pub harness: String,
    /// The model it runs (`claude-opus-4-1`).
    pub model: String,
    /// The author's operators, nearest first, then theirs (the `strict` operator-chain rule).
    #[serde(default)]
    pub operators: Vec<String>,
}

fn default_confidence() -> u8 {
    80
}

fn default_diff_bytes() -> usize {
    200_000
}

fn default_poll() -> u64 {
    15
}

/// The service's configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    /// Whether the service reviews at all. Default off.
    #[serde(default)]
    pub enabled: bool,
    /// Whether an `approve` becomes an approval. Off: the service records and messages only (a shadow run).
    #[serde(default)]
    pub approve: bool,
    /// The reviewer actor, for example `@reviewer:runner.han.home.arpa`.
    pub actor: String,
    /// The reviewer's operators, nearest first, then theirs.
    #[serde(default)]
    pub operators: Vec<String>,
    /// The spaces watched.
    #[serde(default)]
    pub spaces: Vec<String>,
    /// The repositories as plans name them (`github:owner/repo`) and where the reviewer fetches a bundle's base
    /// from (a remote it can read without credentials, for example the public HTTPS URL).
    #[serde(default)]
    pub repos: BTreeMap<String, String>,
    /// The model route.
    pub route: Route,
    /// The model, for example `agy/gemini-flash`. It must not be of the author's vendor.
    pub model: String,
    /// The output ceiling asked of the model. Set it for the route in use: for the agy Gemini Flash route about
    /// 16384, and a wrong value gives truncation or spurious compaction.
    pub max_tokens: u32,
    /// `strict` (default) or `dogfood_relaxed`.
    #[serde(default)]
    pub independence: Independence,
    /// The authors, by actor.
    #[serde(default)]
    pub authors: BTreeMap<String, AuthorSpec>,
    /// The author of any actor not in `authors`.
    #[serde(default)]
    pub default_author: Option<AuthorSpec>,
    /// An `approve` under this confidence (0 to 100) is recorded, not acted on.
    #[serde(default = "default_confidence")]
    pub min_confidence: u8,
    /// A diff over this many bytes is not sent to the model.
    #[serde(default = "default_diff_bytes")]
    pub max_diff_bytes: usize,
    /// Seconds between two reads of the open proposals.
    #[serde(default = "default_poll")]
    pub poll_secs: u64,
}

impl ServiceConfig {
    /// A configuration with the defaults, off.
    pub fn new(actor: &str, model: &str, max_tokens: u32, route: Route) -> ServiceConfig {
        ServiceConfig {
            enabled: false,
            approve: false,
            actor: actor.to_owned(),
            operators: Vec::new(),
            spaces: Vec::new(),
            repos: BTreeMap::new(),
            route,
            model: model.to_owned(),
            max_tokens,
            independence: Independence::Strict,
            authors: BTreeMap::new(),
            default_author: None,
            min_confidence: default_confidence(),
            max_diff_bytes: default_diff_bytes(),
            poll_secs: default_poll(),
        }
    }

    /// Why the configuration can't work, if it can't.
    pub fn validate(&self) -> Result<(), String> {
        if !self.actor.starts_with('@') || !self.actor.contains(':') {
            return Err(format!("actor {:?} is not `@name:server`", self.actor));
        }
        if self.max_tokens == 0 {
            return Err(
                "max_tokens must be set (about 16384 for the agy Gemini Flash route)".into(),
            );
        }
        if self.model.trim().is_empty() {
            return Err("model is empty".into());
        }
        if crate::independence::vendor_of(&self.model).is_none() {
            return Err(format!(
                "the vendor of model {:?} is not known, so independence can't be shown",
                self.model
            ));
        }
        if self.min_confidence > 100 {
            return Err("min_confidence is 0 to 100".into());
        }
        if self.poll_secs == 0 {
            return Err("poll_secs must be at least 1".into());
        }
        if !self.route.base.starts_with("https://")
            && !["http://127.0.0.1", "http://localhost", "http://[::1]"]
                .iter()
                .any(|p| self.route.base.starts_with(p))
        {
            return Err("route.base is TLS only (http only to loopback)".into());
        }
        if !self.route.path.starts_with('/') {
            return Err("route.path starts with `/`".into());
        }
        let key = &self.route.key_env;
        if key.is_empty()
            || !key
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(format!(
                "route.key_env {key:?} is not an environment variable name (the key itself never goes in the file)"
            ));
        }
        Ok(())
    }

    /// The reviewer as independence sees it.
    pub fn reviewer_info(&self) -> ReviewerInfo {
        ReviewerInfo {
            actor: self.actor.clone(),
            operator_chain: self.operators.clone(),
            model: self.model.clone(),
        }
    }

    /// What the reviewer knows of `actor`, from `authors` or `default_author`; `None` when it knows nothing,
    /// which refuses the review (independence can't be shown for an unknown author).
    pub fn author_of(&self, actor: &str) -> Option<AuthorInfo> {
        let spec = self.authors.get(actor).or(self.default_author.as_ref())?;
        Some(AuthorInfo {
            actor: actor.to_owned(),
            operator_chain: spec.operators.clone(),
            harness: spec.harness.clone(),
            model: spec.model.clone(),
        })
    }

    /// The pipeline's configuration. `enabled` follows the file; `authority` follows `approve`.
    pub fn reviewer_config(&self) -> ReviewerConfig {
        let mut c = ReviewerConfig::new(self.reviewer_info(), self.max_tokens);
        c.enabled = self.enabled;
        c.authority = self.approve;
        c.independence = self.independence;
        c.max_diff_bytes = self.max_diff_bytes;
        c.min_confidence = self.min_confidence;
        c
    }
}

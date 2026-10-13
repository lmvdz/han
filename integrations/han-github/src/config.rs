//! The config file (TOML): the node to talk to, the space, the repositories and the App's credentials path.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Why a config could not be used.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("reading the config: {0}")]
    Read(std::io::ErrorKind),
    /// The file is not valid TOML of the expected shape.
    #[error("parsing the config: {0}")]
    Parse(String),
    /// A value is out of range or malformed.
    #[error("invalid config: {0}")]
    Invalid(String),
}

/// The whole config.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The node's endpoint (a pipe name or socket path, see `han_node::rpc::default_endpoint`). Absent: the
    /// default endpoint of `data_dir`.
    pub endpoint: Option<String>,
    /// The data directory of the device's node, when `endpoint` is absent.
    pub data_dir: Option<PathBuf>,
    /// The space issues are surfaced in.
    pub space: String,
    /// The GitHub App, used for reads only.
    pub app: App,
    /// Seconds between polls of a repository (default 60, at least 10).
    #[serde(default = "default_interval")]
    pub poll_secs: u64,
    /// The most pages one poll follows (100 issues each; default 5).
    #[serde(default = "default_pages")]
    pub max_pages: u32,
    /// The repositories.
    pub repos: Vec<String>,
    /// What turns an issue into a task suggestion.
    #[serde(default)]
    pub tasks: Tasks,
}

/// The GitHub App's credentials. The private key stays on disk at `private_key`; the path is the only thing
/// the config (and any log) says about it.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct App {
    /// The App id.
    pub app_id: u64,
    /// The installation id.
    pub installation_id: u64,
    /// The PEM private key file.
    pub private_key: String,
    /// The API base (default `https://api.github.com`; GitHub Enterprise or a mock).
    pub api_base: Option<String>,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("app_id", &self.app_id)
            .field("installation_id", &self.installation_id)
            .field("private_key", &"<path not shown>")
            .finish()
    }
}

/// When an issue becomes a `TaskCreate`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tasks {
    /// The label that asks for a task (default `agent-ready`).
    #[serde(default = "default_label")]
    pub label: String,
    /// The team's shared agent pool (actor ids of agents or services, such as runner agents): an
    /// `agent-ready` issue becomes a task addressed to the pool, and any one pool agent claims it with its
    /// own signed task. Empty (and no `to`): no task is ever created (issues are only surfaced).
    #[serde(default)]
    pub pool: Vec<String>,
    /// Extra addressees (actor ids), merged into `pool`: a task goes to both lists, without duplicates.
    #[serde(default)]
    pub to: Vec<String>,
}

impl Tasks {
    /// Whom a task is addressed to: `pool`, then `to`, each actor once.
    pub fn addressees(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for a in self.pool.iter().chain(&self.to) {
            if !out.contains(a) {
                out.push(a.clone());
            }
        }
        out
    }
}

impl Default for Tasks {
    fn default() -> Self {
        Tasks {
            label: default_label(),
            pool: Vec::new(),
            to: Vec::new(),
        }
    }
}

fn default_interval() -> u64 {
    60
}
fn default_pages() -> u32 {
    5
}
fn default_label() -> String {
    "agent-ready".into()
}

impl Config {
    /// Reads and checks the file at `path`.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Read(e.kind()))?;
        Config::parse(&text)
    }

    /// Parses and checks config text.
    pub fn parse(text: &str) -> Result<Config, ConfigError> {
        let c: Config =
            toml::from_str(text).map_err(|e| ConfigError::Parse(e.message().to_owned()))?;
        c.check()?;
        Ok(c)
    }

    fn check(&self) -> Result<(), ConfigError> {
        if self.poll_secs < 10 {
            return Err(ConfigError::Invalid("poll_secs is at least 10".into()));
        }
        if self.max_pages == 0 || self.max_pages > 50 {
            return Err(ConfigError::Invalid("max_pages is 1 to 50".into()));
        }
        if self.repos.is_empty() {
            return Err(ConfigError::Invalid("no repositories".into()));
        }
        for r in &self.repos {
            if !valid_repo(r) {
                return Err(ConfigError::Invalid(format!("not owner/repo: {r}")));
            }
        }
        if self.endpoint.is_none() && self.data_dir.is_none() {
            return Err(ConfigError::Invalid(
                "one of endpoint and data_dir is required".into(),
            ));
        }
        Ok(())
    }

    /// The node endpoint to connect to.
    pub fn node_endpoint(&self) -> String {
        match (&self.endpoint, &self.data_dir) {
            (Some(e), _) => e.clone(),
            (None, Some(d)) => han_node::rpc::default_endpoint(d),
            (None, None) => String::new(),
        }
    }
}

/// `owner/name` with the characters GitHub allows.
pub fn valid_repo(r: &str) -> bool {
    let ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    r.split_once('/').is_some_and(|(o, n)| ok(o) && ok(n))
}

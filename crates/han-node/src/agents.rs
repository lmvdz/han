//! What a node knows about launching agents on this machine, kept in its data directory so a change needs no
//! restart and `han agent start` needs no flags:
//!
//! * `harness/<name>.json` (written by `han agent setup-harness`): the node executable and the adapter
//!   package directory of an installed harness.
//! * `agents.json` (written by the runner's admin): the sandbox to use and, on Linux, the OS user an agent runs
//!   as (SR-83).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The directory inside an agent's data directory that holds its own harness login (the harness's config
/// directory, `CLAUDE_CONFIG_DIR` for Claude Code).
pub const LOGIN_DIR: &str = "claude-config";

/// Where the OS-user agents of a runner keep their state when `agents.json` does not say.
pub const DEFAULT_STATE_ROOT: &str = "/var/lib/han-agents";

/// An agent actor as a plain directory name.
pub fn safe_name(actor: &str) -> String {
    actor
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// A harness as `han agent setup-harness` installed it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessInstall {
    /// The driver name (`claude` or `pi`).
    pub harness: String,
    /// The node executable that runs the adapter.
    pub node: PathBuf,
    /// The adapter package's directory (`…/node_modules/<package>`).
    pub adapter_dir: PathBuf,
    /// The npm package.
    pub package: String,
    /// The exact version installed (M4 pin).
    pub version: String,
    /// The tarball's npm integrity (`sha512-…`).
    #[serde(default)]
    pub integrity: String,
}

impl HarnessInstall {
    /// Where the record of `harness` is, under the node's data directory.
    pub fn path(data_dir: &Path, harness: &str) -> PathBuf {
        data_dir.join("harness").join(format!("{harness}.json"))
    }

    /// Reads the record, if the harness is set up.
    pub fn load(data_dir: &Path, harness: &str) -> Result<Option<HarnessInstall>, String> {
        let path = Self::path(data_dir, harness);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map(Some)
                .map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Writes the record.
    pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        let path = Self::path(data_dir, &self.harness);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }
}

/// How one agent is launched.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentSettings {
    /// Linux: the OS user the agent's host runs as (SR-83). The node starts it through `han-agent@<user>`
    /// (see docs/runner.md) and never needs root.
    #[serde(default)]
    pub os_user: Option<String>,
    /// The agent's state: its key, sessions and login. Default `/var/lib/han-agents/<osUser>` for an OS-user
    /// agent, else `<data>/agents/<actor>`.
    #[serde(default)]
    pub state_dir: Option<PathBuf>,
    /// The sandbox for this agent, over the file's.
    #[serde(default)]
    pub sandbox: Option<String>,
    /// The toolchains this agent gets inside its sandbox, over the file's (`["rust"]`; `[]` for none).
    #[serde(default)]
    pub toolchains: Option<Vec<String>>,
}

/// `agents.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentsFile {
    /// The sandbox for every agent: `auto` (default), `mxc`, `bwrap` or `none`.
    #[serde(default)]
    pub sandbox: Option<String>,
    /// Toolchains every agent gets inside its sandbox: `["rust"]`. Each is read-only, with a per-agent
    /// writable cache; Rust also lets the egress proxy reach crates.io. Default none.
    #[serde(default)]
    pub toolchains: Vec<String>,
    /// Where the toolchains live, when not in the node user's home: `{"rust": {"rustupHome": "/opt/rust/rustup",
    /// "cargoBin": "/opt/rust/cargo/bin"}}` (Windows MSVC also takes `msvcDir` and `sdkLibDir`).
    #[serde(default)]
    pub toolchain_paths: BTreeMap<String, BTreeMap<String, PathBuf>>,
    /// Per-agent settings, by actor (`@coder:han.example`).
    #[serde(default)]
    pub agents: BTreeMap<String, AgentSettings>,
}

impl AgentsFile {
    /// Reads `<data_dir>/agents.json`; absent means defaults.
    pub fn load(data_dir: &Path) -> Result<AgentsFile, String> {
        let path = data_dir.join("agents.json");
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AgentsFile::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// The settings of `actor` (defaults when it has none).
    pub fn agent(&self, actor: &str) -> AgentSettings {
        self.agents.get(actor).cloned().unwrap_or_default()
    }

    /// The sandbox `actor` runs in.
    pub fn sandbox_of(&self, actor: &str) -> String {
        self.agents
            .get(actor)
            .and_then(|a| a.sandbox.clone())
            .or_else(|| self.sandbox.clone())
            .unwrap_or_else(|| "auto".into())
    }

    /// The toolchains `actor` gets: its own list, else the file's.
    pub fn toolchains_of(&self, actor: &str) -> Vec<String> {
        self.agents
            .get(actor)
            .and_then(|a| a.toolchains.clone())
            .unwrap_or_else(|| self.toolchains.clone())
    }

    /// The toolchain location overrides as `<toolchain>.<name>=<path>`, the form `han host` takes.
    pub fn toolchain_path_args(&self) -> Vec<String> {
        self.toolchain_paths
            .iter()
            .flat_map(|(tool, m)| {
                m.iter()
                    .map(move |(k, v)| format!("{tool}.{k}={}", v.display()))
            })
            .collect()
    }

    /// The agent's state directory: its sessions, taint store, node data and login directory.
    pub fn state_dir(&self, data_dir: &Path, actor: &str) -> PathBuf {
        let s = self.agent(actor);
        match (s.state_dir, s.os_user) {
            (Some(dir), _) => dir,
            (None, Some(user)) => Path::new(DEFAULT_STATE_ROOT).join(user),
            (None, None) => data_dir.join("agents").join(safe_name(actor)),
        }
    }

    /// Where the agent's keys are: the node's own key store, except for an OS-user agent, whose key sits in
    /// its state directory (the host runs as that user, who cannot read the node's).
    pub fn keys_dir(&self, data_dir: &Path, actor: &str) -> PathBuf {
        if self.agent(actor).os_user.is_some() {
            self.state_dir(data_dir, actor)
        } else {
            data_dir.to_path_buf()
        }
    }

    /// The agent's login directory.
    pub fn login_dir(&self, data_dir: &Path, actor: &str) -> PathBuf {
        self.state_dir(data_dir, actor).join(LOGIN_DIR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolchains_are_the_files_unless_the_agent_sets_its_own() {
        let file: AgentsFile = serde_json::from_str(
            r#"{
              "toolchains": ["rust"],
              "toolchainPaths": {"rust": {"rustupHome": "/opt/rust/rustup", "cargoBin": "/opt/rust/cargo/bin"}},
              "agents": {
                "@a:x": {"osUser": "han-agent1"},
                "@b:x": {"toolchains": []}
              }
            }"#,
        )
        .unwrap();
        assert_eq!(file.toolchains_of("@a:x"), ["rust"]);
        assert!(file.toolchains_of("@b:x").is_empty());
        assert_eq!(file.toolchains_of("@nobody:x"), ["rust"]);
        assert_eq!(
            file.toolchain_path_args(),
            [
                "rust.cargoBin=/opt/rust/cargo/bin",
                "rust.rustupHome=/opt/rust/rustup"
            ]
        );
        // The default is none, and an unknown key is still refused.
        assert!(AgentsFile::default().toolchains_of("@a:x").is_empty());
        assert!(serde_json::from_str::<AgentsFile>(r#"{"toolchain": []}"#).is_err());
    }
}

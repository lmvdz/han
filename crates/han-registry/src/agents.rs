//! What a machine knows about launching agents, kept in the node's data directory so a change needs no
//! restart and `han agent start` needs no flags:
//!
//! * `harness/<name>.json` (written by `han agent setup-harness`): the node executable and the adapter
//!   package directory of an installed harness.
//! * `agents.json` (written by `han agent add` and `han agent set`): the sandbox to use, the toolchains and, on
//!   Linux, the OS user an agent runs as (SR-83).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The directory inside an agent's data directory that holds its own harness login (the harness's config
/// directory, `CLAUDE_CONFIG_DIR` for Claude Code).
pub const LOGIN_DIR: &str = "claude-config";

/// Where the OS-user agents of a runner keep their state when `agents.json` does not say.
pub const DEFAULT_STATE_ROOT: &str = "/var/lib/han-agents";

/// Whether `user` is an OS user name `han` will use: the one rule for the supervisor, `han runner setup` and
/// `han agent add` (a plain portable name; the polkit rule narrows it further to `han-agent<N>`).
pub fn valid_os_user(user: &str) -> bool {
    let mut chars = user.chars();
    user.len() <= 32
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

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
    /// A driver file (TOML or JSON) for a harness that is not built in; `han host` runs it as `--harness`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<PathBuf>,
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

/// The toolchains one agent's sandbox gets (PLAN 6.4 Toolchains): the one typed value that travels from the
/// node to the host, whatever carries it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolchainSpec {
    /// Names: `rust`.
    #[serde(default)]
    pub enabled: Vec<String>,
    /// Location overrides, keyed `<toolchain>.<name>`: `rust.rustupHome`, `rust.cargoBin`, `rust.msvcDir`,
    /// `rust.sdkLibDir`.
    #[serde(default)]
    pub paths: BTreeMap<String, PathBuf>,
    /// The per-agent `CARGO_HOME` (default: in the session directory).
    #[serde(default)]
    pub cargo_home: Option<PathBuf>,
}

impl ToolchainSpec {
    /// Whether no toolchain is on.
    pub fn is_empty(&self) -> bool {
        self.enabled.is_empty()
    }

    /// Whether `name` is on.
    pub fn on(&self, name: &str) -> bool {
        self.enabled.iter().any(|n| n == name)
    }

    /// An override.
    pub fn path(&self, key: &str) -> Option<PathBuf> {
        self.paths.get(key).cloned()
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
    /// Allow this agent to run with no OS sandbox when none is available (advisory, SR-23). It shows in status.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_advisory: bool,
    /// The harness `han agent start` uses when it is not given one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// The toolchains this agent gets inside its sandbox, over the file's (`["rust"]`; `[]` for none).
    #[serde(default)]
    pub toolchains: Option<Vec<String>>,
    /// API-key mode: a file (readable only by this user) holding the model key. The key stays in the host's
    /// proxy; the agent never sees it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_key_file: Option<PathBuf>,
}

/// A change to one agent's settings (`han agent add`, `han agent set`): what is `Some` is set, the rest stays.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentEdit {
    /// The OS user the agent's host runs as.
    pub os_user: Option<String>,
    /// The sandbox: `auto`, `mxc`, `bwrap` or `none`.
    pub sandbox: Option<String>,
    /// Whether the agent may run with no OS sandbox.
    pub allow_advisory: Option<bool>,
    /// The harness `han agent start` uses by default.
    pub harness: Option<String>,
    /// The toolchains inside its sandbox (`[]` for none).
    pub toolchains: Option<Vec<String>>,
    /// The file holding the model key (API-key mode).
    pub model_key_file: Option<PathBuf>,
}

/// The sandboxes `han` knows.
pub const SANDBOXES: [&str; 4] = ["auto", "mxc", "bwrap", "none"];

/// The toolchains `han` knows.
pub const TOOLCHAINS: [&str; 1] = ["rust"];

impl AgentEdit {
    /// Refuses a value no launch could use, so a typo is found now and not at the next start.
    pub fn check(&self) -> Result<(), String> {
        if let Some(u) = &self.os_user
            && !valid_os_user(u)
        {
            return Err(format!("{u:?} is not an OS user name"));
        }
        if let Some(s) = &self.sandbox
            && !SANDBOXES.contains(&s.as_str())
        {
            return Err(format!("unknown sandbox {s:?}: auto, mxc, bwrap or none"));
        }
        for t in self.toolchains.iter().flatten() {
            if !TOOLCHAINS.contains(&t.as_str()) {
                return Err(format!("unknown toolchain {t:?}: rust"));
            }
        }
        Ok(())
    }
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

    /// Writes `<data_dir>/agents.json`.
    pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(
            data_dir.join("agents.json"),
            format!(
                "{text}
"
            ),
        )
    }

    /// Applies `edit` to the settings of `actor`; only what the edit names changes. Says whether anything did.
    pub fn set(&mut self, actor: &str, edit: &AgentEdit) -> Result<bool, String> {
        edit.check()?;
        let before = self.agents.get(actor).cloned();
        let mut now = before.clone().unwrap_or_default();
        if let Some(u) = &edit.os_user {
            now.os_user = Some(u.clone());
        }
        if let Some(s) = &edit.sandbox {
            now.sandbox = Some(s.clone());
        }
        if let Some(a) = edit.allow_advisory {
            now.allow_advisory = a;
        }
        if let Some(h) = &edit.harness {
            now.harness = Some(h.clone());
        }
        if let Some(t) = &edit.toolchains {
            now.toolchains = Some(t.clone());
        }
        if let Some(f) = &edit.model_key_file {
            now.model_key_file = Some(f.clone());
        }
        if before.as_ref() == Some(&now) || (before.is_none() && now == AgentSettings::default()) {
            return Ok(false);
        }
        self.agents.insert(actor.to_owned(), now);
        Ok(true)
    }

    /// Turns the toolchain `tool` on for every agent and records where it lives (`han runner setup --toolchain`):
    /// the file's own list and location overrides, not an agent's. Says whether anything changed.
    pub fn set_toolchain(
        &mut self,
        tool: &str,
        paths: BTreeMap<String, PathBuf>,
    ) -> Result<bool, String> {
        if !TOOLCHAINS.contains(&tool) {
            return Err(format!("unknown toolchain {tool:?}: rust"));
        }
        let mut changed = false;
        if !self.toolchains.iter().any(|t| t == tool) {
            self.toolchains.push(tool.to_owned());
            changed = true;
        }
        if self.toolchain_paths.get(tool) != Some(&paths) {
            self.toolchain_paths.insert(tool.to_owned(), paths);
            changed = true;
        }
        Ok(changed)
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

    /// The toolchains `actor` gets, with the file's location overrides and a `CARGO_HOME` in its state.
    pub fn toolchain_spec(&self, data_dir: &Path, actor: &str) -> ToolchainSpec {
        ToolchainSpec {
            enabled: self.toolchains_of(actor),
            paths: self
                .toolchain_paths
                .iter()
                .flat_map(|(tool, m)| {
                    m.iter()
                        .map(move |(k, v)| (format!("{tool}.{k}"), v.clone()))
                })
                .collect(),
            cargo_home: Some(self.state_dir(data_dir, actor).join("cargo-home")),
        }
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
        let spec = file.toolchain_spec(Path::new("/d"), "@a:x");
        assert_eq!(
            spec.path("rust.rustupHome"),
            Some("/opt/rust/rustup".into())
        );
        assert!(spec.cargo_home.unwrap().ends_with("cargo-home"));
        // The default is none, and an unknown key is still refused.
        assert!(AgentsFile::default().toolchains_of("@a:x").is_empty());
        assert!(serde_json::from_str::<AgentsFile>(r#"{"toolchain": []}"#).is_err());
    }

    #[test]
    fn an_edit_changes_only_what_it_names_and_says_when_nothing_changed() {
        let mut f = AgentsFile::default();
        let edit = AgentEdit {
            os_user: Some("han-agent1".into()),
            toolchains: Some(vec!["rust".into()]),
            ..AgentEdit::default()
        };
        assert!(f.set("@a:x", &edit).unwrap());
        assert!(!f.set("@a:x", &edit).unwrap());
        let sandbox = AgentEdit {
            sandbox: Some("bwrap".into()),
            ..AgentEdit::default()
        };
        assert!(f.set("@a:x", &sandbox).unwrap());
        let a = f.agent("@a:x");
        assert_eq!(a.os_user.as_deref(), Some("han-agent1"));
        assert_eq!(a.toolchains, Some(vec!["rust".to_owned()]));
        // a value no launch could use is refused, and nothing is written
        let bad = AgentEdit {
            sandbox: Some("chroot".into()),
            ..AgentEdit::default()
        };
        assert!(f.set("@a:x", &bad).is_err());
        assert_eq!(f.agent("@a:x").sandbox.as_deref(), Some("bwrap"));
    }

    #[test]
    fn a_toolchain_is_turned_on_once_with_its_paths() {
        let mut f = AgentsFile::default();
        let paths: BTreeMap<String, PathBuf> = [
            ("rustupHome".to_owned(), PathBuf::from("/opt/rust/rustup")),
            ("cargoBin".to_owned(), PathBuf::from("/opt/rust/cargo/bin")),
        ]
        .into();
        assert!(f.set_toolchain("rust", paths.clone()).unwrap());
        assert!(!f.set_toolchain("rust", paths).unwrap());
        assert_eq!(f.toolchains, ["rust"]);
        assert!(f.set_toolchain("go", BTreeMap::new()).is_err());
    }
}

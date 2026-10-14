//! The `HostSpec`: everything a `han host` needs to run one agent session, as one value. The node makes it from
//! the registry and what it knows (the space's home, the agent's operator), writes it where the host reads it,
//! and `han host --spec <file|->` is the host's only input. No flags, no environment, no second path.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agents::ToolchainSpec;
use crate::home::HomeRef;

fn k1() -> String {
    "k1".into()
}

fn head() -> String {
    "HEAD".into()
}

fn main_branch() -> String {
    "main".into()
}

fn auto() -> String {
    "auto".into()
}

fn n0() -> String {
    "n0".into()
}

/// The harness: which driver runs the agent and where its program is installed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessSpec {
    /// The driver: `claude`, `pi`, or the path of a driver file (TOML or JSON) when none is built in.
    pub name: String,
    /// A driver file that stands in for the built-in driver of `name` (a harness record can name one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<PathBuf>,
    /// The node executable that runs a node-based adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<PathBuf>,
    /// The adapter's installed directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_dir: Option<PathBuf>,
}

/// The work of the session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkSpec {
    /// The repository: `github:owner/repo`, an `https://` URL, or a local path (default: the working directory).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The base to start from (branch, `HEAD` or commit).
    #[serde(default = "head")]
    pub base: String,
    /// The branch pull requests target.
    #[serde(default = "main_branch")]
    pub default_branch: String,
    /// The issue, `github:owner/repo#123`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
    /// A task to work instead of finding or claiming the issue's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The executor that runs approved effects (default `@exec:<home server>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
    /// Run the session guarded.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub guarded: bool,
}

impl Default for WorkSpec {
    fn default() -> Self {
        WorkSpec {
            repo: None,
            base: head(),
            default_branch: main_branch(),
            issue: None,
            task: None,
            executor: None,
            guarded: false,
        }
    }
}

/// How confined the agent is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxPolicy {
    /// `auto`, `mxc`, `bwrap` or `none`.
    #[serde(default = "auto")]
    pub backend: String,
    /// Allow running without an OS sandbox when none is available (advisory; it shows in status).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_advisory: bool,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        SandboxPolicy {
            backend: auto(),
            allow_advisory: false,
        }
    }
}

/// The egress proxy of the session.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProxySpec {
    /// Hosts the agent may reach on top of its driver's and its toolchains'.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_hosts: Vec<String>,
    /// API-key mode: a file (readable only by this user) holding the model key. The key stays in the proxy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_key_file: Option<PathBuf>,
}

/// Where the agent's files are.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Dirs {
    /// The agent's state: its node, sessions, taint store and cargo cache.
    pub state: PathBuf,
    /// Where the key store is (`<keys>/keys` for file keys; else the OS keychain).
    pub keys: PathBuf,
    /// The agent's own login directory (the harness's config directory), bound read-write into this agent's
    /// sandbox only.
    pub login: PathBuf,
}

/// Everything one `han host` needs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostSpec {
    /// The space.
    pub space: String,
    /// The agent actor.
    pub agent: String,
    /// The agent's key id in its profile.
    #[serde(default = "k1")]
    pub key_id: String,
    /// The agent's operator (its guard when guarded), as its profile names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,
    /// The harness.
    pub harness: HarnessSpec,
    /// The space's home: the host's own node follows the space there.
    pub home: HomeRef,
    /// The work.
    #[serde(default)]
    pub work: WorkSpec,
    /// The sandbox.
    #[serde(default)]
    pub sandbox: SandboxPolicy,
    /// The toolchains inside the sandbox.
    #[serde(default)]
    pub toolchains: ToolchainSpec,
    /// The egress proxy.
    #[serde(default)]
    pub proxy: ProxySpec,
    /// The directories.
    pub dirs: Dirs,
    /// Relays: `n0`, `none`, or URLs.
    #[serde(default = "n0")]
    pub relay: String,
    /// The host endpoint of the node that started this host: the one connection class a host may use to talk
    /// to that node (agent commands only, SR-107). The spawner fills it; it is absent when the host was
    /// started by hand or runs as another OS user, who cannot reach the owner-only pipe. A host is never
    /// given the face endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_endpoint: Option<String>,
}

impl HostSpec {
    /// Reads a spec from `file`, or from standard input when it is `-`.
    pub fn read(file: &str) -> Result<HostSpec, String> {
        let text = if file == "-" {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)
                .map_err(|e| format!("stdin: {e}"))?;
            s
        } else {
            std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?
        };
        serde_json::from_str(&text).map_err(|e| format!("{file}: not a host spec: {e}"))
    }

    /// Writes the spec to `path` as JSON, creating its directory.
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }
}

/// The file a spec is left in for the host called `name`, under the agent's state directory.
pub fn spec_file(state_dir: &Path, name: &str) -> PathBuf {
    state_dir
        .join("spawn")
        .join(format!("{}.json", crate::agents::safe_name(name)))
}

/// The file the unit `han-agent@<user>` reads: the one host of an OS user.
pub fn unit_spec_file(state_dir: &Path) -> PathBuf {
    state_dir.join("spawn").join("host.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> HostSpec {
        HostSpec {
            space: "s1:han.example".into(),
            agent: "@coder:han.example".into(),
            key_id: "k1".into(),
            operator: Some("@lars:han.example".into()),
            harness: HarnessSpec {
                name: "claude".into(),
                driver: Some("/opt/han/claude.toml".into()),
                node: Some("/opt/node22/bin/node".into()),
                adapter_dir: Some("/opt/han/claude/node_modules/@agentclientprotocol/x".into()),
            },
            home: HomeRef::Ticket("han1home.abc".into()),
            work: WorkSpec {
                repo: Some("github:o/r".into()),
                base: "main".into(),
                default_branch: "trunk".into(),
                issue: Some("github:o/r#1".into()),
                task: Some("t1".into()),
                executor: Some("@exec:han.example".into()),
                guarded: true,
            },
            sandbox: SandboxPolicy {
                backend: "bwrap".into(),
                allow_advisory: true,
            },
            toolchains: ToolchainSpec {
                enabled: vec!["rust".into()],
                paths: [(
                    "rust.rustupHome".to_owned(),
                    PathBuf::from("/opt/rust/rustup"),
                )]
                .into(),
                cargo_home: Some("/var/lib/han-agents/a/cargo-home".into()),
            },
            proxy: ProxySpec {
                allow_hosts: vec!["example.org".into()],
                model_key_file: Some("/etc/han/model.key".into()),
            },
            dirs: Dirs {
                state: "/var/lib/han-agents/a".into(),
                keys: "/var/lib/han-agents/a".into(),
                login: "/var/lib/han-agents/a/claude-config".into(),
            },
            relay: "none".into(),
            node_endpoint: Some("/run/han/node.sock-host".into()),
        }
    }

    #[test]
    fn a_full_spec_survives_json() {
        let spec = full();
        let text = serde_json::to_string(&spec).unwrap();
        assert_eq!(serde_json::from_str::<HostSpec>(&text).unwrap(), spec);
    }

    #[test]
    fn a_spec_survives_a_file() {
        let dir = std::env::temp_dir().join(format!("han-spec-{}", std::process::id()));
        let file = spec_file(&dir, "host-@coder:han.example#github:o/r#1");
        full().write(&file).unwrap();
        assert!(file.starts_with(dir.join("spawn")));
        assert_eq!(HostSpec::read(&file.to_string_lossy()).unwrap(), full());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_small_spec_gets_the_defaults() {
        let spec: HostSpec = serde_json::from_str(
            r#"{
              "space": "s1:han.example",
              "agent": "@coder:han.example",
              "harness": {"name": "pi"},
              "home": {"server": "han.example"},
              "dirs": {"state": "a", "keys": "k", "login": "l"}
            }"#,
        )
        .unwrap();
        assert_eq!(spec.key_id, "k1");
        assert_eq!(spec.work, WorkSpec::default());
        assert_eq!(spec.work.base, "HEAD");
        assert_eq!(spec.work.default_branch, "main");
        assert_eq!(spec.sandbox, SandboxPolicy::default());
        assert_eq!(spec.sandbox.backend, "auto");
        assert!(spec.toolchains.is_empty());
        assert_eq!(spec.relay, "n0");
        // and what it prints reads back the same
        let again: HostSpec = serde_json::from_str(&serde_json::to_string(&spec).unwrap()).unwrap();
        assert_eq!(again, spec);
    }

    #[test]
    fn an_unknown_field_is_refused() {
        let text = serde_json::to_string(&full())
            .unwrap()
            .replacen('{', r#"{"flag":1,"#, 1);
        assert!(serde_json::from_str::<HostSpec>(&text).is_err());
        let nested = serde_json::to_string(&full())
            .unwrap()
            .replace(r#""backend""#, r#""backends""#);
        assert!(serde_json::from_str::<HostSpec>(&nested).is_err());
    }
}

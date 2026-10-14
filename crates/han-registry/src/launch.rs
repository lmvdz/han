//! From a request to start an agent to the [`HostSpec`] that runs it: the one place that reads `agents.json`
//! and the harness records. The node asks; it never looks at either file.

use std::path::{Path, PathBuf};

use crate::agents::{AgentsFile, HarnessInstall};
use crate::home::HomeRef;
use crate::spec::{Dirs, HarnessSpec, HostSpec, ProxySpec, SandboxPolicy, WorkSpec};

/// What a node knows when it starts an agent.
#[derive(Clone, Debug)]
pub struct Launch {
    /// The space.
    pub space: String,
    /// The agent actor.
    pub agent: String,
    /// The agent's operator, as its profile names it.
    pub operator: String,
    /// The harness; the agent's recorded one when `None`.
    pub harness: Option<String>,
    /// The repository.
    pub repo: Option<String>,
    /// The issue.
    pub issue: Option<String>,
    /// The space's home, as the host's node reaches it.
    pub home: HomeRef,
    /// The executor of approved effects.
    pub executor: String,
}

/// Another OS user to run the host as. The node never needs root for it: the runner's admin installs the
/// `han-agent@.service` template once (docs/runner.md), and the node starts and stops that unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunAs {
    /// The OS user (the instance of the template).
    pub os_user: String,
    /// The user's state directory, where the node leaves the spec for the unit to read.
    pub state_dir: PathBuf,
}

/// A launch that can go ahead: the spec, and where it runs.
#[derive(Clone, Debug)]
pub struct Resolved {
    /// What the host reads.
    pub spec: HostSpec,
    /// Another OS user to run it as, if the agent is recorded to run as one.
    pub run_as: Option<RunAs>,
}

/// Why a launch cannot go ahead.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    /// The request or a file of the registry is wrong; the same request again fails the same way.
    #[error("{0}")]
    Invalid(String),
    /// This machine is not set up for it yet; the message says what to run.
    #[error("{0}")]
    Unavailable(String),
}

/// The harness name as a record file knows it.
fn record_name(harness: &str) -> &str {
    match harness {
        "claude-code" => "claude",
        other => other,
    }
}

/// The registry of one node: its data directory.
#[derive(Clone, Debug)]
pub struct Registry {
    data_dir: PathBuf,
}

impl Registry {
    /// The registry kept in `data_dir`.
    pub fn open(data_dir: impl Into<PathBuf>) -> Registry {
        Registry {
            data_dir: data_dir.into(),
        }
    }

    /// The node's data directory.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// `agents.json`.
    pub fn agents(&self) -> Result<AgentsFile, String> {
        AgentsFile::load(&self.data_dir)
    }

    /// Where the key of `actor` is kept: under its own state directory when it runs as another OS user (who
    /// cannot read the node's keys), else `None` (the node's own key store).
    pub fn own_key_dir(&self, actor: &str) -> Result<Option<PathBuf>, String> {
        let file = self.agents()?;
        Ok(file
            .agent(actor)
            .os_user
            .is_some()
            .then(|| file.keys_dir(&self.data_dir, actor).join("keys")))
    }

    /// The harness recorded for `agent`, if any.
    pub fn default_harness(&self, agent: &str) -> Result<Option<String>, String> {
        Ok(self.agents()?.agent(agent).harness)
    }

    /// Whether the harness `name` is set up.
    pub fn harness_set_up(&self, name: &str) -> Result<bool, String> {
        Ok(HarnessInstall::load(&self.data_dir, record_name(name))?.is_some())
    }

    /// Makes the [`HostSpec`] for `launch` from the files of the registry.
    pub fn resolve(&self, launch: &Launch) -> Result<Resolved, LaunchError> {
        let file = self.agents().map_err(LaunchError::Invalid)?;
        let dir = self.data_dir.as_path();
        let agent = launch.agent.as_str();
        let state = file.state_dir(dir, agent);
        let settings = file.agent(agent);
        let harness_name = launch
            .harness
            .clone()
            .or(settings.harness.clone())
            .ok_or_else(|| {
                LaunchError::Invalid(
                    "give a harness (or record one with `han agent set --harness`)".into(),
                )
            })?;
        let key = record_name(&harness_name);
        // A driver file given as the harness is a path, not a record's name.
        let named = key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        let install = if named {
            HarnessInstall::load(dir, key).map_err(LaunchError::Invalid)?
        } else {
            None
        };
        let harness = match install {
            Some(h) => HarnessSpec {
                name: harness_name,
                driver: h.driver,
                node: Some(h.node),
                adapter_dir: Some(h.adapter_dir),
            },
            None if matches!(key, "claude" | "pi") => {
                return Err(LaunchError::Unavailable(format!(
                    "the {key} harness is not set up on this node: run `han agent setup-harness {key}`"
                )));
            }
            None => HarnessSpec {
                name: harness_name,
                driver: None,
                node: None,
                adapter_dir: None,
            },
        };
        let run_as = match settings.os_user {
            Some(os_user) => {
                if !cfg!(target_os = "linux") {
                    return Err(LaunchError::Unavailable(
                        "running an agent as another OS user is for Linux runners".into(),
                    ));
                }
                Some(RunAs {
                    os_user,
                    state_dir: state.clone(),
                })
            }
            None => None,
        };
        let spec = HostSpec {
            space: launch.space.clone(),
            agent: launch.agent.clone(),
            key_id: "k1".into(),
            operator: Some(launch.operator.clone()),
            harness,
            home: launch.home.clone(),
            work: WorkSpec {
                repo: launch.repo.clone(),
                issue: launch.issue.clone(),
                executor: Some(launch.executor.clone()),
                ..WorkSpec::default()
            },
            sandbox: SandboxPolicy {
                backend: file.sandbox_of(agent),
                allow_advisory: settings.allow_advisory,
            },
            toolchains: file.toolchain_spec(dir, agent),
            proxy: ProxySpec::default(),
            dirs: Dirs {
                keys: file.keys_dir(dir, agent),
                login: file.login_dir(dir, agent),
                state,
            },
            relay: std::env::var("HAN_RELAY").unwrap_or_else(|_| "n0".into()),
        };
        Ok(Resolved { spec, run_as })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("han-reg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn launch(harness: &str) -> Launch {
        Launch {
            space: "s1:a.localhost".into(),
            agent: "@coder:a.localhost".into(),
            operator: "@alice:a.localhost".into(),
            harness: Some(harness.into()),
            repo: Some("github:o/r".into()),
            issue: Some("github:o/r#1".into()),
            home: HomeRef::Server("a.localhost".into()),
            executor: "@exec:a.localhost".into(),
        }
    }

    #[test]
    fn a_built_in_harness_must_be_set_up_and_says_what_to_run() {
        let d = dir("setup");
        let r = Registry::open(&d);
        let Err(LaunchError::Unavailable(m)) = r.resolve(&launch("pi")) else {
            panic!("expected unavailable")
        };
        assert!(m.contains("han agent setup-harness pi"), "{m}");
        // a driver file named as the harness needs no record
        let ok = r.resolve(&launch("/etc/han/driver.toml")).unwrap();
        assert_eq!(ok.spec.harness.name, "/etc/han/driver.toml");
        assert_eq!(ok.spec.sandbox.backend, "auto");
        assert!(ok.run_as.is_none());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn the_install_and_the_agents_file_end_up_in_the_spec() {
        let d = dir("install");
        HarnessInstall {
            harness: "claude".into(),
            node: "/opt/node22/bin/node".into(),
            adapter_dir: "/opt/han/claude/node_modules/x".into(),
            package: "x".into(),
            version: "1".into(),
            integrity: String::new(),
            driver: None,
        }
        .save(&d)
        .unwrap();
        std::fs::write(
            d.join("agents.json"),
            r#"{"sandbox":"bwrap","toolchains":["rust"],"agents":{"@coder:a.localhost":{"allowAdvisory":true,"harness":"claude"}}}"#,
        )
        .unwrap();
        let r = Registry::open(&d);
        let mut l = launch("claude");
        l.harness = None; // the agent's recorded one
        let got = r.resolve(&l).unwrap().spec;
        assert_eq!(got.harness.name, "claude");
        assert_eq!(
            got.harness.adapter_dir.as_deref(),
            Some(Path::new("/opt/han/claude/node_modules/x"))
        );
        assert_eq!(got.sandbox.backend, "bwrap");
        assert!(got.sandbox.allow_advisory);
        assert_eq!(got.toolchains.enabled, ["rust"]);
        assert_eq!(got.operator.as_deref(), Some("@alice:a.localhost"));
        assert!(got.dirs.login.ends_with(LOGIN_DIR_NAME));
        // no harness anywhere: invalid, not a guess
        let mut none = launch("x");
        none.harness = None;
        none.agent = "@other:a.localhost".into();
        assert!(matches!(r.resolve(&none), Err(LaunchError::Invalid(_))));
        let _ = std::fs::remove_dir_all(d);
    }

    const LOGIN_DIR_NAME: &str = crate::agents::LOGIN_DIR;
}

//! From a request to start an agent to the [`HostSpec`] that runs it: the one place that reads `agents.json`
//! and the harness records. The node asks; it never looks at either file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::agents::{AgentsFile, HarnessInstall};
use crate::home::HomeRef;
use crate::spec::{
    BudgetSpec, Dirs, FinishSpec, HarnessSpec, HostSpec, PriceSpec, ProxySpec, SandboxPolicy,
    WorkSpec,
};

/// The file of model prices in a node's data directory.
pub const PRICES_FILE: &str = "prices.json";

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
    /// The task to work: the open task for the issue, which carries the issue's text.
    pub task: Option<String>,
    /// The space's home, as the host's node reaches it.
    pub home: HomeRef,
    /// The executor of approved effects.
    pub executor: String,
    /// The space's budget (unit and amount), if its policy has one.
    pub budget: Option<(String, u64)>,
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

    /// The model prices this node uses for budgets in `unit`, from `prices.json` in the data directory:
    /// `{"<unit>": {"<model>": {"input": …, "output": …, "cacheRead": …, "cacheCreation": …}}}`, in the unit
    /// per million tokens. A unit with no entry (or no file) has none, which is not an error: the budget then
    /// shows as not enforced.
    pub fn prices(&self, unit: &str) -> Result<BTreeMap<String, PriceSpec>, String> {
        let path = self.data_dir.join(PRICES_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let mut all: BTreeMap<String, BTreeMap<String, PriceSpec>> =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(all.remove(unit).unwrap_or_default())
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
        let named = crate::agents::valid_harness_name(key);
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
            None if named && !key.ends_with(".json") && !key.ends_with(".toml") => {
                return Err(LaunchError::Unavailable(format!(
                    "the {key} harness is not set up on this node: run `han agent setup-harness {key}` (pass its driver file if it is not bundled)"
                )));
            }
            None => HarnessSpec {
                name: harness_name,
                driver: None,
                node: None,
                adapter_dir: None,
            },
        };
        let model_key_file = settings.model_key_file.clone();
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
        let budget = match &launch.budget {
            Some((unit, limit)) => Some(BudgetSpec {
                unit: unit.clone(),
                limit: *limit,
                prices: self.prices(unit).map_err(LaunchError::Invalid)?,
            }),
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
                task: launch.task.clone(),
                executor: Some(launch.executor.clone()),
                finish: FinishSpec {
                    land: file.land_for(&launch.space, launch.repo.as_deref()),
                },
                ..WorkSpec::default()
            },
            sandbox: SandboxPolicy {
                backend: file.sandbox_of(agent),
                allow_advisory: settings.allow_advisory,
            },
            toolchains: file.toolchain_spec(dir, agent),
            proxy: ProxySpec {
                model_key_file,
                ..ProxySpec::default()
            },
            dirs: Dirs {
                keys: file.keys_dir(dir, agent),
                login: file.login_dir(dir, agent),
                state,
            },
            relay: std::env::var("HAN_RELAY").unwrap_or_else(|_| "n0".into()),
            node_endpoint: None,
            budget,
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
            task: None,
            home: HomeRef::Server("a.localhost".into()),
            executor: "@exec:a.localhost".into(),
            budget: None,
        }
    }

    #[test]
    fn the_space_budget_and_the_nodes_prices_end_up_in_the_spec() {
        let d = dir("budget");
        let r = Registry::open(&d);
        let mut l = launch("/etc/han/driver.toml");
        // no budget: none in the spec
        assert_eq!(r.resolve(&l).unwrap().spec.budget, None);
        // a budget and no price file: carried with no prices (shown as not enforced)
        l.budget = Some(("USD-cent".into(), 500));
        let b = r.resolve(&l).unwrap().spec.budget.unwrap();
        assert_eq!(
            (b.unit.as_str(), b.limit, b.prices.len()),
            ("USD-cent", 500, 0)
        );
        // prices are per unit: another unit's table does not apply
        std::fs::write(
            d.join(PRICES_FILE),
            r#"{"USD-cent":{"claude-sonnet":{"input":300,"output":1500}},"EUR-cent":{"x":{"input":1}}}"#,
        )
        .unwrap();
        let b = r.resolve(&l).unwrap().spec.budget.unwrap();
        assert_eq!(b.prices.len(), 1);
        assert_eq!(b.prices["claude-sonnet"].output, 1500);
        l.budget = Some(("GBP".into(), 5));
        assert!(
            r.resolve(&l)
                .unwrap()
                .spec
                .budget
                .unwrap()
                .prices
                .is_empty()
        );
        // a malformed table is an error, not a silent no-budget
        std::fs::write(d.join(PRICES_FILE), "{").unwrap();
        assert!(matches!(r.resolve(&l), Err(LaunchError::Invalid(_))));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_built_in_harness_must_be_set_up_and_says_what_to_run() {
        let d = dir("setup");
        let r = Registry::open(&d);
        let Err(LaunchError::Unavailable(m)) = r.resolve(&launch("pi")) else {
            panic!("expected unavailable")
        };
        assert!(m.contains("han agent setup-harness pi"), "{m}");
        let Err(LaunchError::Unavailable(m)) = r.resolve(&launch("custom-adapter")) else {
            panic!("an uninstalled named adapter is unavailable")
        };
        assert!(m.contains("han agent setup-harness custom-adapter"));
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

    #[test]
    fn the_finish_flow_of_the_space_and_the_repository_ends_up_in_the_spec() {
        let d = dir("finish");
        let r = Registry::open(&d);
        let l = launch("/etc/han/driver.toml");
        // off unless set: the spec does not even mention it
        assert_eq!(
            r.resolve(&l).unwrap().spec.work.finish,
            FinishSpec::default()
        );
        std::fs::write(
            d.join("agents.json"),
            r#"{"finish":{"s1:a.localhost":{
                "land":{"enabled":true,"targetRef":"refs/heads/main","tierRules":[{"path":"docs/**","tier":0}]},
                "repos":{"github:o/r":{"settleSecs":300,"tierRules":[{"path":"crates/core/**","tier":3}]}}}}}"#,
        )
        .unwrap();
        let land = r.resolve(&l).unwrap().spec.work.finish.land.unwrap();
        assert_eq!(land.target_ref.as_deref(), Some("refs/heads/main"));
        assert_eq!(land.settle_secs, 300);
        assert_eq!(land.tier_rules.len(), 2);
        // a repository the space does not name gets the space's
        let mut other = launch("/etc/han/driver.toml");
        other.repo = Some("github:o/other".into());
        assert_eq!(
            r.resolve(&other)
                .unwrap()
                .spec
                .work
                .finish
                .land
                .unwrap()
                .settle_secs,
            0
        );
        // another space has none
        other.space = "s2:a.localhost".into();
        assert_eq!(r.resolve(&other).unwrap().spec.work.finish.land, None);
        let _ = std::fs::remove_dir_all(d);
    }

    const LOGIN_DIR_NAME: &str = crate::agents::LOGIN_DIR;
}

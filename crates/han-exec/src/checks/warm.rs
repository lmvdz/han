//! The warm-runner checks backend: the executor runs a repo's checks itself, in a persistent working tree with a
//! persistent build directory, so a change rebased onto the last green tree rebuilds only what it touched
//! (r33-land-profile.md §4 "Two stages", "Where checks run", "Evidence"; PLAN.md §12).
//!
//! # What it does
//!
//! * One working tree per (repo, lane) under the executor's private `state_dir`. It is a `git worktree` of the
//!   executor's own repository, so it shares that object store (no clone per run). Every run checks it out to the
//!   requested commit with a forced, clean checkout (`checkout --force`, `reset --hard`, `clean -ffdx`). The build
//!   directory lives outside the tree (`CARGO_TARGET_DIR`, one per lane) and is never cleaned.
//! * The steps of each [`Stage`] come from a per-repo [`Profile`]; [`Profile::han`] mirrors `.github/workflows/ci.yml`.
//! * The scope of a change is computed by [`select_scope`] from `cargo metadata`: the changed crates plus their
//!   reverse dependencies, and everything for a path outside a crate (workspace manifest, lockfile, toolchain, CI).
//! * One run at a time per lane, at most [`WarmConfig::max_concurrent`] across the executor.
//!
//! # Security
//!
//! Checks run the code of the commit, so the runner refuses anything that is not a plain commit id: the commit comes
//! from the lane (an approved, rebased change), never from a PR or a fork, and a ref name or an option-looking string
//! is rejected before git sees it.
//!
//! * Environment: the process gets a scrubbed environment ([`scrubbed_env`]): `PATH`, `RUSTUP_HOME`, `CARGO_HOME`,
//!   the target dir, a per-lane temp dir, and a short list of OS and MSVC variables the toolchain needs. No executor
//!   credential, GitHub token or HAN key reaches it. Extra variables from config are refused when their name looks
//!   like a secret.
//! * Network: cargo needs crates.io (index and downloads) for `--locked` fetches the first time a lockfile entry
//!   changes; the warm `CARGO_HOME` normally has them. A first run on a new toolchain pin also lets rustup download it.
//!   Nothing else needs the network.
//! * Isolation: the [`StepLauncher`] seam builds each command. [`Direct`] runs it as a plain child process, which
//!   trusts approved code (gap: it can read and write anything the executor's OS user can). `han-exec` does not
//!   depend on `han-host`, so a sandbox adapter lives on the host side: it implements [`StepLauncher`] with
//!   `han_host::sandbox::launch`, binding [`StepLaunch::rw`] writable and [`StepLaunch::ro`] read-only. `ro` already
//!   lists the repo's git directory and every object directory named by an alternates file, which git needs inside
//!   the sandbox (without them git fails with `bad object HEAD`); the host's `glue::store_binds` computes the same.
//!
//! # Timeouts and logs
//!
//! Each step has a timeout and the whole run has one. A step that is still running then is killed with its process
//! tree and reported `timed_out` (so `ok` is false); the run never hangs. Each step's stdout and stderr go to one log
//! file under `state_dir/logs/<lane>/`, named in the evidence.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::process::Command;
use tokio::sync::Semaphore;

use super::{CheckError, CheckRequest, CheckRun, Checks, Stage};
use crate::{ChecksBackend, RepoTarget};

/// Why a warm run could not be carried out at all (a failing check is evidence, not an error).
#[derive(Debug, thiserror::Error)]
pub enum WarmError {
    /// The request is not one the runner will run (bad commit id, bad lane name, missing repository).
    #[error("refused: {0}")]
    Refused(String),
    /// A git command of the tree preparation failed.
    #[error("git: {0}")]
    Git(String),
    /// `cargo metadata` failed or gave something unexpected.
    #[error("cargo metadata: {0}")]
    Metadata(String),
    /// The configuration is not usable.
    #[error("config: {0}")]
    Config(String),
    /// The tree preparation did not finish within the run timeout.
    #[error("preparing the working tree timed out")]
    PrepareTimeout,
    /// Filesystem or process error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// The result of this module's operations.
pub type Result<T> = std::result::Result<T, WarmError>;

/// Which part of the repo a run checks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", content = "crates", rename_all = "snake_case")]
pub enum Scope {
    /// The whole workspace.
    Full,
    /// These workspace packages (the affected set, reverse dependencies included). Empty means no package.
    Crates(Vec<String>),
}

/// One run to carry out. The commit must come from the lane.
#[derive(Clone, Debug)]
pub struct WarmRequest {
    /// The executor's own repository (a path to a git repository, bare or not).
    pub repo: PathBuf,
    /// The lane's name; with `repo` it names the persistent working tree.
    pub lane: String,
    /// The exact commit to check, a full hex object id.
    pub commit: String,
    /// What to check.
    pub scope: Scope,
    /// Which step set to run.
    pub stage: Stage,
}

/// One step's outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepResult {
    /// The step's name from the profile.
    pub name: String,
    /// The command that ran (after scope expansion and any fallback).
    pub command: Vec<String>,
    /// The exit code, `None` when the step was killed or never exited normally.
    pub exit: Option<i32>,
    /// True when the step (or the run) hit its timeout and was killed.
    pub timed_out: bool,
    /// Wall time of the step.
    pub duration_ms: u64,
    /// The captured stdout and stderr; `None` for a synthetic step that never started.
    pub log: Option<PathBuf>,
}

impl StepResult {
    /// True when the step exited 0.
    pub fn passed(&self) -> bool {
        self.exit == Some(0) && !self.timed_out
    }
}

/// The evidence of a run: what was checked, by which steps, with which result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WarmEvidence {
    /// The commit checked.
    pub commit: String,
    /// Its tree id.
    pub tree: String,
    /// Always `"warm"`.
    pub backend: String,
    /// The scope that ran.
    pub scope: Scope,
    /// The stage that ran.
    pub stage: Stage,
    /// The steps that ran, in order. A run stops at the first failing step.
    pub steps: Vec<StepResult>,
    /// True when every step passed.
    pub ok: bool,
}

impl WarmEvidence {
    /// The first step that did not pass.
    pub fn failed_step(&self) -> Option<&StepResult> {
        self.steps.iter().find(|s| !s.passed())
    }
}

/// One step of a profile. In `command`, `{scope}` expands to `--workspace` or `-p <crate>` per crate, and `{filter}`
/// to nothing for the full scope or to `-E "package(=a) | package(=b)"` (a nextest filter set) for a crate list. A
/// step with either placeholder is skipped when the scope selects no crate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepSpec {
    /// The step's name, shown in the evidence.
    pub name: String,
    /// The program and arguments.
    pub command: Vec<String>,
    /// Run only when this package is in scope.
    #[serde(default)]
    pub when_in_scope: Option<String>,
    /// A command that must exit 0 for `command` to be used; otherwise `fallback` runs instead.
    #[serde(default)]
    pub probe: Option<Vec<String>>,
    /// The command used when the probe fails (for example `cargo test` where nextest is absent).
    #[serde(default)]
    pub fallback: Option<Vec<String>>,
    /// A timeout for this step in seconds, replacing the config's.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

impl StepSpec {
    fn new(name: &str, command: &[&str]) -> Self {
        StepSpec {
            name: name.into(),
            command: command.iter().map(|s| s.to_string()).collect(),
            when_in_scope: None,
            probe: None,
            fallback: None,
            timeout_secs: None,
        }
    }
}

/// A repo's steps per stage (per-repo configuration, r33 §4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// The pre-merge steps.
    pub fast: Vec<StepSpec>,
    /// The post-merge steps.
    pub full: Vec<StepSpec>,
}

impl Profile {
    /// HAN's own profile, mirroring `.github/workflows/ci.yml`: format, clippy with `-D warnings`, tests, and the
    /// conformance-feature clippy when `han` is in scope. The tests use nextest when it is installed and
    /// `cargo test` otherwise. The full stage adds doc tests, which nextest does not run. The repo has no
    /// `test-slow` set yet; when it gets one, add its step to `full`.
    pub fn han() -> Self {
        let fmt = StepSpec::new("fmt", &["cargo", "fmt", "--all", "--check"]);
        // Clippy and the tests build the whole workspace, so cargo's feature unification (and with it every
        // dependency's fingerprint) is the one the cold run built. `-p` would resolve a different feature set
        // and rebuild dependencies the warm target already has (measured, see the PR). Incremental builds
        // keep untouched workspace crates fresh; the scope only picks which tests run.
        let clippy = StepSpec::new(
            "clippy",
            &[
                "cargo",
                "clippy",
                "--workspace",
                "--all-targets",
                "--locked",
                "--",
                "-D",
                "warnings",
            ],
        );
        let mut test = StepSpec::new(
            "test",
            &[
                "cargo",
                "nextest",
                "run",
                "--workspace",
                "--locked",
                "{filter}",
            ],
        );
        test.probe = Some(vec!["cargo".into(), "nextest".into(), "--version".into()]);
        test.fallback = Some(
            ["cargo", "test", "{scope}", "--locked"]
                .map(String::from)
                .to_vec(),
        );
        let mut conformance = StepSpec::new(
            "clippy-conformance",
            &[
                "cargo",
                "clippy",
                "-p",
                "han",
                "--features",
                "conformance",
                "--all-targets",
                "--locked",
                "--",
                "-D",
                "warnings",
            ],
        );
        conformance.when_in_scope = Some("han".into());
        let doc = StepSpec::new(
            "doc-tests",
            &["cargo", "test", "{scope}", "--doc", "--locked"],
        );
        Profile {
            fast: vec![
                fmt.clone(),
                clippy.clone(),
                test.clone(),
                conformance.clone(),
            ],
            full: vec![fmt, clippy, test, doc, conformance],
        }
    }

    fn steps(&self, stage: Stage) -> &[StepSpec] {
        match stage {
            Stage::Fast => &self.fast,
            Stage::Full => &self.full,
        }
    }
}

/// One command a [`StepLauncher`] must turn into a process.
#[derive(Clone, Debug)]
pub struct StepLaunch {
    /// The program.
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// The working directory (the warm tree).
    pub cwd: PathBuf,
    /// The complete environment. Nothing else may be inherited.
    pub env: Vec<(String, String)>,
    /// Paths the step may write: the tree, the target dir, the temp dir, `CARGO_HOME`.
    pub rw: Vec<PathBuf>,
    /// Paths the step reads: the repo's git directory and every alternate object directory it names.
    pub ro: Vec<PathBuf>,
}

/// Builds the process for a step. [`Direct`] is the default; a host with a sandbox implements this with its launcher.
pub trait StepLauncher: Send + Sync {
    /// The command to spawn. The runner sets stdio, `kill_on_drop` and the process group itself.
    fn command(&self, launch: &StepLaunch) -> Result<Command>;
}

/// Runs the step as a plain child process with the scrubbed environment and no isolation.
#[derive(Clone, Copy, Debug, Default)]
pub struct Direct;

impl StepLauncher for Direct {
    fn command(&self, launch: &StepLaunch) -> Result<Command> {
        let mut cmd = Command::new(&launch.program);
        cmd.args(&launch.args)
            .current_dir(&launch.cwd)
            .env_clear()
            .envs(launch.env.iter().cloned());
        Ok(cmd)
    }
}

/// Warm runner configuration. Off by default: nothing constructs a runner unless the executor's config asks.
#[derive(Clone)]
pub struct WarmConfig {
    /// The executor's private state: `trees/`, `targets/`, `logs/`, `tmp/` live here.
    pub state_dir: PathBuf,
    /// Runs allowed at once across the executor (default 1; the runner VM is shared with agents).
    pub max_concurrent: usize,
    /// The default timeout of one step (default 20 minutes).
    pub step_timeout: Duration,
    /// The timeout of the whole run (default 40 minutes).
    pub run_timeout: Duration,
    /// The repo's steps.
    pub profile: Profile,
    /// Extra environment for the steps. A name that looks like a secret is refused.
    pub extra_env: Vec<(String, String)>,
    /// The environment to take allowed variables from; `None` is the process environment. Tests set it.
    pub host_env: Option<Vec<(String, String)>>,
    /// Changes under these prefixes (and top-level `*.md` files) do not affect any crate.
    pub ignore_prefixes: Vec<String>,
    /// How steps are started.
    pub launcher: Arc<dyn StepLauncher>,
}

impl WarmConfig {
    /// The defaults for HAN, with the state under `state_dir`.
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        WarmConfig {
            state_dir: state_dir.into(),
            max_concurrent: 1,
            step_timeout: Duration::from_secs(20 * 60),
            run_timeout: Duration::from_secs(40 * 60),
            profile: Profile::han(),
            extra_env: Vec::new(),
            host_env: None,
            ignore_prefixes: vec!["docs/".into(), "research/".into()],
            launcher: Arc::new(Direct),
        }
    }
}

/// The warm runner.
pub struct WarmRunner {
    cfg: WarmConfig,
    permits: Semaphore,
    lanes: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl WarmRunner {
    /// A runner for the configuration.
    pub fn new(cfg: WarmConfig) -> Result<Self> {
        if cfg.max_concurrent == 0 {
            return Err(WarmError::Config(
                "max_concurrent must be at least 1".into(),
            ));
        }
        for (name, _) in &cfg.extra_env {
            if secret_like(name) {
                return Err(WarmError::Config(format!(
                    "extra env {name} looks like a secret"
                )));
            }
        }
        Ok(WarmRunner {
            permits: Semaphore::new(cfg.max_concurrent),
            lanes: Mutex::new(HashMap::new()),
            cfg,
        })
    }

    /// The configuration.
    pub fn config(&self) -> &WarmConfig {
        &self.cfg
    }

    /// The scope of the change `base..commit`: checks the commit out in the lane's warm tree, lists the changed paths
    /// and selects the affected packages ([`select_scope`]).
    pub async fn scope_for(
        &self,
        repo: &Path,
        lane: &str,
        base: &str,
        commit: &str,
    ) -> Result<Scope> {
        validate_commit(base)?;
        validate_commit(commit)?;
        let lane_lock = self.lane_lock(repo, lane)?;
        let _guard = lane_lock.lock().await;
        let (tree_dir, _) = self.prepare_tree(repo, lane, commit).await?;
        let changed = git(&tree_dir, &["diff", "--name-only", "-z", base, commit]).await?;
        let changed: Vec<String> = changed
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        let meta = cargo_metadata(&tree_dir, &self.scrubbed(&tree_dir, lane, repo)?).await?;
        select_scope(&meta, &changed, &self.cfg.ignore_prefixes)
    }

    /// Runs the request's stage on its commit and returns the evidence. A failing or timed-out step is `ok: false`
    /// evidence, not an error.
    pub async fn run(&self, req: WarmRequest) -> Result<WarmEvidence> {
        validate_commit(&req.commit)?;
        let lane_lock = self.lane_lock(&req.repo, &req.lane)?;
        let _lane = lane_lock.lock().await;
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| WarmError::Config("runner closed".into()))?;
        let started = Instant::now();
        let deadline = started + self.cfg.run_timeout;

        let (tree_dir, tree_id) = tokio::time::timeout(
            self.cfg.run_timeout,
            self.prepare_tree(&req.repo, &req.lane, &req.commit),
        )
        .await
        .map_err(|_| WarmError::PrepareTimeout)??;
        let key = lane_key(&req.repo, &req.lane)?;
        let log_dir = self.cfg.state_dir.join("logs").join(&key);
        std::fs::create_dir_all(&log_dir)?;
        let env = self.scrubbed(&tree_dir, &req.lane, &req.repo)?;
        let (rw, ro) = self.binds(&tree_dir, &key, &req.repo);

        // `when_in_scope` under a full scope needs the workspace's package names
        let members = if self
            .cfg
            .profile
            .steps(req.stage)
            .iter()
            .any(|s| s.when_in_scope.is_some())
            && req.scope == Scope::Full
        {
            Some(package_names(&cargo_metadata(&tree_dir, &env).await?))
        } else {
            None
        };
        let mut steps = Vec::new();
        let mut ok = true;
        'steps: for (n, spec) in self.cfg.profile.steps(req.stage).iter().enumerate() {
            if let Some(pkg) = &spec.when_in_scope {
                let in_scope = match &req.scope {
                    Scope::Full => members.as_ref().is_some_and(|m| m.contains(pkg)),
                    Scope::Crates(v) => v.contains(pkg),
                };
                if !in_scope {
                    continue;
                }
            }
            let base_cmd = self.choose_command(spec, &tree_dir, &env, &rw, &ro).await;
            let Some(command) = expand_scope(&base_cmd, &req.scope) else {
                continue;
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            let limit = Duration::from_secs(spec.timeout_secs.unwrap_or(u64::MAX))
                .min(self.cfg.step_timeout)
                .min(remaining);
            // a step with under a second left is not started: it could only be killed while its launcher is still starting it
            if limit < Duration::from_secs(1) {
                steps.push(StepResult {
                    name: spec.name.clone(),
                    command,
                    exit: None,
                    timed_out: true,
                    duration_ms: 0,
                    log: None,
                });
                ok = false;
                break 'steps;
            }
            let log = log_dir.join(format!(
                "{}-{n:02}-{}.log",
                &req.commit[..12],
                sanitize(&spec.name)
            ));
            let launch = StepLaunch {
                program: command[0].clone(),
                args: command[1..].to_vec(),
                cwd: tree_dir.clone(),
                env: env.clone(),
                rw: rw.clone(),
                ro: ro.clone(),
            };
            let result = self
                .run_step(&spec.name, &command, &launch, &log, limit)
                .await?;
            let passed = result.passed();
            steps.push(result);
            if !passed {
                ok = false;
                break 'steps;
            }
        }
        Ok(WarmEvidence {
            commit: req.commit,
            tree: tree_id,
            backend: "warm".into(),
            scope: req.scope,
            stage: req.stage,
            steps,
            ok,
        })
    }

    fn lane_lock(&self, repo: &Path, lane: &str) -> Result<Arc<tokio::sync::Mutex<()>>> {
        let key = lane_key(repo, lane)?;
        let mut lanes = self.lanes.lock().unwrap_or_else(|e| e.into_inner());
        Ok(lanes.entry(key).or_default().clone())
    }

    /// The writable and read-only paths of a step.
    fn binds(&self, tree: &Path, key: &str, repo: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let mut rw = vec![tree.to_path_buf(), self.target_dir(key), self.tmp_dir(key)];
        rw.extend(cargo_home(&self.host_env()));
        let mut ro = Vec::new();
        for dir in git_dirs(repo) {
            if !ro.contains(&dir) {
                ro.push(dir);
            }
        }
        (rw, ro)
    }

    fn target_dir(&self, key: &str) -> PathBuf {
        self.cfg.state_dir.join("targets").join(key)
    }

    fn tmp_dir(&self, key: &str) -> PathBuf {
        self.cfg.state_dir.join("tmp").join(key)
    }

    fn host_env(&self) -> Vec<(String, String)> {
        self.cfg
            .host_env
            .clone()
            .unwrap_or_else(|| std::env::vars().collect())
    }

    fn scrubbed(&self, _tree: &Path, lane: &str, repo: &Path) -> Result<Vec<(String, String)>> {
        let key = lane_key(repo, lane)?;
        let tmp = self.tmp_dir(&key);
        std::fs::create_dir_all(&tmp)?;
        std::fs::create_dir_all(self.target_dir(&key))?;
        scrubbed_env(
            &self.host_env(),
            &self.cfg.extra_env,
            &self.target_dir(&key),
            &tmp,
        )
    }

    /// Checks the lane's tree out at `commit` and returns its path and the commit's tree id.
    async fn prepare_tree(
        &self,
        repo: &Path,
        lane: &str,
        commit: &str,
    ) -> Result<(PathBuf, String)> {
        if !repo.exists() {
            return Err(WarmError::Refused(format!(
                "repository {} does not exist",
                repo.display()
            )));
        }
        let key = lane_key(repo, lane)?;
        let tree_dir = self.cfg.state_dir.join("trees").join(&key);
        let hooks = self.cfg.state_dir.join("no-hooks");
        std::fs::create_dir_all(&hooks)?;
        std::fs::create_dir_all(tree_dir.parent().unwrap_or(&self.cfg.state_dir))?;
        let hooks_cfg = format!("core.hooksPath={}", hooks.display());
        // The id must name a commit in the executor's store, not a tag or a tree.
        let spec = format!("{commit}^{{commit}}");
        let resolved = git(
            repo,
            &[
                "-c",
                &hooks_cfg,
                "rev-parse",
                "--verify",
                "--end-of-options",
                &spec,
            ],
        )
        .await
        .map_err(|_| {
            WarmError::Refused(format!(
                "{commit} is not a commit in the executor's repository"
            ))
        })?;
        if resolved.trim() != commit {
            return Err(WarmError::Refused(format!(
                "{commit} is not a full commit id"
            )));
        }
        if !tree_dir.join(".git").exists() {
            // a deleted tree leaves a stale registration behind
            let _ = git(repo, &["worktree", "prune"]).await;
            if tree_dir.exists() {
                std::fs::remove_dir_all(&tree_dir)?;
            }
            let dir = tree_dir.to_string_lossy().into_owned();
            git(
                repo,
                &[
                    "-c", &hooks_cfg, "worktree", "add", "--detach", "--force", &dir, commit,
                ],
            )
            .await?;
        }
        git(
            &tree_dir,
            &["-c", &hooks_cfg, "checkout", "--force", "--detach", commit],
        )
        .await?;
        git(&tree_dir, &["-c", &hooks_cfg, "reset", "--hard", commit]).await?;
        // The tree holds sources only: build output is in the target dir, outside it.
        git(&tree_dir, &["clean", "-ffdx"]).await?;
        let tree = git(&tree_dir, &["rev-parse", &format!("{commit}^{{tree}}")]).await?;
        Ok((tree_dir, tree.trim().to_string()))
    }

    /// The probe decides between the command and its fallback.
    async fn choose_command(
        &self,
        spec: &StepSpec,
        tree: &Path,
        env: &[(String, String)],
        rw: &[PathBuf],
        ro: &[PathBuf],
    ) -> Vec<String> {
        let (Some(probe), Some(fallback)) = (&spec.probe, &spec.fallback) else {
            return spec.command.clone();
        };
        let launch = StepLaunch {
            program: probe[0].clone(),
            args: probe[1..].to_vec(),
            cwd: tree.to_path_buf(),
            env: env.to_vec(),
            rw: rw.to_vec(),
            ro: ro.to_vec(),
        };
        let ok = match self.cfg.launcher.command(&launch) {
            Ok(mut cmd) => {
                cmd.stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .kill_on_drop(true);
                matches!(tokio::time::timeout(Duration::from_secs(60), cmd.status()).await, Ok(Ok(s)) if s.success())
            }
            Err(_) => false,
        };
        if ok {
            spec.command.clone()
        } else {
            fallback.clone()
        }
    }

    async fn run_step(
        &self,
        name: &str,
        command: &[String],
        launch: &StepLaunch,
        log: &Path,
        limit: Duration,
    ) -> Result<StepResult> {
        use std::io::Write;
        let mut file = std::fs::File::create(log)?;
        writeln!(file, "$ {}", command.join(" "))?;
        let out = file.try_clone()?;
        let mut cmd = self.cfg.launcher.command(launch)?;
        cmd.stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(file))
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        let started = Instant::now();
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                // a program that cannot start is a failed step with the reason in its log
                let _ = std::fs::OpenOptions::new()
                    .append(true)
                    .open(log)
                    .and_then(|mut f| writeln!(f, "cannot start: {e}"));
                return Ok(StepResult {
                    name: name.into(),
                    command: command.to_vec(),
                    exit: None,
                    timed_out: false,
                    duration_ms: 0,
                    log: Some(log.to_path_buf()),
                });
            }
        };
        let pid = child.id();
        let (exit, timed_out) = match tokio::time::timeout(limit, child.wait()).await {
            Ok(Ok(status)) => (status.code(), false),
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                kill_tree(pid).await;
                let _ = child.kill().await;
                (None, true)
            }
        };
        let result = StepResult {
            name: name.into(),
            command: command.to_vec(),
            exit,
            timed_out,
            duration_ms: started.elapsed().as_millis() as u64,
            log: Some(log.to_path_buf()),
        };
        // whatever the step left running (a test that starts a daemon) must not hold a binary the next build replaces
        let target = launch
            .env
            .iter()
            .find(|(k, _)| k == "CARGO_TARGET_DIR")
            .map(|(_, v)| PathBuf::from(v));
        let dirs: Vec<PathBuf> = std::iter::once(launch.cwd.clone()).chain(target).collect();
        reap(pid, &dirs).await;
        Ok(result)
    }
}

/// Kills what a finished step left behind. On Unix the step led its own process group, so the group goes; on Windows,
/// where an orphan can no longer be found through its parent, every process running an executable under `dirs` (the
/// warm tree and its target dir, which only this lane's steps build) goes. A test that starts a daemon would
/// otherwise keep its binary open and fail the next build with "Access is denied".
async fn reap(pid: Option<u32>, dirs: &[PathBuf]) {
    #[cfg(windows)]
    let mut cmd = {
        let _ = pid;
        let mut c = Command::new("powershell");
        c.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$d = $env:HAN_REAP_DIRS -split ';'; Get-CimInstance Win32_Process | Where-Object { $p = $_.ExecutablePath; $p -and ($d | Where-Object { $p.StartsWith($_, [System.StringComparison]::OrdinalIgnoreCase) }) } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }",
        ]);
        let joined: Vec<String> = dirs
            .iter()
            .map(|d| d.to_string_lossy().into_owned())
            .collect();
        c.env("HAN_REAP_DIRS", joined.join(";"));
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let _ = dirs;
        let Some(pid) = pid else { return };
        let mut c = Command::new("kill");
        c.args(["-KILL", "--", &format!("-{pid}")]);
        c
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _ = tokio::time::timeout(Duration::from_secs(30), cmd.status()).await;
}

/// Kills a step's process and everything it started (cargo starts rustc, the tests start more).
async fn kill_tree(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("taskkill");
        c.args(["/F", "/T", "/PID", &pid.to_string()]);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        // the step leads its own process group (see run_step)
        let mut c = Command::new("kill");
        c.args(["-KILL", "--", &format!("-{pid}")]);
        c
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _ = tokio::time::timeout(Duration::from_secs(10), cmd.status()).await;
}

/// Expands `{scope}`; `None` when the scope selects no package (a scoped step then has nothing to run).
fn expand_scope(command: &[String], scope: &Scope) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for arg in command {
        if arg == "{filter}" {
            match scope {
                Scope::Full => {}
                Scope::Crates(v) if v.is_empty() => return None,
                Scope::Crates(v) => {
                    out.push("-E".into());
                    out.push(
                        v.iter()
                            .map(|c| format!("package(={c})"))
                            .collect::<Vec<_>>()
                            .join(" | "),
                    );
                }
            }
        } else if arg == "{scope}" {
            match scope {
                Scope::Full => out.push("--workspace".into()),
                Scope::Crates(v) if v.is_empty() => return None,
                Scope::Crates(v) => {
                    for c in v {
                        out.push("-p".into());
                        out.push(c.clone());
                    }
                }
            }
        } else {
            out.push(arg.clone());
        }
    }
    Some(out)
}

fn validate_commit(commit: &str) -> Result<()> {
    let ok = matches!(commit.len(), 40 | 64)
        && commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if ok {
        Ok(())
    } else {
        Err(WarmError::Refused(format!(
            "{commit:?} is not a full lowercase hex commit id"
        )))
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The directory name of the (repo, lane) pair: the lane name plus a hash of the repository path.
fn lane_key(repo: &Path, lane: &str) -> Result<String> {
    if lane.is_empty()
        || lane.len() > 64
        || !lane
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        || lane.starts_with('.')
    {
        return Err(WarmError::Refused(format!(
            "lane name {lane:?} is not [A-Za-z0-9._-]{{1,64}}"
        )));
    }
    let canon = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let digest = Sha256::digest(canon.to_string_lossy().as_bytes());
    Ok(format!(
        "{lane}-{}",
        digest[..4]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    ))
}

/// The executor's git directory and every object directory it names through alternates, for a sandbox to bind
/// read-only. The worktree's metadata lives in the common dir, so that is bound too.
fn git_dirs(repo: &Path) -> Vec<PathBuf> {
    let git_dir = if repo.join(".git").is_dir() {
        repo.join(".git")
    } else {
        repo.to_path_buf()
    };
    let mut out = vec![git_dir.clone()];
    let mut pending = vec![git_dir.join("objects")];
    let mut seen = 0;
    while let Some(objects) = pending.pop() {
        seen += 1;
        if seen > 8 {
            break;
        }
        let Ok(text) = std::fs::read_to_string(objects.join("info").join("alternates")) else {
            continue;
        };
        for line in text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            let p = Path::new(line);
            let p = if p.is_absolute() {
                p.to_path_buf()
            } else {
                objects.join(p)
            };
            if p.exists() && !out.contains(&p) {
                pending.push(p.clone());
                out.push(p);
            }
        }
    }
    out
}

/// Runs git in `dir` and returns its trimmed stdout. Git runs with the executor's own environment: it is the
/// executor's operation on its own repository, not a step.
async fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout)
            .trim_end_matches('\n')
            .to_string())
    } else {
        Err(WarmError::Git(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

async fn cargo_metadata(tree: &Path, env: &[(String, String)]) -> Result<Value> {
    let out = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(tree)
        .env_clear()
        .envs(env.iter().cloned())
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await?;
    if !out.status.success() {
        return Err(WarmError::Metadata(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| WarmError::Metadata(e.to_string()))
}

fn package_names(metadata: &Value) -> Vec<String> {
    let names = metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["name"].as_str());
    names.map(String::from).collect()
}

/// Selects the packages a change affects, from `cargo metadata --no-deps` output and the changed paths (relative to
/// the workspace root, `/`-separated): the packages that own a changed path plus every package that depends on them
/// (dev-dependencies included), transitively.
///
/// Any changed path outside every package (workspace manifest, lockfile, toolchain pin, CI configuration, scripts)
/// makes the scope [`Scope::Full`], as does a package whose root is the workspace root. Paths under `ignore_prefixes`
/// and top-level Markdown files affect nothing.
pub fn select_scope(
    metadata: &Value,
    changed: &[String],
    ignore_prefixes: &[String],
) -> Result<Scope> {
    let bad = |what: &str| WarmError::Metadata(format!("missing {what}"));
    let root = Path::new(
        metadata["workspace_root"]
            .as_str()
            .ok_or_else(|| bad("workspace_root"))?,
    );
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| bad("packages"))?;
    // (name, dir relative to the workspace root, names of its dependencies)
    let mut pkgs: Vec<(String, String, Vec<String>)> = Vec::new();
    for p in packages {
        let name = p["name"]
            .as_str()
            .ok_or_else(|| bad("package name"))?
            .to_string();
        let manifest = Path::new(
            p["manifest_path"]
                .as_str()
                .ok_or_else(|| bad("manifest_path"))?,
        );
        let dir = manifest.parent().unwrap_or(manifest);
        let rel = dir
            .strip_prefix(root)
            .map_err(|_| WarmError::Metadata(format!("{name} is outside the workspace")))?;
        let rel = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let deps = p["dependencies"]
            .as_array()
            .map(|d| {
                d.iter()
                    .filter_map(|d| d["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        pkgs.push((name, rel, deps));
    }
    let mut affected: BTreeSet<String> = BTreeSet::new();
    for path in changed {
        let path = path.replace('\\', "/");
        if ignore_prefixes.iter().any(|p| path.starts_with(p.as_str()))
            || (!path.contains('/') && path.ends_with(".md"))
        {
            continue;
        }
        let owner = pkgs
            .iter()
            .filter(|(_, dir, _)| {
                !dir.is_empty() && (path == *dir || path.starts_with(&format!("{dir}/")))
            })
            .max_by_key(|(_, dir, _)| dir.len());
        match owner {
            Some((name, _, _)) => {
                affected.insert(name.clone());
            }
            None => return Ok(Scope::Full),
        }
    }
    // reverse dependencies, to a fixed point
    loop {
        let before = affected.len();
        for (name, _, deps) in &pkgs {
            if deps.iter().any(|d| affected.contains(d)) {
                affected.insert(name.clone());
            }
        }
        if affected.len() == before {
            break;
        }
    }
    Ok(Scope::Crates(affected.into_iter().collect()))
}

/// True when an environment variable name looks like it carries a credential.
pub fn secret_like(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "KEY",
        "AUTH",
        "COOKIE",
        "SESSION",
    ]
    .iter()
    .any(|w| n.contains(w))
}

/// Variables copied from the host environment when present: the OS and MSVC variables the toolchain needs.
const PASS: &[&str] = &[
    "PATH",
    "RUSTUP_HOME",
    "CARGO_HOME",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMW6432",
    "PROGRAMDATA",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "LANG",
    "LC_ALL",
    "LIB",
    "INCLUDE",
    "LIBPATH",
    "VCINSTALLDIR",
    "VSINSTALLDIR",
    "VCTOOLSINSTALLDIR",
    "WINDOWSSDKDIR",
    "WINDOWSSDKVERSION",
    "UCRTVERSION",
    "UNIVERSALCRTSDKDIR",
];

fn lookup<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    env.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn home(env: &[(String, String)]) -> Option<PathBuf> {
    lookup(env, "HOME")
        .or_else(|| lookup(env, "USERPROFILE"))
        .map(PathBuf::from)
}

fn cargo_home(env: &[(String, String)]) -> Option<PathBuf> {
    lookup(env, "CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| home(env).map(|h| h.join(".cargo")))
}

/// The environment a step gets, built from an allow-list: `PATH`, `RUSTUP_HOME`, `CARGO_HOME`, the OS and MSVC
/// variables in [`PASS`], the temp directory, and `CARGO_TARGET_DIR`. Everything else in `host` is dropped,
/// including every credential. `extra` names (checked by [`secret_like`]) are added last.
pub fn scrubbed_env(
    host: &[(String, String)],
    extra: &[(String, String)],
    target: &Path,
    tmp: &Path,
) -> Result<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = Vec::new();
    for name in PASS {
        if let Some(v) = lookup(host, name) {
            out.push((name.to_string(), v.to_string()));
        }
    }
    let mut set = |k: &str, v: String| {
        out.retain(|(n, _)| !n.eq_ignore_ascii_case(k));
        out.push((k.to_string(), v));
    };
    if let Some(h) = cargo_home(host) {
        set("CARGO_HOME", h.to_string_lossy().into_owned());
    }
    let rustup_home = lookup(host, "RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| home(host).map(|h| h.join(".rustup")));
    if let Some(h) = rustup_home {
        set("RUSTUP_HOME", h.to_string_lossy().into_owned());
    }
    let tmp = tmp.to_string_lossy().into_owned();
    for k in ["TMPDIR", "TEMP", "TMP"] {
        set(k, tmp.clone());
    }
    set("CARGO_TARGET_DIR", target.to_string_lossy().into_owned());
    set("CARGO_TERM_COLOR", "never".into());
    for (k, v) in extra {
        if secret_like(k) {
            return Err(WarmError::Config(format!(
                "extra env {k} looks like a secret"
            )));
        }
        set(k, v.clone());
    }
    Ok(out)
}

// ───────────────────────── the `Checks` adapter ─────────────────────────

/// Where a [`WarmChecks`] finds the executor's repository (with the lane's commit in its object store) for a repo.
pub type RepoPath = Arc<dyn Fn(&RepoTarget) -> PathBuf + Send + Sync>;

enum RunState {
    Running,
    Done(Box<WarmEvidence>),
    Failed(String),
}

/// The warm runner behind the lane's [`Checks`] seam.
///
/// * `start` begins a run in the background (once per repo, commit and stage; calling it again is harmless).
///   `poll` also begins one when none exists, so a restarted executor, or the post-merge stage (which the lane
///   only polls), still gets its run.
/// * The fast stage's scope is the change `commit^..commit`: the lane's commit is a squash rebase onto the target's
///   tip, so its parent is the base. Any difficulty computing it means the full scope. The full stage runs the
///   full scope.
/// * `poll` reports one [`CheckRun`] per step that ran (named as in the profile, with command, exit, duration and
///   log path) and, for each requested name that is not a step's name, an aggregate run of that name: pending while
///   the run goes, `success` when every step passed, `timed_out` or `failure` otherwise. A repository's
///   `checks.required` (default `["quick"]`) is therefore satisfied by the aggregate.
/// * `rerun` forgets a finished run of the commit and starts it again.
pub struct WarmChecks {
    runner: Arc<WarmRunner>,
    repo_path: RepoPath,
    runs: Arc<Mutex<HashMap<String, RunState>>>,
}

impl WarmChecks {
    /// An adapter over `runner`; `repo_path` finds the repository a request's commit is in.
    pub fn new(runner: Arc<WarmRunner>, repo_path: RepoPath) -> WarmChecks {
        WarmChecks {
            runner,
            repo_path,
            runs: Arc::default(),
        }
    }

    fn key(req: &CheckRequest) -> String {
        format!("{}|{}|{:?}", req.repo.name, req.commit, req.stage)
    }

    fn begin(&self, req: &CheckRequest) -> std::result::Result<(), CheckError> {
        if req.repo.checks.backend != ChecksBackend::Warm {
            return Err(CheckError(format!(
                "{} does not use the warm runner",
                req.repo.name
            )));
        }
        let key = Self::key(req);
        {
            let mut runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
            if runs.contains_key(&key) {
                return Ok(());
            }
            runs.insert(key.clone(), RunState::Running);
        }
        let (runner, runs, repo, commit, stage) = (
            self.runner.clone(),
            self.runs.clone(),
            (self.repo_path)(&req.repo),
            req.commit.clone(),
            req.stage,
        );
        let lane: String = req
            .repo
            .name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        tokio::spawn(async move {
            let scope = match stage {
                Stage::Full => Scope::Full,
                Stage::Fast => match git(&repo, &["rev-parse", &format!("{commit}^")]).await {
                    Ok(base) => runner
                        .scope_for(&repo, &lane, &base, &commit)
                        .await
                        .unwrap_or(Scope::Full),
                    Err(_) => Scope::Full,
                },
            };
            let state = match runner
                .run(WarmRequest {
                    repo,
                    lane,
                    commit,
                    scope,
                    stage,
                })
                .await
            {
                Ok(ev) => RunState::Done(Box::new(ev)),
                Err(e) => RunState::Failed(e.to_string()),
            };
            runs.lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, state);
        });
        Ok(())
    }
}

fn step_run(s: &StepResult) -> CheckRun {
    let conclusion = if s.timed_out {
        "timed_out"
    } else if s.passed() {
        "success"
    } else {
        "failure"
    };
    let mut run = CheckRun::new(&s.name, conclusion, None);
    run.command = Some(s.command.join(" "));
    run.exit = s.exit;
    run.duration_ms = Some(s.duration_ms);
    run.log = s
        .log
        .as_ref()
        .map(|p| serde_json::json!({ "path": p.to_string_lossy() }));
    run
}

#[async_trait::async_trait]
impl Checks for WarmChecks {
    fn name(&self) -> &'static str {
        "warm-runner"
    }

    async fn start(&self, req: &CheckRequest) -> std::result::Result<(), CheckError> {
        self.begin(req)
    }

    async fn poll(&self, req: &CheckRequest) -> std::result::Result<Vec<CheckRun>, CheckError> {
        self.begin(req)?;
        let runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
        let aggregate = |conclusion: &str, log: Option<Value>| -> Vec<CheckRun> {
            req.names
                .iter()
                .map(|n| {
                    let mut r = CheckRun::new(n, conclusion, None);
                    r.log = log.clone();
                    r
                })
                .collect()
        };
        Ok(match runs.get(&Self::key(req)) {
            None | Some(RunState::Running) => aggregate("pending", None),
            Some(RunState::Failed(e)) => {
                aggregate("failure", Some(serde_json::json!({ "error": e })))
            }
            Some(RunState::Done(ev)) => {
                let mut out: Vec<CheckRun> = ev.steps.iter().map(step_run).collect();
                let conclusion = if ev.ok {
                    "success"
                } else if ev.steps.iter().any(|s| s.timed_out) {
                    "timed_out"
                } else {
                    "failure"
                };
                let info = serde_json::json!({
                    "tree": ev.tree, "scope": ev.scope, "stage": ev.stage, "backend": "warm"
                });
                let named: Vec<String> = ev.steps.iter().map(|s| s.name.clone()).collect();
                out.extend(
                    aggregate(conclusion, Some(info))
                        .into_iter()
                        .filter(|r| !named.contains(&r.name)),
                );
                out
            }
        })
    }

    async fn rerun(
        &self,
        req: &CheckRequest,
        _names: &[String],
    ) -> std::result::Result<bool, CheckError> {
        {
            let mut runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
            if matches!(runs.get(&Self::key(req)), Some(RunState::Running)) {
                return Ok(false);
            }
            runs.remove(&Self::key(req));
        }
        self.begin(req)?;
        Ok(true)
    }
}

/// Sends each request to the backend its repository's `checks.backend` names.
pub struct RoutedChecks {
    github: Option<Arc<dyn Checks>>,
    warm: Option<Arc<dyn Checks>>,
}

impl RoutedChecks {
    /// A router over the backends that are set up. A repository whose backend is missing gets a [`CheckError`].
    pub fn new(github: Option<Arc<dyn Checks>>, warm: Option<Arc<dyn Checks>>) -> RoutedChecks {
        RoutedChecks { github, warm }
    }

    fn pick(&self, repo: &RepoTarget) -> std::result::Result<&Arc<dyn Checks>, CheckError> {
        let (slot, what) = match repo.checks.backend {
            ChecksBackend::Github => (&self.github, "GitHub"),
            ChecksBackend::Warm => (&self.warm, "warm runner"),
        };
        slot.as_ref().ok_or_else(|| {
            CheckError(format!(
                "{} uses the {what} checks, which are not set up",
                repo.name
            ))
        })
    }
}

#[async_trait::async_trait]
impl Checks for RoutedChecks {
    fn name(&self) -> &'static str {
        "routed"
    }

    fn name_for(&self, repo: &RepoTarget) -> &'static str {
        self.pick(repo).map_or("routed", |c| c.name())
    }

    async fn start(&self, req: &CheckRequest) -> std::result::Result<(), CheckError> {
        self.pick(&req.repo)?.start(req).await
    }

    async fn poll(&self, req: &CheckRequest) -> std::result::Result<Vec<CheckRun>, CheckError> {
        self.pick(&req.repo)?.poll(req).await
    }

    async fn rerun(
        &self,
        req: &CheckRequest,
        names: &[String],
    ) -> std::result::Result<bool, CheckError> {
        self.pick(&req.repo)?.rerun(req, names).await
    }
}

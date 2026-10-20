//! The supervisor seam (PLAN section 3): han-node runs the agent hosts as child processes, `han host --spec
//! <file>`, one per session. The node makes the [`HostSpec`] (through `han-registry`) and hands it over; this is
//! the trait it talks to and a spawner that starts the host as a process of the same binary.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use han_proc::NoWindow as _;
use han_registry::{
    HostAdmission, HostSpec, RunAs, spec_file, unit_admission_file, unit_spec_file,
};
use tokio::process::{Child, Command};

/// What to start: one agent host.
#[derive(Clone, Debug)]
pub struct RoleSpec {
    /// A name that identifies this instance (the agent and its issue); starting the same name again is a
    /// conflict.
    pub name: String,
    /// Everything the host needs. It holds no secret (SR-09): model keys stay in files the host reads itself.
    pub spec: HostSpec,
    /// Run the host as this OS user (SR-83); `None` runs it as the node's own user.
    pub run_as: Option<RunAs>,
}

/// The systemd unit that runs a host as `os_user`.
pub fn unit_of(os_user: &str) -> String {
    format!("han-agent@{os_user}.service")
}

/// The name of the role that hosts `agent` on an issue or a task: `host-<agent>#<issue>`, `host-<agent>#<task>`
/// or `host-<agent>`. `agent stop` and the board find a role by it.
pub fn host_name(agent: &str, issue: Option<&str>, task: Option<&str>) -> String {
    match (issue, task) {
        (Some(i), _) => format!("host-{agent}#{i}"),
        (None, Some(t)) => format!("host-{agent}#{t}"),
        (None, None) => format!("host-{agent}"),
    }
}

/// The agent and the work (an issue or a task) in a role's name made by [`host_name`].
pub fn parse_host_name(name: &str) -> Option<(&str, Option<&str>)> {
    let rest = name.strip_prefix("host-")?;
    // An actor has no `#`; an issue does (`github:o/r#1`).
    Some(match rest.split_once('#') {
        Some((agent, work)) => (agent, Some(work)),
        None => (rest, None),
    })
}

/// What the board says of a session whose host exited without closing it.
pub const HOST_EXITED: &str = "host exited";

/// Watches the roles for hosts that exit. A host that ends its work closes its session first; one that is killed
/// (SIGTERM, a stop timeout, the OOM killer) closes nothing, and its session would read `idle` until silence
/// made it `stale`, then `failed`. The node feeds this the roles at each tick and marks the sessions of the
/// hosts it returns as ended.
///
/// An exit is reported only after it has stood for a grace period: a host that ended well has sent its
/// `session.close`, and the node may see the unit end before that entry arrives.
#[derive(Debug, Default)]
pub struct ExitWatch {
    running: std::collections::HashSet<String>,
    gone: HashMap<String, u64>,
}

impl ExitWatch {
    /// Takes the roles as they stand at `now_ms` (the node's steady clock); returns the roles whose exit has
    /// stood for `grace_ms`. A role that is no longer listed is forgotten (see [`ExitWatch::stopped`]).
    pub fn observe(
        &mut self,
        roles: &[(String, RoleStatus)],
        now_ms: u64,
        grace_ms: u64,
    ) -> Vec<String> {
        self.running.retain(|n| roles.iter().any(|(r, _)| r == n));
        for (name, status) in roles {
            match status {
                RoleStatus::Running => {
                    self.running.insert(name.clone());
                    // started again under the same name
                    self.gone.remove(name);
                }
                RoleStatus::Exited(_) => {
                    if self.running.remove(name) {
                        self.gone.entry(name.clone()).or_insert(now_ms);
                    }
                }
            }
        }
        let due: Vec<String> = self
            .gone
            .iter()
            .filter(|(_, since)| now_ms.saturating_sub(**since) >= grace_ms)
            .map(|(n, _)| n.clone())
            .collect();
        for n in &due {
            self.gone.remove(n);
        }
        let mut due = due;
        due.sort();
        due
    }

    /// The role `name` was stopped on purpose (and so is no longer listed): its exit counts from `now_ms`.
    pub fn stopped(&mut self, name: &str, now_ms: u64) {
        self.running.remove(name);
        self.gone.entry(name.to_owned()).or_insert(now_ms);
    }
}

/// The variables a child role inherits from the node (SR-08): what processes need to run on each OS, logging
/// settings, and nothing else. Everything else a host needs is in its [`HostSpec`].
pub const ENV_ALLOW: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SYSTEMROOT",
    "SystemRoot",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "TEMP",
    "TMP",
    "TMPDIR",
    "HOME",
    "USERPROFILE",
    "USERNAME",
    "USER",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "DBUS_SESSION_BUS_ADDRESS",
    "LANG",
    "LC_ALL",
    "RUST_LOG",
    "RUST_BACKTRACE",
    "NUMBER_OF_PROCESSORS",
];

/// Why a role could not be started or stopped.
#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    /// This node has no way to run the host.
    #[error("agent hosts are not available on this node")]
    Unavailable,
    /// A role with this name is running.
    #[error("{0} is already running")]
    Running(String),
    /// Nothing by this name is running.
    #[error("{0} is not running")]
    NotRunning(String),
    /// The process could not be started.
    #[error("spawn: {0}")]
    Spawn(String),
    /// The sandbox the agent is set to use cannot work on this machine, so no host is started. The message
    /// says why and what to do instead.
    #[error("{0}")]
    SandboxUnavailable(String),
}

/// How a role ended, or that it has not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoleStatus {
    /// Still running.
    Running,
    /// Ended with this exit code (`None`: killed by a signal).
    Exited(Option<i32>),
}

/// Starts, stops and watches child roles. A supervisor is a seam: M4 fills it in with the real roles.
pub trait Supervisor: Send + Sync + 'static {
    /// Starts a role.
    fn start(&self, spec: RoleSpec) -> Result<(), SupervisorError>;

    /// Stops the role called `name`.
    fn stop(&self, name: &str) -> Result<(), SupervisorError>;

    /// The roles this supervisor has started, with how they stand.
    fn status(&self) -> Vec<(String, RoleStatus)>;

    /// What stops roles from starting on this machine, one sentence each (shown in the node's status).
    fn problems(&self) -> Vec<String> {
        Vec::new()
    }
}

/// A supervisor that runs nothing: every start fails as unavailable.
pub struct NoSupervisor;

impl Supervisor for NoSupervisor {
    fn start(&self, _spec: RoleSpec) -> Result<(), SupervisorError> {
        Err(SupervisorError::Unavailable)
    }

    fn stop(&self, name: &str) -> Result<(), SupervisorError> {
        Err(SupervisorError::NotRunning(name.to_owned()))
    }

    fn status(&self) -> Vec<(String, RoleStatus)> {
        Vec::new()
    }
}

/// Starts hosts as child processes: `<program> host --spec <file> `. Children die with the spawner
/// (`kill_on_drop`), so a stopped node leaves no hosts behind.
pub struct ProcessSpawner {
    program: PathBuf,
    /// The arguments before the spec file: `host --spec`.
    prefix: Vec<String>,
    systemctl: PathBuf,
    /// The host-class endpoint of the node, given to every host this spawner starts as the node's own user.
    host_endpoint: Option<String>,
    children: Mutex<HashMap<String, Child>>,
    /// Hosts running as another OS user, by name: their unit and the state directory their admission is in.
    units: Mutex<HashMap<String, (String, PathBuf)>>,
    /// How long a start waits for the unit's host to say whether its sandbox works.
    admission_wait: Duration,
}

impl ProcessSpawner {
    /// A spawner that runs `program` (normally this binary's own path, `std::env::current_exe()`).
    pub fn new(program: impl Into<PathBuf>) -> Self {
        ProcessSpawner {
            program: program.into(),
            prefix: vec!["host".into(), "--spec".into()],
            systemctl: PathBuf::from("systemctl"),
            host_endpoint: None,
            children: Mutex::new(HashMap::new()),
            units: Mutex::new(HashMap::new()),
            admission_wait: Duration::from_secs(5),
        }
    }

    /// Waits this long (at most) after starting a unit for its host's admission; the host checks its sandbox
    /// first thing, so a refusal normally arrives in well under a second.
    pub fn with_admission_wait(mut self, wait: Duration) -> Self {
        self.admission_wait = wait;
        self
    }

    /// Puts these arguments in front of the spec file instead of `host --spec` (for a test that runs
    /// something other than `han`).
    pub fn with_prefix(mut self, prefix: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.prefix = prefix.into_iter().map(Into::into).collect();
        self
    }

    /// Uses this `systemctl` to start and stop the units of hosts that run as another OS user.
    pub fn with_systemctl(mut self, systemctl: impl Into<PathBuf>) -> Self {
        self.systemctl = systemctl.into();
        self
    }

    /// Gives the hosts it starts as the node's own user this host-class endpoint (see
    /// `rpc::role_endpoint`) as `node_endpoint`, and no other: a host never learns the face endpoint, and a
    /// spec cannot bring its own.
    pub fn with_host_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.host_endpoint = Some(endpoint.into());
        self
    }

    fn systemctl(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        std::process::Command::new(&self.systemctl)
            .no_window()
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
    }

    fn unit_active(&self, unit: &str) -> bool {
        self.systemctl(&["is-active", unit])
            .map(|o| {
                matches!(
                    String::from_utf8_lossy(&o.stdout).trim(),
                    "active" | "activating"
                )
            })
            .unwrap_or(false)
    }

    /// Starts a host as another OS user: its spec goes to the user's spec file and the unit
    /// `han-agent@<user>` is started. One host per OS user at a time.
    fn start_as(&self, mut spec: RoleSpec, run_as: &RunAs) -> Result<(), SupervisorError> {
        // The unit's user cannot reach the node's owner-only endpoints.
        spec.spec.node_endpoint = None;
        if !han_registry::valid_os_user(&run_as.os_user) {
            return Err(SupervisorError::Spawn(format!(
                "{:?} is not an OS user name",
                run_as.os_user
            )));
        }
        let unit = unit_of(&run_as.os_user);
        if self.unit_active(&unit) {
            return Err(SupervisorError::Running(spec.name));
        }
        let file = unit_spec_file(&run_as.state_dir);
        // An earlier host's word is not this one's.
        let admission = unit_admission_file(&run_as.state_dir);
        let _ = std::fs::remove_file(&admission);
        let spawn = |e: std::io::Error| SupervisorError::Spawn(format!("{}: {e}", file.display()));
        spec.spec.write(&file).map_err(spawn)?;
        // The unit runs as the OS user, who shares the state directory's group (and nothing else of ours).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let set = |p: &std::path::Path, mode| {
                std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
            };
            set(&file, 0o660).map_err(spawn)?;
            if let Some(dir) = file.parent() {
                set(dir, 0o770).map_err(spawn)?;
            }
        }
        let out = self
            .systemctl(&["start", "--no-block", &unit])
            .map_err(|e| SupervisorError::Spawn(format!("systemctl: {e}")))?;
        if !out.status.success() {
            return Err(SupervisorError::Spawn(format!(
                "systemctl start {unit}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        self.units
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(spec.name, (unit.clone(), run_as.state_dir.clone()));
        // The host checks its sandbox where it runs, under its own unit (this process cannot: its unit forbids
        // namespaces). A refusal is the answer to this start; it also stays in `problems`.
        let deadline = Instant::now() + self.admission_wait;
        loop {
            let active = self.unit_active(&unit);
            match HostAdmission::read(&admission) {
                Some(HostAdmission::Refused { message, .. }) => {
                    return Err(SupervisorError::SandboxUnavailable(message));
                }
                Some(HostAdmission::Admitted) => break,
                None => {}
            }
            if Instant::now() >= deadline {
                return Err(SupervisorError::Spawn(if active {
                    format!(
                        "startup of {unit} is not yet confirmed: no host admission before the deadline; inspect agent status and unit logs before retrying"
                    )
                } else {
                    format!(
                        "{unit} did not report host admission and is not active; inspect its unit logs"
                    )
                }));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }

    /// Takes back the hosts that run as other OS users and outlived the node. A unit is not the node's child, so
    /// a restarted node knows nothing of it: `agent stop` would say "is not running" and the roles would not
    /// list it, while the unit went on. For each `(agent, run_as)` whose unit is active and not already
    /// known, the role is remembered under the name its spec (the spawn file the node left for it) gives.
    /// Returns the names adopted. Call it once at node start, with the OS-user agents of `agents.json`.
    pub fn adopt(&self, candidates: &[(String, RunAs)]) -> Vec<String> {
        let mut adopted = Vec::new();
        for (agent, run_as) in candidates {
            if !han_registry::valid_os_user(&run_as.os_user) {
                continue;
            }
            let unit = unit_of(&run_as.os_user);
            let known = self
                .units
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .values()
                .any(|(u, _)| *u == unit);
            if known || !self.unit_active(&unit) {
                continue;
            }
            let file = unit_spec_file(&run_as.state_dir);
            // The spec says what the unit runs; without it the unit is still ours to stop, under the agent's name.
            let name = match HostSpec::read(&file.to_string_lossy()) {
                Ok(s) => host_name(&s.agent, s.work.issue.as_deref(), s.work.task.as_deref()),
                Err(_) => host_name(agent, None, None),
            };
            self.units
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(name.clone(), (unit, run_as.state_dir.clone()));
            adopted.push(name);
        }
        adopted
    }

    fn unit_list(&self) -> Vec<(String, (String, PathBuf))> {
        self.units
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(n, u)| (n.clone(), u.clone()))
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Child>> {
        self.children.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl Supervisor for ProcessSpawner {
    fn start(&self, mut spec: RoleSpec) -> Result<(), SupervisorError> {
        if let Some(run_as) = spec.run_as.clone() {
            return self.start_as(spec, &run_as);
        }
        let mut children = self.lock();
        if let Some(child) = children.get_mut(&spec.name) {
            // A host that has ended may be started again.
            if matches!(child.try_wait(), Ok(None)) {
                return Err(SupervisorError::Running(spec.name));
            }
        }
        spec.spec.node_endpoint = self.host_endpoint.clone();
        let file = spec_file(&spec.spec.dirs.state, &spec.name);
        spec.spec
            .write(&file)
            .map_err(|e| SupervisorError::Spawn(format!("{}: {e}", file.display())))?;
        let inherited = ENV_ALLOW
            .iter()
            .filter_map(|k| std::env::var_os(k).map(|v| (k.to_string(), v)));
        let child = Command::new(&self.program)
            .no_window()
            .args(&self.prefix)
            .arg(&file)
            .env_clear()
            .envs(inherited)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| SupervisorError::Spawn(e.to_string()))?;
        children.insert(spec.name, child);
        Ok(())
    }

    fn stop(&self, name: &str) -> Result<(), SupervisorError> {
        let unit = self
            .units
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(name);
        if let Some((unit, _)) = unit {
            let out = self
                .systemctl(&["stop", &unit])
                .map_err(|e| SupervisorError::Spawn(format!("systemctl: {e}")))?;
            return if out.status.success() {
                Ok(())
            } else {
                Err(SupervisorError::Spawn(format!(
                    "systemctl stop {unit}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )))
            };
        }
        let mut child = self
            .lock()
            .remove(name)
            .ok_or_else(|| SupervisorError::NotRunning(name.to_owned()))?;
        child
            .start_kill()
            .map_err(|e| SupervisorError::Spawn(e.to_string()))
    }

    fn status(&self) -> Vec<(String, RoleStatus)> {
        let mut children = self.lock();
        let mut out: Vec<(String, RoleStatus)> = children
            .iter_mut()
            .map(|(name, child)| {
                let status = match child.try_wait() {
                    Ok(Some(s)) => RoleStatus::Exited(s.code()),
                    _ => RoleStatus::Running,
                };
                (name.clone(), status)
            })
            .collect();
        drop(children);
        for (name, (unit, _)) in self.unit_list() {
            let status = if self.unit_active(&unit) {
                RoleStatus::Running
            } else {
                RoleStatus::Exited(None)
            };
            out.push((name, status));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    fn problems(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .unit_list()
            .into_iter()
            .filter_map(|(name, (_, state))| {
                match HostAdmission::read(&unit_admission_file(&state))? {
                    HostAdmission::Refused { kind, message } => Some(format!(
                        "{name} refused to start ({kind:?}, checked by its own host): {message}"
                    )),
                    HostAdmission::Admitted => None,
                }
            })
            .collect();
        out.sort();
        out
    }
}

//! The supervisor seam (PLAN section 3): han-node runs the agent hosts as child processes, `han host --spec
//! <file>`, one per session. The node makes the [`HostSpec`] (through `han-registry`) and hands it over; this is
//! the trait it talks to and a spawner that starts the host as a process of the same binary.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use han_registry::{HostSpec, RunAs, spec_file, unit_spec_file};
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
    children: Mutex<HashMap<String, Child>>,
    /// Hosts running as another OS user, by name: their unit.
    units: Mutex<HashMap<String, String>>,
}

impl ProcessSpawner {
    /// A spawner that runs `program` (normally this binary's own path, `std::env::current_exe()`).
    pub fn new(program: impl Into<PathBuf>) -> Self {
        ProcessSpawner {
            program: program.into(),
            prefix: vec!["host".into(), "--spec".into()],
            systemctl: PathBuf::from("systemctl"),
            children: Mutex::new(HashMap::new()),
            units: Mutex::new(HashMap::new()),
        }
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

    fn systemctl(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        std::process::Command::new(&self.systemctl)
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
    fn start_as(&self, spec: RoleSpec, run_as: &RunAs) -> Result<(), SupervisorError> {
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
            .insert(spec.name, unit);
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Child>> {
        self.children.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl Supervisor for ProcessSpawner {
    fn start(&self, spec: RoleSpec) -> Result<(), SupervisorError> {
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
        let file = spec_file(&spec.spec.dirs.state, &spec.name);
        spec.spec
            .write(&file)
            .map_err(|e| SupervisorError::Spawn(format!("{}: {e}", file.display())))?;
        let inherited = ENV_ALLOW
            .iter()
            .filter_map(|k| std::env::var_os(k).map(|v| (k.to_string(), v)));
        let child = Command::new(&self.program)
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
        if let Some(unit) = unit {
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
        let units: Vec<(String, String)> = self
            .units
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(n, u)| (n.clone(), u.clone()))
            .collect();
        for (name, unit) in units {
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
}

//! The supervisor seam (PLAN section 3): han-node runs the local roles as child processes, `han host` per agent
//! session, `han exec`, and `han seq` when the device is a home. The roles themselves arrive in M4; this is the
//! trait the node talks to and a spawner that starts a role as a process of the same binary.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use tokio::process::{Child, Command};

/// The roles a node can supervise.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// `han host`: the ACP client, sandbox, egress proxy and worktree mirror of one agent session.
    Host,
    /// `han exec`: the executor, which holds the credentials and runs approved actions.
    Exec,
    /// `han seq`: the sequencer, when this device is a home.
    Seq,
}

impl Role {
    /// The subcommand of the `han` binary that runs the role.
    pub fn subcommand(self) -> &'static str {
        match self {
            Role::Host => "host",
            Role::Exec => "exec",
            Role::Seq => "seq",
        }
    }
}

/// What to start.
#[derive(Clone, Debug)]
pub struct RoleSpec {
    /// The role.
    pub role: Role,
    /// A name that identifies this instance (for example the session or agent); starting the same name again
    /// is a conflict.
    pub name: String,
    /// Arguments after the subcommand.
    pub args: Vec<String>,
    /// Environment variables to set on top of the allow-list (SR-08). Never a secret the role should not
    /// hold (SR-09): model keys go only to a host, GitHub credentials only to an executor.
    pub env: Vec<(String, String)>,
    /// Run the role as this OS user (SR-83); `None` runs it as the node's own user.
    pub run_as: Option<RunAs>,
}

/// Another OS user to run a role as. The node never needs root for it: the runner's admin installs the
/// `han-agent@.service` template once (docs/runner.md), and the node starts and stops that unit.
#[derive(Clone, Debug)]
pub struct RunAs {
    /// The OS user (the instance of the template).
    pub os_user: String,
    /// The user's state directory, where the node leaves the role's arguments (`spawn/host.json`) for the
    /// unit to read.
    pub state_dir: PathBuf,
}

/// The systemd unit that runs a role as `os_user`.
pub fn unit_of(os_user: &str) -> String {
    format!("han-agent@{os_user}.service")
}

/// The file the unit of an OS user reads its role's arguments from, under the user's state directory.
pub fn spawn_file(state_dir: &std::path::Path) -> PathBuf {
    state_dir.join("spawn").join("host.json")
}

/// The variables a child role inherits from the node (SR-08): what processes need to run on each OS, logging
/// settings, and nothing else. Everything else a role needs comes in [`RoleSpec::env`] or its arguments.
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
    "HAN_RELAY",
    "RUST_BACKTRACE",
    "NUMBER_OF_PROCESSORS",
];

/// Why a role could not be started or stopped.
#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    /// This node has no way to run the role.
    #[error("the {0} role is not available on this node")]
    Unavailable(&'static str),
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
    fn start(&self, spec: RoleSpec) -> Result<(), SupervisorError> {
        Err(SupervisorError::Unavailable(spec.role.subcommand()))
    }

    fn stop(&self, name: &str) -> Result<(), SupervisorError> {
        Err(SupervisorError::NotRunning(name.to_owned()))
    }

    fn status(&self) -> Vec<(String, RoleStatus)> {
        Vec::new()
    }
}

/// Starts roles as child processes: `<program> <subcommand> <args...>`. Children die with the spawner
/// (`kill_on_drop`), so a stopped node leaves no roles behind.
pub struct ProcessSpawner {
    program: PathBuf,
    systemctl: PathBuf,
    children: Mutex<HashMap<String, Child>>,
    /// Roles running as another OS user, by name: their unit.
    units: Mutex<HashMap<String, String>>,
}

impl ProcessSpawner {
    /// A spawner that runs `program` (normally this binary's own path, `std::env::current_exe()`).
    pub fn new(program: impl Into<PathBuf>) -> Self {
        ProcessSpawner {
            program: program.into(),
            systemctl: PathBuf::from("systemctl"),
            children: Mutex::new(HashMap::new()),
            units: Mutex::new(HashMap::new()),
        }
    }

    /// Uses this `systemctl` to start and stop the units of roles that run as another OS user.
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

    /// Starts a role as another OS user: its arguments go to the user's spawn file and the unit
    /// `han-agent@<user>` is started. One role per OS user at a time.
    fn start_as(&self, spec: RoleSpec, run_as: &RunAs) -> Result<(), SupervisorError> {
        if spec.role != Role::Host {
            return Err(SupervisorError::Unavailable(spec.role.subcommand()));
        }
        if !crate::agents::valid_os_user(&run_as.os_user) {
            return Err(SupervisorError::Spawn(format!(
                "{:?} is not an OS user name",
                run_as.os_user
            )));
        }
        let unit = unit_of(&run_as.os_user);
        if self.unit_active(&unit) {
            return Err(SupervisorError::Running(spec.name));
        }
        // The unit has no environment of ours: the one variable a host needs travels as its argument.
        let mut args = spec.args;
        for (k, v) in spec.env {
            if k != "HAN_HOME_TICKET" {
                return Err(SupervisorError::Spawn(format!(
                    "{k} cannot reach a role that runs as another user"
                )));
            }
            args.extend(["--home-ticket".to_owned(), v]);
        }
        let file = spawn_file(&run_as.state_dir);
        let spawn = |e: std::io::Error| SupervisorError::Spawn(format!("{}: {e}", file.display()));
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(spawn)?;
        }
        let text = serde_json::json!({ "args": args }).to_string();
        std::fs::write(&file, text).map_err(spawn)?;
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
            // A role that has ended may be started again.
            if matches!(child.try_wait(), Ok(None)) {
                return Err(SupervisorError::Running(spec.name));
            }
        }
        let inherited = ENV_ALLOW
            .iter()
            .filter_map(|k| std::env::var_os(k).map(|v| (k.to_string(), v)));
        let child = Command::new(&self.program)
            .arg(spec.role.subcommand())
            .args(&spec.args)
            .env_clear()
            .envs(inherited)
            .envs(spec.env.iter().map(|(k, v)| (k.clone(), v.clone())))
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

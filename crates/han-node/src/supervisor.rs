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
    children: Mutex<HashMap<String, Child>>,
}

impl ProcessSpawner {
    /// A spawner that runs `program` (normally this binary's own path, `std::env::current_exe()`).
    pub fn new(program: impl Into<PathBuf>) -> Self {
        ProcessSpawner {
            program: program.into(),
            children: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Child>> {
        self.children.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl Supervisor for ProcessSpawner {
    fn start(&self, spec: RoleSpec) -> Result<(), SupervisorError> {
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
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

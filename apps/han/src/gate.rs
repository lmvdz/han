//! The node's supervisor with a sandbox gate: an agent whose sandbox cannot work on this machine is refused
//! before a host is started, with the reason, instead of starting and losing its tools mid-session. The
//! decision is `han_host::sandbox::admit`; this only asks it for the agent's recorded sandbox and tells the
//! node's status what it found.

use std::sync::{Arc, OnceLock};

use han_host::sandbox::{self, ShimCommand};
use han_node::supervisor::{ProcessSpawner, RoleSpec, RoleStatus, Supervisor, SupervisorError};

/// [`ProcessSpawner`], after the sandbox check.
pub struct GatedSpawner {
    inner: ProcessSpawner,
    shim: ShimCommand,
    /// Why MXC cannot give a container a loopback port here, once the self-test has run.
    loopback: Arc<OnceLock<Option<String>>>,
}

impl GatedSpawner {
    /// Wraps `inner`. The MXC self-test (one container) starts now, on a thread, so the first agent start
    /// and the status find it done.
    pub fn new(inner: ProcessSpawner, shim: ShimCommand) -> Self {
        let loopback = Arc::new(OnceLock::new());
        let (cell, probe_shim) = (Arc::clone(&loopback), shim.clone());
        std::thread::spawn(move || {
            let blocked = sandbox::probe(&probe_shim)
                .into_iter()
                .find(|b| b.loopback_blocked)
                .map(|b| b.detail);
            let _ = cell.set(blocked);
        });
        GatedSpawner {
            inner,
            shim,
            loopback,
        }
    }
}

impl Supervisor for GatedSpawner {
    fn start(&self, spec: RoleSpec) -> Result<(), SupervisorError> {
        let s = &spec.spec.sandbox;
        sandbox::gate(&s.backend, s.allow_advisory, &self.shim)
            .map_err(|e| SupervisorError::SandboxUnavailable(e.to_string()))?;
        self.inner.start(spec)
    }

    fn stop(&self, name: &str) -> Result<(), SupervisorError> {
        self.inner.stop(name)
    }

    fn status(&self) -> Vec<(String, RoleStatus)> {
        self.inner.status()
    }

    fn problems(&self) -> Vec<String> {
        match self.loopback.get() {
            Some(Some(why)) => vec![format!(
                "sandbox mxc unavailable: {why}; fix: {}",
                sandbox::LOOPBACK_FIX
            )],
            _ => Vec::new(),
        }
    }
}

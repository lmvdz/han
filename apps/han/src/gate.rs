//! The node's supervisor with a sandbox gate: an agent whose sandbox cannot work is refused before its agent
//! starts, with the reason, instead of starting and losing its tools mid-session. The decision is
//! `han_host::sandbox::admit_for`, and the probe runs where the host will:
//!
//! - a host that is the node's own child (same OS user) is probed here, in the node's process: that is MXC on
//!   Windows, which must fail fast;
//! - a host in its own `han-agent@` unit (an `osUser` agent) is NOT probed here. The node's unit is hardened
//!   (`RestrictNamespaces=yes`, `ProcSubset=pid`, ...) so bwrap can never work in this process, by design, while
//!   it works in the host's unit. The host probes at its own start (`han host`) and, when its sandbox is
//!   unavailable, leaves a typed refusal that [`ProcessSpawner`] turns into the start's error and a node problem.
//!
//! There is no advisory fallback on either path: advisory needs `allow_advisory`, decided the same way.

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
        sandbox::admit_for(
            &s.backend,
            s.allow_advisory,
            spec.run_as.is_some(),
            &self.shim,
        )
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
        let mut out = self.inner.problems();
        if let Some(Some(why)) = self.loopback.get() {
            out.push(format!(
                "sandbox mxc unavailable: {why}; fix: {}",
                sandbox::LOOPBACK_FIX
            ));
        }
        out
    }
}

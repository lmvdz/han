//! `han-registry`: the contract between a node and the processes it launches.
//!
//! * [`HostSpec`]: everything a `han host` needs, as one serde value. The node writes it, `han host --spec`
//!   reads it.
//! * [`HomeRef`]: how a command or a spec names a home (a ticket, or a ServerName).
//! * [`Registry`]: what a machine knows about launching agents (`agents.json`, the harness records), and the one
//!   function that turns a [`Launch`] into a spec. The node does not read those files itself.
//!
//! The crate has no runtime: serde and the types only, so a face may depend on it.

pub mod agents;
mod home;
mod launch;
mod spec;

pub use agents::{
    AgentEdit, AgentSettings, AgentsFile, HarnessInstall, LOGIN_DIR, ToolchainSpec, safe_name,
    valid_os_user,
};
pub use home::{HomeRef, TICKET_PREFIX};
pub use launch::{Launch, LaunchError, Registry, Resolved, RunAs};
pub use spec::{
    Dirs, HarnessSpec, HostSpec, ProxySpec, SandboxPolicy, WorkSpec, spec_file, unit_spec_file,
};

//! `han-host`: runs one agent session (PLAN §3, §5 AgentDriver, §6.4; spec R-24, §8.4).
//!
//! * [`acp`]: the one ACP client (`agent-client-protocol` 3.3, pinned) behind [`acp::AgentDriver`].
//! * [`driver`]: harnesses as data: the ACP agent command, its sandbox profile, how it gets HAN's tools, and
//!   its model-credential mode (pi via `pi-acp` with an API key; Claude Code via `claude-agent-acp` on its own
//!   login).
//! * [`sandbox`]: launching the agent's whole process tree confined: MXC on Windows, bubblewrap on Linux, or
//!   none (advisory, only when the space allows it). The backend and its fidelity go into every status item.
//! * [`proxy`]: the egress proxy: an allow-list of CONNECT hosts, the model route with key injection (API-key
//!   mode), CONNECT-only for subscription mode, a per-session token, no private destinations, and logs without
//!   secrets.
//! * [`shim`]: the in-sandbox side: a loopback listener that forwards to the session's pipe or socket, and the
//!   MCP stub that tunnels stdio to han-mcp outside the sandbox.
//! * [`items`]: the agent's messages, thoughts and tool calls as `han.item/1` stream items.
//! * [`toolchain`]: language toolchains (Rust) inside the sandbox: read-only roots, per-agent caches, and the
//!   registry hosts the proxy then allows.
//! * [`status`], [`mask`], [`mirror`], [`r24`], [`host`]: status derivation, secret masking, worktree
//!   mirroring into han-timeline, the R-24 duties, and the session runner that ties them together.

pub mod acp;
pub mod driver;
pub mod glue;
pub mod host;
pub mod items;
pub mod mask;
pub mod mirror;
pub mod proxy;
pub mod r24;
pub mod sandbox;
pub mod shim;
pub mod status;
pub mod stderr_log;
pub mod toolchain;

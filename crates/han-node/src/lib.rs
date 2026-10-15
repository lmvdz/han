//! `han-node`: the HAN runtime of one device (PLAN section 3), and the node API its faces use.
//!
//! The crate has two halves:
//!
//! * [`api`] and [`rpc`] (always built): `call(Command)` and `view(Query)` as serde enums, the
//!   [`NodeApi`] trait, and the local JSON-RPC client. A face (TUI, GUI, MCP tools, integrations) depends on
//!   these and on nothing else of this crate: build it with `default-features = false`.
//! * The device runtime (feature `runtime`, on by default): the device keys behind a [`keys::KeyStore`], one
//!   endpoint, sync of followed spaces with verified receipts, the outbox in `node.db`, the views engine, the
//!   supervisor seam for child roles, and the JSON-RPC server.
//!
//! Time comes from an injected [`han_seq::Clock`] and the network from an injected [`han_net::Transport`],
//! so a whole team can be simulated in one process (`tests/sim.rs`).

pub mod api;
pub mod rpc;

pub use api::{ClassApi, NodeApi, NodeError, ViewStream};

#[cfg(feature = "runtime")]
pub mod db;
#[cfg(feature = "runtime")]
mod home;
#[cfg(feature = "runtime")]
pub mod keys;
#[cfg(feature = "runtime")]
mod model;
#[cfg(feature = "runtime")]
mod node;
#[cfg(feature = "runtime")]
pub mod presence;
#[cfg(feature = "runtime")]
mod retry;
#[cfg(feature = "runtime")]
pub mod supervisor;
#[cfg(feature = "runtime")]
mod sync;
#[cfg(feature = "runtime")]
pub mod timeline;
#[cfg(feature = "runtime")]
pub mod views;

#[cfg(feature = "runtime")]
pub use node::{Freshness, Node, NodeConfig, NodeParts};
#[cfg(feature = "runtime")]
pub use sync::{ServerKeys, StaticKeys, WellKnownKeys};

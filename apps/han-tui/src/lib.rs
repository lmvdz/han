//! `han tui`: the terminal face of HAN. Board first: live rows from `view(Board)` grouped by issue, person or
//! state; Enter opens a session pane with its live stream and a steering field; a side pane holds the
//! approvals the local actor can give, and signing takes two deliberate steps.
//!
//! This is a face (PLAN section 3): it talks to the running `han node` only through the node API over the
//! local pipe or socket ([`han_node::rpc::RpcClient`]) and depends on nothing of han-node but `api` and `rpc`
//! (`crates/han-node/tests/layering.rs` checks it).
//!
//! The parts:
//!
//! * [`app`]: the whole state as a pure state machine, `update(Msg) -> Vec<Action>`;
//! * [`ui`]: drawing, a pure function of the state;
//! * [`host`]: carries out the actions against a [`han_node::NodeApi`], and reconnects when the daemon restarts;
//! * [`term`]: the real terminal, wired to the three above;
//! * [`testing`]: a fake node and a test terminal, for the tests of this crate and of the node's.

pub mod app;
pub mod approvals;
pub mod board;
pub mod config;
pub mod filter;
pub mod host;
pub mod input;
pub mod msg;
pub mod session;
pub mod term;
pub mod testing;
pub mod text;
pub mod ui;

pub use term::{Options, run};

//! The command-line faces that only talk to the running node over its pipe (PLAN section 3): `han board`,
//! `han space`, `han task`, `han device` and `han outbox`. This crate depends on the node API types and the
//! JSON-RPC client and on nothing of the runtime; `crates/han-node/tests/layering.rs` checks its dependency
//! tree, so the rule holds by construction and not by a search of the sources.

pub mod board;
pub mod device;
pub mod outbox;
pub mod space;
pub mod task;
// scratch

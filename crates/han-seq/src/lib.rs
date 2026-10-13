//! The HAN sequencer engine (architecture.md section 4.1): validation, Policy, profile pinning, receipts,
//! the hash chain, fold and durability, with a single writer, storage behind a trait, an injected clock,
//! fan-out hooks, and the carrier-independent call dispatcher.
//!
//! * [`Home`] is the sequencer of one server; [`Home::connect`] gives a [`Conn`] per connection.
//! * [`Conn::handle`] takes one request frame and returns what a carrier writes back (G-07, G-08).
//! * [`Store`] ([`SqliteStore`], [`MemStore`]) is where records are kept; [`Clock`] is where time comes from.
//! * [`build`] makes signed entries; [`stream_item`] checks session stream items.
//!
//! There is no network and no async runtime here: han-net carries the frames.

mod services;
pub mod auth;
pub mod build;
mod calls;
pub mod clock;
mod conn;
pub mod error;
mod fanout;
mod home;
pub mod identity;
mod jv;
mod paging;
pub mod store;
pub mod stream_item;
pub mod time;

pub use calls::{
    MAX_BLOB_PART_BYTES, MAX_BLOB_RANGE_BYTES, MAX_CLOCK_SKEW_SECONDS, MAX_FOLLOWS,
    MAX_SIGNAL_BYTES, MAX_STATE_ITEMS, MAX_STREAM_ITEM_BYTES,
};
pub use clock::{Clock, SystemClock, TestClock};
pub use conn::{CallFeed, Conn, Handled};
pub use error::{Code, WireError};
pub use fanout::{Hooks, NoHooks};
pub use home::{
    Directory, EPOCH, Home, HomeConfig, NoDirectory, OpenError, RateLimit, ServerInfo,
    profile_policy,
};
pub use identity::{PinKey, PinView, ServerKey};
pub use store::{MemStore, SqliteStore, Store, StoreError};

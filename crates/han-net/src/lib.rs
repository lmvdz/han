//! The `han/1` carrier (architecture.md section 8.2): HAN frames over QUIC, dialled by key.
//!
//! * [`frame`]: a 4-byte big-endian length, then I-JSON.
//! * [`Transport`] and [`Connection`]: what the carrier needs of a network. [`IrohTransport`] is QUIC over
//!   iroh (ALPN `han/1`, n0's public relays for now); [`MemTransport`] is an in-process network for tests
//!   and simulations.
//! * [`serve`] runs a [`han_seq::Home`] on a transport: the control stream with the Challenge, one task per
//!   call stream (handled in stream order), one unidirectional stream per live call, the peer filter (N-02)
//!   at connection and call time.
//! * [`Client`] and [`RawConn`] dial a node, check the Challenge, say `hello` and make calls.
//! * [`wellknown`], [`ticket`], [`control`], [`typed`]: the documents and formats of section 4.3, N-03 and the typed one-way streams of G-07.
//! * [`wirevec`]: a runner for the wire conformance vectors over any transport.

pub mod addr;
pub mod client;
pub mod control;
pub mod dial;
pub mod error;
pub mod frame;
pub mod mem;
pub mod quic;
pub mod server;
pub mod ticket;
pub mod transport;
pub mod typed;
pub mod wellknown;
pub mod wirevec;

pub use addr::{NodeAddr, NodeId};
pub use client::{CallStream, Client, Inbound, RawConn};
pub use error::{NetError, Result};
pub use mem::{MemNetwork, MemTransport};
pub use quic::{ALPN, IrohOptions, IrohTransport, RelayChoice};
pub use server::{ServeOptions, ServerHandle, serve};
pub use ticket::InviteTicket;
pub use transport::{Connection, Transport};

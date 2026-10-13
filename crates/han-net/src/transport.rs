//! What the carrier needs of a network, as traits, so a simulation can stand in for QUIC (M3, turmoil).
//!
//! A [`Transport`] dials and accepts [`Connection`]s between authenticated node ids. A connection opens and
//! accepts bidirectional and unidirectional streams, as QUIC does; streams are tokio byte streams.

use std::future::Future;

use tokio::io::{AsyncRead, AsyncWrite};

use crate::addr::{NodeAddr, NodeId};
use crate::error::Result;

/// The sending half of a stream.
pub trait SendStream: AsyncWrite + Unpin + Send + 'static {
    /// Says that no more data follows. Does not wait for the peer.
    fn finish(&mut self) -> impl Future<Output = Result<()>> + Send;

    /// After [`SendStream::finish`]: waits until the peer has taken everything, or the connection is gone.
    fn delivered(&mut self) -> impl Future<Output = ()> + Send;
}

/// The receiving half of a stream.
pub trait RecvStream: AsyncRead + Unpin + Send + 'static {}

/// A connection to or from one authenticated node.
pub trait Connection: Clone + Send + Sync + 'static {
    /// Its sending streams.
    type Send: SendStream;
    /// Its receiving streams.
    type Recv: RecvStream;

    /// The peer's node id, authenticated by the handshake.
    fn peer(&self) -> NodeId;

    /// Opens a bidirectional stream.
    fn open_bi(&self) -> impl Future<Output = Result<(Self::Send, Self::Recv)>> + Send;

    /// Waits for the peer to open a bidirectional stream; streams arrive in the order they were opened.
    fn accept_bi(&self) -> impl Future<Output = Result<(Self::Send, Self::Recv)>> + Send;

    /// Opens a unidirectional stream for sending.
    fn open_uni(&self) -> impl Future<Output = Result<Self::Send>> + Send;

    /// Waits for the peer to open a unidirectional stream; streams arrive in the order they were opened.
    fn accept_uni(&self) -> impl Future<Output = Result<Self::Recv>> + Send;

    /// Closes the connection with an application error code and a reason.
    fn close(&self, code: u32, reason: &str);

    /// Resolves when the connection is closed, by either side.
    fn closed(&self) -> impl Future<Output = ()> + Send;
}

/// Dials and accepts connections by node id.
pub trait Transport: Clone + Send + Sync + 'static {
    /// Its connections.
    type Conn: Connection;

    /// This node's id.
    fn node_id(&self) -> NodeId;

    /// This node's addresses, as other nodes can dial them.
    fn addr(&self) -> NodeAddr;

    /// Connects to `addr`'s node, authenticating it by its id.
    fn dial(&self, addr: &NodeAddr) -> impl Future<Output = Result<Self::Conn>> + Send;

    /// Waits for the next incoming connection; `None` when the transport is shut down.
    fn accept(&self) -> impl Future<Output = Option<Self::Conn>> + Send;
}

/// Application error codes of a closed connection.
pub mod close {
    /// Normal end.
    pub const OK: u32 = 0;
    /// The peer broke a framing or protocol rule (G-07).
    pub const PROTOCOL: u32 = 1;
    /// Over a limit (G-08): too many connections from one node.
    pub const LIMIT: u32 = 2;
    /// The node no longer vouches for the actor (X14).
    pub const REVOKED: u32 = 3;
}

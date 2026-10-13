//! The iroh implementation of [`Transport`]: QUIC with TLS 1.3, ALPN `han/1`, dialled by node id (G-07).
//!
//! The node id is the Ed25519 public key of the device's signing key (N-01), so one key both signs entries
//! and authenticates the endpoint.

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};

use han_core::base64url;
use iroh::endpoint::{Connection as IrohConnection, QuicTransportConfig, RecvStream as IrohRecv, SendStream as IrohSend, VarInt, presets};
use iroh::protocol::{IncomingFilter, IncomingFilterOutcome};
use iroh::{Endpoint, EndpointAddr, PublicKey, RelayMode, RelayUrl, SecretKey, TransportAddr};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{Mutex, mpsc};

use crate::addr::{NodeAddr, NodeId};
use crate::error::{NetError, Result};
use crate::transport::{Connection, RecvStream, SendStream, Transport};

/// The ALPN of the binding.
pub const ALPN: &[u8] = b"han/1";

/// Concurrent streams of each kind a peer may open: one unidirectional stream per live call, at most
/// `maxFollows` of them, and some for the control stream.
const STREAMS: u32 = 1024;

/// Handshakes in flight before a source address must prove itself with a retry token.
const HANDSHAKE_BUDGET: usize = 128;

/// Which relays an endpoint uses.
#[derive(Clone, Debug)]
pub enum RelayChoice {
    /// n0's public relays, and n0's DNS discovery (the default for now, PLAN 6.8).
    N0,
    /// No relay and no discovery: direct addresses only.
    Disabled,
    /// These relay URLs, and no discovery.
    Custom(Vec<String>),
}

/// How to bind an endpoint.
pub struct IrohOptions {
    /// The 32-byte seed of the device key; the node id is its public key.
    pub secret: [u8; 32],
    /// The UDP address to bind (any address and a free port by default).
    pub bind: Option<SocketAddr>,
    /// Relays and discovery.
    pub relay: RelayChoice,
    /// An extra check on every incoming connection before its handshake (N-02, G-08), run first.
    pub incoming_filter: Option<IncomingFilter>,
}

impl IrohOptions {
    /// Options for the key with seed `secret`, using n0's relays.
    pub fn new(secret: [u8; 32]) -> Self {
        IrohOptions {
            secret,
            bind: None,
            relay: RelayChoice::N0,
            incoming_filter: None,
        }
    }
}

/// A QUIC endpoint speaking `han/1`.
#[derive(Clone)]
pub struct IrohTransport {
    endpoint: Endpoint,
    incoming: Arc<Mutex<mpsc::UnboundedReceiver<IrohConnection>>>,
}

fn dial_err(e: impl std::fmt::Display) -> NetError {
    NetError::Dial(e.to_string())
}

impl IrohTransport {
    /// Binds the endpoint and starts accepting.
    pub async fn bind(opts: IrohOptions) -> Result<Self> {
        let config = QuicTransportConfig::builder()
            .max_concurrent_uni_streams(VarInt::from_u32(STREAMS))
            .max_concurrent_bidi_streams(VarInt::from_u32(STREAMS))
            .build();
        let mut builder = match &opts.relay {
            RelayChoice::N0 => Endpoint::builder(presets::N0),
            RelayChoice::Disabled => {
                Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled)
            }
            RelayChoice::Custom(urls) => {
                let urls = urls
                    .iter()
                    .map(|u| u.parse::<RelayUrl>().map_err(dial_err))
                    .collect::<Result<Vec<_>>>()?;
                Endpoint::builder(presets::Minimal).relay_mode(RelayMode::custom(urls))
            }
        }
        .secret_key(SecretKey::from_bytes(&opts.secret))
        .alpns(vec![ALPN.to_vec()])
        .transport_config(config);
        if let Some(addr) = opts.bind {
            builder = builder.bind_addr(addr).map_err(dial_err)?;
        }
        let endpoint = builder.bind().await.map_err(dial_err)?;
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(pump(endpoint.clone(), opts.incoming_filter, tx));
        Ok(IrohTransport {
            endpoint,
            incoming: Arc::new(Mutex::new(rx)),
        })
    }

    /// The addresses this endpoint is bound to, with the unspecified address replaced by loopback: enough to
    /// dial it from the same machine.
    pub fn loopback_addr(&self) -> NodeAddr {
        let addrs = self.endpoint.bound_sockets().into_iter().map(|a| {
            let ip = if a.ip().is_unspecified() {
                if a.is_ipv4() {
                    std::net::Ipv4Addr::LOCALHOST.into()
                } else {
                    std::net::Ipv6Addr::LOCALHOST.into()
                }
            } else {
                a.ip()
            };
            SocketAddr::new(ip, a.port())
        });
        NodeAddr::direct(&self.node_id(), addrs)
    }

    /// Waits until the endpoint has a home relay (or gives up after `timeout`); a no-op without relays.
    pub async fn online(&self, timeout: std::time::Duration) {
        let _ = tokio::time::timeout(timeout, self.endpoint.online()).await;
    }

    /// Closes the endpoint and its connections.
    pub async fn shutdown(&self) {
        self.endpoint.close().await;
    }
}

/// Accepts incoming connections: filters them before the handshake, and finishes handshakes concurrently.
async fn pump(
    endpoint: Endpoint,
    filter: Option<IncomingFilter>,
    out: mpsc::UnboundedSender<IrohConnection>,
) {
    let in_flight = Arc::new(AtomicUsize::new(0));
    while let Some(incoming) = endpoint.accept().await {
        let verdict = filter
            .as_ref()
            .map_or(IncomingFilterOutcome::Accept, |f| f(&incoming));
        let verdict = match verdict {
            IncomingFilterOutcome::Accept
                if in_flight.load(Ordering::Relaxed) >= HANDSHAKE_BUDGET =>
            {
                if incoming.remote_addr_validated() {
                    IncomingFilterOutcome::Reject
                } else {
                    IncomingFilterOutcome::Retry
                }
            }
            other => other,
        };
        match verdict {
            IncomingFilterOutcome::Accept => {}
            IncomingFilterOutcome::Retry => {
                let _ = incoming.retry();
                continue;
            }
            IncomingFilterOutcome::Ignore => {
                incoming.ignore();
                continue;
            }
            _ => {
                incoming.refuse();
                continue;
            }
        }
        let (out, in_flight) = (out.clone(), Arc::clone(&in_flight));
        in_flight.fetch_add(1, Ordering::Relaxed);
        tokio::spawn(async move {
            if let Ok(accepting) = incoming.accept()
                && let Ok(conn) = accepting.await
                && conn.alpn() == ALPN
            {
                let _ = out.send(conn);
            }
            in_flight.fetch_sub(1, Ordering::Relaxed);
        });
    }
}

fn to_endpoint_addr(addr: &NodeAddr) -> Result<EndpointAddr> {
    let bytes: [u8; 32] = base64url::decode(&addr.node)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| NetError::Dial("not a node id".into()))?;
    let key = PublicKey::from_bytes(&bytes).map_err(dial_err)?;
    let mut out = EndpointAddr::new(key);
    for a in addr.socket_addrs() {
        out = out.with_ip_addr(a);
    }
    for r in &addr.relays {
        out = out.with_relay_url(r.parse::<RelayUrl>().map_err(dial_err)?);
    }
    Ok(out)
}

fn node_id_of(key: &PublicKey) -> NodeId {
    base64url::encode(key.as_bytes())
}

impl Transport for IrohTransport {
    type Conn = IrohConn;

    fn node_id(&self) -> NodeId {
        node_id_of(&self.endpoint.id())
    }

    fn addr(&self) -> NodeAddr {
        let a = self.endpoint.addr();
        let mut out = NodeAddr {
            node: self.node_id(),
            addrs: Vec::new(),
            relays: Vec::new(),
        };
        for t in a.addrs {
            match t {
                TransportAddr::Ip(sa) => out.addrs.push(sa.to_string()),
                TransportAddr::Relay(url) => out.relays.push(url.to_string()),
                _ => {}
            }
        }
        out
    }

    async fn dial(&self, addr: &NodeAddr) -> Result<IrohConn> {
        let target = to_endpoint_addr(addr)?;
        let conn = self.endpoint.connect(target, ALPN).await.map_err(dial_err)?;
        Ok(IrohConn(conn))
    }

    async fn accept(&self) -> Option<IrohConn> {
        self.incoming.lock().await.recv().await.map(IrohConn)
    }
}

/// A QUIC connection.
#[derive(Clone)]
pub struct IrohConn(IrohConnection);

/// The sending half of a QUIC stream.
pub struct IrohSendStream(IrohSend);
/// The receiving half of a QUIC stream.
pub struct IrohRecvStream(IrohRecv);

fn closed_err(e: impl std::fmt::Display) -> NetError {
    NetError::Closed(e.to_string())
}

impl Connection for IrohConn {
    type Send = IrohSendStream;
    type Recv = IrohRecvStream;

    fn peer(&self) -> NodeId {
        node_id_of(&self.0.remote_id())
    }

    async fn open_bi(&self) -> Result<(IrohSendStream, IrohRecvStream)> {
        let (s, r) = self.0.open_bi().await.map_err(closed_err)?;
        Ok((IrohSendStream(s), IrohRecvStream(r)))
    }

    async fn accept_bi(&self) -> Result<(IrohSendStream, IrohRecvStream)> {
        let (s, r) = self.0.accept_bi().await.map_err(closed_err)?;
        Ok((IrohSendStream(s), IrohRecvStream(r)))
    }

    async fn open_uni(&self) -> Result<IrohSendStream> {
        Ok(IrohSendStream(self.0.open_uni().await.map_err(closed_err)?))
    }

    async fn accept_uni(&self) -> Result<IrohRecvStream> {
        Ok(IrohRecvStream(self.0.accept_uni().await.map_err(closed_err)?))
    }

    fn close(&self, code: u32, reason: &str) {
        self.0.close(VarInt::from_u32(code), reason.as_bytes());
    }

    async fn closed(&self) {
        let _ = self.0.closed().await;
    }
}

impl AsyncWrite for IrohSendStream {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        AsyncWrite::poll_write(Pin::new(&mut self.0), cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.0), cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.0), cx)
    }
}

impl AsyncRead for IrohRecvStream {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        AsyncRead::poll_read(Pin::new(&mut self.0), cx, buf)
    }
}

impl SendStream for IrohSendStream {
    async fn finish(&mut self) -> Result<()> {
        self.0.finish().map_err(closed_err)
    }

    async fn delivered(&mut self) {
        let _ = self.0.stopped().await;
    }
}

impl RecvStream for IrohRecvStream {}

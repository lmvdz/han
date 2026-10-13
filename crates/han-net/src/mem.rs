//! An in-process network: nodes dial each other by node id over tokio duplex pipes. It stands in for QUIC in
//! tests and simulations (M3), and carries the same frames the same way.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};
use tokio::sync::{mpsc, watch};

use crate::addr::{NodeAddr, NodeId};
use crate::error::{NetError, Result};
use crate::transport::{Connection, RecvStream, SendStream, Transport};

/// Bytes a pipe buffers before a writer waits for the reader.
const PIPE_BYTES: usize = 256 * 1024;

type Bi = (MemSend, MemRecv);

/// The nodes that can reach one another.
#[derive(Clone, Default)]
pub struct MemNetwork {
    nodes: Arc<Mutex<HashMap<NodeId, mpsc::UnboundedSender<MemConn>>>>,
}

/// A node of a [`MemNetwork`].
#[derive(Clone)]
pub struct MemTransport {
    id: NodeId,
    net: MemNetwork,
    incoming: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<MemConn>>>,
}

impl MemTransport {
    /// Joins `net` as `node`.
    pub fn new(net: &MemNetwork, node: &str) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        net.nodes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(node.to_owned(), tx);
        MemTransport {
            id: node.to_owned(),
            net: net.clone(),
            incoming: Arc::new(tokio::sync::Mutex::new(rx)),
        }
    }
}

/// The sending half of a pipe.
pub struct MemSend(DuplexStream);
/// The receiving half of a pipe.
pub struct MemRecv(DuplexStream);

impl AsyncWrite for MemSend {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

impl AsyncRead for MemRecv {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl SendStream for MemSend {
    async fn finish(&mut self) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        self.0.shutdown().await?;
        Ok(())
    }

    async fn delivered(&mut self) {}
}

impl RecvStream for MemRecv {}

fn pipe() -> (MemSend, MemRecv) {
    let (a, b) = tokio::io::duplex(PIPE_BYTES);
    (MemSend(a), MemRecv(b))
}

/// One end of an in-process connection.
#[derive(Clone)]
pub struct MemConn {
    peer: NodeId,
    bi_out: mpsc::UnboundedSender<Bi>,
    bi_in: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<Bi>>>,
    uni_out: mpsc::UnboundedSender<MemRecv>,
    uni_in: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<MemRecv>>>,
    closed: Arc<watch::Sender<Option<(u32, String)>>>,
}

impl MemConn {
    fn pair(a: &str, b: &str) -> (MemConn, MemConn) {
        let (bi_ab, bi_ab_rx) = mpsc::unbounded_channel();
        let (bi_ba, bi_ba_rx) = mpsc::unbounded_channel();
        let (uni_ab, uni_ab_rx) = mpsc::unbounded_channel();
        let (uni_ba, uni_ba_rx) = mpsc::unbounded_channel();
        let closed = Arc::new(watch::channel(None).0);
        let end = |peer: &str, bi_out, bi_in, uni_out, uni_in| MemConn {
            peer: peer.to_owned(),
            bi_out,
            bi_in: Arc::new(tokio::sync::Mutex::new(bi_in)),
            uni_out,
            uni_in: Arc::new(tokio::sync::Mutex::new(uni_in)),
            closed: Arc::clone(&closed),
        };
        (
            end(b, bi_ab, bi_ba_rx, uni_ab, uni_ba_rx),
            end(a, bi_ba, bi_ab_rx, uni_ba, uni_ab_rx),
        )
    }

    /// Why the connection closed, if it did: the code and reason given to `close`.
    pub fn close_reason(&self) -> Option<(u32, String)> {
        self.closed.borrow().clone()
    }

    async fn closed_wait(&self) {
        let mut rx = self.closed.subscribe();
        let _ = rx.wait_for(Option::is_some).await;
    }
}

impl Connection for MemConn {
    type Send = MemSend;
    type Recv = MemRecv;

    fn peer(&self) -> NodeId {
        self.peer.clone()
    }

    async fn open_bi(&self) -> Result<Bi> {
        if self.close_reason().is_some() {
            return Err(NetError::Closed("connection closed".into()));
        }
        let (c2s_w, c2s_r) = pipe();
        let (s2c_w, s2c_r) = pipe();
        self.bi_out
            .send((s2c_w, c2s_r))
            .map_err(|_| NetError::Closed("peer gone".into()))?;
        Ok((c2s_w, s2c_r))
    }

    async fn accept_bi(&self) -> Result<Bi> {
        let mut rx = self.bi_in.lock().await;
        tokio::select! {
            item = rx.recv() => item.ok_or_else(|| NetError::Closed("peer gone".into())),
            () = self.closed_wait() => Err(NetError::Closed("connection closed".into())),
        }
    }

    async fn open_uni(&self) -> Result<MemSend> {
        if self.close_reason().is_some() {
            return Err(NetError::Closed("connection closed".into()));
        }
        let (w, r) = pipe();
        self.uni_out
            .send(r)
            .map_err(|_| NetError::Closed("peer gone".into()))?;
        Ok(w)
    }

    async fn accept_uni(&self) -> Result<MemRecv> {
        let mut rx = self.uni_in.lock().await;
        tokio::select! {
            item = rx.recv() => item.ok_or_else(|| NetError::Closed("peer gone".into())),
            () = self.closed_wait() => Err(NetError::Closed("connection closed".into())),
        }
    }

    fn close(&self, code: u32, reason: &str) {
        self.closed.send_if_modified(|c| {
            let first = c.is_none();
            if first {
                *c = Some((code, reason.to_owned()));
            }
            first
        });
    }

    async fn closed(&self) {
        self.closed_wait().await;
    }
}

impl Transport for MemTransport {
    type Conn = MemConn;

    fn node_id(&self) -> NodeId {
        self.id.clone()
    }

    fn addr(&self) -> NodeAddr {
        NodeAddr {
            node: self.id.clone(),
            addrs: Vec::new(),
            relays: Vec::new(),
        }
    }

    async fn dial(&self, addr: &NodeAddr) -> Result<MemConn> {
        let target = self
            .net
            .nodes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&addr.node)
            .cloned()
            .ok_or_else(|| NetError::Dial(format!("no node {}", addr.node)))?;
        let (mine, theirs) = MemConn::pair(&self.id, &addr.node);
        target
            .send(theirs)
            .map_err(|_| NetError::Dial("node is gone".into()))?;
        Ok(mine)
    }

    async fn accept(&self) -> Option<MemConn> {
        self.incoming.lock().await.recv().await
    }
}

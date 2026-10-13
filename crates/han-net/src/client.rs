//! The client side of the carrier (G-07, G-08): [`RawConn`] moves frames, [`Client`] makes calls.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use han_core::json::{self, Value};
use han_core::{Receipt, SigningKey};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use crate::addr::{NodeAddr, NodeId};
use crate::control::{Challenge, challenge_verdict};
use crate::error::{NetError, Result};
use crate::frame::{read_frame, write_frame};
use crate::transport::{Connection, SendStream, Transport};

/// What arrives on a connection, from any of its streams.
#[derive(Debug, PartialEq, Eq)]
pub enum Inbound {
    /// A frame of a reply or of a live call.
    Frame(String),
    /// The connection is closed.
    Closed,
}

/// A connection at the frame level: the control stream's Challenge, frames out, and every frame in (replies
/// on call streams and deliveries on the server's unidirectional streams) merged into one queue.
pub struct RawConn<C: Connection> {
    conn: C,
    inbox: mpsc::UnboundedReceiver<Inbound>,
    tx: mpsc::UnboundedSender<Inbound>,
    challenge_frame: String,
}

impl<C: Connection> RawConn<C> {
    /// Wraps a connection: reads the control stream's first frame (the Challenge) and starts collecting
    /// frames.
    pub async fn open(conn: C) -> Result<Self> {
        let (tx, inbox) = mpsc::unbounded_channel();
        let mut control = conn.accept_uni().await?;
        let challenge_frame = read_frame(&mut control)
            .await?
            .ok_or_else(|| NetError::Protocol("no Challenge on the control stream".into()))?;
        // Anything more on the control stream is a protocol error (G-07).
        let guard = tx.clone();
        let guard_conn = conn.clone();
        tokio::spawn(async move {
            if matches!(read_frame(&mut control).await, Ok(Some(_))) {
                guard_conn.close(crate::transport::close::PROTOCOL, "control stream");
                let _ = guard.send(Inbound::Closed);
            }
        });
        let feeds = conn.clone();
        let feed_tx = tx.clone();
        tokio::spawn(async move {
            while let Ok(mut stream) = feeds.accept_uni().await {
                let out = feed_tx.clone();
                tokio::spawn(async move {
                    while let Ok(Some(frame)) = read_frame(&mut stream).await {
                        if out.send(Inbound::Frame(frame)).is_err() {
                            break;
                        }
                    }
                });
            }
        });
        let watcher = conn.clone();
        let closed_tx = tx.clone();
        tokio::spawn(async move {
            watcher.closed().await;
            let _ = closed_tx.send(Inbound::Closed);
        });
        Ok(RawConn {
            conn,
            inbox,
            tx,
            challenge_frame,
        })
    }

    /// The text of the Challenge frame.
    pub fn challenge_frame(&self) -> &str {
        &self.challenge_frame
    }

    /// The peer's node id.
    pub fn peer(&self) -> NodeId {
        self.conn.peer()
    }

    /// Sends one request frame on a new call stream.
    pub async fn send(&self, text: &str) -> Result<()> {
        self.send_payload(text.as_bytes()).await
    }

    /// Sends one frame whose payload is exactly `payload` (a test can send bytes that are not UTF-8).
    pub async fn send_payload(&self, payload: &[u8]) -> Result<()> {
        let (mut send, mut recv) = self.conn.open_bi().await?;
        let mut bytes = u32::try_from(payload.len())
            .map_err(|_| NetError::Frame("frame too long"))?
            .to_be_bytes()
            .to_vec();
        bytes.extend_from_slice(payload);
        send.write_all(&bytes).await?;
        send.finish().await?;
        let out = self.tx.clone();
        tokio::spawn(async move {
            while let Ok(Some(frame)) = read_frame(&mut recv).await {
                if out.send(Inbound::Frame(frame)).is_err() {
                    break;
                }
            }
        });
        Ok(())
    }

    /// Waits for the next frame, or `None` after `timeout`.
    pub async fn recv(&mut self, timeout: Duration) -> Option<Inbound> {
        tokio::time::timeout(timeout, self.inbox.recv()).await.ok().flatten()
    }

    /// Closes the connection.
    pub fn close(&self) {
        self.conn.close(crate::transport::close::OK, "bye");
    }

    /// Whether the connection has closed.
    pub async fn is_closed(&self, within: Duration) -> bool {
        tokio::time::timeout(within, self.conn.closed()).await.is_ok()
    }

    pub(crate) fn into_parts(self) -> (C, mpsc::UnboundedReceiver<Inbound>) {
        (self.conn, self.inbox)
    }
}

type Pending = Arc<Mutex<HashMap<u64, mpsc::UnboundedSender<Value>>>>;

/// A client of a home: dials a node, checks the Challenge, says `hello`, and makes calls.
pub struct Client<C: Connection> {
    conn: C,
    next_id: AtomicU64,
    pending: Pending,
    challenge: Challenge,
    dialed: String,
}

/// A live call's items: records, signals or stream items, until `end`, an error or `cancel`.
pub struct CallStream {
    id: u64,
    rx: mpsc::UnboundedReceiver<Value>,
}

impl CallStream {
    /// The request id.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The next item; `None` at `end` or when the call ended; `Err` for an error frame.
    pub async fn next(&mut self) -> Option<Result<Value>> {
        let frame = self.rx.recv().await?;
        if let Some(e) = frame.get("error") {
            return Some(Err(NetError::Remote(wire_error(e))));
        }
        if frame.get("end").is_some() {
            return None;
        }
        Some(Ok(frame["item"].clone()))
    }

    /// Like [`CallStream::next`], giving up after `timeout`.
    pub async fn next_within(&mut self, timeout: Duration) -> Option<Result<Value>> {
        tokio::time::timeout(timeout, self.next()).await.ok().flatten()
    }
}

fn wire_error(e: &Value) -> han_seq::WireError {
    use han_seq::{Code, WireError};
    let message = e["message"].as_str().unwrap_or_default().to_owned();
    let mut w = match e["code"].as_str() {
        Some("denied") => WireError::denied(message),
        Some("not_found") => WireError::not_found(message),
        Some("unavailable") => WireError::unavailable(message),
        Some("limit") => WireError::limit(message, e["retryAfterMs"].as_u64().unwrap_or(0)),
        Some("moved") => WireError::moved(e["movedTo"].as_str().unwrap_or_default()),
        _ => WireError::invalid(message),
    };
    if let Some(ms) = e["retryAfterMs"].as_u64() {
        w = w.retry_after(ms);
    }
    let _: Code = w.code;
    w
}

impl<C: Connection> Client<C> {
    /// Dials `addr`, reads the Challenge and checks it (G-07, G-08): its `server` must be `dialed`, and the
    /// node reached must be one of `trusted_nodes` (the node ids `dialed`'s well-known document lists, or a
    /// ticket's node).
    pub async fn connect<T: Transport<Conn = C>>(
        transport: &T,
        addr: &NodeAddr,
        dialed: &str,
        trusted_nodes: &[NodeId],
    ) -> Result<Self> {
        let conn = transport.dial(addr).await?;
        let raw = RawConn::open(conn).await?;
        let challenge = challenge_verdict(
            &[raw.challenge_frame().to_owned()],
            dialed,
            &raw.peer(),
            trusted_nodes,
        )
        .map_err(|why| NetError::Protocol(format!("abort: {why}")))?;
        let (conn, mut inbox) = raw.into_parts();
        let pending: Pending = Arc::default();
        let routes = Arc::clone(&pending);
        tokio::spawn(async move {
            while let Some(item) = inbox.recv().await {
                let Inbound::Frame(text) = item else {
                    break;
                };
                let Ok(frame) = json::parse(text.as_bytes()) else {
                    continue;
                };
                let id = frame["id"].as_u64().unwrap_or(0);
                let mut routes = routes.lock().unwrap_or_else(|p| p.into_inner());
                if id == 0 {
                    routes.clear(); // an error with id 0 fails every open request (G-07)
                } else if let Some(tx) = routes.get(&id) {
                    let last = frame.get("ok").is_some() || frame.get("error").is_some() || frame.get("end").is_some();
                    let _ = tx.send(frame);
                    if last {
                        routes.remove(&id);
                    }
                }
            }
            routes.lock().unwrap_or_else(|p| p.into_inner()).clear();
        });
        Ok(Client {
            conn,
            next_id: AtomicU64::new(1),
            pending,
            challenge,
            dialed: dialed.to_owned(),
        })
    }

    /// The Challenge this connection was issued.
    pub fn challenge(&self) -> &Challenge {
        &self.challenge
    }

    /// The connection, to close it or watch it.
    pub fn connection(&self) -> &C {
        &self.conn
    }

    async fn start(&self, call: &str, args: Value) -> Result<CallStream> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::unbounded_channel();
        self.pending.lock().unwrap_or_else(|p| p.into_inner()).insert(id, tx);
        let request = han_seq_obj([
            ("id", Value::Int(i64::try_from(id).unwrap_or(i64::MAX))),
            ("call", Value::Str(call.to_owned())),
            ("args", args),
        ]);
        let text = String::from_utf8(json::canonicalize(&request).map_err(|e| NetError::Protocol(e.to_string()))?)
            .map_err(|e| NetError::Protocol(e.to_string()))?;
        let (mut send, mut recv) = self.conn.open_bi().await?;
        write_frame(&mut send, &text).await?;
        send.finish().await?;
        let routes = Arc::clone(&self.pending);
        tokio::spawn(async move {
            while let Ok(Some(frame)) = read_frame(&mut recv).await {
                let Ok(v) = json::parse(frame.as_bytes()) else {
                    continue;
                };
                let id = v["id"].as_u64().unwrap_or(0);
                let mut routes = routes.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(tx) = routes.get(&id) {
                    let last = v.get("ok").is_some() || v.get("error").is_some() || v.get("end").is_some();
                    let _ = tx.send(v);
                    if last {
                        routes.remove(&id);
                    }
                }
            }
        });
        Ok(CallStream { id, rx })
    }

    /// A call with one reply: its `ok` value, or the wire error it ended with.
    pub async fn call(&self, call: &str, args: Value) -> Result<Value> {
        let mut s = self.start(call, args).await?;
        match tokio::time::timeout(Duration::from_secs(60), s.rx.recv()).await {
            Ok(Some(frame)) => match frame.get("error") {
                Some(e) => Err(NetError::Remote(wire_error(e))),
                None => Ok(frame["ok"].clone()),
            },
            Ok(None) => Err(NetError::Closed("connection ended before the reply".into())),
            Err(_) => Err(NetError::Closed("no reply".into())),
        }
    }

    /// A streaming call: for `read` without `follow`, the items until `end`; for `read` with `follow`,
    /// `subscribe` and `watch`, the live items. A `cancel` of a bogus target afterwards returns only once the
    /// server has handled the call before it, so the subscription is registered when this returns.
    pub async fn stream(&self, call: &str, args: Value) -> Result<CallStream> {
        let s = self.start(call, args).await?;
        let barrier = self.next_id.load(Ordering::SeqCst);
        let _ = barrier;
        self.call(
            "cancel",
            han_seq_obj([("target", Value::Int(han_core::json::MAX_SAFE_INT))]),
        )
        .await?;
        Ok(s)
    }

    /// `hello` as `who` with `key` (G-08). Returns the `ok` of the reply.
    pub async fn hello(&self, who: &str, key_id: &str, key: &SigningKey) -> Result<Value> {
        let proof = han_seq::auth::make_proof(key, &self.challenge.challenge, &self.dialed);
        self.call(
            "hello",
            han_seq_obj([
                ("who", Value::Str(who.to_owned())),
                ("keyId", Value::Str(key_id.to_owned())),
                ("proof", Value::Str(proof)),
                ("version", Value::Int(1)),
            ]),
        )
        .await
    }

    /// `append`: the receipt.
    pub async fn append(&self, entry: &Value, content: Option<&Value>) -> Result<Receipt> {
        let mut args = vec![("entry", entry.clone())];
        if let Some(c) = content {
            args.push(("content", c.clone()));
        }
        let v = self.call("append", han_seq_obj_vec(args)).await?;
        json::from_value(&v).map_err(|e| NetError::Protocol(e.to_string()))
    }

    /// `read` of `space` after `after`, without `follow`: the items.
    pub async fn read(&self, space: &str, after: u64) -> Result<Vec<Value>> {
        let mut s = self
            .start(
                "read",
                han_seq_obj([
                    ("space", Value::Str(space.to_owned())),
                    ("after", Value::Int(i64::try_from(after).unwrap_or(i64::MAX))),
                ]),
            )
            .await?;
        let mut out = Vec::new();
        while let Some(item) = s.next().await {
            out.push(item?);
        }
        Ok(out)
    }

    /// `read` with `follow`: the backlog and then the live records.
    pub async fn follow(&self, space: &str, after: u64) -> Result<CallStream> {
        self.stream(
            "read",
            han_seq_obj([
                ("space", Value::Str(space.to_owned())),
                ("after", Value::Int(i64::try_from(after).unwrap_or(i64::MAX))),
                ("follow", Value::Bool(true)),
            ]),
        )
        .await
    }

    /// Closes the connection.
    pub fn close(&self) {
        self.conn.close(crate::transport::close::OK, "bye");
    }
}

fn han_seq_obj<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    han_seq_obj_vec(pairs.into_iter().collect())
}

fn han_seq_obj_vec(pairs: Vec<(&str, Value)>) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

/// Builds a request object, for callers that make calls the typed helpers do not cover.
pub fn args<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    han_seq_obj(pairs)
}

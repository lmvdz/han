//! The JSON-RPC client: a [`NodeApi`] over the local pipe.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use interprocess::local_socket::tokio::{Stream, prelude::*};
use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use super::{Message, local_name, read_line};
use crate::api::{
    BoxFuture, CallResult, Command, NodeApi, NodeError, Query, ViewEvent, ViewSink, ViewStream,
};

type Reply = oneshot::Sender<Result<Value, NodeError>>;
type Pending = Arc<Mutex<HashMap<u64, Reply>>>;
type Subs = Arc<Mutex<HashMap<u64, ViewSink>>>;

/// A connection to a running node.
pub struct RpcClient {
    writer: mpsc::UnboundedSender<String>,
    next_id: AtomicU64,
    pending: Pending,
    subs: Subs,
    /// Set when the connection has ended: later calls and views fail at once instead of waiting for an
    /// answer that cannot come.
    gone: Arc<AtomicBool>,
    /// Whether a refusal with `presence_required` makes the client ask the device to unlock the connection
    /// and send the command again (a face does; a probe of the lock does not).
    unlocks: bool,
    /// One unlock at a time from this client: concurrent calls that were refused wait for the first one's
    /// answer instead of each asking the person.
    unlocking: tokio::sync::Mutex<()>,
}

impl RpcClient {
    /// Connects to the daemon at `endpoint` (see [`super::default_endpoint`]), as a face does: when the node
    /// answers `presence_required` to a command that governs or steers, the client asks the device to unlock
    /// this connection (the person confirms there) and sends the command again. A command costs a person
    /// one confirmation per unlock window, and a refusal of the confirmation is the command's error.
    pub async fn connect(endpoint: &str) -> io::Result<RpcClient> {
        Self::open(endpoint, true).await
    }

    /// Like [`RpcClient::connect`], but never asks for the unlock: a command that needs a person gets
    /// `presence_required`, as it does for any process that opens the pipe and cannot be at the device.
    pub async fn connect_locked(endpoint: &str) -> io::Result<RpcClient> {
        Self::open(endpoint, false).await
    }

    /// Asks the device to confirm a person for this connection, and returns how long the unlock lasts. The
    /// answer comes when the person has answered the device's prompt.
    pub async fn unlock(&self) -> Result<std::time::Duration, NodeError> {
        let v = self.request("unlock", Value::Null).await?;
        let ms = v["window_ms"]
            .as_u64()
            .ok_or_else(|| NodeError::unavailable("rpc: bad unlock result"))?;
        Ok(std::time::Duration::from_millis(ms))
    }

    async fn open(endpoint: &str, unlocks: bool) -> io::Result<RpcClient> {
        let stream = Stream::connect(local_name(endpoint)?).await?;
        let (recv, mut send) = stream.split();
        let (writer, mut out) = mpsc::unbounded_channel::<String>();
        let pending: Pending = Arc::default();
        let subs: Subs = Arc::default();
        tokio::spawn(async move {
            while let Some(line) = out.recv().await {
                if send.write_all(line.as_bytes()).await.is_err()
                    || send.write_all(b"\n").await.is_err()
                    || send.flush().await.is_err()
                {
                    break;
                }
            }
        });
        let gone_flag = Arc::new(AtomicBool::new(false));
        let (p, s, g) = (
            Arc::clone(&pending),
            Arc::clone(&subs),
            Arc::clone(&gone_flag),
        );
        tokio::spawn(async move {
            let mut reader = BufReader::new(recv);
            let mut buf = Vec::new();
            while let Ok(Some(())) = read_line(&mut reader, &mut buf).await {
                let Ok(msg) = serde_json::from_slice::<Message>(&buf) else {
                    continue;
                };
                dispatch(msg, &p, &s);
            }
            // The daemon is gone: every call fails and every view ends. The flag goes first, so a call that
            // registers after the drain sees it.
            g.store(true, Ordering::SeqCst);
            let gone = || NodeError::unavailable("the node closed the connection");
            for (_, tx) in p.lock().unwrap_or_else(|e| e.into_inner()).drain() {
                let _ = tx.send(Err(gone()));
            }
            for (_, sink) in s.lock().unwrap_or_else(|e| e.into_inner()).drain() {
                sink.send(ViewEvent::Failed {
                    seq: 0,
                    error: gone(),
                });
            }
        });
        Ok(RpcClient {
            writer,
            next_id: AtomicU64::new(1),
            pending,
            subs,
            gone: gone_flag,
            unlocks,
            unlocking: tokio::sync::Mutex::new(()),
        })
    }

    fn register(&self) -> (u64, oneshot::Receiver<Result<Value, NodeError>>) {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);
        (id, rx)
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, NodeError> {
        let (id, rx) = self.register();
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if self.gone.load(Ordering::SeqCst) || self.writer.send(line.to_string()).is_err() {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(NodeError::unavailable("the node closed the connection"));
        }
        rx.await
            .unwrap_or_else(|_| Err(NodeError::unavailable("the node closed the connection")))
    }
}

fn dispatch(msg: Message, pending: &Pending, subs: &Subs) {
    if msg.method.as_deref() == Some("view.event") {
        let Some(params) = msg.params else { return };
        let sub = params["sub"].as_u64().unwrap_or(0);
        let Ok(event) = serde_json::from_value::<ViewEvent>(params["event"].clone()) else {
            return;
        };
        let mut subs = subs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(sink) = subs.get(&sub)
            && !sink.send(event)
        {
            subs.remove(&sub);
        }
        return;
    }
    let Some(id) = msg.id.as_ref().and_then(Value::as_u64) else {
        return;
    };
    let Some(tx) = pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&id)
    else {
        return;
    };
    let _ = tx.send(match msg.error {
        Some(e) => Err(e.into_node()),
        None => Ok(msg.result.unwrap_or(Value::Null)),
    });
}

struct Unsubscribe {
    id: u64,
    writer: mpsc::UnboundedSender<String>,
    subs: Subs,
}

impl Drop for Unsubscribe {
    fn drop(&mut self) {
        self.subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
        let line = json!({"jsonrpc": "2.0", "method": "unsubscribe", "params": {"sub": self.id}});
        let _ = self.writer.send(line.to_string());
    }
}

impl NodeApi for RpcClient {
    fn call(&self, cmd: Command) -> BoxFuture<'_, Result<CallResult, NodeError>> {
        Box::pin(async move {
            let params =
                serde_json::to_value(&cmd).map_err(|e| NodeError::invalid(e.to_string()))?;
            let mut result = self.request("call", params.clone()).await;
            if self.unlocks && matches!(result, Err(NodeError::PresenceRequired { .. })) {
                let _one = self.unlocking.lock().await;
                // another call may have unlocked the connection while this one waited
                result = self.request("call", params.clone()).await;
                if matches!(result, Err(NodeError::PresenceRequired { .. })) {
                    self.unlock().await?;
                    result = self.request("call", params).await;
                }
            }
            serde_json::from_value(result?)
                .map_err(|e| NodeError::unavailable(format!("rpc: bad result: {e}")))
        })
    }

    fn view(&self, query: Query) -> ViewStream {
        let (sink, stream) = ViewStream::channel();
        // The view's id is its request id; events carry it as `sub`, a refusal is the response.
        let (id, rx) = self.register();
        self.subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, sink.clone());
        let params = serde_json::to_value(&query).unwrap_or(Value::Null);
        let line = json!({"jsonrpc": "2.0", "id": id, "method": "view", "params": params});
        if self.gone.load(Ordering::SeqCst) || self.writer.send(line.to_string()).is_err() {
            sink.send(ViewEvent::Failed {
                seq: 0,
                error: NodeError::unavailable("the node closed the connection"),
            });
        }
        tokio::spawn(async move {
            if let Ok(Err(error)) = rx.await {
                sink.send(ViewEvent::Failed { seq: 0, error });
            }
        });
        stream.guarded(Unsubscribe {
            id,
            writer: self.writer.clone(),
            subs: Arc::clone(&self.subs),
        })
    }
}

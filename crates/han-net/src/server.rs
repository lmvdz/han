//! The server side of the carrier (G-07): per connection, the control stream with the Challenge, one task
//! per call stream (handled in stream order), and one unidirectional stream per live call.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use han_seq::{CallFeed, Conn, Home};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::addr::NodeId;
use crate::error::NetError;
use crate::frame::{read_frame, write_frame};
use crate::transport::{Connection, SendStream, Transport, close};

/// How a server behaves.
#[derive(Clone, Debug)]
pub struct ServeOptions {
    /// Connections one node may hold open; over it the connection is closed with `limit` before a Challenge
    /// is issued (G-08).
    pub max_conns_per_peer: usize,
    /// How long a call stream may take to deliver its request frame.
    pub request_timeout: Duration,
    /// How often a connection's node is checked to still vouch for its actor (X14, at most 300 s).
    pub revalidate_every: Duration,
}

impl Default for ServeOptions {
    fn default() -> Self {
        ServeOptions {
            max_conns_per_peer: 256,
            request_timeout: Duration::from_secs(30),
            revalidate_every: Duration::from_secs(300),
        }
    }
}

/// A running server.
pub struct ServerHandle {
    task: JoinHandle<()>,
}

impl ServerHandle {
    /// Stops accepting connections. Connections already accepted run until they end.
    pub fn stop(&self) {
        self.task.abort();
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Serves `home` on `transport` until the handle is dropped or the transport shuts down.
pub fn serve<T: Transport>(transport: T, home: Arc<Home>, opts: ServeOptions) -> ServerHandle {
    let opts = Arc::new(opts);
    let counts: Arc<Mutex<HashMap<NodeId, usize>>> = Arc::default();
    let task = tokio::spawn(async move {
        while let Some(conn) = transport.accept().await {
            let (home, opts, counts) = (Arc::clone(&home), Arc::clone(&opts), Arc::clone(&counts));
            tokio::spawn(async move {
                let peer = conn.peer();
                let held = {
                    let mut c = counts.lock().unwrap_or_else(|p| p.into_inner());
                    let n = c.entry(peer.clone()).or_default();
                    *n += 1;
                    *n
                };
                if held > opts.max_conns_per_peer {
                    conn.close(close::LIMIT, "limit");
                } else {
                    run_connection(conn, home, &opts).await;
                }
                let mut c = counts.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(n) = c.get_mut(&peer) {
                    *n -= 1;
                    if *n == 0 {
                        c.remove(&peer);
                    }
                }
            });
        }
    });
    ServerHandle { task }
}

/// Call streams are handled in the order the client opened them (G-07): each waits for its turn.
#[derive(Default)]
struct Turn {
    next: Mutex<u64>,
    changed: Notify,
}

impl Turn {
    async fn wait(&self, n: u64) {
        loop {
            let notified = self.changed.notified();
            if *self.next.lock().unwrap_or_else(|p| p.into_inner()) >= n {
                return;
            }
            notified.await;
        }
    }

    fn pass(&self, n: u64) {
        let mut next = self.next.lock().unwrap_or_else(|p| p.into_inner());
        if *next == n {
            *next = n + 1;
            self.changed.notify_waiters();
        }
    }
}

async fn run_connection<C: Connection>(conn: C, home: Arc<Home>, opts: &ServeOptions) {
    let (hconn, challenge) = home.connect(&conn.peer());
    let hconn = Arc::new(hconn);
    match conn.open_uni().await {
        Ok(mut control) => {
            // The control stream carries the Challenge and nothing else (G-07).
            let sent = write_frame(&mut control, &challenge).await;
            if sent.is_err() || control.finish().await.is_err() {
                conn.close(close::PROTOCOL, "control stream");
                return;
            }
        }
        Err(_) => return,
    }
    let turn = Arc::new(Turn::default());
    let mut n = 0u64;
    let mut tick = tokio::time::interval(opts.revalidate_every);
    tick.tick().await;
    let mut changes = home.watch_changes();
    loop {
        tokio::select! {
            accepted = conn.accept_bi() => {
                let Ok((send, recv)) = accepted else { break };
                let call = Call {
                    conn: conn.clone(),
                    hconn: Arc::clone(&hconn),
                    turn: Arc::clone(&turn),
                    n,
                    timeout: opts.request_timeout,
                };
                n += 1;
                tokio::spawn(call.run(send, recv));
            }
            () = conn.closed() => break,
            _ = changes.changed() => {
                if !hconn.still_vouched() {
                    conn.close(close::REVOKED, "revoked");
                    break;
                }
            }
            _ = tick.tick() => {
                if !hconn.still_vouched() {
                    conn.close(close::REVOKED, "revoked");
                    break;
                }
            }
        }
    }
    hconn.close();
}

struct Call<C: Connection> {
    conn: C,
    hconn: Arc<Conn>,
    turn: Arc<Turn>,
    n: u64,
    timeout: Duration,
}

impl<C: Connection> Call<C> {
    async fn run(self, mut send: C::Send, mut recv: C::Recv) {
        let request = tokio::time::timeout(self.timeout, read_frame(&mut recv)).await;
        let text = match request {
            Ok(Ok(Some(text))) => text,
            Ok(Err(NetError::Frame(why))) => {
                // An oversized length or a payload that is not UTF-8 closes the connection (G-07).
                self.turn.pass(self.n);
                self.conn.close(close::PROTOCOL, why);
                return;
            }
            _ => {
                self.turn.pass(self.n);
                return;
            }
        };
        self.turn.wait(self.n).await;
        let handled = self.hconn.handle(&text);
        self.turn.pass(self.n);
        for frame in &handled.reply {
            if write_frame(&mut send, frame).await.is_err() {
                break;
            }
        }
        let _ = send.finish().await;
        if handled.close {
            // Let the reply reach the peer before the connection goes.
            let _ = tokio::time::timeout(Duration::from_secs(5), send.delivered()).await;
            self.conn.close(close::PROTOCOL, "invalid frame");
            return;
        }
        if let Some(feed) = handled.feed {
            self.feed(feed).await;
        }
    }

    /// A streaming call: its frames go on a unidirectional stream the server opens for it.
    async fn feed(&self, mut feed: CallFeed) {
        let id = feed.request_id;
        let Ok(mut out) = self.conn.open_uni().await else {
            self.hconn.end_feed(id);
            return;
        };
        let mut ok = true;
        for frame in &feed.backlog {
            if write_frame(&mut out, frame).await.is_err() {
                ok = false;
                break;
            }
        }
        while ok && let Some(frame) = feed.next_frame().await {
            ok = write_frame(&mut out, &frame).await.is_ok();
        }
        let _ = out.finish().await;
        self.hconn.end_feed(id);
    }
}

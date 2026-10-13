//! Sync of followed spaces: one link per home, kept up with backoff. Over a link the node subscribes to each
//! followed space (backlog from its verified head, then live records and signals), checks every receipt, and
//! watches the streams of open sessions for `han.status/1` items. Entries signed here leave through the outbox
//! when the link is up.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use han_core::json::{self, Value};
use han_core::{LogRecord, PublicKey};
use han_net::client::args;
use han_net::{Client, Connection, NetError, NodeAddr, Transport};
use han_seq::ServerKey;
use han_seq::verify::check_chain;
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};

use crate::api::{BoxFuture, NodeError, SpaceLink, StatusBody};
use crate::node::Inner;

/// Where the keys that sign a home's receipts come from (`/.well-known/han`, section 4.3).
pub trait ServerKeys: Send + Sync + 'static {
    /// The published keys of `server`.
    fn keys<'a>(&'a self, server: &'a str) -> BoxFuture<'a, Result<Vec<ServerKey>, String>>;
}

/// Keys known in advance: for tests and simulations, and for a home one runs oneself.
#[derive(Default)]
pub struct StaticKeys(Mutex<HashMap<String, Vec<ServerKey>>>);

impl StaticKeys {
    /// No keys yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the keys of `server`.
    pub fn set(&self, server: &str, keys: Vec<ServerKey>) {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(server.to_owned(), keys);
    }
}

impl ServerKeys for StaticKeys {
    fn keys<'a>(&'a self, server: &'a str) -> BoxFuture<'a, Result<Vec<ServerKey>, String>> {
        Box::pin(async move {
            self.0
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(server)
                .cloned()
                .ok_or_else(|| format!("no keys for {server}"))
        })
    }
}

/// Keys from the server's `/.well-known/han` over HTTPS.
pub struct WellKnownKeys {
    fetcher: han_net::wellknown::Fetcher,
}

impl WellKnownKeys {
    /// Fetches with `fetcher`.
    pub fn new(fetcher: han_net::wellknown::Fetcher) -> Self {
        WellKnownKeys { fetcher }
    }
}

impl ServerKeys for WellKnownKeys {
    fn keys<'a>(&'a self, server: &'a str) -> BoxFuture<'a, Result<Vec<ServerKey>, String>> {
        Box::pin(async move {
            self.fetcher
                .wellknown(server)
                .await
                .map(|doc| doc.keys)
                .map_err(|e| e.to_string())
        })
    }
}

/// What the node tells a link task.
pub(crate) enum LinkCmd {
    /// Start following this space.
    AddSpace(String),
    /// Watch this session's stream.
    Watch { space: String, session: String },
}

/// A link to one home.
pub(crate) struct LinkHandle<T: Transport> {
    pub cmds: mpsc::UnboundedSender<LinkCmd>,
    pub client: watch::Receiver<Option<Arc<Client<T::Conn>>>>,
    pub task: JoinHandle<()>,
}

/// Maps what went wrong on the wire to the node's closed set of errors.
pub(crate) fn map_net_error(e: &NetError) -> NodeError {
    use han_seq::Code;
    match e {
        NetError::Remote(w) => match w.code {
            Code::Denied => NodeError::denied(&w.message),
            Code::Invalid => NodeError::invalid(&w.message),
            Code::NotFound => NodeError::not_found(&w.message),
            Code::Limit | Code::Unavailable => NodeError::Unavailable {
                message: w.message.clone(),
                retry_after_ms: w.retry_after_ms,
            },
            Code::Moved => NodeError::unavailable(format!(
                "the space moved to {}",
                w.moved_to.as_deref().unwrap_or("another home")
            )),
        },
        other => NodeError::unavailable(other.to_string()),
    }
}

/// Whether a wire error is a final answer (never retry) rather than a reason to try again later.
pub(crate) fn is_final(e: &NodeError) -> bool {
    matches!(
        e,
        NodeError::Denied { .. } | NodeError::Invalid { .. } | NodeError::NotFound { .. }
    )
}

impl<T: Transport> Inner<T> {
    /// The keys of `server`, from the cache unless `refresh`.
    pub(crate) async fn keys_for(
        &self,
        server: &str,
        refresh: bool,
    ) -> Result<Vec<ServerKey>, String> {
        if !refresh
            && let Some(k) = self
                .key_cache
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(server)
        {
            return Ok(k.clone());
        }
        let keys = self.server_keys.keys(server).await?;
        self.key_cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(server.to_owned(), keys.clone());
        Ok(keys)
    }

    /// Checks a record the home sent (ids, the home's signature under its published key, the `prev` link). A
    /// reader who lost and regained a role sees gaps in the seqs; then only the record itself can be checked.
    async fn verify_record(
        &self,
        server: &str,
        rec: &LogRecord,
        head: u64,
        tail_hash: &str,
    ) -> Result<String, String> {
        rec.validate().map_err(|e| format!("record: {e}"))?;
        let r = &rec.receipt;
        let prev = if r.seq == head + 1 {
            tail_hash.to_owned()
        } else {
            r.prev.clone()
        };
        let mut keys = self.keys_for(server, false).await?;
        let mut checked = check_chain(std::slice::from_ref(rec), &keys, &prev);
        if matches!(&checked, Err(e) if e.contains("no published key")) {
            keys = self.keys_for(server, true).await?;
            checked = check_chain(std::slice::from_ref(rec), &keys, &prev);
        }
        checked?;
        r.hash().map_err(|e| e.to_string())
    }

    /// Verifies a receipt the home returned for an entry this device sent.
    pub(crate) async fn verify_receipt(
        &self,
        server: &str,
        receipt: &han_core::Receipt,
        entry_id: &str,
    ) -> Result<(), String> {
        if receipt.id != entry_id {
            return Err("receipt is for another entry".into());
        }
        for refresh in [false, true] {
            let keys = self.keys_for(server, refresh).await?;
            let key = keys
                .iter()
                .find(|k| k.key_id == receipt.home_key && k.covers(&receipt.time))
                .and_then(|k| PublicKey::from_base64url(&k.public_key).ok());
            if let Some(key) = key {
                return receipt
                    .verify(&key)
                    .map_err(|_| "homeSig does not verify".to_owned());
            }
        }
        Err("no published key covers the receipt".into())
    }

    /// Folds one record the home sent: verifies it, stores it, and shows it in the views. `Err` is a receipt
    /// that does not verify: the space stops being followed.
    async fn ingest(
        self: &Arc<Self>,
        server: &str,
        space: &str,
        rec: LogRecord,
        cmds: &mpsc::UnboundedSender<LinkCmd>,
    ) -> Result<(), String> {
        if rec.receipt.space != space {
            return Err("record of another space".into());
        }
        let (head, tail) = self
            .db
            .follows()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|f| f.space == space)
            .map(|f| (f.head, f.tail_hash))
            .ok_or("not followed")?;
        if rec.receipt.seq <= head {
            return Ok(()); // already folded: the backlog and the live feed overlap
        }
        let hash = self.verify_record(server, &rec, head, &tail).await?;
        self.db.put_record(&rec, &hash).map_err(|e| e.to_string())?;
        let opened = self.engine().apply_record(space, &rec);
        for session in opened {
            let _ = cmds.send(LinkCmd::Watch {
                space: space.to_owned(),
                session,
            });
        }
        Ok(())
    }

    /// A live `han.status/1` item of `session`.
    fn status_item(&self, space: &str, item: &Value) {
        if item["type"].as_str() != Some("profile") || item["name"].as_str() != Some("han.status/1")
        {
            return;
        }
        let Ok(status) = json::from_value::<StatusBody>(&item["body"]) else {
            return;
        };
        if self.engine().apply_status(space, status.clone())
            && let Err(e) = self.db.put_status(space, &status)
        {
            tracing::warn!("node.db: {e}");
        }
    }
}

async fn connect<T: Transport>(
    inner: &Inner<T>,
    home: &crate::api::HomeRef,
    trusted: &[String],
) -> Result<Client<T::Conn>, String> {
    let addr = NodeAddr {
        node: home.node.clone(),
        addrs: home.addrs.clone(),
        relays: home.relays.clone(),
    };
    let client = Client::connect(&inner.transport, &addr, &home.server, trusted)
        .await
        .map_err(|e| e.to_string())?;
    client
        .hello(&inner.cfg.actor, &inner.cfg.key_id, &inner.key)
        .await
        .map_err(|e| format!("hello: {e}"))?;
    Ok(client)
}

/// Runs the link to `home` until the node shuts down: connect, follow, flush, and reconnect with backoff.
pub(crate) async fn run_link<T: Transport>(
    inner: Arc<Inner<T>>,
    home: crate::api::HomeRef,
    trusted: Vec<String>,
    cmds_tx: mpsc::UnboundedSender<LinkCmd>,
    mut cmds: mpsc::UnboundedReceiver<LinkCmd>,
    client_tx: watch::Sender<Option<Arc<Client<T::Conn>>>>,
) {
    let base = inner.cfg.retry;
    let mut backoff = base;
    loop {
        let spaces = inner.spaces_on(&home.server);
        for s in &spaces {
            inner.engine().set_link(s, SpaceLink::Connecting, None);
        }
        match connect(&inner, &home, &trusted).await {
            Err(why) => {
                for s in &spaces {
                    inner
                        .engine()
                        .set_link(s, SpaceLink::Offline, Some(why.clone()));
                }
            }
            Ok(client) => {
                backoff = base;
                let client = Arc::new(client);
                client_tx.send_replace(Some(Arc::clone(&client)));
                let mut tasks: JoinSet<()> = JoinSet::new();
                let mut started: HashSet<String> = HashSet::new();
                let mut watched: HashSet<(String, String)> = HashSet::new();
                for s in &spaces {
                    started.insert(s.clone());
                    tasks.spawn(follow_space(
                        Arc::clone(&inner),
                        Arc::clone(&client),
                        home.server.clone(),
                        s.clone(),
                        cmds_tx.clone(),
                    ));
                    for session in inner.engine().open_sessions(s) {
                        if watched.insert((s.clone(), session.clone())) {
                            tasks.spawn(watch_session(
                                Arc::clone(&inner),
                                Arc::clone(&client),
                                s.clone(),
                                session,
                            ));
                        }
                    }
                }
                tasks.spawn(flush_all(Arc::clone(&inner), home.server.clone()));
                let mut flush = tokio::time::interval(inner.cfg.flush_every);
                flush.tick().await;
                loop {
                    tokio::select! {
                        () = client.connection().closed() => break,
                        _ = flush.tick() => {
                            tasks.spawn(flush_all(Arc::clone(&inner), home.server.clone()));
                        }
                        cmd = cmds.recv() => match cmd {
                            None => return,
                            Some(LinkCmd::AddSpace(s)) => {
                                if started.insert(s.clone()) {
                                    tasks.spawn(follow_space(
                                        Arc::clone(&inner),
                                        Arc::clone(&client),
                                        home.server.clone(),
                                        s,
                                        cmds_tx.clone(),
                                    ));
                                }
                            }
                            Some(LinkCmd::Watch { space, session }) => {
                                if watched.insert((space.clone(), session.clone())) {
                                    tasks.spawn(watch_session(
                                        Arc::clone(&inner),
                                        Arc::clone(&client),
                                        space,
                                        session,
                                    ));
                                }
                            }
                        },
                        _ = tasks.join_next(), if !tasks.is_empty() => {}
                    }
                }
                client_tx.send_replace(None);
                tasks.abort_all();
                for s in inner.spaces_on(&home.server) {
                    inner.engine().set_link(
                        &s,
                        SpaceLink::Offline,
                        Some("connection closed".into()),
                    );
                }
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
        // Commands that came while down are only hints: the next connection starts from the stored state.
        while cmds.try_recv().is_ok() {}
    }
}

async fn flush_all<T: Transport>(inner: Arc<Inner<T>>, server: String) {
    for space in inner.spaces_on(&server) {
        inner.flush_space(&space, None, Duration::ZERO).await;
    }
}

/// Follows one space over a connection: subscribe from the verified head, fold what arrives.
async fn follow_space<T: Transport>(
    inner: Arc<Inner<T>>,
    client: Arc<Client<T::Conn>>,
    server: String,
    space: String,
    cmds: mpsc::UnboundedSender<LinkCmd>,
) {
    loop {
        let head = inner
            .db
            .follows()
            .ok()
            .and_then(|f| f.into_iter().find(|f| f.space == space))
            .map_or(0, |f| f.head);
        let after = i64::try_from(head).unwrap_or(i64::MAX);
        let started = client
            .stream(
                "subscribe",
                args([
                    ("space", Value::Str(space.clone())),
                    ("after", Value::Int(after)),
                ]),
            )
            .await;
        match started {
            Ok(mut stream) => {
                inner.engine().set_link(&space, SpaceLink::Live, None);
                while let Some(item) = stream.next().await {
                    let Ok(item) = item else { break };
                    match item["type"].as_str() {
                        Some("record") => {
                            let Ok(rec) = json::from_value::<LogRecord>(&item["record"]) else {
                                continue;
                            };
                            if let Err(why) = inner.ingest(&server, &space, rec, &cmds).await {
                                tracing::warn!("{space}: rejected: {why}");
                                inner
                                    .engine()
                                    .set_link(&space, SpaceLink::Rejected, Some(why));
                                return;
                            }
                        }
                        Some("signal") => inner.engine().count_signal(&space),
                        _ => {}
                    }
                }
            }
            Err(NetError::Remote(w))
                if matches!(w.code, han_seq::Code::Denied | han_seq::Code::NotFound) =>
            {
                // Not admitted yet, or not created yet: try again later.
                inner.engine().set_link(
                    &space,
                    SpaceLink::Offline,
                    Some(format!("not readable yet: {}", w.message)),
                );
            }
            Err(_) => {}
        }
        if tokio::time::timeout(Duration::ZERO, client.connection().closed())
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(inner.cfg.retry).await;
    }
}

/// Watches one session's stream for status items until it ends.
async fn watch_session<T: Transport>(
    inner: Arc<Inner<T>>,
    client: Arc<Client<T::Conn>>,
    space: String,
    session: String,
) {
    let started = client
        .stream(
            "watch",
            args([
                ("space", Value::Str(space.clone())),
                ("session", Value::Str(session)),
            ]),
        )
        .await;
    let Ok(mut stream) = started else { return };
    while let Some(Ok(item)) = stream.next().await {
        inner.status_item(&space, &item);
    }
}

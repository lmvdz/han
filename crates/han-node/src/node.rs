//! The node: the runtime of one device. It owns the device key, the database, the views engine and the links
//! to homes, and implements the node API (`call` and `view`).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use han_core::json::{self, Value};
use han_core::{SigningKey, base64url, grammar, ids};
use han_net::client::args;
use han_net::{Client, HomeTicket, InviteTicket, NodeAddr, Transport};
use han_seq::Clock;
use han_seq::ServerKey;
use han_seq::build::{EntrySpec, entry_id, sign_entry, space_policy};
use han_seq::time::format_time;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::agents::{AgentsFile, HarnessInstall};
use crate::api::{
    BoxFuture, CallResult, Command, HomeRef, NodeApi, NodeError, Query, RoleInfo, ViewEvent,
    ViewStream,
};
use crate::db::{NodeDb, OutboxRow};
use crate::keys::{DEVICE_KEY, FileKeyStore, KeyStore};
use crate::presence::{self, PresenceSlot};
use crate::retry::{Disposition, Via, disposition, node_error as map_net_error, stored_error};
use crate::supervisor::{Role, RoleSpec, RoleStatus, RunAs, Supervisor, SupervisorError};
use crate::sync::{LinkCmd, LinkHandle, ServerKeys, run_link};
pub use crate::views::Freshness;
use crate::views::{Engine, NodeInfo};

/// How a node is set up.
#[derive(Clone, Debug)]
pub struct NodeConfig {
    /// The actor this device acts as (a person, or an agent's host).
    pub actor: String,
    /// The id of this device's key in the actor's profile.
    pub key_id: String,
    /// Where `node.db` lives; in memory when absent.
    pub data_dir: Option<PathBuf>,
    /// When silence turns sessions stale and failed, unless a space says otherwise.
    pub freshness: Freshness,
    /// Per-space thresholds (PLAN 6.4: configurable per space).
    pub space_freshness: HashMap<String, Freshness>,
    /// How long a command waits for the link to its home before it is queued in the outbox.
    pub connect_wait: Duration,
    /// The first wait before dialing a home again; it doubles up to 30 s.
    pub retry: Duration,
    /// How often a link looks for entries in the outbox to send.
    pub flush_every: Duration,
    /// How many tries the home may answer with a retry-later error before an outbox entry is marked `failed`.
    pub outbox_max_attempts: u32,
    /// The wait after the first such try; it doubles with each try, up to 60 times this (five minutes).
    pub outbox_backoff: Duration,
    /// How often the node moves the views' clock when it runs by itself.
    pub tick_every: Duration,
    /// Fetches `/.well-known/han`, to confirm a ticket's node before it is dialed (Z4).
    pub fetcher: han_net::wellknown::Fetcher,
    /// The human-presence check approvals wait for. Absent, the node builds the backend `presence.backend`
    /// names in `node.db` (the platform's own by default). Only tests and embedding apps set it.
    pub presence: Option<PresenceSlot>,
    /// How long a person has to answer the presence check.
    pub presence_timeout: Duration,
}

impl NodeConfig {
    /// A configuration for `actor` with device key id `d1` and the defaults.
    pub fn new(actor: &str) -> Self {
        NodeConfig {
            actor: actor.to_owned(),
            key_id: "d1".into(),
            data_dir: None,
            freshness: Freshness::default(),
            space_freshness: HashMap::new(),
            connect_wait: Duration::from_secs(3),
            retry: Duration::from_secs(1),
            flush_every: Duration::from_secs(5),
            outbox_max_attempts: 12,
            outbox_backoff: Duration::from_secs(5),
            tick_every: Duration::from_secs(5),
            fetcher: han_net::wellknown::Fetcher::new(),
            presence: None,
            presence_timeout: crate::presence::DEFAULT_TIMEOUT,
        }
    }

    /// Uses `presence` for the human-presence check, whatever `node.db` says about the backend. The
    /// `presence.require` setting still decides which actions it is asked for.
    pub fn set_presence(&mut self, presence: Arc<dyn han_presence::Presence>) {
        self.presence = Some(PresenceSlot(presence));
    }
}

/// What a node is built from. The network, the clock and the key store are injected (PLAN section 5), so a
/// whole team can run in one process on a simulated network.
pub struct NodeParts<T: Transport> {
    /// The setup.
    pub config: NodeConfig,
    /// The device's endpoint; its node id must be the device key's public key (N-01).
    pub transport: T,
    /// Time.
    pub clock: Arc<dyn Clock>,
    /// Where the device key lives.
    pub keys: Arc<dyn KeyStore>,
    /// Where the keys that sign a home's receipts come from.
    pub server_keys: Arc<dyn ServerKeys>,
    /// What runs child roles.
    pub supervisor: Arc<dyn Supervisor>,
}

pub(crate) struct Inner<T: Transport> {
    pub cfg: NodeConfig,
    pub key: SigningKey,
    pub transport: T,
    pub clock: Arc<dyn Clock>,
    pub db: NodeDb,
    engine: Arc<Mutex<Engine>>,
    pub server_keys: Arc<dyn ServerKeys>,
    pub key_cache: Mutex<HashMap<String, Vec<ServerKey>>>,
    /// Homes joined by ticket (ticket-pinned trust), by ServerName; their keys stand in for the well-known.
    pub pins: Mutex<HashMap<String, HomeTicket>>,
    key_store: Arc<dyn KeyStore>,
    supervisor: Arc<dyn Supervisor>,
    links: Mutex<HashMap<String, (HomeRef, LinkHandle<T>)>>,
    space_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Outbox rows waiting out a backoff, by row id: the clock reading (ms) before which they are not tried.
    outbox_retry_at: Mutex<HashMap<u64, u64>>,
    ticker: Mutex<Option<JoinHandle<()>>>,
    /// The work timelines of watched sessions, fed by their `han.timeline/1` items.
    pub timelines: Arc<crate::timeline::StreamTimelines>,
    /// Spaces whose invite asked for `auto_admit`, and until when (wall seconds).
    pub auto_admit: Mutex<HashMap<String, i64>>,
    /// One presence prompt at a time.
    presence_gate: tokio::sync::Mutex<()>,
}

/// The most a face may fetch or put of a blob through the node.
const MAX_FACE_BLOB: u64 = 4 * 1024 * 1024;

/// The runtime of one device.
pub struct Node<T: Transport> {
    inner: Arc<Inner<T>>,
}

impl<T: Transport> Clone for Node<T> {
    fn clone(&self) -> Self {
        Node {
            inner: Arc::clone(&self.inner),
        }
    }
}

pub(crate) enum Outcome {
    Sent(han_core::Receipt),
    Rejected(NodeError),
    Offline,
}

/// The `config` key under which a home ticket is kept in `node.db`.
const PIN_PREFIX: &str = "home-pin/";

/// Where to dial the home a ticket names.
fn home_of_ticket(t: &HomeTicket) -> HomeRef {
    HomeRef {
        server: t.server.clone(),
        node: t.node.node.clone(),
        addrs: t.node.addrs.clone(),
        relays: t.node.relays.clone(),
    }
}

/// Lets the group of the file's directory read and write it, for a key an OS user's host reads (the
/// directory is shared with that user's group by the runner's admin).
fn share_with_group(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660));
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn unavailable(e: impl std::fmt::Display) -> NodeError {
    NodeError::unavailable(e.to_string())
}

fn obj<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    Value::Object(
        pairs
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect::<BTreeMap<_, _>>(),
    )
}

fn s(t: &str) -> Value {
    Value::Str(t.to_owned())
}

fn int(n: u64) -> Value {
    Value::Int(i64::try_from(n.min(han_core::json::MAX_SAFE_INT as u64)).unwrap_or(i64::MAX))
}

fn strs(items: &[String]) -> Value {
    Value::Array(items.iter().map(|x| s(x)).collect())
}

fn random_id(len: usize) -> String {
    const ALPHABET: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut bytes = vec![0u8; len];
    // The system random source failing is not recoverable.
    getrandom::fill(&mut bytes).expect("system random source");
    bytes
        .iter()
        .map(|b| ALPHABET[usize::from(*b) % ALPHABET.len()] as char)
        .collect()
}

impl<T: Transport> Inner<T> {
    pub(crate) fn engine(&self) -> MutexGuard<'_, Engine> {
        self.engine.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn spaces_on(&self, server: &str) -> Vec<String> {
        self.engine().spaces_on(server)
    }

    fn space_lock(&self, space: &str) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(
            self.space_locks
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .entry(space.to_owned())
                .or_default(),
        )
    }

    pub(crate) fn now_secs(&self) -> i64 {
        i64::try_from(self.clock.now_ms() / 1000).unwrap_or(0)
    }

    /// Copies the outbox of `space` into the views.
    fn refresh_outbox(&self, space: &str) {
        match self.db.outbox_recent(space, 200) {
            Ok(items) => self.engine().set_outbox(space, items),
            Err(e) => tracing::warn!("node.db: {e}"),
        }
    }

    /// The client of the link to `server`, waiting up to `wait` for the link to come up.
    async fn client_for(
        &self,
        server: &str,
        wait: Duration,
    ) -> Result<Arc<Client<T::Conn>>, NodeError> {
        let mut rx = self
            .links
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(server)
            .map(|(_, l)| l.client.clone())
            .ok_or_else(|| NodeError::unavailable(format!("no link to {server}")))?;
        let found = if wait.is_zero() {
            rx.borrow().clone()
        } else {
            let up = async {
                loop {
                    let current = rx.borrow().clone();
                    if let Some(c) = current {
                        return Some(c);
                    }
                    if rx.changed().await.is_err() {
                        return None;
                    }
                }
            };
            tokio::time::timeout(wait, up).await.ok().flatten()
        };
        found.ok_or_else(|| NodeError::unavailable(format!("{server} cannot be reached now")))
    }

    /// Sends what waits in the outbox of `space`, in order. Returns what became of row `want`.
    pub(crate) async fn flush_space(
        &self,
        space: &str,
        want: Option<u64>,
        wait: Duration,
    ) -> Option<Outcome> {
        let lock = self.space_lock(space);
        let _guard = lock.lock().await;
        let rows = match self.db.outbox_pending(space) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("node.db: {e}");
                return Some(Outcome::Offline);
            }
        };
        let mut mine = None;
        if !rows.is_empty() {
            let server = self.engine().home_of(space)?;
            match self.client_for(&server, wait).await {
                Err(_) => {
                    mine = Some(Outcome::Offline);
                }
                Ok(client) => mine = self.send_rows(&server, &client, rows, want).await,
            }
            self.refresh_outbox(space);
        }
        if mine.is_none()
            && let Some(id) = want
        {
            // Another flusher may have sent it already.
            mine = match self.db.outbox_get(id) {
                Ok(Some(row)) => match row.state.as_str() {
                    "sent" => row
                        .receipt
                        .and_then(|r| serde_json::from_str(&r).ok())
                        .map(Outcome::Sent),
                    "rejected" => Some(Outcome::Rejected(stored_error(
                        row.code.as_deref(),
                        row.error.unwrap_or_default(),
                    ))),
                    "failed" => Some(Outcome::Rejected(NodeError::unavailable(
                        row.error.unwrap_or_default(),
                    ))),
                    _ => Some(Outcome::Offline),
                },
                _ => Some(Outcome::Offline),
            };
        }
        mine
    }

    /// Notes a try of `row` that reached no verdict. Returns the error to hand the caller if the entry has now
    /// used up its tries (it is `failed`; the line moves on, which is what `rejected` does too), else sets the
    /// wait before the next try. Only answers from the home count: a link that is down never gets here.
    fn defer_row(&self, row: &OutboxRow, err: &NodeError, counts: bool) -> Option<NodeError> {
        let _ = self.db.outbox_attempt(row.id, &err.to_string());
        let attempts = row.attempts.saturating_add(1);
        if counts && attempts >= self.cfg.outbox_max_attempts {
            let why = format!("gave up after {attempts} tries: {err}");
            let _ = self.db.outbox_failed(row.id, &why);
            self.outbox_retry_at
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&row.id);
            let kind = row.entry["kind"].as_str().unwrap_or("entry");
            self.engine().note_error(
                &format!("outbox #{} {kind} in {}: {why}", row.id, row.space),
                &NodeError::unavailable(why.clone()),
            );
            return Some(NodeError::unavailable(why));
        }
        let base = u64::try_from(self.cfg.outbox_backoff.as_millis()).unwrap_or(u64::MAX);
        let cap = base.saturating_mul(60);
        let mut wait = base
            .saturating_mul(1u64 << attempts.saturating_sub(1).min(16))
            .min(cap);
        if let NodeError::Unavailable {
            retry_after_ms: Some(ms),
            ..
        } = err
        {
            wait = wait.max((*ms).min(cap));
        }
        self.outbox_retry_at
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(row.id, self.clock.now_ms().saturating_add(wait));
        None
    }

    /// Makes `row` wait one base backoff without counting a try.
    fn hold_row(&self, row: &OutboxRow) {
        let wait = u64::try_from(self.cfg.outbox_backoff.as_millis()).unwrap_or(u64::MAX);
        self.outbox_retry_at
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(row.id, self.clock.now_ms().saturating_add(wait));
    }

    /// Whether `row` is still waiting out a backoff.
    fn backing_off(&self, row: &OutboxRow) -> bool {
        self.outbox_retry_at
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&row.id)
            .is_some_and(|at| *at > self.clock.now_ms())
    }

    /// Sends `rows` of one space in order. A row the home keeps answering "later" holds the rows behind it
    /// (R-07: finish what is stored before appending anything new to the space) until it is sent, refused or
    /// `failed`; rows of other spaces are not held, each space having its own line.
    async fn send_rows(
        &self,
        server: &str,
        client: &Client<T::Conn>,
        rows: Vec<OutboxRow>,
        want: Option<u64>,
    ) -> Option<Outcome> {
        let mut mine = None;
        for row in rows {
            if self.backing_off(&row) {
                if want == Some(row.id) || mine.is_none() {
                    mine = Some(Outcome::Offline);
                }
                break;
            }
            let result = client.append(&row.entry, row.content.as_ref()).await;
            match result {
                Ok(receipt) => {
                    if let Err(why) = self.verify_receipt(server, &receipt, &row.entry_id).await {
                        // A receipt that does not verify is not an answer: the entry stays queued.
                        let _ = self.db.outbox_attempt(row.id, &why);
                        if want == Some(row.id) {
                            mine = Some(Outcome::Rejected(unavailable(why)));
                        }
                        break;
                    }
                    let text = serde_json::to_string(&receipt).unwrap_or_default();
                    if let Err(e) = self.db.outbox_sent(row.id, &text) {
                        tracing::warn!("node.db: {e}");
                    }
                    self.outbox_retry_at
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(&row.id);
                    if want == Some(row.id) {
                        mine = Some(Outcome::Sent(receipt));
                    }
                }
                Err(e) => {
                    let err = map_net_error(&e);
                    match disposition(Via::Call, &e) {
                        Disposition::Final => {
                            let _ = self
                                .db
                                .outbox_rejected(row.id, err.code(), &err.to_string());
                            if want == Some(row.id) {
                                mine = Some(Outcome::Rejected(err));
                            }
                        }
                        Disposition::Moved { to } => {
                            // Not a failed try and not counted: the entry waits, and the Outbox view says why.
                            let _ = self.db.outbox_note(
                                row.id,
                                &format!(
                                    "the space moved to {to}; follow it there (home pin) to send this entry"
                                ),
                            );
                            self.hold_row(&row);
                            if want == Some(row.id) || mine.is_none() {
                                mine = Some(Outcome::Offline);
                            }
                            break;
                        }
                        Disposition::Retry { .. } | Disposition::Misconfigured => {
                            let counts = matches!(e, han_net::NetError::Remote(_));
                            if let Some(given_up) = self.defer_row(&row, &err, counts) {
                                if want == Some(row.id) {
                                    mine = Some(Outcome::Rejected(given_up));
                                }
                                continue;
                            }
                            if want == Some(row.id) || mine.is_none() {
                                mine = Some(Outcome::Offline);
                            }
                            break;
                        }
                    }
                }
            }
        }
        mine
    }
}

impl<T: Transport> Node<T> {
    /// Starts a node: opens `node.db`, rebuilds the views from what it holds, and dials the homes of the
    /// followed spaces in the background.
    pub fn start(parts: NodeParts<T>) -> Result<Node<T>, NodeError> {
        let NodeParts {
            config,
            transport,
            clock,
            keys,
            server_keys,
            supervisor,
        } = parts;
        if !grammar::is_actor_id(&config.actor) {
            return Err(NodeError::invalid("the node's actor is not an ActorId"));
        }
        let seed = keys
            .load_or_create(DEVICE_KEY)
            .map_err(|e| NodeError::unavailable(e.to_string()))?;
        let key = SigningKey::from_seed(seed);
        let node_id = key.public_key().to_base64url();
        if transport.node_id() != node_id {
            return Err(NodeError::invalid(
                "the endpoint's node id is not the device key's public key (N-01)",
            ));
        }
        let db = match &config.data_dir {
            Some(dir) => {
                std::fs::create_dir_all(dir).map_err(unavailable)?;
                NodeDb::open(&dir.join("node.db"))
            }
            None => NodeDb::in_memory(),
        }
        .map_err(unavailable)?;
        let mut engine = Engine::new(&config.actor, config.freshness);
        engine.tick(
            clock.steady_ms(),
            i64::try_from(clock.now_ms() / 1000).unwrap_or(0),
        );
        engine.set_node_info(NodeInfo {
            node_id,
            roles: Vec::new(),
        });
        engine.set_device_key(&config.key_id);
        let follows = db.follows().map_err(unavailable)?;
        for f in &follows {
            engine.add_space(
                &f.space,
                &f.home.server,
                config.space_freshness.get(&f.space).copied(),
            );
            for rec in db.records(&f.space).map_err(unavailable)? {
                engine.apply_record(&f.space, &rec);
            }
            for st in db.statuses(&f.space).map_err(unavailable)? {
                engine.apply_status(&f.space, st);
            }
        }
        let pins = db.home_pins().map_err(unavailable)?;
        let timelines = Arc::new(crate::timeline::StreamTimelines::new());
        engine.set_timeline(timelines.clone());
        let inner = Arc::new(Inner {
            timelines,
            cfg: config,
            key,
            transport,
            clock,
            db,
            engine: Arc::new(Mutex::new(engine)),
            server_keys,
            key_cache: Mutex::new(HashMap::new()),
            pins: Mutex::new(pins),
            key_store: keys,
            supervisor,
            links: Mutex::new(HashMap::new()),
            space_locks: Mutex::new(HashMap::new()),
            outbox_retry_at: Mutex::new(HashMap::new()),
            ticker: Mutex::new(None),
            auto_admit: Mutex::new(HashMap::new()),
            presence_gate: tokio::sync::Mutex::new(()),
        });
        let node = Node { inner };
        let pinned: Vec<HomeTicket> = node
            .inner
            .pins
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .cloned()
            .collect();
        for t in &pinned {
            node.ensure_link(&home_of_ticket(t), vec![t.node.node.clone()]);
        }
        for f in &follows {
            node.inner.refresh_outbox(&f.space);
            node.ensure_link(&f.home, vec![f.home.node.clone()]);
        }
        let weak = Arc::downgrade(&node.inner);
        let every = node.inner.cfg.tick_every;
        let ticker = tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                let Some(inner) = weak.upgrade() else { return };
                Node { inner }.tick();
            }
        });
        *node.inner.ticker.lock().unwrap_or_else(|p| p.into_inner()) = Some(ticker);
        Ok(node)
    }

    /// Stops the node's tasks: the links, and the clock of the views. Child roles die with their supervisor.
    pub fn shutdown(&self) {
        if let Some(t) = self
            .inner
            .ticker
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            t.abort();
        }
        for (_, (_, link)) in self
            .inner
            .links
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain()
        {
            link.task.abort();
        }
        self.inner.engine().end_all();
    }

    /// This device's node id.
    pub fn node_id(&self) -> String {
        self.inner.key.public_key().to_base64url()
    }

    /// The actor this device acts as.
    pub fn actor(&self) -> &str {
        &self.inner.cfg.actor
    }

    /// Moves the views' clock to the injected clock's reading (the node does this by itself every
    /// `tick_every`; a simulation calls it after moving its clock).
    pub fn tick(&self) {
        let inner = &self.inner;
        let roles: Vec<RoleInfo> = inner
            .supervisor
            .status()
            .into_iter()
            .map(|(name, st)| RoleInfo {
                name,
                state: match st {
                    RoleStatus::Running => "running".into(),
                    RoleStatus::Exited(_) => "exited".into(),
                },
            })
            .collect();
        let mut engine = inner.engine();
        engine.set_node_info(NodeInfo {
            node_id: self.node_id(),
            roles,
        });
        engine.tick(inner.clock.steady_ms(), inner.now_secs());
    }

    /// How many views are open on this node.
    pub fn open_views(&self) -> usize {
        self.inner.engine().open_views()
    }

    /// A fresh read of a view's window, as a snapshot event: what a snapshot plus its diffs must equal.
    pub fn read(&self, query: &Query) -> Result<ViewEvent, NodeError> {
        self.inner.engine().read(query)
    }

    /// Sends what waits in the outbox of every space now (links also do this on their own).
    pub async fn flush(&self) {
        let spaces: Vec<String> = self
            .inner
            .db
            .follows()
            .unwrap_or_default()
            .into_iter()
            .map(|f| f.space)
            .collect();
        for space in spaces {
            self.inner.flush_space(&space, None, Duration::ZERO).await;
        }
    }

    // ───────────────────────── links ─────────────────────────

    fn ensure_link(&self, home: &HomeRef, trusted: Vec<String>) {
        let mut links = self.inner.links.lock().unwrap_or_else(|p| p.into_inner());
        if links.contains_key(&home.server) {
            return;
        }
        let (cmds_tx, cmds_rx) = mpsc::unbounded_channel();
        let (client_tx, client_rx) = watch::channel(None);
        let task = tokio::spawn(run_link(
            Arc::clone(&self.inner),
            home.clone(),
            trusted,
            cmds_tx.clone(),
            cmds_rx,
            client_tx,
        ));
        links.insert(
            home.server.clone(),
            (
                home.clone(),
                LinkHandle {
                    cmds: cmds_tx,
                    client: client_rx,
                    task,
                },
            ),
        );
    }

    fn home_ref(&self, server: &str) -> Option<HomeRef> {
        self.inner
            .links
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(server)
            .map(|(h, _)| h.clone())
    }

    /// Starts following `space`; false if it already is followed.
    fn follow(&self, home: &HomeRef, trusted: Vec<String>, space: &str) -> Result<bool, NodeError> {
        let new = self.inner.db.add_follow(space, home).map_err(unavailable)?;
        if !new {
            return Ok(false);
        }
        self.inner.engine().add_space(
            space,
            &home.server,
            self.inner.cfg.space_freshness.get(space).copied(),
        );
        self.ensure_link(home, trusted);
        if let Some((_, link)) = self
            .inner
            .links
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&home.server)
        {
            let _ = link.cmds.send(LinkCmd::AddSpace(space.to_owned()));
        }
        Ok(true)
    }

    fn unfollow(&self, space: &str) {
        let _ = self.inner.db.remove_follow(space);
        self.inner.engine().remove_space(space);
    }

    // ───────────────────────── writing the log ─────────────────────────

    /// The node around an inner, for the tasks of a link.
    pub(crate) fn from_inner(inner: Arc<Inner<T>>) -> Node<T> {
        Node { inner }
    }

    /// Starts following this actor's profile space (where its keys are), if it is not followed yet.
    fn follow_profile(&self) {
        let actor = self.inner.cfg.actor.clone();
        let space = format!("profile:{actor}");
        if self.inner.engine().has_space(&space) {
            return;
        }
        let Some(home) = grammar::server_of(&actor).and_then(|s| self.home_ref(s)) else {
            return;
        };
        let _ = self.follow(&home, vec![home.node.clone()], &space);
    }

    /// Signs an entry as this device's actor, queues it in the outbox and tries to send it.
    async fn append(
        &self,
        space: &str,
        kind: &str,
        body: Value,
        content: Option<Value>,
    ) -> Result<CallResult, NodeError> {
        let inner = &self.inner;
        let seen = inner
            .engine()
            .head(space)
            .ok_or_else(|| NodeError::not_found(format!("space {space} is not followed")))?;
        let (entry, content) = sign_entry(
            EntrySpec {
                space: space.to_owned(),
                kind: kind.to_owned(),
                author: inner.cfg.actor.clone(),
                key_id: inner.cfg.key_id.clone(),
                body,
                content,
                refs: vec![],
                seen,
                time_secs: inner.now_secs(),
            },
            &inner.key,
        )
        .map_err(|e| NodeError::invalid(e.to_string()))?;
        let id = entry_id(&entry).map_err(|e| NodeError::invalid(e.to_string()))?;
        let row = inner
            .db
            .outbox_insert(space, &id, &entry, content.as_ref())
            .map_err(unavailable)?;
        inner.refresh_outbox(space);
        match inner
            .flush_space(space, Some(row), inner.cfg.connect_wait)
            .await
        {
            Some(Outcome::Sent(r)) => {
                // Read your own writes: the record comes back on the subscription; wait for it, so that
                // the next command (and the views) see what this one did.
                let deadline = tokio::time::Instant::now() + inner.cfg.connect_wait;
                while inner.engine().head(space).unwrap_or(0) < r.seq
                    && tokio::time::Instant::now() < deadline
                {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                Ok(CallResult::Receipt {
                    space: r.space,
                    seq: r.seq,
                    id: r.id,
                    time: r.time,
                })
            }
            Some(Outcome::Rejected(e)) => Err(e),
            _ => Ok(CallResult::Queued {
                outbox_id: row,
                space: space.to_owned(),
                id,
            }),
        }
    }

    async fn client_of(&self, space: &str) -> Result<(String, Arc<Client<T::Conn>>), NodeError> {
        let server = self
            .inner
            .engine()
            .home_of(space)
            .ok_or_else(|| NodeError::not_found(format!("space {space} is not followed")))?;
        let client = self
            .inner
            .client_for(&server, self.inner.cfg.connect_wait)
            .await?;
        Ok((server, client))
    }

    /// A task that is known and still open; `conflict` if it is closed, `not_found` if unknown.
    fn check_task_open(&self, space: &str, task: &str) -> Result<(), NodeError> {
        let engine = self.inner.engine();
        match engine.task_open(space, task) {
            Some(true) => Ok(()),
            Some(false) => Err(NodeError::conflict("the task is closed")),
            None if engine.has_space(space) => Err(NodeError::not_found("no such task")),
            None => Ok(()), // an unknown space is reported by the append
        }
    }

    fn require_followed(&self, space: &str) -> Result<(), NodeError> {
        if self.inner.engine().has_space(space) {
            Ok(())
        } else {
            Err(NodeError::not_found(format!(
                "space {space} is not followed"
            )))
        }
    }

    /// An entry in this actor's profile space (`key.add`, `key.revoke`), which the device follows on demand.
    async fn profile_append(&self, kind: &str, body: Value) -> Result<CallResult, NodeError> {
        let actor = self.inner.cfg.actor.clone();
        let space = format!("profile:{actor}");
        if !self.inner.engine().has_space(&space) {
            let server = grammar::server_of(&actor).unwrap_or_default().to_owned();
            let home = self
                .home_ref(&server)
                .ok_or_else(|| NodeError::unavailable("no link to this actor's home"))?;
            self.follow(&home, vec![home.node.clone()], &space)?;
        }
        // The profile's records must have arrived before an entry can say what it has seen.
        let deadline = tokio::time::Instant::now() + self.inner.cfg.connect_wait;
        while self.inner.engine().head(&space).unwrap_or(0) == 0
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        self.append(&space, kind, body, None).await
    }

    /// The number the next item of a session's stream will carry.
    fn next_item(&self, space: &str, session: &str) -> Result<u64, NodeError> {
        let key = format!("n/{space}/{session}");
        Ok(self
            .inner
            .db
            .config(&key)
            .map_err(unavailable)?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
            + 1)
    }

    /// Sends one profile item on a session's stream, one sender per session at a time, and returns its
    /// number.
    async fn publish_item(
        &self,
        space: &str,
        session: &str,
        name: &str,
        body: Value,
    ) -> Result<u64, NodeError> {
        let lock = self.inner.space_lock(&format!("stream/{space}/{session}"));
        let _held = lock.lock().await;
        self.send_item(space, session, name, body).await
    }

    /// Sends one item; the caller holds the session's stream lock.
    async fn send_item(
        &self,
        space: &str,
        session: &str,
        name: &str,
        body: Value,
    ) -> Result<u64, NodeError> {
        let inner = &self.inner;
        let (_, client) = self.client_of(space).await?;
        let n = self.next_item(space, session)?;
        let item = obj([
            ("space", s(space)),
            ("session", s(session)),
            ("n", int(n)),
            ("time", s(&format_time(inner.now_secs()))),
            ("type", s("profile")),
            ("name", s(name)),
            ("body", body),
        ]);
        client
            .call("stream", args([("item", item)]))
            .await
            .map_err(|e| map_net_error(&e))?;
        inner
            .db
            .set_config(&format!("n/{space}/{session}"), &n.to_string())
            .map_err(unavailable)?;
        Ok(n)
    }

    pub(crate) async fn execute(&self, cmd: Command) -> Result<CallResult, NodeError> {
        let inner = &self.inner;
        match cmd {
            Command::SpaceCreate {
                home,
                name,
                visibility,
                join,
            } => {
                if !["members", "public"].contains(&visibility.as_str())
                    || !["invite", "open", "closed"].contains(&join.as_str())
                {
                    return Err(NodeError::invalid(
                        "visibility or join is not one of the policy's",
                    ));
                }
                if !grammar::is_server_name(&home.server) {
                    return Err(NodeError::invalid("not a ServerName"));
                }
                let space = format!("{}:{}", random_id(10), home.server);
                self.follow(&home, vec![home.node.clone()], &space)?;
                let created = self
                    .append(
                        &space,
                        "space.create",
                        obj([("policy", space_policy(&visibility, &join))]),
                        Some(obj([("name", s(&name))])),
                    )
                    .await;
                match created {
                    Err(e) => {
                        self.unfollow(&space);
                        Err(e)
                    }
                    Ok(_) => Ok(CallResult::Space { space }),
                }
            }
            Command::SpaceJoin { ticket } => {
                let t = InviteTicket::parse(&ticket)
                    .ok_or_else(|| NodeError::invalid("not an invite ticket"))?;
                let trusted = han_net::dial::trusted_nodes_for_ticket(&inner.cfg.fetcher, &t)
                    .await
                    .map_err(|e| NodeError::invalid(e.to_string()))?;
                if inner.engine().has_space(&t.space) {
                    return Err(NodeError::conflict("the space is already followed"));
                }
                let home = HomeRef {
                    server: t.server.clone(),
                    node: t.node.node.clone(),
                    addrs: t.node.addrs.clone(),
                    relays: t.node.relays.clone(),
                };
                self.ensure_link(&home, trusted);
                let client = inner
                    .client_for(&home.server, inner.cfg.connect_wait)
                    .await?;
                client
                    .call(
                        "join",
                        args([("space", s(&t.space)), ("secret", s(&t.secret))]),
                    )
                    .await
                    .map_err(|e| map_net_error(&e))?;
                self.follow(&home, vec![home.node.clone()], &t.space)?;
                Ok(CallResult::Space { space: t.space })
            }
            Command::HomePin { ticket } => {
                let t = HomeTicket::parse(&ticket)
                    .ok_or_else(|| NodeError::invalid("not a home ticket"))?;
                if t.node.addrs.is_empty() && t.node.relays.is_empty() {
                    return Err(NodeError::invalid("the ticket's node has no address"));
                }
                inner
                    .db
                    .set_config(&format!("{PIN_PREFIX}{}", t.server), &t.to_text())
                    .map_err(unavailable)?;
                inner
                    .key_cache
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&t.server);
                let home = home_of_ticket(&t);
                let trusted = vec![t.node.node.clone()];
                inner
                    .pins
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(t.server.clone(), t);
                self.ensure_link(&home, trusted);
                Ok(CallResult::Done)
            }
            Command::SpaceFollow { home, space } => {
                if !grammar::is_space_id(&space) {
                    return Err(NodeError::invalid("not a SpaceId"));
                }
                if !self.follow(&home, vec![home.node.clone()], &space)? {
                    return Err(NodeError::conflict("the space is already followed"));
                }
                Ok(CallResult::Space { space })
            }
            Command::Invite {
                space,
                ttl_secs,
                expires_at,
                auto_admit,
            } => {
                let ttl = match (ttl_secs, expires_at) {
                    (Some(_), Some(_)) => {
                        return Err(NodeError::invalid("give ttl_secs or expires_at, not both"));
                    }
                    (None, Some(t)) => {
                        let ahead = grammar::epoch_seconds(&t)
                            .ok_or_else(|| NodeError::invalid("expires_at is not a HAN Time"))?
                            - inner.now_secs();
                        if !(1..=7 * 86_400).contains(&ahead) {
                            return Err(NodeError::invalid(
                                "expires_at must be within the next 7 days",
                            ));
                        }
                        ahead
                    }
                    (secs, None) => {
                        i64::try_from(secs.unwrap_or(86_400).clamp(1, 7 * 86_400)).unwrap_or(86_400)
                    }
                };
                let (server, client) = self.client_of(&space).await?;
                let home = self
                    .home_ref(&server)
                    .ok_or_else(|| NodeError::unavailable("no link to the home"))?;
                let mut secret = [0u8; 32];
                getrandom::fill(&mut secret).map_err(unavailable)?;
                client
                    .call(
                        "invite",
                        args([
                            ("space", s(&space)),
                            ("hash", s(&ids::sha256_hex(&secret))),
                            ("expiresAt", s(&format_time(inner.now_secs() + ttl))),
                        ]),
                    )
                    .await
                    .map_err(|e| map_net_error(&e))?;
                let ticket = InviteTicket {
                    server,
                    node: NodeAddr {
                        node: home.node,
                        addrs: home.addrs,
                        relays: home.relays,
                    },
                    space,
                    secret: base64url::encode(&secret),
                };
                if auto_admit {
                    let until = inner.now_secs() + ttl;
                    let mut window = inner.auto_admit.lock().unwrap_or_else(|p| p.into_inner());
                    let slot = window.entry(ticket.space.clone()).or_insert(until);
                    *slot = (*slot).max(until);
                }
                Ok(CallResult::Ticket {
                    text: ticket.to_text(),
                })
            }
            Command::MemberSet { space, actor, role } => {
                if !["owner", "member", "guest", "banned", "none"].contains(&role.as_str()) {
                    return Err(NodeError::invalid("unknown role"));
                }
                // Already so: nothing to append (a command that sets what is set changes nothing).
                if self.inner.engine().role_is(&space, &actor, &role) {
                    return Ok(CallResult::Done);
                }
                self.append(
                    &space,
                    "member.set",
                    obj([("actor", s(&actor)), ("role", s(&role))]),
                    None,
                )
                .await
            }
            Command::AgentCreate {
                name,
                server,
                operator: _,
            } => {
                let actor = format!("@{name}:{server}");
                if !grammar::is_actor_id(&actor) || name.is_empty() {
                    return Err(NodeError::invalid("not an ActorId"));
                }
                let key_name = format!("agent-{name}-{server}");
                // An agent that runs as another OS user keeps its key in its own state directory, where that
                // user (and not the node's other keys) can read it.
                let own = match &inner.cfg.data_dir {
                    Some(dir) => {
                        let file = AgentsFile::load(dir).map_err(NodeError::invalid)?;
                        file.agent(&actor)
                            .os_user
                            .is_some()
                            .then(|| file.keys_dir(dir, &actor).join("keys"))
                    }
                    None => None,
                };
                let seed = match own {
                    Some(dir) => {
                        let store = FileKeyStore::new(dir.clone());
                        let seed = store.load_or_create(&key_name).map_err(unavailable)?;
                        share_with_group(&dir.join(format!("{key_name}.key")));
                        seed
                    }
                    None => inner
                        .key_store
                        .load_or_create(&key_name)
                        .map_err(unavailable)?,
                };
                Ok(CallResult::Agent {
                    actor,
                    key_id: "k1".into(),
                    public_key: SigningKey::from_seed(seed).public_key().to_base64url(),
                })
            }
            Command::AgentStart {
                space,
                agent,
                harness,
                repo,
                issue,
            } => {
                self.require_followed(&space)?;
                if !grammar::is_actor_id(&agent) {
                    return Err(NodeError::invalid("not an ActorId"));
                }
                let home = inner
                    .engine()
                    .home_of(&space)
                    .and_then(|server| self.home_ref(&server))
                    .ok_or_else(|| NodeError::unavailable("no link to the space's home yet"))?;
                // The agent's operator is the one its profile names (§4.3), never this node's actor.
                let profile = format!("profile:{agent}");
                let operator = {
                    let engine = inner.engine();
                    if !engine.has_space(&profile) {
                        return Err(NodeError::not_found(format!(
                            "the profile of {agent} is not followed: follow {profile} first"
                        )));
                    }
                    match engine.profile_operator(&profile) {
                        Some(op) => op,
                        None if engine.head(&profile).unwrap_or(0) == 0 => {
                            return Err(NodeError::unavailable(format!(
                                "the profile of {agent} has not arrived yet"
                            )));
                        }
                        None => {
                            return Err(NodeError::invalid(format!(
                                "the profile of {agent} names no operator"
                            )));
                        }
                    }
                };
                let name = match &issue {
                    Some(i) => format!("host-{agent}#{i}"),
                    None => format!("host-{agent}"),
                };
                let mut a = vec![
                    "--space".to_owned(),
                    space,
                    "--agent".into(),
                    agent.clone(),
                    "--operator".into(),
                    operator,
                    "--home-server".into(),
                    home.server.clone(),
                    "--home-node".into(),
                    home.node.clone(),
                ];
                for addr in &home.addrs {
                    a.extend(["--home-addr".into(), addr.clone()]);
                }
                for relay in &home.relays {
                    a.extend(["--home-relay".into(), relay.clone()]);
                }
                let mut run_as = None;
                let mut harness_arg = harness.clone();
                if let Some(dir) = &inner.cfg.data_dir {
                    let file = AgentsFile::load(dir).map_err(NodeError::invalid)?;
                    let state = file.state_dir(dir, &agent);
                    a.extend([
                        "--data".into(),
                        state.to_string_lossy().into_owned(),
                        "--keys".into(),
                        file.keys_dir(dir, &agent).to_string_lossy().into_owned(),
                        "--sandbox".into(),
                        file.sandbox_of(&agent),
                    ]);
                    if file.agent(&agent).allow_advisory {
                        a.push("--allow-advisory".into());
                    }
                    let tools = file.toolchain_spec(dir, &agent);
                    if !tools.is_empty() {
                        a.extend([
                            "--toolchains".into(),
                            serde_json::to_string(&tools)
                                .map_err(|e| NodeError::invalid(e.to_string()))?,
                        ]);
                    }
                    // The harness as `han agent setup-harness` installed it: node and adapter.
                    let key = match harness.as_str() {
                        "claude-code" => "claude",
                        other => other,
                    };
                    // A driver file given as the harness is a path, not a record's name.
                    let named = key
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
                    let install = if named {
                        HarnessInstall::load(dir, key).map_err(NodeError::invalid)?
                    } else {
                        None
                    };
                    match install {
                        Some(h) => {
                            a.extend([
                                "--node".into(),
                                h.node.to_string_lossy().into_owned(),
                                "--adapter-dir".into(),
                                h.adapter_dir.to_string_lossy().into_owned(),
                            ]);
                            if let Some(driver) = h.driver {
                                harness_arg = driver.to_string_lossy().into_owned();
                            }
                        }
                        None if matches!(key, "claude" | "pi") => {
                            return Err(NodeError::unavailable(format!(
                                "the {key} harness is not set up on this node: run `han agent setup-harness {key}`"
                            )));
                        }
                        None => {}
                    }
                    if let Some(os_user) = file.agent(&agent).os_user {
                        if !cfg!(target_os = "linux") {
                            return Err(NodeError::unavailable(
                                "running an agent as another OS user is for Linux runners",
                            ));
                        }
                        run_as = Some(RunAs {
                            os_user,
                            state_dir: state,
                        });
                    }
                }
                a.extend(["--harness".into(), harness_arg]);
                if let Some(r) = repo {
                    a.extend(["--repo".into(), r]);
                }
                if let Some(i) = issue {
                    a.extend(["--issue".into(), i]);
                }
                // A home joined by ticket has no well-known: the host's own node pins the same ticket.
                let env = inner
                    .pins
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .get(&home.server)
                    .map(|t| vec![("HAN_HOME_TICKET".to_owned(), t.to_text())])
                    .unwrap_or_default();
                inner
                    .supervisor
                    .start(RoleSpec {
                        role: Role::Host,
                        name,
                        args: a,
                        env,
                        run_as,
                    })
                    .map_err(|e| match e {
                        SupervisorError::Running(_) => NodeError::conflict(e.to_string()),
                        other => NodeError::unavailable(other.to_string()),
                    })?;
                Ok(CallResult::Done)
            }
            Command::AgentStop { target } => {
                // The target is an agent (all its sessions) or one `host-<agent>#<issue>` name.
                let exact = format!("host-{target}");
                let prefix = format!("host-{target}#");
                let names: Vec<String> = inner
                    .supervisor
                    .status()
                    .into_iter()
                    .filter(|(n, s)| {
                        (n == &exact || n.starts_with(&prefix) || n == &target)
                            && *s == RoleStatus::Running
                    })
                    .map(|(n, _)| n)
                    .collect();
                if names.is_empty() {
                    return Err(NodeError::not_found(format!("{target} is not running")));
                }
                for n in names {
                    inner.supervisor.stop(&n).map_err(|e| match e {
                        SupervisorError::NotRunning(_) => NodeError::not_found(e.to_string()),
                        other => NodeError::unavailable(other.to_string()),
                    })?;
                }
                Ok(CallResult::Done)
            }
            Command::TaskCreate {
                space,
                to,
                ask,
                external,
            } => {
                if to.is_empty() {
                    return Err(NodeError::invalid("a task needs an assignee"));
                }
                let mut body = vec![("to", strs(&to))];
                if let Some(e) = &external {
                    body.push(("external", s(e)));
                }
                self.append(
                    &space,
                    "task",
                    Value::Object(body.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()),
                    Some(obj([("ask", s(&ask))])),
                )
                .await
            }
            Command::TaskUpdate { space, task, state } => {
                self.check_task_open(&space, &task)?;
                self.append(
                    &space,
                    "task.update",
                    obj([("task", s(&task)), ("state", s(&state))]),
                    None,
                )
                .await
            }
            Command::TaskCancel { space, task } => {
                self.check_task_open(&space, &task)?;
                self.append(&space, "task.cancel", obj([("task", s(&task))]), None)
                    .await
            }
            Command::SessionOpen {
                space,
                steer_mode,
                task,
                title,
            } => {
                let mode = steer_mode.unwrap_or_else(|| "auto".into());
                if !["auto", "guarded"].contains(&mode.as_str()) {
                    return Err(NodeError::invalid("steer mode is auto or guarded"));
                }
                let mut body = vec![("steerMode", s(&mode))];
                if let Some(t) = &task {
                    body.push(("task", s(t)));
                }
                let content = title.map(|t| obj([("title", s(&t))]));
                self.append(
                    &space,
                    "session.open",
                    Value::Object(body.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()),
                    content,
                )
                .await
            }
            Command::SessionClose { space, session } => {
                self.append(
                    &space,
                    "session.close",
                    obj([("session", s(&session))]),
                    None,
                )
                .await
            }
            Command::SessionInput {
                space,
                session,
                text,
            } => {
                self.append(
                    &space,
                    "session.input",
                    obj([("session", s(&session))]),
                    Some(obj([("text", s(&text))])),
                )
                .await
            }
            Command::StatusPublish { space, status } => {
                let mut status = *status;
                let session = status.session.clone();
                let lock = inner.space_lock(&format!("stream/{space}/{session}"));
                let _held = lock.lock().await;
                if status.seq == 0 {
                    status.seq = self.next_item(&space, &session)?;
                }
                let body =
                    json::to_value(&status).map_err(|e| NodeError::invalid(e.to_string()))?;
                let n = self
                    .send_item(&space, &session, "han.status/1", body)
                    .await?;
                Ok(CallResult::Status { n })
            }
            Command::LeaseSet {
                space,
                paths,
                ttl_secs,
                issue,
            } => {
                if paths.is_empty()
                    || paths.len() > 32
                    || paths.iter().any(|p| p.is_empty() || p.len() > 512)
                    || !(1..=7 * 86_400).contains(&ttl_secs)
                {
                    return Err(NodeError::invalid(
                        "a lease names 1 to 32 paths and lasts 1 s to 7 days",
                    ));
                }
                let mut body = vec![("paths", strs(&paths)), ("ttl", int(ttl_secs))];
                if let Some(i) = &issue {
                    body.push(("issue", s(i)));
                }
                self.append(
                    &space,
                    crate::model::LEASE_KIND,
                    Value::Object(body.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()),
                    None,
                )
                .await
            }
            Command::DeviceOffer => {
                let node_id = self.node_id();
                let digest = ids::sha256_hex(node_id.as_bytes());
                Ok(CallResult::DeviceCode {
                    code: format!("hand1-{node_id}"),
                    check: format!("{}-{}", &digest[..4], &digest[4..8]),
                    node_id,
                })
            }
            Command::DevicePair { code } => {
                let public = code
                    .strip_prefix("hand1-")
                    .filter(|p| han_core::PublicKey::from_base64url(p).is_ok())
                    .ok_or_else(|| NodeError::invalid("not a device pairing code"))?;
                let key_id = format!("d{}", &ids::sha256_hex(public.as_bytes())[..8]);
                self.profile_append(
                    "key.add",
                    obj([("keyId", s(&key_id)), ("publicKey", s(public))]),
                )
                .await
            }
            Command::DeviceRevoke { key_id } => {
                if key_id == inner.cfg.key_id {
                    return Err(NodeError::conflict("a device does not revoke its own key"));
                }
                self.profile_append("key.revoke", obj([("keyId", s(&key_id))]))
                    .await
            }
            Command::BlobFetch {
                space,
                hash,
                size,
                media_type,
            } => {
                self.require_followed(&space)?;
                if hash.len() != 64 || !grammar::is_hex(&hash) {
                    return Err(NodeError::invalid("hash is 64 lowercase hex digits"));
                }
                if !grammar::is_media_type(&media_type) {
                    return Err(NodeError::invalid("not a media type"));
                }
                if size > MAX_FACE_BLOB {
                    return Err(NodeError::invalid(format!(
                        "a face fetches at most {MAX_FACE_BLOB} bytes of a blob"
                    )));
                }
                let (_, client) = self.client_of(&space).await?;
                let r = obj([
                    ("hash", s(&hash)),
                    ("size", int(size)),
                    ("type", s(&media_type)),
                ]);
                let mut data: Vec<u8> = Vec::new();
                while (data.len() as u64) < size {
                    let want = (size - data.len() as u64).min(786_432);
                    let part = client
                        .call(
                            "blobGet",
                            args([
                                ("ref", r.clone()),
                                ("space", s(&space)),
                                ("offset", int(data.len() as u64)),
                                ("length", int(want)),
                            ]),
                        )
                        .await
                        .map_err(|e| map_net_error(&e))?;
                    if part["size"].as_u64() != Some(size) {
                        return Err(NodeError::invalid(
                            "the home holds a blob of another size than the reference names",
                        ));
                    }
                    let bytes = part["data"]
                        .as_str()
                        .and_then(|d| base64url::decode(d).ok())
                        .filter(|b| !b.is_empty() && b.len() as u64 <= want)
                        .ok_or_else(|| NodeError::unavailable("the home sent no bytes"))?;
                    data.extend_from_slice(&bytes);
                }
                if ids::sha256_hex(&data) != hash {
                    return Err(NodeError::invalid(
                        "the blob does not hash to what the reference names",
                    ));
                }
                Ok(CallResult::Blob {
                    hash,
                    media_type,
                    data: base64url::encode(&data),
                })
            }
            Command::BlobPut {
                space,
                media_type,
                data,
            } => {
                self.require_followed(&space)?;
                if !grammar::is_media_type(&media_type) {
                    return Err(NodeError::invalid("not a media type"));
                }
                let bytes = base64url::decode(&data)
                    .map_err(|_| NodeError::invalid("data is not base64url"))?;
                if bytes.is_empty() {
                    return Err(NodeError::invalid("a blob has at least one byte"));
                }
                let size = bytes.len() as u64;
                if size > MAX_FACE_BLOB {
                    return Err(NodeError::invalid(format!(
                        "a blob put through the node is at most {MAX_FACE_BLOB} bytes"
                    )));
                }
                let hash = ids::sha256_hex(&bytes);
                let (_, client) = self.client_of(&space).await?;
                let r = obj([
                    ("hash", s(&hash)),
                    ("size", int(size)),
                    ("type", s(&media_type)),
                ]);
                let mut offset = 0u64;
                for part in bytes.chunks(786_432) {
                    client
                        .call(
                            "blobPut",
                            args([
                                ("ref", r.clone()),
                                ("offset", int(offset)),
                                ("data", s(&base64url::encode(part))),
                            ]),
                        )
                        .await
                        .map_err(|e| map_net_error(&e))?;
                    offset += part.len() as u64;
                }
                Ok(CallResult::BlobRef {
                    hash,
                    size,
                    media_type,
                })
            }
            Command::EffectApprove {
                space,
                proposal,
                expires_at,
                note,
            } => {
                let found = inner.engine().approval_subject(&space, &proposal);
                let Some(subject) = found else {
                    return Err(NodeError::not_found("no such proposal"));
                };
                if !subject.open {
                    return Err(NodeError::conflict(
                        "the proposal no longer waits for approval",
                    ));
                }
                let mut body = vec![
                    ("proposal", s(&proposal)),
                    ("planDigest", s(&subject.digest)),
                ];
                if let Some(t) = &expires_at {
                    if !grammar::epoch_seconds(t).is_some_and(|at| at > inner.now_secs()) {
                        return Err(NodeError::invalid(
                            "expires_at must be a HAN Time in the future",
                        ));
                    }
                    body.push(("expiresAt", s(t)));
                }
                // A person confirms on this device, out of band, before anything is signed (SR-106).
                presence::confirm(
                    &inner.db,
                    inner.cfg.presence.as_ref(),
                    inner.cfg.presence_timeout,
                    &inner.presence_gate,
                    presence::EFFECT_APPROVE,
                    &space,
                    &subject,
                )
                .await?;
                // The wait can be long: sign only the plan the person confirmed, and only if it still waits.
                let still = inner.engine().approval_subject(&space, &proposal);
                if !still.is_some_and(|n| n.open && n.digest == subject.digest) {
                    return Err(NodeError::conflict(
                        "the proposal changed while you were confirming; nothing was signed",
                    ));
                }
                self.append(
                    &space,
                    "effect.approve",
                    Value::Object(body.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()),
                    note.map(|n| obj([("note", s(&n))])),
                )
                .await
            }
            Command::EffectPropose {
                space,
                executor,
                plan,
                task,
                cost,
                repeatable,
                expires_at,
            } => {
                let plan = json::parse(plan.as_bytes())
                    .map_err(|e| NodeError::invalid(format!("plan: {e}")))?;
                let action = plan
                    .get("action")
                    .and_then(Value::as_str)
                    .ok_or_else(|| NodeError::invalid("a plan names its action"))?
                    .to_owned();
                if plan.get("target").and_then(Value::as_str).is_none() {
                    return Err(NodeError::invalid("a plan names its target"));
                }
                let digest =
                    ids::json_digest(&plan).map_err(|e| NodeError::invalid(e.to_string()))?;
                let mut body = vec![
                    ("executor", s(&executor)),
                    ("action", s(&action)),
                    ("planDigest", s(&digest)),
                    ("repeatable", Value::Bool(repeatable)),
                ];
                if let Some(t) = &task {
                    body.push(("task", s(t)));
                }
                if let Some((unit, amount)) = &cost {
                    body.push(("cost", obj([("unit", s(unit)), ("amount", int(*amount))])));
                }
                if let Some(x) = &expires_at {
                    body.push(("expiresAt", s(x)));
                }
                self.append(
                    &space,
                    "effect.propose",
                    Value::Object(body.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()),
                    Some(obj([("plan", plan)])),
                )
                .await
            }
            Command::EffectClaim {
                space,
                proposal,
                attempt,
                window,
            } => {
                let key_id = inner.cfg.key_id.clone();
                self.append(
                    &space,
                    "effect.claim",
                    obj([
                        ("proposal", s(&proposal)),
                        ("attempt", int(attempt)),
                        ("instance", s(&key_id)),
                        ("window", int(window)),
                    ]),
                    None,
                )
                .await
            }
            Command::EffectOutcome {
                space,
                claim,
                result,
                started,
                cost,
            } => {
                let mut body = vec![
                    ("claim", s(&claim)),
                    ("result", s(&result)),
                    ("started", s(&started)),
                ];
                if let Some((unit, amount)) = &cost {
                    body.push(("cost", obj([("unit", s(unit)), ("amount", int(*amount))])));
                }
                self.append(
                    &space,
                    "effect.outcome",
                    Value::Object(body.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()),
                    None,
                )
                .await
            }
            Command::PolicySet { space, policy } => {
                let policy = json::parse(policy.as_bytes())
                    .map_err(|e| NodeError::invalid(format!("policy: {e}")))?;
                self.append(&space, "policy.set", obj([("policy", policy)]), None)
                    .await
            }
            Command::StreamPublish {
                space,
                session,
                name,
                body,
            } => {
                if name == "han.status/1" {
                    return Err(NodeError::invalid("status items go through StatusPublish"));
                }
                let body = json::parse(body.as_bytes())
                    .map_err(|e| NodeError::invalid(format!("body: {e}")))?;
                let n = self.publish_item(&space, &session, &name, body).await?;
                Ok(CallResult::Status { n })
            }
            Command::OutboxRetry { outbox_id } => {
                let info = inner
                    .db
                    .outbox_get(outbox_id)
                    .map_err(unavailable)?
                    .ok_or_else(|| NodeError::not_found("no such outbox entry"))?;
                if info.state != "failed" {
                    return Err(NodeError::conflict(format!(
                        "the entry is {}, not failed",
                        info.state
                    )));
                }
                inner.db.outbox_retry(outbox_id).map_err(unavailable)?;
                inner
                    .outbox_retry_at
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&outbox_id);
                inner.refresh_outbox(&info.space);
                match inner
                    .flush_space(&info.space, Some(outbox_id), inner.cfg.connect_wait)
                    .await
                {
                    Some(Outcome::Sent(r)) => Ok(CallResult::Receipt {
                        space: r.space,
                        seq: r.seq,
                        id: r.id,
                        time: r.time,
                    }),
                    Some(Outcome::Rejected(e)) => Err(e),
                    _ => Ok(CallResult::Queued {
                        outbox_id,
                        space: info.space,
                        id: info.entry_id,
                    }),
                }
            }
            Command::EffectCancel { space, proposal } => {
                let found = inner.engine().plan_digest(&space, &proposal);
                match found {
                    None => return Err(NodeError::not_found("no such proposal")),
                    Some((_, false)) => {
                        return Err(NodeError::conflict("the proposal is no longer open"));
                    }
                    Some(_) => {}
                }
                self.append(
                    &space,
                    "effect.cancel",
                    obj([("proposal", s(&proposal))]),
                    None,
                )
                .await
            }
        }
    }
}

struct Unsubscribe {
    engine: Arc<Mutex<Engine>>,
    id: u64,
}

impl Drop for Unsubscribe {
    fn drop(&mut self) {
        self.engine
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .unsubscribe(self.id);
    }
}

impl<T: Transport> NodeApi for Node<T> {
    fn call(&self, cmd: Command) -> BoxFuture<'_, Result<CallResult, NodeError>> {
        Box::pin(async move {
            let what = serde_json::to_value(&cmd)
                .ok()
                .and_then(|v| v["type"].as_str().map(str::to_owned))
                .unwrap_or_default();
            let result = self.execute(cmd).await;
            if let Err(e) = &result {
                self.inner.engine().note_error(&what, e);
            }
            result
        })
    }

    fn view(&self, query: Query) -> ViewStream {
        if matches!(query, Query::Devices { .. }) {
            self.follow_profile();
        }
        let (sink, stream) = ViewStream::channel();
        let id = self.inner.engine().subscribe(query, sink);
        match id {
            Some(id) => stream.guarded(Unsubscribe {
                engine: Arc::clone(&self.inner.engine),
                id,
            }),
            None => stream,
        }
    }
}

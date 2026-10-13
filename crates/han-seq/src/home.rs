//! The home: the section 4.1 pipeline with a single writer, over a [`Store`], a [`Clock`] and the fan-out.
//!
//! One mutex serialises everything that reads or writes a space, which is the "one serial transaction per
//! space" of section 4.1 (coarser than it needs to be; a lock per space is a later refinement).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use han_core::admit::{AdmitSpace, AdmitSpaces};
use han_core::json::{self, Value};
use han_core::policy::{
    Body, Ctx, Effect, PolicyEntry, ProfileView, Record, SpaceState, Verdict, abandon_after,
    can_read, decide, fold, relevant_actors,
};
use han_core::types::{ActorId, Durability};
use han_core::{Entry, LogRecord, PublicKey, Receipt, SigningKey, grammar};

use crate::clock::{Clock, TestClock};
use crate::error::{Result, WireError};
use crate::fanout::{Hooks, NoHooks, Subs};
use crate::identity::{PinKey, PinView, ServerKey};
use crate::jv;
use crate::paging::MapOrder;
use crate::services::{BlobState, StreamState};
use crate::store::{Store, StoreError, Stored};
use crate::time::{format_time, parse_time};

/// The first instant of every server key window.
pub const EPOCH: &str = "1970-01-01T00:00:00Z";

/// Why a home could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// Storage failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A stored chain does not verify; the home refuses to serve a corrupt log.
    #[error("the stored chain of {space} is corrupt at seq {seq}")]
    Corrupt {
        /// The space.
        space: String,
        /// The seq.
        seq: u64,
    },
    /// The server key does not match the one the store was created with.
    #[error("{0}")]
    Key(String),
}

/// How a home is set up.
pub struct HomeConfig {
    /// The ServerName this home answers to.
    pub server: String,
    /// The key that signs receipts (and entries authored by the server actor).
    pub key: SigningKey,
    /// Its id in `/.well-known/han`.
    pub key_id: String,
    /// The durability the store provides (R-03): `Local` for a single machine.
    pub durability: Durability,
}

/// Where other servers' keys and nodes come from (`/.well-known/han`). The network layer fills it.
pub trait Directory: Send + Sync + 'static {
    /// The nodes and keys of `server`, if known.
    fn server(&self, server: &str) -> Option<ServerInfo>;
}

/// What a home knows of another server.
#[derive(Clone, Debug)]
pub struct ServerInfo {
    /// Node ids of its `sequencer` list.
    pub nodes: Vec<String>,
    /// Its published keys.
    pub keys: Vec<ServerKey>,
}

/// A directory that knows no other server.
pub struct NoDirectory;

impl Directory for NoDirectory {
    fn server(&self, _server: &str) -> Option<ServerInfo> {
        None
    }
}

/// Whose `space.create` for a space id naming this server the home accepts (section 4.1 step 2: "the
/// server's own hosting rules").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum HostingPolicy {
    /// Only known actors: those whose profile space this home serves (its own users, agents and server
    /// actor) and those holding a role, not banned, in any space it hosts. The default.
    #[default]
    KnownActors,
    /// Anyone who can authenticate.
    Open,
    /// Known actors, and also the listed ones: each entry is an ActorId or a bare ServerName (every actor
    /// of that server).
    AllowList(Vec<String>),
}

/// The reason of the hosting rule's `denied`.
pub const HOSTING_DENIED: &str =
    "this home creates spaces only for its own actors; ask an owner for an invite";

/// Remote profile fetches a stranger's request may trigger: at most this many per window.
const FETCH_CALLS: u32 = 20;
const FETCH_WINDOW_MS: u64 = 60_000;

/// A per-author rate limit (section 4.1 step 6): the `retryAfterMs` to answer with, or `None` when under it.
pub trait RateLimit: Send + Sync + 'static {
    /// Checks one append by `author`.
    fn check(&self, author: &str) -> Option<u64>;
}

/// One space.
pub(crate) struct Space {
    pub records: Vec<LogRecord>,
    pub pins: Vec<BTreeMap<ActorId, PinView>>,
    pub state: Arc<SpaceState>,
    pub order: MapOrder,
    pub ids: HashMap<String, u64>,
}

/// Everything a home mutates.
pub(crate) struct Inner {
    pub spaces: HashMap<String, Space>,
    pub moved: HashMap<String, String>,
    pub admit: Arc<AdmitSpaces>,
    pub subs: Subs,
    pub blobs: BlobState,
    pub streams: StreamState,
    /// Steady reading (ms) when this run ordered a claim, by claim id (R-16).
    pub claim_steady: HashMap<String, u64>,
    /// Steady reading (ms) when this run began: claims ordered earlier count from here (R-16).
    pub run_start: u64,
    pub broken: bool,
    pub max_frame_bytes: usize,
    /// Open-tier calls per node: window start (steady ms) and count (N-02).
    pub open_rate: HashMap<String, (u64, u32)>,
    pub own_nodes: Vec<String>,
    pub server_keys: Vec<ServerKey>,
    pub hooks: Arc<dyn Hooks>,
    pub directory: Arc<dyn Directory>,
    pub limiter: Option<Arc<dyn RateLimit>>,
    pub hosting: HostingPolicy,
    /// Remote profile fetches triggered by hosting-rule requests: window start (steady ms) and count.
    pub fetch_rate: (u64, u32),
}

/// The sequencer of a server: the engine behind every call.
pub struct Home {
    pub(crate) server: String,
    key: SigningKey,
    key_id: String,
    pub(crate) durability: Durability,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) test_clock: Option<Arc<TestClock>>,
    pub(crate) store: Arc<dyn Store>,
    pub(crate) inner: Mutex<Inner>,
    /// Counts the records that can change who a node vouches for or what it may do (X14).
    changes: tokio::sync::watch::Sender<u64>,
}

/// The defaults of a profile space's policy (section 4.3): a sequencer SHOULD write exactly this one.
pub fn profile_policy() -> Value {
    use jv::{int, obj, s};
    obj([
        ("visibility", s("public")),
        ("join", s("closed")),
        ("membersMayInvite", Value::Bool(false)),
        ("allowAgents", s("any")),
        ("approvals", Value::Array(vec![])),
        ("separateApprover", Value::Bool(false)),
        ("budget", Value::Null),
        ("maxDelegationDepth", int(0)),
        ("minDurability", s("local")),
        ("launchWindow", int(30)),
        ("extensionsMinRole", s("owner")),
        (
            "sessions",
            obj([
                ("watch", s("nobody")),
                ("read", s("nobody")),
                ("steer", s("nobody")),
                ("modes", Value::Array(vec![s("auto")])),
            ]),
        ),
        ("servers", obj([("deny", Value::Array(vec![]))])),
    ])
}

fn policy_profiles(pins: &BTreeMap<ActorId, PinView>) -> BTreeMap<ActorId, ProfileView> {
    pins.iter()
        .map(|(a, v)| (a.clone(), v.for_policy()))
        .collect()
}

impl WireError {
    pub(crate) fn from_json(e: json::JsonError) -> Self {
        WireError::invalid(e.to_string())
    }
}

/// Whether `reader` may read some seq of the space (section 3: otherwise `not_found`, as for a missing space).
pub(crate) fn readable_any(state: &SpaceState, reader: &str) -> bool {
    let mut candidates: HashSet<u64> = HashSet::from([state.head]);
    let server = reader.strip_prefix("@:");
    for (actor, intervals) in &state.role_history {
        let relevant = match server {
            Some(s) => grammar::server_of(actor) == Some(s),
            None => actor == reader,
        };
        if relevant {
            candidates.extend(intervals.iter().map(|i| i.from));
        }
    }
    candidates.into_iter().any(|q| can_read(state, reader, q))
}

impl Home {
    /// Opens a home over `store`: loads and checks every stored chain (a corrupt one is refused) and
    /// rebuilds the states.
    pub fn open(
        cfg: HomeConfig,
        clock: Arc<dyn Clock>,
        store: Arc<dyn Store>,
    ) -> std::result::Result<Home, OpenError> {
        Self::build(cfg, clock, None, store)
    }

    /// Opens a home in test mode (G-11): a clock that moves only by `test.clock`, the peer filter admitting
    /// every node, and the control calls `test.clock` and `test.limits`.
    pub fn open_test(
        cfg: HomeConfig,
        clock: Arc<TestClock>,
        store: Arc<dyn Store>,
    ) -> std::result::Result<Home, OpenError> {
        Self::build(cfg, clock.clone(), Some(clock), store)
    }

    fn build(
        cfg: HomeConfig,
        clock: Arc<dyn Clock>,
        test_clock: Option<Arc<TestClock>>,
        store: Arc<dyn Store>,
    ) -> std::result::Result<Home, OpenError> {
        let public_key = cfg.key.public_key().to_base64url();
        let mut keys: Vec<ServerKey> = match store.get("meta", "own_keys")? {
            Some(bytes) => serde_json::from_slice(&bytes).map_err(StoreError::from)?,
            None => Vec::new(),
        };
        match keys.iter().find(|k| k.key_id == cfg.key_id) {
            Some(k)
                if k.public_key != public_key || k.until.is_some() || k.compromised.is_some() =>
            {
                return Err(OpenError::Key(format!(
                    "server key {} is retired or belongs to another key",
                    cfg.key_id
                )));
            }
            Some(_) => {}
            None => {
                keys.push(ServerKey {
                    key_id: cfg.key_id.clone(),
                    public_key,
                    from: EPOCH.to_owned(),
                    until: None,
                    compromised: None,
                });
                store.put(
                    "meta",
                    "own_keys",
                    &serde_json::to_vec(&keys).map_err(StoreError::from)?,
                )?;
            }
        }
        let run_start = clock.steady_ms();
        let is_test = test_clock.is_some();
        let home = Home {
            server: cfg.server,
            key: cfg.key,
            key_id: cfg.key_id,
            durability: cfg.durability,
            clock,
            test_clock,
            store: store.clone(),
            changes: tokio::sync::watch::channel(0).0,
            inner: Mutex::new(Inner {
                spaces: HashMap::new(),
                moved: HashMap::new(),
                admit: Arc::new(BTreeMap::new()),
                subs: Subs::default(),
                blobs: BlobState::default(),
                streams: StreamState::default(),
                claim_steady: HashMap::new(),
                run_start,
                broken: false,
                max_frame_bytes: han_core::limits::MAX_FRAME_BYTES,
                open_rate: HashMap::new(),
                own_nodes: Vec::new(),
                server_keys: keys,
                hooks: Arc::new(NoHooks),
                directory: Arc::new(NoDirectory),
                limiter: None,
                // test mode (G-11) keeps the wire vectors' strangers able to create spaces
                hosting: if is_test {
                    HostingPolicy::Open
                } else {
                    HostingPolicy::KnownActors
                },
                fetch_rate: (run_start, 0),
            }),
        };
        {
            let mut g = home.lock();
            for space in store.spaces()? {
                home.load_space(&mut g, &space)?;
            }
            g.blobs.load(&*store)?;
            g.streams.load(&*store)?;
        }
        Ok(home)
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn load_space(&self, g: &mut Inner, space: &str) -> std::result::Result<(), OpenError> {
        let stored = self.store.load(space)?;
        let mut sp: Option<Space> = None;
        for (i, st) in stored.into_iter().enumerate() {
            let rec = st.record;
            let corrupt = || OpenError::Corrupt {
                space: space.to_owned(),
                seq: rec.receipt.seq,
            };
            let prev = sp.as_ref().and_then(|s| s.records.last());
            let sound = rec.receipt.seq == i as u64 + 1
                && rec.entry.id().ok().as_deref() == Some(rec.receipt.id.as_str())
                && match prev {
                    Some(p) => p.receipt.hash().ok().as_deref() == Some(rec.receipt.prev.as_str()),
                    None => rec.receipt.prev.is_empty(),
                }
                && self.verify_own_receipt(g, &rec.receipt);
            if !sound {
                return Err(corrupt());
            }
            let ctx =
                Ctx::new(&rec.receipt.time, policy_profiles(&st.pins)).map_err(|_| corrupt())?;
            let record = Record::from_log_record(&rec).map_err(|_| corrupt())?;
            let old = sp.as_ref().map(|s| &*s.state);
            let new = fold(old, &record, &ctx).map_err(|_| corrupt())?;
            let s = sp.get_or_insert_with(|| Space {
                records: Vec::new(),
                pins: Vec::new(),
                state: Arc::new(new.clone()),
                order: MapOrder::default(),
                ids: HashMap::new(),
            });
            s.order.advance(
                if s.records.is_empty() {
                    None
                } else {
                    Some(&s.state)
                },
                &new,
            );
            s.state = Arc::new(new);
            s.ids.insert(rec.receipt.id.clone(), rec.receipt.seq);
            if rec.entry.kind == "space.move"
                && let Some(to) = rec.entry.body.get("to").and_then(Value::as_str)
            {
                if to == self.server {
                    g.moved.remove(space);
                } else {
                    g.moved.insert(space.to_owned(), to.to_owned());
                }
            }
            s.pins.push(st.pins);
            s.records.push(rec);
        }
        if let Some(s) = sp {
            g.admit_update(space, &s.state);
            g.spaces.insert(space.to_owned(), s);
        }
        Ok(())
    }

    /// A receipt of this server must verify under the key set it published; receipts of another home
    /// (after a move) are the verifier's business and are not checked here.
    fn verify_own_receipt(&self, g: &Inner, r: &Receipt) -> bool {
        if r.home != self.server {
            return true;
        }
        g.server_keys
            .iter()
            .find(|k| k.key_id == r.home_key && k.covers(&r.time))
            .and_then(|k| PublicKey::from_base64url(&k.public_key).ok())
            .is_some_and(|pk| r.verify(&pk).is_ok())
    }

    // ───────────────────────── configuration ─────────────────────────

    /// The ServerName.
    pub fn server(&self) -> &str {
        &self.server
    }

    /// The node ids of this server's sequencer nodes: they vouch for `@:S` (N-02) and are what
    /// `/.well-known/han` lists.
    pub fn set_nodes(&self, nodes: Vec<String>) {
        self.lock().own_nodes = nodes;
    }

    /// Installs the hooks that see every ordered record and the notices for other servers.
    pub fn set_hooks(&self, hooks: Arc<dyn Hooks>) {
        self.lock().hooks = hooks;
    }

    /// Installs the source of other servers' nodes and keys.
    pub fn set_directory(&self, directory: Arc<dyn Directory>) {
        self.lock().directory = directory;
    }

    /// Installs a per-author rate limit (section 4.1 step 6).
    pub fn set_rate_limit(&self, limit: Arc<dyn RateLimit>) {
        self.lock().limiter = Some(limit);
    }

    /// Sets the hosting policy: whose `space.create` for a space of this server is accepted.
    pub fn set_hosting(&self, policy: HostingPolicy) {
        self.lock().hosting = policy;
    }

    /// The hosting policy (default [`HostingPolicy::KnownActors`]; test-mode homes start `Open`).
    pub fn hosting(&self) -> HostingPolicy {
        self.lock().hosting.clone()
    }

    /// For a carrier, before it fetches the profile or well-known document of `actor`'s server on behalf of
    /// a request: `Ok` only if a `space.create` by `actor` could pass the hosting rule, and the fetch is
    /// within the budget (`limit` otherwise). Strangers get `denied` without a fetch.
    pub fn may_fetch_profile(&self, actor: &str) -> Result<()> {
        let mut g = self.lock();
        if !self.hosting_allows(&g, actor) {
            return Err(WireError::denied(HOSTING_DENIED));
        }
        self.charge_fetch(&mut g)
    }

    /// Whether this home serves the profile space of `actor` (or `actor` is its server actor), or `actor`
    /// holds a role, not banned, in a space it hosts.
    fn is_known_actor(&self, g: &Inner, actor: &str) -> bool {
        if actor == format!("@:{}", self.server) {
            return true;
        }
        if grammar::server_of(actor) == Some(self.server.as_str())
            && g.spaces.contains_key(&format!("profile:{actor}"))
        {
            return true;
        }
        g.spaces
            .values()
            .any(|s| s.state.roles.get(actor).is_some_and(|r| r.reads()))
    }

    /// The hosting rule for `actor` under the current policy.
    fn hosting_allows(&self, g: &Inner, actor: &str) -> bool {
        match &g.hosting {
            HostingPolicy::Open => true,
            HostingPolicy::KnownActors => self.is_known_actor(g, actor),
            HostingPolicy::AllowList(list) => {
                self.is_known_actor(g, actor)
                    || list
                        .iter()
                        .any(|e| e == actor || grammar::server_of(actor) == Some(e.as_str()))
            }
        }
    }

    fn charge_fetch(&self, g: &mut Inner) -> Result<()> {
        let now = self.steady_ms();
        let slot = &mut g.fetch_rate;
        if now.saturating_sub(slot.0) >= FETCH_WINDOW_MS {
            *slot = (now, 0);
        }
        slot.1 += 1;
        if slot.1 > FETCH_CALLS {
            let wait = FETCH_WINDOW_MS - now.saturating_sub(slot.0);
            return Err(WireError::limit(
                "too many profile fetches for strangers",
                wait.max(1),
            ));
        }
        Ok(())
    }

    /// Changes whenever a record that can revoke a key or remove a role is ordered: a carrier re-checks the
    /// nodes behind its connections then (X14), not only every 300 s.
    pub fn watch_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// The spaces this home holds records of.
    pub fn spaces(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.lock().spaces.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// The current state of a space this home holds (a view of the log, for views and tools).
    pub fn state_of(&self, space: &str) -> Option<Arc<SpaceState>> {
        self.lock().spaces.get(space).map(|s| Arc::clone(&s.state))
    }

    /// The keys this server publishes at `/.well-known/han`.
    pub fn server_keys(&self) -> Vec<ServerKey> {
        self.lock().server_keys.clone()
    }

    /// Whether this home is in test mode (G-11).
    pub fn is_test_mode(&self) -> bool {
        self.test_clock.is_some()
    }

    /// The home's clock, as receipts see it.
    pub fn now_secs(&self) -> i64 {
        i64::try_from(self.clock.now_ms() / 1000).unwrap_or(i64::MAX)
    }

    /// The current time as a HAN Time.
    pub fn now_str(&self) -> String {
        format_time(self.now_secs())
    }

    /// The steady reading in milliseconds.
    pub(crate) fn steady_ms(&self) -> u64 {
        self.clock.steady_ms()
    }

    /// The spaces served, for the peer filter (N-02).
    pub fn admit_spaces(&self) -> Arc<AdmitSpaces> {
        Arc::clone(&self.lock().admit)
    }

    /// The node ids that vouch for `actor` (N-01): its valid keys, or for a server actor the
    /// `sequencer` nodes of its server.
    pub fn nodes_for(&self, actor: &str) -> Vec<String> {
        let g = self.lock();
        self.nodes_for_locked(&g, actor)
    }

    pub(crate) fn nodes_for_locked(&self, g: &Inner, actor: &str) -> Vec<String> {
        match actor.strip_prefix("@:") {
            Some(s) if s == self.server => g.own_nodes.clone(),
            Some(s) => g.directory.server(s).map(|i| i.nodes).unwrap_or_default(),
            None => self
                .profile_view(g, actor, &self.now_str())
                .map(|v| v.valid_nodes())
                .unwrap_or_default(),
        }
    }

    // ───────────────────────── identity ─────────────────────────

    /// Identity.profile: the view of `actor` now, or `None` if this home cannot produce it.
    pub(crate) fn profile_view(&self, g: &Inner, actor: &str, now: &str) -> Option<PinView> {
        if let Some(server) = actor.strip_prefix("@:") {
            let keys = if server == self.server {
                g.server_keys.clone()
            } else {
                g.directory.server(server)?.keys
            };
            return Some(PinView {
                seq: 0,
                actor_kind: han_core::policy::ActorKind::Service,
                operator: None,
                operator_confirmed: false,
                keys: keys
                    .into_iter()
                    .filter(|k| k.covers(now) && k.compromised != Some(true))
                    .map(|k| {
                        (
                            k.key_id,
                            PinKey {
                                public_key: k.public_key,
                                status: han_core::policy::KeyStatus::Valid,
                            },
                        )
                    })
                    .collect(),
            });
        }
        if grammar::server_of(actor) != Some(self.server.as_str()) {
            return None;
        }
        let space = g.spaces.get(&format!("profile:{actor}"))?;
        let profile = space.state.profile.as_ref()?;
        Some(PinView::from_profile(space.state.head, profile))
    }

    /// P-01: calls `relevantActors` with the views fetched so far, fetches what it names that was not tried,
    /// and calls again until it names nothing new (three calls at most). The last list is what the receipt
    /// pins; `None` if a profile on it cannot be had now.
    fn pin(
        &self,
        g: &Inner,
        state: Option<&SpaceState>,
        pe: &PolicyEntry,
        now: &str,
    ) -> Result<Option<BTreeMap<ActorId, PinView>>> {
        let mut fetched: BTreeMap<ActorId, PinView> = BTreeMap::new();
        let mut tried: HashSet<ActorId> = HashSet::new();
        let mut need = Vec::new();
        for _ in 0..3 {
            let ctx = Ctx::new(now, policy_profiles(&fetched))?;
            need = relevant_actors(state, pe, &ctx);
            let new: Vec<ActorId> = need
                .iter()
                .filter(|a| !tried.contains(*a))
                .cloned()
                .collect();
            if new.is_empty() {
                break;
            }
            for a in new {
                if let Some(v) = self.profile_view(g, &a, now) {
                    fetched.insert(a.clone(), v);
                }
                tried.insert(a);
            }
        }
        let mut pinned = BTreeMap::new();
        for a in need {
            match fetched.remove(&a) {
                Some(v) => {
                    pinned.insert(a, v);
                }
                None => return Ok(None),
            }
        }
        Ok(Some(pinned))
    }

    // ───────────────────────── routing ─────────────────────────

    /// Section 4.1 step 2: `moved` for a space this server does not host.
    pub(crate) fn route(&self, g: &Inner, space: &str) -> Result<()> {
        if !grammar::is_space_id(space) {
            return Ok(());
        }
        if let Some(to) = g.moved.get(space) {
            return Err(WireError::moved(to.clone()));
        }
        let named = grammar::server_of(space).unwrap_or_default();
        if g.spaces.contains_key(space) || named == self.server {
            return Ok(());
        }
        Err(WireError::moved(named))
    }

    // ───────────────────────── append (section 4.1) ─────────────────────────

    /// `append`: the pipeline, in the one order of verdicts. `content` absent and `null` are the same.
    pub fn append(&self, entry: &Value, content: Option<&Value>) -> Result<Receipt> {
        let mut g = self.lock();
        self.append_locked(&mut g, entry, content)
    }

    fn append_locked(
        &self,
        g: &mut Inner,
        entry_v: &Value,
        content: Option<&Value>,
    ) -> Result<Receipt> {
        if g.broken {
            return Err(WireError::unavailable("storage failure; restart"));
        }
        let content = content.filter(|c| **c != Value::Null);
        let eid = crate::build::entry_id(entry_v)?;
        let space_id = entry_v.get("space").and_then(Value::as_str);
        // 1 dedupe (R-01), before any other check
        if let Some(sp) = space_id.and_then(|s| g.spaces.get(s))
            && let Some(seq) = sp.ids.get(&eid)
        {
            return Ok(sp.records[usize::try_from(seq - 1).unwrap_or(0)]
                .receipt
                .clone());
        }
        // 2 not home
        if let Some(sp) = space_id {
            self.route(g, sp)?;
        }
        // 3 shape
        let entry: Entry = json::from_value(entry_v).map_err(WireError::from_json)?;
        entry.validate()?;
        entry.validate_content(content)?;
        if entry.kind == "space.move" {
            self.check_acceptance(g, &entry)?;
        }
        let pe = PolicyEntry::from_entry(&entry)?;
        // 4 hosting rule (step 2's "the server's own hosting rules"), before any profile is looked up or
        // fetched, so a stranger costs nothing; then profile and signature
        let hosted_create = entry.kind == "space.create"
            && !entry.space.starts_with("profile:")
            && grammar::server_of(&entry.space) == Some(self.server.as_str())
            && !g.spaces.contains_key(&entry.space);
        if hosted_create && !self.hosting_allows(g, &entry.author) {
            return Err(WireError::denied(HOSTING_DENIED));
        }
        let now_secs = self.now_secs();
        let now = format_time(now_secs); // read once: the pin loop, Policy's ctx.time and the receipt time
        let Some(author) = self.profile_view(g, &entry.author, &now) else {
            if hosted_create {
                self.charge_fetch(g)?; // the carrier fetches on this answer: bounded
            }
            return Err(WireError::unavailable("profile"));
        };
        if !author.keys.contains_key(&entry.key_id) {
            return Err(WireError::denied("no valid key")); // R-12: nothing to verify against
        }
        let public = author
            .public_key(&entry.key_id)
            .ok_or_else(|| WireError::invalid("signature"))?;
        entry
            .verify(&public)
            .map_err(|_| WireError::invalid("signature"))?;
        let state = g.spaces.get(&entry.space).map(|s| &*s.state);
        let pinned = self
            .pin(g, state, &pe, &now)?
            .ok_or_else(|| WireError::unavailable("profile"))?;
        let ctx = Ctx::new(&now, policy_profiles(&pinned))?;
        // 5 Policy; an owner's early abandon counts as allowed here and is refused in step 7 (R-16)
        let wait_ms = self.abandon_wait_ms(g, state, &pe, &entry, &pinned, now_secs)?;
        if wait_ms == 0 {
            match decide(state, &pe, &ctx) {
                Verdict::Allow => {}
                Verdict::Denied(r) => return Err(WireError::denied(r)),
                Verdict::Invalid(r) => return Err(WireError::invalid(r)),
            }
        }
        // 6 rate limits
        if let Some(limit) = &g.limiter
            && let Some(ms) = limit.check(&entry.author)
        {
            return Err(WireError::limit("rate limit", ms));
        }
        // 7 the moment-dependent `unavailable`
        if wait_ms > 0 {
            return Err(
                WireError::unavailable("abandon before the launch window closed (R-16)")
                    .retry_after(wait_ms),
            );
        }
        let last = g.spaces.get(&entry.space).and_then(|s| s.records.last());
        if let Some(prev) = last {
            let last_secs = parse_time(&prev.receipt.time).unwrap_or(0);
            if now_secs < last_secs {
                let ms = u64::try_from((last_secs - now_secs + 1) * 1000).unwrap_or(1000);
                return Err(
                    WireError::unavailable("clock behind the chain; wait (R-03a)")
                        .retry_after(ms.max(1000)),
                );
            }
        }
        let wanted = match state {
            Some(s) => s.policy.min_durability,
            None => match &pe.body {
                Body::SpaceCreate(b) => b.policy.min_durability,
                _ => Durability::Local,
            },
        };
        let rescue = matches!(pe.body, Body::PolicySet(_) | Body::SpaceMove(_));
        if wanted == Durability::Replicated && self.durability != Durability::Replicated && !rescue
        {
            return Err(WireError::unavailable("minDurability not reachable"));
        }
        let requested = match &pe.body {
            Body::PolicySet(b) => Some(b.policy.min_durability),
            Body::SpaceCreate(b) => Some(b.policy.min_durability),
            _ => None,
        };
        if requested == Some(Durability::Replicated) && self.durability != Durability::Replicated {
            return Err(WireError::unavailable(
                "this home can't meet the requested minDurability",
            ));
        }
        // 8 assign seq, prev, time, durability, profiles; sign
        let seq = last.map_or(1, |p| p.receipt.seq + 1);
        let mut receipt = Receipt {
            space: entry.space.clone(),
            seq,
            id: eid.clone(),
            prev: match last {
                Some(p) => p.receipt.hash()?,
                None => String::new(),
            },
            home: self.server.clone(),
            home_key: self.key_id.clone(),
            time: now.clone(),
            durability: self.durability,
            profiles: pinned.iter().map(|(a, v)| (a.clone(), v.seq)).collect(),
            home_sig: String::new(),
        };
        receipt.sign(&self.key)?;
        let record = LogRecord {
            redacted: false,
            redacted_by: None,
            entry: entry.clone(),
            content: content.cloned(),
            receipt: receipt.clone(),
        };
        let folded = fold(state, &Record::from_log_record(&record)?, &ctx)?;
        // 9 durable at the stated level, then the state
        let stored = Stored {
            record: record.clone(),
            pins: pinned.clone(),
        };
        if let Err(e) = self.store.append(&entry.space, &stored) {
            g.broken = true; // memory must not run ahead of disk
            tracing::error!("storage failure: {e}");
            return Err(WireError::unavailable("storage failure; restart"));
        }
        self.install(g, record, pinned, folded);
        Ok(receipt)
    }

    /// Installs an ordered record: state, indexes, fan-out, notices, hooks (section 4.1 steps 9 and 10).
    fn install(
        &self,
        g: &mut Inner,
        record: LogRecord,
        pins: BTreeMap<ActorId, PinView>,
        state: SpaceState,
    ) {
        let space_id = record.entry.space.clone();
        let seq = record.receipt.seq;
        let entry = record.entry.clone();
        let state_now = Arc::new(state);
        let sp = g.spaces.entry(space_id.clone()).or_insert_with(|| Space {
            records: Vec::new(),
            pins: Vec::new(),
            state: Arc::clone(&state_now),
            order: MapOrder::default(),
            ids: HashMap::new(),
        });
        sp.order.advance(
            if sp.records.is_empty() {
                None
            } else {
                Some(&sp.state)
            },
            &state_now,
        );
        sp.state = Arc::clone(&state_now);
        sp.ids.insert(record.receipt.id.clone(), seq);
        sp.pins.push(pins);
        sp.records.push(record.clone());
        g.admit_update(&space_id, &state_now);
        if entry.kind == "effect.claim" {
            let steady = self.steady_ms();
            g.claim_steady.insert(record.receipt.id.clone(), steady);
        }
        if entry.kind == "space.move"
            && let Some(to) = entry.body.get("to").and_then(Value::as_str)
        {
            g.moved.insert(space_id.clone(), to.to_owned());
        }
        self.supply_from_record(g, &record);
        if entry.kind == "redact"
            && let Some(target) = entry.body.get("target").and_then(Value::as_str)
        {
            self.redact(g, &space_id, target, &record);
        }
        let item = json::to_value(&record)
            .map(|rec| jv::obj([("type", jv::s("record")), ("record", rec)]))
            .ok();
        if let Some(item) = item {
            let subs = g.subs.by_space.entry(space_id.clone()).or_default();
            subs.retain(|sub| {
                if seq <= sub.cutoff || !can_read(&state_now, &sub.actor, seq) {
                    return true;
                }
                sub.push(item.clone())
            });
        }
        self.notify(g, &record.entry, &space_id, seq, &state_now);
        g.hooks.ordered(&record, &state_now);
        if matches!(
            entry.kind.as_str(),
            "key.add" | "key.revoke" | "member.set" | "policy.set" | "space.move"
        ) {
            self.changes.send_modify(|n| *n += 1);
        }
    }

    /// R-04: the content of a redacted record is erased, and the record carries the `redact` entry as proof.
    fn redact(&self, g: &mut Inner, space: &str, target: &str, by: &LogRecord) {
        let Some(sp) = g.spaces.get_mut(space) else {
            return;
        };
        let Some(seq) = sp.ids.get(target).copied() else {
            return;
        };
        let idx = usize::try_from(seq - 1).unwrap_or(0);
        let proof = han_core::types::RedactedBy {
            entry: by.entry.clone(),
            receipt: by.receipt.clone(),
        };
        let rec = &mut sp.records[idx];
        rec.content = None;
        rec.redacted = true;
        rec.redacted_by = Some(proof);
        let stored = Stored {
            record: rec.clone(),
            pins: sp.pins[idx].clone(),
        };
        if let Err(e) = self.store.replace(space, &stored) {
            g.broken = true;
            tracing::error!("storage failure: {e}");
        }
    }

    /// G-03: tells the server of every actor given a role, or named in `to` while holding one.
    fn notify(&self, g: &mut Inner, entry: &Entry, space: &str, seq: u64, state: &SpaceState) {
        let reads = |a: &str| state.roles.get(a).is_some_and(|r| r.reads());
        let mut named: Vec<String> = Vec::new();
        if entry.kind == "member.set"
            && let (Some(actor), Some(role)) = (
                entry.body.get("actor").and_then(Value::as_str),
                entry.body.get("role").and_then(Value::as_str),
            )
            && matches!(role, "owner" | "member" | "guest")
        {
            named.push(actor.to_owned());
        }
        if let Some(to) = entry.body.get("to").and_then(Value::as_array) {
            named.extend(
                to.iter()
                    .filter_map(Value::as_str)
                    .filter(|a| reads(a))
                    .map(str::to_owned),
            );
        }
        let mut seen = HashSet::new();
        for actor in named {
            if !seen.insert(actor.clone()) {
                continue;
            }
            let Some(server) = grammar::server_of(&actor) else {
                continue;
            };
            if server == self.server {
                let item = jv::obj([
                    ("type", jv::s("signal")),
                    ("from", jv::s(&format!("@:{}", self.server))),
                    (
                        "body",
                        jv::obj([
                            ("type", jv::s("notice")),
                            ("space", jv::s(space)),
                            ("seq", jv::int(seq)),
                        ]),
                    ),
                ]);
                if let Some(subs) = g.subs.notices.get_mut(&actor) {
                    subs.retain(|sub| sub.push(item.clone()));
                }
            } else {
                g.hooks.notice(server, &actor, space, seq);
            }
        }
    }

    /// §7.3: a `space.move`'s acceptance must verify under the target's published keys.
    fn check_acceptance(&self, g: &Inner, entry: &Entry) -> Result<()> {
        let bad = || WireError::invalid("no valid acceptance from the target server");
        let to = entry
            .body
            .get("to")
            .and_then(Value::as_str)
            .ok_or_else(bad)?;
        let accept = entry.body.get("accept").ok_or_else(bad)?;
        let (Some(key_id), Some(sig)) = (
            accept.get("keyId").and_then(Value::as_str),
            accept.get("sig").and_then(Value::as_str),
        ) else {
            return Err(bad());
        };
        let head = g
            .spaces
            .get(&entry.space)
            .and_then(|s| s.records.last())
            .ok_or_else(bad)?;
        let info = g.directory.server(to).ok_or_else(bad)?;
        let now = self.now_str();
        let key = info
            .keys
            .iter()
            .find(|k| k.key_id == key_id && k.covers(&now) && k.compromised != Some(true))
            .and_then(|k| PublicKey::from_base64url(&k.public_key).ok())
            .ok_or_else(bad)?;
        han_core::sign::verify_accept_move(&key, sig, &entry.space, &head.receipt)
            .map_err(|_| bad())
    }

    /// R-16, P-09: the milliseconds an owner's abandon must still wait. Non-zero only if Policy would allow
    /// the entry at claim time + `A` (the latest claim, phase `claimed`, the right key and author): then it
    /// is early, by this home's steady clock (`A` since it ordered the claim, or since this run began) or by
    /// the receipt time it would get. Anything else gets Policy's own verdict now.
    fn abandon_wait_ms(
        &self,
        g: &Inner,
        state: Option<&SpaceState>,
        pe: &PolicyEntry,
        entry: &Entry,
        pinned: &BTreeMap<ActorId, PinView>,
        now_secs: i64,
    ) -> Result<u64> {
        let (Some(state), Body::Outcome(b)) = (state, &pe.body) else {
            return Ok(0);
        };
        let Some((eff, claim)) = find_claim(state, &b.claim) else {
            return Ok(0);
        };
        if entry.author == eff.executor && entry.key_id == claim.instance {
            return Ok(0); // the executor's own report
        }
        let a = abandon_after(claim.window);
        let due = parse_time(&claim.time).unwrap_or(0) + i64::try_from(a).unwrap_or(0);
        let ctx = Ctx::new(&format_time(due), policy_profiles(pinned))?;
        if !decide(Some(state), pe, &ctx).is_allow() {
            return Ok(0);
        }
        let started = g
            .claim_steady
            .get(&claim.id)
            .copied()
            .unwrap_or(g.run_start);
        let elapsed = self.steady_ms().saturating_sub(started);
        let steady_wait = (a * 1000).saturating_sub(elapsed);
        let time_wait = u64::try_from((due - now_secs) * 1000).unwrap_or(0);
        Ok(steady_wait.max(time_wait))
    }

    /// Creates `profile:<actor>` (the server actor appends it, section 4.3). Idempotent: an existing profile
    /// returns the receipt of its first record. An agent's or service's profile names its operator.
    pub fn register(
        &self,
        actor: &str,
        kind: &str,
        key_id: &str,
        public_key: &str,
        operator: Option<&str>,
    ) -> Result<Receipt> {
        let space = format!("profile:{actor}");
        if kind != "human" && operator.is_none() {
            return Err(WireError::invalid(format!(
                "a {kind} profile names its operator: {actor}"
            )));
        }
        {
            let g = self.lock();
            if let Some(s) = g.spaces.get(&space) {
                return Ok(s.records[0].receipt.clone());
            }
        }
        let mut key = std::collections::BTreeMap::new();
        key.insert("keyId".to_owned(), jv::s(key_id));
        key.insert("publicKey".to_owned(), jv::s(public_key));
        let mut profile = std::collections::BTreeMap::new();
        profile.insert("actorKind".to_owned(), jv::s(kind));
        profile.insert("key".to_owned(), Value::Object(key));
        if let Some(op) = operator {
            profile.insert("operator".to_owned(), jv::s(op));
        }
        let body = jv::obj([
            ("policy", profile_policy()),
            ("profile", Value::Object(profile)),
        ]);
        let (entry, content) = crate::build::sign_entry(
            crate::build::EntrySpec {
                space,
                kind: "space.create".into(),
                author: format!("@:{}", self.server),
                key_id: self.key_id.clone(),
                body,
                content: Some(jv::obj([("name", jv::s(actor))])),
                refs: vec![],
                seen: 0,
                time_secs: self.now_secs(),
            },
            &self.key,
        )?;
        self.append(&entry, content.as_ref())
    }
}

/// The claim with id `claim` and the effect it belongs to.
fn find_claim<'a>(
    state: &'a SpaceState,
    claim: &str,
) -> Option<(&'a Effect, &'a han_core::policy::ClaimState)> {
    state
        .effects
        .values()
        .find_map(|e| e.claims.iter().find(|c| c.id == claim).map(|c| (e, c)))
}

impl Inner {
    pub(crate) fn admit_update(&mut self, space: &str, state: &SpaceState) {
        Arc::make_mut(&mut self.admit).insert(space.to_owned(), AdmitSpace::from(state));
    }
}

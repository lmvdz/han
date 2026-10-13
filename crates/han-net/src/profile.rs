//! Remote profiles (section 4.1 step 4, section 4.3): reading, verifying and pinning the profile space of an
//! actor of another server, so that a home can judge entries whose author or approvers live elsewhere.
//!
//! [`ProfileDirectory`] is the [`Directory`] a federating home is given. For `@x:other.example` it
//!
//! 1. asks the home's [`Home::may_fetch_profile`] (the hosting rule and the fetch budget),
//! 2. reads `/.well-known/han` of `other.example`,
//! 3. dials one of its nodes as the server actor `@:S` with the `han/1` hello binding,
//! 4. reads `profile:@x:other.example` and checks the whole chain ([`verify_profile_chain`]): ids, `prev`
//!    links, every receipt under the server keys and key windows of the document, every entry signature
//!    under the key its profile had at that point,
//! 5. pins the view at the head it read (never lower than one pinned before, R-10) and serves it for 300 s,
//!    after which the home asks for a new read.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use han_core::json;
use han_core::policy::{Ctx, KeyStatus, Record, SpaceState, fold};
use han_core::{LogRecord, PublicKey, grammar};
use han_seq::verify::check_chain;
use han_seq::{Clock, Directory, Home, PinView, RemoteProfile, ServerInfo, ServerKey};

use crate::client::Client;
use crate::error::{NetError, Result};
use crate::transport::Transport;
use crate::wellknown::{CACHE, WellKnown, WellKnownDirectory};

/// Records asked of a profile space per `read`.
const PAGE: usize = 1000;
/// A profile space longer than this is refused: a key list that long is not a person's.
const MAX_RECORDS: usize = 20_000;

/// A boxed future, so the source of remote profiles can be a trait object.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a carrier asks of a federating home's profile reader, without knowing the transport behind it.
pub trait RemoteProfiles: Send + Sync + 'static {
    /// Makes sure a fresh view of `actor` (of another server) is held: nothing to do if it is, a read otherwise.
    fn ensure<'a>(&'a self, actor: &'a str) -> BoxFuture<'a, Result<()>>;

    /// Reads `actor`'s profile again whatever the cache holds (N-02: a connected device's key is re-checked).
    fn refresh<'a>(&'a self, actor: &'a str) -> BoxFuture<'a, Result<()>>;
}

/// The key of a record's signature, or why there is none.
fn signing_key(
    rec: &LogRecord,
    actor: &str,
    server_actor: &str,
    keys: &[ServerKey],
    state: Option<&SpaceState>,
) -> std::result::Result<Option<PublicKey>, String> {
    let (e, time) = (&rec.entry, rec.receipt.time.as_str());
    if e.author == server_actor {
        // an entry of `@:S` is signed by a server key whose window holds the receipt time (section 4.3)
        let key = keys
            .iter()
            .find(|k| k.key_id == e.key_id && k.covers(time))
            .ok_or("no published server key signs this entry")?;
        return PublicKey::from_base64url(&key.public_key)
            .map(Some)
            .map_err(|_| "a server key that is not a key".to_owned());
    }
    let profile = state
        .and_then(|s| s.profile.as_ref())
        .ok_or("an entry before the profile exists")?;
    if e.author == actor {
        let key = profile
            .keys
            .get(&e.key_id)
            .ok_or("signed with a key the profile does not have")?;
        if key.status != KeyStatus::Valid {
            return Err("signed with a key that was revoked (R-12)".into());
        }
        return PublicKey::from_base64url(&key.public_key)
            .map(Some)
            .map_err(|_| "a profile key that is not a key".to_owned());
    }
    if e.kind == "member.set" && profile.operator.as_deref() == Some(e.author.as_str()) {
        // the operator's confirmation: its key lives in the operator's own profile, which this chain does
        // not carry. It cannot change a key, so it is not checked here.
        return Ok(None);
    }
    Err(format!("an entry by {}", e.author))
}

/// Checks a read of `profile:<actor>` against the server keys of the actor's home and returns the view at its
/// head (section 4.3, R-14 as far as a profile needs it). `records` are all of the space from seq 1.
///
/// The receipts must be the home's own, signed by a key whose window holds the receipt time (a key marked
/// compromised signs nothing here); the entries must be for this space, signed by `@:S` or by a key the
/// profile held and that was valid then; and the fold must succeed. Policy's other rules are not replayed.
pub fn verify_profile_chain(
    actor: &str,
    keys: &[ServerKey],
    records: &[LogRecord],
) -> std::result::Result<PinView, String> {
    let server = grammar::server_of(actor).ok_or("not an actor")?;
    let server_actor = format!("@:{server}");
    let space = format!("profile:{actor}");
    let first = records.first().ok_or("the profile space is empty")?;
    if first.receipt.seq != 1 {
        return Err("the chain does not start at seq 1".into());
    }
    let trusted: Vec<ServerKey> = keys
        .iter()
        .filter(|k| k.compromised != Some(true))
        .cloned()
        .collect();
    check_chain(records, &trusted, "")?;
    let mut state: Option<SpaceState> = None;
    for rec in records {
        let at = |why: &str| format!("seq {}: {why}", rec.receipt.seq);
        let e = &rec.entry;
        if e.space != space || rec.receipt.space != space {
            return Err(at("a record of another space"));
        }
        if rec.receipt.home != server {
            return Err(at("a receipt of another home"));
        }
        let creating = state.is_none();
        if creating != (e.kind == "space.create") {
            return Err(at("the profile space is created exactly once, first"));
        }
        if creating && e.author != server_actor {
            return Err(at("the profile is not created by its server"));
        }
        if !matches!(
            e.kind.as_str(),
            "space.create" | "key.add" | "key.revoke" | "member.set"
        ) {
            return Err(at("a kind a profile space does not take"));
        }
        let author = state.as_ref().and_then(|s| {
            s.profile
                .as_ref()
                .map(|p| (e.author == actor).then(|| PinView::from_profile(s.head, p)))
        });
        if let Some(public) = signing_key(rec, actor, &server_actor, &trusted, state.as_ref())
            .map_err(|why| at(&why))?
        {
            e.verify(&public)
                .map_err(|_| at("the signature is forged"))?;
        }
        let profiles = match author.flatten() {
            Some(v) => BTreeMap::from([(actor.to_owned(), v.for_policy())]),
            None => BTreeMap::new(),
        };
        let ctx = Ctx::new(&rec.receipt.time, profiles).map_err(|e| at(&e.to_string()))?;
        let record = Record::from_log_record(rec).map_err(|e| at(&e.to_string()))?;
        state = Some(fold(state.as_ref(), &record, &ctx).map_err(|e| at(&e.to_string()))?);
    }
    let state = state.ok_or("the profile space is empty")?;
    let profile = state.profile.as_ref().ok_or("no profile")?;
    Ok(PinView::from_profile(state.head, profile))
}

/// A view and when it was read (a reading of the home's steady clock).
struct Pinned {
    view: PinView,
    read_at_ms: Option<u64>,
}

struct Shared<T> {
    wk: WellKnownDirectory,
    transport: T,
    clock: Arc<dyn Clock>,
    home: OnceLock<Weak<Home>>,
    pinned: Mutex<HashMap<String, Pinned>>,
    /// One read of an actor at a time; the others wait for it and use what it found.
    reading: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// The [`Directory`] of a federating home: other servers' nodes and keys from their well-known documents, and
/// their actors' profiles read from their homes over `transport`. Cloning shares the cache.
pub struct ProfileDirectory<T: Transport> {
    shared: Arc<Shared<T>>,
}

impl<T: Transport> Clone for ProfileDirectory<T> {
    fn clone(&self) -> Self {
        ProfileDirectory {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T: Transport> std::fmt::Debug for ProfileDirectory<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProfileDirectory")
    }
}

fn lock<V>(m: &Mutex<V>) -> std::sync::MutexGuard<'_, V> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl<T: Transport> ProfileDirectory<T> {
    /// A directory that reads documents with `wk` and dials with `transport` (a node of this server: its
    /// id must be in this server's `sequencer` list, since the profile home binds the hello to it). `clock`
    /// is the home's, so a profile is fresh for 300 s of the time the home sees.
    ///
    /// Give it to the home with [`Home::set_directory`] and bind it with [`ProfileDirectory::bind`].
    pub fn new(wk: WellKnownDirectory, transport: T, clock: Arc<dyn Clock>) -> Self {
        ProfileDirectory {
            shared: Arc::new(Shared {
                wk,
                transport,
                clock,
                home: OnceLock::new(),
                pinned: Mutex::default(),
                reading: Mutex::default(),
            }),
        }
    }

    /// Says which home this directory reads for: it is asked [`Home::may_fetch_profile`] before every read
    /// and signs the hello as its server actor. Held weakly, so the directory the home owns does not keep it
    /// alive. A second call has no effect.
    pub fn bind(&self, home: &Arc<Home>) {
        let _ = self.shared.home.set(Arc::downgrade(home));
    }

    /// The well-known directory underneath (refresh a server's document here).
    pub fn wellknown(&self) -> &WellKnownDirectory {
        &self.shared.wk
    }

    /// Forgets that `actor`'s view is fresh (a notice of a revocation, say): the next use reads it again. The
    /// seq pinned so far stays as the floor (R-10).
    pub fn invalidate(&self, actor: &str) {
        if let Some(p) = lock(&self.shared.pinned).get_mut(actor) {
            p.read_at_ms = None;
        }
    }

    fn fresh(&self, actor: &str) -> bool {
        let now = self.shared.clock.steady_ms();
        lock(&self.shared.pinned)
            .get(actor)
            .and_then(|p| p.read_at_ms)
            .is_some_and(|at| {
                now.saturating_sub(at) < u64::try_from(CACHE.as_millis()).unwrap_or(u64::MAX)
            })
    }

    /// The view of `actor` held now, fresh or not.
    pub fn pinned(&self, actor: &str) -> Option<PinView> {
        lock(&self.shared.pinned).get(actor).map(|p| p.view.clone())
    }

    /// A fresh view of `actor`: the cached one within 300 s, otherwise a read from its home.
    pub async fn ensure_profile(&self, actor: &str) -> Result<PinView> {
        self.read(actor, false, None).await
    }

    /// A read from the home of `actor` whatever the cache holds.
    pub async fn refresh_profile(&self, actor: &str) -> Result<PinView> {
        self.read(actor, true, None).await
    }

    async fn read(&self, actor: &str, force: bool, requester: Option<&str>) -> Result<PinView> {
        if !grammar::is_actor_id(actor) || actor.starts_with("@:") {
            return Err(NetError::Protocol(format!("not a user or agent: {actor}")));
        }
        let gate = Arc::clone(
            lock(&self.shared.reading)
                .entry(actor.to_owned())
                .or_default(),
        );
        let _turn = gate.lock().await;
        let out = self.read_locked(actor, force, requester).await;
        drop(_turn);
        let mut reading = lock(&self.shared.reading);
        if reading
            .get(actor)
            .is_some_and(|g| Arc::strong_count(g) <= 2)
        {
            reading.remove(actor);
        }
        out
    }

    async fn read_locked(
        &self,
        actor: &str,
        force: bool,
        requester: Option<&str>,
    ) -> Result<PinView> {
        if !force
            && self.fresh(actor)
            && let Some(v) = self.pinned(actor)
        {
            return Ok(v);
        }
        let home = self
            .shared
            .home
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| NetError::Closed("the home is gone".into()))?;
        // before every fetch: the hosting rule and the budget
        home.may_fetch_profile(actor, requester)?;
        let server = grammar::server_of(actor).unwrap_or_default();
        if server == home.server() {
            return Err(NetError::Protocol("a profile of this server".into()));
        }
        let doc = self.shared.wk.refresh(server).await?;
        let mut last = NetError::Dial(format!("{server} lists no node"));
        for addr in &doc.sequencer {
            match self.read_from(&home, &doc, addr, actor).await {
                Ok(view) => return self.pin(actor, view),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    async fn read_from(
        &self,
        home: &Home,
        doc: &WellKnown,
        addr: &crate::addr::NodeAddr,
        actor: &str,
    ) -> Result<PinView> {
        let client =
            Client::connect(&self.shared.transport, addr, &doc.server, &doc.nodes()).await?;
        let out = async {
            client.hello_as_server(home).await?;
            let space = format!("profile:{actor}");
            let mut records: Vec<LogRecord> = Vec::new();
            loop {
                let after = u64::try_from(records.len()).unwrap_or(u64::MAX);
                let items = client.read(&space, after).await?;
                let mut got = 0;
                for item in items {
                    if item["type"].as_str() != Some("record") {
                        continue;
                    }
                    let rec: LogRecord = json::from_value(&item["record"])
                        .map_err(|e| NetError::Protocol(format!("profile record: {e}")))?;
                    records.push(rec);
                    got += 1;
                }
                if got < PAGE || records.len() > MAX_RECORDS {
                    break;
                }
            }
            if records.len() > MAX_RECORDS {
                return Err(NetError::Protocol("profile chain: too long".into()));
            }
            verify_profile_chain(actor, &doc.keys, &records)
                .map_err(|why| NetError::Protocol(format!("profile chain of {actor}: {why}")))
        }
        .await;
        client.close();
        out
    }

    /// R-10: the seq pinned for an actor never goes down, and a seq pinned twice is the same view.
    fn pin(&self, actor: &str, view: PinView) -> Result<PinView> {
        let mut pinned = lock(&self.shared.pinned);
        if let Some(old) = pinned.get(actor) {
            if view.seq < old.view.seq {
                return Err(NetError::Protocol(format!(
                    "profile of {actor} went back from seq {} to {} (R-10)",
                    old.view.seq, view.seq
                )));
            }
            if view.seq == old.view.seq && view != old.view {
                return Err(NetError::Protocol(format!(
                    "profile of {actor} differs at seq {}",
                    view.seq
                )));
            }
        }
        pinned.insert(
            actor.to_owned(),
            Pinned {
                view: view.clone(),
                read_at_ms: Some(self.shared.clock.steady_ms()),
            },
        );
        Ok(view)
    }
}

impl<T: Transport> Directory for ProfileDirectory<T> {
    fn server(&self, server: &str) -> Option<ServerInfo> {
        self.shared.wk.server(server)
    }

    fn profile(&self, actor: &str) -> Option<RemoteProfile> {
        let fresh = self.fresh(actor);
        self.pinned(actor).map(|view| RemoteProfile { view, fresh })
    }

    fn want_profile(&self, actor: &str, requester: Option<&str>) {
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (dir, actor) = (self.clone(), actor.to_owned());
        let requester = requester.map(str::to_owned);
        rt.spawn(async move {
            if let Err(e) = dir.read(&actor, false, requester.as_deref()).await {
                tracing::debug!("profile of {actor} not read: {e}");
            }
        });
    }
}

impl<T: Transport> RemoteProfiles for ProfileDirectory<T> {
    fn ensure<'a>(&'a self, actor: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { self.ensure_profile(actor).await.map(|_| ()) })
    }

    fn refresh<'a>(&'a self, actor: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { self.refresh_profile(actor).await.map(|_| ()) })
    }
}

/// Whether `actor` is a user or agent of a server other than `home`'s.
pub(crate) fn is_remote_actor(home: &Home, actor: &str) -> bool {
    !actor.starts_with("@:") && grammar::server_of(actor).is_some_and(|s| s != home.server())
}

/// The actor a `hello` or `append` request frame is for, if it is a remote user or agent: the carrier reads
/// its profile before the home handles the call, so the answer is not `unavailable`.
pub(crate) fn remote_subject(home: &Home, frame: &str) -> Option<String> {
    let v = json::parse(frame.as_bytes()).ok()?;
    let who = match v["call"].as_str()? {
        "hello" => v["args"]["who"].as_str()?,
        "append" => v["args"]["entry"]["author"].as_str()?,
        _ => return None,
    };
    is_remote_actor(home, who).then(|| who.to_owned())
}

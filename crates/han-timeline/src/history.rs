//! A history: integration of untrusted envelopes (§14), materialisation (§10), timelines and
//! checkpoints (§4), and local writes.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::envelope::{self, Compression, Envelope, Signer};
use crate::error::{Error, Reason, Report, Verdict};
use crate::graph::{Closure, Graph, Node, Pos};
use crate::ids::{BatchId, HistoryId, MAX_SAFE, OpId, ReplicaId, Version};
use crate::op::{Batch, Change, HistoryKind, Op};
use crate::state::{Seen, State};
use crate::store::{Directory, FORMAT_VERSION, MemoryStore, Scope, Store};

/// How many held-back batches a history keeps by default (TL-102).
pub const DEFAULT_HOLD_LIMIT: usize = 1024;
/// How many quarantined batches a history keeps by default (TL-103).
pub const DEFAULT_QUARANTINE_LIMIT: usize = 1024;
const CACHE_SIZE: usize = 32;

/// Two validly signed envelopes for one op id: the integrated one and the equivocating one
/// (TL-104).
pub type FaultProof = (Arc<[u8]>, Arc<[u8]>);

/// A batch that passed the envelope checks and waits for integration.
#[derive(Clone, Debug)]
pub(crate) struct Pending {
    pub id: BatchId,
    pub envelope: Arc<[u8]>,
    pub raw: Arc<[u8]>,
    pub batch: Batch,
    /// The envelope carried an empty `history` (a genesis envelope).
    pub empty_history: bool,
    /// Whether the signing key is `valid` in the actor's profile.
    pub key_valid: bool,
    /// TL-103; `None` until the history's kind is known.
    pub auth: Option<bool>,
}

struct CacheEntry {
    heads: Vec<Pos>,
    closure: Closure,
    /// The number of ops in the closure.
    size: u64,
    state: Arc<State>,
    used: u64,
}

#[derive(Default)]
struct Cache {
    entries: Vec<CacheEntry>,
    tick: u64,
}

/// Where to look: a timeline's head or a version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum At {
    /// The head of the timeline with this id.
    Timeline(OpId),
    /// A version (any set of held op ids; the frontier of its closure is used, TL-13).
    Version(Version),
}

/// A timeline (§4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineInfo {
    /// Its id: the op id of its create op.
    pub id: OpId,
    /// Its name.
    pub name: String,
    /// The timeline it forked, `None` for main.
    pub source: Option<OpId>,
    /// Its head.
    pub head: Version,
}

/// A checkpoint (TL-25).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    /// The checkpoint op.
    pub id: OpId,
    /// Its label.
    pub label: String,
    /// The version it names: its parent version.
    pub version: Version,
}

/// What a local write produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Written {
    /// The signed envelope, to store and send to peers.
    pub envelope: Vec<u8>,
    /// The batch id.
    pub batch: BatchId,
    /// The op ids of the batch's ops, in order.
    pub ops: Vec<OpId>,
}

/// A failed check while integrating: the verdict, and an integrated batch that proves an
/// equivocation.
pub(crate) struct Fail {
    pub verdict: Verdict,
    pub fault: Option<usize>,
}

impl From<Reason> for Fail {
    fn from(r: Reason) -> Self {
        Fail {
            verdict: Verdict::Rejected(r),
            fault: None,
        }
    }
}

/// One history of one space: its integrated batches, the batches it holds back or keeps in
/// quarantine, and the derived timelines and states.
pub struct History<S: Store = MemoryStore> {
    store: S,
    space: String,
    id: HistoryId,
    pub(crate) graph: Graph,
    pub(crate) kind: Option<HistoryKind>,
    session: Option<String>,
    integrated: HashSet<BatchId>,
    held: BTreeMap<BatchId, Pending>,
    quarantine: BTreeMap<BatchId, Pending>,
    /// Held-back batches waiting for an op, by replica and counter (one missing op each).
    waits: HashMap<Arc<ReplicaId>, BTreeMap<u64, Vec<BatchId>>>,
    /// Held-back batches whose dependencies are all held, in integration order.
    ready: BTreeSet<(u64, Arc<ReplicaId>, BatchId)>,
    faults: Vec<FaultProof>,
    max_counter: u64,
    hold_limit: usize,
    quarantine_limit: usize,
    cache: Mutex<Cache>,
}

impl<S: Store> std::fmt::Debug for History<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("History")
            .field("space", &self.space)
            .field("id", &self.id)
            .field("batches", &self.graph.len())
            .finish()
    }
}

fn store_err(e: String) -> Error {
    Error::Store(e)
}

impl<S: Store> History<S> {
    fn empty(store: S, space: &str, id: HistoryId) -> Self {
        History {
            store,
            space: space.to_string(),
            id,
            graph: Graph::default(),
            kind: None,
            session: None,
            integrated: HashSet::new(),
            held: BTreeMap::new(),
            quarantine: BTreeMap::new(),
            waits: HashMap::new(),
            ready: BTreeSet::new(),
            faults: Vec::new(),
            max_counter: 0,
            hold_limit: DEFAULT_HOLD_LIMIT,
            quarantine_limit: DEFAULT_QUARANTINE_LIMIT,
            cache: Mutex::new(Cache::default()),
        }
    }

    /// Creates a new history: writes its genesis batch, whose first op creates main with the
    /// history's kind (TL-15, TL-27). `session` is the `session.open` EntryId of a transcript
    /// history. `ops` are more ops for the genesis batch (on main). The history id is the
    /// genesis batch id (TL-10).
    pub fn create(
        mut store: S,
        space: &str,
        signer: &dyn Signer,
        lane: u32,
        kind: HistoryKind,
        session: Option<String>,
        ops: Vec<Op>,
    ) -> Result<(Self, Written), Error> {
        if store.format_version().map_err(store_err)?.is_some() {
            return Err(Error::Store("store is not empty".into()));
        }
        let replica = Arc::new(ReplicaId::new(signer.actor(), signer.key_id(), lane));
        let start = store
            .high_water(signer.actor(), signer.key_id())
            .map_err(store_err)?
            + 1;
        let mut all = vec![Op::TimelineCreate {
            name: "main".into(),
            source: None,
            history_kind: Some(kind),
            session,
        }];
        all.extend(ops);
        let batch = Batch {
            replica,
            start,
            parents: Version::root(),
            timeline: None,
            ops: all,
            time: None,
            step: None,
        };
        let (envelope, raw) = envelope::seal_raw(space, None, &batch, signer, Compression::Zstd);
        let env = Envelope::decode(&envelope).map_err(|e| Error::Rejected(e.into()))?;
        let id = env.received_id(&envelope);
        store
            .set_format_version(FORMAT_VERSION)
            .map_err(store_err)?;
        let mut history = History::empty(store, space, id);
        let written = history.commit_local(batch, envelope, raw)?;
        Ok((history, written))
    }

    /// Opens a history of `space` with id `id` over `store`, replaying what it holds. An empty
    /// store gives an empty history that takes up `id` from its peers (TL-101 step 2).
    pub fn open(store: S, space: &str, id: HistoryId) -> Result<Self, Error> {
        let version = store.format_version().map_err(store_err)?;
        if let Some(v) = version
            && v > FORMAT_VERSION
        {
            return Err(Error::Format(v));
        }
        let envelopes = store.envelopes().map_err(store_err)?;
        let mut history = History::empty(store, space, id);
        if version.is_none() {
            history
                .store
                .set_format_version(FORMAT_VERSION)
                .map_err(store_err)?;
        }
        for bytes in envelopes {
            // Stored data is re-checked except for signatures and roles, which were checked on
            // receipt and may have changed since (TL-103: integrated ops are never removed).
            let p = history
                .pending(&bytes, None)
                .map_err(|(_, v)| Error::Store(format!("stored envelope fails: {}", v.code())))?;
            history.integrate(p, false).map_err(|e| match e {
                Ok(v) => Error::Store(format!("stored envelope fails: {}", v.code())),
                Err(e) => e,
            })?;
        }
        Ok(history)
    }

    /// The history id.
    pub fn id(&self) -> HistoryId {
        self.id
    }

    /// The space.
    pub fn space(&self) -> &str {
        &self.space
    }

    /// The history kind, once the genesis batch is held.
    pub fn kind(&self) -> Option<HistoryKind> {
        self.kind
    }

    /// The store.
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Sets how many batches may be held back and quarantined (TL-102, TL-103).
    pub fn set_buffer_limits(&mut self, hold: usize, quarantine: usize) {
        self.hold_limit = hold;
        self.quarantine_limit = quarantine;
    }

    /// The batches currently held back, waiting for dependencies.
    pub fn held_back(&self) -> Vec<BatchId> {
        self.held.keys().copied().collect()
    }

    /// The batches currently in quarantine.
    pub fn quarantined(&self) -> Vec<BatchId> {
        self.quarantine.keys().copied().collect()
    }

    /// Pairs of validly signed envelopes that equivocate (TL-104): fault proofs.
    pub fn faults(&self) -> &[FaultProof] {
        &self.faults
    }

    /// The integrated envelopes, in integration order.
    pub fn envelopes(&self) -> impl Iterator<Item = &[u8]> {
        self.graph.nodes.iter().map(|n| &*n.envelope)
    }

    // -----------------------------------------------------------------------------------------
    // Receiving (§14)

    /// Integrates one envelope from a peer, from storage written by another version, or from a
    /// file (TL-100 – TL-104). The only error is a storage failure.
    pub fn receive(&mut self, bytes: &[u8], dir: &dyn Directory) -> Result<Report, Error> {
        let p = match self.pending(bytes, Some(dir)) {
            Ok(p) => p,
            Err((batch, verdict)) => {
                return Ok(Report {
                    batch,
                    verdict,
                    others: Vec::new(),
                });
            }
        };
        let id = p.id;
        let report = |verdict, others| {
            Ok(Report {
                batch: Some(id),
                verdict,
                others,
            })
        };
        if self.integrated.contains(&id) {
            return report(Verdict::Duplicate, Vec::new());
        }
        if self.held.contains_key(&id) {
            return report(Verdict::Held, Vec::new());
        }
        if self.quarantine.contains_key(&id) {
            return report(Verdict::Quarantined, Vec::new());
        }
        if let Err(fail) = self.check_collisions(&p.batch, &p.raw) {
            self.record_fault(&fail, &p.envelope);
            return report(fail.verdict, Vec::new());
        }
        if p.auth == Some(false) {
            self.quarantine_batch(p);
        } else {
            self.hold(p);
        }
        let mut events = Vec::new();
        self.run(dir, &mut events)?;
        let mut verdict = if self.held.contains_key(&id) {
            Verdict::Held
        } else if self.quarantine.contains_key(&id) {
            Verdict::Quarantined
        } else {
            Verdict::Integrated
        };
        let mut others = Vec::new();
        for (b, v) in events {
            if b == id {
                verdict = v;
            } else {
                others.push((b, v));
            }
        }
        report(verdict, others)
    }

    /// TL-101 steps 1 – 3 and 5, and the authorisation of step 4. `dir` is `None` for trusted
    /// data (our own store).
    fn pending(
        &self,
        bytes: &[u8],
        dir: Option<&dyn Directory>,
    ) -> Result<Pending, (Option<BatchId>, Verdict)> {
        self.pending_raw(bytes, dir, None)
    }

    fn pending_raw(
        &self,
        bytes: &[u8],
        dir: Option<&dyn Directory>,
        local_raw: Option<Vec<u8>>,
    ) -> Result<Pending, (Option<BatchId>, Verdict)> {
        let reject = |r: Reason| (None, Verdict::Rejected(r));
        let env = Envelope::decode(bytes).map_err(|e| reject(e.into()))?;
        if env.space != self.space || env.history.is_some_and(|h| h != self.id) {
            return Err(reject(Reason::WrongHistory));
        }
        let id = env.received_id(bytes);
        let mut key_valid = true;
        if let Some(dir) = dir {
            let info = dir
                .key(&env.actor, &env.key)
                .ok_or((Some(id), Verdict::Rejected(Reason::UnknownKey)))?;
            if !env.verify_id(&info.public_key, &id) {
                return Err((Some(id), Verdict::Rejected(Reason::BadSignature)));
            }
            key_valid = info.valid;
        }
        let decoded = match local_raw {
            Some(raw) => {
                if raw.len() > envelope::MAX_BATCH {
                    Err(crate::codec::DecodeError::TooLarge)
                } else {
                    crate::codec::decode_batch(&raw, &env.actor, &env.key).map(|batch| (batch, raw))
                }
            }
            None => env.batch(),
        };
        let (batch, raw) = decoded.map_err(|e| (Some(id), Verdict::Rejected(e.into())))?;
        // TL-101 step 7, Counters: judged at once, so a batch that could never be integrated is
        // never held back (TL-04, TL-76).
        let late = |v: &Version| v.heads().iter().any(|h| h.counter >= batch.start);
        let merge_late = batch.ops.iter().any(|op| match op {
            Op::Merge(m) => late(&m.incoming) || late(&m.base),
            _ => false,
        });
        let overflow = batch.start + batch.ops.len() as u64 - 1 > MAX_SAFE;
        if late(&batch.parents) || merge_late || overflow {
            return Err((Some(id), Verdict::Rejected(Reason::Lamport)));
        }
        let mut p = Pending {
            id,
            envelope: bytes.into(),
            raw: raw.into(),
            batch,
            empty_history: env.history.is_none(),
            key_valid,
            auth: None,
        };
        p.auth = match dir {
            None => Some(true),
            Some(dir) => self.authorisation(&p, dir),
        };
        Ok(p)
    }

    /// TL-103: `None` while the history's kind is unknown.
    fn authorisation(&self, p: &Pending, dir: &dyn Directory) -> Option<bool> {
        // The scope comes from the history's kind: from the batch itself when it is shaped as
        // a genesis, otherwise from the held genesis. Without one, nothing can be judged yet.
        let (kind, session) = if p.batch.parents.is_root() {
            match p.batch.ops.first() {
                Some(Op::TimelineCreate {
                    history_kind,
                    session,
                    ..
                }) => (*history_kind, session.clone()),
                _ => (None, None),
            }
        } else {
            (Some(self.kind?), self.session.clone())
        };
        if !p.key_valid {
            return Some(false);
        }
        // A transcript genesis without a session fails as `genesis` later; judge it as space.
        let scope = match (kind, &session) {
            (Some(HistoryKind::Transcript), Some(s)) => Scope::Session(s),
            _ => Scope::Space,
        };
        Some(dir.may_write(&p.batch.replica.actor, scope))
    }

    /// The op ids a batch needs before it can be integrated (TL-101 step 7), leaving out its own.
    fn dependencies(batch: &Batch) -> Vec<OpId> {
        let own = |id: &OpId| {
            *id.replica == *batch.replica && id.counter >= batch.start && id.counter <= batch.end()
        };
        let mut deps: Vec<OpId> = batch.parents.heads().to_vec();
        deps.extend(batch.timeline.clone());
        for op in &batch.ops {
            match op {
                Op::TimelineCreate {
                    source: Some(s), ..
                } => deps.push(s.clone()),
                Op::Merge(m) => {
                    deps.extend(m.incoming.heads().iter().cloned());
                    deps.extend(m.base.heads().iter().cloned());
                    for c in &m.changes {
                        if let Change::Text { inserts, .. } = c {
                            deps.extend(
                                inserts
                                    .iter()
                                    .filter_map(|i| i.origin.as_ref())
                                    .map(|o| o.insertion.op.clone()),
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        deps.retain(|d| !own(d));
        deps
    }

    fn deps_held(&self, batch: &Batch) -> bool {
        self.first_missing(batch).is_none()
    }

    /// A dependency the receiver does not hold yet, if any.
    fn first_missing(&self, batch: &Batch) -> Option<OpId> {
        Self::dependencies(batch)
            .into_iter()
            .find(|d| self.graph.locate(d).is_none())
    }

    /// The quarantined batch holding op `d`, if any.
    fn quarantined_holding(&self, d: &OpId) -> Option<BatchId> {
        self.quarantine
            .values()
            .find(|q| {
                q.batch.replica == d.replica
                    && q.batch.start <= d.counter
                    && d.counter <= q.batch.end()
            })
            .map(|q| q.id)
    }

    /// Holds a batch back: queued when its dependencies are held, otherwise filed under the
    /// first one missing. An authorised batch vouches for the quarantined batches it needs
    /// (TL-103), transitively.
    fn hold(&mut self, p: Pending) {
        let id = p.id;
        let vouches: Vec<BatchId> = if p.auth == Some(true) && !self.quarantine.is_empty() {
            Self::dependencies(&p.batch)
                .iter()
                .filter_map(|d| self.quarantined_holding(d))
                .collect()
        } else {
            Vec::new()
        };
        self.file(&p);
        self.held.insert(id, p);
        for v in vouches {
            if let Some(mut q) = self.quarantine.remove(&v) {
                q.auth = Some(true);
                self.hold(q);
            }
        }
    }

    /// Files a held batch as ready or under its first missing dependency.
    fn file(&mut self, p: &Pending) {
        match self.first_missing(&p.batch) {
            None => {
                self.ready
                    .insert((p.batch.start, p.batch.replica.clone(), p.id));
            }
            Some(d) => self
                .waits
                .entry(d.replica)
                .or_default()
                .entry(d.counter)
                .or_default()
                .push(p.id),
        }
    }

    /// Puts a batch in quarantine, unless an authorised held-back batch already depends on it.
    fn quarantine_batch(&mut self, q: Pending) {
        let needed = self.held.values().any(|h| {
            h.auth == Some(true)
                && Self::dependencies(&h.batch).iter().any(|d| {
                    d.replica == q.batch.replica
                        && d.counter >= q.batch.start
                        && d.counter <= q.batch.end()
                })
        });
        if needed {
            let mut q = q;
            q.auth = Some(true);
            self.hold(q);
        } else {
            self.quarantine.insert(q.id, q);
        }
    }

    /// Once the genesis is held, judges the batches held back before it (TL-101 step 4), so
    /// an authorised one vouches for what it needs.
    fn judge_waiting(&mut self, dir: &dyn Directory) {
        let ids: Vec<BatchId> = self
            .held
            .iter()
            .filter(|(_, h)| h.auth.is_none())
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(mut h) = self.held.remove(&id) {
                h.auth = self.authorisation(&h, dir);
                self.hold(h);
            }
        }
    }

    /// Re-files the held batches that waited for an op of a newly integrated batch.
    fn wake(&mut self, replica: &Arc<ReplicaId>, from: u64, to: u64) {
        let Some(by_counter) = self.waits.get_mut(replica) else {
            return;
        };
        let counters: Vec<u64> = by_counter.range(from..=to).map(|(c, _)| *c).collect();
        let mut woken = Vec::new();
        for c in counters {
            woken.extend(by_counter.remove(&c).unwrap_or_default());
        }
        for id in woken {
            if let Some(p) = self.held.remove(&id) {
                self.file(&p);
                self.held.insert(id, p);
            }
        }
    }

    /// Integrates ready held-back batches until none is left, then enforces the buffer limits.
    fn run(
        &mut self,
        dir: &dyn Directory,
        events: &mut Vec<(BatchId, Verdict)>,
    ) -> Result<(), Error> {
        while let Some((_, _, id)) = self.ready.pop_first() {
            let Some(mut h) = self.held.remove(&id) else {
                continue;
            };
            if h.auth.is_none() {
                h.auth = self.authorisation(&h, dir);
            }
            if h.auth == Some(false) {
                self.quarantine_batch(h);
                continue;
            }
            let (replica, from, to) = (h.batch.replica.clone(), h.batch.start, h.batch.end());
            let genesis = h.batch.parents.is_root();
            let verdict = match self.integrate(h, true) {
                Ok(()) => {
                    if genesis {
                        self.judge_waiting(dir);
                    }
                    self.wake(&replica, from, to);
                    Verdict::Integrated
                }
                Err(Ok(v)) => v,
                Err(Err(e)) => return Err(e),
            };
            events.push((id, verdict));
        }
        // Drop the greatest start first (TL-102, TL-103).
        let greatest = |m: &BTreeMap<BatchId, Pending>| {
            m.values()
                .max_by(|a, b| {
                    (a.batch.start, &a.batch.replica, a.id).cmp(&(
                        b.batch.start,
                        &b.batch.replica,
                        b.id,
                    ))
                })
                .map(|p| p.id)
        };
        while self.held.len() > self.hold_limit {
            let Some(id) = greatest(&self.held) else {
                break;
            };
            self.held.remove(&id);
            events.push((id, Verdict::Missing));
        }
        while self.quarantine.len() > self.quarantine_limit {
            let Some(id) = greatest(&self.quarantine) else {
                break;
            };
            self.quarantine.remove(&id);
            events.push((id, Verdict::Unauthorized));
        }
        Ok(())
    }

    fn record_fault(&mut self, fail: &Fail, envelope: &Arc<[u8]>) {
        if let Some(idx) = fail.fault {
            self.faults
                .push((self.graph.nodes[idx].envelope.clone(), envelope.clone()));
        }
    }

    /// Validates and integrates a batch whose dependencies are held. `Err(Ok(v))` is a verdict
    /// other than integrated; `Err(Err(e))` a storage failure.
    fn integrate(&mut self, p: Pending, persist: bool) -> Result<(), Result<Verdict, Error>> {
        let (closure, state, parents, timeline, merges) = match self.validate(&p) {
            Ok(x) => x,
            Err(fail) => {
                self.record_fault(&fail, &p.envelope);
                return Err(Ok(fail.verdict));
            }
        };
        if persist {
            self.store
                .append(&p.envelope)
                .map_err(|e| Err(store_err(e)))?;
        }
        if p.batch.parents.is_root()
            && let Some(Op::TimelineCreate {
                history_kind,
                session,
                ..
            }) = p.batch.ops.first()
        {
            self.kind = *history_kind;
            self.session = session.clone();
        }
        self.max_counter = self.max_counter.max(p.batch.end());
        self.integrated.insert(p.id);
        let n = p.batch.ops.len() as u32;
        let idx = self.graph.push(Node {
            batch: p.batch,
            parents,
            timeline,
            merges,
            envelope: p.envelope,
            raw: p.raw,
        });
        let mut closure = closure;
        closure.0.resize(idx + 1, 0);
        closure.0[idx] = n;
        self.cache_insert(vec![(idx, n - 1)], closure, Arc::new(state));
        Ok(())
    }

    /// TL-104 against integrated batches.
    pub(crate) fn check_collisions(&self, batch: &Batch, raw: &[u8]) -> Result<(), Fail> {
        let overlapping = self
            .graph
            .batches_in_range(&batch.replica, batch.start, batch.end());
        let equivocation = |idx: usize| Fail {
            verdict: Verdict::Rejected(Reason::Equivocation),
            fault: Some(idx),
        };
        for &idx in &overlapping {
            let node = &self.graph.nodes[idx];
            if node.batch.start == batch.start && node.batch.end() == batch.end() {
                if *node.raw == *raw {
                    return Err(Fail {
                        verdict: Verdict::Duplicate,
                        fault: None,
                    });
                }
                return Err(equivocation(idx));
            }
        }
        let parents_of = |b: &Batch, c: u64| {
            if c == b.start {
                b.parents.clone()
            } else {
                Version::new(vec![OpId::new(c - 1, b.replica.clone())])
            }
        };
        for &idx in &overlapping {
            let other = &self.graph.nodes[idx].batch;
            for c in batch.start.max(other.start)..=batch.end().min(other.end()) {
                let a = &batch.ops[(c - batch.start) as usize];
                let b = &other.ops[(c - other.start) as usize];
                if a != b || parents_of(batch, c) != parents_of(other, c) {
                    return Err(equivocation(idx));
                }
            }
        }
        if overlapping.is_empty() {
            Ok(())
        } else {
            Err(Reason::Overlap.into())
        }
    }

    // -----------------------------------------------------------------------------------------
    // Materialisation (§10)

    fn cache_insert(&self, heads: Vec<Pos>, closure: Closure, state: Arc<State>) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.tick += 1;
        let tick = cache.tick;
        if let Some(e) = cache.entries.iter_mut().find(|e| e.heads == heads) {
            e.used = tick;
            return;
        }
        if cache.entries.len() >= CACHE_SIZE
            && let Some(lru) = cache
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.used)
                .map(|(i, _)| i)
        {
            cache.entries.swap_remove(lru);
        }
        let size = closure.0.iter().map(|n| u64::from(*n)).sum();
        cache.entries.push(CacheEntry {
            heads,
            size,
            closure,
            state,
            used: tick,
        });
    }

    /// The closure of `heads` and the state at it (TL-60).
    pub(crate) fn materialise(&self, heads: &[Pos]) -> (Closure, Arc<State>) {
        // Fast path: these exact heads are cached (the usual case for a timeline's head).
        {
            let mut sorted = heads.to_vec();
            sorted.sort_unstable();
            sorted.dedup();
            let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.tick += 1;
            let tick = cache.tick;
            if let Some(e) = cache.entries.iter_mut().find(|e| e.heads == sorted) {
                e.used = tick;
                return (e.closure.clone(), e.state.clone());
            }
        }
        let closure = self.graph.closure(heads, false);
        let mut front = self.graph.frontier(&closure);
        front.sort();
        let base = {
            let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.tick += 1;
            let tick = cache.tick;
            if let Some(e) = cache.entries.iter_mut().find(|e| e.heads == front) {
                e.used = tick;
                return (closure, e.state.clone());
            }
            cache
                .entries
                .iter_mut()
                .filter(|e| e.heads.iter().all(|h| closure.contains(*h)))
                .max_by_key(|e| e.size)
                .map(|e| {
                    e.used = tick;
                    (e.closure.clone(), e.state.clone())
                })
        };
        let (below, mut state) = match base {
            Some((c, s)) => (Some(c), (*s).clone()),
            None => (None, State::default()),
        };
        for pos in self.graph.ops_between(&closure, below.as_ref()) {
            let id = self.graph.op_id(pos);
            let graph = &self.graph;
            let saw = |w: &OpId| {
                graph
                    .locate(w)
                    .is_some_and(|wp| wp != pos && graph.happens_before_eq(wp, pos))
            };
            state.apply(&id, graph.op(pos), &Seen::Some(&saw));
        }
        let state = Arc::new(state);
        self.cache_insert(front, closure.clone(), state.clone());
        (closure, state)
    }

    /// The state at `heads`, taken out of the cache: a batch built on it replaces it there,
    /// so it can be changed in place instead of copied.
    pub(crate) fn take_state(&self, heads: &[Pos]) -> (Closure, Arc<State>) {
        let (closure, state) = self.materialise(heads);
        let mut sorted = heads.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.entries.retain(|e| e.heads != sorted);
        (closure, state)
    }

    /// The positions of a version's ops, or `unknown_op`.
    pub(crate) fn positions(&self, v: &Version) -> Result<Vec<Pos>, Error> {
        v.heads()
            .iter()
            .map(|h| self.graph.locate(h).ok_or(Error::UnknownOp))
            .collect()
    }

    pub(crate) fn at_positions(&self, at: &At) -> Result<Vec<Pos>, Error> {
        match at {
            At::Timeline(t) => self
                .graph
                .head(t)
                .map(<[Pos]>::to_vec)
                .ok_or(Error::UnknownTimeline),
            At::Version(v) => self.positions(v),
        }
    }

    /// The state at a timeline's head or at a version (TL-60).
    pub fn state(&self, at: &At) -> Result<Arc<State>, Error> {
        let heads = self.at_positions(at)?;
        Ok(self.materialise(&heads).1)
    }

    /// The version at a timeline's head or the frontier of a version's closure.
    pub fn version(&self, at: &At) -> Result<Version, Error> {
        let heads = self.at_positions(at)?;
        let closure = self.graph.closure(&heads, false);
        Ok(self.graph.version_of(&self.graph.frontier(&closure)))
    }

    /// The head of a timeline (TL-24).
    pub fn head(&self, timeline: &OpId) -> Option<Version> {
        self.graph.head(timeline).map(|h| self.graph.version_of(h))
    }

    /// Every timeline, in timeline id order.
    pub fn timelines(&self) -> Vec<TimelineInfo> {
        self.graph
            .timelines()
            .filter_map(|t| {
                let pos = self.graph.locate(t)?;
                let Op::TimelineCreate { name, source, .. } = self.graph.op(pos) else {
                    return None;
                };
                Some(TimelineInfo {
                    id: t.clone(),
                    name: name.clone(),
                    source: source.clone(),
                    head: self.head(t)?,
                })
            })
            .collect()
    }

    /// The timeline with this name: `unknown_timeline` or `ambiguous_name` (TL-20).
    pub fn timeline_by_name(&self, name: &str) -> Result<OpId, Error> {
        let found: Vec<OpId> = self
            .timelines()
            .into_iter()
            .filter(|t| t.name == name)
            .map(|t| t.id)
            .collect();
        match found.as_slice() {
            [one] => Ok(one.clone()),
            [] => Err(Error::UnknownTimeline),
            _ => Err(Error::AmbiguousName),
        }
    }

    /// The main timeline, once the genesis batch is held.
    pub fn main(&self) -> Option<OpId> {
        self.graph.nodes.first().map(|n| n.timeline.clone())
    }

    /// The checkpoints of a timeline in op id order (TL-25).
    pub fn checkpoints(&self, timeline: &OpId) -> Vec<Checkpoint> {
        let mut out = Vec::new();
        for (b, node) in self.graph.nodes.iter().enumerate() {
            if node.timeline != *timeline {
                continue;
            }
            for (o, op) in node.batch.ops.iter().enumerate() {
                if let Op::Checkpoint { label } = op {
                    let parents = self.graph.op_parents((b, o as u32));
                    out.push(Checkpoint {
                        id: node.batch.op_id(o),
                        label: label.clone(),
                        version: self.graph.version_of(&parents),
                    });
                }
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    // -----------------------------------------------------------------------------------------
    // Local writes

    /// The next free counter for a device key (TL-04, TL-120).
    fn next_counter(&self, signer: &dyn Signer) -> Result<u64, Error> {
        let hw = self
            .store
            .high_water(signer.actor(), signer.key_id())
            .map_err(store_err)?;
        Ok(self.max_counter.max(hw) + 1)
    }

    fn commit_local(
        &mut self,
        batch: Batch,
        envelope: Vec<u8>,
        raw: Vec<u8>,
    ) -> Result<Written, Error> {
        self.store
            .raise_high_water(&batch.replica.actor, &batch.replica.key, batch.end())
            .map_err(store_err)?;
        let ops = (0..batch.ops.len()).map(|i| batch.op_id(i)).collect();
        let p = self
            .pending_raw(&envelope, None, Some(raw))
            .map_err(|(_, v)| match v {
                Verdict::Rejected(r) => Error::Rejected(r),
                _ => Error::Rejected(Reason::Malformed),
            })?;
        let id = p.id;
        if !self.deps_held(&p.batch) {
            return Err(Error::UnknownOp);
        }
        if let Err(fail) = self.check_collisions(&p.batch, &p.raw) {
            return Err(match fail.verdict {
                Verdict::Rejected(r) => Error::Rejected(r),
                _ => Error::Rejected(Reason::Equivocation),
            });
        }
        self.integrate(p, true).map_err(|e| match e {
            Ok(Verdict::Rejected(r)) => Error::Rejected(r),
            Ok(_) => Error::Rejected(Reason::Equivocation),
            Err(e) => e,
        })?;
        Ok(Written {
            envelope,
            batch: id,
            ops,
        })
    }

    /// Writes one batch whose ops `make` builds from the batch's first op id.
    pub(crate) fn write_with(
        &mut self,
        signer: &dyn Signer,
        lane: u32,
        parents: Version,
        timeline: Option<OpId>,
        make: impl FnOnce(&OpId) -> Vec<Op>,
    ) -> Result<Written, Error> {
        let start = self.next_counter(signer)?;
        let replica = Arc::new(ReplicaId::new(signer.actor(), signer.key_id(), lane));
        let ops = make(&OpId::new(start, replica.clone()));
        if ops.is_empty() {
            return Err(Error::Rejected(Reason::Malformed));
        }
        if start.saturating_add(ops.len() as u64) > MAX_SAFE {
            return Err(Error::Rejected(Reason::Lamport));
        }
        let batch = Batch {
            replica,
            start,
            parents,
            timeline,
            ops,
            time: None,
            step: None,
        };
        let (envelope, raw) = envelope::seal_raw(
            &self.space,
            Some(self.id),
            &batch,
            signer,
            Compression::Zstd,
        );
        self.commit_local(batch, envelope, raw)
    }

    /// Writes `ops` as one batch on `timeline`, after its head (TL-24). `step` names the tool
    /// call or step that caused it. The batch is validated like a peer's before it is kept.
    pub fn write(
        &mut self,
        signer: &dyn Signer,
        lane: u32,
        timeline: &OpId,
        ops: Vec<Op>,
        step: Option<String>,
    ) -> Result<Written, Error> {
        let head = self.head(timeline).ok_or(Error::UnknownTimeline)?;
        let start = self.next_counter(signer)?;
        if start.saturating_add(ops.len() as u64) > MAX_SAFE {
            return Err(Error::Rejected(Reason::Lamport));
        }
        if ops.is_empty() {
            return Err(Error::Rejected(Reason::Malformed));
        }
        let batch = Batch {
            replica: Arc::new(ReplicaId::new(signer.actor(), signer.key_id(), lane)),
            start,
            parents: head,
            timeline: Some(timeline.clone()),
            ops,
            time: None,
            step,
        };
        let (envelope, raw) = envelope::seal_raw(
            &self.space,
            Some(self.id),
            &batch,
            signer,
            Compression::Zstd,
        );
        self.commit_local(batch, envelope, raw)
    }

    /// Forks `source` at `at` (its head when `None`) into a new timeline named `name` (TL-22,
    /// TL-26). The new timeline's id is the first op id of the result.
    pub fn fork(
        &mut self,
        signer: &dyn Signer,
        lane: u32,
        source: &OpId,
        at: Option<&Version>,
        name: &str,
    ) -> Result<Written, Error> {
        let parents = match at {
            Some(v) => self.version(&At::Version(v.clone()))?,
            None => self.head(source).ok_or(Error::UnknownTimeline)?,
        };
        let op = Op::TimelineCreate {
            name: name.to_string(),
            source: Some(source.clone()),
            history_kind: None,
            session: None,
        };
        self.write_with(signer, lane, parents, None, |_| vec![op])
    }
}

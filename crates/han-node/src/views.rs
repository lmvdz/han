//! The views engine (PLAN section 3): incremental projections of the followed spaces. A view is a snapshot of
//! its window and then diffs; the property is that a snapshot plus every later diff equals a fresh read
//! (`Engine::read`), at every moment, including as silence turns sessions stale and failed.
//!
//! Each followed space has a [`SpaceModel`] folded from verified records and live status items. A model
//! counts its changes in `rev`; every subscription remembers the revisions it was last computed from and is
//! recomputed only when one it depends on moved. Time enters only through [`Engine::tick`], so views never
//! change between ticks without an input.
//!
//! The diff vocabulary is the same for every view (see [`DiffOp`]): rows are keyed and ordered by key, a diff
//! is inserts (with their index), updates and removes, and a resnapshot replaces everything when a diff would
//! be as big as the window.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use han_core::LogRecord;

use crate::api::{
    BoardTotals, ChangeSummaryRow, DiffOp, ErrorNote, Facets, FilePatchRow, GroupTotal, ItemBody,
    JoinProgressRow, JoinStage, NodeError, NodeStatusRow, OutboxRow, Query, RECENT_ERRORS,
    RoleInfo, Row, Scope, SessionPart, SpaceLink, StatusBody, TimelinePointRow, ViewEvent,
    ViewSink, masked_spans,
};
pub use crate::model::ProfileFacts;
pub use crate::model::{Freshness, LEASE_KIND};
use crate::model::{Now, SpaceModel};

/// A diff with at least this many ops, and as many as half the window, is sent as a resnapshot.
const RESNAPSHOT_OPS: usize = 64;

/// An entry in the outbox, as the `Outbox` view shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxItem {
    /// The outbox id.
    pub id: u64,
    /// The entry's id.
    pub entry_id: String,
    /// The entry's kind.
    pub kind: String,
    /// `pending`, `sent`, `rejected` or `failed` (gave up after too many tries; `OutboxRetry` queues it again).
    pub state: String,
    /// Delivery attempts.
    pub attempts: u32,
    /// The receipt's seq once sent.
    pub seq: Option<u64>,
    /// The last error.
    pub error: Option<String>,
}

/// Facts about the node itself, for the `NodeStatus` view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodeInfo {
    /// The device's node id.
    pub node_id: String,
    /// Supervised roles.
    pub roles: Vec<RoleInfo>,
    /// What stops roles from starting on this machine (the supervisor's).
    pub problems: Vec<String>,
}

/// Where a session's work timeline comes from. han-timeline backs it in M4; until then [`NoTimeline`] says
/// the views are unavailable.
pub trait TimelineSource: Send + Sync + 'static {
    /// The points of a session's timeline, up to `at` (a time) if given.
    fn points(&self, session: &str, at: Option<&str>) -> Result<Vec<TimelinePointRow>, NodeError>;

    /// The per-file patches of a session between two points.
    fn patches(
        &self,
        session: &str,
        from: Option<&str>,
        to: &str,
    ) -> Result<Vec<FilePatchRow>, NodeError>;

    /// The change set of an effect: a push plan or a merge preview.
    fn change_set(&self, space: &str, proposal: &str) -> Result<Vec<FilePatchRow>, NodeError>;

    /// How many paths of a session the host kept out of its timeline (secrets, per its exclusion list).
    fn excluded(&self, _session: &str) -> u32 {
        0
    }
}

/// The timeline source of a node with no timeline engine.
pub struct NoTimeline;

/// What a person confirms when they approve a proposal (see [`Engine::approval_subject`]).
#[derive(Clone, Debug)]
pub(crate) struct ApprovalSubject {
    pub digest: String,
    pub open: bool,
    pub action: String,
    pub target: String,
    pub requester: String,
}

impl NoTimeline {
    fn err<T>() -> Result<T, NodeError> {
        Err(NodeError::unavailable(
            "work timelines are not available on this node yet",
        ))
    }
}

impl TimelineSource for NoTimeline {
    fn points(&self, _: &str, _: Option<&str>) -> Result<Vec<TimelinePointRow>, NodeError> {
        Self::err()
    }

    fn patches(&self, _: &str, _: Option<&str>, _: &str) -> Result<Vec<FilePatchRow>, NodeError> {
        Self::err()
    }

    fn change_set(&self, _: &str, _: &str) -> Result<Vec<FilePatchRow>, NodeError> {
        Self::err()
    }
}

struct Sub {
    id: u64,
    query: Query,
    sink: ViewSink,
    last: Page,
    sig: u64,
    /// The `seq` of the last event sent.
    seq: u64,
}

/// The projections of every followed space and the views open on them.
pub struct Engine {
    me: String,
    spaces: BTreeMap<String, SpaceModel>,
    default_fresh: Freshness,
    now: Now,
    subs: Vec<Sub>,
    next_sub: u64,
    info: NodeInfo,
    timeline: Arc<dyn TimelineSource>,
    /// Moves when something outside the spaces changes (node info, timeline).
    misc_rev: u64,
    /// The id of this device's key in its actor's profile.
    device_key: String,
    /// Views ended with a `Resync` since the node started.
    resyncs: u64,
    /// The last errors the node answered.
    errors: VecDeque<ErrorNote>,
    /// Moves when what only the `NodeStatus` view shows changes: views opened and closed, resyncs, errors.
    node_rev: u64,
    /// How many views are open (kept apart from `subs`, which is taken while publishing).
    live_subs: usize,
    /// Moves whenever the engine changes (a record folded, a space added or removed, a link changed): what
    /// a command waits on for the state it needs (`Engine::watch`).
    progress: tokio::sync::watch::Sender<u64>,
    /// Sessions `(space, session)` whose live stream the node has asked the home for (and got its answer):
    /// from then on the items a session publishes come back to this node.
    watch_settled: BTreeSet<(String, String)>,
}

/// The rows of a window, ordered by key, with where the window sits among all matching rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Page {
    rows: BTreeMap<String, Row>,
    total: u32,
    offset: u32,
    next: Option<String>,
    totals: Option<BoardTotals>,
}

/// The board's totals, from every row of the space (before the filter and the collapsed groups apply).
fn board_totals(rows: &[(Facets, Row)], w: &crate::api::Window) -> BoardTotals {
    let mut t = BoardTotals::default();
    // group -> (the first key of the group, its totals)
    let mut groups: BTreeMap<String, (String, GroupTotal)> = BTreeMap::new();
    for (f, row) in rows {
        let Row::Board(b) = row else { continue };
        if b.closed && !w.filter.include_closed {
            continue;
        }
        t.all.add(b.state);
        if b.needs_me {
            t.needs_me_total += 1;
        }
        if w.filter.matches(f) {
            t.filtered.add(b.state);
            let tier = b.overlaps.iter().map(|k| k.tier()).max().unwrap_or(0);
            let g = groups.entry(b.group.clone()).or_insert_with(|| {
                (
                    b.key.clone(),
                    GroupTotal {
                        group: b.group.clone(),
                        count: 0,
                        needs_me: 0,
                        max_overlap_tier: 0,
                    },
                )
            });
            if b.key < g.0 {
                g.0 = b.key.clone();
            }
            g.1.count += 1;
            g.1.needs_me += u32::from(b.needs_me);
            g.1.max_overlap_tier = g.1.max_overlap_tier.max(tier);
        }
    }
    let mut groups: Vec<(String, GroupTotal)> = groups.into_values().collect();
    groups.sort_by(|a, b| a.0.cmp(&b.0));
    t.groups = groups.into_iter().map(|(_, g)| g).collect();
    t
}

fn page(mut rows: Vec<(Facets, Row)>, q: &Query) -> Page {
    let w = q.window();
    let totals = matches!(q, Query::Board { .. }).then(|| board_totals(&rows, w));
    if let Query::Board { collapsed, .. } = q {
        rows.retain(|(_, r)| match r {
            Row::Board(b) => !collapsed.contains(&b.group),
            _ => true,
        });
    }
    rows.retain(|(f, _)| w.filter.matches(f));
    rows.sort_by(|a, b| a.1.key().cmp(b.1.key()));
    let total = u32::try_from(rows.len()).unwrap_or(u32::MAX);
    let limit = w.limit.clamp(1, q.max_limit()) as usize;
    // Where the window sits among the matching rows: after a cursor, at the end, or before a key.
    let (start, end) = if let Some(b) = w.before.as_deref() {
        let end = rows.partition_point(|(_, r)| r.key() < b);
        (end.saturating_sub(limit), end)
    } else if let Some(c) = w.cursor.as_deref() {
        let start = rows.partition_point(|(_, r)| r.key() <= c);
        (start, (start + limit).min(rows.len()))
    } else if w.from_end {
        (rows.len().saturating_sub(limit), rows.len())
    } else {
        (0, limit.min(rows.len()))
    };
    let offset = u32::try_from(start).unwrap_or(u32::MAX);
    let next = (end < rows.len() && end > start).then(|| rows[end - 1].1.key().to_owned());
    let out: BTreeMap<String, Row> = rows
        .drain(start..end)
        .map(|(_, row)| (row.key().to_owned(), row))
        .collect();
    Page {
        rows: out,
        total,
        offset,
        next,
        totals,
    }
}

fn diff(old: &BTreeMap<String, Row>, new: &BTreeMap<String, Row>) -> Vec<DiffOp> {
    let mut ops = Vec::new();
    for k in old.keys().filter(|k| !new.contains_key(*k)) {
        ops.push(DiffOp::Remove { key: k.clone() });
    }
    for (k, row) in new {
        if old.get(k).is_some_and(|o| o != row) {
            ops.push(DiffOp::Update { row: row.clone() });
        }
    }
    for (i, (k, row)) in new.iter().enumerate() {
        if !old.contains_key(k) {
            ops.push(DiffOp::Insert {
                index: u32::try_from(i).unwrap_or(u32::MAX),
                row: row.clone(),
            });
        }
    }
    ops
}

fn snapshot(p: &Page, seq: u64, now_ms: u64) -> ViewEvent {
    ViewEvent::Snapshot {
        seq,
        rows: p.rows.values().cloned().collect(),
        total: p.total,
        offset: p.offset,
        next_cursor: p.next.clone(),
        now_ms,
        totals: p.totals.clone(),
    }
}

impl Engine {
    /// An engine with no spaces, for the device acting as `me`. `freshness` is the default for spaces that do
    /// not set their own.
    pub fn new(me: &str, freshness: Freshness) -> Engine {
        Engine {
            me: me.to_owned(),
            spaces: BTreeMap::new(),
            default_fresh: freshness,
            now: Now { ms: 0, secs: 0 },
            subs: Vec::new(),
            next_sub: 1,
            info: NodeInfo::default(),
            timeline: Arc::new(NoTimeline),
            misc_rev: 0,
            device_key: String::new(),
            resyncs: 0,
            progress: tokio::sync::watch::channel(0).0,
            watch_settled: BTreeSet::new(),
            errors: VecDeque::new(),
            node_rev: 0,
            live_subs: 0,
        }
    }

    /// Tells the engine which key of the actor's profile this device holds.
    pub fn set_device_key(&mut self, key_id: &str) {
        key_id.clone_into(&mut self.device_key);
    }

    /// Notes an error the node answered (a command or a view), for the Node panel.
    pub fn note_error(&mut self, what: &str, error: &NodeError) {
        self.errors.push_back(ErrorNote {
            code: error.code().to_owned(),
            what: what.to_owned(),
            at_ms: self.now.ms,
        });
        while self.errors.len() > RECENT_ERRORS {
            self.errors.pop_front();
        }
        self.node_rev += 1;
        self.publish();
    }

    /// Takes a join signal of `space`: `actor` asks to be admitted.
    pub fn join_signal(&mut self, space: &str, actor: &str) {
        let ms = self.now.ms;
        if let Some(m) = self.spaces.get_mut(space) {
            m.join_signal(actor, ms);
            self.publish();
        }
    }

    /// The actors of `space` whose profiles the node still has to read.
    pub fn take_wanted(&mut self, space: &str) -> Vec<String> {
        self.spaces
            .get_mut(space)
            .map(|m| std::mem::take(&mut m.wanted).into_iter().collect())
            .unwrap_or_default()
    }

    /// What the home says about an actor (`None`: it could not be read).
    pub fn set_profile(&mut self, space: &str, actor: &str, facts: Option<ProfileFacts>) {
        let ms = self.now.ms;
        if let Some(m) = self.spaces.get_mut(space) {
            m.set_profile(actor, facts, ms);
            self.publish();
        }
    }

    /// What the home says about an admission's actor, if it has been read.
    pub fn profile_of(&self, space: &str, actor: &str) -> Option<Option<ProfileFacts>> {
        self.spaces.get(space)?.profiles.get(actor).cloned()
    }

    /// Whether `actor` asked to join `space` and waits.
    pub fn is_pending(&self, space: &str, actor: &str) -> bool {
        self.spaces
            .get(space)
            .is_some_and(|m| m.pending.contains_key(actor) && !m.roles.contains_key(actor))
    }

    /// Whether `actor` holds a role in `space`.
    pub fn has_role(&self, space: &str, actor: &str) -> bool {
        self.spaces
            .get(space)
            .is_some_and(|m| m.roles.contains_key(actor))
    }

    /// Whether `actor` holds exactly `role` (`owner`, `member`, `guest`, `banned`; `none` means no role) in
    /// `space`, as this device has read it.
    pub fn role_is(&self, space: &str, actor: &str, role: &str) -> bool {
        use han_core::policy::Role;
        let Some(m) = self.spaces.get(space) else {
            return false;
        };
        matches!(
            (m.roles.get(actor), role),
            (None, "none")
                | (Some(Role::Owner), "owner")
                | (Some(Role::Member), "member")
                | (Some(Role::Guest), "guest")
                | (Some(Role::Banned), "banned")
        )
    }

    /// Takes an item a host put on a session's stream. True if it changed what a face sees.
    pub fn apply_item(
        &mut self,
        space: &str,
        session: &str,
        id: String,
        turn: Option<u32>,
        body: ItemBody,
    ) -> bool {
        let ms = self.now.ms;
        let changed = self
            .spaces
            .get_mut(space)
            .is_some_and(|m| m.push_item(session, id, turn, body, ms, true));
        self.publish();
        changed
    }

    /// Plugs in the timeline engine behind `Timeline`, `Patches` and `ChangeSet`.
    pub fn set_timeline(&mut self, source: Arc<dyn TimelineSource>) {
        self.timeline = source;
        self.timeline_changed();
    }

    /// The timeline source has new points or patches.
    pub fn timeline_changed(&mut self) {
        self.misc_rev += 1;
        self.publish();
    }

    /// Sets what the `NodeStatus` view shows besides the spaces.
    pub fn set_node_info(&mut self, info: NodeInfo) {
        if self.info != info {
            self.info = info;
            self.misc_rev += 1;
            self.publish();
        }
    }

    /// Starts projecting `space`, homed at `home`. Does nothing if it already is.
    pub fn add_space(&mut self, space: &str, home: &str, freshness: Option<Freshness>) {
        if !self.spaces.contains_key(space) {
            let fresh = freshness.unwrap_or(self.default_fresh);
            self.spaces.insert(
                space.to_owned(),
                SpaceModel::new(space, home, fresh, &self.me),
            );
            self.publish();
        }
    }

    /// Moves whenever the engine changes. Take it, then check the state wanted, then wait on it: nothing
    /// between the check and the wait is missed.
    pub fn watch(&self) -> tokio::sync::watch::Receiver<u64> {
        self.progress.subscribe()
    }

    /// The node's watch of a session's stream is up (or the home refused it): items sent from now on are seen.
    pub fn session_watch_settled(&mut self, space: &str, session: &str) {
        self.watch_settled
            .insert((space.to_owned(), session.to_owned()));
        self.progress.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Whether the watch of a session's stream has settled (`session_watch_settled`).
    pub fn is_session_watched(&self, space: &str, session: &str) -> bool {
        self.watch_settled
            .contains(&(space.to_owned(), session.to_owned()))
    }

    /// Stops projecting `space`.
    pub fn remove_space(&mut self, space: &str) {
        if self.spaces.remove(space).is_some() {
            self.misc_rev += 1;
            self.publish();
        }
    }

    /// Whether `space` is projected.
    pub fn has_space(&self, space: &str) -> bool {
        self.spaces.contains_key(space)
    }

    /// The home of `space`.
    pub fn home_of(&self, space: &str) -> Option<String> {
        self.spaces.get(space).map(|m| m.home.clone())
    }

    /// The projected spaces homed at `server`.
    pub fn spaces_on(&self, server: &str) -> Vec<String> {
        self.spaces
            .values()
            .filter(|m| m.home == server)
            .map(|m| m.space.clone())
            .collect()
    }

    /// The budget a session in `space` may spend, working `task` if it has one: unit and amount.
    pub fn session_budget(&self, space: &str, task: Option<&str>) -> Option<(String, u64)> {
        self.spaces.get(space).and_then(|m| m.session_budget(task))
    }

    /// The operator the profile space `space` names, once its first record has arrived.
    pub fn profile_operator(&self, space: &str) -> Option<String> {
        self.spaces
            .get(space)
            .and_then(|m| m.profile_operator.clone())
    }

    /// The last seq folded for `space`.
    pub fn head(&self, space: &str) -> Option<u64> {
        self.spaces.get(space).map(|m| m.head)
    }

    /// How `space` is linked.
    pub fn link(&self, space: &str) -> Option<SpaceLink> {
        self.spaces.get(space).map(|m| m.link)
    }

    /// The actor of a session of `space`, if this node knows the session.
    pub fn session_actor(&self, space: &str, session: &str) -> Option<String> {
        self.spaces
            .get(space)
            .and_then(|m| m.sessions.get(session))
            .map(|s| s.actor.clone())
    }

    /// The sessions of `space` that are not closed.
    pub fn open_sessions(&self, space: &str) -> Vec<String> {
        self.spaces
            .get(space)
            .map(|m| {
                m.sessions
                    .values()
                    .filter(|s| !s.closed)
                    .map(|s| s.id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The proposal's plan digest and whether it still waits for approval, for `EffectApprove`.
    pub fn plan_digest(&self, space: &str, proposal: &str) -> Option<(String, bool)> {
        let e = self.spaces.get(space)?.effects.get(proposal)?;
        Some((
            e.plan_digest.clone(),
            crate::model::effect_open(e, &self.now),
        ))
    }

    /// What an approver is asked to confirm about a proposal: the digest, whether it still waits, and the
    /// action, target and proposer for the presence prompt.
    pub(crate) fn approval_subject(&self, space: &str, proposal: &str) -> Option<ApprovalSubject> {
        let e = self.spaces.get(space)?.effects.get(proposal)?;
        let target = e
            .plan
            .as_deref()
            .and_then(|p| han_core::json::parse(p.as_bytes()).ok())
            .and_then(|p| p.get("target").and_then(|t| t.as_str()).map(str::to_owned))
            .unwrap_or_else(|| "(unnamed)".to_owned());
        Some(ApprovalSubject {
            digest: e.plan_digest.clone(),
            open: crate::model::effect_open(e, &self.now),
            action: e.action.clone(),
            target,
            requester: e.proposer.clone(),
        })
    }

    /// Whether a task is open; `None` if it is unknown.
    pub fn task_open(&self, space: &str, task: &str) -> Option<bool> {
        Some(self.spaces.get(space)?.tasks.get(task)?.open)
    }

    /// Folds a verified record. Returns the sessions it opened.
    pub fn apply_record(&mut self, space: &str, rec: &LogRecord) -> Vec<String> {
        let now = self.now;
        let opened = match self.spaces.get_mut(space) {
            Some(m) => m.apply_record(rec, &now),
            None => Vec::new(),
        };
        self.publish();
        opened
    }

    /// Takes a live `han.status/1` item of a session; false if the model does not accept it.
    pub fn apply_status(&mut self, space: &str, status: StatusBody) -> bool {
        let now = self.now;
        let ok = match self.spaces.get_mut(space) {
            Some(m) => {
                let session = status.session.clone();
                m.apply_status(&session, status, &now)
            }
            None => false,
        };
        self.publish();
        ok
    }

    /// The link to a space's home changed.
    pub fn set_link(&mut self, space: &str, link: SpaceLink, error: Option<String>) {
        if let Some(m) = self.spaces.get_mut(space)
            && (m.link != link || m.link_error != error)
        {
            m.link = link;
            m.link_error = error;
            m.bump();
            self.publish();
        }
    }

    /// The outbox of `space` changed: its rows, newest last.
    pub fn set_outbox(&mut self, space: &str, items: Vec<OutboxItem>) {
        if let Some(m) = self.spaces.get_mut(space) {
            let pending = items.iter().filter(|i| i.state == "pending").count();
            let pending = u32::try_from(pending).unwrap_or(u32::MAX);
            if m.outbox != items || m.outbox_pending != pending {
                m.outbox = items;
                m.outbox_pending = pending;
                m.bump();
                self.publish();
            }
        }
    }

    /// A signal (for example a join) arrived for `space`.
    pub fn count_signal(&mut self, space: &str) {
        if let Some(m) = self.spaces.get_mut(space) {
            m.signals += 1;
            m.bump();
            self.publish();
        }
    }

    /// Moves time: `steady_ms` is the node's steady clock, `wall_secs` the wall clock. Sessions that have
    /// been silent long enough turn stale and then failed; leases and approvals lapse.
    pub fn tick(&mut self, steady_ms: u64, wall_secs: i64) {
        self.now = Now {
            ms: steady_ms,
            secs: wall_secs,
        };
        let now = self.now;
        // The node's own row carries the clock, so a view of it moves with every tick.
        let mut moved = self
            .subs
            .iter()
            .any(|s| matches!(s.query, Query::NodeStatus { .. }));
        for m in self.spaces.values_mut() {
            moved |= m.tick(&now);
            m.recheck_profiles(now.ms);
        }
        if moved {
            self.publish();
        }
    }

    // ───────────────────────── reading ─────────────────────────

    fn join_rows(&self, space: &str) -> Result<Vec<(Facets, Row)>, NodeError> {
        let m = self
            .spaces
            .get(space)
            .ok_or_else(|| NodeError::not_found(format!("no such space: {space}")))?;
        let waiting = m
            .link_error
            .as_deref()
            .is_some_and(|e| e.starts_with("not readable yet"));
        let admitted = m.roles.get(&self.me).is_some_and(|r| r.reads());
        let stage = match m.link {
            SpaceLink::Rejected => JoinStage::Rejected,
            SpaceLink::Misconfigured => JoinStage::Misconfigured,
            SpaceLink::Connecting => JoinStage::Connecting,
            SpaceLink::Offline if waiting => JoinStage::WaitingForAdmission,
            SpaceLink::Offline => JoinStage::Connecting,
            SpaceLink::Live if admitted => JoinStage::Ready,
            SpaceLink::Live => JoinStage::Syncing,
        };
        Ok(vec![(
            Facets::default(),
            Row::JoinProgress(JoinProgressRow {
                key: space.to_owned(),
                space: space.to_owned(),
                stage,
                actor: self.me.clone(),
                head: m.head,
                error: m.link_error.clone(),
            }),
        )])
    }

    /// What is stuck, one sentence each, in space order.
    fn problems(&self) -> Vec<String> {
        let mut spaces: Vec<&SpaceModel> = self.spaces.values().collect();
        spaces.sort_by(|a, b| a.space.cmp(&b.space));
        let mut out = self.info.problems.clone();
        for m in spaces {
            if m.link == SpaceLink::Misconfigured {
                out.push(format!(
                    "space {}: link to {} misconfigured: {}",
                    m.space,
                    m.home,
                    m.link_error.as_deref().unwrap_or("no reason given")
                ));
            }
            if let Some(i) = m.blocking_entry() {
                out.push(format!(
                    "space {} blocked by outbox entry {}: {}",
                    m.space,
                    i.id,
                    i.error.as_deref().unwrap_or("not sent")
                ));
            }
        }
        out
    }

    fn node_row(&self) -> (Facets, Row) {
        let live = self
            .spaces
            .values()
            .filter(|m| m.link == SpaceLink::Live)
            .count();
        let pending: u32 = self.spaces.values().map(|m| m.outbox_pending).sum();
        (
            Facets::default(),
            Row::NodeStatus(NodeStatusRow {
                key: "node".into(),
                actor: self.me.clone(),
                node_id: self.info.node_id.clone(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
                spaces: u32::try_from(self.spaces.len()).unwrap_or(u32::MAX),
                spaces_live: u32::try_from(live).unwrap_or(u32::MAX),
                outbox_pending: pending,
                roles: self.info.roles.clone(),
                now_ms: self.now.ms,
                subscriptions: u32::try_from(self.live_subs).unwrap_or(u32::MAX),
                resyncs: self.resyncs,
                recent_errors: self.errors.iter().cloned().collect(),
                problems: self.problems(),
            }),
        )
    }

    fn patch_rows(rows: Vec<FilePatchRow>, excluded: u32) -> Vec<(Facets, Row)> {
        let mut out: Vec<(Facets, Row)> = Vec::new();
        if excluded > 0 {
            out.push((
                Facets::default(),
                Row::ChangeSummary(ChangeSummaryRow {
                    key: String::new(),
                    excluded,
                }),
            ));
        }
        out.extend(rows.into_iter().map(|mut r| {
            // The host masked the patch; the node says where.
            r.masked = masked_spans(&r.patch);
            (
                Facets {
                    text: r.path.clone(),
                    ..Facets::default()
                },
                Row::FilePatch(r),
            )
        }));
        out
    }

    fn gather(&mut self, q: &Query) -> Result<Vec<(Facets, Row)>, NodeError> {
        let now = self.now;
        let not_found =
            |what: &str, id: &str| NodeError::not_found(format!("no such {what}: {id}"));
        q.window().filter.validate()?;
        match q {
            Query::Board { space, group, .. } => self
                .spaces
                .get_mut(space)
                .map(|m| m.board_rows(*group, &now))
                .ok_or_else(|| not_found("space", space)),
            Query::Session { id, part, .. } => match part {
                SessionPart::Facts => self
                    .spaces
                    .values()
                    .find_map(|m| m.sessions.get(id).map(|s| m.session_row(s, &now)))
                    .map(|r| vec![r])
                    .ok_or_else(|| not_found("session", id)),
                SessionPart::Items | SessionPart::Record => self
                    .spaces
                    .values()
                    .find_map(|m| match part {
                        SessionPart::Items => m.item_rows(id),
                        _ => m.record_rows(id),
                    })
                    .ok_or_else(|| not_found("session", id))?
                    .map_err(NodeError::denied),
            },
            Query::WhoIsOn { space, target, .. } => match space {
                Some(sp) => self
                    .spaces
                    .get(sp)
                    .map(|m| m.who_rows(target, &now))
                    .ok_or_else(|| not_found("space", sp)),
                None => Ok(self
                    .spaces
                    .values()
                    .flat_map(|m| m.who_rows(target, &now))
                    .collect()),
            },
            Query::Overlaps { space, .. } => self
                .spaces
                .get_mut(space)
                .map(|m| m.overlap_rows(&now))
                .ok_or_else(|| not_found("space", space)),
            Query::Inputs { session, .. } => self
                .spaces
                .values()
                .find_map(|m| m.input_rows(session))
                .ok_or_else(|| not_found("session", session)),
            Query::Tasks {
                space, assignee, ..
            } => self
                .spaces
                .get(space)
                .map(|m| m.task_rows(assignee.as_deref()))
                .ok_or_else(|| not_found("space", space)),
            Query::Guard { space, actor, .. } => self
                .spaces
                .get(space)
                .map(|m| vec![m.guard_row(actor)])
                .ok_or_else(|| not_found("space", space)),
            Query::Effects {
                space,
                executor,
                include_done,
                ..
            } => match space {
                Some(sp) => self
                    .spaces
                    .get(sp)
                    .map(|m| m.effect_rows(executor.as_deref(), *include_done))
                    .ok_or_else(|| not_found("space", sp)),
                None => Ok(self
                    .spaces
                    .values()
                    .flat_map(|m| m.effect_rows(executor.as_deref(), *include_done))
                    .collect()),
            },
            Query::Approvals { actor, .. } => Ok(self
                .spaces
                .values()
                .flat_map(|m| {
                    let mut rows = m.approval_rows(actor, &now);
                    rows.extend(m.admission_rows(actor));
                    rows
                })
                .collect()),
            Query::Devices { .. } => Ok(self
                .spaces
                .get(&format!("profile:{}", self.me))
                .map(|m| m.device_rows(&self.device_key))
                .unwrap_or_default()),
            Query::Spaces { .. } => Ok(self.spaces.values().map(SpaceModel::status_row).collect()),
            Query::SpaceStatus { space, .. } => self
                .spaces
                .get(space)
                .map(|m| vec![m.status_row()])
                .ok_or_else(|| not_found("space", space)),
            Query::Cost { scope, .. } => match scope {
                Scope::Space { space } => self
                    .spaces
                    .get(space)
                    .map(|m| m.cost_rows(None))
                    .ok_or_else(|| not_found("space", space)),
                Scope::Session { session } => self
                    .spaces
                    .values()
                    .find(|m| m.sessions.contains_key(session))
                    .map(|m| m.cost_rows(Some(session)))
                    .ok_or_else(|| not_found("session", session)),
            },
            Query::Outbox { space, .. } => {
                let mut rows = Vec::new();
                for m in self
                    .spaces
                    .values()
                    .filter(|m| space.as_deref().is_none_or(|s| s == m.space))
                {
                    for i in &m.outbox {
                        rows.push((
                            Facets {
                                text: format!("{} {}", i.kind, i.state),
                                ..Facets::default()
                            },
                            Row::Outbox(OutboxRow {
                                key: format!("{:012}", i.id),
                                outbox_id: i.id,
                                space: m.space.clone(),
                                entry_id: i.entry_id.clone(),
                                kind: i.kind.clone(),
                                state: i.state.clone(),
                                attempts: i.attempts,
                                seq: i.seq,
                                error: i.error.clone(),
                                blocking: m.blocking_entry().is_some_and(|b| b.id == i.id),
                            }),
                        ));
                    }
                }
                if let Some(sp) = space
                    && !self.spaces.contains_key(sp)
                {
                    return Err(not_found("space", sp));
                }
                Ok(rows)
            }
            Query::JoinProgress { space, .. } => self.join_rows(space),
            Query::NodeStatus { .. } => Ok(vec![self.node_row()]),
            Query::Timeline { session, at, .. } => {
                let points = self.timeline.points(session, at.as_deref())?;
                Ok(points
                    .into_iter()
                    .map(|p| {
                        (
                            Facets {
                                text: p.label.clone(),
                                ..Facets::default()
                            },
                            Row::TimelinePoint(p),
                        )
                    })
                    .collect())
            }
            Query::Patches {
                session, from, to, ..
            } => self
                .timeline
                .patches(session, from.as_deref(), to)
                .map(|rows| Self::patch_rows(rows, self.timeline.excluded(session))),
            Query::ChangeSet {
                space, proposal, ..
            } => self
                .timeline
                .change_set(space, proposal)
                .map(|rows| Self::patch_rows(rows, 0)),
        }
    }

    /// A fresh read of a window: its rows ordered by key, and the snapshot event that carries them.
    pub fn read(&mut self, q: &Query) -> Result<ViewEvent, NodeError> {
        let rows = self.gather(q)?;
        Ok(snapshot(&page(rows, q), 1, self.now.ms))
    }

    /// The revisions a query's rows are computed from.
    fn signature(&self, q: &Query) -> u64 {
        let mut h = DefaultHasher::new();
        self.misc_rev.hash(&mut h);
        if matches!(q, Query::NodeStatus { .. }) {
            self.now.ms.hash(&mut h);
            self.node_rev.hash(&mut h);
        }
        let one = |sp: &str, h: &mut DefaultHasher| {
            sp.hash(h);
            self.spaces.get(sp).map(|m| m.rev).hash(h);
        };
        match q {
            Query::Board { space, .. }
            | Query::Overlaps { space, .. }
            | Query::SpaceStatus { space, .. }
            | Query::JoinProgress { space, .. }
            | Query::Cost {
                scope: Scope::Space { space },
                ..
            }
            | Query::WhoIsOn {
                space: Some(space), ..
            }
            | Query::Outbox {
                space: Some(space), ..
            } => one(space, &mut h),
            _ => {
                for (sp, m) in &self.spaces {
                    sp.hash(&mut h);
                    m.rev.hash(&mut h);
                }
            }
        }
        h.finish()
    }

    // ───────────────────────── subscriptions ─────────────────────────

    /// Opens a view: sends the snapshot of its window now and diffs as inputs arrive. Returns the id to pass
    /// to [`Engine::unsubscribe`]. A query that cannot be served sends [`ViewEvent::Failed`] and opens nothing.
    pub fn subscribe(&mut self, query: Query, sink: ViewSink) -> Option<u64> {
        // The node panel counts the view it is about to be shown to.
        self.live_subs = self.subs.len() + 1;
        let last = match self.gather(&query) {
            Ok(rows) => page(rows, &query),
            Err(error) => {
                self.errors.push_back(ErrorNote {
                    code: error.code().to_owned(),
                    what: "view".to_owned(),
                    at_ms: self.now.ms,
                });
                while self.errors.len() > RECENT_ERRORS {
                    self.errors.pop_front();
                }
                self.live_subs = self.subs.len();
                self.node_rev += 1;
                sink.send(ViewEvent::Failed { seq: 1, error });
                return None;
            }
        };
        sink.send(snapshot(&last, 1, self.now.ms));
        let id = self.next_sub;
        self.next_sub += 1;
        self.node_rev += 1;
        let sig = self.signature(&query);
        self.subs.push(Sub {
            id,
            query,
            sink,
            last,
            sig,
            seq: 1,
        });
        self.publish();
        Some(id)
    }

    /// Closes a view.
    pub fn unsubscribe(&mut self, id: u64) {
        self.subs.retain(|s| s.id != id);
        self.live_subs = self.subs.len();
        self.node_rev += 1;
        self.publish();
    }

    /// Ends every open view without an error: the node is shutting down.
    pub fn end_all(&mut self) {
        for sub in std::mem::take(&mut self.subs) {
            sub.sink.send(ViewEvent::End { seq: sub.seq + 1 });
        }
        self.live_subs = 0;
    }

    /// How many views are open.
    pub fn open_views(&self) -> usize {
        self.subs.len()
    }

    fn publish(&mut self) {
        self.progress.send_modify(|n| *n = n.wrapping_add(1));
        // A view dropped while publishing changes what the node panel counts: once more for it.
        for _ in 0..3 {
            let before = self.node_rev;
            self.publish_once();
            if self.node_rev == before {
                break;
            }
        }
    }

    fn publish_once(&mut self) {
        let now = self.now;
        for m in self.spaces.values_mut() {
            m.sync_time(&now);
        }
        let mut subs = std::mem::take(&mut self.subs);
        let before = subs.len();
        subs.retain_mut(|sub| {
            if sub.sink.is_closed() {
                return false;
            }
            let sig = self.signature(&sub.query);
            if sig == sub.sig {
                return true;
            }
            sub.sig = sig;
            let new = match self.gather(&sub.query) {
                Ok(rows) => page(rows, &sub.query),
                Err(error) => {
                    sub.sink.send(ViewEvent::Failed {
                        seq: sub.seq + 1,
                        error,
                    });
                    return false;
                }
            };
            if new == sub.last {
                return true;
            }
            let ops = diff(&sub.last.rows, &new.rows);
            let now_ms = self.now.ms;
            sub.seq += 1;
            let seq = sub.seq;
            let event = if ops.len() >= RESNAPSHOT_OPS && ops.len() * 2 >= new.rows.len() {
                match snapshot(&new, seq, now_ms) {
                    ViewEvent::Snapshot {
                        seq,
                        rows,
                        total,
                        offset,
                        next_cursor,
                        now_ms,
                        totals,
                    } => ViewEvent::Resnapshot {
                        seq,
                        rows,
                        total,
                        offset,
                        next_cursor,
                        now_ms,
                        totals,
                    },
                    other => other,
                }
            } else {
                ViewEvent::Diff {
                    seq,
                    ops,
                    total: new.total,
                    offset: new.offset,
                    next_cursor: new.next.clone(),
                    now_ms,
                    totals: new.totals.clone(),
                }
            };
            sub.last = new;
            let sent = sub.sink.send(event);
            if !sent && sub.sink.was_resynced() {
                self.resyncs += 1;
            }
            sent
        });
        if subs.len() != before {
            self.node_rev += 1;
        }
        // Views opened while publishing cannot happen (the engine is locked), so this only puts them back.
        subs.append(&mut self.subs);
        self.subs = subs;
        self.live_subs = self.subs.len();
    }
}

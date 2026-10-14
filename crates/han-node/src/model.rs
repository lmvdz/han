//! What a device knows of one followed space, folded from the verified records and the live status items.
//!
//! This is the node's own projection for views, not Policy's fold: the home decided every entry already, and
//! the views only need who has what open. Authority questions stay with Policy, reached through han-core's
//! public API by the homes that decide and by the verifier (M4).

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use han_core::grammar;
use han_core::policy::{
    Body, EffectResult, PolicyDoc, PolicyEntry, Role, SessionLevel, Started, TaskState,
};
use han_core::{LogRecord, Value};
use han_seq::time::format_time;

use crate::api::{
    ApprovalRow, BoardRow, ClaimKind, CostRow, Facets, Group, OverlapClaim, OverlapFile,
    OverlapKind, OverlapParty, OverlapRow, OverlapSummary, SUMMARY_FIELDS, SUMMARY_TEXT,
    SessionRow, SessionState, SpaceLink, SpaceStatusRow, StatusBody, WhoRow,
};

/// The extension entry kind of an advisory path lease: body `{paths: [string], ttl: seconds, issue?: string}`.
/// A lease lapses `ttl` seconds after its receipt; nothing is ever blocked by one (PLAN 6.6).
pub const LEASE_KIND: &str = "han.lease";

/// When silence changes what the board shows (PLAN 6.4, configurable per space).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Freshness {
    /// A session with no item for this long shows as `stale` (default 10 minutes).
    pub stale_ms: u64,
    /// A session silent for this long shows as `failed` (default 30 minutes).
    pub failed_ms: u64,
}

impl Default for Freshness {
    fn default() -> Self {
        Freshness {
            stale_ms: 10 * 60 * 1000,
            failed_ms: 30 * 60 * 1000,
        }
    }
}

/// The time every row is computed at; it moves only when the engine ticks, so a fresh read and a stream of
/// diffs always agree.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Now {
    /// This node's steady clock.
    pub ms: u64,
    /// The wall clock, seconds since the epoch (lease and approval lifetimes).
    pub secs: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct SessionInfo {
    pub id: String,
    pub actor: String,
    pub opened: String,
    pub task: Option<String>,
    pub closed: bool,
    pub status: Option<StatusBody>,
    /// Its steering inputs, oldest first.
    pub inputs: Vec<InputInfo>,
    /// This node's steady clock when the last item arrived (or the session was learnt of).
    pub last_item_ms: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct TaskInfo {
    pub id: String,
    pub author: String,
    pub to: Vec<String>,
    pub external: Option<String>,
    pub open: bool,
    pub seq: u64,
    pub ask: Option<String>,
    /// P-15: fixed at creation, never shrinks.
    pub guarded_for: Vec<String>,
    /// The assignees that accepted and did not finish (Z2).
    pub accepted: Vec<String>,
}

/// A `session.input` as its session's host reads it.
#[derive(Clone, Debug)]
pub(crate) struct InputInfo {
    pub id: String,
    pub seq: u64,
    pub author: String,
    pub text: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Open,
    Claimed,
    Done,
}

#[derive(Clone, Debug)]
pub(crate) struct EffectInfo {
    pub id: String,
    pub proposer: String,
    pub executor: String,
    pub action: String,
    pub plan_digest: String,
    pub cost: Option<(String, u64)>,
    pub expires_at: Option<String>,
    pub approvals: Vec<String>,
    pub seq: u64,
    repeatable: bool,
    phase: Phase,
    /// The plan from the proposal's content, as JSON text.
    pub plan: Option<String>,
    /// The task it is charged to.
    pub task: Option<String>,
    /// Claims so far, and the latest.
    pub claims: u64,
    pub last_claim: Option<String>,
    /// The node hashed the plan: `sha256(JCS(plan))` is the digest.
    pub plan_ok: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct LeaseInfo {
    pub id: String,
    pub actor: String,
    pub paths: Vec<String>,
    pub from_secs: i64,
    pub until_secs: i64,
}

/// Everything about one followed space.
pub(crate) struct SpaceModel {
    pub space: String,
    pub home: String,
    pub link: SpaceLink,
    pub link_error: Option<String>,
    pub head: u64,
    pub roles: BTreeMap<String, Role>,
    pub sessions: BTreeMap<String, SessionInfo>,
    pub tasks: BTreeMap<String, TaskInfo>,
    pub effects: BTreeMap<String, EffectInfo>,
    pub leases: BTreeMap<String, LeaseInfo>,
    pub unit: Option<String>,
    pub outbox_pending: u32,
    pub signals: u64,
    pub outbox: Vec<crate::views::OutboxItem>,
    pub fresh: Freshness,
    /// The actor this device acts as.
    pub me: String,
    /// The approval rules of the policy: action (or `*`) and approvers.
    pub rules: Vec<(String, Vec<String>)>,
    /// Who may steer sessions, by the policy.
    pub steer: Option<SessionLevel>,
    /// The policy's `launchWindow` in seconds.
    pub launch_window: u64,
    /// Actors that chose guarding (sticky until `agent.unguard`), and whether owners force non-humans.
    pub guarded: BTreeSet<String>,
    pub force_guarded: bool,
    /// Counts every change a view could show; a view recomputes when it moves.
    pub rev: u64,
    claims: BTreeMap<String, String>,
    time_sig: u64,
    overlap_cache: Option<(u64, Arc<Vec<OverlapRow>>)>,
}

/// A span of time in words, for a reason that names a threshold.
fn span(ms: u64) -> String {
    let (n, unit) = if ms.is_multiple_of(60_000) {
        (ms / 60_000, "minute")
    } else if ms.is_multiple_of(1000) {
        (ms / 1000, "second")
    } else {
        (ms, "millisecond")
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

/// Whether `a` and `b` name overlapping paths: equal, or one is a directory prefix (ending in `/`) of the other.
pub(crate) fn paths_overlap(a: &str, b: &str) -> bool {
    a == b || (a.ends_with('/') && b.starts_with(a)) || (b.ends_with('/') && a.starts_with(b))
}

/// Whether line ranges `[start, len]` and `[start, len]` intersect. A zero length is a point.
fn ranges_intersect(a: (u32, u32), b: (u32, u32)) -> bool {
    let (a0, a1) = (u64::from(a.0), u64::from(a.0) + u64::from(a.1.max(1)));
    let (b0, b1) = (u64::from(b.0), u64::from(b.0) + u64::from(b.1.max(1)));
    a0 < b1 && b0 < a1
}

impl SpaceModel {
    pub fn new(space: &str, home: &str, fresh: Freshness, me: &str) -> SpaceModel {
        SpaceModel {
            space: space.to_owned(),
            home: home.to_owned(),
            link: SpaceLink::Connecting,
            link_error: None,
            head: 0,
            roles: BTreeMap::new(),
            sessions: BTreeMap::new(),
            tasks: BTreeMap::new(),
            effects: BTreeMap::new(),
            leases: BTreeMap::new(),
            unit: None,
            outbox_pending: 0,
            signals: 0,
            outbox: Vec::new(),
            fresh,
            me: me.to_owned(),
            rules: Vec::new(),
            steer: None,
            launch_window: 30,
            guarded: BTreeSet::new(),
            force_guarded: false,
            rev: 1,
            claims: BTreeMap::new(),
            time_sig: 0,
            overlap_cache: None,
        }
    }

    pub fn bump(&mut self) {
        self.rev += 1;
    }

    fn set_policy(&mut self, policy: PolicyDoc) {
        self.unit = policy.budget.map(|b| b.unit);
        self.rules = policy
            .approvals
            .into_iter()
            .map(|r| (r.action, r.approvers))
            .collect();
        self.steer = Some(policy.sessions.steer);
        self.launch_window = policy.launch_window;
        self.force_guarded = policy.sessions.force_guarded == Some(true);
    }

    fn is_owner(&self, actor: &str) -> bool {
        self.roles.get(actor) == Some(&Role::Owner)
    }

    fn is_member(&self, actor: &str) -> bool {
        matches!(self.roles.get(actor), Some(Role::Owner | Role::Member))
    }

    /// The node's reading of `sessions.steer` for the device's actor and one session.
    fn can_steer(&self, s: &SessionInfo) -> bool {
        if s.closed {
            return false;
        }
        let by_role = match self.steer {
            Some(SessionLevel::Guest) => self.roles.get(&self.me).is_some_and(|r| r.reads()),
            Some(SessionLevel::Member) => self.is_member(&self.me),
            Some(SessionLevel::Owner) | None => self.is_owner(&self.me),
            Some(SessionLevel::Nobody) => false,
        };
        by_role || s.actor == self.me
    }

    /// Z2: an actor that can no longer read carries no task.
    fn drop_carry(&mut self, actor: &str) {
        for t in self.tasks.values_mut() {
            t.accepted.retain(|a| a != actor);
        }
    }

    /// Whether `actor` is forced into guarding: the owners' `forceGuarded` applies to non-humans, which the
    /// node cannot tell from profiles, so it exempts the device's own person.
    fn forced(&self, actor: &str) -> bool {
        self.force_guarded && actor != self.me
    }

    /// P-15, Z1: the agents whose guard `actor` answers to: itself if it chose guarding or is forced, then
    /// the `guardedFor` of the tasks it accepted, in the order the tasks were created; each once.
    pub fn guards_of(&self, actor: &str) -> Vec<String> {
        let mut out = Vec::new();
        if self.forced(actor) || self.guarded.contains(actor) {
            out.push(actor.to_owned());
        }
        let mut mine: Vec<&TaskInfo> = self
            .tasks
            .values()
            .filter(|t| t.accepted.iter().any(|a| a == actor))
            .collect();
        mine.sort_by_key(|t| t.seq);
        for t in mine {
            for g in &t.guarded_for {
                if !out.contains(g) {
                    out.push(g.clone());
                }
            }
        }
        out
    }

    /// The approval rule that applies to `action`, if the policy has one (the first match wins, P-08).
    fn rule_for(&self, action: &str) -> Option<&(String, Vec<String>)> {
        self.rules.iter().find(|(a, _)| a == "*" || a == action)
    }

    // ───────────────────────── folding ─────────────────────────

    /// Folds one verified record. Returns the sessions it opened, so the node can watch their streams.
    pub fn apply_record(&mut self, rec: &LogRecord, now: &Now) -> Vec<String> {
        let mut opened = Vec::new();
        let seq = rec.receipt.seq;
        if seq <= self.head {
            return opened;
        }
        self.head = seq;
        self.bump();
        let Ok(entry) = PolicyEntry::from_entry(&rec.entry) else {
            return opened;
        };
        let id = rec.receipt.id.clone();
        let author = entry.author.clone();
        match entry.body {
            Body::SpaceCreate(b) => {
                if !self.space.starts_with("profile:") {
                    self.roles.insert(author, Role::Owner);
                }
                self.set_policy(b.policy);
            }
            Body::PolicySet(b) => self.set_policy(b.policy),
            Body::MemberSet(b) => match b.role.role() {
                Some(r) => {
                    if !r.reads() {
                        self.drop_carry(&b.actor);
                    }
                    self.roles.insert(b.actor, r);
                }
                None => {
                    self.drop_carry(&b.actor);
                    self.roles.remove(&b.actor);
                }
            },
            Body::Task(t) => {
                // P-15: the parent's list, then the guards of the author; each once.
                let mut guarded_for = t
                    .parent
                    .as_ref()
                    .and_then(|p| self.tasks.get(p))
                    .map(|p| p.guarded_for.clone())
                    .unwrap_or_default();
                for g in self.guards_of(&author) {
                    if !guarded_for.contains(&g) {
                        guarded_for.push(g);
                    }
                }
                let accepted = if t.to.contains(&author) {
                    vec![author.clone()]
                } else {
                    Vec::new()
                };
                self.tasks.insert(
                    id.clone(),
                    TaskInfo {
                        id,
                        author,
                        to: t.to,
                        external: t.external,
                        open: true,
                        guarded_for,
                        accepted,
                        seq,
                        ask: rec
                            .content
                            .as_ref()
                            .and_then(|c| c.get("ask"))
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    },
                );
            }
            Body::TaskUpdate(u) => {
                if let Some(t) = self.tasks.get_mut(&u.task) {
                    if u.state == TaskState::Accepted && !t.accepted.contains(&author) {
                        t.accepted.push(author.clone());
                    }
                    if u.state.is_final() {
                        // An assignee's own final update ends what it carries; anyone else's, all of it.
                        if t.to.contains(&author) {
                            t.accepted.retain(|a| *a != author);
                        } else {
                            t.accepted.clear();
                        }
                        t.open = false;
                    }
                }
            }
            Body::TaskCancel(c) => {
                if let Some(t) = self.tasks.get_mut(&c.task) {
                    t.open = false;
                }
            }
            Body::SessionOpen(s) => {
                if s.steer_mode == han_core::policy::SteerMode::Guarded {
                    self.guarded.insert(author.clone());
                }
                opened.push(id.clone());
                self.sessions.insert(
                    id.clone(),
                    SessionInfo {
                        id,
                        actor: author,
                        opened: rec.receipt.time.clone(),
                        task: s.task,
                        closed: false,
                        status: None,
                        inputs: Vec::new(),
                        last_item_ms: now.ms,
                    },
                );
            }
            Body::SessionClose(c) => {
                if let Some(s) = self.sessions.get_mut(&c.session) {
                    s.closed = true;
                }
            }
            Body::Propose(p) => {
                let plan_json = rec.content.as_ref().and_then(|c| c.get("plan"));
                let plan = plan_json
                    .and_then(|p| han_core::json::canonicalize(p).ok())
                    .and_then(|b| String::from_utf8(b).ok());
                // DAT-11: the node hashes the plan itself; a face never trusts a plan without this.
                let plan_ok = plan_json
                    .and_then(|v| han_core::ids::json_digest(v).ok())
                    .is_some_and(|d| d == p.plan_digest);
                self.effects.insert(
                    id.clone(),
                    EffectInfo {
                        id,
                        proposer: author,
                        executor: p.executor,
                        action: p.action,
                        plan_digest: p.plan_digest,
                        cost: p.cost.map(|c| (c.unit, c.amount)),
                        expires_at: p.expires_at,
                        approvals: Vec::new(),
                        seq,
                        repeatable: p.repeatable,
                        phase: Phase::Open,
                        plan,
                        plan_ok,
                        task: p.task,
                        claims: 0,
                        last_claim: None,
                    },
                );
            }
            Body::Approve(a) => {
                if let Some(e) = self.effects.get_mut(&a.proposal)
                    && !e.approvals.contains(&author)
                {
                    e.approvals.push(author);
                }
            }
            Body::EffectCancel(c) => {
                if let Some(e) = self.effects.get_mut(&c.proposal)
                    && e.phase == Phase::Open
                {
                    e.phase = Phase::Done;
                }
            }
            Body::Claim(c) => {
                if let Some(e) = self.effects.get_mut(&c.proposal) {
                    e.phase = Phase::Claimed;
                    e.approvals.clear(); // a claim consumes the approvals it stands on
                    e.claims += 1;
                    e.last_claim = Some(id.clone());
                    self.claims.insert(id, c.proposal);
                }
            }
            Body::Outcome(o) => {
                if let Some(p) = self.claims.get(&o.claim)
                    && let Some(e) = self.effects.get_mut(p)
                {
                    e.phase = match (o.result, o.started) {
                        // P-09: not started returns the effect to open (cancelled if a cancel was
                        // asked for, which this fold does not track: it shows open until the next
                        // claim is refused).
                        (EffectResult::Failed | EffectResult::Cancelled, Started::No) => {
                            Phase::Open
                        }
                        (EffectResult::Failed, _) if e.repeatable => Phase::Open,
                        (EffectResult::Unknown, _) if e.repeatable => Phase::Open,
                        _ => Phase::Done,
                    };
                }
            }
            Body::SessionInput(i) => {
                if let Some(s) = self.sessions.get_mut(&i.session) {
                    s.inputs.push(InputInfo {
                        id: id.clone(),
                        seq,
                        author: author.clone(),
                        text: rec
                            .content
                            .as_ref()
                            .and_then(|c| c.get("text"))
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    });
                }
            }
            Body::AgentUnguard(u) => {
                self.guarded.remove(&u.agent);
            }
            Body::Resolve(r) => {
                if let Some(e) = self.effects.get_mut(&r.proposal) {
                    e.phase = Phase::Done;
                }
            }
            Body::Extension(v) if entry.kind == LEASE_KIND => {
                let paths: Vec<String> = v
                    .get("paths")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|p| p.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                let ttl = i64::try_from(v["ttl"].as_u64().unwrap_or(0)).unwrap_or(0);
                if let Some(from) = grammar::epoch_seconds(&rec.receipt.time)
                    && !paths.is_empty()
                {
                    self.leases.insert(
                        id.clone(),
                        LeaseInfo {
                            id,
                            actor: author,
                            paths,
                            from_secs: from,
                            until_secs: from.saturating_add(ttl),
                        },
                    );
                }
            }
            _ => {}
        }
        opened
    }

    /// Takes a live status item. False when it is not one this device may show: the session is unknown, or
    /// the item names another session.
    pub fn apply_status(&mut self, session: &str, status: StatusBody, now: &Now) -> bool {
        let Some(s) = self.sessions.get_mut(session) else {
            return false;
        };
        if status.session != session {
            return false;
        }
        s.last_item_ms = now.ms;
        // The item with the highest `seq` wins; an older one only shows the host is alive.
        if s.status.as_ref().is_none_or(|old| status.seq > old.seq) {
            s.status = Some(status);
        }
        self.bump();
        true
    }

    // ───────────────────────── derived facts ─────────────────────────

    /// The state of a session with silence applied (spec/profiles/han.status-1.md; PLAN 6.4).
    pub fn derived_state(&self, s: &SessionInfo, now: &Now) -> SessionState {
        let claimed = match s.status.as_ref().map(|st| st.state) {
            None | Some(SessionState::Stale) => SessionState::Idle, // `stale` is never claimed
            Some(st) => st,
        };
        if s.closed {
            return if claimed == SessionState::Failed {
                SessionState::Failed
            } else {
                SessionState::Done
            };
        }
        if claimed.is_terminal() {
            return claimed;
        }
        let silent = now.ms.saturating_sub(s.last_item_ms);
        if silent >= self.fresh.failed_ms {
            SessionState::Failed
        } else if silent >= self.fresh.stale_ms {
            SessionState::Stale
        } else {
            claimed
        }
    }

    /// Why a session is `failed` or `blocked`, in words (Q-DAT-12): what its host said, or how long it may be
    /// silent. The text holds no elapsed time, so the row does not change with every tick.
    pub fn state_reason(&self, s: &SessionInfo, now: &Now) -> Option<String> {
        let st = s.status.as_ref();
        let said = || {
            st.and_then(|x| {
                x.summary
                    .clone()
                    .filter(|t| !t.is_empty())
                    .or_else(|| Some(x.step.clone()).filter(|t| !t.is_empty()))
            })
        };
        match self.derived_state(s, now) {
            SessionState::Failed => {
                let claimed_terminal = st.is_some_and(|x| x.state.is_terminal());
                let silent = now.ms.saturating_sub(s.last_item_ms) >= self.fresh.failed_ms;
                if !s.closed && !claimed_terminal && silent {
                    Some(format!("no status for {}", span(self.fresh.failed_ms)))
                } else {
                    said()
                }
            }
            SessionState::Blocked => said(),
            _ => None,
        }
    }

    fn live(&self, s: &SessionInfo, now: &Now) -> bool {
        !s.closed && !self.derived_state(s, now).is_terminal()
    }

    fn lease_active(l: &LeaseInfo, now: &Now) -> bool {
        l.from_secs <= now.secs && now.secs < l.until_secs
    }

    pub(crate) fn effect_waiting(e: &EffectInfo, now: &Now) -> bool {
        e.phase == Phase::Open
            && e.expires_at
                .as_deref()
                .and_then(grammar::epoch_seconds)
                .is_none_or(|t| now.secs < t)
    }

    pub fn issue_of(&self, s: &SessionInfo) -> Option<String> {
        s.status
            .as_ref()
            .and_then(|st| st.issue.clone())
            .or_else(|| {
                s.task
                    .as_ref()
                    .and_then(|t| self.tasks.get(t))
                    .and_then(|t| t.external.clone())
            })
    }

    fn person_of(&self, s: &SessionInfo) -> String {
        s.status
            .as_ref()
            .and_then(|st| st.delegate_of.clone())
            .or_else(|| {
                s.task
                    .as_ref()
                    .and_then(|t| self.tasks.get(t))
                    .map(|t| t.author.clone())
            })
            .unwrap_or_else(|| s.actor.clone())
    }

    /// Hashes what changes with time alone: derived states, active leases, waiting approvals. When it moves,
    /// views are stale even though no entry arrived.
    pub fn time_signature(&self, now: &Now) -> u64 {
        let mut h = DefaultHasher::new();
        for (id, s) in &self.sessions {
            id.hash(&mut h);
            self.derived_state(s, now).name().hash(&mut h);
        }
        for (id, l) in &self.leases {
            if Self::lease_active(l, now) {
                id.hash(&mut h);
            }
        }
        for (id, e) in &self.effects {
            if Self::effect_waiting(e, now) {
                id.hash(&mut h);
            }
        }
        h.finish()
    }

    /// Remembers what time-driven facts look like now, after something else changed the model.
    pub fn sync_time(&mut self, now: &Now) {
        self.time_sig = self.time_signature(now);
    }

    /// Recomputes the time signature; bumps `rev` if it moved. True if it did.
    pub fn tick(&mut self, now: &Now) -> bool {
        let sig = self.time_signature(now);
        if sig == self.time_sig {
            return false;
        }
        self.time_sig = sig;
        self.bump();
        true
    }

    // ───────────────────────── overlaps ─────────────────────────

    /// Tiers 1 and 2 of PLAN 6.6, as rows ordered by key. Cached per `rev`.
    pub fn overlaps(&mut self, now: &Now) -> Arc<Vec<OverlapRow>> {
        if let Some((rev, rows)) = &self.overlap_cache
            && *rev == self.rev
        {
            return Arc::clone(rows);
        }
        let mut rows = Vec::new();
        self.issue_overlaps(now, &mut rows);
        self.lease_overlaps(now, &mut rows);
        self.file_overlaps(now, &mut rows);
        rows.sort_by(|a, b| a.key.cmp(&b.key));
        let rows = Arc::new(rows);
        self.overlap_cache = Some((self.rev, Arc::clone(&rows)));
        rows
    }

    /// Tier 1: two actors claim (open task with an `external`) or work (live session) the same issue.
    fn issue_overlaps(&self, now: &Now, out: &mut Vec<OverlapRow>) {
        // issue -> (actor -> session, number of independent sources)
        let mut by_issue: BTreeMap<String, (BTreeMap<String, Option<String>>, usize)> =
            BTreeMap::new();
        for t in self.tasks.values().filter(|t| t.open) {
            let Some(issue) = &t.external else { continue };
            let entry = by_issue.entry(issue.clone()).or_default();
            entry.1 += 1;
            for a in &t.to {
                entry.0.entry(a.clone()).or_insert(None);
            }
        }
        for s in self.sessions.values().filter(|s| self.live(s, now)) {
            let Some(issue) = self.issue_of(s) else {
                continue;
            };
            let entry = by_issue.entry(issue.clone()).or_default();
            // A session that works a task already counted is the same claim.
            let covered = s
                .task
                .as_ref()
                .and_then(|t| self.tasks.get(t))
                .is_some_and(|t| t.open && t.external.as_deref() == Some(issue.as_str()));
            if !covered {
                entry.1 += 1;
            }
            entry.0.insert(s.actor.clone(), Some(s.id.clone()));
        }
        for (issue, (actors, sources)) in by_issue {
            if actors.len() < 2 || sources < 2 {
                continue;
            }
            let parties: Vec<OverlapParty> = actors
                .into_iter()
                .map(|(actor, session)| OverlapParty { actor, session })
                .collect();
            let names: Vec<&str> = parties.iter().map(|p| p.actor.as_str()).collect();
            out.push(OverlapRow {
                key: format!("1a|{issue}"),
                space: self.space.clone(),
                kind: OverlapKind::IssueClaim,
                tier: 1,
                subject: issue.clone(),
                detail: format!("{} are on {issue}", names.join(" and ")),
                parties,
                files: Vec::new(),
                claim: Some(OverlapClaim {
                    kind: ClaimKind::Issue,
                    reference: issue.clone(),
                    expires_at: None,
                }),
                conflict: None,
            });
        }
    }

    /// Tier 1: two live leases of different actors cover overlapping paths.
    fn lease_overlaps(&self, now: &Now, out: &mut Vec<OverlapRow>) {
        let live: Vec<&LeaseInfo> = self
            .leases
            .values()
            .filter(|l| Self::lease_active(l, now))
            .collect();
        for (i, a) in live.iter().enumerate() {
            for b in &live[i + 1..] {
                if a.actor == b.actor {
                    continue;
                }
                for (pa, pb) in a
                    .paths
                    .iter()
                    .flat_map(|pa| b.paths.iter().map(move |pb| (pa, pb)))
                {
                    if !paths_overlap(pa, pb) {
                        continue;
                    }
                    // The path the other lease reaches into: the shorter of the two.
                    let subject = if pa.len() <= pb.len() { pa } else { pb };
                    let mut parties = vec![
                        OverlapParty {
                            actor: a.actor.clone(),
                            session: None,
                        },
                        OverlapParty {
                            actor: b.actor.clone(),
                            session: None,
                        },
                    ];
                    parties.sort();
                    out.push(OverlapRow {
                        key: format!("1b|{subject}|{}|{}", parties[0].actor, parties[1].actor),
                        space: self.space.clone(),
                        kind: OverlapKind::PathLease,
                        tier: 1,
                        subject: subject.clone(),
                        detail: format!("{} leases {pa}, {} leases {pb}", a.actor, b.actor),
                        parties,
                        files: Vec::new(),
                        claim: Some(OverlapClaim {
                            kind: ClaimKind::Lease,
                            reference: subject.clone(),
                            expires_at: Some(format_time(a.until_secs.min(b.until_secs))),
                        }),
                        conflict: None,
                    });
                }
            }
        }
    }

    /// Tier 2: two live sessions changed the same file, or overlapping line ranges of it.
    fn file_overlaps(&self, now: &Now, out: &mut Vec<OverlapRow>) {
        type Touch<'a> = (&'a SessionInfo, &'a Vec<(u32, u32)>);
        let mut by_path: BTreeMap<(Option<&str>, &str), Vec<Touch<'_>>> = BTreeMap::new();
        for s in self.sessions.values().filter(|s| self.live(s, now)) {
            let Some(st) = &s.status else { continue };
            for f in &st.files {
                by_path
                    .entry((st.repo.as_deref(), f.path.as_str()))
                    .or_default()
                    .push((s, &f.hunks));
            }
        }
        for ((_, path), touches) in by_path {
            for (i, (sa, ha)) in touches.iter().enumerate() {
                for (sb, hb) in &touches[i + 1..] {
                    let hit = ha
                        .iter()
                        .flat_map(|x| hb.iter().map(move |y| (*x, *y)))
                        .find(|(x, y)| ranges_intersect(*x, *y));
                    // A file listed with no ranges is changed as a whole: it meets any range.
                    let whole = ha.is_empty() || hb.is_empty();
                    let (kind, detail) = match hit {
                        Some((x, y)) => (
                            OverlapKind::HunkRange,
                            format!("{path} lines {}+{} and {}+{}", x.0, x.1, y.0, y.1),
                        ),
                        None if whole => (OverlapKind::SameFile, format!("{path} changed by both")),
                        None => (
                            OverlapKind::SameFile,
                            format!("{path} changed by both, in different ranges"),
                        ),
                    };
                    let mut parties = vec![
                        OverlapParty {
                            actor: sa.actor.clone(),
                            session: Some(sa.id.clone()),
                        },
                        OverlapParty {
                            actor: sb.actor.clone(),
                            session: Some(sb.id.clone()),
                        },
                    ];
                    parties.sort_by(|a, b| a.session.cmp(&b.session));
                    // Side A is the first party: the ranges follow the order of the parties.
                    let (ra, rb) = if sa.id <= sb.id { (ha, hb) } else { (hb, ha) };
                    out.push(OverlapRow {
                        key: format!(
                            "2|{path}|{}|{}",
                            parties[0].session.as_deref().unwrap_or(""),
                            parties[1].session.as_deref().unwrap_or("")
                        ),
                        space: self.space.clone(),
                        kind,
                        tier: 2,
                        subject: path.to_owned(),
                        detail,
                        parties,
                        files: vec![OverlapFile {
                            path: path.to_owned(),
                            ranges_a: ra.to_vec(),
                            ranges_b: rb.to_vec(),
                        }],
                        claim: None,
                        conflict: None,
                    });
                }
            }
        }
    }

    // ───────────────────────── rows ─────────────────────────

    /// Who session `s` overlaps with, strongest first: one entry per other party, folded over the overlaps
    /// (`touching`, indices into `overlaps`) that name it.
    fn overlap_summaries(
        &self,
        s: &SessionInfo,
        touching: &[usize],
        overlaps: &[OverlapRow],
    ) -> Vec<OverlapSummary> {
        let mut out: BTreeMap<(String, String), OverlapSummary> = BTreeMap::new();
        for &oi in touching {
            let o = &overlaps[oi];
            for p in &o.parties {
                let me = p.session.as_deref() == Some(s.id.as_str())
                    || (p.session.is_none() && p.actor == s.actor)
                    || (o.kind == OverlapKind::IssueClaim && p.actor == s.actor);
                if me {
                    continue;
                }
                let other = p.session.as_ref().and_then(|id| self.sessions.get(id));
                let e = out
                    .entry((p.session.clone().unwrap_or_default(), p.actor.clone()))
                    .or_insert_with(|| OverlapSummary {
                        session: p.session.clone(),
                        agent: p.actor.clone(),
                        person: other.map_or_else(|| p.actor.clone(), |x| self.person_of(x)),
                        issue: other.and_then(|x| self.issue_of(x)).or_else(|| {
                            (o.kind == OverlapKind::IssueClaim).then(|| o.subject.clone())
                        }),
                        tier: o.tier,
                        kind: o.kind,
                        files: Vec::new(),
                    });
                if o.kind > e.kind {
                    e.kind = o.kind;
                    e.tier = o.tier;
                }
                let paths: Vec<String> = match o.kind {
                    OverlapKind::IssueClaim => Vec::new(),
                    OverlapKind::PathLease => vec![o.subject.clone()],
                    _ => o.files.iter().map(|f| f.path.clone()).collect(),
                };
                for f in paths {
                    if !e.files.contains(&f) {
                        e.files.push(f);
                    }
                }
            }
        }
        let mut v: Vec<OverlapSummary> = out.into_values().collect();
        for o in &mut v {
            o.files.sort();
        }
        v.sort_by(|a, b| {
            b.tier
                .cmp(&a.tier)
                .then_with(|| a.agent.cmp(&b.agent))
                .then_with(|| a.session.cmp(&b.session))
        });
        v
    }

    pub fn board_rows(&mut self, group: Group, now: &Now) -> Vec<(Facets, crate::api::Row)> {
        let overlaps = self.overlaps(now);
        let mut kinds: BTreeMap<&str, BTreeSet<OverlapKind>> = BTreeMap::new();
        let mut actor_kinds: BTreeMap<&str, BTreeSet<OverlapKind>> = BTreeMap::new();
        // The overlaps (indices into `overlaps`) each session and each actor without a session is in.
        let mut by_session: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        let mut by_actor: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (oi, o) in overlaps.iter().enumerate() {
            for p in &o.parties {
                match (&p.session, o.kind) {
                    (Some(s), _) => {
                        kinds.entry(s).or_default().insert(o.kind);
                        by_session.entry(s).or_default().push(oi);
                    }
                    (None, k) => {
                        actor_kinds.entry(&p.actor).or_default().insert(k);
                        by_actor.entry(&p.actor).or_default().push(oi);
                    }
                }
            }
            // A claim names the actor and, when it has one, the session; both sides see the badge.
            if o.kind == OverlapKind::IssueClaim {
                for p in &o.parties {
                    actor_kinds.entry(&p.actor).or_default().insert(o.kind);
                    if p.session.is_some() {
                        by_actor.entry(&p.actor).or_default().push(oi);
                    }
                }
            }
        }
        let mut rows = Vec::new();
        for s in self.sessions.values() {
            let st = s.status.as_ref();
            let state = self.derived_state(s, now);
            let person = self.person_of(s);
            let issue = self.issue_of(s);
            let (group_label, key_prefix) = match group {
                Group::Issue => (
                    issue.clone().unwrap_or_default(),
                    issue.clone().unwrap_or_else(|| "~".into()),
                ),
                Group::Person => (person.clone(), person.clone()),
                Group::State => (
                    state.name().to_owned(),
                    format!("{}-{}", state as u8, state.name()),
                ),
            };
            let mut overlap_kinds: BTreeSet<OverlapKind> =
                kinds.get(s.id.as_str()).cloned().unwrap_or_default();
            if !s.closed
                && let Some(k) = actor_kinds.get(s.actor.as_str())
            {
                overlap_kinds.extend(k.iter().copied());
            }
            let files = st.map(|x| x.files.clone()).unwrap_or_default();
            let mut touching: Vec<usize> =
                by_session.get(s.id.as_str()).cloned().unwrap_or_default();
            if !s.closed
                && let Some(v) = by_actor.get(s.actor.as_str())
            {
                touching.extend(v.iter().copied());
            }
            touching.sort_unstable();
            touching.dedup();
            let overlap_with = self.overlap_summaries(s, &touching, &overlaps);
            let tier = overlap_with.iter().map(|o| o.tier).max().unwrap_or(0);
            let needs_me = !s.closed
                && matches!(state, SessionState::WaitingApproval | SessionState::Blocked)
                && (person == self.me || s.actor == self.me);
            let can_cancel =
                !s.closed && (s.actor == self.me || person == self.me || self.is_owner(&self.me));
            let can_steer = self.can_steer(s);
            let text = format!(
                "{} {} {} {} {} {} {} {}",
                s.actor,
                person,
                st.map_or("", |x| x.harness.as_str()),
                issue.as_deref().unwrap_or(""),
                st.and_then(|x| x.repo.as_deref()).unwrap_or(""),
                st.map_or("", |x| x.step.as_str()),
                st.and_then(|x| x.tool.as_deref()).unwrap_or(""),
                files
                    .iter()
                    .map(|f| f.path.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            let facets = Facets {
                state: Some(state),
                actors: vec![s.actor.clone(), person.clone()],
                issue: issue.clone(),
                closed: s.closed,
                needs_me,
                text,
                person: Some(person.clone()),
                agent: Some(s.actor.clone()),
                harness: st.map(|x| x.harness.clone()),
                repo: st.and_then(|x| x.repo.clone()),
                labels: Vec::new(),
                paths: files.iter().map(|f| f.path.clone()).collect(),
                overlap_tier: tier,
                mine: person == self.me || s.actor == self.me,
            };
            rows.push((
                facets,
                crate::api::Row::Board(BoardRow {
                    key: format!("{key_prefix}|{}", s.id),
                    space: self.space.clone(),
                    group: group_label,
                    session: s.id.clone(),
                    agent: s.actor.clone(),
                    person,
                    harness: st.map(|x| x.harness.clone()),
                    issue,
                    repo: st.and_then(|x| x.repo.clone()),
                    state,
                    step: st.map(|x| x.step.clone()).unwrap_or_default(),
                    tool: st.and_then(|x| x.tool.clone()),
                    files,
                    overlaps: overlap_kinds.into_iter().collect(),
                    overlap_with,
                    state_reason: self.state_reason(s, now),
                    tests: st.and_then(|x| x.tests),
                    tokens: st.and_then(|x| x.tokens),
                    summary: st.and_then(|x| x.summary.clone()),
                    sandbox: st.and_then(|x| x.sandbox),
                    last_active_ms: s.last_item_ms,
                    closed: s.closed,
                    needs_me,
                    can_steer,
                    can_cancel,
                }),
            ));
        }
        rows
    }

    pub fn session_row(&self, s: &SessionInfo, now: &Now) -> (Facets, crate::api::Row) {
        let state = self.derived_state(s, now);
        let task = s.task.as_ref().and_then(|t| self.tasks.get(t));
        let st = s.status.as_ref();
        let person = self.person_of(s);
        let facets = Facets {
            state: Some(state),
            actors: vec![s.actor.clone(), person.clone()],
            issue: self.issue_of(s),
            closed: s.closed,
            needs_me: false,
            text: format!(
                "{} {} {}",
                s.actor,
                st.map_or("", |x| x.step.as_str()),
                st.and_then(|x| x.summary.as_deref()).unwrap_or("")
            ),
            mine: person == self.me || s.actor == self.me,
            person: Some(person),
            agent: Some(s.actor.clone()),
            harness: st.map(|x| x.harness.clone()),
            repo: st.and_then(|x| x.repo.clone()),
            paths: st
                .map(|x| x.files.iter().map(|f| f.path.clone()).collect())
                .unwrap_or_default(),
            ..Facets::default()
        };
        (
            facets,
            crate::api::Row::Session(SessionRow {
                key: s.id.clone(),
                space: self.space.clone(),
                actor: s.actor.clone(),
                opened: s.opened.clone(),
                task: s.task.clone(),
                external: task.and_then(|t| t.external.clone()),
                closed: s.closed,
                state,
                state_reason: self.state_reason(s, now),
                status: s.status.clone(),
                last_active_ms: s.last_item_ms,
            }),
        )
    }

    pub fn overlap_rows(&mut self, now: &Now) -> Vec<(Facets, crate::api::Row)> {
        self.overlaps(now)
            .iter()
            .map(|o| {
                let facets = Facets {
                    state: None,
                    actors: o.parties.iter().map(|p| p.actor.clone()).collect(),
                    issue: (o.kind == OverlapKind::IssueClaim).then(|| o.subject.clone()),
                    closed: false,
                    needs_me: false,
                    text: format!("{} {}", o.subject, o.detail),
                    paths: o.files.iter().map(|f| f.path.clone()).collect(),
                    overlap_tier: o.tier,
                    ..Facets::default()
                };
                (facets, crate::api::Row::Overlap(o.clone()))
            })
            .collect()
    }

    /// Sessions, claims and leases that are on an issue or a path.
    pub fn who_rows(
        &self,
        target: &crate::api::Target,
        now: &Now,
    ) -> Vec<(Facets, crate::api::Row)> {
        let mut rows = Vec::new();
        let mut push =
            |actor: &str, session: Option<&SessionInfo>, via: &str, subject: &str, key: String| {
                let state = session.map(|s| self.derived_state(s, now));
                rows.push((
                    Facets {
                        state,
                        actors: vec![actor.to_owned()],
                        issue: session.and_then(|s| self.issue_of(s)),
                        closed: false,
                        needs_me: false,
                        text: format!("{actor} {via} {subject}"),
                        ..Facets::default()
                    },
                    crate::api::Row::WhoIsOn(WhoRow {
                        key,
                        space: self.space.clone(),
                        actor: actor.to_owned(),
                        session: session.map(|s| s.id.clone()),
                        via: via.to_owned(),
                        subject: subject.to_owned(),
                        state,
                    }),
                ));
            };
        match target {
            crate::api::Target::Issue { issue } => {
                for t in self.tasks.values().filter(|t| t.open) {
                    if t.external.as_deref() == Some(issue.as_str()) {
                        for a in &t.to {
                            push(a, None, "claim", issue, format!("claim|{}|{a}", t.id));
                        }
                    }
                }
                for s in self.sessions.values().filter(|s| self.live(s, now)) {
                    if self.issue_of(s).as_deref() == Some(issue.as_str()) {
                        push(
                            &s.actor,
                            Some(s),
                            "session",
                            issue,
                            format!("session|{}", s.id),
                        );
                    }
                }
            }
            crate::api::Target::Path { path } => {
                for l in self.leases.values().filter(|l| Self::lease_active(l, now)) {
                    if let Some(p) = l.paths.iter().find(|p| paths_overlap(p, path)) {
                        push(&l.actor, None, "lease", p, format!("lease|{}", l.id));
                    }
                }
                for s in self.sessions.values().filter(|s| self.live(s, now)) {
                    let hit = s
                        .status
                        .as_ref()
                        .and_then(|st| st.files.iter().find(|f| paths_overlap(&f.path, path)));
                    if let Some(f) = hit {
                        push(
                            &s.actor,
                            Some(s),
                            "session",
                            &f.path,
                            format!("session|{}", s.id),
                        );
                    }
                }
            }
        }
        rows
    }

    pub fn approval_rows(&self, actor: &str, now: &Now) -> Vec<(Facets, crate::api::Row)> {
        // Only a member who is not the proposer and has not approved yet is asked. Whether the approval
        // counts is Policy's call (P-08), made by the home when `EffectApprove` is appended.
        if !self
            .roles
            .get(actor)
            .is_some_and(|r| r.reads() && *r != Role::Guest)
        {
            return Vec::new();
        }
        self.effects
            .values()
            .filter(|e| Self::effect_waiting(e, now))
            .filter(|e| e.proposer != actor && !e.approvals.iter().any(|a| a == actor))
            .map(|e| {
                let rule = self.rule_for(&e.action);
                let approvers = match rule {
                    Some((_, who)) => who.clone(),
                    None => self
                        .roles
                        .iter()
                        .filter(|(_, r)| **r == Role::Owner)
                        .map(|(a, _)| a.clone())
                        .collect(),
                };
                let why_gated = match rule {
                    Some((a, who)) if a == "*" => format!(
                        "the space requires approval of every action by {}",
                        who.join(", ")
                    ),
                    Some((_, who)) => format!(
                        "the space requires approval of {} by {}",
                        e.action,
                        who.join(", ")
                    ),
                    None => format!(
                        "no approval rule names {}: the default is the operator's approval",
                        e.action
                    ),
                };
                let (target, summary) = plan_facts(e.plan.as_deref());
                (
                    Facets {
                        state: None,
                        actors: vec![e.proposer.clone(), e.executor.clone()],
                        issue: None,
                        closed: false,
                        needs_me: true,
                        text: format!(
                            "{} {} {} {}",
                            e.action,
                            target.as_deref().unwrap_or(""),
                            e.proposer,
                            e.executor
                        ),
                        ..Facets::default()
                    },
                    crate::api::Row::Approval(ApprovalRow {
                        key: format!("{}|{:012}|{}", self.space, e.seq, e.id),
                        space: self.space.clone(),
                        proposal: e.id.clone(),
                        proposer: e.proposer.clone(),
                        executor: e.executor.clone(),
                        action: e.action.clone(),
                        target,
                        summary,
                        plan: e.plan.clone(),
                        plan_digest: e.plan_digest.clone(),
                        digest_verified: e.plan_ok,
                        approvals: e.approvals.clone(),
                        cost: e.cost.clone(),
                        expires_at: e.expires_at.clone(),
                        seq: e.seq,
                        can_approve: true,
                        can_cancel: e.proposer == actor || self.is_owner(actor),
                        approvers,
                        why_gated,
                    }),
                )
            })
            .collect()
    }

    pub fn status_row(&self) -> (Facets, crate::api::Row) {
        let sessions =
            u32::try_from(self.sessions.values().filter(|s| !s.closed).count()).unwrap_or(u32::MAX);
        let members = u32::try_from(
            self.roles
                .values()
                .filter(|r| matches!(r, Role::Owner | Role::Member))
                .count(),
        )
        .unwrap_or(u32::MAX);
        (
            Facets {
                text: format!("{} {}", self.space, self.home),
                ..Facets::default()
            },
            crate::api::Row::SpaceStatus(SpaceStatusRow {
                key: self.space.clone(),
                space: self.space.clone(),
                home: self.home.clone(),
                link: self.link,
                head: self.head,
                sessions,
                members,
                outbox_pending: self.outbox_pending,
                signals: self.signals,
                error: self.link_error.clone(),
            }),
        )
    }

    /// One row per session that reported tokens, and a total (key `0`).
    pub fn cost_rows(&self, only: Option<&str>) -> Vec<(Facets, crate::api::Row)> {
        let mut rows = Vec::new();
        let (mut tin, mut tout, mut cost) = (0u64, 0u64, 0u64);
        for s in self.sessions.values() {
            if only.is_some_and(|o| o != s.id) {
                continue;
            }
            let Some(t) = s.status.as_ref().and_then(|st| st.tokens) else {
                continue;
            };
            tin += t.input;
            tout += t.out;
            cost += t.cost;
            rows.push((
                Facets {
                    actors: vec![s.actor.clone()],
                    text: s.actor.clone(),
                    ..Facets::default()
                },
                crate::api::Row::Cost(CostRow {
                    key: format!("1/{}", s.id),
                    space: self.space.clone(),
                    session: Some(s.id.clone()),
                    agent: Some(s.actor.clone()),
                    tokens_in: t.input,
                    tokens_out: t.out,
                    cost: t.cost,
                    unit: self.unit.clone(),
                }),
            ));
        }
        if only.is_none() {
            rows.push((
                Facets {
                    text: "total".into(),
                    ..Facets::default()
                },
                crate::api::Row::Cost(CostRow {
                    key: "0".into(),
                    space: self.space.clone(),
                    session: None,
                    agent: None,
                    tokens_in: tin,
                    tokens_out: tout,
                    cost,
                    unit: self.unit.clone(),
                }),
            ));
        }
        rows
    }
}

impl SpaceModel {
    /// The inputs of a session.
    pub fn input_rows(&self, session: &str) -> Option<Vec<(Facets, crate::api::Row)>> {
        let s = self.sessions.get(session)?;
        Some(
            s.inputs
                .iter()
                .map(|i| {
                    (
                        Facets {
                            actors: vec![i.author.clone()],
                            ..Facets::default()
                        },
                        crate::api::Row::Input(crate::api::InputRow {
                            key: format!("{:012}", i.seq),
                            space: self.space.clone(),
                            session: session.to_owned(),
                            id: i.id.clone(),
                            seq: i.seq,
                            author: i.author.clone(),
                            text: i.text.clone(),
                        }),
                    )
                })
                .collect(),
        )
    }

    /// The tasks of the space, optionally only one assignee's.
    pub fn task_rows(&self, assignee: Option<&str>) -> Vec<(Facets, crate::api::Row)> {
        self.tasks
            .values()
            .filter(|t| assignee.is_none_or(|a| t.to.iter().any(|x| x == a)))
            .map(|t| {
                (
                    Facets {
                        actors: t.to.iter().chain([&t.author]).cloned().collect(),
                        issue: t.external.clone(),
                        ..Facets::default()
                    },
                    crate::api::Row::Task(crate::api::TaskRow {
                        key: format!("{}|{:012}", self.space, t.seq),
                        space: self.space.clone(),
                        task: t.id.clone(),
                        seq: t.seq,
                        author: t.author.clone(),
                        to: t.to.clone(),
                        external: t.external.clone(),
                        ask: t.ask.clone(),
                        open: t.open,
                        guarded_for: t.guarded_for.clone(),
                        accepted: t.accepted.clone(),
                    }),
                )
            })
            .collect()
    }

    /// Whether `actor` is guarded, as this device folds the log. Forcing applies to non-humans; the node
    /// does not hold profiles, so it treats every actor without a human role-holder's device as forced when
    /// `forceGuarded` is on except the device's own person.
    pub fn guard_row(&self, actor: &str) -> (Facets, crate::api::Row) {
        let forced = self.forced(actor);
        let guards = self.guards_of(actor);
        (
            Facets {
                actors: vec![actor.to_owned()],
                ..Facets::default()
            },
            crate::api::Row::Guard(crate::api::GuardRow {
                key: format!("{}|{}", self.space, actor),
                space: self.space.clone(),
                actor: actor.to_owned(),
                guarded: !guards.is_empty(),
                forced,
                guards,
                head: self.head,
            }),
        )
    }

    /// The effects of the space as an executor reads them.
    pub fn effect_rows(
        &self,
        executor: Option<&str>,
        include_done: bool,
    ) -> Vec<(Facets, crate::api::Row)> {
        self.effects
            .values()
            .filter(|e| executor.is_none_or(|x| e.executor == x))
            .filter(|e| include_done || e.phase != Phase::Done)
            .map(|e| {
                let phase = match e.phase {
                    Phase::Open => "open",
                    Phase::Claimed => "claimed",
                    Phase::Done => "done",
                };
                (
                    Facets {
                        state: None,
                        actors: vec![e.proposer.clone(), e.executor.clone()],
                        issue: None,
                        closed: false,
                        needs_me: false,
                        text: format!("{} {} {}", e.action, e.proposer, e.executor),
                        ..Facets::default()
                    },
                    crate::api::Row::Effect(crate::api::EffectRow {
                        key: format!("{}|{:012}|{}", self.space, e.seq, e.id),
                        space: self.space.clone(),
                        proposal: e.id.clone(),
                        proposer: e.proposer.clone(),
                        executor: e.executor.clone(),
                        action: e.action.clone(),
                        plan_digest: e.plan_digest.clone(),
                        plan: e.plan.clone(),
                        task: e.task.clone(),
                        phase: phase.into(),
                        approvals: e.approvals.clone(),
                        claims: e.claims,
                        last_claim: e.last_claim.clone(),
                        launch_window: self.launch_window,
                        repeatable: e.repeatable,
                        cost: e.cost.clone(),
                        expires_at: e.expires_at.clone(),
                        seq: e.seq,
                        head: self.head,
                    }),
                )
            })
            .collect()
    }
}

/// Whether an effect still waits for approval at `now`.
pub(crate) fn effect_open(e: &EffectInfo, now: &Now) -> bool {
    SpaceModel::effect_waiting(e, now)
}

/// The target and the plain-valued arguments of a plan (JSON text), for an approval card. Only top-level
/// strings, numbers and booleans are listed; nested values are left to the plan itself.
fn plan_facts(plan: Option<&str>) -> (Option<String>, Vec<(String, String)>) {
    let Some(serde_json::Value::Object(plan)) = plan.and_then(|p| serde_json::from_str(p).ok())
    else {
        return (None, Vec::new());
    };
    let target = plan
        .get("target")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let summary = plan
        .get("args")
        .and_then(serde_json::Value::as_object)
        .map(|args| {
            args.iter()
                .filter_map(|(k, v)| {
                    let text = match v {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Number(n) => n.to_string(),
                        serde_json::Value::Bool(b) => b.to_string(),
                        _ => return None,
                    };
                    Some((k.clone(), text.chars().take(SUMMARY_TEXT).collect()))
                })
                .take(SUMMARY_FIELDS)
                .collect()
        })
        .unwrap_or_default();
    (target, summary)
}

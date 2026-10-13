//! What a device knows of one followed space, folded from the verified records and the live status items.
//!
//! This is the node's own projection for views, not Policy's fold: the home decided every entry already, and
//! the views only need who has what open. Authority questions stay with Policy, reached through han-core's
//! public API by the homes that decide and by the verifier (M4).

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use han_core::grammar;
use han_core::policy::{Body, EffectResult, PolicyDoc, PolicyEntry, Role, SessionLevel};
use han_core::{LogRecord, Value};

use crate::api::{
    ApprovalRow, BoardRow, CostRow, Facets, Group, OverlapKind, OverlapParty, OverlapRow,
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
    /// Counts every change a view could show; a view recomputes when it moves.
    pub rev: u64,
    claims: BTreeMap<String, String>,
    time_sig: u64,
    overlap_cache: Option<(u64, Arc<Vec<OverlapRow>>)>,
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
                    self.roles.insert(b.actor, r);
                }
                None => {
                    self.roles.remove(&b.actor);
                }
            },
            Body::Task(t) => {
                self.tasks.insert(
                    id.clone(),
                    TaskInfo {
                        id,
                        author,
                        to: t.to,
                        external: t.external,
                        open: true,
                    },
                );
            }
            Body::TaskUpdate(u) => {
                if let Some(t) = self.tasks.get_mut(&u.task)
                    && u.state.is_final()
                {
                    t.open = false;
                }
            }
            Body::TaskCancel(c) => {
                if let Some(t) = self.tasks.get_mut(&c.task) {
                    t.open = false;
                }
            }
            Body::SessionOpen(s) => {
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
                    self.claims.insert(id, c.proposal);
                }
            }
            Body::Outcome(o) => {
                if let Some(p) = self.claims.get(&o.claim)
                    && let Some(e) = self.effects.get_mut(p)
                {
                    e.phase = match o.result {
                        EffectResult::Failed if e.repeatable => Phase::Open,
                        _ => Phase::Done,
                    };
                }
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
                    });
                }
            }
        }
    }

    // ───────────────────────── rows ─────────────────────────

    pub fn board_rows(&mut self, group: Group, now: &Now) -> Vec<(Facets, crate::api::Row)> {
        let overlaps = self.overlaps(now);
        let mut kinds: BTreeMap<&str, BTreeSet<OverlapKind>> = BTreeMap::new();
        let mut actor_kinds: BTreeMap<&str, BTreeSet<OverlapKind>> = BTreeMap::new();
        for o in overlaps.iter() {
            for p in &o.parties {
                match (&p.session, o.kind) {
                    (Some(s), _) => {
                        kinds.entry(s).or_default().insert(o.kind);
                    }
                    (None, k) => {
                        actor_kinds.entry(&p.actor).or_default().insert(k);
                    }
                }
            }
            // A claim names the actor and, when it has one, the session; both sides see the badge.
            if o.kind == OverlapKind::IssueClaim {
                for p in &o.parties {
                    actor_kinds.entry(&p.actor).or_default().insert(o.kind);
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
                text,
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
        let facets = Facets {
            state: Some(state),
            actors: vec![s.actor.clone(), self.person_of(s)],
            issue: self.issue_of(s),
            closed: s.closed,
            text: format!(
                "{} {} {}",
                s.actor,
                st.map_or("", |x| x.step.as_str()),
                st.and_then(|x| x.summary.as_deref()).unwrap_or("")
            ),
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
                    text: format!("{} {}", o.subject, o.detail),
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
                        text: format!("{actor} {via} {subject}"),
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
                (
                    Facets {
                        state: None,
                        actors: vec![e.proposer.clone(), e.executor.clone()],
                        issue: None,
                        closed: false,
                        text: format!("{} {} {}", e.action, e.proposer, e.executor),
                    },
                    crate::api::Row::Approval(ApprovalRow {
                        key: format!("{}|{:012}|{}", self.space, e.seq, e.id),
                        space: self.space.clone(),
                        proposal: e.id.clone(),
                        proposer: e.proposer.clone(),
                        executor: e.executor.clone(),
                        action: e.action.clone(),
                        plan_digest: e.plan_digest.clone(),
                        approvals: e.approvals.clone(),
                        cost: e.cost.clone(),
                        expires_at: e.expires_at.clone(),
                        seq: e.seq,
                        can_approve: true,
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

/// Whether an effect still waits for approval at `now`.
pub(crate) fn effect_open(e: &EffectInfo, now: &Now) -> bool {
    SpaceModel::effect_waiting(e, now)
}

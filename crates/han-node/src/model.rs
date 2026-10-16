//! What a device knows of one followed space, folded from the verified records and the live status items.
//!
//! This is the node's own projection for views, not Policy's fold: the home decided every entry already, and
//! the views only need who has what open. Authority questions stay with Policy, reached through han-core's
//! public API by the homes that decide and by the verifier (M4).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use han_core::grammar;
use han_core::policy::{
    ActorKind as ProfileKind, Body, EffectResult, PolicyDoc, PolicyEntry, Role, SessionLevel,
    Started, TaskState,
};
use han_core::{LogRecord, Value};
use han_seq::time::format_time;

use crate::api::{
    AGENT_TEXT, ActorKind, AdmissionRow, AgentText, ApprovalRow, BoardRow, ClaimKind, CostRow,
    DeviceRow, DeviceState, Facets, Group, InputState, ItemBody, ItemRow, Needed, NeededGuard,
    OverlapClaim, OverlapFile, OverlapKind, OverlapParty, OverlapRow, OverlapSummary, RecordRow,
    Row, SUMMARY_FIELDS, SUMMARY_TEXT, SessionRights, SessionRow, SessionState, SpaceLink,
    SpaceStatusRow, StatusBody, WhoRow, masked_spans,
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
    /// This node saw the host of the session exit while the session was open, and says why it is over. Not in
    /// the log (the host's key signs the session's entries, not the node's): it is this device's reading, and
    /// a `session.close` that arrives later wins.
    pub ended: Option<String>,
    pub status: Option<StatusBody>,
    /// Its steering inputs, oldest first.
    pub inputs: Vec<InputInfo>,
    /// This node's steady clock when the last item arrived (or the session was learnt of).
    pub last_item_ms: u64,
    /// Its stream items.
    pub items: ItemLog,
    /// The entries of the log that are about it, oldest first.
    pub record: Vec<RecordInfo>,
}

/// How long the node waits before it reads again the profile of an agent that is not confirmed yet.
pub(crate) const PROFILE_RECHECK_MS: u64 = 30_000;

/// The most items a node keeps of one session; the oldest go first.
pub(crate) const MAX_ITEMS: usize = 10_000;

/// The most entries a node keeps in one session's record view.
pub(crate) const MAX_RECORD: usize = 10_000;

#[derive(Clone, Debug)]
pub(crate) struct ItemInfo {
    pub id: String,
    pub seq: u64,
    pub turn: Option<u32>,
    pub body: ItemBody,
    pub updated_ms: u64,
}

/// The items of a session, numbered in the order the node first saw them.
#[derive(Clone, Debug, Default)]
pub(crate) struct ItemLog {
    pub items: BTreeMap<u64, ItemInfo>,
    by_id: HashMap<String, u64>,
    next: u64,
}

impl ItemLog {
    /// Puts an item: new, or replacing the body of the one with this id. True if anything changed.
    pub fn put(&mut self, id: String, turn: Option<u32>, body: ItemBody, now_ms: u64) -> bool {
        if let Some(seq) = self.by_id.get(&id).copied()
            && let Some(it) = self.items.get_mut(&seq)
        {
            if it.body == body && it.turn == turn {
                return false;
            }
            it.body = body;
            it.turn = turn;
            it.updated_ms = now_ms;
            return true;
        }
        self.next += 1;
        let seq = self.next;
        self.by_id.insert(id.clone(), seq);
        self.items.insert(
            seq,
            ItemInfo {
                id,
                seq,
                turn,
                body,
                updated_ms: now_ms,
            },
        );
        while self.items.len() > MAX_ITEMS {
            if let Some((_, old)) = self.items.pop_first() {
                self.by_id.remove(&old.id);
            }
        }
        true
    }

    /// An input a turn started on has been seen by the agent.
    pub fn mark_seen(&mut self, entry: &str, now_ms: u64) -> bool {
        let Some(seq) = self.by_id.get(&format!("input:{entry}")).copied() else {
            return false;
        };
        match self.items.get_mut(&seq) {
            Some(ItemInfo {
                body: ItemBody::Input { state, .. },
                updated_ms,
                ..
            }) if *state == InputState::Recorded => {
                *state = InputState::Seen;
                *updated_ms = now_ms;
                true
            }
            _ => false,
        }
    }
}

/// An entry of the log that is about a session.
#[derive(Clone, Debug)]
pub(crate) struct RecordInfo {
    pub seq: u64,
    pub entry_id: String,
    pub kind: String,
    pub author: String,
    pub at: String,
    pub summary: String,
    pub redacted: bool,
    pub redact_reason: Option<String>,
    pub effect: Option<String>,
}

/// What an actor's profile says (read from its home).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileFacts {
    /// A person, an agent or a service.
    pub kind: ProfileKind,
    /// The operator of an agent or a service.
    pub operator: Option<String>,
    /// The operator confirmed the actor in its profile space (P-03).
    pub confirmed: bool,
}

impl ProfileFacts {
    /// A person.
    pub fn human() -> ProfileFacts {
        ProfileFacts {
            kind: ProfileKind::Human,
            operator: None,
            confirmed: false,
        }
    }

    /// An agent run by `operator`, who has or has not confirmed it.
    pub fn agent(operator: &str, confirmed: bool) -> ProfileFacts {
        ProfileFacts {
            kind: ProfileKind::Agent,
            operator: Some(operator.to_owned()),
            confirmed,
        }
    }
}

/// A key of this device's identity, from its profile space.
#[derive(Clone, Debug)]
pub(crate) struct DeviceInfo {
    pub added_at: String,
    pub revoked_at: Option<String>,
}

/// What an entry did to a session's record.
struct Touch {
    session: String,
    summary: String,
    effect: Option<String>,
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
    /// Its budget (P-05): unit and amount.
    pub budget: Option<(String, u64)>,
    /// The final state that closed it (`done`, `failed`, `declined`, `cancelled`); `None` while open.
    pub closed_as: Option<&'static str>,
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
    /// The seq and time of the latest approval entry the open approvals stand on (cleared with them).
    pub approved: Option<(u64, String)>,
    /// What the latest claim consumed, returned by a not-started outcome (P-09).
    consumed: (Vec<String>, Option<(u64, String)>),
    pub seq: u64,
    repeatable: bool,
    phase: Phase,
    /// The plan from the proposal's content, as JSON text.
    pub plan: Option<String>,
    /// The digest of the body's change summary (P-17).
    pub change_digest: Option<String>,
    /// The body's change summary: the delta digest, and the `vcs.land` it reverts (A2).
    pub change_delta: Option<String>,
    pub change_revert_of: Option<String>,
    /// The risk tier of a `vcs.land`, derived at the proposal from the policy then in force, as Policy derives
    /// it (P-17): the change's tier, at least that of the change it reverts. `None` without `gates`.
    pub gate_tier: Option<u8>,
    /// The task it is charged to.
    pub task: Option<String>,
    /// Claims so far, and the latest.
    pub claims: u64,
    pub last_claim: Option<String>,
    /// The node hashed the plan: `sha256(JCS(plan))` is the digest.
    pub plan_ok: bool,
    /// The session of the proposer that was open when it proposed.
    pub session: Option<String>,
    /// Notes the approvers wrote, as `(author, text)`.
    pub notes: Vec<(String, String)>,
    /// How it ended, once its phase is `Done`.
    pub ended: Option<crate::api::EffectEnd>,
}

/// The `evidence` of a record's content, a `BlobRef`.
fn evidence_of(content: Option<&Value>) -> Option<crate::api::EvidenceRef> {
    let e = content?.get("evidence")?;
    Some(crate::api::EvidenceRef {
        hash: e.get("hash")?.as_str()?.to_owned(),
        size: e.get("size")?.as_u64()?,
        media_type: e.get("type")?.as_str()?.to_owned(),
    })
}

fn result_name(r: EffectResult) -> &'static str {
    match r {
        EffectResult::Succeeded => "succeeded",
        EffectResult::Failed => "failed",
        EffectResult::Cancelled => "cancelled",
        EffectResult::Unknown => "unknown",
    }
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
    /// The policy's budget: unit and amount.
    pub budget: Option<(String, u64)>,
    pub outbox_pending: u32,
    pub signals: u64,
    pub outbox: Vec<crate::views::OutboxItem>,
    pub fresh: Freshness,
    /// The actor this device acts as.
    pub me: String,
    /// The policy's semantic gates (P-17), from which a landing's tier is derived.
    pub gates: Option<han_core::policy::GatesDoc>,
    /// The approval rules of the policy: action (or `*`) and approvers.
    pub rules: Vec<(String, Vec<String>)>,
    /// The approval rules in full, from which Policy reports whether a proposer suffices (P-17).
    pub approval_rules: Vec<han_core::policy::ApprovalRule>,
    /// Who may steer sessions, by the policy.
    pub steer: Option<SessionLevel>,
    /// Who may watch and who may read sessions, by the policy.
    pub watch: Option<SessionLevel>,
    pub read: Option<SessionLevel>,
    /// Actors that asked to join and wait for an owner: the node's clock when the join arrived.
    pub pending: BTreeMap<String, u64>,
    /// What the home says about actors (`None`: it could not be read), and the actors still to ask about.
    pub profiles: BTreeMap<String, Option<ProfileFacts>>,
    pub wanted: BTreeSet<String>,
    /// The node's steady clock (ms) when each profile was read.
    pub profile_at: BTreeMap<String, u64>,
    /// The keys of the identity, when this is its profile space.
    pub devices: BTreeMap<String, DeviceInfo>,
    /// The policy's `launchWindow` in seconds.
    pub launch_window: u64,
    /// Actors that chose guarding (sticky until `agent.unguard`), and whether owners force non-humans.
    pub guarded: BTreeSet<String>,
    pub force_guarded: bool,
    /// In a profile space: the operator its profile names (§4.3), fixed when the space was created.
    pub profile_operator: Option<String>,
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
    /// The outbox entry that holds the space: the first one still pending, if its last try failed.
    pub fn blocking_entry(&self) -> Option<&crate::views::OutboxItem> {
        self.outbox
            .iter()
            .find(|i| i.state == "pending")
            .filter(|i| i.error.is_some())
    }

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
            budget: None,
            outbox_pending: 0,
            signals: 0,
            outbox: Vec::new(),
            fresh,
            me: me.to_owned(),
            gates: None,
            rules: Vec::new(),
            approval_rules: Vec::new(),
            steer: None,
            watch: None,
            read: None,
            pending: BTreeMap::new(),
            profiles: BTreeMap::new(),
            wanted: BTreeSet::new(),
            profile_at: BTreeMap::new(),
            devices: BTreeMap::new(),
            launch_window: 30,
            guarded: BTreeSet::new(),
            force_guarded: false,
            profile_operator: None,
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
        self.unit = policy.budget.as_ref().map(|b| b.unit.clone());
        self.budget = policy.budget.map(|b| (b.unit, b.amount));
        self.rules = policy
            .approvals
            .iter()
            .map(|r| (r.action.clone(), r.approvers.clone()))
            .collect();
        self.approval_rules = policy.approvals;
        self.gates = policy.gates;
        self.steer = Some(policy.sessions.steer);
        self.watch = Some(policy.sessions.watch);
        self.read = Some(policy.sessions.read);
        self.launch_window = policy.launch_window;
        self.force_guarded = policy.sessions.force_guarded == Some(true);
    }

    /// The tier of a `vcs.land` proposal under the policy in force now, as Policy derives it for the gate it
    /// pins (P-17): none without `gates`; a missing summary is tier 3; a revert is at least the tier of the
    /// change it reverts, and a revert of a change with no known tier is tier 3.
    fn land_tier(&self, action: &str, change: Option<&han_core::policy::Change>) -> Option<u8> {
        let gates = self.gates.as_ref().filter(|_| action == "vcs.land")?;
        let mut tier = change.map_or(3, |c| han_core::policy::change_tier(gates, c));
        if let Some(r) = change.and_then(|c| c.revert_of.as_ref())
            && let Some(of) = self.effects.get(r)
        {
            tier = tier.max(of.gate_tier.unwrap_or(3));
        }
        Some(tier)
    }

    /// The budget a session may spend: the budget of the task it works when that task has one (P-05), else
    /// the space's (P-06: a human's work is charged to the space). The unit is the space's either way.
    pub fn session_budget(&self, task: Option<&str>) -> Option<(String, u64)> {
        task.and_then(|t| self.tasks.get(t))
            .and_then(|t| t.budget.clone())
            .or_else(|| self.budget.clone())
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

    /// Whether the device's actor holds a session right at `level`.
    fn holds(&self, level: Option<SessionLevel>) -> bool {
        match level {
            Some(SessionLevel::Guest) => self.roles.get(&self.me).is_some_and(|r| r.reads()),
            Some(SessionLevel::Member) => self.is_member(&self.me),
            Some(SessionLevel::Owner) | None => self.is_owner(&self.me),
            Some(SessionLevel::Nobody) => false,
        }
    }

    /// What the device's actor may see of a session: the policy's `watch` and `read`, and always its own.
    pub fn rights(&self, s: &SessionInfo) -> SessionRights {
        let own = s.actor == self.me;
        SessionRights {
            watch: own || self.holds(self.watch),
            read: own || self.holds(self.read),
        }
    }

    /// The open session of `actor` that was opened last.
    fn open_session_of(&self, actor: &str) -> Option<String> {
        self.sessions
            .values()
            .filter(|s| s.actor == actor && !s.closed)
            .max_by(|a, b| (&a.opened, &a.id).cmp(&(&b.opened, &b.id)))
            .map(|s| s.id.clone())
    }

    /// The guarded agents an effect of `proposer` and `executor` involves (P-15), each once.
    fn guarded_agents(&self, proposer: &str, executor: &str) -> Vec<String> {
        let mut agents = self.guards_of(proposer);
        for g in self.guards_of(executor) {
            if !agents.contains(&g) {
                agents.push(g);
            }
        }
        agents
    }

    /// Asks for the profiles of the agents an effect involves, to say who must approve for them.
    fn want_guard_profiles(&mut self, proposer: &str, executor: &str) {
        for a in self.guarded_agents(proposer, executor) {
            if !self.profiles.contains_key(&a) {
                self.wanted.insert(a);
            }
        }
    }

    /// Who must approve for each guarded agent of an effect: its operator, if the home says it confirmed
    /// the agent (`None`: nobody can approve for the agent).
    fn effect_guards(&self, proposer: &str, executor: &str) -> Vec<(String, Option<String>)> {
        self.guarded_agents(proposer, executor)
            .into_iter()
            .map(|a| {
                let guard = self
                    .profiles
                    .get(&a)
                    .and_then(Option::as_ref)
                    .filter(|f| f.kind != ProfileKind::Human && f.confirmed)
                    .and_then(|f| f.operator.clone());
                (a, guard)
            })
            .collect()
    }

    /// Puts an item on a session's stream. True if it changed.
    pub fn push_item(
        &mut self,
        session: &str,
        id: String,
        turn: Option<u32>,
        body: ItemBody,
        now_ms: u64,
        activity: bool,
    ) -> bool {
        let Some(s) = self.sessions.get_mut(session) else {
            return false;
        };
        if let ItemBody::TurnStart {
            driver: crate::api::TurnDriver::Input,
            driver_entry: Some(e),
            ..
        } = &body
        {
            s.items.mark_seen(e, now_ms);
        }
        if activity {
            s.last_item_ms = now_ms;
        }
        let changed = s.items.put(id, turn, body, now_ms);
        if changed || activity {
            self.bump();
        }
        changed
    }

    /// An actor asked to join (the home's `join` signal). Nothing happens if it already holds a role.
    pub fn join_signal(&mut self, actor: &str, now_ms: u64) {
        if self.roles.contains_key(actor) {
            return;
        }
        self.pending.entry(actor.to_owned()).or_insert(now_ms);
        if !self.profiles.contains_key(actor) {
            self.wanted.insert(actor.to_owned());
        }
        self.bump();
    }

    /// What the home says about an actor, read at `now_ms`.
    pub fn set_profile(&mut self, actor: &str, facts: Option<ProfileFacts>, now_ms: u64) {
        self.wanted.remove(actor);
        self.profiles.insert(actor.to_owned(), facts);
        self.profile_at.insert(actor.to_owned(), now_ms);
        self.bump();
    }

    /// An agent that waits to be admitted may be confirmed by its operator any time: its profile is read
    /// again now and then until it is.
    pub fn recheck_profiles(&mut self, now_ms: u64) {
        for actor in self.pending.keys() {
            let unconfirmed = matches!(
                self.profiles.get(actor),
                Some(Some(f)) if f.kind != ProfileKind::Human && !f.confirmed
            );
            let age = now_ms.saturating_sub(self.profile_at.get(actor).copied().unwrap_or(0));
            if unconfirmed && age >= PROFILE_RECHECK_MS {
                self.wanted.insert(actor.clone());
            }
        }
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

    /// Whether the rule that applies to an effect of `action` lets the proposal itself satisfy E3 (P-17, round 33):
    /// what Policy reports, so that an executor asks for an approval where Policy does and not otherwise. It is
    /// judged under the policy in force now, as a claim is.
    pub fn proposer_suffices(&self, action: &str) -> bool {
        han_core::policy::proposer_suffices(&self.approval_rules, action)
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
        let mut touch: Option<Touch> = None;
        match entry.body {
            Body::SpaceCreate(b) => {
                if !self.space.starts_with("profile:") {
                    self.roles.insert(author, Role::Owner);
                } else if let Some(op) = b.profile.as_ref().and_then(|p| p.operator.as_ref()) {
                    self.profile_operator = Some(op.to_string());
                }
                if let Some(p) = &b.profile {
                    self.devices.insert(
                        p.key.key_id.clone(),
                        DeviceInfo {
                            added_at: rec.receipt.time.clone(),
                            revoked_at: None,
                        },
                    );
                }
                self.set_policy(b.policy);
            }
            Body::PolicySet(b) => self.set_policy(b.policy),
            Body::KeyAdd(k) => {
                self.devices.entry(k.key_id).or_insert(DeviceInfo {
                    added_at: rec.receipt.time.clone(),
                    revoked_at: None,
                });
            }
            Body::KeyRevoke(k) => {
                if let Some(d) = self.devices.get_mut(&k.key_id) {
                    d.revoked_at.get_or_insert_with(|| rec.receipt.time.clone());
                }
            }
            Body::MemberSet(b) => {
                self.pending.remove(&b.actor);
                match b.role.role() {
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
                }
            }
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
                        budget: t.budget.map(|b| (b.unit, b.amount)),
                        closed_as: None,
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
                        if t.open {
                            t.closed_as = u.state.final_name();
                        }
                        t.open = false;
                    }
                }
            }
            Body::TaskCancel(c) => {
                if let Some(t) = self.tasks.get_mut(&c.task) {
                    if t.open {
                        t.closed_as = Some("cancelled");
                    }
                    t.open = false;
                }
            }
            Body::SessionOpen(s) => {
                if s.steer_mode == han_core::policy::SteerMode::Guarded {
                    self.guarded.insert(author.clone());
                }
                opened.push(id.clone());
                touch = Some(Touch {
                    session: id.clone(),
                    summary: "opened the session".into(),
                    effect: None,
                });
                self.sessions.insert(
                    id.clone(),
                    SessionInfo {
                        id,
                        actor: author,
                        opened: rec.receipt.time.clone(),
                        task: s.task,
                        closed: false,
                        ended: None,
                        status: None,
                        inputs: Vec::new(),
                        last_item_ms: now.ms,
                        items: ItemLog::default(),
                        record: Vec::new(),
                    },
                );
            }
            Body::SessionClose(c) => {
                if let Some(s) = self.sessions.get_mut(&c.session) {
                    s.closed = true;
                }
                touch = Some(Touch {
                    session: c.session,
                    summary: "closed the session".into(),
                    effect: None,
                });
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
                let session = self.open_session_of(&author);
                if let Some(sid) = &session {
                    let item = ItemBody::Decision {
                        effect: id.clone(),
                        action: p.action.clone(),
                    };
                    self.push_item(sid, format!("decision:{id}"), None, item, now.ms, false);
                    touch = Some(Touch {
                        session: sid.clone(),
                        summary: format!("proposed {}", p.action),
                        effect: Some(id.clone()),
                    });
                }
                // The guards of the agents it involves are asked for, to say who must approve.
                self.want_guard_profiles(&author, &p.executor);
                let gate_tier = self.land_tier(&p.action, p.change.as_ref());
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
                        approved: None,
                        consumed: (Vec::new(), None),
                        seq,
                        repeatable: p.repeatable,
                        phase: Phase::Open,
                        plan,
                        change_digest: p.change.as_ref().map(han_core::policy::Change::digest),
                        change_delta: p.change.as_ref().map(|c| c.delta_digest.clone()),
                        change_revert_of: p.change.as_ref().and_then(|c| c.revert_of.clone()),
                        gate_tier,
                        plan_ok,
                        task: p.task,
                        claims: 0,
                        last_claim: None,
                        session,
                        notes: Vec::new(),
                        ended: None,
                    },
                );
            }
            Body::Approve(a) => {
                if let Some(e) = self.effects.get_mut(&a.proposal) {
                    touch = e.session.clone().map(|session| Touch {
                        session,
                        summary: "approved the proposal".into(),
                        effect: Some(a.proposal.clone()),
                    });
                    if !e.approvals.contains(&author) {
                        if let Some(note) = rec
                            .content
                            .as_ref()
                            .and_then(|c| c.get("note"))
                            .and_then(Value::as_str)
                        {
                            e.notes.push((author.clone(), note.to_owned()));
                        }
                        e.approvals.push(author);
                        e.approved = Some((seq, rec.receipt.time.clone()));
                    }
                }
            }
            Body::EffectCancel(c) => {
                if let Some(e) = self.effects.get_mut(&c.proposal) {
                    if e.phase == Phase::Open {
                        e.phase = Phase::Done;
                        e.ended = Some(crate::api::EffectEnd {
                            by: "cancel".into(),
                            result: None,
                            reason: rec
                                .content
                                .as_ref()
                                .and_then(|c| c.get("reason"))
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            evidence: evidence_of(rec.content.as_ref()),
                        });
                    }
                    touch = e.session.clone().map(|session| Touch {
                        session,
                        summary: "cancelled the proposal".into(),
                        effect: Some(c.proposal.clone()),
                    });
                }
            }
            Body::Claim(c) => {
                if let Some(e) = self.effects.get_mut(&c.proposal) {
                    e.phase = Phase::Claimed;
                    // A claim consumes the approvals it stands on; a not-started outcome returns them.
                    e.consumed = (std::mem::take(&mut e.approvals), e.approved.take());
                    e.notes.clear();
                    e.claims += 1;
                    e.last_claim = Some(id.clone());
                    touch = e.session.clone().map(|session| Touch {
                        session,
                        summary: "claimed the proposal".into(),
                        effect: Some(c.proposal.clone()),
                    });
                    self.claims.insert(id, c.proposal);
                }
            }
            Body::Outcome(o) => {
                if let Some(p) = self.claims.get(&o.claim)
                    && let Some(e) = self.effects.get_mut(p)
                {
                    touch = e.session.clone().map(|session| Touch {
                        session,
                        summary: "reported an outcome".into(),
                        effect: Some(p.clone()),
                    });
                    e.phase = match (o.result, o.started) {
                        // P-09: not started returns the effect to open (cancelled if a cancel was
                        // asked for, which this fold does not track: it shows open until the next
                        // claim is refused).
                        (EffectResult::Failed | EffectResult::Cancelled, Started::No) => {
                            let (back, approved) = std::mem::take(&mut e.consumed);
                            for a in back {
                                // A newer approval of the same author supersedes the returned one.
                                if !e.approvals.contains(&a) {
                                    e.approvals.push(a);
                                }
                            }
                            if e.approved.is_none() {
                                e.approved = approved;
                            }
                            Phase::Open
                        }
                        (EffectResult::Failed, _) if e.repeatable => Phase::Open,
                        (EffectResult::Unknown, _) if e.repeatable => Phase::Open,
                        _ => Phase::Done,
                    };
                    if e.phase == Phase::Done {
                        e.ended = Some(crate::api::EffectEnd {
                            by: "outcome".into(),
                            result: Some(result_name(o.result).into()),
                            reason: None,
                            evidence: evidence_of(rec.content.as_ref()),
                        });
                    }
                }
            }
            Body::SessionInput(i) => {
                let text = rec
                    .content
                    .as_ref()
                    .and_then(|c| c.get("text"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if let Some(s) = self.sessions.get_mut(&i.session) {
                    s.inputs.push(InputInfo {
                        id: id.clone(),
                        seq,
                        author: author.clone(),
                        text: text.clone(),
                    });
                    let item = ItemBody::Input {
                        entry: Some(id.clone()),
                        author: author.clone(),
                        masked: text.as_deref().map(masked_spans).unwrap_or_default(),
                        text: text.clone(),
                        state: InputState::Recorded,
                    };
                    s.items.put(format!("input:{id}"), None, item, now.ms);
                    touch = Some(Touch {
                        session: i.session,
                        summary: match &text {
                            Some(t) => format!("steered: \"{}\"", cut(t, 80)),
                            None => "steered (text redacted)".into(),
                        },
                        effect: None,
                    });
                }
            }
            Body::AgentUnguard(u) => {
                self.guarded.remove(&u.agent);
            }
            Body::Resolve(r) => {
                if let Some(e) = self.effects.get_mut(&r.proposal) {
                    e.phase = Phase::Done;
                    e.ended = Some(crate::api::EffectEnd {
                        by: "resolve".into(),
                        result: Some(
                            match r.result {
                                han_core::policy::ResolveResult::Succeeded => "succeeded",
                                han_core::policy::ResolveResult::Failed => "failed",
                            }
                            .into(),
                        ),
                        reason: None,
                        evidence: evidence_of(rec.content.as_ref()),
                    });
                    touch = e.session.clone().map(|session| Touch {
                        session,
                        summary: "resolved the proposal".into(),
                        effect: Some(r.proposal.clone()),
                    });
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
        if let Some(t) = touch
            && let Some(s) = self.sessions.get_mut(&t.session)
        {
            s.record.push(RecordInfo {
                seq,
                entry_id: rec.receipt.id.clone(),
                kind: rec.entry.kind.clone(),
                author: rec.entry.author.clone(),
                at: rec.receipt.time.clone(),
                summary: t.summary,
                redacted: rec.redacted,
                redact_reason: rec
                    .redacted_by
                    .as_ref()
                    .and_then(|r| r.entry.body.get("reason"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                effect: t.effect,
            });
            if s.record.len() > MAX_RECORD {
                s.record.remove(0);
            }
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
        let current = s.status.as_ref().is_none_or(|old| status.seq > old.seq);
        if current {
            let item = ItemBody::Status {
                body: Box::new(status.clone()),
            };
            s.items
                .put(format!("status:{}", status.seq), None, item, now.ms);
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
        if s.ended.is_some() {
            return SessionState::Failed;
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
                if !s.closed
                    && !claimed_terminal
                    && let Some(why) = &s.ended
                {
                    return Some(why.clone());
                }
                // A host that failed says so in the step ("failed: <why>"); the summary may be about something else
                // (for example the sandbox note), so the failure comes first.
                if let Some(x) = st
                    && x.state == SessionState::Failed
                    && x.step.starts_with("failed")
                {
                    return Some(x.step.clone());
                }
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

    /// The host of `agent` exited (`work`: the issue or task it was started on, if the role's name says) without
    /// closing its sessions: marks the open ones that are not over as ended for `reason`. True if any changed.
    pub fn host_exited(&mut self, agent: &str, work: Option<&str>, reason: &str) -> bool {
        let mut changed = false;
        for s in self.sessions.values_mut() {
            let works = work.is_none_or(|w| {
                s.task.as_deref() == Some(w)
                    || s.status.as_ref().and_then(|st| st.issue.as_deref()) == Some(w)
            });
            let over = s.status.as_ref().is_some_and(|st| st.state.is_terminal());
            if s.actor == agent && !s.closed && !over && works && s.ended.is_none() {
                s.ended = Some(reason.to_owned());
                changed = true;
            }
        }
        if changed {
            self.bump();
        }
        changed
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
                    budget: st.and_then(|x| x.budget.clone()),
                    publish_errors: st.map_or(0, |x| x.publish_errors),
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
                rights: self.rights(s),
                is_mine: s.actor == self.me || self.person_of(s) == self.me,
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
            // under a proposer rule (P-17) only the guards are asked
            .filter(|e| {
                !self.proposer_suffices(&e.action)
                    || self
                        .effect_guards(&e.proposer, &e.executor)
                        .iter()
                        .any(|(_, g)| g.as_ref().is_none_or(|g| !e.approvals.contains(g)))
            })
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
                let mut guards: Vec<NeededGuard> = Vec::new();
                for (agent, guard) in self.effect_guards(&e.proposer, &e.executor) {
                    if guard.is_some() && guards.iter().any(|x| x.guard == guard) {
                        continue; // one operator guards several agents: one approval covers them
                    }
                    let approved = guard.as_ref().is_some_and(|g| e.approvals.contains(g));
                    guards.push(NeededGuard {
                        guard,
                        agent,
                        approved,
                    });
                }
                let mut agent_text = plan_texts(e.plan.as_deref(), &e.proposer);
                agent_text.extend(e.notes.iter().map(|(author, text)| AgentText {
                    author: author.clone(),
                    field: "note".into(),
                    text: text.chars().take(AGENT_TEXT).collect(),
                }));
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
                        needed: Needed {
                            rule: u32::from(!self.proposer_suffices(&e.action)),
                            guards,
                        },
                        agent_text,
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
    /// The typed items of a session, oldest first, as far as the device's actor may see them. `None` if the
    /// session is unknown; `Err` text if the actor may not watch it.
    pub fn item_rows(&self, session: &str) -> Option<Result<Vec<(Facets, Row)>, String>> {
        let s = self.sessions.get(session)?;
        let rights = self.rights(s);
        if !rights.watch {
            return Some(Err("this space does not let you watch that session".into()));
        }
        let rows = s
            .items
            .items
            .values()
            .filter_map(|it| {
                let body = visible(&it.body, rights.read)?;
                let text = item_text(&body);
                Some((
                    Facets {
                        text,
                        ..Facets::default()
                    },
                    Row::Item(ItemRow {
                        key: format!("{:012}", it.seq),
                        session: session.to_owned(),
                        id: it.id.clone(),
                        seq_in_session: it.seq,
                        turn: it.turn,
                        updated_ms: it.updated_ms,
                        item: body,
                    }),
                ))
            })
            .collect();
        Some(Ok(rows))
    }

    /// The entries of the log that are about a session, oldest first.
    pub fn record_rows(&self, session: &str) -> Option<Result<Vec<(Facets, Row)>, String>> {
        let s = self.sessions.get(session)?;
        if !self.rights(s).watch {
            return Some(Err("this space does not let you watch that session".into()));
        }
        let rows = s
            .record
            .iter()
            .map(|r| {
                (
                    Facets {
                        actors: vec![r.author.clone()],
                        text: format!("{} {} {}", r.kind, r.author, r.summary),
                        ..Facets::default()
                    },
                    Row::Record(RecordRow {
                        key: format!("{:012}", r.seq),
                        session: session.to_owned(),
                        seq: r.seq,
                        entry_id: r.entry_id.clone(),
                        kind: r.kind.clone(),
                        author: r.author.clone(),
                        at: r.at.clone(),
                        summary: r.summary.clone(),
                        verified: true, // the node checked the receipt before it folded the record
                        redacted: r.redacted,
                        redact_reason: r.redact_reason.clone(),
                        effect: r.effect.clone(),
                    }),
                )
            })
            .collect();
        Some(Ok(rows))
    }

    /// The actors waiting to be admitted, for an owner.
    pub fn admission_rows(&self, actor: &str) -> Vec<(Facets, Row)> {
        if !self.is_owner(actor) {
            return Vec::new();
        }
        self.pending
            .iter()
            .filter(|(a, _)| !self.roles.contains_key(*a))
            .map(|(a, at_ms)| {
                let facts = self.profiles.get(a).and_then(Option::as_ref);
                let kind = facts.map_or(ActorKind::Unknown, |f| match f.kind {
                    ProfileKind::Human => ActorKind::Human,
                    ProfileKind::Agent => ActorKind::Agent,
                    ProfileKind::Service => ActorKind::Service,
                });
                (
                    Facets {
                        needs_me: true,
                        actors: vec![a.clone()],
                        text: format!("join {a}"),
                        ..Facets::default()
                    },
                    Row::Admission(AdmissionRow {
                        key: format!("{}|adm|{a}", self.space),
                        space: self.space.clone(),
                        actor: a.clone(),
                        kind,
                        operator: facts.and_then(|f| f.operator.clone()),
                        operator_confirmed: facts
                            .is_some_and(|f| f.kind == ProfileKind::Human || f.confirmed),
                        at_ms: *at_ms,
                        via_my_ticket: false,
                    }),
                )
            })
            .collect()
    }

    /// The keys of the identity, when this is its profile space.
    pub fn device_rows(&self, device_key: &str) -> Vec<(Facets, Row)> {
        self.devices
            .iter()
            .map(|(id, d)| {
                (
                    Facets {
                        text: id.clone(),
                        ..Facets::default()
                    },
                    Row::Device(DeviceRow {
                        key: id.clone(),
                        key_id: id.clone(),
                        state: if d.revoked_at.is_some() {
                            DeviceState::Revoked
                        } else {
                            DeviceState::Active
                        },
                        added_at: d.added_at.clone(),
                        revoked_at: d.revoked_at.clone(),
                        this_device: id == device_key,
                    }),
                )
            })
            .collect()
    }

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
                        closed_as: t.closed_as.map(str::to_owned),
                        guarded_for: t.guarded_for.clone(),
                        accepted: t.accepted.clone(),
                        budget: t
                            .budget
                            .as_ref()
                            .map(|(unit, amount)| crate::api::TaskBudget {
                                unit: unit.clone(),
                                amount: *amount,
                            }),
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
                        change_digest: e.change_digest.clone(),
                        gate_tier: e.gate_tier,
                        proposer_suffices: self.proposer_suffices(&e.action),
                        delta_digest: e.change_delta.clone(),
                        task: e.task.clone(),
                        phase: phase.into(),
                        approvals: e.approvals.clone(),
                        claims: e.claims,
                        last_claim: e.last_claim.clone(),
                        launch_window: self.launch_window,
                        repeatable: e.repeatable,
                        cost: e.cost.clone(),
                        expires_at: e.expires_at.clone(),
                        approved_seq: e.approved.as_ref().map(|(n, _)| *n),
                        approved_at: e.approved.as_ref().map(|(_, t)| t.clone()),
                        seq: e.seq,
                        head: self.head,
                        ended: e.ended.clone(),
                    }),
                )
            })
            .collect()
    }
}

/// `text` cut to `n` characters, with an ellipsis when it was longer.
fn cut(text: &str, n: usize) -> String {
    if text.chars().count() <= n {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(n).collect();
    out.push('…');
    out
}

/// An item as the device's actor may see it: the masked spans are the node's own reading of the text, and
/// without the read right the reasoning and the content of tool calls are left out.
fn visible(body: &ItemBody, read: bool) -> Option<ItemBody> {
    Some(match body {
        ItemBody::Reasoning { .. } if !read => return None,
        ItemBody::Reasoning {
            text, duration_ms, ..
        } => ItemBody::Reasoning {
            masked: masked_spans(text),
            text: text.clone(),
            duration_ms: *duration_ms,
        },
        ItemBody::Message {
            role, text, done, ..
        } => ItemBody::Message {
            role: *role,
            masked: masked_spans(text),
            text: text.clone(),
            done: *done,
        },
        ItemBody::Input {
            entry,
            author,
            text,
            state,
            ..
        } => ItemBody::Input {
            entry: entry.clone(),
            author: author.clone(),
            masked: text.as_deref().map(masked_spans).unwrap_or_default(),
            text: text.clone(),
            state: *state,
        },
        ItemBody::ToolCall {
            tool_call_id,
            tool_kind,
            title,
            status,
            locations,
            content,
            started_at,
            ended_at,
        } => ItemBody::ToolCall {
            tool_call_id: tool_call_id.clone(),
            tool_kind: *tool_kind,
            title: title.clone(),
            status: *status,
            locations: locations.clone(),
            content: if read {
                content
                    .iter()
                    .map(|c| crate::api::ToolText {
                        masked: masked_spans(&c.text),
                        text: c.text.clone(),
                    })
                    .collect()
            } else {
                Vec::new()
            },
            started_at: started_at.clone(),
            ended_at: ended_at.clone(),
        },
        other => other.clone(),
    })
}

/// The searchable text of an item.
fn item_text(body: &ItemBody) -> String {
    match body {
        ItemBody::Message { text, .. } | ItemBody::Reasoning { text, .. } => text.clone(),
        ItemBody::Input { text, .. } => text.clone().unwrap_or_default(),
        ItemBody::ToolCall { title, .. } => title.clone().unwrap_or_default(),
        ItemBody::FileChange { path, .. } => path.clone(),
        ItemBody::Decision { action, .. } => action.clone(),
        ItemBody::Status { body } => body.step.clone(),
        ItemBody::Plan { entries } => entries
            .iter()
            .map(|e| e.text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// The plain string arguments of a plan, in full: text an agent wrote and a face shows as quoted.
fn plan_texts(plan: Option<&str>, author: &str) -> Vec<AgentText> {
    let Some(serde_json::Value::Object(plan)) = plan.and_then(|p| serde_json::from_str(p).ok())
    else {
        return Vec::new();
    };
    plan.get("args")
        .and_then(serde_json::Value::as_object)
        .map(|args| {
            args.iter()
                .filter_map(|(k, v)| {
                    Some(AgentText {
                        author: author.to_owned(),
                        field: k.clone(),
                        text: v.as_str()?.chars().take(AGENT_TEXT).collect(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
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

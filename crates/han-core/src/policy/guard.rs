//! Guarding (P-15), approvals (P-08) and budgets (P-05, P-06): the helpers `decide` and `fold`
//! share.

use serde::Serialize;

use super::doc::{ActorKind, ApprovalRule, PolicyDoc, Role};
use super::state::{ActorSet, Approval, Effect, Remaining, Source, SpaceState};
use super::{Ctx, KeyStatus, MAX_GUARDED_FOR};
use crate::grammar;
use crate::types::{ActorId, Budget};

/// What [`guard_status`] reads of a state: the policy and the agents that chose guarding. A
/// [`SpaceState`] and a `state {agent}` reply both provide it.
#[derive(Clone, Copy, Debug)]
pub struct GuardInput<'a> {
    /// The policy in force.
    pub policy: &'a PolicyDoc,
    /// The agents that chose guarding.
    pub guarded: &'a ActorSet,
}

impl<'a> From<&'a SpaceState> for GuardInput<'a> {
    fn from(s: &'a SpaceState) -> Self {
        GuardInput {
            policy: &s.policy,
            guarded: &s.guarded,
        }
    }
}

/// The answer of [`guard_status`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GuardStatus {
    /// The actor's operator, if it is confirmed and the actor is not human.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard: Option<ActorId>,
    /// The actor chose guarding (sticky) or is forced.
    pub guarded: bool,
    /// The owners' `forceGuarded` is on and the actor is not human.
    pub forced: bool,
}

/// P-15, the one guard function: whether `agent` is guarded and by whom, at this entry. The
/// guard is the `operator` of the profile pinned in `ctx`, if confirmed and the actor is not
/// human; it is never stored.
pub fn guard_status<'a>(state: impl Into<GuardInput<'a>>, ctx: &Ctx, agent: &str) -> GuardStatus {
    let input = state.into();
    let p = ctx.profile(agent);
    let non_human = p.is_some_and(|p| p.actor_kind != ActorKind::Human);
    let forced = non_human && input.policy.forced();
    let guard = p
        .filter(|_| non_human)
        .filter(|p| p.operator_confirmed)
        .and_then(|p| p.operator.clone())
        .filter(|o| !o.is_empty());
    GuardStatus {
        guard,
        guarded: forced || input.guarded.contains(agent),
        forced,
    }
}

/// P-09: whole seconds after a claim's receipt time before an owner may abandon it. The executor
/// launches at most `window` after sending on its own clock, which runs within 1% of true time;
/// the home measures from a receipt time rounded down. Safe iff `A(1-d) - 1 > window(1+d)`; we
/// take the next whole second, plus one.
pub fn abandon_after(window: u64) -> u64 {
    const DRIFT_PPM: u64 = 10_000;
    let num = window * (1_000_000 + DRIFT_PPM);
    let den = 1_000_000 - DRIFT_PPM;
    num.div_ceil(den) + 2
}

// ───────────────────────── small helpers ─────────────────────────

pub(super) fn server_of(id: &str) -> &str {
    grammar::server_of(id).unwrap_or("")
}

pub(super) fn secs(time: &str) -> i64 {
    // State times were validated when they were folded; an unreadable one never expires.
    grammar::epoch_seconds(time).unwrap_or(i64::MAX)
}

/// How a key stands in a pinned profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Key {
    Unpinned,
    Absent,
    Valid,
    Revoked,
}

pub(super) fn key_status(ctx: &Ctx, actor: &str, key_id: &str) -> Key {
    match ctx.profile(actor) {
        None => Key::Unpinned,
        Some(p) => match p.keys.get(key_id).map(|k| k.status) {
            None => Key::Absent,
            Some(KeyStatus::Valid) => Key::Valid,
            Some(KeyStatus::Revoked) => Key::Revoked,
        },
    }
}

pub(super) fn role_of(state: &SpaceState, actor: &str) -> Option<Role> {
    state.roles.get(actor).copied()
}

pub(super) fn reads(role: Option<Role>) -> bool {
    role.is_some_and(Role::reads)
}

pub(super) fn writes(role: Option<Role>) -> bool {
    role.is_some_and(Role::writes)
}

pub(super) fn is_human(ctx: &Ctx, actor: &str) -> bool {
    ctx.profile(actor)
        .is_some_and(|p| p.actor_kind == ActorKind::Human)
}

pub(super) fn approval_rule<'a>(policy: &'a PolicyDoc, action: &str) -> Option<&'a ApprovalRule> {
    policy
        .approvals
        .iter()
        .find(|r| r.action == action || r.action == "*")
}

pub(super) fn may_approve(state: &SpaceState, rule: &ApprovalRule, actor: &str) -> bool {
    match role_of(state, actor) {
        Some(role) if role.reads() => rule
            .approvers
            .iter()
            .any(|x| x == actor || x == role.as_str()),
        _ => false,
    }
}

pub(super) fn expired(a: &Approval, now: i64) -> bool {
    a.expires_at.as_deref().is_some_and(|e| secs(e) <= now)
}

/// E3 order: unconsumed, unexpired approvals whose author satisfies the rule now (no profiles
/// needed). These are also the approvals the cap counts (P-08).
pub(super) fn approval_candidates<'e>(
    state: &SpaceState,
    eff: &'e Effect,
    now: i64,
) -> Vec<&'e Approval> {
    let Some(rule) = approval_rule(&state.policy, &eff.action) else {
        return Vec::new();
    };
    eff.approvals
        .iter()
        .filter(|a| a.consumed_by.is_none())
        .filter(|a| !expired(a, now))
        .filter(|a| may_approve(state, rule, &a.author))
        .filter(|a| !(state.policy.separate_approver && a.author == eff.proposer))
        .collect()
}

/// E3: a candidate is consumed if its key is valid (no candidate's author is guarded: the fold
/// drops such approvals).
pub(super) fn usable(ctx: &Ctx, a: &Approval) -> bool {
    key_status(ctx, &a.author, &a.key_id) == Key::Valid
}

/// How E3 resolves for a pick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Pick {
    Ok,
    Unpinned,
    None,
}

/// E3: the first usable candidate. A candidate reached with no pinned profile is `Unpinned`.
pub(super) fn pick_approval<'e>(
    state: &SpaceState,
    eff: &'e Effect,
    ctx: &Ctx,
) -> (Pick, Option<&'e Approval>) {
    for a in approval_candidates(state, eff, ctx.secs) {
        if key_status(ctx, &a.author, &a.key_id) == Key::Unpinned {
            return (Pick::Unpinned, Some(a));
        }
        if usable(ctx, a) {
            return (Pick::Ok, Some(a));
        }
    }
    (Pick::None, None)
}

/// E3 for a guard: an unconsumed, unexpired approval by `g` itself, while `g` holds a role,
/// signed with a valid key. Nobody stands in for a guard.
pub(super) fn guard_approval<'e>(
    state: &SpaceState,
    eff: &'e Effect,
    g: &str,
    ctx: &Ctx,
) -> (Pick, Option<&'e Approval>) {
    if !reads(role_of(state, g)) {
        return (Pick::None, None);
    }
    for a in &eff.approvals {
        if a.author != g || a.consumed_by.is_some() || expired(a, ctx.secs) {
            continue;
        }
        match key_status(ctx, &a.author, &a.key_id) {
            Key::Unpinned => return (Pick::Unpinned, Some(a)),
            Key::Valid => return (Pick::Ok, Some(a)),
            _ => {}
        }
    }
    (Pick::None, None)
}

// ───────────────────────── guarding (P-15) ─────────────────────────

/// P-15: the guarded agents whose limits `x` carries: the union of the `guardedFor` of the tasks
/// whose `accepted` lists `x`, in the order the tasks were created.
pub(super) fn carried(state: &SpaceState, x: &str) -> Vec<ActorId> {
    let mut tasks: Vec<_> = state
        .tasks
        .values()
        .filter(|t| t.accepted.iter().any(|a| a == x))
        .collect();
    tasks.sort_by_key(|t| t.seq);
    let mut out = Vec::new();
    for t in tasks {
        for a in &t.guarded_for {
            if !out.contains(a) {
                out.push(a.clone());
            }
        }
    }
    out
}

/// P-15: guarded, or carrying a guarded agent's limits: either way `x` may not approve, govern or
/// steer others.
pub(super) fn limited(state: &SpaceState, ctx: &Ctx, x: &str) -> bool {
    guard_status(state, ctx, x).guarded || !carried(state, x).is_empty()
}

/// P-15: `recorded` plus each of `agents` that is guarded at this entry, each once.
fn guarded_for(
    state: &SpaceState,
    ctx: &Ctx,
    recorded: Vec<ActorId>,
    agents: &[&str],
) -> Vec<ActorId> {
    let mut out = recorded;
    for a in agents {
        if !out.iter().any(|x| x == a) && guard_status(state, ctx, a).guarded {
            out.push((*a).to_owned());
        }
    }
    out
}

/// P-15, a task's `guardedFor` at its creation: the parent's list, or `carried(author)` when it has
/// no parent; then the author if guarded; each once.
pub(super) fn task_guarded_for(
    state: &SpaceState,
    ctx: &Ctx,
    author: &str,
    parent: Option<&str>,
) -> Vec<ActorId> {
    let out = match parent {
        Some(p) => state
            .tasks
            .get(p)
            .map(|t| t.guarded_for.clone())
            .unwrap_or_default(),
        None => carried(state, author),
    };
    guarded_for(state, ctx, out, &[author])
}

/// P-15, an effect's `guardedFor` at its proposal: the named task's list, or `carried(proposer)`
/// when it names none; then `carried(executor)`; then the proposer and the executor, each if
/// guarded now; each agent once.
pub(super) fn effect_guarded_for(
    state: &SpaceState,
    ctx: &Ctx,
    proposer: &str,
    executor: &str,
    task: Option<&str>,
) -> Vec<ActorId> {
    let mut out = match task {
        Some(t) => state
            .tasks
            .get(t)
            .map(|t| t.guarded_for.clone())
            .unwrap_or_default(),
        None => carried(state, proposer),
    };
    for a in carried(state, executor) {
        if !out.contains(&a) {
            out.push(a);
        }
    }
    guarded_for(state, ctx, out, &[proposer, executor])
}

/// P-15: the proposal bound, counting the proposer and the executor whether guarded or not.
pub(super) fn over_bound(recorded: &[ActorId], actors: &[&str]) -> bool {
    let mut all: Vec<&str> = Vec::new();
    for a in recorded
        .iter()
        .map(String::as_str)
        .chain(actors.iter().copied())
    {
        if !all.contains(&a) {
            all.push(a);
        }
    }
    all.len() > MAX_GUARDED_FOR
}

/// P-15: whose own approval a claim of `eff` needs besides any rule: the guard of each agent in
/// its `guardedFor`, and of its proposer and executor if guarded now, each guard once. `None`
/// stands for such an agent with no confirmed operator: nobody can approve for it.
pub(super) fn guards_of(state: &SpaceState, ctx: &Ctx, eff: &Effect) -> Vec<Option<ActorId>> {
    let need = guarded_for(
        state,
        ctx,
        eff.guarded_for.clone(),
        &[&eff.proposer, &eff.executor],
    );
    let mut out: Vec<Option<ActorId>> = Vec::new();
    for a in need {
        let g = guard_status(state, ctx, &a).guard;
        if !out.contains(&g) {
            out.push(g);
        }
    }
    out
}

pub(super) fn in_guards(guards: &[Option<ActorId>], actor: &str) -> bool {
    guards.iter().any(|g| g.as_deref() == Some(actor))
}

// ───────────────────────── budgets (P-05, P-06) ─────────────────────────

pub(super) fn space_unit(state: &SpaceState) -> Option<&str> {
    state.policy.budget.as_ref().map(|b| b.unit.as_str())
}

/// Where a new task's budget is reserved from: its parent if the parent has a budget, else the space.
pub(super) fn task_source(state: &SpaceState, parent: Option<&str>) -> Source {
    match parent.and_then(|p| state.tasks.get(p).map(|t| (p, t))) {
        Some((p, t)) if t.left.is_some() => Source::Task(p.to_owned()),
        _ => Source::Space,
    }
}

/// Where an effect's cost is charged: its task if that task has a budget, else the space.
pub(super) fn effect_source(state: &SpaceState, eff: &Effect) -> Source {
    match eff
        .task
        .as_deref()
        .and_then(|t| state.tasks.get(t).map(|x| (t, x)))
    {
        Some((t, x)) if x.left.is_some() => Source::Task(t.to_owned()),
        _ => Source::Space,
    }
}

pub(super) fn remaining(state: &SpaceState, src: &Source) -> i64 {
    // A source names a budgeted task or the space of a budgeted policy, so the amount exists.
    match src {
        Source::Task(t) => state
            .tasks
            .get(t)
            .and_then(|t| t.left.as_ref())
            .map_or(0, |l| l.amount),
        Source::Space => state.budget_left.as_ref().map_or(0, |l| l.amount),
    }
}

pub(super) fn has_reservations(state: &SpaceState) -> bool {
    state.tasks.values().any(|t| t.open && t.left.is_some())
        || state.effects.values().any(|e| {
            e.claims
                .last()
                .is_some_and(|c| c.reserved.is_some() && !c.settled)
        })
}

/// P-06: the remaining budget after a policy change: same unit gives the new amount minus what is
/// committed (reserved or spent so far), possibly negative; a unit change starts fresh.
pub(super) fn rebudget(
    old_budget: &Option<Budget>,
    old_left: &Option<Remaining>,
    new_budget: &Option<Budget>,
) -> Option<Remaining> {
    let new = new_budget.as_ref()?;
    if let (Some(ob), Some(ol)) = (old_budget, old_left)
        && ob.unit == new.unit
    {
        let committed = ob.amount as i64 - ol.amount;
        return Some(Remaining {
            unit: new.unit.clone(),
            amount: new.amount as i64 - committed,
        });
    }
    Some(Remaining {
        unit: new.unit.clone(),
        amount: new.amount as i64,
    })
}

pub(super) fn budget_change_ok(state: &SpaceState, new_budget: &Option<Budget>) -> bool {
    let same = match (&state.policy.budget, new_budget) {
        (None, None) => true,
        (Some(o), Some(n)) => o.unit == n.unit,
        _ => false,
    };
    same || !has_reservations(state)
}

/// P-08: whether an open approval can never count again: its author is guarded now, judged from
/// the state and the `actorKind` the approval recorded, or its author's server is denied by the
/// current policy.
pub(super) fn approval_dead(guarded: &ActorSet, policy: &PolicyDoc, a: &Approval) -> bool {
    guarded.contains(&a.author)
        || (policy.forced() && a.actor_kind != ActorKind::Human)
        || policy.denies(server_of(&a.author))
}

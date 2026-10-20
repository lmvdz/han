//! Guarding (P-15), approvals (P-08) and budgets (P-05, P-06): the helpers `decide` and `fold`
//! share.

use serde::Serialize;

use super::doc::{ActorKind, ApprovalRule, PolicyDoc, Role};
use super::gate::{Evidence, Gate, PATH_OWNER};
use std::collections::BTreeMap;
use std::sync::LazyLock;

use super::state::{ActorSet, Approval, Effect, Remaining, Source, SpaceState, Task};
use super::{Ctx, KeyStatus, MAX_GUARDED_FOR};
use crate::grammar;
use crate::types::{ActorId, Budget, EntryId};

/// What [`guard_status`] reads of a state: the policy, the agents that chose guarding and the
/// tasks (for `carried`, round 32 Z1). A [`SpaceState`] and a `state {agent}` reply both provide it.
#[derive(Clone, Copy, Debug)]
pub struct GuardInput<'a> {
    /// The policy in force.
    pub policy: &'a PolicyDoc,
    /// The agents that chose guarding.
    pub guarded: &'a ActorSet,
    /// The tasks; a `state {agent}` reply holds those whose `accepted` lists the agent.
    pub tasks: &'a BTreeMap<EntryId, Task>,
}

impl<'a> From<&'a SpaceState> for GuardInput<'a> {
    fn from(s: &'a SpaceState) -> Self {
        GuardInput {
            policy: &s.policy,
            guarded: &s.guarded,
            tasks: &s.tasks,
        }
    }
}

/// The answer of [`guard_status`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GuardStatus {
    /// The actor's operator, if it is confirmed and the actor is not human.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard: Option<ActorId>,
    /// `guards` is not empty.
    pub guarded: bool,
    /// The owners' `forceGuarded` is on and the actor is not human.
    pub forced: bool,
    /// Z1: the agents whose guard the actor answers to: itself if it chose guarding (sticky) or
    /// is forced, then `carried(actor)`, each once.
    pub guards: Vec<ActorId>,
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
    let mut guards = Vec::new();
    if forced || input.guarded.contains(agent) {
        guards.push(agent.to_owned());
    }
    for a in carried(input.tasks, agent) {
        if !guards.contains(&a) {
            guards.push(a);
        }
    }
    GuardStatus {
        guard,
        guarded: !guards.is_empty(),
        forced,
        guards,
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

/// A gated effect of tier 1 or more that no rule covers still needs an owner (P-17).
static DEFAULT_GATED_RULE: LazyLock<ApprovalRule> = LazyLock::new(|| ApprovalRule {
    action: "*".to_owned(),
    repo: None,
    approvers: vec!["owner".to_owned()],
    min_tier: None,
    max_tier: None,
    humans: None,
    evidence: None,
    needs: None,
    revert: None,
    proposer: None,
});

/// P-08 (round 33): the rule that applies to `eff`: the first whose action matches and whose tier predicate
/// (`minTier`, `maxTier`) holds. A rule with a tier predicate never applies to an effect without a gate. A
/// gated effect of tier 1 or more that no rule covers needs an owner.
pub(super) fn rule_for<'a>(state: &'a SpaceState, eff: &Effect) -> Option<&'a ApprovalRule> {
    matching_approval_rule(&state.policy.approvals, &eff.action, eff.gate.as_ref())
}

/// Whether the first matching current approval rule lets this proposal satisfy the rule approval.
/// Faces and executors must supply the derived gate, including its exact repository.
pub fn proposer_suffices(approvals: &[ApprovalRule], action: &str, gate: Option<&Gate>) -> bool {
    matching_approval_rule(approvals, action, gate).is_some_and(|r| r.proposer == Some(true))
}

/// The rule of `approvals` that applies to an effect of `action` with `gate`: action, repository, tier and revert predicates all apply.
pub fn matching_approval_rule<'a>(
    approvals: &'a [ApprovalRule],
    action: &str,
    g: Option<&Gate>,
) -> Option<&'a ApprovalRule> {
    for r in approvals {
        if r.action != action && r.action != "*" {
            continue;
        }
        if r.repo
            .as_ref()
            .is_some_and(|repo| g.and_then(|g| g.repo.as_ref()) != Some(repo))
        {
            continue;
        }
        if r.revert == Some(true) && g.is_none_or(|g| g.revert_of.is_none()) {
            continue; // A2: the revert predicate covers only a revert
        }
        let predicated = r.min_tier.is_some() || r.max_tier.is_some();
        if predicated
            && g.is_none_or(|g| {
                g.tier < r.min_tier.unwrap_or(0) || g.tier > r.max_tier.unwrap_or(3)
            })
        {
            continue;
        }
        return Some(r);
    }
    g.filter(|g| g.tier >= 1).map(|_| &*DEFAULT_GATED_RULE)
}

/// P-07 E3: whether a claim of `eff` consumes approvals of the rule that applies: there is a rule and it does
/// not let the proposer suffice (P-17, round 33).
pub(super) fn asks_approval(state: &SpaceState, eff: &Effect) -> bool {
    rule_for(state, eff).is_some_and(|r| r.proposer != Some(true))
}

/// P-08, P-17: whether an approval by `actor` (of `kind`, with `evidence`) counts under `rule`: the actor
/// holds a role the rule names (a role, its id, or `@path-owner`); of tier 2 or more, or under `humans`, only
/// humans count; and when the rule asks for `evidence`, a non-human approval counts only with that kind over
/// the effect's `deltaDigest`.
pub(super) fn counts(
    state: &SpaceState,
    eff: &Effect,
    rule: &ApprovalRule,
    actor: &str,
    kind: ActorKind,
    evidence: Option<&Evidence>,
) -> bool {
    let Some(role) = role_of(state, actor).filter(|r| r.reads()) else {
        return false;
    };
    let g = eff.gate.as_ref();
    let named = rule.approvers.iter().any(|x| {
        x == actor
            || x == role.as_str()
            || (x == PATH_OWNER && g.is_some_and(|g| g.owners.iter().any(|o| o == actor)))
    });
    if !named {
        return false;
    }
    if kind != ActorKind::Human {
        if rule.humans == Some(true) || g.is_some_and(|g| g.tier >= 2) {
            return false;
        }
        if let Some(k) = &rule.evidence {
            // A2: a revert rule's evidence names the failing run over the *reverted* delta
            let over = if rule.revert == Some(true) {
                g.and_then(|g| g.reverted_digest.as_deref())
            } else {
                g.and_then(|g| g.delta_digest.as_deref())
            };
            if !evidence.is_some_and(|e| e.kind == *k && Some(e.over.as_str()) == over) {
                return false;
            }
        }
    }
    true
}

/// A2: whether `author` approving `eff` is the lane approving its own revert, which `separateApprover` does not
/// stop: the rule is a revert rule, the author is a machine and is both the proposer and the executor. A machine
/// never counts at tier 2 or more (`counts`), so this lifts nothing there.
pub(super) fn self_revert_approval(
    eff: &Effect,
    rule: &ApprovalRule,
    author: &str,
    kind: ActorKind,
) -> bool {
    rule.revert == Some(true)
        && kind != ActorKind::Human
        && author == eff.proposer
        && author == eff.executor
}

/// P-07 E3 (round 33): the distinct approvals a claim consumes under the rule: its `needs`, and two when the
/// effect's tier is 3.
pub(super) fn needs(eff: &Effect, rule: &ApprovalRule) -> usize {
    let floor = if eff.gate.as_ref().is_some_and(|g| g.tier >= 3) {
        2
    } else {
        1
    };
    usize::from(rule.needs.unwrap_or(1)).max(floor)
}

pub(super) fn expired(a: &Approval, now: i64) -> bool {
    a.expires_at.as_deref().is_some_and(|e| secs(e) <= now)
}

/// E3 order: unconsumed, unexpired approvals that count under the rule now (no profiles needed). These are
/// also the approvals the cap counts (P-08).
pub(super) fn approval_candidates<'e>(
    state: &SpaceState,
    eff: &'e Effect,
    now: i64,
) -> Vec<&'e Approval> {
    let Some(rule) = rule_for(state, eff).filter(|r| r.proposer != Some(true)) else {
        return Vec::new(); // under a proposer rule no approval is asked, so none is a candidate (P-17)
    };
    eff.approvals
        .iter()
        .filter(|a| a.consumed_by.is_none())
        .filter(|a| !expired(a, now))
        .filter(|a| {
            counts(
                state,
                eff,
                rule,
                &a.author,
                a.actor_kind,
                a.evidence.as_ref(),
            )
        })
        .filter(|a| {
            !(state.policy.separate_approver
                && a.author == eff.proposer
                && !self_revert_approval(eff, rule, &a.author, a.actor_kind))
        })
        .collect()
}

/// E3: a candidate is consumed if its key is valid (no candidate's author is guarded: the fold drops such
/// approvals).
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

/// E3: the first usable candidates, as many as the rule needs (one, two at tier 3). A candidate reached with
/// no pinned profile is `Unpinned`; fewer usable candidates than needed is `None`. The approvals picked so
/// far come with the answer.
pub(super) fn pick_approval<'e>(
    state: &SpaceState,
    eff: &'e Effect,
    ctx: &Ctx,
) -> (Pick, Vec<&'e Approval>) {
    let need = rule_for(state, eff).map_or(1, |r| needs(eff, r));
    let mut picked = Vec::new();
    for a in approval_candidates(state, eff, ctx.secs) {
        if key_status(ctx, &a.author, &a.key_id) == Key::Unpinned {
            return (Pick::Unpinned, picked);
        }
        if usable(ctx, a) {
            picked.push(a);
            if picked.len() >= need {
                return (Pick::Ok, picked);
            }
        }
    }
    (Pick::None, picked)
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

/// P-15: the guarded agents whose guarding `x` carries: the union of the `guardedFor` of the tasks
/// whose `accepted` lists `x`, in the order the tasks were created.
pub(super) fn carried(tasks: &BTreeMap<EntryId, Task>, x: &str) -> Vec<ActorId> {
    let mut mine: Vec<_> = tasks
        .values()
        .filter(|t| t.accepted.iter().any(|a| a == x))
        .collect();
    mine.sort_by_key(|t| t.seq);
    let mut out = Vec::new();
    for t in mine {
        for a in &t.guarded_for {
            if !out.contains(a) {
                out.push(a.clone());
            }
        }
    }
    out
}

/// P-15, Z1: the agents whose guard `x` answers to ([`guard_status`]).
pub(super) fn guards(state: &SpaceState, ctx: &Ctx, x: &str) -> Vec<ActorId> {
    guard_status(state, ctx, x).guards
}

fn push_new(out: &mut Vec<ActorId>, more: Vec<ActorId>) {
    for a in more {
        if !out.contains(&a) {
            out.push(a);
        }
    }
}

/// P-15, a task's `guardedFor` at its creation: the parent's list, then guards(author); each once.
pub(super) fn task_guarded_for(
    state: &SpaceState,
    ctx: &Ctx,
    author: &str,
    parent: Option<&str>,
) -> Vec<ActorId> {
    let mut out = parent
        .and_then(|p| state.tasks.get(p))
        .map(|t| t.guarded_for.clone())
        .unwrap_or_default();
    push_new(&mut out, guards(state, ctx, author));
    out
}

/// P-15, an effect's `guardedFor` at its proposal: the named task's list, then guards(proposer),
/// then guards(executor); each agent once.
pub(super) fn effect_guarded_for(
    state: &SpaceState,
    ctx: &Ctx,
    proposer: &str,
    executor: &str,
    task: Option<&str>,
) -> Vec<ActorId> {
    let mut out = task
        .and_then(|t| state.tasks.get(t))
        .map(|t| t.guarded_for.clone())
        .unwrap_or_default();
    push_new(&mut out, guards(state, ctx, proposer));
    push_new(&mut out, guards(state, ctx, executor));
    out
}

/// E3, Z1: guards(proposer) then guards(executor) as they stand at this entry, each once.
pub(super) fn claim_guards(state: &SpaceState, ctx: &Ctx, eff: &Effect) -> Vec<ActorId> {
    let mut out = guards(state, ctx, &eff.proposer);
    push_new(&mut out, guards(state, ctx, &eff.executor));
    out
}

/// E3, Z1: whether [`claim_guards`] holds an agent the proposal never counted (neither in
/// `guardedFor` nor the proposer or executor); a claim needing one is denied.
pub(super) fn unbounded_guards(state: &SpaceState, ctx: &Ctx, eff: &Effect) -> bool {
    claim_guards(state, ctx, eff)
        .iter()
        .any(|a| !eff.guarded_for.contains(a) && *a != eff.proposer && *a != eff.executor)
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
/// its `guardedFor`, then of each agent of [`claim_guards`] the proposal counted (its proposer or
/// executor), each guard once. `None` stands for such an agent with no confirmed operator: nobody
/// can approve for it.
pub(super) fn guards_of(state: &SpaceState, ctx: &Ctx, eff: &Effect) -> Vec<Option<ActorId>> {
    let mut need = eff.guarded_for.clone();
    for a in claim_guards(state, ctx, eff) {
        if (a == eff.proposer || a == eff.executor) && !need.contains(&a) {
            need.push(a);
        }
    }
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

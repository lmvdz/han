//! `decide` and `relevant_actors`: architecture.md section 6, P-01 and P-16.

use super::access::{live, session_access};
use super::body::{
    Approve, Body, Claim, EffectCancel, EffectResult, KeyAdd, KeyRevoke, MemberSet, Outcome,
    PolicyEntry, PolicySet, Propose, Redact, Resolve, ResolveResult, SessionOpen, SessionRef,
    Started, TaskBody, TaskCancel, TaskUpdate,
};
use super::doc::{ActorKind, AllowAgents, JoinMode, NewRole, Role, SteerMode};
use super::guard::{
    Key, Pick, abandon_after, approval_candidates, asks_approval, budget_change_ok, counts,
    effect_guarded_for, effect_source, expired, guard_approval, guard_status, guards_of, in_guards,
    is_human, key_status, needs, over_bound, pick_approval, reads, remaining, role_of, rule_for,
    secs, self_revert_approval, server_of, space_unit, task_guarded_for, task_source,
    unbounded_guards, usable, writes,
};
use super::state::{Effect, Phase, Source, SpaceState, Task};
use super::{Ctx, MAX_ATTEMPTS, MAX_GUARDED_FOR, MAX_OPEN_APPROVALS, MAX_OPEN_SESSIONS, Verdict};
use crate::json::Value;
use crate::types::ActorId;

fn deny(reason: impl Into<String>) -> Verdict {
    Verdict::Denied(reason.into())
}

fn invalid(reason: impl Into<String>) -> Verdict {
    Verdict::Invalid(reason.into())
}

/// P-16: decides an entry given as Json. An entry that is malformed (R-09) is `invalid`.
pub fn decide_value(state: Option<&SpaceState>, entry: &Value, ctx: &Ctx) -> Verdict {
    match PolicyEntry::from_value(entry) {
        Ok(e) => decide(state, &e, ctx),
        Err(e) => invalid(e.to_string()),
    }
}

/// P-01: the actors an entry names, which `decide` needs pinned: the author; the target of
/// `member.set`; the targets of an instruction (the actor of the session a steer goes into, each
/// assignee of a task, the executor of a proposal); the agent of an unguard; for an approve,
/// cancel or claim, the proposer, the executor and the `guardedFor` of the proposal. Approvers
/// are never pinned for an approve.
fn named_actors(state: Option<&SpaceState>, entry: &PolicyEntry) -> Vec<ActorId> {
    let mut out = vec![entry.author.clone()];
    match &entry.body {
        Body::MemberSet(b) => out.push(b.actor.clone()),
        Body::Task(b) => out.extend(b.to.iter().cloned()),
        Body::Propose(b) => out.push(b.executor.clone()),
        Body::SessionInput(b) => {
            if let Some(s) = state.and_then(|s| s.sessions.get(&b.session)) {
                out.push(s.actor.clone());
            }
        }
        Body::AgentUnguard(b) => out.push(b.agent.clone()),
        Body::Approve(Approve { proposal, .. })
        | Body::EffectCancel(EffectCancel { proposal })
        | Body::Claim(Claim { proposal, .. }) => {
            if let Some(eff) = state.and_then(|s| s.effects.get(proposal)) {
                out.push(eff.proposer.clone());
                out.push(eff.executor.clone());
                out.extend(eff.guarded_for.iter().cloned());
            }
        }
        _ => {}
    }
    dedup(out)
}

fn dedup(v: Vec<ActorId>) -> Vec<ActorId> {
    let mut out: Vec<ActorId> = Vec::with_capacity(v.len());
    for a in v {
        if !out.contains(&a) {
            out.push(a);
        }
    }
    out
}

/// P-01, the complete pin list given the profiles pinned so far (`ctx`): the named actors; for a
/// claim, then the guards (the operators, in the profiles in `ctx`, of the agents whose guard must
/// approve it) and the E3 candidates in order up to and including the first that is usable by the
/// profiles in `ctx`. The sequencer calls it again with what it fetched until it names nothing
/// new: at most three calls.
pub fn relevant_actors(state: Option<&SpaceState>, entry: &PolicyEntry, ctx: &Ctx) -> Vec<ActorId> {
    let mut out = named_actors(state, entry);
    if let (Some(state), Body::Claim(b)) = (state, &entry.body)
        && let Some(eff) = state.effects.get(&b.proposal)
    {
        out.extend(guards_of(state, ctx, eff).into_iter().flatten());
        let need = rule_for(state, eff).map_or(1, |r| needs(eff, r));
        let mut got = 0;
        for a in approval_candidates(state, eff, ctx.secs) {
            out.push(a.author.clone());
            if usable(ctx, a) {
                got += 1;
                if got >= need {
                    break;
                }
            }
        }
    }
    dedup(out)
}

/// P-16: may `entry` be appended to `state` (`None` before the first entry)? The Log has already
/// checked the signature, the content and the exact header; `ctx` carries the receipt time and
/// the profiles pinned for the entry. The checks run in the order the rules give.
pub fn decide(state: Option<&SpaceState>, entry: &PolicyEntry, ctx: &Ctx) -> Verdict {
    let author = entry.author.as_str();
    for actor in named_actors(state, entry) {
        if ctx.profile(&actor).is_none() {
            return invalid(format!("no pinned profile for {actor}"));
        }
    }
    if key_status(ctx, author, &entry.key_id) != Key::Valid {
        return deny("author key not valid");
    }
    let Some(state) = state else {
        return decide_create(entry, ctx);
    };
    if matches!(entry.body, Body::SpaceCreate(_)) {
        return invalid("space.create only at seq 1");
    }
    if entry.seen > state.head {
        return invalid("seen beyond head");
    }
    if entry.refs.iter().any(|r| !state.entries.contains_key(r)) {
        return invalid("ref not in space"); // R-05
    }
    if state.policy.denies(server_of(author))
        && !matches!(entry.body, Body::Outcome(_) | Body::Resolve(_))
    {
        return deny("server denied");
    }
    if state.space.starts_with("profile:") {
        return decide_profile(state, entry, ctx);
    }
    let role = role_of(state, author);
    if guard_status(state, ctx, author).guarded && governs_others(state, ctx, entry) {
        return deny("a guarded author may not approve, govern or steer others"); // P-15, Z1
    }
    match &entry.body {
        Body::Extension(_) => {
            let need = state.policy.extensions_min_role.rank();
            let ok = role
                .and_then(Role::rank)
                .zip(need)
                .is_some_and(|(r, n)| r <= n);
            if ok {
                Verdict::Allow
            } else {
                deny(format!(
                    "extensions need role {} or higher",
                    state.policy.extensions_min_role.as_str()
                ))
            }
        }
        Body::MemberSet(b) => member_set(state, entry, ctx, role, b),
        Body::PolicySet(b) => policy_set(state, role, b),
        Body::Message(_) => {
            if reads(role) {
                Verdict::Allow
            } else {
                deny("not a member")
            }
        }
        Body::Task(b) => task(state, entry, ctx, role, b),
        Body::TaskUpdate(b) => task_update(state, entry, role, b),
        Body::TaskCancel(b) => task_cancel(state, entry, role, b),
        Body::Propose(b) => propose(state, entry, ctx, role, b),
        Body::Approve(b) => approve(state, entry, ctx, role, b),
        Body::Claim(b) => claim(state, entry, ctx, role, b),
        Body::EffectCancel(b) => cancel(state, entry, ctx, role, b),
        Body::Outcome(b) => outcome(state, entry, ctx, role, b),
        Body::Resolve(b) => resolve(state, entry, role, b),
        Body::Checkpoint(b) => {
            if !writes(role) {
                deny("not allowed")
            } else if b.through <= state.head {
                Verdict::Allow
            } else {
                invalid("through beyond head")
            }
        }
        Body::Redact(b) => redact(state, entry, role, b),
        Body::SpaceMove(_) => space_move(state, role),
        Body::SessionOpen(b) => session_open(state, entry, ctx, role, b),
        Body::SessionClose(b) => session_close(state, entry, role, b),
        Body::SessionInput(b) => session_input(state, entry, ctx, b),
        Body::AgentUnguard(b) => agent_unguard(state, entry, ctx, role, &b.agent),
        Body::SpaceCreate(_) | Body::KeyAdd(_) | Body::KeyRevoke(_) => {
            deny(format!("{} not allowed here", entry.kind))
        }
    }
}

fn decide_create(entry: &PolicyEntry, ctx: &Ctx) -> Verdict {
    let Body::SpaceCreate(b) = &entry.body else {
        return invalid("first entry must be space.create");
    };
    let (author, space) = (entry.author.as_str(), entry.space.as_str());
    if let Some(actor) = space.strip_prefix("profile:") {
        if author != format!("@:{}", server_of(actor)) {
            return deny("a profile space is created by its server");
        }
        let Some(profile) = &b.profile else {
            return invalid("profile space needs profile");
        };
        if profile.actor_kind != ActorKind::Human && profile.operator.is_none() {
            return invalid("a non-human profile names its operator");
        }
        if profile.operator.as_deref() == Some(actor) {
            return invalid("an actor can't be its own operator");
        }
        return Verdict::Allow;
    }
    if b.profile.is_some() {
        return invalid("profile only in profile spaces");
    }
    if b.policy.denies(server_of(space)) {
        return invalid("a space can't deny its own home");
    }
    if !is_human(ctx, author) {
        return deny("owners are human: only a human creates a space (P-03)");
    }
    Verdict::Allow
}

fn decide_profile(state: &SpaceState, entry: &PolicyEntry, ctx: &Ctx) -> Verdict {
    let author = entry.author.as_str();
    let actor = state.space.strip_prefix("profile:").unwrap_or_default();
    let server = format!("@:{}", server_of(actor));
    let Some(profile) = &state.profile else {
        return deny(format!("{} not allowed in a profile space", entry.kind));
    };
    match &entry.body {
        Body::KeyAdd(KeyAdd { key_id, .. }) | Body::KeyRevoke(KeyRevoke { key_id }) => {
            if author != actor && author != server {
                return deny("only the actor or its server manages keys");
            }
            let adding = matches!(entry.body, Body::KeyAdd(_));
            if adding && profile.keys.contains_key(key_id) {
                return invalid("keyIds are single-use");
            }
            if !adding
                && profile.keys.get(key_id).map(|k| k.status) != Some(super::KeyStatus::Valid)
            {
                return invalid("no such valid key");
            }
            Verdict::Allow
        }
        Body::MemberSet(b) => {
            if let Some(op) = profile.operator.as_deref() {
                if author == op && b.actor == op && b.role == NewRole::Owner {
                    if !is_human(ctx, op) {
                        return deny("an operator must be a human actor");
                    }
                    return Verdict::Allow;
                }
                if (author == op || author == server) && b.actor == op && b.role == NewRole::None {
                    return Verdict::Allow; // operator withdraws (or server revokes)
                }
            }
            deny("only operator confirmation or withdrawal allowed")
        }
        _ => deny(format!("{} not allowed in a profile space", entry.kind)),
    }
}

/// P-15: a guarded author, or one that carries a guarded agent's limits, may not approve, govern
/// or steer others: `effect.approve`, `member.set` other than leaving (its own role to none),
/// `policy.set`, and a `session.input` into a session that isn't its own.
/// P-15, the Limits (Z1, Z3): what a guarded author may not do: approve (unless it is one of the
/// effect's guards), govern (`member.set` other than leaving, `policy.set`, `space.move`, a final
/// update of a task it isn't assigned, a cancel of another's task) or steer others. A task or
/// session that isn't there is left to the kind's checks; an approve of a missing proposal is
/// denied here.
fn governs_others(state: &SpaceState, ctx: &Ctx, entry: &PolicyEntry) -> bool {
    let author = entry.author.as_str();
    match &entry.body {
        Body::MemberSet(b) => !(b.actor == author && b.role == NewRole::None),
        Body::PolicySet(_) | Body::SpaceMove(_) => true,
        Body::Approve(b) => state
            .effects
            .get(&b.proposal)
            .is_none_or(|eff| !in_guards(&guards_of(state, ctx, eff), author)),
        Body::SessionInput(b) => state
            .sessions
            .get(&b.session)
            .is_some_and(|s| s.actor != author),
        Body::TaskUpdate(b) => state
            .tasks
            .get(&b.task)
            .is_some_and(|t| b.state.is_final() && !t.to.iter().any(|a| a == author)),
        Body::TaskCancel(b) => state.tasks.get(&b.task).is_some_and(|t| t.author != author),
        _ => false,
    }
}

fn member_set(
    state: &SpaceState,
    entry: &PolicyEntry,
    ctx: &Ctx,
    role: Option<Role>,
    b: &MemberSet,
) -> Verdict {
    let (author, tgt) = (entry.author.as_str(), b.actor.as_str());
    let cur = role_of(state, tgt);
    let pol = &state.policy;
    let owners = state.roles.values().filter(|r| **r == Role::Owner).count();
    let new = b.role.role();
    if cur == Some(Role::Owner) && new != Some(Role::Owner) && owners == 1 {
        return deny("would leave zero owners");
    }
    if reads(new) && pol.denies(server_of(tgt)) {
        return deny("target server denied");
    }
    if new == Some(Role::Owner) && !is_human(ctx, tgt) {
        return deny("owners must be human");
    }
    if reads(new)
        && !reads(cur)
        && let Some(prof) = ctx.profile(tgt)
        && prof.actor_kind != ActorKind::Human
    {
        if pol.allow_agents == AllowAgents::None {
            return deny("agents not allowed");
        }
        if pol.allow_agents == AllowAgents::WithOperator && !prof.operator_confirmed {
            return deny("agent needs confirmed operator");
        }
    }
    if role == Some(Role::Owner) {
        return Verdict::Allow;
    }
    if author == tgt && new.is_none() && !matches!(cur, None | Some(Role::Banned)) {
        return Verdict::Allow; // self-leave
    }
    if author == tgt && new == Some(Role::Member) && cur.is_none() && pol.join == JoinMode::Open {
        return Verdict::Allow; // open join
    }
    if role == Some(Role::Member)
        && pol.members_may_invite
        && cur.is_none()
        && matches!(new, Some(Role::Guest | Role::Member))
    {
        return Verdict::Allow; // invite
    }
    deny("member.set not permitted")
}

fn policy_set(state: &SpaceState, role: Option<Role>, b: &PolicySet) -> Verdict {
    if role != Some(Role::Owner) {
        return deny("owners only");
    }
    let new = &b.policy;
    if new.denies(&state.home) {
        return deny("can't deny the home's own server");
    }
    if state
        .roles
        .iter()
        .filter(|(_, r)| **r == Role::Owner)
        .all(|(a, _)| new.denies(server_of(a)))
    {
        return deny("can't deny every owner's server");
    }
    if !budget_change_ok(state, &new.budget) {
        return deny("budget unit can't change while reservations exist");
    }
    Verdict::Allow
}

/// P-03: under `with-operator`, a non-human actor needs a confirmed operator at every
/// propose, claim and budget draw.
fn accountable(state: &SpaceState, ctx: &Ctx, actor: &str) -> bool {
    let Some(p) = ctx.profile(actor) else {
        return false;
    };
    if p.actor_kind == ActorKind::Human {
        return true;
    }
    match state.policy.allow_agents {
        AllowAgents::None => false,
        AllowAgents::WithOperator => p.operator_confirmed,
        AllowAgents::Any => true,
    }
}

fn budgeted_tasks_of<'a>(state: &'a SpaceState, actor: &str) -> Vec<&'a str> {
    state
        .tasks
        .iter()
        .filter(|(_, t)| t.open && t.left.is_some() && t.to.iter().any(|a| a == actor))
        .map(|(id, _)| id.as_str())
        .collect()
}

fn task(
    state: &SpaceState,
    entry: &PolicyEntry,
    ctx: &Ctx,
    role: Option<Role>,
    b: &TaskBody,
) -> Verdict {
    let author = entry.author.as_str();
    if !reads(role) {
        return deny("author holds no role");
    }
    if b.to.iter().any(|a| !reads(role_of(state, a))) {
        return deny("assignee holds no role");
    }
    let parent: Option<&Task> = match &b.parent {
        Some(pid) => {
            let Some(p) = state.tasks.get(pid) else {
                return invalid("no such parent task");
            };
            if !p.open {
                return deny("parent task closed");
            }
            if !p.to.iter().any(|a| a == author) {
                return deny("only an assignee may delegate");
            }
            if p.depth + 1 > state.policy.max_delegation_depth {
                return deny("delegation too deep");
            }
            if p.left.is_some() && b.budget.is_none() {
                return deny("child of a budgeted task needs a budget");
            }
            Some(p)
        }
        None => {
            if !writes(role) {
                return deny("not allowed to create tasks");
            }
            None
        }
    };
    let _ = parent;
    if let Some(budget) = &b.budget {
        if !accountable(state, ctx, author) {
            return deny("an agent without a confirmed operator can't draw budget");
        }
        if !is_human(ctx, author)
            && !b
                .parent
                .as_deref()
                .is_some_and(|p| budgeted_tasks_of(state, author).contains(&p))
        {
            return deny("agents draw budget only beneath a budgeted task assigned to them");
        }
        let Some(unit) = space_unit(state) else {
            return deny("no space budget");
        };
        if budget.unit != unit {
            return deny("budget unit must be the space's unit");
        }
        let src = task_source(state, b.parent.as_deref());
        if budget.amount as i64 > remaining(state, &src) {
            return deny("budget exceeds what is left");
        }
    }
    if task_guarded_for(state, ctx, author, b.parent.as_deref()).len() > MAX_GUARDED_FOR - 2 {
        return deny("too many guarded tasks; close some"); // P-15, X6
    }
    Verdict::Allow
}

fn task_update(
    state: &SpaceState,
    entry: &PolicyEntry,
    role: Option<Role>,
    b: &TaskUpdate,
) -> Verdict {
    let author = entry.author.as_str();
    let Some(t) = state.tasks.get(&b.task) else {
        return invalid("no such task");
    };
    let is_final = b.state.is_final();
    if !t.open && !is_final {
        return deny("task closed"); // Z2: a final update still ends carries on a closed task
    }
    if !reads(role) {
        return deny("author holds no role");
    }
    let assignee = t.to.iter().any(|a| a == author);
    if role == Some(Role::Owner) && is_final && !assignee {
        return Verdict::Allow; // owner force-close (P-05)
    }
    if assignee {
        if is_final
            && state.effects.values().any(|e| {
                e.task.as_deref() == Some(b.task.as_str())
                    && matches!(e.phase, Phase::Claimed | Phase::Unknown)
            })
        {
            return deny("task has effects still running or of unknown outcome");
        }
        return Verdict::Allow;
    }
    deny("only assignees update")
}

fn task_cancel(
    state: &SpaceState,
    entry: &PolicyEntry,
    role: Option<Role>,
    b: &TaskCancel,
) -> Verdict {
    let Some(t) = state.tasks.get(&b.task) else {
        return invalid("no such task");
    };
    if !t.open {
        return deny("task closed");
    }
    if !reads(role) {
        return deny("author holds no role");
    }
    if entry.author != t.author && role != Some(Role::Owner) {
        return deny("only asker or owner");
    }
    Verdict::Allow
}

/// P-06 (round 33): a non-human's proposal of a revert in a budgeted space may name exactly the task its
/// reverted `vcs.land` named, if that task is open and budgeted. Then neither the assignee check nor the
/// agent's own-task check applies to it.
fn revert_task(state: &SpaceState, ctx: &Ctx, author: &str, b: &Propose) -> bool {
    if space_unit(state).is_none() || is_human(ctx, author) || b.action != "vcs.land" {
        return false;
    }
    let (Some(task), Some(of)) = (
        b.task.as_deref(),
        b.change.as_ref().and_then(|c| c.revert_of.as_ref()),
    ) else {
        return false;
    };
    let Some(target) = state.effects.get(of) else {
        return false;
    };
    target.action == "vcs.land"
        && target.phase == Phase::Succeeded
        && target.task.as_deref() == Some(task)
        && state
            .tasks
            .get(task)
            .is_some_and(|t| t.open && t.left.is_some())
}

fn propose(
    state: &SpaceState,
    entry: &PolicyEntry,
    ctx: &Ctx,
    role: Option<Role>,
    b: &Propose,
) -> Verdict {
    let author = entry.author.as_str();
    if !writes(role) {
        return deny("not allowed to propose");
    }
    let revert_task = revert_task(state, ctx, author, b);
    if let Some(tid) = &b.task {
        let Some(t) = state.tasks.get(tid) else {
            return invalid("no such task");
        };
        if !t.open {
            return deny("task not open");
        }
        if !t.to.iter().any(|a| a == author) && role != Some(Role::Owner) && !revert_task {
            return deny("only an assignee or owner charges a task");
        }
    }
    let unit = space_unit(state);
    if let Some(unit) = unit
        && b.cost.as_ref().is_none_or(|c| c.unit != unit)
    {
        return deny("budgeted space: cost in the space's unit required");
    }
    if !accountable(state, ctx, author) {
        return deny("agent's operator confirmation withdrawn");
    }
    if unit.is_some()
        && !is_human(ctx, author)
        && !revert_task
        && !b
            .task
            .as_deref()
            .is_some_and(|t| budgeted_tasks_of(state, author).contains(&t))
    {
        return deny("agents spend only from a budgeted task assigned to them");
    }
    let guarded = effect_guarded_for(state, ctx, author, &b.executor, b.task.as_deref());
    if over_bound(&guarded, &[author, &b.executor]) {
        return deny("too many guarded tasks; close some"); // P-15: the bound is kept, never exceeded
    }
    if b.change.is_some() && b.action != "vcs.land" {
        return invalid("a change summary belongs to a vcs.land"); // P-17
    }
    if let Some(of) = b.change.as_ref().and_then(|c| c.revert_of.as_ref()) {
        // A2: a revert names a vcs.land of this space that has landed
        let Some(target) = state.effects.get(of).filter(|e| e.action == "vcs.land") else {
            return invalid("revertOf names no vcs.land of this space");
        };
        if target.phase != Phase::Succeeded {
            return deny("revertOf names a vcs.land that has not succeeded");
        }
    }
    Verdict::Allow
}

fn approve(
    state: &SpaceState,
    entry: &PolicyEntry,
    ctx: &Ctx,
    role: Option<Role>,
    b: &Approve,
) -> Verdict {
    let author = entry.author.as_str();
    let Some(eff) = state.effects.get(&b.proposal) else {
        return invalid("no such proposal");
    };
    if b.plan_digest != eff.plan_digest {
        return invalid("planDigest mismatch");
    }
    if !matches!(eff.phase, Phase::Open | Phase::Claimed) {
        return deny("cannot approve in this phase");
    }
    let rule = rule_for(state, eff);
    if b.expires_at.as_deref().is_some_and(|e| secs(e) <= ctx.secs) {
        return deny("approval already expired");
    }
    let guards = guards_of(state, ctx, eff);
    if !(in_guards(&guards, author) && reads(role)) {
        // a guard approves as a guard (P-15): no rule, no cap
        let kind = ctx
            .profile(author)
            .map_or(ActorKind::Human, |p| p.actor_kind);
        if rule.is_some_and(|r| r.proposer == Some(true)) {
            return deny("the rule asks no approval: the proposer suffices");
        }
        if !rule.is_some_and(|r| counts(state, eff, r, author, kind, b.evidence.as_ref())) {
            return deny("not an approver for this action");
        }
        if state.policy.separate_approver
            && author == eff.proposer
            && !rule.is_some_and(|r| self_revert_approval(eff, r, author, kind))
        {
            return deny("separate approver required");
        }
        let counted = approval_candidates(state, eff, ctx.secs)
            .into_iter()
            .filter(|a| a.author != author && !in_guards(&guards, &a.author))
            .count();
        if counted >= MAX_OPEN_APPROVALS {
            return deny("too many open approvals"); // only E3 candidates count (P-08)
        }
    }
    Verdict::Allow
}

fn claim(
    state: &SpaceState,
    entry: &PolicyEntry,
    ctx: &Ctx,
    role: Option<Role>,
    b: &Claim,
) -> Verdict {
    let author = entry.author.as_str();
    let Some(eff) = state.effects.get(&b.proposal) else {
        return invalid("no such proposal");
    };
    if author != eff.executor || b.instance != entry.key_id {
        return deny("E1 not the named executor instance");
    }
    if !writes(role) {
        return deny("E1 executor no longer a member");
    }
    if !accountable(state, ctx, author) {
        return deny("E1 executor's operator confirmation withdrawn");
    }
    if eff
        .expires_at
        .as_deref()
        .is_some_and(|e| secs(e) <= ctx.secs)
    {
        return deny("E5 proposal expired");
    }
    if !writes(role_of(state, &eff.proposer))
        || key_status(ctx, &eff.proposer, &eff.proposer_key) != Key::Valid
    {
        return deny("E2 proposer no longer authorized");
    }
    if !accountable(state, ctx, &eff.proposer) {
        return deny("E2 proposer's operator confirmation withdrawn");
    }
    if b.window != state.policy.launch_window {
        return deny("E5 claim's window is not the space's launchWindow");
    }
    if asks_approval(state, eff) {
        match pick_approval(state, eff, ctx).0 {
            Pick::Unpinned => return invalid("approver profile not pinned"),
            Pick::None => return deny("E3 no usable approval"),
            Pick::Ok => {}
        }
    }
    if unbounded_guards(state, ctx, eff) {
        return deny(
            "E3 the proposer or executor took on guarding the proposal never counted: propose again",
        ); // Z1
    }
    for g in guards_of(state, ctx, eff) {
        let Some(g) = g else {
            return deny("E3 a guarded agent has no confirmed operator");
        };
        match guard_approval(state, eff, &g, ctx).0 {
            Pick::Unpinned => return invalid("guard profile not pinned"),
            Pick::None => return deny("E3 a guard hasn't approved"),
            Pick::Ok => {}
        }
    }
    if eff.cancel_requested {
        return deny("E4 cancel requested");
    }
    if eff.phase != Phase::Open {
        return deny("E5 phase is not open");
    }
    if eff
        .task
        .as_deref()
        .and_then(|t| state.tasks.get(t))
        .is_some_and(|t| !t.open)
    {
        return deny("E5 the effect's task is closed");
    }
    if b.attempt != eff.claims.len() as u64 + 1 {
        return deny("E5 attempt number");
    }
    if eff.claims.len() >= MAX_ATTEMPTS {
        return deny("E5 attempt limit: propose again");
    }
    if let Some(unit) = space_unit(state) {
        let Some(cost) = eff.cost.as_ref().filter(|c| c.unit == unit) else {
            return deny("E6 uncosted effect in budgeted space");
        };
        let src = effect_source(state, eff);
        if remaining(state, &src) < cost.amount as i64 {
            return deny("E6 budget");
        }
        if src == Source::Space && !is_human(ctx, &eff.proposer) {
            return deny("E6 agents spend only from a budgeted task assigned to them");
        }
    }
    Verdict::Allow
}

fn cancel(
    state: &SpaceState,
    entry: &PolicyEntry,
    ctx: &Ctx,
    role: Option<Role>,
    b: &EffectCancel,
) -> Verdict {
    let Some(eff) = state.effects.get(&b.proposal) else {
        return invalid("no such proposal");
    };
    let a = entry.author.as_str();
    if !reads(role) {
        return deny("author holds no role");
    }
    // a guarded author's approval grants no cancel right (P-04); a consumed approval, or an
    // unexpired one, is a standing approval
    let approved = !guard_status(state, ctx, a).guarded
        && eff
            .approvals
            .iter()
            .any(|x| x.author == a && (x.consumed_by.is_some() || !expired(x, ctx.secs)));
    // round 33: the executor may refuse an effect no attempt is running on
    let refusal = a == eff.executor && eff.phase == Phase::Open;
    if !(a == eff.proposer
        || role == Some(Role::Owner)
        || in_guards(&guards_of(state, ctx, eff), a)
        || approved
        || refusal)
    {
        return deny("not proposer/approver/owner");
    }
    if eff.phase.is_final() {
        return deny("already final");
    }
    if matches!(eff.phase, Phase::Claimed | Phase::Unknown) && eff.cancel_requested {
        return deny("already requested");
    }
    Verdict::Allow
}

/// P-06: a reported cost must fit the claim's reservation; with no reservation it is allowed
/// only when the space has no budget (then it is informational).
fn cost_ok(
    state: &SpaceState,
    cost: &crate::types::Budget,
    reserved: Option<&crate::types::Budget>,
) -> bool {
    match reserved {
        Some(r) => cost.unit == r.unit && cost.amount <= r.amount,
        None => space_unit(state).is_none(),
    }
}

/// The claim with this id and the effect it belongs to.
pub(super) fn find_claim<'a>(
    state: &'a SpaceState,
    claim_id: &str,
) -> Option<(&'a str, &'a Effect, usize)> {
    state.effects.iter().find_map(|(pid, eff)| {
        eff.claims
            .iter()
            .position(|c| c.id == claim_id)
            .map(|i| (pid.as_str(), eff, i))
    })
}

fn valid_pair(result: EffectResult, started: Started) -> bool {
    matches!(
        (result, started),
        (EffectResult::Succeeded, Started::Yes)
            | (
                EffectResult::Failed | EffectResult::Cancelled,
                Started::Yes | Started::No
            )
            | (EffectResult::Unknown, Started::Yes | Started::Unknown)
    )
}

fn outcome(
    state: &SpaceState,
    entry: &PolicyEntry,
    ctx: &Ctx,
    role: Option<Role>,
    b: &Outcome,
) -> Verdict {
    let Some((_, eff, i)) = find_claim(state, &b.claim) else {
        return invalid("no such claim");
    };
    let c = &eff.claims[i];
    if i + 1 != eff.claims.len() || eff.phase != Phase::Claimed {
        return deny("claim not open");
    }
    if !valid_pair(b.result, b.started) {
        return invalid("inconsistent result/started");
    }
    if let Some(cost) = &b.cost {
        if b.started != Started::Yes {
            return invalid("cost only with started:yes");
        }
        if !cost_ok(state, cost, c.reserved.as_ref()) {
            return invalid("cost not covered by the reservation");
        }
    }
    if entry.author == eff.executor && entry.key_id == c.instance {
        return Verdict::Allow;
    }
    if role == Some(Role::Owner)
        && b.result == EffectResult::Unknown
        && b.started == Started::Unknown
        && b.cost.is_none()
    {
        if ctx.secs >= secs(&c.time).saturating_add(abandon_after(c.window) as i64) {
            return Verdict::Allow;
        }
        return deny("abandon before the launch window closed");
    }
    deny("only the claiming instance reports")
}

fn resolve(state: &SpaceState, entry: &PolicyEntry, role: Option<Role>, b: &Resolve) -> Verdict {
    let Some(eff) = state.effects.get(&b.proposal) else {
        return invalid("no such proposal");
    };
    if eff.phase != Phase::Unknown {
        return deny("nothing to resolve");
    }
    if entry.author != eff.executor && role != Some(Role::Owner) {
        return deny("executor or owner only");
    }
    if let Some(cost) = &b.cost {
        if b.result == ResolveResult::Failed {
            return invalid("no cost on failed");
        }
        let reserved = eff.claims.last().and_then(|c| c.reserved.as_ref());
        if !cost_ok(state, cost, reserved) {
            return invalid("cost not covered by the reservation");
        }
    }
    Verdict::Allow
}

fn redact(state: &SpaceState, entry: &PolicyEntry, role: Option<Role>, b: &Redact) -> Verdict {
    let Some(t) = state.entries.get(&b.target) else {
        return invalid("no such entry");
    };
    if t.kind == "redact" {
        return deny("cannot redact a redact");
    }
    if t.kind == "effect.propose"
        && state
            .effects
            .get(&b.target)
            .is_some_and(|e| !e.phase.is_final())
    {
        return deny("the plan of an unfinished effect can't be redacted; cancel it first");
    }
    if entry.author == t.author || role == Some(Role::Owner) {
        Verdict::Allow
    } else {
        deny("author or owner only")
    }
}

fn space_move(state: &SpaceState, role: Option<Role>) -> Verdict {
    if role != Some(Role::Owner) {
        return deny("owners only");
    }
    if state.effects.values().any(|e| e.phase == Phase::Claimed) {
        return deny("cannot move while an effect is claimed");
    }
    Verdict::Allow
}

fn session_open(
    state: &SpaceState,
    entry: &PolicyEntry,
    ctx: &Ctx,
    role: Option<Role>,
    b: &SessionOpen,
) -> Verdict {
    let author = entry.author.as_str();
    if !reads(role) {
        return deny("author holds no role");
    }
    if !accountable(state, ctx, author) {
        return deny("agent's operator confirmation withdrawn");
    }
    if !state.policy.sessions.modes.contains(&b.steer_mode) {
        return deny("steer mode not allowed in this space");
    }
    if b.steer_mode == SteerMode::Guarded && !is_human(ctx, author) {
        let guard = guard_status(state, ctx, author).guard;
        if !reads(guard.as_deref().and_then(|g| role_of(state, g))) {
            return deny(
                "a guarded agent session needs a confirmed operator who holds a role here",
            );
        }
    }
    if let Some(tid) = &b.task {
        let Some(task) = state.tasks.get(tid) else {
            return invalid("no such task");
        };
        if !task.open {
            return deny("task closed");
        }
        if !task.to.iter().any(|a| a == author) && role != Some(Role::Owner) {
            return deny("only an assignee or owner works a task");
        }
    }
    if state
        .sessions
        .values()
        .filter(|x| x.actor == author)
        .count()
        >= MAX_OPEN_SESSIONS
    {
        return deny("too many open sessions for this actor");
    }
    Verdict::Allow
}

fn session_close(
    state: &SpaceState,
    entry: &PolicyEntry,
    role: Option<Role>,
    b: &SessionRef,
) -> Verdict {
    let Some(sess) = state.sessions.get(&b.session) else {
        return invalid("no such session");
    };
    if entry.author != sess.actor && role != Some(Role::Owner) {
        return deny("only the session's actor or an owner closes it");
    }
    Verdict::Allow
}

fn session_input(state: &SpaceState, entry: &PolicyEntry, ctx: &Ctx, b: &SessionRef) -> Verdict {
    let Some(sess) = state.sessions.get(&b.session) else {
        return invalid("no such session");
    };
    if !live(state, sess) {
        return deny("session not live");
    }
    if !accountable(state, ctx, &entry.author) {
        return deny("agent's operator confirmation withdrawn");
    }
    if !session_access(state, &entry.author, &b.session).steer {
        return deny("no steer right on this session");
    }
    Verdict::Allow
}

fn agent_unguard(
    state: &SpaceState,
    entry: &PolicyEntry,
    ctx: &Ctx,
    role: Option<Role>,
    x: &str,
) -> Verdict {
    if !state.guarded.contains(x)
        || guard_status(state, ctx, x).guard.as_deref() != Some(entry.author.as_str())
    {
        return deny("only the guard of an agent that chose guarding unguards it");
    }
    if reads(role) {
        Verdict::Allow
    } else {
        deny("guard holds no role")
    }
}

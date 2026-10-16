//! `fold`: the state after a record the Log ordered (architecture.md sections 4.3 and 6).

use std::collections::{BTreeMap, BTreeSet};

use super::body::{
    Approve, Body, EffectResult, Outcome, PolicyEntry, ResolveResult, SpaceCreate, Started,
    TaskBody, TaskState,
};
use super::decide::find_claim;
use super::doc::{Role, SteerMode};
use super::gate::gate_of;
use super::guard::{
    approval_candidates, approval_dead, asks_approval, effect_guarded_for, effect_source, expired,
    guard_approval, guards_of, is_human, pick_approval, rebudget, role_of, server_of, space_unit,
    task_guarded_for, task_source,
};
use super::state::{
    ActorSet, Approval, ClaimState, Effect, EntryRef, Interval, KeyState, Phase, ProfileState,
    Remaining, Session, Source, SpaceState, Task,
};
use super::{Ctx, ROLE_INTERVALS, Record};
use crate::error::Error;
use crate::types::EntryId;

/// The state after `record`, given the state before it (`None` before the first record). Only a
/// record `decide` allowed has a defined effect; for anything else the result is still a state, but
/// not a meaningful one. Fails only when there is no state and the record is not a
/// `space.create`, or when it lacks a pinned profile for an approval's author.
pub fn fold(state: Option<&SpaceState>, record: &Record, ctx: &Ctx) -> Result<SpaceState, Error> {
    let e = &record.entry;
    let Some(state) = state else {
        return fold_create(e, record);
    };
    let mut s = state.clone();
    s.head = record.seq;
    let eid = record.id.clone();
    s.entries.insert(
        eid.clone(),
        EntryRef {
            author: e.author.clone(),
            kind: e.kind.clone(),
        },
    );
    let profile_space = s.space.starts_with("profile:");
    let author = e.author.as_str();
    let mut touched: Option<EntryId> = None; // the effect an `effect.*` entry touches

    match &e.body {
        Body::MemberSet(b) if !profile_space => {
            let tgt = &b.actor;
            let before = s.roles.get(tgt).is_some_and(|r| r.reads());
            let new = b.role.role();
            match new {
                None => {
                    s.roles.remove(tgt);
                }
                Some(r) => {
                    s.roles.insert(tgt.clone(), r);
                }
            }
            let now = new.is_some_and(Role::reads);
            if !now {
                drop_open_approvals(&mut s, tgt); // P-08: they could never count again
                for t in s.tasks.values_mut() {
                    t.accepted.retain(|a| a != tgt); // Z2: an actor that leaves carries nothing
                }
            }
            let hist = s.role_history.entry(tgt.clone()).or_default();
            if now && !before {
                hist.push(Interval {
                    from: record.seq,
                    to: None,
                });
                if hist.len() > ROLE_INTERVALS {
                    hist.drain(..hist.len() - ROLE_INTERVALS);
                }
            }
            if before
                && !now
                && let Some(last) = hist.last_mut()
            {
                last.to = Some(record.seq);
            }
        }
        Body::MemberSet(b) => {
            if let Some(p) = &mut s.profile {
                p.operator_confirmed = b.role == super::doc::NewRole::Owner;
            }
        }
        Body::KeyAdd(b) => {
            if let Some(p) = &mut s.profile {
                p.keys.insert(
                    b.key_id.clone(),
                    KeyState {
                        public_key: b.public_key.clone(),
                        status: super::KeyStatus::Valid,
                    },
                );
            }
        }
        Body::KeyRevoke(b) => {
            if let Some(k) = s.profile.as_mut().and_then(|p| p.keys.get_mut(&b.key_id)) {
                k.status = super::KeyStatus::Revoked;
            }
        }
        Body::PolicySet(b) => {
            s.budget_left = rebudget(&s.policy.budget, &s.budget_left, &b.policy.budget);
            s.policy = b.policy.clone();
            // P-08: forcing on drops the open approvals of the authors it guards; a deny, those of
            // the servers it denies (X7)
            let mut effects = std::mem::take(&mut s.effects);
            for eff in effects.values_mut() {
                eff.approvals.retain(|a| {
                    a.consumed_by.is_some() || !approval_dead(&s.guarded, &s.policy, a)
                });
            }
            s.effects = effects;
        }
        Body::Task(b) => fold_task(&mut s, ctx, e, b, record),
        Body::TaskUpdate(b) => {
            let (was_open, assignee) = match s.tasks.get(&b.task) {
                Some(t) => (t.open, t.to.iter().any(|a| a == author)),
                None => return Ok(s),
            };
            let mut credit_back = None;
            if let Some(t) = s.tasks.get_mut(&b.task) {
                if was_open {
                    t.state = b.state; // a closed task only loses acceptances (Z2)
                }
                if b.state == TaskState::Accepted && !t.accepted.iter().any(|a| a == author) {
                    t.accepted.push(author.to_owned()); // P-15, X4: only an explicit accept accepts
                }
                if b.state.is_final() {
                    // X1, Z2: an assignee's own final update ends what it carries; an owner's
                    // force-close, all, on a closed task too
                    if assignee {
                        t.accepted.retain(|a| a != author);
                    } else {
                        t.accepted.clear();
                    }
                }
                if b.state.is_final() && was_open {
                    t.open = false;
                    if let Some(left) = &mut t.left {
                        let amount = left.amount;
                        left.amount = 0;
                        credit_back = Some((t.parent.clone(), amount));
                    }
                }
            }
            if let Some((parent, amount)) = credit_back {
                credit(&mut s, parent.as_deref(), amount);
            }
        }
        Body::TaskCancel(b) => {
            if let Some(t) = s.tasks.get_mut(&b.task) {
                t.cancel_requested = true; // Z2: a cancel only asks; it ends no carry
            }
        }
        Body::Propose(b) => {
            let eff = Effect {
                proposer: author.to_owned(),
                proposer_key: e.key_id.clone(),
                executor: b.executor.clone(),
                action: b.action.clone(),
                plan_digest: b.plan_digest.clone(),
                seq: record.seq,
                cost: b.cost.clone(),
                repeatable: b.repeatable,
                task: b.task.clone(),
                phase: Phase::Open,
                cancel_requested: false,
                // P-15: fixed now, never shrinks
                guarded_for: effect_guarded_for(&s, ctx, author, &b.executor, b.task.as_deref()),
                // P-17: fixed now, from the policy in force
                gate: gate_of(
                    &s.policy,
                    &b.action,
                    b.change.as_ref(),
                    &s.effects,
                    ctx,
                    author,
                ),
                expires_at: b.expires_at.clone(),
                approvals: Vec::new(),
                claims: Vec::new(),
            };
            s.effects.insert(eid.clone(), eff);
            touched = Some(eid.clone());
        }
        Body::Approve(b) => {
            touched = fold_approve(&mut s, ctx, author, e, b, &eid)?;
        }
        Body::Claim(b) => {
            touched = fold_claim(&mut s, ctx, b, &eid, record);
        }
        Body::EffectCancel(b) => {
            if let Some(eff) = s.effects.get_mut(&b.proposal) {
                if eff.phase == Phase::Open {
                    eff.phase = Phase::Cancelled;
                }
                eff.cancel_requested = true;
            }
            touched = Some(b.proposal.clone());
        }
        Body::Outcome(b) => {
            touched = fold_outcome(&mut s, b, author, &e.key_id);
        }
        Body::Resolve(b) => {
            let Some(eff) = s.effects.get(&b.proposal) else {
                return Ok(s);
            };
            let claim = eff.claims.len().checked_sub(1);
            let again = reopen(eff);
            if b.result == ResolveResult::Succeeded {
                settle(
                    &mut s,
                    &b.proposal,
                    claim,
                    b.cost.as_ref().map(|c| c.amount as i64),
                );
                set_phase(&mut s, &b.proposal, Phase::Succeeded);
            } else {
                settle(&mut s, &b.proposal, claim, Some(0));
                if let Some(eff) = s.effects.get_mut(&b.proposal) {
                    eff.approvals.retain(|a| a.consumed_by.is_some());
                }
                set_phase(&mut s, &b.proposal, again);
            }
            prune_approvals(&mut s, &b.proposal);
            touched = Some(b.proposal.clone());
        }
        Body::SpaceMove(b) => s.home = b.to.clone(),
        Body::SessionOpen(b) => {
            s.sessions.insert(
                eid.clone(),
                Session {
                    actor: author.to_owned(),
                    opened: record.time.clone(),
                    steer_mode: b.steer_mode,
                    steers: 0,
                    task: b.task.clone(),
                },
            );
            if b.steer_mode == SteerMode::Guarded && !is_human(ctx, author) {
                s.guarded.0.insert(author.to_owned()); // sticky: only its guard unguards it
                drop_open_approvals(&mut s, author); // P-08: a guarded author's approval never counts
            }
        }
        Body::SessionClose(b) => {
            s.sessions.remove(&b.session); // F3(b): closed sessions leave the state
        }
        Body::SessionInput(b) => {
            if let Some(sess) = s.sessions.get_mut(&b.session) {
                sess.steers += 1;
            }
        }
        Body::AgentUnguard(b) => {
            s.guarded.0.remove(&b.agent); // only the guard loosens its agent
        }
        _ => {}
    }
    if let Some(pid) = touched {
        // K4: every entry touching an effect drops its expired approvals
        let now = ctx.secs;
        if let Some(eff) = s.effects.get_mut(&pid) {
            eff.approvals
                .retain(|a| a.consumed_by.is_some() || !expired(a, now));
        }
    }
    Ok(s)
}

fn fold_create(e: &PolicyEntry, record: &Record) -> Result<SpaceState, Error> {
    let Body::SpaceCreate(SpaceCreate {
        policy, profile, ..
    }) = &e.body
    else {
        return Err(Error::invalid("the first record must be a space.create"));
    };
    let space = e.space.clone();
    let mut base = SpaceState {
        home: server_of(&space).to_owned(),
        head: record.seq,
        policy: policy.clone(),
        roles: BTreeMap::new(),
        role_history: BTreeMap::new(),
        tasks: BTreeMap::new(),
        effects: BTreeMap::new(),
        budget_left: policy.budget.as_ref().map(|b| Remaining {
            unit: b.unit.clone(),
            amount: b.amount as i64,
        }),
        entries: BTreeMap::from([(
            record.id.clone(),
            EntryRef {
                author: e.author.clone(),
                kind: "space.create".to_owned(),
            },
        )]),
        sessions: BTreeMap::new(),
        guarded: ActorSet::default(),
        profile: None,
        space,
    };
    if let Some(actor) = base.space.strip_prefix("profile:") {
        let p = profile
            .as_ref()
            .ok_or_else(|| Error::invalid("a profile space needs a profile"))?;
        base.roles = BTreeMap::from([
            (actor.to_owned(), Role::Owner),
            (e.author.clone(), Role::Owner),
        ]);
        base.profile = Some(ProfileState {
            actor_kind: p.actor_kind,
            operator: p.operator.clone(),
            operator_confirmed: false,
            keys: BTreeMap::from([(
                p.key.key_id.clone(),
                KeyState {
                    public_key: p.key.public_key.clone(),
                    status: super::KeyStatus::Valid,
                },
            )]),
        });
        return Ok(base);
    }
    base.roles = BTreeMap::from([(e.author.clone(), Role::Owner)]); // plain or fork (P-13): fresh
    base.role_history = BTreeMap::from([(
        e.author.clone(),
        vec![Interval {
            from: record.seq,
            to: None,
        }],
    )]);
    Ok(base)
}

fn fold_task(s: &mut SpaceState, ctx: &Ctx, e: &PolicyEntry, b: &TaskBody, record: &Record) {
    let author = e.author.as_str();
    let parent_depth = b
        .parent
        .as_deref()
        .and_then(|p| s.tasks.get(p))
        .map(|p| p.depth);
    let mut t = Task {
        author: author.to_owned(),
        to: b.to.clone(),
        open: true,
        state: TaskState::Pending,
        cancel_requested: false,
        depth: parent_depth.map_or(0, |d| d + 1),
        seq: record.seq,
        // P-15: an author assigning itself accepts at once
        accepted: if b.to.iter().any(|a| a == author) {
            vec![author.to_owned()]
        } else {
            Vec::new()
        },
        // P-15: fixed now, never shrinks
        guarded_for: task_guarded_for(s, ctx, author, b.parent.as_deref()),
        parent: parent_depth.and(b.parent.clone()),
        budget: None,
        left: None,
    };
    if let Some(budget) = &b.budget {
        let src = task_source(s, b.parent.as_deref());
        debit(s, &src, budget.amount as i64);
        t.budget = Some(budget.clone());
        t.left = Some(Remaining {
            unit: budget.unit.clone(),
            amount: budget.amount as i64,
        });
    }
    s.tasks.insert(record.id.clone(), t);
}

fn fold_approve(
    s: &mut SpaceState,
    ctx: &Ctx,
    author: &str,
    e: &PolicyEntry,
    b: &Approve,
    eid: &str,
) -> Result<Option<EntryId>, Error> {
    let Some(eff) = s.effects.get(&b.proposal) else {
        return Ok(None);
    };
    let guards = guards_of(s, ctx, eff);
    let mut keep: BTreeSet<&str> = approval_candidates(s, eff, ctx.secs)
        .into_iter()
        .map(|a| a.id.as_str())
        .collect();
    keep.extend(
        eff.approvals
            .iter()
            .filter(|x| super::guard::in_guards(&guards, &x.author))
            .map(|x| x.id.as_str()),
    );
    // a new approval supersedes (and drops) the author's open ones, and the open ones that don't
    // count (P-08)
    let kept: Vec<Approval> = eff
        .approvals
        .iter()
        .filter(|old| {
            old.consumed_by.is_some() || (old.author != author && keep.contains(old.id.as_str()))
        })
        .cloned()
        .collect();
    let actor_kind = ctx
        .profile(author)
        .ok_or_else(|| Error::invalid("no pinned profile for the approval's author"))?
        .actor_kind;
    let mut approvals = kept;
    approvals.push(Approval {
        id: eid.to_owned(),
        author: author.to_owned(),
        key_id: e.key_id.clone(),
        actor_kind,
        expires_at: b.expires_at.clone(),
        consumed_by: None,
        evidence: b.evidence.clone(),
    });
    if let Some(eff) = s.effects.get_mut(&b.proposal) {
        eff.approvals = approvals;
    }
    Ok(Some(b.proposal.clone()))
}

fn fold_claim(
    s: &mut SpaceState,
    ctx: &Ctx,
    b: &super::body::Claim,
    eid: &str,
    record: &Record,
) -> Option<EntryId> {
    let eff = s.effects.get(&b.proposal)?;
    let mut c = ClaimState {
        id: eid.to_owned(),
        attempt: b.attempt,
        instance: b.instance.clone(),
        time: record.time.clone(),
        window: b.window,
        approval: None,
        co_approvals: None,
        guard_approvals: None,
        reserved: None,
        source: None,
        settled: false,
        outcome: None,
        started: None,
    };
    let mut chosen: Vec<(String, String)> = Vec::new(); // (approval id, author)
    if asks_approval(s, eff) {
        let picked = pick_approval(s, eff, ctx).1;
        for (i, a) in picked.iter().enumerate() {
            chosen.push((a.id.clone(), a.author.clone()));
            if i == 0 {
                c.approval = Some(a.id.clone());
            } else {
                c.co_approvals
                    .get_or_insert_with(Vec::new)
                    .push(a.id.clone());
            }
        }
    }
    for g in guards_of(s, ctx, eff).into_iter().flatten() {
        let found = chosen
            .iter()
            .find(|(_, author)| *author == g)
            .cloned()
            .or_else(|| {
                guard_approval(s, eff, &g, ctx)
                    .1
                    .map(|a| (a.id.clone(), a.author.clone()))
            });
        let Some(a) = found else {
            continue; // unreachable when decide allowed the claim
        };
        if !chosen.contains(&a) {
            chosen.push(a.clone());
        }
        c.guard_approvals.get_or_insert_with(Vec::new).push(a.0);
    }
    let reserve = space_unit(s)
        .and(eff.cost.clone())
        .map(|cost| (effect_source(s, eff), cost));
    if let Some(eff) = s.effects.get_mut(&b.proposal) {
        for a in &mut eff.approvals {
            if chosen.iter().any(|(id, _)| *id == a.id) {
                a.consumed_by = Some(eid.to_owned());
            }
        }
    }
    if let Some((src, cost)) = reserve {
        debit(s, &src, cost.amount as i64);
        c.reserved = Some(cost);
        c.source = Some(src);
    }
    if let Some(eff) = s.effects.get_mut(&b.proposal) {
        eff.claims.push(c);
        eff.phase = Phase::Claimed;
    }
    Some(b.proposal.clone())
}

fn fold_outcome(s: &mut SpaceState, b: &Outcome, author: &str, key_id: &str) -> Option<EntryId> {
    let (pid, _, i) = find_claim(s, &b.claim)?;
    let pid = pid.to_owned();
    {
        let c = &mut s.effects.get_mut(&pid)?.claims[i];
        c.outcome = Some(b.result);
        c.started = Some(b.started);
    }
    let eff = &s.effects[&pid];
    let (repeatable, cancel_requested, executor) =
        (eff.repeatable, eff.cancel_requested, eff.executor.clone());
    let instance = eff.claims[i].instance.clone();
    let again = reopen(eff);
    if b.started == Started::No {
        settle(s, &pid, Some(i), Some(0));
        let c = &s.effects[&pid].claims[i];
        let mut back: BTreeSet<String> = c.guard_approvals.iter().flatten().cloned().collect();
        back.extend(c.approval.clone());
        back.extend(c.co_approvals.iter().flatten().cloned());
        let mut gone: Vec<String> = Vec::new();
        let mut approvals = s.effects[&pid].approvals.clone();
        for ai in 0..approvals.len() {
            if !back.contains(&approvals[ai].id) {
                continue;
            }
            approvals[ai].consumed_by = None;
            let a = approvals[ai].clone();
            let superseded = approvals.iter().enumerate().any(|(ni, n)| {
                n.author == a.author
                    && n.consumed_by.is_none()
                    && ni != ai
                    && !gone.contains(&n.id)
                    && ni > ai
            });
            if superseded {
                gone.push(a.id.clone()); // superseded while it was consumed
            } else if !super::guard::reads(role_of(s, &a.author))
                || approval_dead(&s.guarded, &s.policy, &a)
            {
                gone.push(a.id.clone()); // its author lost its role, became guarded or was denied meanwhile (P-08)
            }
        }
        approvals.retain(|a| !gone.contains(&a.id));
        if let Some(eff) = s.effects.get_mut(&pid) {
            eff.approvals = approvals;
        }
        set_phase(s, &pid, again);
    } else if b.result == EffectResult::Unknown {
        if repeatable && !cancel_requested && author == executor && key_id == instance {
            settle(s, &pid, Some(i), None); // charged in full, then retry
            set_phase(s, &pid, again);
        } else {
            set_phase(s, &pid, Phase::Unknown);
        }
    } else {
        settle(s, &pid, Some(i), b.cost.as_ref().map(|c| c.amount as i64));
        if b.result == EffectResult::Succeeded {
            set_phase(s, &pid, Phase::Succeeded);
        } else if repeatable && !cancel_requested {
            set_phase(s, &pid, again);
        } else {
            set_phase(s, &pid, Phase::Failed);
        }
    }
    prune_approvals(s, &pid);
    Some(pid)
}

/// The phase after an attempt ends without success: `cancelled` if asked, else `open` (E3 at the
/// next claim decides whether it can proceed).
fn reopen(eff: &Effect) -> Phase {
    if eff.cancel_requested {
        Phase::Cancelled
    } else {
        Phase::Open
    }
}

fn set_phase(s: &mut SpaceState, pid: &str, phase: Phase) {
    if let Some(eff) = s.effects.get_mut(pid) {
        eff.phase = phase;
    }
}

/// P-08: the fold drops `actor`'s unconsumed approvals on every effect when it loses its role or
/// chooses guarding (a consumed one stays with its claim; if a not-started outcome returns it, it
/// is dropped then).
fn drop_open_approvals(s: &mut SpaceState, actor: &str) {
    for eff in s.effects.values_mut() {
        eff.approvals
            .retain(|a| a.author != actor || a.consumed_by.is_some());
    }
}

/// F3(a): a consumed approval goes once its claim has settled.
fn prune_approvals(s: &mut SpaceState, pid: &str) {
    if let Some(eff) = s.effects.get_mut(pid) {
        let done: BTreeSet<&str> = eff
            .claims
            .iter()
            .filter(|c| c.settled)
            .map(|c| c.id.as_str())
            .collect();
        let keep: Vec<Approval> = eff
            .approvals
            .iter()
            .filter(|a| !a.consumed_by.as_deref().is_some_and(|c| done.contains(c)))
            .cloned()
            .collect();
        eff.approvals = keep;
    }
}

fn debit(s: &mut SpaceState, src: &Source, amount: i64) {
    match src {
        Source::Task(t) => {
            if let Some(left) = s.tasks.get_mut(t).and_then(|t| t.left.as_mut()) {
                left.amount -= amount;
            }
        }
        Source::Space => {
            if let Some(left) = &mut s.budget_left {
                left.amount -= amount;
            }
        }
    }
}

/// Settles a claim once (`settled` is on every claim): `spent` of `None` charges the reservation in
/// full, `Some(n)` refunds reserved minus `n` (P-06). A claim without a reservation is marked
/// settled and nothing moves.
fn settle(s: &mut SpaceState, pid: &str, claim: Option<usize>, spent: Option<i64>) {
    let Some(c) = claim.and_then(|i| s.effects.get_mut(pid).and_then(|e| e.claims.get_mut(i)))
    else {
        return;
    };
    if c.settled {
        return;
    }
    c.settled = true;
    let (Some(reserved), source) = (c.reserved.clone(), c.source.clone()) else {
        return;
    };
    let refund = spent.map_or(0, |n| reserved.amount as i64 - n);
    if refund != 0 {
        let task = match source {
            Some(Source::Task(t)) => Some(t),
            _ => None,
        };
        credit(s, task.as_deref(), refund);
    }
}

/// Returns `amount` to the nearest open budgeted task from `task_id` upward, else to the space
/// (P-05, P-06).
fn credit(s: &mut SpaceState, task_id: Option<&str>, amount: i64) {
    let mut cur = task_id.map(str::to_owned);
    while let Some(id) = cur {
        let Some(t) = s.tasks.get_mut(&id) else {
            break;
        };
        if t.open
            && let Some(left) = &mut t.left
        {
            left.amount += amount;
            return;
        }
        cur = t.parent.clone();
    }
    if let Some(left) = &mut s.budget_left {
        left.amount += amount;
    }
}

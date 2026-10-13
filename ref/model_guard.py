"""Bounded exhaustive check of P-15 guarded steering against the real policy.py (round 27: guarding is recorded on the work; round 28:
a guarded author's approval never counts, a guarded agent may leave, and an operator never changes; round 29: an assignee carries its
open tasks' guarding into all of its proposals, and the approval cap counts only approvals that can still count; round 30: only accepted
tasks carry guarding, a delegate carries the delegator's limits (`carried`), and approvals record their author's actorKind; round 31: a
delegate's carry ends only with its own final update or an owner's force-close, a carrying executor needs the guard, and only an explicit
accept accepts; round 32: carrying is guarding, so everything a carrying actor creates records guards(actor), a claim recomputes
guards(proposer) and guards(executor), a cancel ends no carry and leaving ends every one, and a guard's approval of its own agent's effect
is exempt from the Limits).

The property (DECISIONS-r27 P9), checked on every claim Policy allows:

    GuardApproved: no effect is claimed unless, for every agent that was guarded when the effect or any task it descends from
    was created, and for every agent guarded at the claim's seq, that agent's guard approved it.

"Approved it" is an approval by that guard which no earlier claim used up (a not-started outcome gives it back), while the
guard holds a role and its key is valid; an agent with no confirmed operator has no guard, so nothing that needs one may be
claimed. The ghost records guarding at creation itself, from the definitions of P-15 and never from Policy's state: an agent is
guarded when it chose to be (it opened a `guarded` session; only its guard ends that, with `agent.unguard`) or when owners force
it (`forceGuarded` on and it is not human, a service included); its guard is the confirmed operator in its current profile. An actor
*carries* the agents recorded on every task it accepted (a `task.update` "accepted" by an assignee, or its own task: X4) until its own
final update on that task, a final update by an owner who isn't an assignee (on a closed task too), or its leaving the space, ends it
(X1, Z2); a cancel ends nothing. guards(x) (Z1) is x itself if guarded, then what x carries. When a task is created the ghost records its
parent's agents plus guards(author); when an effect is proposed, its task's agents plus guards(proposer) and guards(executor).
A claim needs the guard of every agent recorded and of every agent in guards(proposer) and guards(executor) now (ClaimGuards); one of the
latter that the proposal never counted (neither recorded nor the proposer or executor) refuses the claim.

World: owner O, member P, operators OP and OP2 (human members), agents BOT (operator OP) and BOT2 (operator OP2), service X
(operator OP, the executor of some effects). At the start BOT is guarded, P has tasked BOT, BOT has delegated a task to BOT2, and
BOT has proposed pr0 (executor X), which OP approved, and which BOT2 approved as a rule approver while a rule was on; BOT2 has not
accepted t2 yet. Moves (round 32 adds t9, BOT's task to its own guard OP, and BOT cancelling its own t7): steers (an outsider's, a guard's, BOT's into its own and into BOT2's
session, BOT2's into BOT's), unguards by the guards and by others, BOT2 choosing guarding, P opening a guarded session (humans are
never guarded), closing, delegation chains (BOT2 delegating beneath BOT's task back to BOT; BOT delegating to X; BOT2 delegating to X
without a parent, t6; BOT's task t7 to BOT2 and P; BOT2's subtask t8 under P's unrelated task t5), every assignee accepting each open
task, BOT2 reporting progress on t2, every assignee closing each task it may (its own final update, on a closed task too), an owner
force-closing t2 and cancelling t7, proposals by
guarded agents, by their delegates (X under t6 among them), by an outsider naming BOT2 as executor, by BOT's own guard and by an owner
naming the task BOT delegated (it isn't an assignee, so only the named task carries the guarding), by BOT2 naming P's unrelated task t5
(pr10); approvals by OP, OP2, O, BOT and BOT2; claims, not-started and failed outcomes, cancels; BOT inviting a member, and BOT
leaving; owners switching `forceGuarded` and an
approval rule (owners, BOT and BOT2 approve: a guard's approval is never a rule approval here, and BOT's is refused while it is
guarded) on and off; OP removed and re-added, OP's key revoked; BOT's operator withdrawing and confirming again. An operator never
changes (§4.3), so a withdrawal and a new confirmation are the only profile moves.

Properties: GuardApproved (above); ClaimGuards (round 32, replacing DelegateChain: an allowed claim had an unused approval by the
guard of every agent in guards(proposer) and guards(executor), computed by the ghost at the claim, and none of them was outside what
the proposal counted; a claim refused as "never counted" had one outside); GuardedRestricted (a guarded author, guards not empty, is
never allowed `effect.approve` unless it is one of the effect's guards (Z3), a `member.set` other than leaving, a `policy.set`, a final
update of a task it isn't assigned, a cancel of another's task, or a steer into another's session, P5, V1, Z1); ExactAcceptance (each
task's `accepted` is the ghost's); ClosedTasksTakeFinalUpdates (Z2: a final update is never refused only for a closed task);
GuardedApprovalsUnused (a claim never consumes, as the rule's approval, one whose
author is guarded at the claim, R2); UnguardOnlyByTheGuard (an unguard is allowed exactly when its author is the guard, holding a role with
a valid key, of an agent that chose guarding, P6); NotStricter (Policy refuses no claim for a guard when every guard the ghost
needs has approved, and refuses no entry as a guarded author's that the ghost allows); ExactGuarding (Policy's set `guarded` and
the `guardedFor` of every task and effect equal the ghost's records); OneOpenApprovalPerAuthor; CapLiveness (round 29, T2: an
approval by an unguarded role holder that is none of the effect's guards is never refused for the cap while fewer than the cap of
*countable* approvals exist: open, an E3 candidate by the ghost's reckoning of the rule (round 30, V3), by another author that holds a
role and is not guarded, and none of the guards the ghost requires); DeadApprovalsDropped (T2, V5: no effect holds an unconsumed
approval whose author holds no role or is guarded, chosen or forced). So that the cap is reached with these few approvers, the model lowers MAX_OPEN_APPROVALS to 1 (`CAP`: with 2, the first approval made while BOT2 is forced drops its approval before a second could be refused); the real 16 and the
guards' exemption at full size are covered by vectors `guards-are-never-crowded-out`, `a-full-guard-chain-is-claimable` and
`cap-counts-only-countable-approvals`, and by the fuzzer's bounds. The MAX_GUARDED_FOR bound is never reached here (vector
`guarded-tasks-are-bounded`, `guarded-tasks-leave-room-to-work`); nor is a denied server (vector `denied-servers-approvals-are-dropped`). Not modelled, and covered by
vectors instead (ref/mutate_vectors.py catches each): expiry of approvals (`guarded-proposals-need-the-guard`, `approvals-stay-bounded`),
the attempt limit (`attempt-limit`; model_check has it as a property), cancel rights (`guards-are-checked`, `departed-guard`,
`guarded-approvals-grant-no-cancel`), key revocation of approvers other than OP (`revoked-keys-still-count-until-superseded`), the
`guard_status` result and the `state {agent}` view (`guard-status`, `guard-status-takes-the-agent-view`), pinning
(`relevant-actors-*`, `delegation-keeps-the-guard`), and the `session.open` shapes.
States are deduplicated on content: a 32-byte SHA-256 of (Policy state minus history, plus the ghost) is stored per state,
not a copy. A memory cap (default 6 GB of resident memory) ends the run cleanly: one line "NOT COMPLETED ..." and exit 3.

Usage: python ref/model_guard.py [--depth N] [--all-mutations] [--max-mem-gb G]"""
import argparse, hashlib, json, os, pickle, sys, types, zlib
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import policy as POL

SRC = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "policy.py"), encoding="utf-8").read()
DEPTH = 4                      # run_all runs depth 4, and the mutations at depth 4; depth 5 is opt-in (run_all --depth5, with --max-mem-gb)
MAX_MEM_GB = 6.0               # abort cleanly above this resident size (F7)
CAP = 1                        # the model's MAX_OPEN_APPROVALS (the real 16 can't be reached by its four approvers)
SP = "sg:a.example"
O, P, OP, OP2, X, Z = "@o:a.example", "@p:a.example", "@op:b.example", "@op2:b.example", "@x:c.example", "@z:a.example"
BOT, BOT2 = "@bot:b.example", "@bot2:b.example"
OPERATOR = {BOT: OP, BOT2: OP2, X: OP}          # the operator in each non-human's profile, fixed (§4.3)
T0 = "2026-10-10T12:00:00Z"
DG = "d" * 64

def H(label): return hashlib.sha256(label.encode()).hexdigest()
def short(a): return a[1:].split(":")[0]
def human(rev=False): return {"actorKind": "human", "operatorConfirmed": False, "keys": {"k1": {"publicKey": "pk", "status": "revoked" if rev else "valid"}}}
def nonhuman(kind, op, ok=True): return {"actorKind": kind, "operator": op, "operatorConfirmed": ok, "keys": {"k1": {"publicKey": "pk", "status": "valid"}}}

def policy(rule=False, force=False):
    return {"visibility": "members", "join": "invite", "membersMayInvite": True, "allowAgents": "any",
            "approvals": [{"action": "pay", "approvers": ["owner", BOT, BOT2]}] if rule else [], "separateApprover": False, "budget": None,
            "maxDelegationDepth": 4, "minDurability": "local", "launchWindow": 30, "extensionsMinRole": "member",
            "sessions": {"watch": "member", "read": "member", "steer": "member", "modes": ["auto", "guarded"], "forceGuarded": force}, "servers": {"deny": []}}

def E(label, kind, author, body):
    return {"id": H(label), "v": 1, "space": SP, "kind": kind, "author": author, "keyId": "k1", "body": body,
            "contentHash": "", "refs": [], "seen": 0, "nonce": label, "time": T0}

SESS = {"s1": (BOT, "guarded"), "s2": (BOT2, "auto"), "s4": (BOT2, "guarded"), "s8": (P, "guarded")}
SID = {H(k): k for k in SESS}
TASKS = {"t1": (P, [BOT], None), "t2": (BOT, [BOT2], None),          # made by init
         "t3": (BOT2, [BOT], "t2"), "t4": (BOT, [X], "t1"), "t5": (P, [BOT2], None),       # t3, t4: delegation chains; t5: an outsider's task
         "t6": (BOT2, [X], None),                                     # round 30: BOT2's parentless subtask (U1)
         "t7": (BOT, [BOT2, P], None),                                # round 31: two assignees; one closing ends nothing for the other (X1)
         "t8": (BOT2, [X], "t5"),                                     # round 31: a subtask under an unrelated parent (round 32: it records guards(BOT2))
         "t9": (BOT, [OP], None)}                                     # round 32: BOT's task to its own guard, which may still approve as the guard (Z3)
PROPS = {"pr0": (BOT, X, None),                                       # made by init, approved by OP
         "pr1": (BOT, BOT, "t1"), "pr2": (BOT2, BOT2, "t2"), "pr3": (BOT, BOT, "t3"), "pr4": (BOT2, X, None),
         "pr5": (P, BOT2, None), "pr6": (OP, BOT, None), "pr7": (X, X, "t4"),
         "pr8": (O, P, "t2"),                                         # an owner charges a task it isn't assigned (the named task's agents)
         "pr9": (X, X, "t6"),                                         # the work of BOT2's parentless subtask
         "pr10": (BOT2, X, "t5")}                                     # BOT2's work named under P's unrelated task (round 32: it records guards(BOT2))
LABEL = {H(k): k for k in list(TASKS) + list(PROPS)}

def fz(d): return tuple(sorted(d.items()))

class M:
    """A model state: Policy's state plus the ghost. The ghost: `gd` the agents that chose guarding; `rec` what was recorded on each task
    and effect when it was created (label → agents); `closed` the tasks closed since; `acc` the (task, actor) pairs of acceptances;
    `fresh` the (effect, approver) pairs with an approval no claim used up; `lastk` the guards whose approvals the latest claim of an
    effect used; `unconf`, `oprev`, `forced`, `rule` the profile and policy facts."""
    __slots__ = ("labels", "state", "gd", "rec", "closed", "acc", "fresh", "lastk", "unconf", "oprev", "forced", "rule")
    def __init__(self, **kw):
        for k in self.__slots__: setattr(self, k, kw[k])
    def key(self):
        st = {k: v for k, v in self.state.items() if k not in ("entries", "head", "roleHistory")}
        body = json.dumps([st, sorted(self.gd), self.rec, sorted(self.closed), sorted(self.acc), sorted(self.fresh), self.lastk, sorted(self.unconf), self.oprev, self.forced, self.rule], sort_keys=True)
        return hashlib.sha256(body.encode()).digest()
    def with_(self, **kw):
        d = {k: getattr(self, k) for k in self.__slots__}; d.update(kw); return M(**d)

def ctx(m):
    return {"time": T0, "profiles": {O: human(), P: human(), Z: human(), OP: human(m.oprev), OP2: human(), X: nonhuman("service", OP),
                                     BOT: nonhuman("agent", OP, BOT not in m.unconf), BOT2: nonhuman("agent", OP2)}}

def apply(pol, m, e, label):
    d = pol.decide(m.state, e, ctx(m))
    if not d["allow"]: return None, d
    s = pol.fold(m.state, {"entry": e, "content": None, "receipt": {"space": SP, "seq": (m.state["head"] + 1) if m.state else 1,
                                                                    "id": e["id"], "time": T0}}, ctx(m))
    return m.with_(labels=m.labels + (label,), state=s), d

# ── the ghost: P-15's definitions, read independently of Policy ──
def guarded(m, a):
    """Chose guarding, or forced: owners' flag is on and `a` is not human."""
    return a in m.gd or (m.forced and a in OPERATOR)

def guard_of(m, a):
    """The guard `a` has now: its operator, if confirmed; humans have none."""
    if a not in OPERATOR or a in m.unconf: return None
    return OPERATOR[a]

def can_sign(m, a): return m.state["roles"].get(a) in POL.READ_ROLES and not (a == OP and m.oprev)

def carried(m, x):
    """Round 30–32: the agents recorded on the tasks `x` accepted and has not closed for itself, nor an owner force-closed, nor left (X1, Z2)."""
    out = set()
    for t, agents in m.rec:
        if t in TASKS and (t, x) in m.acc: out |= set(agents)
    return out

def guards(m, x):
    """Z1: x itself if guarded, then what x carries."""
    return ({x} if guarded(m, x) else set()) | carried(m, x)

def claim_guards(m, pr):
    """Z1: guards(proposer) and guards(executor) now."""
    author, executor, _ = PROPS[pr]
    return guards(m, author) | guards(m, executor)

def uncounted(m, pr):
    """Z1: the agents of claim_guards the proposal never counted: neither recorded on it nor its proposer or executor."""
    author, executor, _ = PROPS[pr]
    return claim_guards(m, pr) - set(dict(m.rec)[pr]) - {author, executor}

def candidate(m, a):
    """V3: an approval by `a` is an E3 candidate now: the model's rule (owners, BOT and BOT2) is on and `a` matches it holding a role."""
    role = m.state["roles"].get(a)
    return m.rule and role in POL.READ_ROLES and (role == "owner" or a in (BOT, BOT2))

def required(m, pr):
    """The agents whose guard must approve a claim of `pr` now: those recorded when it (or a task it descends from) was created, and
    those of guards(proposer) and guards(executor) now (Z1; any the proposal never counted refuse the claim, `uncounted`)."""
    return set(dict(m.rec)[pr]) | claim_guards(m, pr)

def unmet(m, pr):
    """Why the ghost would refuse a claim of `pr` for its guards, or None."""
    if uncounted(m, pr): return f"{sorted(map(short, uncounted(m, pr)))} never counted by the proposal"
    for a in sorted(required(m, pr)):
        g = guard_of(m, a)
        if g is None: return f"{short(a)} has no confirmed operator"
        if not can_sign(m, g): return f"{short(a)}'s guard {short(g)} holds no role or has no valid key"
        if (pr, g) not in m.fresh: return f"{short(a)}'s guard {short(g)} has no unused approval"
    return None

def forbidden(m, author, kind, body):
    """P5, V1, Z1, Z3: a guarded author (guards not empty) may not approve unless it is one of the effect's guards, govern (leaving is
    not governing, R5; a final update of a task it isn't assigned is a force-close; a cancel of another's task) or steer others."""
    if not guards(m, author): return False
    if kind == "member.set": return body != {"actor": author, "role": "none"}
    if kind == "policy.set": return True
    if kind == "effect.approve": return author not in {guard_of(m, a) for a in required(m, LABEL[body["proposal"]]) - uncounted(m, LABEL[body["proposal"]])}
    if kind == "task.update": return body["state"] in POL.TASK_FINAL and author not in TASKS[LABEL[body["task"]]][1]
    if kind == "task.cancel": return TASKS[LABEL[body["task"]]][0] != author
    return kind == "session.input" and body["session"] in m.state["sessions"] and SESS[SID[body["session"]]][0] != author

def unguard_ok(m, author, a):
    return a in m.gd and guard_of(m, a) == author and can_sign(m, author)

def record(m, n, kind, author, body, label):
    """The ghost's own update after an allowed entry, judged on `m` (the state before it)."""
    if kind == "session.open" and body["steerMode"] == "guarded" and author in OPERATOR:   # T2: and its open approvals go
        return n.with_(gd=n.gd | {author}, fresh=frozenset(x for x in n.fresh if x[1] != author))
    if kind == "agent.unguard": return n.with_(gd=n.gd - {body["agent"]})
    if kind == "task":
        parent = TASKS[label][2]
        got = (set(dict(m.rec)[parent]) if parent else set()) | guards(m, author)       # Z1: the parent's agents, then guards(author)
        acc = n.acc | ({(label, author)} if author in TASKS[label][1] else set())           # V4: an author assigning itself has accepted
        return n.with_(rec=fz(dict(m.rec, **{label: tuple(sorted(got))})), acc=acc)
    if kind == "effect.propose":
        task = PROPS[label][2]
        got = (set(dict(m.rec)[task]) if task else set()) | guards(m, author) | guards(m, body["executor"])   # Z1
        return n.with_(rec=fz(dict(m.rec, **{label: tuple(sorted(got))})))
    if kind == "effect.approve":                                          # V3: the approval's fold drops the others that don't count
        pr = LABEL[body["proposal"]]
        gs = {guard_of(m, a) for a in required(m, pr) - uncounted(m, pr)}
        keep = frozenset(x for x in n.fresh if x[0] != pr or x[1] == author or x[1] in gs or candidate(m, x[1]))
        return n.with_(fresh=keep | {(pr, author)})
    if kind == "effect.claim":
        pr = LABEL[body["proposal"]]
        gs = tuple(sorted({guard_of(m, a) for a in required(m, pr)}))
        return n.with_(fresh=n.fresh - {(pr, g) for g in gs}, lastk=fz(dict(m.lastk, **{pr: gs})))
    if kind == "effect.outcome":
        pr = LABEL[body["proposal_"]]
        back = dict(m.lastk).get(pr, ()) if body["started"] == "no" else ()
        back = tuple(g for g in back if m.state["roles"].get(g) in POL.READ_ROLES)       # T2: a returned approval of a guard that left is dropped
        return n.with_(fresh=n.fresh | {(pr, g) for g in back}, lastk=fz(dict(m.lastk, **{pr: ()})))
    if kind == "policy.set":                                              # V5: forcing on drops the agents' open approvals
        forced = body["policy"]["sessions"]["forceGuarded"]
        fresh = frozenset(x for x in n.fresh if not (forced and x[1] in OPERATOR))
        return n.with_(forced=forced, rule=bool(body["policy"]["approvals"]), fresh=fresh)
    if kind == "task.update":
        t = LABEL[body["task"]]
        if body["state"] in POL.TASK_FINAL:                                # X1, Z2: an assignee's own final update ends its carry; an owner's
            acc = n.acc - {(t, author)} if author in TASKS[t][1] else frozenset(x for x in n.acc if x[0] != t)   # force-close ends all, closed or not
            return n.with_(closed=n.closed | {t}, acc=acc)
        if body["state"] == "accepted": return n.with_(acc=n.acc | {(t, author)})   # X4: only an explicit accept accepts
        return n
    if kind == "member.set" and body["role"] not in POL.READ_ROLES:    # T2: an author that loses its role loses its open approvals; Z2: and its carries
        return n.with_(fresh=frozenset(x for x in n.fresh if x[1] != body["actor"]), acc=frozenset(x for x in n.acc if x[1] != body["actor"]))
    return n

def init(pol):
    m = M(labels=(), state=None, gd=frozenset(), rec=(), closed=frozenset(), acc=frozenset(), fresh=frozenset(), lastk=(), unconf=frozenset(), oprev=False, forced=False, rule=False)
    def go(m, label, kind, author, body):
        n, d = apply(pol, m, E(label, kind, author, body), label)
        assert n, f"NotStricter: the setup step {label} ({kind} by {short(author)}) was refused: {d.get('reason')}"
        return record(m, n, kind, author, body, label)
    m = go(m, "C", "space.create", O, {"policy": policy()})
    for lab, a in (("m1", P), ("m2", X), ("m3", OP), ("m4", OP2), ("m5", BOT), ("m6", BOT2)):
        m = go(m, lab, "member.set", O, {"actor": a, "role": "member"})
    for sid in ("s1", "s2"):
        m = go(m, sid, "session.open", SESS[sid][0], {"steerMode": SESS[sid][1]})
    for t in ("t1", "t2"):
        author, to, _ = TASKS[t]
        m = go(m, t, "task", author, {"to": to})
    m = go(m, "pr0", "effect.propose", BOT, {"executor": X, "action": "pay", "planDigest": DG, "repeatable": True})
    m = go(m, "ap0", "effect.approve", OP, {"proposal": H("pr0"), "planDigest": DG})
    m = go(m, "r1", "policy.set", O, {"policy": policy(rule=True)})
    m = go(m, "ap1", "effect.approve", BOT2, {"proposal": H("pr0"), "planDigest": DG})      # R2: BOT2 approves while it isn't guarded
    return go(m, "r0", "policy.set", O, {"policy": policy()})

def successors(pol, m):
    out = []
    def go(name, kind, author, body, label=None):
        lab = label or (name + "#" + str(sum(1 for x in m.labels if x.startswith(name + "#"))))
        e = E(lab, kind, author, body)
        n, d = apply(pol, m, e, lab)
        if n: n = record(m, n, kind, author, body, label)
        out.append((name, kind, author, body, n, d))
    for author, sid in ((P, "s1"), (OP, "s1"), (BOT, "s1"), (BOT, "s2"), (BOT2, "s1")):     # outsider, guard, own, into another's (both ways)
        go(f"{short(author)} steers {sid}", "session.input", author, {"session": H(sid)})
    for author, a in ((OP, BOT), (OP2, BOT), (OP2, BOT2), (O, BOT), (BOT, BOT)):
        go(f"{short(author)} unguards {short(a)}", "agent.unguard", author, {"agent": a})
    for sid in ("s4", "s8"):
        if H(sid) not in m.state["sessions"] and not any(x == sid for x in m.labels):
            go(f"{short(SESS[sid][0])} opens {sid} {SESS[sid][1]}", "session.open", SESS[sid][0], {"steerMode": SESS[sid][1]}, label=sid)
    if H("s1") in m.state["sessions"]: go("BOT closes s1", "session.close", BOT, {"session": H("s1")})
    for t, (author, to, parent) in TASKS.items():
        if H(t) not in m.state["tasks"]:
            go(f"{short(author)} tasks {'+'.join(map(short, to))} ({t})", "task", author, dict({"to": to}, **({"parent": H(parent)} if parent else {})), label=t)
    for pr, (author, executor, task) in PROPS.items():
        if H(pr) not in m.state["effects"]:
            go(f"{short(author)} proposes {pr}", "effect.propose", author,
               dict({"executor": executor, "action": "pay", "planDigest": DG, "repeatable": pr == "pr0"}, **({"task": H(task)} if task else {})), label=pr)
    for pr in PROPS:
        pid = H(pr); eff = m.state["effects"].get(pid)
        if not eff or eff["phase"] in POL.FINAL: continue
        go(f"{short(eff['proposer'])} cancels {pr}", "effect.cancel", eff["proposer"], {"proposal": pid})
        if len(eff["claims"]) >= 2: continue
        for who in (OP, OP2, O, BOT, BOT2):
            go(f"{short(who)} approves {pr}", "effect.approve", who, {"proposal": pid, "planDigest": DG})
        a = len(eff["claims"]) + 1
        go(f"{short(eff['executor'])} claims {pr}", "effect.claim", eff["executor"], {"proposal": pid, "attempt": a, "instance": "k1", "window": 30})
        if eff["phase"] == "claimed":
            c = eff["claims"][-1]
            for result, started in (("failed", "no"), ("failed", "yes")):
                body = {"claim": c["id"], "result": result, "started": started}
                e = E(f"out-{pr}-{started}-{a}", "effect.outcome", eff["executor"], body)
                n, d = apply(pol, m, e, f"{pr} {started}")
                if n: n = record(m, n, "effect.outcome", eff["executor"], dict(body, proposal_=pid), None)
                out.append((f"{short(eff['executor'])} outcome {pr} started:{started}", "effect.outcome", eff["executor"], body, n, d))
    for t, (_, to, _) in TASKS.items():                                   # V4: each assignee may accept each open task once
        tk = m.state["tasks"].get(H(t))
        for a in to:
            if tk and tk["open"] and (t, a) not in m.acc:
                go(f"{short(a)} accepts {t}", "task.update", a, {"task": H(t), "state": "accepted"})
            if tk and (tk["open"] or (t, a) in m.acc):                        # X1: its own final update, on a closed task too
                go(f"{short(a)} closes {t}", "task.update", a, {"task": H(t), "state": "done"})
    tk = m.state["tasks"].get(H("t2"))
    if tk and tk["open"]: go("BOT2 reports progress on t2", "task.update", BOT2, {"task": H("t2"), "state": "progress"})       # X4: no acceptance
    if tk and (tk["open"] or tk["accepted"]): go("O force-closes t2", "task.update", O, {"task": H("t2"), "state": "cancelled"})   # X1, Z2: a closed one too
    tk = m.state["tasks"].get(H("t7"))
    if tk and not tk["open"] and tk["accepted"]: go("O clears t7", "task.update", O, {"task": H("t7"), "state": "cancelled"})   # Z2: an owner's final update on the closed task
    if tk and tk["open"]:
        go("O cancels t7", "task.cancel", O, {"task": H("t7")})                              # Z2: it ends no carry
        go("BOT cancels t7", "task.cancel", BOT, {"task": H("t7")})                          # a guarded author's own task
    if m.state["roles"].get(Z) is None: go("BOT invites Z", "member.set", BOT, {"actor": Z, "role": "guest"})
    if m.state["roles"].get(BOT): go("BOT leaves", "member.set", BOT, {"actor": BOT, "role": "none"})
    go("O removes OP" if m.state["roles"].get(OP) else "O re-adds OP", "member.set", O, {"actor": OP, "role": "none" if m.state["roles"].get(OP) else "member"})
    rule, force = bool(m.state["policy"]["approvals"]), m.state["policy"]["sessions"]["forceGuarded"]
    go("O toggles the approval rule", "policy.set", O, {"policy": policy(not rule, force)})
    go("O toggles forceGuarded", "policy.set", O, {"policy": policy(rule, not force)})
    def fact(name, **kw): out.append((name, "profile", None, {}, m.with_(labels=m.labels + (name,), **kw), {"allow": True}))
    if not m.oprev: fact("OP's key revoked", oprev=True)
    fact(f"BOT's operator {'confirms again' if BOT in m.unconf else 'withdraws'}", unconf=m.unconf ^ {BOT})
    return out

GUARD_REASONS = ("E3 a guard hasn't approved", "E3 a guarded agent has no confirmed operator", "guard profile not pinned")
UNCOUNTED = "E3 the proposer or executor took on guarding the proposal never counted: propose again"

def check_state(m):
    if set(m.state["guarded"]) != m.gd:
        return f"ExactGuarding: Policy's guarded is {sorted(map(short, m.state['guarded']))}, the ghost's {sorted(map(short, m.gd))}"
    for label, want in m.rec:
        item = (m.state["tasks"] if label in TASKS else m.state["effects"])[H(label)]
        if sorted(item["guardedFor"]) != list(want) or len(item["guardedFor"]) != len(set(item["guardedFor"])):
            return f"ExactGuarding: {label} records {sorted(map(short, item['guardedFor']))}, the ghost {sorted(map(short, want))}"
    for t in TASKS:
        tk = m.state["tasks"].get(H(t))
        if tk and set(tk["accepted"]) != {a for (x, a) in m.acc if x == t}:
            return f"ExactAcceptance: {t} accepted {sorted(map(short, tk['accepted']))}, the ghost {sorted(short(a) for (x, a) in m.acc if x == t)}"
    for pid, eff in m.state["effects"].items():
        authors = [a["author"] for a in eff["approvals"] if not a.get("consumedBy")]
        if len(authors) != len(set(authors)): return "OneOpenApprovalPerAuthor: an author has two open approvals on one proposal"
        dead = [a for a in authors if m.state["roles"].get(a) not in POL.READ_ROLES or guarded(m, a)]
        if dead: return f"DeadApprovalsDropped: {LABEL[pid]} holds an open approval by {short(dead[0])}, which holds no role or is guarded"
    return None

def countable(m, pr, who):
    """T2, V3, the ghost's count for the cap: open approvals on `pr` by authors other than `who` that are E3 candidates now, aren't
    guarded and are none of the guards a claim of `pr` would need now."""
    gs = {guard_of(m, a) for a in required(m, pr) - uncounted(m, pr)}
    eff = m.state["effects"][H(pr)]
    return sum(1 for a in eff["approvals"] if not a.get("consumedBy") and a["author"] != who and a["author"] not in gs
               and candidate(m, a["author"]) and not guarded(m, a["author"]))

def rule_approver_guarded(m, m2, body):
    """R2: the author of the approval an allowed claim consumed as the rule's, if the ghost holds it guarded at the claim."""
    eff = m2.state["effects"][body["proposal"]]
    c = eff["claims"][-1]
    a = next((x["author"] for x in eff["approvals"] if x["id"] == c.get("approval")), None)
    return a if a and guarded(m, a) else None

def check(m, name, kind, author, body, d, m2=None):
    if kind == "profile": return None
    allowed, reason = d["allow"], d.get("reason", "")
    if allowed and forbidden(m, author, kind, body): return f"GuardedRestricted: guarded {short(author)} was allowed {kind}"
    if kind == "task.update" and body["state"] in POL.TASK_FINAL and not allowed and reason == "task closed":
        return f"ClosedTasksTakeFinalUpdates: {short(author)}'s final update refused only because the task is closed"
    if not allowed and "author may not approve" in reason and not forbidden(m, author, kind, body):
        return f"NotStricter: {kind} by {short(author)} refused as a guarded author's, though the ghost allows it"
    if kind == "agent.unguard":
        ok = unguard_ok(m, author, body["agent"])
        if allowed and not ok: return f"UnguardOnlyByTheGuard: {short(author)} unguarded {short(body['agent'])}"
        if ok and not allowed: return f"UnguardOnlyByTheGuard: the guard {short(author)} could not unguard {short(body['agent'])}: {reason}"
    if kind == "effect.claim":
        pr = LABEL[body["proposal"]]
        why = unmet(m, pr)
        if allowed and why: return f"{'ClaimGuards' if uncounted(m, pr) or claim_guards(m, pr) - set(dict(m.rec)[pr]) else 'GuardApproved'}: claim of {pr} allowed though {why} (needed: {sorted(map(short, required(m, pr)))})"
        if not allowed and reason in GUARD_REASONS and not why: return f"NotStricter: claim of {pr} refused ({reason}) though every guard the ghost needs approved"
        if not allowed and reason == UNCOUNTED and not uncounted(m, pr): return f"NotStricter: claim of {pr} refused as never counted, though the ghost counts every guard"
        g = rule_approver_guarded(m, m2, body) if allowed and m2 is not None else None
        if g: return f"GuardedApprovalsUnused: claim of {pr} consumed the approval of {short(g)}, guarded at the claim"
    if kind == "effect.approve" and not allowed and reason == "too many open approvals":
        pr = LABEL[body["proposal"]]
        n = countable(m, pr, author)
        if n < CAP and m.state["roles"].get(author) in POL.READ_ROLES and not guards(m, author):
            return f"CapLiveness: {short(author)}'s approval of {pr} refused for the cap with only {n} countable approvals"
    return None

class OutOfMemory(Exception): pass

def pack(m): return zlib.compress(pickle.dumps(m, 4), 1)
def unpack(b): return pickle.loads(zlib.decompress(b))

def rss_bytes():
    """Resident size of this process (psutil if present; else Windows' working set; else resource)."""
    try:
        import psutil
        return psutil.Process().memory_info().rss
    except ImportError:
        pass
    if os.name == "nt":
        import ctypes
        from ctypes import wintypes
        class PMC(ctypes.Structure):
            _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD), ("PeakWorkingSetSize", ctypes.c_size_t),
                        ("WorkingSetSize", ctypes.c_size_t), ("QuotaPeakPagedPoolUsage", ctypes.c_size_t), ("QuotaPagedPoolUsage", ctypes.c_size_t),
                        ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t), ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
                        ("PagefileUsage", ctypes.c_size_t), ("PeakPagefileUsage", ctypes.c_size_t)]
        pmc = PMC(); pmc.cb = ctypes.sizeof(pmc)
        ctypes.windll.psapi.GetProcessMemoryInfo(ctypes.windll.kernel32.GetCurrentProcess(), ctypes.byref(pmc), pmc.cb)
        return pmc.WorkingSetSize
    import resource
    r = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    return r if sys.platform == "darwin" else r * 1024

def explore(pol, depth, cap_bytes=None, progress=False):
    """Breadth-first to `depth`. Returns (transitions, violation, path, seen-flags): the flags show that the run reached a claim that needed a
    guard, one that needed the guard of a delegating agent (neither its proposer nor its executor), a claim refused for a guard, and a
    guarded author refused a kind."""
    pol.MAX_OPEN_APPROVALS = CAP                             # the model's cap (see the docstring); `pol` is this run's own module
    try:
        start = init(pol)
    except AssertionError as ex:                             # Policy refuses a setup step the ghost allows
        return 0, str(ex), ("init",), {}
    v = check_state(start)
    if v: return 0, v, ("init",), {}
    seen = {start.key()}; frontier = [(pack(start), ())]                # the frontier is kept compressed; `seen` holds 32-byte hashes
    n, tick = 0, 0
    flags = {"guarded claim": False, "delegated claim": False, "guard denial": False, "restricted author": False, "guarded approval dropped": False,
             "cap refusal": False, "uncounted approval": False, "task-less delegated work": False, "limited delegate": False, "parentless delegated task": False,
             "carry outlives the close": False, "carrying executor": False, "claim refused as never counted": False, "carrying guard approved": False,
             "closed task cleared by an owner": False, "leaving ended a carry": False}
    for level in range(depth):
        nxt = []
        for blob, path in frontier:
            m = unpack(blob)
            tick += 1
            if cap_bytes and tick % 128 == 0 and rss_bytes() > cap_bytes:
                raise OutOfMemory(f"memory cap of {cap_bytes / 2**30:.1f} GB reached at depth {level + 1} of {depth} after {n} transitions and {len(seen)} states "
                                  f"(frontier {len(frontier)}, next {len(nxt)})")
            for name, kind, author, body, m2, d in successors(pol, m):
                n += 1
                v = check(m, name, kind, author, body, d, m2)
                if v: return n, v, path + (name,), flags
                if kind == "effect.claim":
                    pr = LABEL[body["proposal"]]; need = required(m, pr)
                    if d["allow"] and need: flags["guarded claim"] = True
                    if d["allow"] and need - {PROPS[pr][0], PROPS[pr][1]}: flags["delegated claim"] = True
                    if not d["allow"] and d.get("reason") in GUARD_REASONS: flags["guard denial"] = True
                    if not d["allow"] and d.get("reason") == UNCOUNTED: flags["claim refused as never counted"] = True
                if kind in ("policy.set", "session.open") and d["allow"] and m2 is not None:
                    before = sum(1 for e in m.state["effects"].values() for x in e["approvals"] if not x.get("consumedBy") and x["author"] in (BOT, BOT2))
                    after = sum(1 for e in m2.state["effects"].values() for x in e["approvals"] if not x.get("consumedBy") and x["author"] in (BOT, BOT2))
                    if after < before: flags["guarded approval dropped"] = True
                if not d["allow"] and "author may not approve" in d.get("reason", ""):
                    flags["restricted author"] = True
                    if not guarded(m, author): flags["limited delegate"] = True
                if kind == "task" and d["allow"] and m2 is not None and not guarded(m, author) and carried(m, author):
                    flags["parentless delegated task"] = True
                if kind == "effect.approve" and d["allow"] and carried(m, author): flags["carrying guard approved"] = True
                if kind == "task.update" and d["allow"] and author == O and not m.state["tasks"][body["task"]]["open"] and m.state["tasks"][body["task"]]["accepted"]:
                    flags["closed task cleared by an owner"] = True
                if kind == "member.set" and d["allow"] and body["role"] not in POL.READ_ROLES and any((x, body["actor"]) in m.acc for x in TASKS):
                    flags["leaving ended a carry"] = True
                if kind == "effect.approve" and not d["allow"] and d.get("reason") == "too many open approvals": flags["cap refusal"] = True
                if kind == "task.update" and d["allow"] and not m.state["tasks"][body["task"]]["open"]: flags["carry outlives the close"] = True
                if kind == "effect.propose" and d["allow"] and carried(m, body["executor"]) - carried(m, author): flags["carrying executor"] = True
                if kind == "effect.approve" and d["allow"] and author not in {guard_of(m, a) for a in required(m, LABEL[body["proposal"]])}:
                    eff = m.state["effects"][body["proposal"]]
                    if sum(1 for x in eff["approvals"] if not x.get("consumedBy") and x["author"] != author) >= CAP: flags["uncounted approval"] = True
                if kind == "effect.propose" and d["allow"] and PROPS[name.split()[-1]][2] is None and m2 is not None:
                    if set(m2.state["effects"][H(name.split()[-1])]["guardedFor"]) - {author, body["executor"]}: flags["task-less delegated work"] = True
                if m2 is not None:
                    k = m2.key()
                    if k in seen: continue
                    v = check_state(m2)
                    if v: return n, v, path + (name,), flags
                    seen.add(k)
                    if level + 1 < depth: nxt.append((pack(m2), path + (name,)))      # the last level is only checked, never expanded
        frontier = nxt
        if progress: print(f"  depth {level + 1}: {n} transitions, {len(seen)} states, frontier {len(frontier)}, rss {rss_bytes() / 2**20:.0f} MB", flush=True)
    return n, None, None, flags

def mutant(*edits):
    """Apply one or more (old, new) source edits to policy.py; each old text must occur exactly once."""
    if edits and isinstance(edits[0], str): edits = (edits,)
    src = SRC
    for old, new in edits:
        assert src.count(old) == 1, old
        src = src.replace(old, new)
    mod = types.ModuleType("policy_mut"); mod.__file__ = POL.__file__
    exec(compile(src, "policy_mut", "exec"), mod.__dict__)
    return mod

TASK_REC = 't["guardedFor"] = task_guarded_for(s, ctx, author, b.get("parent"))'
TASK_RET = '    return out + [a for a in guards(state, ctx, author) if a not in out]'
EFF_REC = 'out = list(state["tasks"][task_id]["guardedFor"]) if task_id else []'
EFF_FOR = '    for x in (proposer, executor):'
NEED = '    need = list(eff["guardedFor"]) + [a'
CLAIM_G = 'guards(state, ctx, eff["proposer"]) + guards(state, ctx, eff["executor"])'
FORCED = '    forced = agent and bool(state["policy"]["sessions"].get("forceGuarded"))'
OWN = 'own = [x] if forced or x in state["guarded"] else []'
GATE = '    if guard_status(state, ctx, author)["guarded"] and governs_others(state, ctx, entry):'
GATE_IF = 'if guard_status(state, ctx, author)["guarded"] and governs_others'
APPROVE = '        return not eff or author not in guards_of(state, ctx, eff)'
MEMBER = '    if k == "member.set": return b != {"actor": author, "role": "none"}'
KINDS = '    if k in ("policy.set", "space.move"): return True'
STEER = '    if k == "session.input": return b["session"] in state["sessions"] and state["sessions"][b["session"]]["actor"] != author'
CANCEL = '    if k == "task.cancel": return bool(t) and t["author"] != author'
UNGUARD = '    if x not in state["guarded"] or guard_status(state, ctx, x).get("guard") != entry["author"]:'
CLOSED = 'if not t["open"] and not final: return deny("task closed")'
MUTATIONS = {
    # what is recorded on the work (P2, Z1)
    "tasks-record-no-guarding": (TASK_REC, 't["guardedFor"] = []'),
    "tasks-drop-the-parents-agents": ('out = list(state["tasks"][parent_id]["guardedFor"]) if parent_id else []', 'out = []'),
    "tasks-omit-the-authors-guards": (TASK_RET, '    return out'),
    "effects-drop-the-named-tasks-agents": (EFF_REC, 'out = []'),
    "effects-omit-the-proposers-guards": (EFF_FOR, '    for x in (executor,):'),
    "effects-omit-the-executors-guards": (EFF_FOR, '    for x in (proposer,):'),
    "guards-omit-what-is-carried": ('    gs = own + [a for a in carried(state, x) if a not in own]', '    gs = own'),
    # what a claim needs (P4, Z1)
    "claims-ignore-recorded-guarding": (NEED, '    need = [] + [a'),
    "claims-ignore-the-proposers-guards-now": (CLAIM_G, 'guards(state, ctx, eff["executor"])'),
    "claims-ignore-the-executors-guards-now": (CLAIM_G, 'guards(state, ctx, eff["proposer"])'),
    "claims-accept-guarding-never-counted": ('    if unbounded_guards(state, ctx, eff): return deny(', '    if False: return deny('),
    "guards-not-required": ('        if res == "none": return deny("E3 a guard hasn\'t approved")', '        pass'),
    "unguardable-agents-claimable": ('        if g is None: return deny("E3 a guarded agent has no confirmed operator")', '        if g is None: continue'),
    "agents-without-an-operator-dropped": ('    return list(dict.fromkeys(guard_status(state, ctx, a).get("guard") for a in need))', '    return list(dict.fromkeys(g for g in (guard_status(state, ctx, a).get("guard") for a in need) if g))'),
    # "guard-approval-needs-no-role" (round 27) is retired in round 29: T2 drops a guard's open approvals when it loses its role, so E3's role check can no longer fire (an equivalent mutant)
    "guard-key-ignored": ('        if st == "valid": return "ok", a\n    return "none", None\n\ndef live', '        return "ok", a\n    return "none", None\n\ndef live'),
    "guard-approval-reused": ('        for a in chosen: a["consumedBy"] = eid', '        pass'),
    "owners-stand-in": [('    if not (author in guards and role in READ_ROLES):', '    if not ((author in guards or role == "owner") and role in READ_ROLES):'),
                        ('        if a["author"] != g or a.get("consumedBy")', '        if (a["author"] != g and role_of(state, a["author"]) != "owner") or a.get("consumedBy")')],
    "returned-approvals-unsuperseded": ('                        gone.append(a)                         # superseded while it was consumed', '                        pass'),
    # who is guarded, and who is the guard (P1)
    "forcing-ignored": (FORCED, '    forced = False'),
    "forcing-without-the-flag": (FORCED, '    forced = agent'),
    "services-exempt-from-forcing": ('    agent = bool(p) and p["actorKind"] != "human"', '    agent = bool(p) and p["actorKind"] == "agent"'),
    "humans-forced": ('    agent = bool(p) and p["actorKind"] != "human"', '    agent = bool(p)'),
    "unconfirmed-operators-guard": ('if agent and p.get("operatorConfirmed") and p.get("operator"):', 'if agent and p.get("operator"):'),
    "the-choice-does-not-guard": (OWN, 'own = [x] if forced else []'),
    "forcing-does-not-guard": (OWN, 'own = [x] if x in state["guarded"] else []'),
    "humans-may-choose-guarding": ('        if b["steerMode"] == "guarded" and not is_human(ctx, author):\n            s["guarded"]', '        if b["steerMode"] == "guarded":\n            s["guarded"]'),
    "guarded-not-sticky": ('            s["guarded"][author] = True                                          # sticky: only its guard unguards it', '            pass'),
    "agent-closing-unguards-itself": ('        del s["sessions"][b["session"]]', '        if author == s["sessions"][b["session"]]["actor"]: s["guarded"].pop(author, None)\n        del s["sessions"][b["session"]]'),
    # unguard (P6)
    "unguard-does-nothing": ('        s["guarded"].pop(b["agent"], None)', '        pass'),
    "anyone-unguards": (UNGUARD, '    if x not in state["guarded"]:'),
    "unguarding-an-agent-that-chose-nothing": (UNGUARD, '    if guard_status(state, ctx, x).get("guard") != entry["author"]:'),
    "unguard-needs-no-role": ('    return ALLOW if role in READ_ROLES else deny("guard holds no role")', '    return ALLOW'),
    # what a guarded author may not do (P5, Z1, Z3)
    "guarded-authors-unrestricted": (GATE, '    if False:'),
    "only-a-chosen-guarding-restricts": (GATE_IF, 'if author in state["guarded"] and governs_others'),
    "delegates-carry-no-limits": (GATE_IF, 'if (author in state["guarded"] or guard_status(state, ctx, author)["forced"]) and governs_others'),
    "guarded-agents-may-approve": (APPROVE, '        return False'),
    "a-carrying-guard-may-not-approve": (APPROVE, '        return True'),
    "guarded-agents-may-govern": (MEMBER, '    if k == "member.set": return False'),
    "guarded-agents-may-steer-others": (STEER, '    if k == "session.input": return False'),
    "guarded-agents-may-not-delegate": (KINDS, '    if k in ("policy.set", "space.move", "task"): return True'),
    "guarded-agents-may-not-propose": (KINDS, '    if k in ("policy.set", "space.move", "effect.propose"): return True'),
    "guarded-agents-may-not-steer-themselves": (STEER, '    if k == "session.input": return True'),
    "guarded-authors-may-not-cancel-their-own-tasks": (CANCEL, '    if k == "task.cancel": return bool(t)'),
    # round 28: a guarded agent may leave (R5). "guarded-approvers-count" and "only-a-chosen-guarding-stops-an-approver" (R2) are retired
    # in round 30: the fold drops a guarded author's open approvals, so E3 has nothing to skip (equivalent mutants)
    "guarded-agents-may-not-leave": (MEMBER, '    if k == "member.set": return True'),
    # round 29: the cap counts only what can still count (T2)
    "the-cap-counts-every-open-approval": ('        if sum(1 for a in approval_candidates(state, eff, ctx["time"])\n               if a["author"] != author and a["author"] not in guards)',
                                           '        if sum(1 for a in open_approvals(eff, ctx["time"])\n               if a["author"] != author)'),
    "removed-authors-keep-open-approvals": ('            drop_open_approvals(s, tgt)                                          # P-08', '            pass                                          # P-08'),
    "guarded-authors-keep-open-approvals": ('            drop_open_approvals(s, author)', '            pass'),
    "returned-approvals-of-departed-authors-stay": ('                    elif role_of(s, a["author"]) not in READ_ROLES or approval_dead(s, a):', '                    elif False:'),
    # round 30: only accepted tasks carry (V4); forcing drops (V5)
    "unaccepted-tasks-carry": ('if x in t["accepted"]), key=', 'if x in t["to"]), key='),
    "acceptance-ignored": ('            t["accepted"].append(author)', '            pass'),
    "forcing-on-keeps-the-agents-approvals": ('            eff["approvals"] = [a for a in eff["approvals"] if a.get("consumedBy") or not approval_dead(s, a)]', '            pass'),
    "recorded-approvals-ignore-forcing": ('    return (a["author"] in s["guarded"] or (bool(s["policy"]["sessions"].get("forceGuarded")) and a["actorKind"] != "human")', '    return (a["author"] in s["guarded"]'),
    # round 31 and 32: per-assignee closure (X1, Z2), carrying executors (X3), explicit acceptance (X4)
    "closing-ends-every-assignees-carry": ('if x in t["accepted"]), key=', 'if t["open"] and x in t["accepted"]), key='),
    "own-final-update-keeps-the-carry": ('[a for a in t["accepted"] if a != author] if author in t["to"] else []', 't["accepted"] if author in t["to"] else []'),
    "owner-force-close-ends-nothing": ('[a for a in t["accepted"] if a != author] if author in t["to"] else []', '[a for a in t["accepted"] if a != author] if author in t["to"] else t["accepted"]'),
    "owner-cancel-ends-the-carry": ('        s["tasks"][b["task"]]["cancelRequested"] = True', '        s["tasks"][b["task"]]["cancelRequested"] = True\n        if s["roles"].get(author) == "owner": s["tasks"][b["task"]]["accepted"] = []'),
    "leaving-keeps-the-carry": ('                if tgt in t["accepted"]: t["accepted"]', '                if False: t["accepted"]'),
    "a-closed-task-takes-no-final-update": (CLOSED, 'if not t["open"]: return deny("task closed")'),
    "a-closed-task-takes-only-an-accepted-assignees-update": (CLOSED, 'if not t["open"] and not (final and entry["author"] in t["accepted"]): return deny("task closed")'),
    "work-under-a-task-ignores-the-proposers-guards": (EFF_FOR, '    for x in ((executor,) if task_id else (proposer, executor)):'),
    "subtasks-ignore-what-the-author-carries": (TASK_RET, '    return out + [a for a in guards(state, ctx, author) if a not in out and (not parent_id or a == author)]'),
    "effects-ignore-what-the-executor-carries": ('        out += [a for a in guards(state, ctx, x) if a not in out]', '        out += [a for a in guards(state, ctx, x) if a not in out and (x == proposer or a == x)]'),
    "a-progress-update-accepts": ('if b["state"] == "accepted" and author not in t["accepted"]:', 'if b["state"] not in TASK_FINAL and author not in t["accepted"]:'),
    # "an approval keeps what does not count" (V3) is not reachable here: with the model's rule and roles an approval stops being a
    # candidate only when the rule is off, where no non-guard can approve; vector `cap-counts-only-countable-approvals` catches it
    # "carrying owners may force-close, cancel others' tasks or move the space" (Z1) is not reachable here: the model's one owner never
    # carries; vector `carrying-is-guarding` and mutate_vectors catch them
}

def main():
    try: sys.stdout.reconfigure(encoding="utf-8")       # the counterexample paths print arrows, also into a redirected log
    except Exception: pass
    ap = argparse.ArgumentParser()
    ap.add_argument("--depth", type=int, default=DEPTH)
    ap.add_argument("--all-mutations", action="store_true")
    ap.add_argument("--max-mem-gb", type=float, default=MAX_MEM_GB, help="abort with NOT COMPLETED (exit 3) above this resident size")
    a = ap.parse_args()
    cap = int(a.max_mem_gb * 2**30)
    if a.all_mutations:
        missed = 0
        for name, edits in MUTATIONS.items():
            try:
                n, v, path, _ = explore(mutant(*(edits if isinstance(edits, list) else [edits])), a.depth, cap)
            except OutOfMemory as ex:
                print(f"NOT COMPLETED: {name}: {ex}"); sys.exit(3)
            if v: print(f"caught  {name:42} after {n} transitions: {v}  [{' → '.join(path)}]", flush=True)
            else: print(f"MISSED  {name:42} no violation in {n} transitions", flush=True); missed += 1
        print(f"{len(MUTATIONS) - missed}/{len(MUTATIONS)} guard mutations caught by a property")
        sys.exit(1 if missed else 0)
    try:
        n, v, path, flags = explore(POL, a.depth, cap, progress=True)
    except OutOfMemory as ex:
        print(f"NOT COMPLETED: {ex}")
        sys.exit(3)
    if v: print(f"VIOLATION after {n} transitions: {v}\n  path: {' → '.join(path)}"); sys.exit(1)
    if not all(flags.values()): print(f"VACUOUS: {flags}"); sys.exit(1)
    print(f"[guard] {n} transitions to depth {a.depth}; GuardApproved, ClaimGuards, GuardedRestricted, GuardedApprovalsUnused, UnguardOnlyByTheGuard, NotStricter, "
          f"ExactGuarding, ExactAcceptance, ClosedTasksTakeFinalUpdates, OneOpenApprovalPerAuthor, CapLiveness and DeadApprovalsDropped HOLD (non-vacuous: {', '.join(flags)})")

if __name__ == "__main__":
    main()

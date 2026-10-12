"""Bounded exhaustive check of P-15 guarded steering against the real policy.py (round 27: guarding is recorded on the work; round 28:
a guarded author's approval never counts, a guarded agent may leave, and an operator never changes; round 29: an assignee carries its
open tasks' guarding into all of its proposals, and the approval cap counts only approvals that can still count).

The property (DECISIONS-r27 P9), checked on every claim Policy allows:

    GuardApproved: no effect is claimed unless, for every agent that was guarded when the effect or any task it descends from
    was created, and for every agent guarded at the claim's seq, that agent's guard approved it.

"Approved it" is an approval by that guard which no earlier claim used up (a not-started outcome gives it back), while the
guard holds a role and its key is valid; an agent with no confirmed operator has no guard, so nothing that needs one may be
claimed. The ghost records guarding at creation itself, from the definitions of P-15 and never from Policy's state: an agent is
guarded when it chose to be (it opened a `guarded` session; only its guard ends that, with `agent.unguard`) or when owners force
it (`forceGuarded` on and it is not human, a service included); its guard is the confirmed operator in its current profile. When a
task is created the ghost records its parent's agents plus the author if guarded; when an effect is proposed, its task's agents, the
agents of every task the ghost holds open that lists the proposer as an assignee (round 29, T1), plus the proposer and the executor if
guarded. A claim needs the guards of those agents and of the proposer and executor if guarded now.

World: owner O, member P, operators OP and OP2 (human members), agents BOT (operator OP) and BOT2 (operator OP2), service X
(operator OP, the executor of some effects). At the start BOT is guarded, P has tasked BOT, BOT has delegated a task to BOT2, and
BOT has proposed pr0 (executor X), which OP approved, and which BOT2 approved as a rule approver while a rule was on. Moves: steers (an outsider's, a guard's, BOT's into its own and into BOT2's
session, BOT2's into BOT's), unguards by the guards and by others, BOT2 choosing guarding, P opening a guarded session (humans are
never guarded), closing, delegation chains (BOT2 delegating beneath BOT's task back to BOT; BOT delegating to X), proposals by
guarded agents, by their delegates, by an outsider naming BOT2 as executor, by BOT's own guard and by an owner naming the task
BOT delegated (it isn't an assignee, so only the named task carries the guarding); approvals by OP, OP2, O and BOT;
claims, not-started and failed outcomes, cancels; BOT2 closing the task BOT delegated to it (t2); BOT inviting a member, and BOT
leaving; owners switching `forceGuarded` and an
approval rule (owners, BOT and BOT2 approve: a guard's approval is never a rule approval here, and BOT's is refused while it is
guarded) on and off; OP removed and re-added, OP's key revoked; BOT's operator withdrawing and confirming again. An operator never
changes (§4.3), so a withdrawal and a new confirmation are the only profile moves.

Properties: GuardApproved (above); GuardedRestricted (a guarded author is never allowed `effect.approve`, a `member.set` other than
leaving, or a steer into another's session, P5); GuardedApprovalsUnused (a claim never consumes, as the rule's approval, one whose
author is guarded at the claim, R2); UnguardOnlyByTheGuard (an unguard is allowed exactly when its author is the guard, holding a role with
a valid key, of an agent that chose guarding, P6); NotStricter (Policy refuses no claim for a guard when every guard the ghost
needs has approved, and refuses no entry as a guarded author's that the ghost allows); ExactGuarding (Policy's set `guarded` and
the `guardedFor` of every task and effect equal the ghost's records); OneOpenApprovalPerAuthor; CapLiveness (round 29, T2: an
approval by an unguarded role holder that is none of the effect's guards is never refused for the cap while fewer than the cap of
*countable* approvals exist: open, by another author that holds a role, is not guarded by the ghost's reckoning, and is none of the
guards the ghost requires); DeadApprovalsDropped (T2: no effect holds an unconsumed approval whose author holds no role or chose
guarding). So that the cap is reached with these few approvers, the model lowers MAX_OPEN_APPROVALS to 1 (`CAP`: with 2, the first approval made while BOT2 is forced drops its approval before a second could be refused); the real 16 and the
guards' exemption at full size are covered by vectors `guards-are-never-crowded-out`, `a-full-guard-chain-is-claimable` and
`cap-counts-only-countable-approvals`, and by the fuzzer's bounds. The MAX_GUARDED_FOR bound is never reached here (vector
`guarded-tasks-are-bounded`). Not modelled, and covered by
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
         "t3": (BOT2, [BOT], "t2"), "t4": (BOT, [X], "t1"), "t5": (P, [BOT2], None)}       # t3, t4: delegation chains; t5: an outsider's task
PROPS = {"pr0": (BOT, X, None),                                       # made by init, approved by OP
         "pr1": (BOT, BOT, "t1"), "pr2": (BOT2, BOT2, "t2"), "pr3": (BOT, BOT, "t3"), "pr4": (BOT2, X, None),
         "pr5": (P, BOT2, None), "pr6": (OP, BOT, None), "pr7": (X, X, "t4"),
         "pr8": (O, P, "t2")}                                         # an owner charges a task it isn't assigned (the named task's agents)
LABEL = {H(k): k for k in list(TASKS) + list(PROPS)}

def fz(d): return tuple(sorted(d.items()))

class M:
    """A model state: Policy's state plus the ghost. The ghost: `gd` the agents that chose guarding; `rec` what was recorded on each task
    and effect when it was created (label → agents); `closed` the tasks closed since; `fresh` the (effect, approver) pairs with an
    approval no claim used up; `lastk` the guards whose approvals the latest claim of an effect used; `unconf`, `oprev`, `forced` the
    profile and policy facts."""
    __slots__ = ("labels", "state", "gd", "rec", "closed", "fresh", "lastk", "unconf", "oprev", "forced")
    def __init__(self, **kw):
        for k in self.__slots__: setattr(self, k, kw[k])
    def key(self):
        st = {k: v for k, v in self.state.items() if k not in ("entries", "head", "roleHistory")}
        body = json.dumps([st, sorted(self.gd), self.rec, sorted(self.closed), sorted(self.fresh), self.lastk, sorted(self.unconf), self.oprev, self.forced], sort_keys=True)
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

def required(m, pr):
    """The agents whose guard must approve a claim of `pr` now: those recorded when it (or a task it descends from) was created,
    and its proposer and executor if guarded now."""
    author, executor, _ = PROPS[pr]
    return set(dict(m.rec)[pr]) | {a for a in (author, executor) if guarded(m, a)}

def unmet(m, pr):
    """Why the ghost would refuse a claim of `pr` for its guards, or None."""
    for a in sorted(required(m, pr)):
        g = guard_of(m, a)
        if g is None: return f"{short(a)} has no confirmed operator"
        if not can_sign(m, g): return f"{short(a)}'s guard {short(g)} holds no role or has no valid key"
        if (pr, g) not in m.fresh: return f"{short(a)}'s guard {short(g)} has no unused approval"
    return None

def forbidden(m, author, kind, body):
    """P5: a guarded author may not approve, govern (leaving is not governing, R5) or steer others."""
    if not guarded(m, author): return False
    if kind == "member.set" and body == {"actor": author, "role": "none"}: return False
    if kind in ("effect.approve", "member.set"): return True
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
        got = set(dict(m.rec)[parent]) if parent else set()
        if guarded(m, author): got.add(author)
        return n.with_(rec=fz(dict(m.rec, **{label: tuple(sorted(got))})))
    if kind == "effect.propose":
        task = PROPS[label][2]
        got = set(dict(m.rec)[task]) if task else set()
        for t, agents in m.rec:                                           # T1: every open task that has the proposer as an assignee
            if t in TASKS and t not in m.closed and author in TASKS[t][1]: got |= set(agents)
        got |= {a for a in (author, body["executor"]) if guarded(m, a)}
        return n.with_(rec=fz(dict(m.rec, **{label: tuple(sorted(got))})))
    if kind == "effect.approve": return n.with_(fresh=n.fresh | {(LABEL[body["proposal"]], author)})
    if kind == "effect.claim":
        pr = LABEL[body["proposal"]]
        gs = tuple(sorted({guard_of(m, a) for a in required(m, pr)}))
        return n.with_(fresh=n.fresh - {(pr, g) for g in gs}, lastk=fz(dict(m.lastk, **{pr: gs})))
    if kind == "effect.outcome":
        pr = LABEL[body["proposal_"]]
        back = dict(m.lastk).get(pr, ()) if body["started"] == "no" else ()
        back = tuple(g for g in back if m.state["roles"].get(g) in POL.READ_ROLES)       # T2: a returned approval of a guard that left is dropped
        return n.with_(fresh=n.fresh | {(pr, g) for g in back}, lastk=fz(dict(m.lastk, **{pr: ()})))
    if kind == "policy.set": return n.with_(forced=body["policy"]["sessions"]["forceGuarded"])
    if kind == "task.update": return n.with_(closed=n.closed | {LABEL[body["task"]]})
    if kind == "member.set" and body["role"] not in POL.READ_ROLES:    # T2: an author that loses its role loses its open approvals
        return n.with_(fresh=frozenset(x for x in n.fresh if x[1] != body["actor"]))
    return n

def init(pol):
    m = M(labels=(), state=None, gd=frozenset(), rec=(), closed=frozenset(), fresh=frozenset(), lastk=(), unconf=frozenset(), oprev=False, forced=False)
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
        for who in (OP, OP2, O, BOT):
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
    if H("t2") in m.state["tasks"] and m.state["tasks"][H("t2")]["open"]:
        go("BOT2 closes t2", "task.update", BOT2, {"task": H("t2"), "state": "done"})
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

def check_state(m):
    if set(m.state["guarded"]) != m.gd:
        return f"ExactGuarding: Policy's guarded is {sorted(map(short, m.state['guarded']))}, the ghost's {sorted(map(short, m.gd))}"
    for label, want in m.rec:
        item = (m.state["tasks"] if label in TASKS else m.state["effects"])[H(label)]
        if sorted(item["guardedFor"]) != list(want) or len(item["guardedFor"]) != len(set(item["guardedFor"])):
            return f"ExactGuarding: {label} records {sorted(map(short, item['guardedFor']))}, the ghost {sorted(map(short, want))}"
    for pid, eff in m.state["effects"].items():
        authors = [a["author"] for a in eff["approvals"] if not a.get("consumedBy")]
        if len(authors) != len(set(authors)): return "OneOpenApprovalPerAuthor: an author has two open approvals on one proposal"
        dead = [a for a in authors if m.state["roles"].get(a) not in POL.READ_ROLES or a in m.gd]
        if dead: return f"DeadApprovalsDropped: {LABEL[pid]} holds an open approval by {short(dead[0])}, which holds no role or chose guarding"
    return None

def countable(m, pr, who):
    """T2, the ghost's count for the cap: open approvals on `pr` by authors other than `who` that hold a role, aren't guarded and
    are none of the guards a claim of `pr` would need now."""
    guards = {guard_of(m, a) for a in required(m, pr)}
    eff = m.state["effects"][H(pr)]
    return sum(1 for a in eff["approvals"] if not a.get("consumedBy") and a["author"] != who and a["author"] not in guards
               and m.state["roles"].get(a["author"]) in POL.READ_ROLES and not guarded(m, a["author"]))

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
    if not allowed and "guarded author may not" in reason and not forbidden(m, author, kind, body):
        return f"NotStricter: {kind} by {short(author)} refused as a guarded author's, though the ghost allows it"
    if kind == "agent.unguard":
        ok = unguard_ok(m, author, body["agent"])
        if allowed and not ok: return f"UnguardOnlyByTheGuard: {short(author)} unguarded {short(body['agent'])}"
        if ok and not allowed: return f"UnguardOnlyByTheGuard: the guard {short(author)} could not unguard {short(body['agent'])}: {reason}"
    if kind == "effect.claim":
        pr = LABEL[body["proposal"]]
        why = unmet(m, pr)
        if allowed and why: return f"GuardApproved: claim of {pr} allowed though {why} (needed: {sorted(map(short, required(m, pr)))})"
        if not allowed and reason in GUARD_REASONS and not why: return f"NotStricter: claim of {pr} refused ({reason}) though every guard the ghost needs approved"
        g = rule_approver_guarded(m, m2, body) if allowed and m2 is not None else None
        if g: return f"GuardedApprovalsUnused: claim of {pr} consumed the approval of {short(g)}, guarded at the claim"
    if kind == "effect.approve" and not allowed and reason == "too many open approvals":
        pr = LABEL[body["proposal"]]
        n = countable(m, pr, author)
        if n < CAP and m.state["roles"].get(author) in POL.READ_ROLES and not guarded(m, author):
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
    flags = {"guarded claim": False, "delegated claim": False, "guard denial": False, "restricted author": False, "guarded approver skipped": False,
             "cap refusal": False, "uncounted approval": False, "task-less delegated work": False}
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
                    eff = m.state["effects"][body["proposal"]]
                    if m.state["policy"]["approvals"] and any(not x.get("consumedBy") and x["author"] in (BOT, BOT2) and guarded(m, x["author"]) for x in eff["approvals"]):
                        flags["guarded approver skipped"] = True
                if not d["allow"] and "guarded author may not" in d.get("reason", ""): flags["restricted author"] = True
                if kind == "effect.approve" and not d["allow"] and d.get("reason") == "too many open approvals": flags["cap refusal"] = True
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

TASK_REC = 't["guardedFor"] = guarded_for(s, ctx, parent["guardedFor"] if parent else [], author)'
EFF_REC = '    out = list(state["tasks"][task_id]["guardedFor"]) if task_id else []'
EFF_RET = '    return guarded_for(state, ctx, out, proposer, executor)'
NEED = 'need = guarded_for(state, ctx, eff["guardedFor"], eff["proposer"], eff["executor"])'
FORCED = '    forced = agent and bool(state["policy"]["sessions"].get("forceGuarded"))'
GUARDED = '"guarded": forced or x in state["guarded"]'
GATE = '    if guard_status(state, ctx, author)["guarded"] and guarded_may_not(state, entry):'
KINDS = '    if k in ("effect.approve", "member.set"): return True'
STEER = '    if k == "session.input": return b["session"] in state["sessions"] and state["sessions"][b["session"]]["actor"] != entry["author"]'
UNGUARD = '    if x not in state["guarded"] or guard_status(state, ctx, x).get("guard") != entry["author"]:'
USABLE = '    return key_status(ctx, a["author"], a["keyId"]) == "valid" and not guard_status(state, ctx, a["author"])["guarded"]'
LEAVE = '    if k == "member.set" and b == {"actor": entry["author"], "role": "none"}: return False        # leaving is not governing\n'
MUTATIONS = {
    # what is recorded on the work (P2)
    "tasks-record-no-guarding": (TASK_REC, 't["guardedFor"] = []'),
    "tasks-drop-the-parents-agents": (TASK_REC, 't["guardedFor"] = guarded_for(s, ctx, [], author)'),
    "tasks-omit-a-guarded-author": (TASK_REC, 't["guardedFor"] = list(parent["guardedFor"]) if parent else []'),
    "effects-drop-the-named-tasks-agents": (EFF_REC, '    out = []'),
    "effects-omit-a-guarded-proposer": (EFF_RET, '    return guarded_for(state, ctx, out, executor)'),
    "effects-omit-a-guarded-executor": (EFF_RET, '    return guarded_for(state, ctx, out, proposer)'),
    # what a claim needs (P4)
    "claims-ignore-recorded-guarding": (NEED, 'need = guarded_for(state, ctx, [], eff["proposer"], eff["executor"])'),
    "claims-ignore-a-proposer-guarded-now": (NEED, 'need = guarded_for(state, ctx, eff["guardedFor"], eff["executor"])'),
    "claims-ignore-an-executor-guarded-now": (NEED, 'need = guarded_for(state, ctx, eff["guardedFor"], eff["proposer"])'),
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
    "the-choice-does-not-guard": (GUARDED, '"guarded": forced'),
    "forcing-does-not-guard": (GUARDED, '"guarded": x in state["guarded"]'),
    "humans-may-choose-guarding": ('        if b["steerMode"] == "guarded" and not is_human(ctx, author):\n            s["guarded"]', '        if b["steerMode"] == "guarded":\n            s["guarded"]'),
    "guarded-not-sticky": ('            s["guarded"][author] = True                                          # sticky: only its guard unguards it', '            pass'),
    "agent-closing-unguards-itself": ('        del s["sessions"][b["session"]]', '        if author == s["sessions"][b["session"]]["actor"]: s["guarded"].pop(author, None)\n        del s["sessions"][b["session"]]'),
    # unguard (P6)
    "unguard-does-nothing": ('        s["guarded"].pop(b["agent"], None)', '        pass'),
    "anyone-unguards": (UNGUARD, '    if x not in state["guarded"]:'),
    "unguarding-an-agent-that-chose-nothing": (UNGUARD, '    if guard_status(state, ctx, x).get("guard") != entry["author"]:'),
    "unguard-needs-no-role": ('    return ALLOW if role in READ_ROLES else deny("guard holds no role")', '    return ALLOW'),
    # what a guarded author may not do (P5)
    "guarded-authors-unrestricted": (GATE, '    if False:'),
    "only-a-chosen-guarding-restricts": (GATE, '    if author in state["guarded"] and guarded_may_not(state, entry):'),
    "guarded-agents-may-approve": (KINDS, '    if k in ("member.set",): return True'),
    "guarded-agents-may-govern": (KINDS, '    if k in ("effect.approve",): return True'),
    "guarded-agents-may-steer-others": (STEER, '    if k == "session.input": return False'),
    "guarded-agents-may-not-delegate": (KINDS, '    if k in ("effect.approve", "member.set", "task"): return True'),
    "guarded-agents-may-not-propose": (KINDS, '    if k in ("effect.approve", "member.set", "effect.propose"): return True'),
    "guarded-agents-may-not-steer-themselves": (STEER, '    if k == "session.input": return True'),
    # round 28: a guarded author's approval never counts (R2); a guarded agent may leave (R5)
    "guarded-approvers-count": (USABLE, '    return key_status(ctx, a["author"], a["keyId"]) == "valid"'),
    "only-a-chosen-guarding-stops-an-approver": (USABLE, '    return key_status(ctx, a["author"], a["keyId"]) == "valid" and a["author"] not in state["guarded"]'),
    "guarded-agents-may-not-leave": (LEAVE, ''),
    # round 29: an assignee carries its open tasks' guarding (T1); the cap counts only what can still count (T2)
    "effects-ignore-the-proposers-open-tasks": ('        out += [a for a in t["guardedFor"] if a not in out]', '        pass'),
    "closed-tasks-still-carry": ('if t["open"] and proposer in t["to"]), key=', 'if proposer in t["to"]), key='),
    "the-cap-counts-forced-authors": ('    return role_of(state, actor) in READ_ROLES and not guard_status(state, ctx, actor)["guarded"]', '    return role_of(state, actor) in READ_ROLES and actor not in state["guarded"]'),
    "the-cap-counts-every-open-approval": ('    return role_of(state, actor) in READ_ROLES and not guard_status(state, ctx, actor)["guarded"]', '    return True'),
    "removed-authors-keep-open-approvals": ('        if new not in READ_ROLES: drop_open_approvals(s, tgt)', '        pass'),
    "guarded-authors-keep-open-approvals": ('            drop_open_approvals(s, author)', '            pass'),
    "returned-approvals-of-departed-authors-stay": ('                    elif role_of(s, a["author"]) not in READ_ROLES or a["author"] in s["guarded"]:', '                    elif False:'),
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
    print(f"[guard] {n} transitions to depth {a.depth}; GuardApproved, GuardedRestricted, GuardedApprovalsUnused, UnguardOnlyByTheGuard, NotStricter, "
          f"ExactGuarding, OneOpenApprovalPerAuthor, CapLiveness and DeadApprovalsDropped HOLD (non-vacuous: {', '.join(flags)})")

if __name__ == "__main__":
    main()

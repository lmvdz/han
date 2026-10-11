"""Bounded exhaustive check of P-15 guarded steering against the real policy.py.

The ghost is derived from the invariant sentence of P-15, not from Policy's rules:

    While an agent is guarded, any instruction to it from anyone but its guard holds it; while it is held, nothing it
    proposes or is named executor of runs without its guard's own approval; only its guard ends a hold.

"Guarded" = the agent's own sticky choice (it opened a guarded session; only it, while not held, undoes that), or forced
by owners (one of its live sessions is effectively guarded). "Instruction" = a steer into any of its sessions, a task
naming it, a proposal naming it as executor. Two agents, BOT (operator OP) and BOT2 (operator OP2), both of which can
become guarded; owner O and member P instruct; OP's role and key vary; the approval rule and allowed modes vary.

Properties: GuardedClaimsNeedTheirHuman, HeldInstructNoOne (no task, approval or steer of another's session by a held
agent), NeverLooser (no outside steer into a dead session), GuardRightsNeedRole, OneOpenApprovalPerAuthor, and
ExactHolds (Policy holds exactly the agents the invariant holds: never fewer, never more).
States are deduplicated on content (Policy state minus history, plus the ghost).

Usage: python ref/model_guard.py [--depth N] [--all-mutations]"""
import argparse, hashlib, json, os, sys, types
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import policy as POL

SRC = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "policy.py"), encoding="utf-8").read()
DEPTH = 5                      # full runs (run_all, §10); --quick runs depth 4; mutations run at depth 4
SP = "sg:a.example"
O, P, OP, OP2, X = "@o:a.example", "@p:a.example", "@op:b.example", "@op2:b.example", "@x:c.example"
BOT, BOT2 = "@bot:b.example", "@bot2:b.example"
GUARD = {BOT: OP, BOT2: OP2}
T0 = "2026-10-10T12:00:00Z"
DG = "d" * 64
MODESETS = (("auto", "guarded"), ("guarded",), ("auto",))

def H(label): return hashlib.sha256(label.encode()).hexdigest()
def human(rev=False): return {"actorKind": "human", "operatorConfirmed": False, "keys": {"k1": {"publicKey": "pk", "status": "revoked" if rev else "valid"}}}
def agent(op): return {"actorKind": "agent", "operator": op, "operatorConfirmed": True, "keys": {"k1": {"publicKey": "pk", "status": "valid"}}}
XS = {"actorKind": "service", "operatorConfirmed": True, "keys": {"k1": {"publicKey": "pk", "status": "valid"}}}

def policy(rule=False, modes=MODESETS[0]):
    return {"visibility": "members", "join": "invite", "membersMayInvite": False, "allowAgents": "any",
            "approvals": [{"action": "pay", "approvers": ["member"]}] if rule else [], "separateApprover": True, "budget": None,
            "maxDelegationDepth": 4, "minDurability": "local", "launchWindow": 30, "extensionsMinRole": "member",
            "sessions": {"watch": "member", "read": "member", "steer": "member", "modes": list(modes)}, "servers": {"deny": []}}

def E(label, kind, author, body):
    return {"id": H(label), "v": 1, "space": SP, "kind": kind, "author": author, "keyId": "k1", "body": body,
            "contentHash": "", "refs": [], "seen": 0, "nonce": label, "time": T0}

S1, S2, S3, S4 = H("s1"), H("s2"), H("s3"), H("s4")
SESS = {S1: ("s1", BOT, "guarded"), S2: ("s2", BOT2, "auto"), S3: ("s3", BOT, "auto"), S4: ("s4", BOT2, "guarded")}
OPENED_AT_START = (S1, S2, S3)                                   # S4: BOT2 may open it later (becoming guarded)
PROPS = {"pr1": (BOT, X), "pr2": (BOT2, X), "pr3": (P, BOT), "pr4": (BOT, BOT2), "pr5": (OP, BOT)}   # pr5: its own guard's
CLAIMABLE = ("pr0", "pr1", "pr2", "pr3", "pr4", "pr5")
TASKS = {"P tasks BOT": (P, [BOT]), "P tasks BOT2+BOT": (P, [BOT2, BOT]), "BOT tasks BOT2": (BOT, [BOT2]), "BOT2 tasks BOT": (BOT2, [BOT])}

def fz(d): return tuple(sorted(d.items()))

class M:
    __slots__ = ("labels", "state", "oprev", "gd", "held", "need", "covered", "used", "lastk", "cl")
    def __init__(self, **kw):
        for k in self.__slots__: setattr(self, k, kw[k])
    def key(self):
        st = {k: v for k, v in self.state.items() if k not in ("entries", "head", "roleHistory")}
        return (json.dumps(st, sort_keys=True), self.oprev, self.gd, self.held, self.need, self.covered, self.used, self.lastk, self.cl)
    def with_(self, **kw):
        d = {k: getattr(self, k) for k in self.__slots__}; d.update(kw); return M(**d)

def ctx(m):
    return {"time": T0, "profiles": {O: human(), P: human(), OP: human(m.oprev), OP2: human(), X: XS, BOT: agent(OP), BOT2: agent(OP2)}}

def apply(pol, m, e, label):
    d = pol.decide(m.state, e, ctx(m))
    if not d["allow"]: return None, d
    s = pol.fold(m.state, {"entry": e, "content": None, "receipt": {"space": SP, "seq": (m.state["head"] + 1) if m.state else 1,
                                                                    "id": e["id"], "time": T0}}, ctx(m))
    return m.with_(labels=m.labels + (label,), state=s), d

def cnt(t, k): return dict(t).get(k, 0)
def setk(t, k, v): d = dict(t); d[k] = v; return fz(d)

# ── the ghost: the invariant sentence, read independently ──
def mode_now(state, sid):
    _, _, mode = SESS[sid]; allowed = state["policy"]["sessions"]["modes"]
    if mode in allowed: return mode
    return "guarded" if mode == "auto" and "guarded" in allowed else None

def alive(state, sid):
    _, actor, _ = SESS[sid]
    se = state["sessions"].get(sid)
    if not se or not se["open"] or state["roles"].get(actor) is None: return False
    mode = mode_now(state, sid)
    if mode is None: return False
    return mode == "auto" or state["roles"].get(GUARD[actor]) is not None

def guarded(m, a):
    return dict(m.gd).get(a, False) or any(SESS[sid][1] == a and alive(m.state, sid) and mode_now(m.state, sid) == "guarded" for sid in SESS)

def instruct(m, author, a):
    """The ghost's held map after `author` instructs agent `a`."""
    held = dict(m.held)
    if a in GUARD and guarded(m, a) and author not in (a, GUARD[a]) and a not in held: held[a] = GUARD[a]
    return fz(held)

def settle(m):
    """The invariant's "while guarded": after every step, a guarded agent with an outside instruction still open that its
    guard hasn't cleared (since its last release) is held, and an open effect aimed at it records the guard. Read from
    the facts in the log (who tasked whom, who proposed what), not from Policy's rules."""
    held = dict(m.held); need = set(m.need); cl = dict(m.cl)
    for a, g in GUARD.items():
        if not guarded(m, a): continue
        since = cl.get(a, 0)
        tasks = [t for t in m.state["tasks"].values() if t["open"] and a in t["to"] and t["author"] not in (a, g) and t["seq"] > since]
        effs = [(pr, e) for pr in CLAIMABLE for e in [m.state["effects"].get(H(pr))]
                if e and e["phase"] not in POL.FINAL and e["executor"] == a and e["proposer"] not in (a, g) and e["seq"] > since]
        if (tasks or effs) and a not in held: held[a] = g
        for pr, _ in effs: need.add((pr, g))
    return m.with_(held=fz(held), need=frozenset(need))

def required(m, pr, proposer, executor):
    """Whose own approval a claim of `pr` needs: a guard the proposer was held for when proposing, and the guards its
    proposer and executor are held for now."""
    held = dict(m.held); out = set(g for (p_, g) in m.need if p_ == pr)
    for who in (proposer, executor):
        if who in held: out.add(held[who])
    return out

def init(pol):
    m = M(labels=(), state=None, oprev=False, gd=(), held=(), need=frozenset(), covered=(), used=(), lastk=(), cl=())
    m, d = apply(pol, m, E("C", "space.create", O, {"policy": policy()}), "C"); assert m, d
    for lab, a in (("m1", P), ("m2", X), ("m3", OP), ("m4", OP2), ("m5", BOT), ("m6", BOT2)):
        m, d = apply(pol, m, E(lab, "member.set", O, {"actor": a, "role": "member"}), lab); assert m, (lab, d)
    for sid in OPENED_AT_START:
        lab, a, mode = SESS[sid]
        m, d = apply(pol, m, E(lab, "session.open", a, {"steerMode": mode}), lab); assert m, (lab, d)
    m = m.with_(gd=fz({BOT: True}))
    m, d = apply(pol, m, E("pr0", "effect.propose", BOT, {"executor": X, "action": "pay", "planDigest": DG, "repeatable": True}), "pr0")
    assert m, d                                                                               # proposed before any hold
    m2, d = apply(pol, m, E("in0", "session.input", P, {"session": S1}), "in0"); assert m2, d     # P's outside steer
    m2 = settle(m2.with_(held=instruct(m, P, BOT)))
    m3, d = apply(pol, m2, E("ap0", "effect.approve", OP, {"proposal": H("pr0"), "planDigest": DG}), "ap0"); assert m3, d   # its guard approves once
    return m3.with_(covered=setk(m3.covered, ("pr0", OP), 1))

def successors(pol, m):
    out = []
    def go(name, kind, author, body, after=None, label=None):
        lab = label or (name + "#" + str(sum(1 for x in m.labels if x == name)))
        m2, d = apply(pol, m, E(lab, kind, author, body), name)
        if m2 and after: m2 = after(m2)
        if m2: m2 = settle(m2)
        out.append((name, kind, author, body, m2, d))
    for author in (P, O, OP2, OP, BOT, BOT2):
        for sid, (lab, actor, _) in SESS.items():
            if sid not in m.state["sessions"]: continue
            go(f"{author} steers {lab}", "session.input", author, {"session": sid},
               lambda n, a=author, x=actor: n.with_(held=instruct(m, a, x)))
    for author in (OP, OP2, O):
        for sid, a in ((S1, BOT), (S2, BOT2)):
            go(f"{author} releases {SESS[sid][0]}", "session.input", author, {"session": sid, "release": True},
               (lambda n, a=a: n.with_(held=fz({k: v for k, v in m.held if k != a}), cl=setk(m.cl, a, n.state["head"])))
               if dict(m.held).get(a) == author else None)
    for name, (author, to) in TASKS.items():
        def after(n, author=author, to=to):
            mm = m
            for a in to: mm = mm.with_(held=instruct(mm, author, a))
            return n.with_(held=mm.held)
        go(name, "task", author, {"to": to}, after)
    if S4 not in m.state["sessions"]:
        def opened(n):
            return n.with_(gd=setk(m.gd, BOT2, True))          # becoming guarded; settle() applies open outside instructions
        go("BOT2 opens s4 guarded", "session.open", BOT2, {"steerMode": "guarded"}, opened, label="s4")
    for author in (BOT, O):
        def closed(n, author=author):
            if author != BOT or BOT in dict(m.held): return n
            return n.with_(gd=setk(m.gd, BOT, False))          # S1 is BOT's only guarded session
        go(f"{author} closes s1", "session.close", author, {"session": S1}, closed)
    for pr, (author, executor) in PROPS.items():
        if H(pr) in m.state["effects"]: continue
        body = {"executor": executor, "action": "pay", "planDigest": DG, "repeatable": pr == "pr1"}
        g = dict(m.held).get(author)
        go(f"{author} proposes {pr}", "effect.propose", author, body,
           lambda n, pr=pr, g=g, a=author, x=executor: n.with_(need=n.need | ({(pr, g)} if g else set()), held=instruct(m, a, x)), label=pr)
    for pr in CLAIMABLE:
        pid = H(pr); eff = m.state["effects"].get(pid)
        if not eff or eff["phase"] in POL.FINAL or len(eff["claims"]) >= 2: continue
        for who in (OP, OP2, P, BOT, O):
            go(f"{who} approves {pr}", "effect.approve", who, {"proposal": pid, "planDigest": DG},
               lambda n, who=who, pr=pr: n.with_(covered=setk(n.covered, (pr, who), cnt(n.used, (pr, who)) + 1)))
        a = len(eff["claims"]) + 1
        req = tuple(sorted(required(m, pr, eff["proposer"], eff["executor"])))
        go(f"{eff['executor']} claims {pr}#{a}", "effect.claim", eff["executor"], {"proposal": pid, "attempt": a, "instance": "k1", "window": 30},
           lambda n, pr=pr, req=req: n.with_(used=fz({**dict(n.used), **{(pr, g): cnt(n.used, (pr, g)) + 1 for g in req}}), lastk=setk(n.lastk, pr, req)))
        if eff["phase"] == "claimed":
            c = eff["claims"][-1]; lk = dict(m.lastk).get(pr) or ()
            go(f"{eff['executor']} not started {pr}", "effect.outcome", eff["executor"], {"claim": c["id"], "result": "failed", "started": "no"},
               lambda n, pr=pr, lk=lk: n.with_(used=fz({**dict(n.used), **{(pr, g): cnt(n.used, (pr, g)) - 1 for g in lk}}), lastk=setk(n.lastk, pr, ())))
            go(f"{eff['executor']} failed {pr}", "effect.outcome", eff["executor"], {"claim": c["id"], "result": "failed", "started": "yes"},
               lambda n, pr=pr: n.with_(lastk=setk(n.lastk, pr, ())))
    if m.state["roles"].get(OP): go("O removes OP", "member.set", O, {"actor": OP, "role": "none"})
    else: go("O re-adds OP", "member.set", O, {"actor": OP, "role": "member"})
    if not m.oprev:
        out.append(("OP key revoked", "key", OP, {}, m.with_(oprev=True, labels=m.labels + ("OP key revoked",)), {"allow": True}))
    pol_now = m.state["policy"]
    rule, modes = bool(pol_now["approvals"]), tuple(pol_now["sessions"]["modes"])
    go("O toggles approval rule", "policy.set", O, {"policy": policy(not rule, modes)})
    for ms_ in MODESETS:
        if ms_ != modes: go(f"O sets modes {'/'.join(ms_)}", "policy.set", O, {"policy": policy(rule, ms_)})
    return out

def check_state(pol, m):
    if dict(m.held) != m.state["held"]:
        return f"ExactHolds: Policy holds {m.state['held']} but the invariant holds {dict(m.held)}"
    if m.state["roles"].get(OP) is None and any(any(pol.session_access(m.state, OP, sid).values()) for sid in (S1, S3)):
        return "a guard without a role keeps rights on its agent's session"
    for pid, eff in m.state["effects"].items():
        authors = [a["author"] for a in eff["approvals"] if not a.get("consumedBy") and not a.get("dropped")]
        if len(authors) != len(set(authors)): return "an author has two open approvals on one proposal"
    return None

def check(m, name, kind, author, body, d):
    if not d["allow"]: return None
    if kind == "session.input" and not body.get("release") and author != SESS[body["session"]][1] and not alive(m.state, body["session"]):
        return f"outside steer allowed into a dead session {SESS[body['session']][0]}"
    held = dict(m.held)
    if author in held:
        if kind in ("task", "effect.approve"): return f"held {author} allowed {kind}"
        if kind == "session.input" and SESS[body["session"]][1] != author: return f"held {author} allowed to steer another's session"
    if kind == "effect.claim":
        pr = name.split()[2].split("#")[0]
        eff = m.state["effects"][H(pr)]
        for g in required(m, pr, eff["proposer"], eff["executor"]):
            if m.state["roles"].get(g) is None: return f"claim of {pr} allowed while {g}, whose approval it needs, holds no role"
            if g == OP and m.oprev: return f"claim of {pr} allowed after {g}'s key was revoked"
            if cnt(m.covered, (pr, g)) - cnt(m.used, (pr, g)) < 1: return f"claim of {pr} allowed without an unused approval by {g}"
    return None

def explore(pol, depth):
    try:
        start = init(pol)
    except AssertionError as ex:                             # Policy disagrees with the invariant already in the setup
        return 0, f"setup step refused or diverged: {ex}", ("init",), False, False
    seen = {start.key()}; frontier = [(start, ())]
    n, saw_claim, saw_denial = 0, False, False
    for _ in range(depth):
        nxt = []
        for m, path in frontier:
            for name, kind, author, body, m2, d in successors(pol, m):
                n += 1
                v = check(m, name, kind, author, body, d)
                if v: return n, v, path + (name,), saw_claim, saw_denial
                if kind == "effect.claim":
                    pr = name.split()[2].split("#")[0]; eff = m.state["effects"][H(pr)]
                    if required(m, pr, eff["proposer"], eff["executor"]):
                        saw_claim |= d["allow"]; saw_denial |= (not d["allow"] and "guard" in d.get("reason", ""))
                if m2 is not None:
                    k = m2.key()
                    if k in seen: continue
                    v = check_state(pol, m2)
                    if v: return n, v, path + (name,), saw_claim, saw_denial
                    seen.add(k); nxt.append((m2, path + (name,)))
        frontier = nxt
    return n, None, None, saw_claim, saw_denial

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

MUTATIONS = {
    "held-gate-off": ('    if author in state["held"] and not held_may(state, entry):', '    if False:'),
    "held-may-relay": ('        return bool(sess) and sess["actor"] == entry["author"]', '        return True'),
    "held-may-delegate": ('HELD_KINDS = {"message",', 'HELD_KINDS = {"task", "message",'),
    "held-may-approve": ('HELD_KINDS = {"message",', 'HELD_KINDS = {"effect.approve", "message",'),
    "held-proposals-unguarded": ('        if author in s["held"]: eff["guards"] = [s["held"][author]]; eff["phase"] = "awaiting-approval"', '        pass'),
    "held-proposers-older-proposals-free": ('    for who in (eff["proposer"], eff["executor"]):', '    for who in (eff["executor"],):'),
    "held-executors-unguarded": ('    for who in (eff["proposer"], eff["executor"]):', '    for who in (eff["proposer"],):'),
    "no-settling": ('    settle_holds(s)\n', ''),
    "settled-holds-ignore-tasks": ('        if (tasks or effs) and x not in s["held"]: s["held"][x] = g', '        if effs and x not in s["held"]: s["held"][x] = g'),
    "settled-holds-ignore-effects": ('        if (tasks or effs) and x not in s["held"]: s["held"][x] = g', '        if tasks and x not in s["held"]: s["held"][x] = g'),
    "settled-tasks-first-assignee-only": ('        tasks = [t for t in s["tasks"].values() if t["open"] and x in t["to"]', '        tasks = [t for t in s["tasks"].values() if t["open"] and x in t["to"][:1]'),
    "executor-guard-not-recorded": ('                e.setdefault("guards", []).append(g)', '                pass'),
    "release-does-not-clear": ('        if b.get("release"): s["held"].pop(x, None); s["cleared"][x] = rc["seq"]', '        if b.get("release"): s["held"].pop(x, None)'),
    "owner-forced-guarding-ignored": ('    for se in state["sessions"].values():\n        if se["actor"] == x and "guard" in se and live(state, se) and effective_mode(state, se) == "guarded": return se["guard"]\n    return None', '    return None'),
    "guarded-not-sticky": ('                s["guarded"][author] = p["operator"]                            # sticky until it unguards itself', '                pass'),
    "anyone-closing-unguards": ('        if (author == x and x not in s["held"] and sess["steerMode"] == "guarded"', '        if (x not in s["held"] and sess["steerMode"] == "guarded"'),
    "guards-not-required": ('        if res == "none": return deny("E3 a guard hasn\'t approved")', '        pass'),
    "guard-approval-needs-no-role": ('    if role_of(state, g) not in READ_ROLES: return "none", None\n    for a in eff["approvals"]:\n        if a["author"] != g', '    for a in eff["approvals"]:\n        if a["author"] != g'),
    "owners-stand-in": [('    is_guard = author in guards and role in READ_ROLES', '    is_guard = (author in guards or role == "owner") and role in READ_ROLES'),
                        ('        if a["author"] != g or a.get("consumedBy")', '        if (a["author"] != g and role_of(state, a["author"]) != "owner") or a.get("consumedBy")')],
    "guard-key-ignored": ('        if st == "valid": return "ok", a\n    return "none", None\n\ndef live', '        return "ok", a\n    return "none", None\n\ndef live'),
    "anyone-releases": ('        if state["held"].get(sess["actor"]) != entry["author"]: return deny("only the guard holding this agent releases it")', '        pass'),
    "modes-ignored-for-open-sessions": ('    return next((m for m in CAUTION[CAUTION.index(sess["steerMode"]):] if m in allowed), None)', '    return sess["steerMode"]'),
    "owners-may-loosen-modes": ('    return next((m for m in CAUTION[CAUTION.index(sess["steerMode"]):] if m in allowed), None)', '    return next((m for m in CAUTION if m in allowed), None)'),
    "guarded-without-guard-stays-live": (' or role_of(state, sess.get("guard")) in READ_ROLES))', ' or "guard" in sess))'),
    "banned-guard-keeps-rights": ('    if actor == s["actor"] or (actor == s.get("guard") and role_of(state, actor) in READ_ROLES):', '    if actor == s["actor"] or actor == s.get("guard"):'),
    "guard-approval-reused": ('        for a in chosen: a["consumedBy"] = eid', '        pass'),
    "returned-approvals-unsuperseded": ('                        a["dropped"] = True                    # superseded while it was consumed', '                        pass'),
}

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--depth", type=int, default=DEPTH)
    ap.add_argument("--all-mutations", action="store_true")
    a = ap.parse_args()
    if a.all_mutations:
        missed = 0
        for name, edits in MUTATIONS.items():
            n, v, path, _, _ = explore(mutant(*(edits if isinstance(edits, list) else [edits])), a.depth)
            if v: print(f"caught  {name:38} after {n} transitions: {v}  [{' → '.join(path)}]", flush=True)
            else: print(f"MISSED  {name:38} no violation in {n} transitions", flush=True); missed += 1
        print(f"{len(MUTATIONS) - missed}/{len(MUTATIONS)} guard mutations caught by a property")
        sys.exit(1 if missed else 0)
    n, v, path, g, gd = explore(POL, a.depth)
    if v: print(f"VIOLATION after {n} transitions: {v}\n  path: {' → '.join(path)}"); sys.exit(1)
    if not (g and gd): print(f"VACUOUS: guarded claim seen={g}, guard denial seen={gd}"); sys.exit(1)
    print(f"[guard] {n} transitions to depth {a.depth}; GuardedClaimsNeedTheirHuman, HeldInstructNoOne, NeverLooser, "
          f"GuardRightsNeedRole, OneOpenApprovalPerAuthor and ExactHolds HOLD (non-vacuous)")

if __name__ == "__main__":
    main()

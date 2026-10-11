"""Bounded exhaustive check of P-15 guarded steering against the real policy.py.

The model tracks the ground truth independently of Policy: a *ghost* set of agents whose next actions were influenced by
an outside steer (directly into a guarded or suggest session, or relayed by an influenced agent through a session.input
or a task), cleared only when the agents' human steers them. Property GuardedClaimsNeedTheirHuman: every claim Policy
allows on an effect proposed under influence is covered by an approval of the human (OP) that is not yet used by an
earlier claim, while OP holds a role and its key is valid. Non-vacuity: some run claims an influenced effect, and some
run is denied a claim for lack of OP's approval.

Usage: python ref/model_guard.py [--depth N] [--all-mutations]"""
import argparse, copy, json, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import policy as POL

SRC = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "policy.py"), encoding="utf-8").read()
SP = "sg:a.example"
O, P, OP, X, BOT, BOT2 = "@o:a.example", "@p:a.example", "@op:b.example", "@x:c.example", "@bot:b.example", "@bot2:b.example"
T0 = "2026-10-10T12:00:00Z"
DG = "d" * 64

def H(label):
    import hashlib
    return hashlib.sha256(label.encode()).hexdigest()

def human(rev=False): return {"actorKind": "human", "operatorConfirmed": False, "keys": {"k1": {"publicKey": "pk", "status": "revoked" if rev else "valid"}}}
AG = {"actorKind": "agent", "operator": OP, "operatorConfirmed": True, "keys": {"k1": {"publicKey": "pk", "status": "valid"}}}
XS = {"actorKind": "service", "operatorConfirmed": True, "keys": {"k1": {"publicKey": "pk", "status": "valid"}}}

def policy(rule=False):
    return {"visibility": "members", "join": "invite", "membersMayInvite": False, "allowAgents": "any",
            "approvals": [{"action": "pay", "approvers": ["owner"]}] if rule else [], "separateApprover": False, "budget": None,
            "maxDelegationDepth": 4, "minDurability": "local", "launchWindow": 30, "extensionsMinRole": "member",
            "sessions": {"watch": "member", "read": "member", "steer": "member", "modes": ["auto", "guarded", "suggest"]}, "servers": {"deny": []}}

def E(label, kind, author, body):
    return {"id": H(label), "v": 1, "space": SP, "kind": kind, "author": author, "keyId": "k1", "body": body,
            "contentHash": "", "refs": [], "seen": 0, "nonce": label, "time": T0}

S1, S2, TK = H("s1"), H("s2"), H("t")
PROPS = {"pr1": (BOT, None), "pr2": (BOT2, None), "pr3": (BOT2, TK)}

class M:
    """Model state: the log (as a tuple of entry labels), Policy state, OP key status, and the ghost."""
    __slots__ = ("labels", "state", "oprev", "ghost", "task_ghost", "need", "covered", "used")
    def __init__(self, **kw):
        for k in self.__slots__: setattr(self, k, kw[k])
    def key(self): return (self.labels, self.oprev, self.ghost, self.task_ghost, self.need, self.covered, self.used)
    def with_(self, **kw):
        d = {k: getattr(self, k) for k in self.__slots__}; d.update(kw); return M(**d)

def ctx(m):
    return {"time": T0, "profiles": {O: human(), P: human(), OP: human(m.oprev), X: XS, BOT: AG, BOT2: AG}}

def step(pol, m, label, kind, author, body):
    e = E(label + "#" + str(len(m.labels)), kind, author, body)
    d = pol.decide(m.state, e, ctx(m))
    if not d["allow"]: return None, d
    s = pol.fold(m.state, {"entry": e, "content": None, "receipt": {"space": SP, "seq": m.state["head"] + 1, "id": e["id"], "time": T0}}, ctx(m))
    return m.with_(labels=m.labels + (label,), state=s), d

def init(pol):
    m = M(labels=(), state=None, oprev=False, ghost=frozenset(), task_ghost=False, need=frozenset(), covered=(), used=())
    e = E("C", "space.create", O, {"policy": policy()})
    s = pol.fold(None, {"entry": e, "content": None, "receipt": {"space": SP, "seq": 1, "id": e["id"], "time": T0}}, ctx(m))
    m = m.with_(state=s)
    for lab, a, r in (("m1", P, "member"), ("m2", X, "member"), ("m3", OP, "member"), ("m4", BOT, "member"), ("m5", BOT2, "member")):
        m, d = step(pol, m, lab, "member.set", O, {"actor": a, "role": r}); assert m, (lab, d)
    for lab, a, mode in (("s1", BOT, "guarded"), ("s2", BOT2, "auto")):
        e = E(lab, "session.open", a, {"steerMode": mode})
        assert pol.decide(m.state, e, ctx(m))["allow"]
        s = pol.fold(m.state, {"entry": e, "content": None, "receipt": {"space": SP, "seq": m.state["head"] + 1, "id": e["id"], "time": T0}}, ctx(m))
        m = m.with_(state=s, labels=m.labels + (lab,))
    return m

def cnt(t, k): return dict(t).get(k, 0)
def bump(t, k, n=1): d = dict(t); d[k] = d.get(k, 0) + n; return tuple(sorted(d.items()))

def successors(pol, m):
    """(name, new model state or None, decision, kind of action)"""
    out = []
    def go(name, kind, author, body, after=None):
        m2, d = step(pol, m, name, kind, author, body)
        if m2 and after: m2 = after(m2)
        out.append((name, kind, m2, d, body))
    go("P steers BOT", "session.input", P, {"session": S1}, lambda n: n.with_(ghost=n.ghost | {BOT}))
    go("OP steers BOT", "session.input", OP, {"session": S1}, lambda n: n.with_(ghost=n.ghost - {BOT}))
    go("BOT steers itself", "session.input", BOT, {"session": S1})
    go("OP steers BOT2", "session.input", OP, {"session": S2}, lambda n: n.with_(ghost=n.ghost - {BOT2}))
    go("BOT relays to BOT2", "session.input", BOT, {"session": S2}, lambda n: n.with_(ghost=n.ghost | ({BOT2} if BOT in m.ghost else set())))
    if TK not in m.state["tasks"]:
        m2, d = step(pol, m, "BOT tasks BOT2", "task", BOT, {"to": [BOT2]})
        if m2:   # the task needs a fixed id so pr3 can name it
            e = E("t", "task", BOT, {"to": [BOT2]})
            s = pol.fold(m.state, {"entry": e, "content": None, "receipt": {"space": SP, "seq": m.state["head"] + 1, "id": e["id"], "time": T0}}, ctx(m))
            m2 = m.with_(labels=m.labels + ("BOT tasks BOT2",), state=s, task_ghost=BOT in m.ghost)
        out.append(("BOT tasks BOT2", "task", m2, d, {}))
    for pr, (author, task) in PROPS.items():
        pid = H(pr)
        if pid in m.state["effects"]:
            continue
        if task and task not in m.state["tasks"]: continue
        body = {"executor": X, "action": "pay", "planDigest": DG, "repeatable": pr == "pr1"}
        if task: body["task"] = task
        e = E(pr, "effect.propose", author, body)
        d = pol.decide(m.state, e, ctx(m))
        m2 = None
        if d["allow"]:
            s = pol.fold(m.state, {"entry": e, "content": None, "receipt": {"space": SP, "seq": m.state["head"] + 1, "id": e["id"], "time": T0}}, ctx(m))
            influenced = author in m.ghost or (task is not None and m.task_ghost)
            m2 = m.with_(labels=m.labels + (pr,), state=s, need=m.need | ({pr} if influenced else set()))
        out.append((f"{author} proposes {pr}", "effect.propose", m2, d, body))
    for pr in PROPS:
        pid = H(pr)
        eff = m.state["effects"].get(pid)
        if not eff: continue
        if eff["phase"] in POL.FINAL or len(eff["claims"]) >= 2: continue
        for who in (OP, P, O):
            if cnt(m.covered, (pr, who)) - cnt(m.used, (pr, who)) >= 1: continue     # one open approval each is enough
            ok = role_ok = m.state["roles"].get(who) in POL.READ_ROLES
            def after(n, who=who, pr=pr): return n.with_(covered=bump(n.covered, (pr, who)))
            go(f"{who} approves {pr}", "effect.approve", who, {"proposal": pid, "planDigest": DG}, after if who == OP else None)
        a = len(eff["claims"]) + 1
        def after_claim(n, pr=pr):
            return n.with_(used=bump(n.used, (pr, OP))) if pr in m.need else n
        go(f"X claims {pr}#{a}", "effect.claim", X, {"proposal": pid, "attempt": a, "instance": "k1", "window": 30}, after_claim)
        if eff["phase"] == "claimed":
            c = eff["claims"][-1]
            go(f"X not started {pr}", "effect.outcome", X, {"claim": c["id"], "result": "failed", "started": "no"},
               (lambda n, pr=pr: n.with_(used=bump(n.used, (pr, OP), -1))) if pr in m.need else None)
            go(f"X failed {pr}", "effect.outcome", X, {"claim": c["id"], "result": "failed", "started": "yes"})
    if m.state["roles"].get(OP):
        go("O removes OP", "member.set", O, {"actor": OP, "role": "none"})
    else:
        go("O re-adds OP", "member.set", O, {"actor": OP, "role": "member"})
    if not m.oprev:
        out.append(("OP key revoked", "key", m.with_(oprev=True, labels=m.labels + ("OP key revoked",)), {"allow": True}, {}))
    rule = bool(m.state["policy"]["approvals"])
    go("O toggles approval rule", "policy.set", O, {"policy": policy(not rule)})
    return out

def check(pol, m, name, kind, m2, d):
    """Returns a violation message, or None. Called for every attempted claim on an influenced effect."""
    if kind != "effect.claim" or not d["allow"]: return None
    pr = name.split()[2].split("#")[0]
    if pr not in m.need: return None
    if m.state["roles"].get(OP) not in POL.READ_ROLES: return f"claim of {pr} allowed while OP holds no role"
    if m.oprev: return f"claim of {pr} allowed after OP's key was revoked"
    if cnt(m.covered, (pr, OP)) - cnt(m.used, (pr, OP)) < 1: return f"claim of {pr} allowed without an unused approval by OP"
    return None

def explore(pol, depth):
    start = init(pol)
    seen = {start.key()}
    frontier = [(start, ())]
    n, saw_guarded_claim, saw_guard_denial = 0, False, False
    for _ in range(depth):
        nxt = []
        for m, path in frontier:
            for name, kind, m2, d, body in successors(pol, m):
                n += 1
                v = check(pol, m, name, kind, m2, d)
                if v: return n, v, path + (name,), saw_guarded_claim, saw_guard_denial
                if kind == "effect.claim":
                    pr = name.split()[2].split("#")[0]
                    if pr in m.need and d["allow"]: saw_guarded_claim = True
                    if pr in m.need and not d["allow"] and "guard" in d.get("reason", ""): saw_guard_denial = True
                if m2 is not None and m2.key() not in seen:
                    seen.add(m2.key()); nxt.append((m2, path + (name,)))
        frontier = nxt
    return n, None, None, saw_guarded_claim, saw_guard_denial

def mutant(old, new):
    import types
    assert SRC.count(old) == 1, old
    mod = types.ModuleType("policy_mut"); mod.__file__ = POL.__file__
    exec(compile(SRC.replace(old, new), "policy_mut", "exec"), mod.__dict__)
    return mod

MUTATIONS = {
    "relays-drop-taint": ('add = set(s["taint"].get(author, []))', 'add = set()'),
    "auto-taints-nothing-guarded-too": ('            if sess["steerMode"] != "auto": add.add(sess["guard"])', '            pass'),
    "guards-not-required": ('        if res == "none": return deny("E3 a guard hasn\'t approved")', '        pass'),
    "guard-role-ignored": ('    if role_of(state, g) not in READ_ROLES: return "none", None', '    pass'),
    "guard-key-ignored": ('        if st == "valid": return "ok", a\n    return "none", None\n\ndef live', '        return "ok", a\n    return "none", None\n\ndef live'),
    "tasks-drop-taint": ('        if tg: t["guards"] = tg', '        pass'),
    "self-clears": ('        if author == x: pass                                     # own input: no change', '        if author == x: s["taint"].pop(x, None)'),
    "guard-approval-reused": ('        for a in chosen: a["consumedBy"] = eid', '        pass'),
}

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--depth", type=int, default=9)
    ap.add_argument("--all-mutations", action="store_true")
    a = ap.parse_args()
    if a.all_mutations:
        missed = 0
        for name, (old, new) in MUTATIONS.items():
            try:
                n, v, path, _, _ = explore(mutant(old, new), a.depth)
            except Exception as ex:                   # decide allowed what fold can't apply: caught as a crash
                n, v, path = 0, f"fold crashed ({type(ex).__name__}) on an entry the mutant allowed", ()
            if v: print(f"caught  {name:32} VIOLATION after {n} transitions: {v}  [{' → '.join(path)}]")
            else: print(f"MISSED  {name:32} no violation in {n} transitions"); missed += 1
        print("all mutations caught" if not missed else f"{missed} mutations MISSED")
        sys.exit(1 if missed else 0)
    n, v, path, g, gd = explore(POL, a.depth)
    if v:
        print(f"VIOLATION after {n} transitions: {v}\n  path: {' → '.join(path)}"); sys.exit(1)
    if not (g and gd):
        print(f"VACUOUS: guarded claim seen={g}, guard denial seen={gd}"); sys.exit(1)
    print(f"[guard] {n} transitions to depth {a.depth}; GuardedClaimsNeedTheirHuman HOLDS (non-vacuous: influenced effects were claimed and refused)")

if __name__ == "__main__":
    main()

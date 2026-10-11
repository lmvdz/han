"""Bounded exhaustive check of P-15 guarded steering against the real policy.py.

The model keeps its own ground truth (a *ghost*) from the intent of P-15, not from Policy's code: BOT is held from the
first instruction to it (a steer into any of its sessions, or a task naming it) by anyone but itself and its operator OP
while it has a live guarded session, until OP itself releases it. Nobody stands in for OP. Properties:
- GuardedClaimsNeedTheirHuman: a claim Policy allows on an effect that BOT proposed while held, or that names BOT as
  executor while BOT is held, is covered by an unused approval of OP, while OP holds a role and its key is valid;
- HeldInstructNoOne: Policy never lets a held agent create a task, approve, or steer another's session;
- NeverLooser: Policy never lets an outsider steer a session that is dead by the spec's own reading;
- GuardRightsNeedRole: a guard without a role has no right on its agent's sessions.
Actors: owner O (who also steers), member P, a second operator OP2 with agent BOT2, executor X. BOT has a guarded
session S1 and an auto side-session S3. An approval rule, three sets of allowed modes, OP's removal and key revocation
vary. States are deduplicated on their content (Policy state minus history, plus the ghost), so depth 6 fits in memory.
Non-vacuity: some run claims a guarded effect, and some run is refused one for lack of OP's approval.

Usage: python ref/model_guard.py [--depth N] [--all-mutations]"""
import argparse, hashlib, json, os, sys, types
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import policy as POL

SRC = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "policy.py"), encoding="utf-8").read()
DEPTH = 6                      # full runs (run_all, §10); --quick runs depth 5 and the mutations
SP = "sg:a.example"
O, P, OP, OP2, X = "@o:a.example", "@p:a.example", "@op:b.example", "@op2:b.example", "@x:c.example"
BOT, BOT2 = "@bot:b.example", "@bot2:b.example"
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

S1, S2, S3 = H("s1"), H("s2"), H("s3")
SESS = {S1: ("s1", BOT, "guarded"), S2: ("s2", BOT2, "auto"), S3: ("s3", BOT, "auto")}
PROPS = {"pr1": (BOT, X), "pr2": (BOT2, X), "pr3": (P, BOT)}       # pr3: BOT as executor
CLAIMABLE = ("pr0", "pr1", "pr2", "pr3")

class M:
    __slots__ = ("labels", "state", "oprev", "held", "need", "covered", "used", "lastk")
    def __init__(self, **kw):
        for k in self.__slots__: setattr(self, k, kw[k])
    def key(self):
        st = {k: v for k, v in self.state.items() if k not in ("entries", "head", "roleHistory")}
        return (json.dumps(st, sort_keys=True), self.oprev, self.held, self.need, self.covered, self.used, self.lastk)
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

def init(pol):
    m = M(labels=(), state=None, oprev=False, held=False, need=frozenset(), covered=(), used=(), lastk=())
    m, d = apply(pol, m, E("C", "space.create", O, {"policy": policy()}), "C"); assert m, d
    for lab, a in (("m1", P), ("m2", X), ("m3", OP), ("m4", OP2), ("m5", BOT), ("m6", BOT2)):
        m, d = apply(pol, m, E(lab, "member.set", O, {"actor": a, "role": "member"}), lab); assert m, (lab, d)
    for sid, (lab, a, mode) in SESS.items():
        m, d = apply(pol, m, E(lab, "session.open", a, {"steerMode": mode}), lab); assert m, (lab, d)
    m, d = apply(pol, m, E("pr0", "effect.propose", BOT, {"executor": X, "action": "pay", "planDigest": DG, "repeatable": False}), "pr0")
    assert m, d                                                                               # proposed before any hold
    m, d = apply(pol, m, E("in0", "session.input", P, {"session": S1}), "in0"); assert m, d     # P's outside steer
    return m.with_(held=True)

def cnt(t, k): return dict(t).get(k, 0)
def bump(t, k, n=1): d = dict(t); d[k] = d.get(k, 0) + n; return tuple(sorted(d.items()))
def setk(t, k, v): d = dict(t); d[k] = v; return tuple(sorted(d.items()))

# ── the spec's intent, read independently of Policy's code ──
def mode_now(state, sid):
    """§6.6: owners can only tighten: a guarded session with guarded no longer allowed is dead; auto falls back to guarded."""
    lab, _, mode = SESS[sid]; allowed = state["policy"]["sessions"]["modes"]
    if mode in allowed: return mode
    return "guarded" if mode == "auto" and "guarded" in allowed else None

def alive(state, sid):
    _, actor, _ = SESS[sid]
    if not state["sessions"][sid]["open"] or state["roles"].get(actor) is None: return False
    mode = mode_now(state, sid)
    if mode is None: return False
    return mode == "auto" or state["roles"].get(OP if actor == BOT else OP2) is not None

def bot_guarded(state):
    return any(SESS[sid][1] == BOT and alive(state, sid) and mode_now(state, sid) == "guarded" for sid in SESS)

def instructs_bot(author, state):
    return author not in (BOT, OP) and bot_guarded(state)

def successors(pol, m):
    out = []
    def go(name, kind, author, body, after=None, label=None):
        lab = label or (name + "#" + str(sum(1 for x in m.labels if x == name)))     # commuting steps get equal ids
        m2, d = apply(pol, m, E(lab, kind, author, body), name)
        if m2 and after: m2 = after(m2)
        out.append((name, kind, author, body, m2, d))
    for author in (P, O, OP2, OP, BOT, BOT2):
        for sid, (lab, actor, _) in SESS.items():
            hold = actor == BOT and instructs_bot(author, m.state)
            go(f"{author} steers {lab}", "session.input", author, {"session": sid},
               (lambda n: n.with_(held=True)) if hold else None)
    for author in (OP, OP2, O):
        go(f"{author} releases BOT", "session.input", author, {"session": S1, "release": True},
           (lambda n: n.with_(held=False)) if author == OP else None)
    go("P tasks BOT", "task", P, {"to": [BOT]}, (lambda n: n.with_(held=True)) if instructs_bot(P, m.state) else None)
    go("BOT tasks BOT2", "task", BOT, {"to": [BOT2]})
    if m.state["sessions"][S1]["open"]: go("BOT closes s1", "session.close", BOT, {"session": S1})
    for pr, (author, executor) in PROPS.items():
        if H(pr) in m.state["effects"]: continue
        body = {"executor": executor, "action": "pay", "planDigest": DG, "repeatable": pr == "pr1"}
        guarded = author == BOT and m.held
        go(f"{author} proposes {pr}", "effect.propose", author, body,
           lambda n, pr=pr, g=guarded: n.with_(need=n.need | ({pr} if g else set())), label=pr)
    for pr in CLAIMABLE:
        pid = H(pr); eff = m.state["effects"].get(pid)
        if not eff or eff["phase"] in POL.FINAL or len(eff["claims"]) >= 2: continue
        for who in (OP, OP2, P, BOT, O):
            go(f"{who} approves {pr}", "effect.approve", who, {"proposal": pid, "planDigest": DG},
               lambda n, who=who, pr=pr: n.with_(covered=setk(n.covered, (pr, who), cnt(n.used, (pr, who)) + 1)))  # supersedes
        a = len(eff["claims"]) + 1
        executor = eff["executor"]
        needs_op = pr in m.need or (executor == BOT and m.held)
        go(f"{executor} claims {pr}#{a}", "effect.claim", executor, {"proposal": pid, "attempt": a, "instance": "k1", "window": 30},
           (lambda n, pr=pr: n.with_(used=bump(n.used, (pr, OP)), lastk=setk(n.lastk, pr, OP))) if needs_op else None)
        if eff["phase"] == "claimed":
            c = eff["claims"][-1]
            lk = dict(m.lastk).get(pr)
            go(f"{executor} not started {pr}", "effect.outcome", executor, {"claim": c["id"], "result": "failed", "started": "no"},
               (lambda n, pr=pr, lk=lk: n.with_(used=bump(n.used, (pr, lk), -1), lastk=setk(n.lastk, pr, None))) if lk else None)
            go(f"{executor} failed {pr}", "effect.outcome", executor, {"claim": c["id"], "result": "failed", "started": "yes"},
               (lambda n, pr=pr: n.with_(lastk=setk(n.lastk, pr, None))) if lk else None)
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
    if m.state["roles"].get(OP) is None and any(any(pol.session_access(m.state, OP, sid).values()) for sid in (S1, S3)):
        return "a guard without a role keeps rights on its agent's session"
    return None

def check(m, name, kind, author, body, d):
    if not d["allow"]: return None
    if kind == "session.input" and not body.get("release") and author != SESS[body["session"]][1] and not alive(m.state, body["session"]):
        return f"outside steer allowed into a dead session {SESS[body['session']][0]}"
    if author == BOT and m.held:
        if kind in ("task", "effect.approve"): return f"held BOT allowed {kind}"
        if kind == "session.input" and SESS[body["session"]][1] != BOT: return "held BOT allowed to steer another's session"
    if kind == "effect.claim":
        pr = name.split()[2].split("#")[0]
        if pr in m.need or (author == BOT and m.held):
            if m.state["roles"].get(OP) is None: return f"claim of {pr} allowed while OP, whose approval it needs, holds no role"
            if m.oprev: return f"claim of {pr} allowed after OP's key was revoked"
            if cnt(m.covered, (pr, OP)) - cnt(m.used, (pr, OP)) < 1: return f"claim of {pr} allowed without an unused approval by OP"
    return None

def explore(pol, depth):
    start = init(pol)
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
                    pr = name.split()[2].split("#")[0]
                    if pr in m.need or (author == BOT and m.held):
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
    "held-executors-unguarded": ('    h = state["held"].get(eff["executor"])\n    if h and h not in g: g.append(h)', '    pass'),
    "tasks-never-hold": ('        for x in b["to"]: hold_on_instruction(s, x, author)\n', ''),
    "hold-per-session-only": ('    g = agent_guard(s, x)\n', '    g = None\n'),
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
            if v: print(f"caught  {name:34} after {n} transitions: {v}  [{' → '.join(path)}]", flush=True)
            else: print(f"MISSED  {name:34} no violation in {n} transitions", flush=True); missed += 1
        print(f"{len(MUTATIONS) - missed}/{len(MUTATIONS)} guard mutations caught by a property")
        sys.exit(1 if missed else 0)
    n, v, path, g, gd = explore(POL, a.depth)
    if v: print(f"VIOLATION after {n} transitions: {v}\n  path: {' → '.join(path)}"); sys.exit(1)
    if not (g and gd): print(f"VACUOUS: guarded claim seen={g}, guard denial seen={gd}"); sys.exit(1)
    print(f"[guard] {n} transitions to depth {a.depth}; GuardedClaimsNeedTheirHuman, HeldInstructNoOne, NeverLooser and GuardRightsNeedRole HOLD (non-vacuous)")

if __name__ == "__main__":
    main()

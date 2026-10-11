"""Bounded exhaustive check of P-15 guarded steering against the real policy.py.

The model keeps its own ground truth (a *ghost*), independent of Policy: an agent is held by its operator from the first
outside steer into any of its sessions while one of its live sessions is guarded (outside = neither the agent nor its
operator) until that operator, or an owner once the operator is gone, explicitly releases it. Properties:
- GuardedClaimsNeedTheirHuman: every claim Policy allows on an effect proposed by a held agent is covered by an approval
  not used by an earlier claim, of the holding operator while it holds a role (and its key is valid), else of an owner;
- HeldInstructNoOne: Policy never allows a held agent to create a task, approve, or put input into another's session;
- NeverLooser: no outside steer is allowed into a session whose mode the space no longer allows at least as cautiously;
- GuardRightsNeedRole: a guard without a role has no watch, read or steer right on its agent's session.
Two agents with different operators, an outsider human, an approval rule and the allowed modes all vary.
Non-vacuity: some run claims a held agent's effect, and some run is refused a claim for lack of the guard's approval.

Usage: python ref/model_guard.py [--depth N] [--all-mutations]"""
import argparse, hashlib, os, sys, types
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import policy as POL

SRC = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "policy.py"), encoding="utf-8").read()
DEPTH = 5                      # full runs (run_all, §10); --quick runs depth 4. Runs start with BOT already held
SP = "sg:a.example"
O, P, OP, OP2, X = "@o:a.example", "@p:a.example", "@op:b.example", "@op2:b.example", "@x:c.example"
BOT, BOT2 = "@bot:b.example", "@bot2:b.example"
GUARD = {BOT: OP, BOT2: OP2}
T0 = "2026-10-10T12:00:00Z"
DG = "d" * 64

def H(label): return hashlib.sha256(label.encode()).hexdigest()
def human(rev=False): return {"actorKind": "human", "operatorConfirmed": False, "keys": {"k1": {"publicKey": "pk", "status": "revoked" if rev else "valid"}}}
def agent(op): return {"actorKind": "agent", "operator": op, "operatorConfirmed": True, "keys": {"k1": {"publicKey": "pk", "status": "valid"}}}
XS = {"actorKind": "service", "operatorConfirmed": True, "keys": {"k1": {"publicKey": "pk", "status": "valid"}}}

MODESETS = (("auto", "guarded"), ("guarded",), ("auto",))

def policy(rule=False, modes=MODESETS[0]):
    return {"visibility": "members", "join": "invite", "membersMayInvite": False, "allowAgents": "any",
            "approvals": [{"action": "pay", "approvers": ["member"]}] if rule else [], "separateApprover": True, "budget": None,
            "maxDelegationDepth": 4, "minDurability": "local", "launchWindow": 30, "extensionsMinRole": "member",
            "sessions": {"watch": "member", "read": "member", "steer": "member",
                         "modes": list(modes)}, "servers": {"deny": []}}

def E(label, kind, author, body):
    return {"id": H(label), "v": 1, "space": SP, "kind": kind, "author": author, "keyId": "k1", "body": body,
            "contentHash": "", "refs": [], "seen": 0, "nonce": label, "time": T0}

S1, S2, S3 = H("s1"), H("s2"), H("s3")
SESS_OF = {S1: BOT, S2: BOT2, S3: BOT}      # S3: BOT's auto side-session
SESS_NAME = {S1: "s1", S2: "s2", S3: "s3"}
PROPS = {"pr1": BOT, "pr2": BOT2}
CLAIMABLE = ("pr0", "pr1", "pr2")
CAUTION = ("auto", "guarded")

def ghost_mode(state, sess_id):
    """The model's own reading of P-15's effective mode: never looser than opened; None = dead."""
    mode = state["sessions"][sess_id]["steerMode"]
    return next((x for x in CAUTION[CAUTION.index(mode):] if x in state["policy"]["sessions"]["modes"]), None)

class M:
    __slots__ = ("labels", "state", "oprev", "held", "need", "covered", "used", "lastk")
    def __init__(self, **kw):
        for k in self.__slots__: setattr(self, k, kw[k])
    def key(self): return (self.labels, self.oprev, self.held, self.need, self.covered, self.used, self.lastk)
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
    m = M(labels=(), state=None, oprev=False, held=(), need=frozenset(), covered=(), used=(), lastk=())
    m, d = apply(pol, m, E("C", "space.create", O, {"policy": policy()}), "C"); assert m, d
    for lab, a in (("m1", P), ("m2", X), ("m3", OP), ("m4", OP2), ("m5", BOT), ("m6", BOT2)):
        m, d = apply(pol, m, E(lab, "member.set", O, {"actor": a, "role": "member"}), lab); assert m, (lab, d)
    for lab, a, mode in (("s1", BOT, "guarded"), ("s2", BOT2, "auto"), ("s3", BOT, "auto")):
        m, d = apply(pol, m, E(lab, "session.open", a, {"steerMode": mode}), lab); assert m, (lab, d)
    m, d = apply(pol, m, E("pr0", "effect.propose", BOT, {"executor": X, "action": "pay", "planDigest": DG, "repeatable": False}), "pr0")
    assert m, d                                                                               # proposed before any hold
    m, d = apply(pol, m, E("in0", "session.input", P, {"session": S1}), "in0"); assert m, d     # P's outside steer: BOT is held
    return m.with_(held=((BOT, OP),))

def cnt(t, k): return dict(t).get(k, 0)
def setk(t, k, v): d = dict(t); d[k] = v; return tuple(sorted(d.items()))
def bump(t, k, n=1): d = dict(t); d[k] = d.get(k, 0) + n; return tuple(sorted(d.items()))

def ghost_live(state, sid):
    se = state["sessions"][sid]; mode = ghost_mode(state, sid)
    if not se["open"] or state["roles"].get(SESS_OF[sid]) is None or mode is None: return False
    return mode == "auto" or state["roles"].get(GUARD[SESS_OF[sid]]) is not None

def ghost_guarded(state, agent_):
    return any(SESS_OF[sid] == agent_ and ghost_live(state, sid) and ghost_mode(state, sid) == "guarded" for sid in SESS_OF)

def ghost_after_input(m, author, sess_id, release):
    """The independent ground truth for holds."""
    agent_ = SESS_OF[sess_id]; held = dict(m.held)
    if release:
        g = held.get(agent_)
        if g == author or (author == O and g and m.state["roles"].get(g) is None): held.pop(agent_, None)
    elif author not in (agent_, GUARD[agent_]) and ghost_guarded(m.state, agent_):
        held[agent_] = GUARD[agent_]
    return tuple(sorted(held.items()))

def approver_for(m, g):
    return g if m.state["roles"].get(g) else O       # owners stand in for a departed guard

def successors(pol, m):
    out = []
    def go(name, kind, author, body, after=None, label=None):
        e = E(label or (name + "#" + str(len(m.labels))), kind, author, body)
        m2, d = apply(pol, m, e, name)
        if m2 and after: m2 = after(m2)
        out.append((name, kind, author, body, m2, d))
    for author in (P, OP2, OP, BOT, BOT2):
        for sid in (S1, S2, S3):
            go(f"{author} steers {SESS_NAME[sid]}", "session.input", author, {"session": sid},       # names must differ per session: states are deduplicated by history
               lambda n, a=author, s=sid: n.with_(held=ghost_after_input(m, a, s, False)))
    for author in (OP, OP2, O):
        go(f"{author} releases BOT", "session.input", author, {"session": S1, "release": True},
           lambda n, a=author: n.with_(held=ghost_after_input(m, a, S1, True)))
    go("BOT tasks BOT2", "task", BOT, {"to": [BOT2]})
    if m.state["sessions"][S1]["open"]: go("BOT closes S1", "session.close", BOT, {"session": S1})
    for pr, author in PROPS.items():
        if H(pr) in m.state["effects"]: continue
        body = {"executor": X, "action": "pay", "planDigest": DG, "repeatable": pr == "pr1"}
        held_now = dict(m.held).get(author)
        go(f"{author} proposes {pr}", "effect.propose", author, body,
           lambda n, pr=pr, g=held_now: n.with_(need=n.need | ({(pr, g)} if g else set())), label=pr)
    for pr in CLAIMABLE:
        pid = H(pr); eff = m.state["effects"].get(pid)
        if not eff or eff["phase"] in POL.FINAL or len(eff["claims"]) >= 2: continue
        for who in (OP, OP2, P, BOT, O):
            if cnt(m.covered, (pr, who)) - cnt(m.used, (pr, who)) >= 1: continue
            go(f"{who} approves {pr}", "effect.approve", who, {"proposal": pid, "planDigest": DG},
               lambda n, who=who, pr=pr: n.with_(covered=bump(n.covered, (pr, who))))
        a = len(eff["claims"]) + 1
        g = next((g for (p_, g) in m.need if p_ == pr), None)
        k = approver_for(m, g) if g else None
        go(f"X claims {pr}#{a}", "effect.claim", X, {"proposal": pid, "attempt": a, "instance": "k1", "window": 30},
           (lambda n, pr=pr, k=k: n.with_(used=bump(n.used, (pr, k)), lastk=setk(n.lastk, pr, k))) if g else None)
        if eff["phase"] == "claimed":
            c = eff["claims"][-1]
            lk = dict(m.lastk).get(pr)
            go(f"X not started {pr}", "effect.outcome", X, {"claim": c["id"], "result": "failed", "started": "no"},
               (lambda n, pr=pr, lk=lk: n.with_(used=bump(n.used, (pr, lk), -1))) if (g and lk) else None)
            go(f"X failed {pr}", "effect.outcome", X, {"claim": c["id"], "result": "failed", "started": "yes"})
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
    held = dict(m.held)
    if kind == "session.input" and not body.get("release") and author != SESS_OF[body["session"]] \
            and not ghost_live(m.state, body["session"]):
        return f"outside steer allowed into {SESS_OF[body['session']]}'s dead session (no mode as cautious as its own, or a guarded session whose guard is gone)"
    if author in held:
        if kind in ("task", "effect.approve"): return f"held {author} allowed {kind}"
        if kind == "session.input" and SESS_OF.get(body["session"]) != author: return f"held {author} allowed to steer another's session"
    if kind == "effect.claim":
        pr = name.split()[2].split("#")[0]
        g = next((g for (p_, g) in m.need if p_ == pr), None)
        if g:
            k = approver_for(m, g)
            if k == OP and m.oprev: return f"claim of {pr} allowed after its guard's key was revoked"
            if cnt(m.covered, (pr, k)) - cnt(m.used, (pr, k)) < 1: return f"claim of {pr} allowed without an unused approval by {k}"
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
                if kind == "effect.claim" and any(p_ == name.split()[2].split("#")[0] for (p_, _) in m.need):
                    saw_claim |= d["allow"]; saw_denial |= (not d["allow"] and "guard" in d.get("reason", ""))
                if m2 is not None and m2.key() not in seen:
                    v = check_state(pol, m2)
                    if v: return n, v, path + (name,), saw_claim, saw_denial
                    seen.add(m2.key()); nxt.append((m2, path + (name,)))
        frontier = nxt
    return n, None, None, saw_claim, saw_denial

def mutant(old, new):
    assert SRC.count(old) == 1, old
    mod = types.ModuleType("policy_mut"); mod.__file__ = POL.__file__
    exec(compile(SRC.replace(old, new), "policy_mut", "exec"), mod.__dict__)
    return mod

MUTATIONS = {
    "held-gate-off": ('    if author in state["held"] and not held_may(state, entry):', '    if False:'),
    "held-may-relay": ('        return bool(sess) and sess["actor"] == entry["author"]', '        return True'),
    "held-may-delegate": ('HELD_KINDS = {"message",', 'HELD_KINDS = {"task", "message",'),
    "held-may-approve": ('HELD_KINDS = {"message",', 'HELD_KINDS = {"effect.approve", "message",'),
    "held-proposals-unguarded": ('        if author in s["held"]: eff["guards"] = [s["held"][author]]; eff["phase"] = "awaiting-approval"', '        pass'),
    "guards-not-required": ('        if res == "none": return deny("E3 a guard hasn\'t approved")', '        pass'),
    "departed-guard-still-approves": ('    return actor == g and role_of(state, g) in READ_ROLES or (', '    return actor == g or ('),
    "owners-stand-in-over-live-guard": (' or (role_of(state, g) not in READ_ROLES and role_of(state, actor) == "owner")', ' or role_of(state, actor) == "owner"'),
    "hold-per-session-only": ('and "guard" in sess and agent_guarded(s, x):', 'and "guard" in sess and effective_mode(s, sess) == "guarded":'),
    "guarded-without-guard-stays-live": (' or role_of(state, sess.get("guard")) in READ_ROLES))', ' or "guard" in sess))'),
    "guard-key-ignored": ('        if st == "valid": return "ok", a\n    return "none", None\n\ndef live', '        return "ok", a\n    return "none", None\n\ndef live'),
    "any-guard-input-releases": ('        if b.get("release"): s["held"].pop(x, None)', '        if b.get("release") or author == sess.get("guard"): s["held"].pop(x, None)'),
    "anyone-releases": ('        return deny("only the guard holding this agent releases it")', '        return ALLOW'),
    "modes-ignored-for-open-sessions": ('    return next((m for m in CAUTION[CAUTION.index(sess["steerMode"]):] if m in allowed), None)', '    return sess["steerMode"]'),
    "owners-may-loosen-modes": ('    return next((m for m in CAUTION[CAUTION.index(sess["steerMode"]):] if m in allowed), None)', '    return next((m for m in CAUTION if m in allowed), None)'),
    "banned-guard-keeps-rights": ('    if actor == s["actor"] or (actor == s.get("guard") and role_of(state, actor) in READ_ROLES):', '    if actor == s["actor"] or actor == s.get("guard"):'),
    "owners-release-over-live-guard": ('        if role == "owner" and role_of(state, g) not in READ_ROLES: return ALLOW     # the guard is gone: owners recover', '        if role == "owner": return ALLOW'),
    "guard-approval-reused": ('        for a in chosen: a["consumedBy"] = eid', '        pass'),
}

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--depth", type=int, default=DEPTH)
    ap.add_argument("--all-mutations", action="store_true")
    a = ap.parse_args()
    if a.all_mutations:
        missed = 0
        for name, (old, new) in MUTATIONS.items():
            n, v, path, _, _ = explore(mutant(old, new), a.depth)
            if v: print(f"caught  {name:34} after {n} transitions: {v}  [{' → '.join(path)}]")
            else: print(f"MISSED  {name:34} no violation in {n} transitions"); missed += 1
        print(f"{len(MUTATIONS) - missed}/{len(MUTATIONS)} guard mutations caught by a property")
        sys.exit(1 if missed else 0)
    n, v, path, g, gd = explore(POL, a.depth)
    if v: print(f"VIOLATION after {n} transitions: {v}\n  path: {' → '.join(path)}"); sys.exit(1)
    if not (g and gd): print(f"VACUOUS: held claim seen={g}, guard denial seen={gd}"); sys.exit(1)
    print(f"[guard] {n} transitions to depth {a.depth}; GuardedClaimsNeedTheirHuman, HeldInstructNoOne, NeverLooser and GuardRightsNeedRole HOLD (non-vacuous)")

if __name__ == "__main__":
    main()

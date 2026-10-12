"""Bounded exhaustive check of P-15 guarded steering against the real policy.py.

The ghost is derived from the invariant sentence of P-15, not from Policy's rules:

    While an agent is guarded, any instruction to it from anyone but its guard holds it; while it is held, nothing it
    proposes or is named executor of runs without its guard's own approval; only its guard ends a hold.

"Guarded" = the agent's own sticky choice (it opened a guarded session; only its guard ends that, with an `agent.release`
that unguards, never the agent itself and never by closing), or forced by owners' modes (one of its live sessions is
effectively guarded). Under `forceGuarded` every non-human target of an instruction is guarded for that instruction,
for the operator in the profile pinned for it (round 23, F1): no session needed, no snapshot; an instruction to such a
target whose operator holds no role here is denied. "Instruction" = a steer into any of its sessions, a task naming it, a
proposal naming it as executor. Three agents, BOT (operator OP), BOT2 (operator OP2) and the session-less BOT3 (operator
OP); a service X (also with an operator) is forced when tasked, but not when named as executor. Owner O and member P
instruct; OP's role and key vary; the approval rule, allowed modes and forceGuarded vary. Closed sessions leave the
state (F3b), so steering or closing one is `invalid`.

Properties: GuardedClaimsNeedTheirHuman, HeldInstructNoOne (no task, approval or steer of another's session by a held
agent), NeverLooser (no outside steer into a dead session), GuardRightsNeedRole, OneOpenApprovalPerAuthor, and
ExactHolds (Policy holds exactly the agents the invariant holds: never fewer, never more); NeverLooser also covers
unguarding (allowed exactly for the agent's guard while it holds a role, never for the agent itself).
States are deduplicated on content: a 32-byte SHA-256 of (Policy state minus history, plus the ghost) is stored per state,
not a copy. A memory cap (default 6 GB of resident memory) ends the run cleanly: one line "NOT COMPLETED ..." and exit 3.

Usage: python ref/model_guard.py [--depth N] [--all-mutations] [--max-mem-gb G]"""
import argparse, hashlib, json, os, pickle, sys, types, zlib
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import policy as POL

SRC = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "policy.py"), encoding="utf-8").read()
DEPTH = 5                      # full runs (run_all, §10); --quick runs depth 4; mutations run at depth 4
MAX_MEM_GB = 6.0               # abort cleanly above this resident size (F7)
SP = "sg:a.example"
O, P, OP, OP2, X = "@o:a.example", "@p:a.example", "@op:b.example", "@op2:b.example", "@x:c.example"
BOT, BOT2, BOT3 = "@bot:b.example", "@bot2:b.example", "@bot3:b.example"
GUARD = {BOT: OP, BOT2: OP2, BOT3: OP}
FORCEMAP = dict(GUARD, **{X: OP})              # whom `forceGuarded` guards, for whom: the service X too, except as executor
T0 = "2026-10-10T12:00:00Z"
DG = "d" * 64
MODESETS = (("auto", "guarded"), ("guarded",), ("auto",))

def H(label): return hashlib.sha256(label.encode()).hexdigest()
def human(rev=False): return {"actorKind": "human", "operatorConfirmed": False, "keys": {"k1": {"publicKey": "pk", "status": "revoked" if rev else "valid"}}}
def agent(op): return {"actorKind": "agent", "operator": op, "operatorConfirmed": True, "keys": {"k1": {"publicKey": "pk", "status": "valid"}}}
XS = {"actorKind": "service", "operator": OP, "operatorConfirmed": True, "keys": {"k1": {"publicKey": "pk", "status": "valid"}}}

def policy(rule=False, modes=MODESETS[0], force=False):
    return {"visibility": "members", "join": "invite", "membersMayInvite": False, "allowAgents": "any",
            "approvals": [{"action": "pay", "approvers": ["member"]}] if rule else [], "separateApprover": True, "budget": None,
            "maxDelegationDepth": 4, "minDurability": "local", "launchWindow": 30, "extensionsMinRole": "member",
            "sessions": {"watch": "member", "read": "member", "steer": "member", "modes": list(modes), "forceGuarded": force}, "servers": {"deny": []}}

def E(label, kind, author, body):
    return {"id": H(label), "v": 1, "space": SP, "kind": kind, "author": author, "keyId": "k1", "body": body,
            "contentHash": "", "refs": [], "seen": 0, "nonce": label, "time": T0}

S1, S2, S3, S4, S5 = H("s1"), H("s2"), H("s3"), H("s4"), H("s5")
SESS = {S1: ("s1", BOT, "guarded"), S2: ("s2", BOT2, "auto"), S3: ("s3", BOT, "auto"), S4: ("s4", BOT2, "guarded"), S5: ("s5", BOT3, "auto")}
OPENED_AT_START = (S1, S2, S3)                                   # S4: BOT2 may open it later (becoming guarded); S5: session-less BOT3 may open an auto one
PROPS = {"pr1": (BOT, X), "pr2": (BOT2, X), "pr3": (P, BOT), "pr4": (BOT, BOT2), "pr5": (OP, BOT), "pr6": (P, BOT3), "pr7": (P, BOT2)}   # pr5: its own guard's; pr7: aimed at BOT2 before it is guarded (a settled hold records the guard)
CLAIMABLE = ("pr0", "pr1", "pr2", "pr3", "pr4", "pr5", "pr6", "pr7")
TASKS = {"P tasks BOT": (P, [BOT]), "P tasks BOT2+BOT": (P, [BOT2, BOT]), "BOT tasks BOT2": (BOT, [BOT2]), "BOT2 tasks BOT": (BOT2, [BOT]),
         "P tasks BOT3": (P, [BOT3]), "P tasks X": (P, [X])}

def fz(d): return tuple(sorted(d.items()))

class M:
    __slots__ = ("labels", "state", "oprev", "gd", "held", "need", "covered", "used", "lastk", "cl", "closed")
    def __init__(self, **kw):
        for k in self.__slots__: setattr(self, k, kw[k])
    def key(self):
        st = {k: v for k, v in self.state.items() if k not in ("entries", "head", "roleHistory")}
        body = json.dumps([st, self.oprev, self.gd, self.held, sorted(self.need), self.covered, self.used, self.lastk, self.cl, sorted(self.closed)], sort_keys=True)
        return hashlib.sha256(body.encode()).digest()
    def with_(self, **kw):
        d = {k: getattr(self, k) for k in self.__slots__}; d.update(kw); return M(**d)

def ctx(m):
    return {"time": T0, "profiles": {O: human(), P: human(), OP: human(m.oprev), OP2: human(), X: XS, BOT: agent(OP), BOT2: agent(OP2), BOT3: agent(OP)}}

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

def alive(m, sid):
    """Is session `sid` live? Closed sessions are tracked by the ghost itself (m.closed), not read back from Policy's state."""
    state = m.state
    _, actor, _ = SESS[sid]
    se = state["sessions"].get(sid)
    if not se or sid in m.closed or state["roles"].get(actor) is None: return False
    mode = mode_now(state, sid)
    if mode is None: return False
    return mode == "auto" or state["roles"].get(GUARD[actor]) is not None

def guarded(m, a):
    """Guarded by its own choice or by owners' modes (not by forceGuarded, which acts per instruction)."""
    return dict(m.gd).get(a, False) or any(SESS[sid][1] == a and alive(m, sid) and mode_now(m.state, sid) == "guarded" for sid in SESS)

def forced(m, a, slot):
    """forceGuarded makes `a` a guarded target of this instruction: every non-human, except a service named as executor."""
    return m.state["policy"]["sessions"].get("forceGuarded", False) and a in FORCEMAP and not (slot == "executor" and a == X)

def guard_for(m, a, slot):
    """The guard `a` has for an instruction now, or None."""
    if a in GUARD and guarded(m, a): return GUARD[a]
    return FORCEMAP[a] if forced(m, a, slot) else None

def instruct(m, author, a, slot):
    """The ghost's held map after `author` instructs `a` (slot: session, task or executor)."""
    held = dict(m.held)
    g = guard_for(m, a, slot)
    if g and author not in (a, g) and a not in held: held[a] = g
    return fz(held)

def instructed(kind, author, body):
    """The (target, slot) pairs an entry instructs."""
    if kind == "task": return [(a, "task") for a in body["to"]]
    if kind == "effect.propose": return [(body["executor"], "executor")]
    if kind == "session.input" and body["session"] in SESS: return [(SESS[body["session"]][1], "session")]
    return []

def forced_refusal(m, kind, author, body):
    """What the invariant says about forcing: an instruction to a forced target whose operator holds no role is refused."""
    return any(forced(m, a, slot) and a != author and m.state["roles"].get(FORCEMAP[a]) is None for a, slot in instructed(kind, author, body))

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
    m = M(labels=(), state=None, oprev=False, gd=(), held=(), need=frozenset(), covered=(), used=(), lastk=(), cl=(), closed=frozenset())
    m, d = apply(pol, m, E("C", "space.create", O, {"policy": policy()}), "C"); assert m, d
    for lab, a in (("m1", P), ("m2", X), ("m3", OP), ("m4", OP2), ("m5", BOT), ("m6", BOT2), ("m7", BOT3)):
        m, d = apply(pol, m, E(lab, "member.set", O, {"actor": a, "role": "member"}), lab); assert m, (lab, d)
    for sid in OPENED_AT_START:
        lab, a, mode = SESS[sid]
        m, d = apply(pol, m, E(lab, "session.open", a, {"steerMode": mode}), lab); assert m, (lab, d)
    m = m.with_(gd=fz({BOT: True}))
    m, d = apply(pol, m, E("pr0", "effect.propose", BOT, {"executor": X, "action": "pay", "planDigest": DG, "repeatable": True}), "pr0")
    assert m, d                                                                               # proposed before any hold
    m2, d = apply(pol, m, E("in0", "session.input", P, {"session": S1}), "in0"); assert m2, d     # P's outside steer
    m2 = settle(m2.with_(held=instruct(m, P, BOT, "session")))
    m3, d = apply(pol, m2, E("ap0", "effect.approve", OP, {"proposal": H("pr0"), "planDigest": DG}), "ap0"); assert m3, d   # its guard approves once
    return m3.with_(covered=setk(m3.covered, ("pr0", OP), 1))

def successors(pol, m):
    out = []
    def go(name, kind, author, body, after=None, label=None):
        lab = label or (name + "#" + str(sum(1 for x in m.labels if x == name)))
        e = E(lab, kind, author, body)
        if kind == "agent.release": e["seen"] = m.state["head"]      # the guard reviewed everything so far
        m2, d = apply(pol, m, e, name)
        if m2 and after: m2 = after(m2)
        if m2: m2 = settle(m2)
        out.append((name, kind, author, body, m2, d))
    for author in (P, O, OP2, OP, BOT, BOT2):
        for sid, (lab, actor, _) in SESS.items():
            if sid not in m.state["sessions"] and sid not in OPENED_AT_START: continue        # a closed one is steered too: `invalid`
            go(f"{author} steers {lab}", "session.input", author, {"session": sid},
               lambda n, a=author, x=actor: n.with_(held=instruct(m, a, x, "session")))
    for author in (OP, OP2, O, BOT):
        for a in (BOT, BOT2, BOT3, X):                           # F2: a release names the agent; no session is needed
            go(f"{author} releases {a[1:].split(':')[0]}", "agent.release", author, {"agent": a},
               (lambda n, a=a: n.with_(held=fz({k: v for k, v in m.held if k != a}), cl=setk(m.cl, a, m.state["head"])))
               if dict(m.held).get(a) == author else None)
    for author in (OP, OP2, BOT):                                # E1: only the guard unguards, with a release
        for a in (BOT, BOT2, BOT3):
            go(f"{author} unguards {a[1:].split(':')[0]}", "agent.release", author, {"agent": a, "unguard": True},
               lambda n, a=a: n.with_(held=fz({k: v for k, v in m.held if k != a}), gd=setk(m.gd, a, False), cl=setk(m.cl, a, m.state["head"])))
    for name, (author, to) in TASKS.items():
        def after(n, author=author, to=to):
            mm = m
            for a in to: mm = mm.with_(held=instruct(mm, author, a, "task"))
            return n.with_(held=mm.held)
        go(name, "task", author, {"to": to}, after)
    if S4 not in m.state["sessions"]:
        def opened(n):
            return n.with_(gd=setk(m.gd, BOT2, True))          # becoming guarded; settle() applies open outside instructions
        go("BOT2 opens s4 guarded", "session.open", BOT2, {"steerMode": "guarded"}, opened, label="s4")
    if S5 not in m.state["sessions"]:
        go("BOT3 opens s5", "session.open", BOT3, {"steerMode": "auto"}, label="s5")
    for author in (BOT, O):
        go(f"{author} closes s1", "session.close", author, {"session": S1}, lambda n: n.with_(closed=n.closed | {S1}))      # E1: closing never unguards, whoever closes
    for pr, (author, executor) in PROPS.items():
        if H(pr) in m.state["effects"]: continue
        body = {"executor": executor, "action": "pay", "planDigest": DG, "repeatable": pr == "pr1"}
        g = dict(m.held).get(author)
        go(f"{author} proposes {pr}", "effect.propose", author, body,
           lambda n, pr=pr, g=g, a=author, x=executor: n.with_(need=n.need | ({(pr, g)} if g else set()) | ({(pr, gx)} if (gx := guard_for(m, x, "executor")) and a not in (x, gx) else set()),
                                                                held=instruct(m, a, x, "executor")), label=pr)
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
    rule, modes, force = bool(pol_now["approvals"]), tuple(pol_now["sessions"]["modes"]), pol_now["sessions"]["forceGuarded"]
    go("O toggles approval rule", "policy.set", O, {"policy": policy(not rule, modes, force)})
    go("O toggles forceGuarded", "policy.set", O, {"policy": policy(rule, modes, not force)})
    for ms_ in MODESETS:
        if ms_ != modes: go(f"O sets modes {'/'.join(ms_)}", "policy.set", O, {"policy": policy(rule, ms_, force)})
    return out

def check_state(pol, m):
    if dict(m.held) != m.state["held"]:
        return f"ExactHolds: Policy holds {m.state['held']} but the invariant holds {dict(m.held)}"
    if m.state["roles"].get(OP) is None and any(any(pol.session_access(m.state, OP, sid).values()) for sid in (S1, S3)):
        return "a guard without a role keeps rights on its agent's session"
    for pid, eff in m.state["effects"].items():
        authors = [a["author"] for a in eff["approvals"] if not a.get("consumedBy")]
        if len(authors) != len(set(authors)): return "an author has two open approvals on one proposal"
    return None

def check(m, name, kind, author, body, d):
    if kind == "agent.release" and body.get("unguard"):
        a = body["agent"]
        should = author == GUARD[a] and m.state["roles"].get(author) is not None and not (author == OP and m.oprev) and (guarded(m, a) or a in dict(m.held))
        if d["allow"] and not should: return f"{author} unguarded {a}, which only its guard {GUARD[a]} may, while guarded or held"
        if should and not d["allow"]: return f"guard {author} refused when unguarding {a}: {d.get('reason')}"
    refuse = forced_refusal(m, kind, author, body)
    if d["allow"] and refuse: return f"{kind} by {author} allowed to a forced target whose operator holds no role"
    if not d["allow"] and "forced space" in d.get("reason", "") and not refuse: return f"{kind} by {author} refused for forcing, though no forced target lacks an operator in the space"
    if kind in ("session.input", "session.close") and d["allow"] and (body["session"] not in m.state["sessions"] or body["session"] in m.closed):
        return f"{kind} allowed on a session that is closed"
    if not d["allow"]: return None
    if kind == "session.input" and author != SESS[body["session"]][1] and not alive(m, body["session"]):
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
    try:
        start = init(pol)
    except AssertionError as ex:                             # Policy disagrees with the invariant already in the setup
        return 0, f"setup step refused or diverged: {ex}", ("init",), False, False
    seen = {start.key()}; frontier = [(pack(start), ())]                # the frontier is kept compressed; `seen` holds 32-byte hashes
    n, saw_claim, saw_denial, tick = 0, False, False, 0
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
                    seen.add(k)
                    if level + 1 < depth: nxt.append((pack(m2), path + (name,)))      # the last level is only checked, never expanded
        frontier = nxt
        if progress: print(f"  depth {level + 1}: {n} transitions, {len(seen)} states, frontier {len(frontier)}, rss {rss_bytes() / 2**20:.0f} MB", flush=True)
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
    "held-may-relay": ('        return not sess or sess["actor"] == entry["author"]', '        return True'),
    "held-may-delegate": ('HELD_KINDS = {"message",', 'HELD_KINDS = {"task", "message",'),
    "held-may-approve": ('HELD_KINDS = {"message",', 'HELD_KINDS = {"effect.approve", "message",'),
    "held-proposals-unguarded": ('        if author in s["held"]: eff["guards"] = [s["held"][author]]; eff["phase"] = "awaiting-approval"', '        pass'),
    "held-proposers-older-proposals-free": ('    for who in (eff["proposer"], eff["executor"]):', '    for who in (eff["executor"],):'),
    "held-executors-unguarded": ('    for who in (eff["proposer"], eff["executor"]):', '    for who in (eff["proposer"],):'),
    "no-settling": ('    settle_holds(s)\n', ''),
    "settled-holds-ignore-tasks": ('        if (tasks or effs) and x not in s["held"]: s["held"][x] = g', '        if effs and x not in s["held"]: s["held"][x] = g'),
    "settled-holds-ignore-effects": ('        if (tasks or effs) and x not in s["held"]: s["held"][x] = g', '        if tasks and x not in s["held"]: s["held"][x] = g'),
    "settled-tasks-first-assignee-only": ('        tasks = [t for t in s["tasks"].values() if t["open"] and x in t["to"]', '        tasks = [t for t in s["tasks"].values() if t["open"] and x in t["to"][:1]'),
    "executor-guard-not-recorded": [('                e.setdefault("guards", []).append(g)', '                pass'),
                                    ('                if e["phase"] == "ready": e["phase"] = "awaiting-approval"', '                pass')],
    "release-does-not-clear": ('s["held"].pop(x, None); s["cleared"][x] = e["seen"]', 's["held"].pop(x, None)'),
    "owner-forced-guarding-ignored": ('    for se in state["sessions"].values():\n        if se["actor"] == x and "guard" in se and live(state, se) and effective_mode(state, se) == "guarded": return se["guard"]\n    return None', '    return None'),
    "guarded-not-sticky": ('                s["guarded"][author] = p["operator"]                            # sticky: only its guard unguards it', '                pass'),
    "agent-closing-unguards-itself": ('        del s["sessions"][b["session"]]', '        if author == s["sessions"][b["session"]]["actor"]: s["guarded"].pop(author, None)\n        del s["sessions"][b["session"]]'),
    "anyone-closing-unguards": ('        del s["sessions"][b["session"]]', '        s["guarded"].pop(s["sessions"][b["session"]]["actor"], None)\n        del s["sessions"][b["session"]]'),
    "closed-sessions-stay": ('del s["sessions"][b["session"]]', 'pass'),
    "agent-may-unguard-itself": ('(b.get("unguard") and guarded_now(state, x) == entry["author"])', '(b.get("unguard") and x == entry["author"])'),
    "anyone-unguards": ('(b.get("unguard") and guarded_now(state, x) == entry["author"])', '(b.get("unguard"))'),
    "unguard-does-nothing": ('if b.get("unguard"): s["guarded"].pop(x, None)', 'if False: s["guarded"].pop(x, None)'),
    "forcing-ignored-for-session-less-agents": ('    p = ctx["profiles"].get(x)\n    if not p or p["actorKind"] == "human": return False', '    p = ctx["profiles"].get(x)\n    if not p or p["actorKind"] == "human" or not any(se["actor"] == x for se in state["sessions"].values()): return False'),
    "forcing-ignored": ('    if not state["policy"]["sessions"].get("forceGuarded"): return False', '    return False'),
    "forcing-without-the-flag": ('    if not state["policy"]["sessions"].get("forceGuarded"): return False', '    pass'),
    "services-exempt-in-every-slot": ('    return not (slot == "executor" and p["actorKind"] == "service")', '    return p["actorKind"] != "service"'),
    "services-forced-as-executors": ('    return not (slot == "executor" and p["actorKind"] == "service")', '    return True'),
    "forced-instruction-to-an-unwatched-target-allowed": ('    if role_of(state, forced_guard(ctx, x)) not in READ_ROLES:\n        return deny("a forced space needs every agent\'s operator here")\n    return None', '    return None'),
    "forced-operator-needs-no-role": ('    if role_of(state, forced_guard(ctx, x)) not in READ_ROLES:', '    if forced_guard(ctx, x) is None:'),
    "forced-tasks-unchecked": ('    for a in b["to"]:\n        d = forced_denial(state, ctx, a, author, "task")\n        if d: return d\n', ''),
    "forced-proposals-unchecked": ('return forced_denial(state, ctx, b["executor"], author, "executor") or ALLOW', 'return ALLOW'),
    "forced-steers-unchecked": ('return forced_denial(state, ctx, sess["actor"], entry["author"], "session") or ALLOW', 'return ALLOW'),
    "forced-executor-guard-not-recorded": [('            if g not in eff.get("guards", []): eff.setdefault("guards", []).append(g)\n', ''),
                                           ('            eff["phase"] = "awaiting-approval"\n        s["effects"][eid] = eff', '            pass\n        s["effects"][eid] = eff')],
    "guards-not-required": ('        if res == "none": return deny("E3 a guard hasn\'t approved")', '        pass'),
    "guard-approval-needs-no-role": ('    if role_of(state, g) not in READ_ROLES: return "none", None\n    for a in eff["approvals"]:\n        if a["author"] != g', '    for a in eff["approvals"]:\n        if a["author"] != g'),
    "owners-stand-in": [('    is_guard = author in guards and role in READ_ROLES', '    is_guard = (author in guards or role == "owner") and role in READ_ROLES'),
                        ('        if a["author"] != g or a.get("consumedBy")', '        if (a["author"] != g and role_of(state, a["author"]) != "owner") or a.get("consumedBy")')],
    "guard-key-ignored": ('        if st == "valid": return "ok", a\n    return "none", None\n\ndef live', '        return "ok", a\n    return "none", None\n\ndef live'),
    "anyone-releases": ('    if not (state["held"].get(x) == entry["author"] or (b.get("unguard") and guarded_now(state, x) == entry["author"])):', '    if False:'),
    "release-needs-no-role": ('    return ALLOW if role in READ_ROLES else deny("guard holds no role")', '    return ALLOW'),
    "modes-ignored-for-open-sessions": ('    return next((m for m in CAUTION[CAUTION.index(sess["steerMode"]):] if m in allowed), None)', '    return sess["steerMode"]'),
    "owners-may-loosen-modes": ('    return next((m for m in CAUTION[CAUTION.index(sess["steerMode"]):] if m in allowed), None)', '    return next((m for m in CAUTION if m in allowed), None)'),
    "guarded-without-guard-stays-live": (' or role_of(state, sess.get("guard")) in READ_ROLES))', ' or "guard" in sess))'),
    "banned-guard-keeps-rights": ('    if actor == s["actor"] or (actor == s.get("guard") and role_of(state, actor) in READ_ROLES):', '    if actor == s["actor"] or actor == s.get("guard"):'),
    "guard-approval-reused": ('        for a in chosen: a["consumedBy"] = eid', '        pass'),
    "returned-approvals-unsuperseded": ('                        gone.append(a)                         # superseded while it was consumed', '                        pass'),
}

def main():
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
                n, v, path, _, _ = explore(mutant(*(edits if isinstance(edits, list) else [edits])), a.depth, cap)
            except OutOfMemory as ex:
                print(f"NOT COMPLETED: {name}: {ex}"); sys.exit(3)
            if v: print(f"caught  {name:38} after {n} transitions: {v}  [{' → '.join(path)}]", flush=True)
            else: print(f"MISSED  {name:38} no violation in {n} transitions", flush=True); missed += 1
        print(f"{len(MUTATIONS) - missed}/{len(MUTATIONS)} guard mutations caught by a property")
        sys.exit(1 if missed else 0)
    try:
        n, v, path, g, gd = explore(POL, a.depth, cap, progress=True)
    except OutOfMemory as ex:
        print(f"NOT COMPLETED: {ex}")
        sys.exit(3)
    if v: print(f"VIOLATION after {n} transitions: {v}\n  path: {' → '.join(path)}"); sys.exit(1)
    if not (g and gd): print(f"VACUOUS: guarded claim seen={g}, guard denial seen={gd}"); sys.exit(1)
    print(f"[guard] {n} transitions to depth {a.depth}; GuardedClaimsNeedTheirHuman, HeldInstructNoOne, NeverLooser, "
          f"GuardRightsNeedRole, OneOpenApprovalPerAuthor and ExactHolds HOLD (non-vacuous)")

if __name__ == "__main__":
    main()

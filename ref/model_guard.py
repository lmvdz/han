"""Bounded exhaustive check of P-15 guarded steering against the real policy.py.

The ghost is derived from the invariant sentence of P-15, not from Policy's rules or its state:

    While an agent is guarded, any instruction to it from anyone but its guard holds it; while it is held, nothing it
    proposes or is named executor of runs without its guard's own approval; only its guard ends a hold, and a release
    covers only what its guard had seen.

The guard of an agent is the operator in its pinned profile, when confirmed (K1). Two sources of guarding: the agent's own
sticky choice (it opened a guarded session; only its guard ends that, with an `agent.release` that unguards, never the agent
itself and never by closing), and owners' `forceGuarded`, under which every non-human is guarded, a service included. An
instruction to a guarded agent whose operator is unconfirmed or holds no role here is denied. Policy `modes` only restrict
`session.open` and are toggled here to show it. "Instruction" = a steer into any of its sessions, a task naming it, a proposal
naming it as executor. Three agents, BOT (operator OP), BOT2 (operator OP2) and the session-less BOT3 (operator OP); a service
X (also with an operator) is forced in every slot. The operators of BOT and BOT3 confirm and withdraw. Owner O and member P
instruct; OP's role and key vary; the approval rule, allowed modes and forceGuarded vary. Releases are signed with `seen` = head,
one less, or the first head of the run. Moves include the held agent finishing its own outside work and the outsider cancelling
it. Closed sessions leave the state, so steering or closing one is `invalid`.

The ghost records, per agent, the seq of every outside instruction it saw (K5), from its own reading of who instructed whom
while the agent was guarded, plus the open outside work it finds in the log when an agent becomes guarded; it never reads
Policy's `lastOutside`. Properties: GuardedClaimsNeedTheirHuman (a claim of an effect whose proposer or executor is guarded,
forced or held needs that agent's guard's unused approval, a role and a valid key, and is refused when the agent has no
confirmed operator), HeldInstructNoOne, NeverLooser (no outside steer into a dead session; no instruction to a guarded agent
without its operator), OneOpenApprovalPerAuthor, ReleaseCoversOnlyWhatWasSeen (a release is allowed exactly when its guard may
release and no outside instruction newer than its `seen` was ever received) and ExactHolds (Policy holds and guards exactly the
agents the invariant holds and guards). Not modelled, and covered by vectors instead (ref/mutate_vectors.py catches each): the
attempt limit (`attempt-limit`; model_check has it as a property), cancel rights, expiry of approvals (`approvals-stay-bounded`),
the `guard_status` result itself (`guard-status`), the shapes of `session.open`.
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
FORCEMAP = dict(GUARD, **{X: OP})              # whom `forceGuarded` guards, for whom: the service X too, in every slot
TOGGLED = (BOT, BOT3)                           # agents whose operator confirms and withdraws
T0 = "2026-10-10T12:00:00Z"
LATE = 13                     # between the setup's outside task and its outside steer
FIRST = 11                    # the head after the setup's sessions are open: a release signed with it predates the setup's proposal and steer
DG = "d" * 64
MODESETS = (("auto", "guarded"), ("guarded",), ("auto",))

def H(label): return hashlib.sha256(label.encode()).hexdigest()
def human(rev=False): return {"actorKind": "human", "operatorConfirmed": False, "keys": {"k1": {"publicKey": "pk", "status": "revoked" if rev else "valid"}}}
def agent(op, ok=True): return {"actorKind": "agent", "operator": op, "operatorConfirmed": ok, "keys": {"k1": {"publicKey": "pk", "status": "valid"}}}
XS = {"actorKind": "service", "operator": OP, "operatorConfirmed": True, "keys": {"k1": {"publicKey": "pk", "status": "valid"}}}

def policy(rule=False, modes=MODESETS[0], force=False):
    return {"visibility": "members", "join": "invite", "membersMayInvite": False, "allowAgents": "any",
            "approvals": [{"action": "pay", "approvers": ["member"]}] if rule else [], "separateApprover": True, "budget": None,
            "maxDelegationDepth": 4, "minDurability": "local", "launchWindow": 30, "extensionsMinRole": "member",
            "sessions": {"watch": "member", "read": "member", "steer": "member", "modes": list(modes), "forceGuarded": force}, "servers": {"deny": []}}

def E(label, kind, author, body):
    return {"id": H(label), "v": 1, "space": SP, "kind": kind, "author": author, "keyId": "k1", "body": body,
            "contentHash": "", "refs": [], "seen": 0, "nonce": label, "time": T0}

S1, S2, S3, S4, S5, S7 = H("s1"), H("s2"), H("s3"), H("s4"), H("s5"), H("s7")
SESS = {S1: ("s1", BOT, "guarded"), S2: ("s2", BOT2, "auto"), S3: ("s3", BOT, "auto"), S4: ("s4", BOT2, "guarded"), S5: ("s5", BOT3, "auto"), S7: ("s7", BOT, "guarded")}
OPENED_AT_START = (S1, S2, S3)                                   # S4: BOT2 may open it later (becoming guarded); S5: session-less BOT3 may open an auto one
PROPS = {"pr1": (BOT, X), "pr2": (BOT2, X), "pr3": (P, BOT), "pr4": (BOT, BOT2), "pr5": (OP, BOT), "pr6": (P, BOT3), "pr7": (P, BOT2)}   # pr5: its own guard's; pr7: aimed at BOT2 before it is guarded (a settled hold records the guard)
CLAIMABLE = ("pr0", "pr1", "pr2", "pr3", "pr4", "pr5", "pr6", "pr7")
TASKS = {"P tasks BOT": (P, [BOT]), "P tasks BOT2+BOT": (P, [BOT2, BOT]), "BOT tasks BOT2": (BOT, [BOT2]), "BOT2 tasks BOT": (BOT2, [BOT]),
         "P tasks BOT3": (P, [BOT3]), "OP2 tasks BOT2": (OP2, [BOT2]), "P tasks BOT+BOT2": (P, [BOT, BOT2]), "P tasks X": (P, [X]), "O tasks P": (O, [P])}      # P is human: never forced

def fz(d): return tuple(sorted(d.items()))

class M:
    __slots__ = ("labels", "state", "oprev", "gd", "held", "covered", "used", "lastk", "closed", "unconf", "outs")
    def __init__(self, **kw):
        for k in self.__slots__: setattr(self, k, kw[k])
    def key(self):
        st = {k: v for k, v in self.state.items() if k not in ("entries", "head", "roleHistory")}
        body = json.dumps([st, self.oprev, self.gd, self.held, self.covered, self.used, self.lastk, sorted(self.closed), sorted(self.unconf), self.outs], sort_keys=True)
        return hashlib.sha256(body.encode()).digest()
    def with_(self, **kw):
        d = {k: getattr(self, k) for k in self.__slots__}; d.update(kw); return M(**d)

def ctx(m):
    return {"time": T0, "profiles": {O: human(), P: human(), OP: human(m.oprev), OP2: human(), X: XS, BOT: agent(OP, BOT not in m.unconf), BOT2: agent(OP2), BOT3: agent(OP, BOT3 not in m.unconf)}}

def apply(pol, m, e, label):
    d = pol.decide(m.state, e, ctx(m))
    if not d["allow"]: return None, d
    s = pol.fold(m.state, {"entry": e, "content": None, "receipt": {"space": SP, "seq": (m.state["head"] + 1) if m.state else 1,
                                                                    "id": e["id"], "time": T0}}, ctx(m))
    return m.with_(labels=m.labels + (label,), state=s), d

def cnt(t, k): return dict(t).get(k, 0)
def setk(t, k, v): d = dict(t); d[k] = v; return fz(d)

# ── the ghost: the invariant sentence, read independently ──
def alive(m, sid):
    """Is session `sid` live? Closed sessions are tracked by the ghost itself (m.closed), not read back from Policy's state."""
    _, actor, _ = SESS[sid]
    return sid in m.state["sessions"] and sid not in m.closed and m.state["roles"].get(actor) is not None

def chosen(m, a):
    """Guarded by its own choice."""
    return dict(m.gd).get(a, False)

def forced(m, a):
    """forceGuarded guards every non-human, whatever it calls itself."""
    return m.state["policy"]["sessions"].get("forceGuarded", False) and a in FORCEMAP

def watched(m, a): return (chosen(m, a) and a in GUARD) or forced(m, a)

def guard_of(m, a):
    """The guard `a` has now: its operator, if confirmed."""
    return FORCEMAP[a] if a in FORCEMAP and a not in m.unconf else None

def outside(m, a, author):
    """Is an instruction by `author` to `a` an outside one, held for a guard?"""
    g = guard_of(m, a)
    return bool(watched(m, a) and g and author not in (a, g))

def hold(m, n, author, targets):
    """`n`, the model after an entry, with the holds and the seqs the invariant puts on the (target, slot) pairs the entry instructs, judged on `m`."""
    held, outs, seq = dict(m.held), dict(m.outs), m.state["head"] + 1
    for a, slot in targets:
        if outside(m, a, author):
            held[a] = True
            outs[a] = max(outs.get(a, 0), seq)
    return n.with_(held=fz(held), outs=fz(outs))

def instructed(kind, author, body):
    """The (target, slot) pairs an entry instructs."""
    if kind == "task": return [(a, "task") for a in body["to"]]
    if kind == "effect.propose": return [(body["executor"], "executor")]
    if kind == "session.input" and body["session"] in SESS: return [(SESS[body["session"]][1], "session")]
    return []

def refusal(m, kind, author, body):
    """What the invariant says about an instruction to a guarded target whose operator is unconfirmed or holds no role: refused."""
    return any(watched(m, a) and a != author and (a in m.unconf or m.state["roles"].get(FORCEMAP[a]) is None) for a, _ in instructed(kind, author, body))

def open_outside_work(m, a):
    """Seqs of the outside work still open for `a`, from the facts in the log: open tasks naming it and unfinished effects naming it as
    executor, from anyone but `a` and its operator."""
    g = FORCEMAP[a]
    seqs = [t["seq"] for t in m.state["tasks"].values() if t["open"] and a in t["to"] and t["author"] not in (a, g)]
    seqs += [e["seq"] for pr in CLAIMABLE for e in [m.state["effects"].get(H(pr))]
             if e and e["phase"] not in POL.FINAL and e["executor"] == a and e["proposer"] not in (a, g)]
    return seqs

def required(m, proposer, executor):
    """Whose own approval a claim needs: the guard of the proposer and of the executor, each if guarded, forced or held. None = no confirmed operator."""
    out = set()
    for who in (proposer, executor):
        if who in dict(m.held) or watched(m, who): out.add(guard_of(m, who))
    return out

def init(pol):
    m = M(labels=(), state=None, oprev=False, gd=(), held=(), covered=(), used=(), lastk=(), closed=frozenset(), unconf=frozenset(), outs=())
    m, d = apply(pol, m, E("C", "space.create", O, {"policy": policy()}), "C"); assert m, d
    for lab, a in (("m1", P), ("m2", X), ("m3", OP), ("m4", OP2), ("m5", BOT), ("m6", BOT2), ("m7", BOT3)):
        m, d = apply(pol, m, E(lab, "member.set", O, {"actor": a, "role": "member"}), lab); assert m, (lab, d)
    for sid in OPENED_AT_START:
        lab, a, mode = SESS[sid]
        m, d = apply(pol, m, E(lab, "session.open", a, {"steerMode": mode}), lab); assert m, (lab, d)
    m = m.with_(gd=fz({BOT: True}))
    m, d = apply(pol, m, E("pr0", "effect.propose", BOT, {"executor": X, "action": "pay", "planDigest": DG, "repeatable": True}), "pr0")
    assert m, d                                                                               # proposed before any hold
    m1, d = apply(pol, m, E("t0", "task", P, {"to": [BOT]}), "t0"); assert m1, d             # an outside task already open, for release staleness
    m = hold(m, m1, P, [(BOT, "task")])
    m2, d = apply(pol, m, E("in0", "session.input", P, {"session": S1}), "in0"); assert m2, d     # P's outside steer
    m2 = hold(m, m2, P, [(BOT, "session")])
    m3, d = apply(pol, m2, E("ap0", "effect.approve", OP, {"proposal": H("pr0"), "planDigest": DG}), "ap0"); assert m3, d   # its guard approves once
    return m3.with_(covered=setk(m3.covered, ("pr0", OP), 1))

def successors(pol, m):
    out = []
    def go(name, kind, author, body, after=None, label=None, seen=None):
        lab = label or (name + "#" + str(sum(1 for x in m.labels if x == name)))
        e = E(lab, kind, author, body)
        e["seen"] = m.state["head"] if seen is None and kind == "agent.release" else (seen or 0)      # a release defaults to: the guard reviewed everything so far
        m2, d = apply(pol, m, e, name)
        if m2 and after: m2 = after(m2)
        out.append((name, kind, author, body, m2, d, e["seen"]))
    for author in (P, O, OP2, OP, BOT, BOT2):
        for sid, (lab, actor, _) in SESS.items():
            if sid not in m.state["sessions"] and sid not in OPENED_AT_START: continue        # a closed one is steered too: `invalid`
            go(f"{author} steers {lab}", "session.input", author, {"session": sid},
               lambda n, a=author, x=actor: hold(m, n, a, [(x, "session")]))
    head = m.state["head"]
    for author in (OP, OP2, O, BOT):
        for a in (BOT, BOT2, BOT3, X):                           # a release names the agent; no session is needed
            for seen in ((head, head - 1, FIRST, LATE) if FORCEMAP.get(a) == author else (head,)):   # stale releases, only where they could matter
                go(f"{author} releases {a[1:].split(':')[0]}@{seen}", "agent.release", author, {"agent": a},
                   lambda n, a=a: n.with_(held=fz({k: v for k, v in m.held if k != a})), seen=seen)
    for author in (OP, OP2, BOT):                                # only the guard unguards, with a release
        for a in (BOT, BOT2, BOT3):
            for seen in ((head, head - 1, FIRST, LATE) if GUARD.get(a) == author else (head,)):
                go(f"{author} unguards {a[1:].split(':')[0]}@{seen}", "agent.release", author, {"agent": a, "unguard": True},
                   lambda n, a=a: n.with_(held=fz({k: v for k, v in m.held if k != a}), gd=setk(m.gd, a, False)), seen=seen)
    for name, (author, to) in TASKS.items():
        go(name, "task", author, {"to": to}, lambda n, author=author, to=to: hold(m, n, author, [(a, "task") for a in to]))
    for tid, t in m.state["tasks"].items():                      # the assignee finishes its own work; the outsider cancels it
        if t["open"]:
            who = t["to"][0][1:].split(':')[0]
            go(f"{who} closes task {tid[:6]}", "task.update", t["to"][0], {"task": tid, "state": "done"})
            go(f"{who} declines task {tid[:6]}", "task.update", t["to"][0], {"task": tid, "state": "declined"})
            go(f"{t['author'][1:].split(':')[0]} cancels task {tid[:6]}", "task.cancel", t["author"], {"task": tid})
    for sid, a in ((S4, BOT2), (S7, BOT)):                      # becoming guarded: open outside work holds at once and is noted
        if sid in m.state["sessions"] or chosen(m, a): continue
        def opened(n, a=a):
            seqs = open_outside_work(m, a)
            held, outs = dict(n.held), dict(n.outs)
            if seqs: held[a] = True; outs[a] = max(seqs + [outs.get(a, 0)])
            return n.with_(gd=setk(m.gd, a, True), held=fz(held), outs=fz(outs))
        go(f"{a[1:].split(':')[0]} opens {SESS[sid][0]} guarded", "session.open", a, {"steerMode": "guarded"}, opened, label=SESS[sid][0])
    if S5 not in m.state["sessions"]:
        go("BOT3 opens s5", "session.open", BOT3, {"steerMode": "auto"}, label="s5")
    for author in (BOT, O):
        go(f"{author} closes s1", "session.close", author, {"session": S1}, lambda n: n.with_(closed=n.closed | {S1}))      # closing never unguards, whoever closes
    for pr, (author, executor) in PROPS.items():
        if H(pr) in m.state["effects"]: continue
        body = {"executor": executor, "action": "pay", "planDigest": DG, "repeatable": pr == "pr1"}
        go(f"{author} proposes {pr}", "effect.propose", author, body, lambda n, a=author, x=executor: hold(m, n, a, [(x, "executor")]), label=pr)
    for pr in CLAIMABLE:
        pid = H(pr); eff = m.state["effects"].get(pid)
        if not eff or eff["phase"] in POL.FINAL: continue
        go(f"{eff['proposer'][1:].split(':')[0]} cancels {pr}", "effect.cancel", eff["proposer"], {"proposal": pid})
        if len(eff["claims"]) >= 2: continue
        for who in (OP, OP2, P, BOT, O):
            go(f"{who} approves {pr}", "effect.approve", who, {"proposal": pid, "planDigest": DG},
               lambda n, who=who, pr=pr: n.with_(covered=setk(n.covered, (pr, who), cnt(n.used, (pr, who)) + 1)))
        a = len(eff["claims"]) + 1
        req = tuple(sorted(g for g in required(m, eff["proposer"], eff["executor"]) if g))
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
        out.append(("OP key revoked", "key", OP, {}, m.with_(oprev=True, labels=m.labels + ("OP key revoked",)), {"allow": True}, 0))
    for a in TOGGLED:                                            # the pinned profile varies: the operator confirms or withdraws
        who = a[1:].split(":")[0]
        now = m.unconf | {a} if a not in m.unconf else m.unconf - {a}
        out.append((f"OP {'withdraws' if a not in m.unconf else 'confirms'} {who}", "profile", OP, {}, m.with_(unconf=frozenset(now), labels=m.labels + (f"op-toggles-{who}",)), {"allow": True}, 0))
    pol_now = m.state["policy"]
    rule, modes, force = bool(pol_now["approvals"]), tuple(pol_now["sessions"]["modes"]), pol_now["sessions"]["forceGuarded"]
    go("O toggles approval rule", "policy.set", O, {"policy": policy(not rule, modes, force)})
    go("O toggles forceGuarded", "policy.set", O, {"policy": policy(rule, modes, not force)})
    for ms_ in MODESETS:
        if ms_ != modes: go(f"O sets modes {'/'.join(ms_)}", "policy.set", O, {"policy": policy(rule, ms_, force)})
    return out

def stale(m, a, seen):
    """The invariant's "a release covers only what its guard had seen": an outside instruction newer than `seen` was received."""
    return dict(m.outs).get(a, 0) > seen

def check_state(pol, m):
    if set(dict(m.held)) != set(m.state["held"]):
        return f"ExactHolds: Policy holds {sorted(m.state['held'])} but the invariant holds {sorted(dict(m.held))}"
    if set(a for a, v in m.gd if v) != set(m.state["guarded"]):
        return f"ExactHolds: Policy guards {sorted(m.state['guarded'])} but the invariant guards {sorted(a for a, v in m.gd if v)}"
    for pid, eff in m.state["effects"].items():
        authors = [a["author"] for a in eff["approvals"] if not a.get("consumedBy")]
        if len(authors) != len(set(authors)): return "an author has two open approvals on one proposal"
    return None

def check(m, name, kind, author, body, d, seen):
    if kind == "agent.release":
        a = body["agent"]
        authorized = (dict(m.held).get(a) or (body.get("unguard") and chosen(m, a))) and author == FORCEMAP.get(a) and a not in m.unconf
        ok = bool(authorized) and m.state["roles"].get(author) is not None and not (author == OP and m.oprev)
        if d["allow"] and not ok: return f"{author} released {a} ({'unguard' if body.get('unguard') else 'plain'}), which only its guard {FORCEMAP.get(a)} may, holding a role"
        if d["allow"] and stale(m, a, seen): return f"ReleaseCoversOnlyWhatWasSeen: {author} released {a} with seen={seen}, though an outside instruction newer than that arrived"
        if ok and not d["allow"] and not stale(m, a, seen): return f"guard {author} refused when releasing {a}: {d.get('reason')}"
    refuse = refusal(m, kind, author, body)
    if d["allow"] and refuse: return f"{kind} by {author} allowed to a guarded target whose operator is unconfirmed or holds no role"
    if not d["allow"] and "needs its operator" in d.get("reason", "") and not refuse: return f"{kind} by {author} refused for lack of an operator, though every guarded target has one here"
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
        for g in required(m, eff["proposer"], eff["executor"]):
            if g is None: return f"claim of {pr} allowed for a guarded agent with no confirmed operator"
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
        return 0, f"setup step refused or diverged: {ex}", ("init",), False, False, False
    seen = {start.key()}; frontier = [(pack(start), ())]                # the frontier is kept compressed; `seen` holds 32-byte hashes
    n, saw_claim, saw_denial, saw_stale, tick = 0, False, False, False, 0
    for level in range(depth):
        nxt = []
        for blob, path in frontier:
            m = unpack(blob)
            tick += 1
            if cap_bytes and tick % 128 == 0 and rss_bytes() > cap_bytes:
                raise OutOfMemory(f"memory cap of {cap_bytes / 2**30:.1f} GB reached at depth {level + 1} of {depth} after {n} transitions and {len(seen)} states "
                                  f"(frontier {len(frontier)}, next {len(nxt)})")
            for name, kind, author, body, m2, d, rseen in successors(pol, m):
                n += 1
                v = check(m, name, kind, author, body, d, rseen)
                if v: return n, v, path + (name,), saw_claim, saw_denial, saw_stale
                if kind == "agent.release": saw_stale |= (not d["allow"] and "stale" in d.get("reason", ""))
                if kind == "effect.claim":
                    pr = name.split()[2].split("#")[0]; eff = m.state["effects"][H(pr)]
                    if required(m, eff["proposer"], eff["executor"]):
                        saw_claim |= d["allow"]; saw_denial |= (not d["allow"] and "guard" in d.get("reason", ""))
                if m2 is not None:
                    k = m2.key()
                    if k in seen: continue
                    v = check_state(pol, m2)
                    if v: return n, v, path + (name,), saw_claim, saw_denial, saw_stale
                    seen.add(k)
                    if level + 1 < depth: nxt.append((pack(m2), path + (name,)))      # the last level is only checked, never expanded
        frontier = nxt
        if progress: print(f"  depth {level + 1}: {n} transitions, {len(seen)} states, frontier {len(frontier)}, rss {rss_bytes() / 2**20:.0f} MB", flush=True)
    return n, None, None, saw_claim, saw_denial, saw_stale

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

STALE = '    if state["lastOutside"].get(b["agent"], 0) > entry["seen"]:'
FORCED = '"forced": agent and bool(state["policy"]["sessions"].get("forceGuarded"))}'
GUARDS = 'if g["held"] or g["guarded"] or g["forced"]: out.append(g.get("guard"))'
DENIAL = '    if role_of(state, g.get("guard")) not in READ_ROLES: return deny("a guarded agent needs its operator here")'
NOTE = '        s["lastOutside"][x] = seq'
MUTATIONS = {
    "held-gate-off": ('    if author in state["held"] and not held_may(state, entry):', '    if False:'),
    "held-may-relay": ('        return not sess or sess["actor"] == entry["author"]', '        return True'),
    "held-may-delegate": ('HELD_KINDS = {"message",', 'HELD_KINDS = {"task", "message",'),
    "held-may-approve": ('HELD_KINDS = {"message",', 'HELD_KINDS = {"effect.approve", "message",'),
    "guarded-effects-need-no-guard": (GUARDS, 'if g["held"] or g["forced"]: out.append(g.get("guard"))'),
    "forced-effects-need-no-guard": (GUARDS, 'if g["held"] or g["guarded"]: out.append(g.get("guard"))'),
    "held-effects-need-no-guard": (GUARDS, 'if g["guarded"] or g["forced"]: out.append(g.get("guard"))'),
    "proposers-need-no-guard": ('for who in dict.fromkeys((eff["proposer"], eff["executor"])):', 'for who in (eff["executor"],):'),
    "executors-need-no-guard": ('for who in dict.fromkeys((eff["proposer"], eff["executor"])):', 'for who in (eff["proposer"],):'),
    "unguardable-agents-claimable": ('        if g is None: return deny("E3 a guarded agent has no confirmed operator")', '        if g is None: continue'),
    "steers-hold-no-one": ('        instruct(s, ctx, sess["actor"], author, seq)', '        pass'),
    "tasks-hold-no-one": ('        for x in b["to"]: instruct(s, ctx, x, author, seq)', '        pass'),
    "proposals-hold-no-executor": ('        instruct(s, ctx, b["executor"], author, seq)', '        pass'),
    "guards-hold-their-own-agent": ('    if author not in (x, g.get("guard")) and (g["guarded"] or g["forced"]):', '    if author != x and (g["guarded"] or g["forced"]):'),
    "outside-seqs-not-noted": (NOTE, '        pass'),
    "only-the-first-outside-seq-noted": (NOTE, '        s["lastOutside"].setdefault(x, seq)'),
    "guarding-holds-no-one": ('                s["held"][author] = True', '                pass'),
    "guarding-ignores-pending-tasks": ('pending = [t["seq"] for t in s["tasks"].values() if t["open"] and author in t["to"] and t["author"] not in (author, g)]', 'pending = []'),
    "guarding-ignores-pending-effects": ('pending += [x["seq"] for x in s["effects"].values() if x["phase"] not in FINAL and x["executor"] == author and x["proposer"] not in (author, g)]', 'pass'),
    "pending-includes-the-guards-own-work": ('t["author"] not in (author, g)]', 't["author"] != author]'),
    "pending-counts-closed-tasks": ('if t["open"] and author in t["to"] and', 'if author in t["to"] and'),
    "pending-counts-finished-effects": ('if x["phase"] not in FINAL and x["executor"] == author', 'if x["executor"] == author'),
    "pending-tasks-first-assignee-only": ('author in t["to"] and t["author"] not in (author, g)]', 'author == t["to"][0] and t["author"] not in (author, g)]'),
    "guarding-notes-no-bound": ('                s["lastOutside"][author] = max(pending + [s["lastOutside"].get(author, 0)])', '                pass'),
    "guarding-lowers-the-bound": ('max(pending + [s["lastOutside"].get(author, 0)])', 'max(pending)'),
    "release-lowers-the-bound": ('        s["held"].pop(x, None)\n', '        s["held"].pop(x, None); s["lastOutside"].pop(x, None)\n'),
    "stale-releases-accepted": (STALE, '    if False:'),
    "releases-need-to-see-more": (STALE, '    if state["lastOutside"].get(b["agent"], 0) >= entry["seen"]:'),
    "guarded-not-sticky": ('            s["guarded"][author] = True                                          # sticky: only its guard unguards it', '            pass'),
    "agent-closing-unguards-itself": ('        del s["sessions"][b["session"]]', '        if author == s["sessions"][b["session"]]["actor"]: s["guarded"].pop(author, None)\n        del s["sessions"][b["session"]]'),
    "anyone-closing-unguards": ('        del s["sessions"][b["session"]]', '        s["guarded"].pop(s["sessions"][b["session"]]["actor"], None)\n        del s["sessions"][b["session"]]'),
    "closed-sessions-stay": ('del s["sessions"][b["session"]]', 'pass'),
    "unguard-does-nothing": ('if b.get("unguard"): s["guarded"].pop(x, None)', 'if False: s["guarded"].pop(x, None)'),
    "unguard-needs-a-hold": ('not (g["held"] or (b.get("unguard") and g["guarded"]))', 'not g["held"]'),
    "anyone-unguards": ('(b.get("unguard") and g["guarded"])', 'b.get("unguard")'),
    "anyone-releases": ('    if g.get("guard") != author or not (', '    if not ('),
    "release-needs-no-role": ('    return ALLOW if role in READ_ROLES else deny("guard holds no role")', '    return ALLOW'),
    "forcing-ignored": (FORCED, '"forced": False}'),
    "forcing-without-the-flag": (FORCED, '"forced": agent}'),
    "services-exempt-from-forcing": ('    agent = bool(p) and p["actorKind"] != "human"', '    agent = bool(p) and p["actorKind"] == "agent"'),
    "humans-forced": ('    agent = bool(p) and p["actorKind"] != "human"', '    agent = bool(p)'),
    "unconfirmed-operators-guard": ('if agent and p.get("operatorConfirmed") and p.get("operator"):', 'if agent and p.get("operator"):'),
    "instructions-to-unguardable-agents-allowed": (DENIAL, '    pass'),
    "instructions-need-only-a-confirmation": (DENIAL, '    if not g.get("guard"): return deny("a guarded agent needs its operator here")'),
    "chosen-guarding-needs-no-operator-here": ('    if author == x or not (g["guarded"] or g["forced"]): return None', '    if author == x or not g["forced"]: return None'),
    "forced-needs-no-operator-here": ('    if author == x or not (g["guarded"] or g["forced"]): return None', '    if author == x or not g["guarded"]: return None'),
    "tasks-to-guarded-agents-unchecked": ('    for a in b["to"]:\n        d = instruction_denial(state, ctx, a, author)\n        if d: return d\n', ''),
    "proposals-naming-guarded-executors-unchecked": ('return instruction_denial(state, ctx, b["executor"], author) or ALLOW', 'return ALLOW'),
    "steers-into-guarded-agents-unchecked": ('return instruction_denial(state, ctx, sess["actor"], entry["author"]) or ALLOW', 'return ALLOW'),
    "guards-not-required": ('        if res == "none": return deny("E3 a guard hasn\'t approved")', '        pass'),
    "guard-approval-needs-no-role": ('    if role_of(state, g) not in READ_ROLES: return "none", None\n    for a in eff["approvals"]:\n        if a["author"] != g', '    for a in eff["approvals"]:\n        if a["author"] != g'),
    "owners-stand-in": [('    if not (author in guards_of(state, ctx, eff) and role in READ_ROLES):', '    if not ((author in guards_of(state, ctx, eff) or role == "owner") and role in READ_ROLES):'),
                        ('        if a["author"] != g or a.get("consumedBy")', '        if (a["author"] != g and role_of(state, a["author"]) != "owner") or a.get("consumedBy")')],
    "guard-key-ignored": ('        if st == "valid": return "ok", a\n    return "none", None\n\ndef live', '        return "ok", a\n    return "none", None\n\ndef live'),
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
                n, v, path, _, _, _ = explore(mutant(*(edits if isinstance(edits, list) else [edits])), a.depth, cap)
            except OutOfMemory as ex:
                print(f"NOT COMPLETED: {name}: {ex}"); sys.exit(3)
            if v: print(f"caught  {name:38} after {n} transitions: {v}  [{' → '.join(path)}]", flush=True)
            else: print(f"MISSED  {name:38} no violation in {n} transitions", flush=True); missed += 1
        print(f"{len(MUTATIONS) - missed}/{len(MUTATIONS)} guard mutations caught by a property")
        sys.exit(1 if missed else 0)
    try:
        n, v, path, g, gd, st = explore(POL, a.depth, cap, progress=True)
    except OutOfMemory as ex:
        print(f"NOT COMPLETED: {ex}")
        sys.exit(3)
    if v: print(f"VIOLATION after {n} transitions: {v}\n  path: {' → '.join(path)}"); sys.exit(1)
    if not (g and gd and st): print(f"VACUOUS: guarded claim seen={g}, guard denial seen={gd}, stale release refused seen={st}"); sys.exit(1)
    print(f"[guard] {n} transitions to depth {a.depth}; GuardedClaimsNeedTheirHuman, HeldInstructNoOne, NeverLooser, "
          f"OneOpenApprovalPerAuthor, ReleaseCoversOnlyWhatWasSeen and ExactHolds HOLD (non-vacuous)")

if __name__ == "__main__":
    main()

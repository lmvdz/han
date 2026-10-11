"""Exhaustive bounded model check of HAN v1 effects, driving the REAL reference Policy (ref/policy.py).

World: one space. Actors: owner O, proposer P, executor X (one instance; its process may crash and restart; its launch
journal survives). Time is the home's clock in tenths of a second; receipts carry whole seconds (rounded down). The
executor's clock differs from the home's by a fixed offset (0 or −10 s) and its RATE may differ too: the home may run
fast and the executor slow. The model exaggerates drift to ±20% (the reference formula P.abandon_after is parametric in
the drift bound; the spec's bound is ±1%), so drift attacks are reachable at small windows. Claims travel to the home with
arbitrary delay, the owner may shorten launchWindow at any time, and between the executor's deadline check and its launch
up to the stall bound may pass on the executor's clock (X-04).

Every interleaving of these actions is explored (BFS, states deduplicated):
  P proposes · O approves · O removes P · P's key is revoked · O cancels · O abandons · O resolves failed/succeeded
  O shortens launchWindow · time ticks (timing: +0.9 s, +9.1 s — chosen to land on every rounding and window boundary;
  single: +5 s, +25 s)
  X: journal claiming → send claim → (network delivers it) → see receipt → journal launching → deadline check
     → [stall ≤ 10 s] → launch → report; or expired → report not started; crash; restart recovery (X-06)

Properties (architecture.md §10):
  AtMostOneLaunchPerClaim · NoLaunchWithoutReceipt · NoLaunchAfterAbandon · HonestNotStarted · HonestStarted
  RevokedBeforeClaimNeverLaunches · UnknownNeverSilentlyRetried (launches ≤ 1 + resolve(failed) statements that were
  already false when made, i.e. made after a launch: honest resolves never allow a second run)

Usage:  python model_check.py [--scenario timing|single|all]       check the spec as written
        python model_check.py --mutation NAME                      break one rule; a violation MUST be found
        python model_check.py --all-mutations                      every mutation; exit 0 only if each is caught
"""
from __future__ import annotations
import argparse, hashlib, json, os, re, subprocess, sys
from collections import deque
from datetime import datetime, timedelta, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import policy as POL
try:
    sys.stdout.reconfigure(encoding="utf-8")
except Exception:
    pass

O, P, X = "@o:a.example", "@p:b.example", "@x:c.example"
A, B = "sa:a.example", "sb:a.example"
BASE = datetime(2026, 10, 10, 12, 0, 0, tzinfo=timezone.utc)
W = 20                       # initial policy.launchWindow (seconds)
MODEL_DRIFT_PPM = 200_000    # ±20% in the model (spec: ±1%); P.abandon_after is evaluated with this bound
W_SHORT = 15                 # what the owner may shorten it to
W_LONG = 30                  # ... or lengthen it to
STALL = POL.STALL_BOUND      # 10 s
DS = 10                      # model time unit: tenths of a second
# scenario → bounds. "timing": clock offset, network delay and stall (one attempt); "single": two attempts with
# revocation, cancel and abandon; "successor": home death and successors from any mirror head.
# clocks: (executor offset ds, home rate num/den, executor rate num/den)
BOUNDS = {"timing": {"attempts": 1, "crashes": 1, "ticks": 4, "ticksizes": (9, 91), "delay": True, "setwindow": True,
                     "clocks": ((0, (1, 1), (1, 1)), (-100, (1, 1), (1, 1)))},
          "drift":  {"attempts": 1, "crashes": 1, "ticks": 4, "ticksizes": (9, 91, 109), "delay": True, "setwindow": False,
                     "clocks": ((0, (6, 5), (4, 5)),)},
          "single": {"attempts": 2, "crashes": 1, "ticks": 2, "ticksizes": (50, 250), "delay": False, "setwindow": False,
                     "clocks": ((0, (1, 1), (1, 1)),)}}
def bound(k): return BOUNDS[SCENARIO][k]
MUTATIONS = {
    "no-launch-journal": "X-04 broken: executor launches without journaling `launching`",
    "launch-after-restart": "X-06 broken: a recovered claim is launched instead of reported not-started",
    "no-window-check": "X-04 broken: executor launches regardless of the deadline",
    "deadline-from-receipt-time": "X-04 broken: deadline = home receipt time + window, compared with the executor's own clock",
    "window-not-pinned": "P-09 broken: abandon uses the current launchWindow, not the claim's",
    "executor-uses-current-window": "X-04 broken: executor uses the window in force at send, not the one pinned in its claim",
    "no-quantization-margin": "P-09 broken: abandon at exactly receipt time + window (no whole-second / drift margin)",
    "one-sided-drift-margin": "P-09 broken: the margin covers drift of one clock only (round-5 formula)",
    "no-E2": "P-07 E2 broken: claim ignores proposer removal and key revocation",
    "abandon-anytime": "P-09 broken: owner may abandon a claim immediately",
}
MUT = None
SCENARIO = "single"

def iso(t): return (BASE + timedelta(seconds=t // DS)).strftime("%Y-%m-%dT%H:%M:%SZ")     # receipts round down
def home_now(st):
    _, (n, d), _ = st.clk
    return st.time * n // d                       # home clock (ds) from true time
def exec_now(st):
    off, _, (n, d) = st.clk
    return st.time * n // d + off                 # executor clock (ds)
def secs(s): return int((datetime.strptime(s, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc) - BASE).total_seconds())  # whole seconds

def POLICY(w=W):
    return {"visibility": "members", "join": "invite", "membersMayInvite": False, "allowAgents": "any",
            "approvals": [{"action": "*", "approvers": ["owner"]}], "separateApprover": False, "budget": None,
            "maxDelegationDepth": 4, "minDurability": "local", "launchWindow": w, "extensionsMinRole": "member", "sessions": {"watch": "member", "read": "member", "steer": "owner", "modes": ["auto", "guarded"]}, "servers": {"deny": []}}

def HX(label):
    """Entry ids are 64-hex (R-09); the model keeps readable labels and hashes them."""
    return label if re.fullmatch(r"[0-9a-f]{64}", label) else hashlib.sha256(("id:" + label).encode()).hexdigest()
PR = HX("pr")
DG = hashlib.sha256(b"plan:pay").hexdigest()
CLAIM_ATTEMPT = {}

def E(id, space, kind, author, body):
    return {"id": HX(id), "v": 1, "space": space, "kind": kind, "author": author, "keyId": "k1", "body": body,
            "contentHash": "", "refs": [], "seen": 0, "nonce": id, "time": iso(0)}

def prof(actor, kind="human", revoked=False, seq=1):
    return {"actor": actor, "seq": seq, "actorKind": kind, "operatorConfirmed": kind != "human",
            "keys": {"k1": {"publicKey": "pk", "status": "revoked" if revoked else "valid"}}}

# ── policy patches for mutations ──
_orig_claim, _orig_outcome, _orig_abandon_after = POL._claim, POL._outcome, POL.abandon_after
def patch():
    if MUT == "no-E2":
        def claim(state, entry, ctx, role):
            eff = state["effects"].get(entry["body"]["proposal"])
            if eff:
                state = dict(state, roles=dict(state["roles"], **{eff["proposer"]: "member"}))
                ctx = dict(ctx, profiles=dict(ctx["profiles"], **{eff["proposer"]: prof(eff["proposer"])}))
            return _orig_claim(state, entry, ctx, role)
        POL._claim = claim
    POL.DRIFT_PPM = MODEL_DRIFT_PPM if SCENARIO == "drift" else 10_000     # the exaggerated bound only where rates differ
    if MUT == "no-quantization-margin":
        POL.abandon_after = lambda w, drift_ppm=None: w
    if MUT == "one-sided-drift-margin":
        POL.abandon_after = lambda w, drift_ppm=None: w + 1 + -(-w * MODEL_DRIFT_PPM // 1_000_000)
    if MUT == "window-not-pinned":
        def outcome(state, entry, ctx, role):
            pid, eff, c = POL.find_claim(state, entry["body"]["claim"])
            if c and role == "owner":
                state = dict(state, effects=dict(state["effects"], **{pid: dict(eff, claims=eff["claims"][:-1] + [dict(c, window=state["policy"]["launchWindow"])])}))
            return _orig_outcome(state, entry, ctx, role)
        POL._outcome = outcome
    if MUT == "abandon-anytime":
        def outcome(state, entry, ctx, role):
            if role == "owner" and entry["body"].get("result") == "unknown":
                ctx = dict(ctx, time="2099-01-01T00:00:00Z")
            return _orig_outcome(state, entry, ctx, role)
        POL._outcome = outcome

def freeze(d): return tuple(sorted(d.items()))

class St:
    __slots__ = ("logs", "time", "clk", "keyrev", "journal", "sent", "inflight", "x_up", "x_seen", "checked",
                 "world", "launch_at", "false_resolves", "crashes", "ticks", "wset")
    def __init__(self, **kw):
        for k in self.__slots__: setattr(self, k, kw[k])
    def key(self): return tuple(getattr(self, k) for k in self.__slots__)
    def with_(self, **kw):
        d = {k: getattr(self, k) for k in self.__slots__}; d.update(kw); return St(**d)

_fold_cache, _decide_cache = {}, {}
def state_of(logs, space):
    log = dict(logs).get(space, ())
    k = (space, log)
    if k in _fold_cache: return _fold_cache[k]
    s = None
    for seq, (ej, ctxj) in enumerate(log, 1):
        e, ctx = json.loads(ej), json.loads(ctxj)
        s = POL.fold(s, {"entry": e, "content": None, "receipt": {"space": space, "seq": seq, "id": e["id"], "time": ctx["time"]}}, ctx)
    _fold_cache[k] = s
    return s

def ctx_for(st):
    return {"time": iso(home_now(st)), "profiles": {O: prof(O), X: prof(X, "service"), P: prof(P, revoked=st.keyrev, seq=2 if st.keyrev else 1)}}

def append(st, e):
    """The home appends with R-01 dedupe → (state, allow|dup|denied|invalid)."""
    logs = dict(st.logs); sp = e["space"]; log = logs.get(sp, ())
    if any(json.loads(x[0])["id"] == e["id"] for x in log): return st, "dup"
    ctx = ctx_for(st)
    mk = (sp, log, e["id"], json.dumps(e["body"], sort_keys=True), iso(home_now(st)), st.keyrev)
    d = _decide_cache.get(mk)
    if d is None: d = _decide_cache[mk] = POL.decide(state_of(st.logs, sp), e, ctx)
    if not d["allow"]: return st, d["code"]
    logs[sp] = log + ((json.dumps(e, sort_keys=True), json.dumps(ctx, sort_keys=True)),)
    return st.with_(logs=freeze(logs)), "allow"

def rec_index(st, space, eid):
    for i, x in enumerate(dict(st.logs).get(space, ())):
        if json.loads(x[0])["id"] == eid: return i
    return -1

def claim_id(space, a):
    h = HX(f"cl-{space[:2]}-{a}"); CLAIM_ATTEMPT[h] = a; return h

def claim_receipt_time(st, space, a):
    for x in dict(st.logs).get(space, ()):
        if json.loads(x[0])["id"] == claim_id(space, a): return secs(json.loads(x[1])["time"])
    return None

def claim_entry(st, sp, a):
    """The executor's claim states the window it read when it sent the claim (X-03); Policy accepts it only if it is still
    the space's launchWindow (E5), and then pins it."""
    return E(f"cl-{sp[:2]}-{a}", sp, "effect.claim", X, {"proposal": PR, "attempt": a, "instance": "k1", "window": dict(st.wset)[(sp, a)]})

def pinned_window(st, sp, a):
    eff = state_of(st.logs, sp)["effects"][PR]
    return next(c["window"] for c in eff["claims"] if c["id"] == claim_id(sp, a))

def deadline_ok(st, sp, a):
    """X-04: elapsed on the executor's own clock since the claim was first sent < (pinned window − stall bound)."""
    local_now = exec_now(st)
    w = pinned_window(st, sp, a)
    if MUT == "executor-uses-current-window": w = state_of(st.logs, sp)["policy"]["launchWindow"]   # reads policy instead of its claim
    if MUT == "deadline-from-receipt-time":
        return local_now < (claim_receipt_time(st, sp, a) + w - STALL) * DS
    return local_now - dict(st.sent)[(sp, a)] < (w - STALL) * DS

def init_states():
    out = []
    for clk in bound("clocks"):
        st = St(logs=(), time=0, clk=clk, keyrev=False, journal=(), sent=(), inflight=frozenset(), x_up=True,
                x_seen=frozenset(), checked=(), world=(), launch_at=(), false_resolves=0, crashes=0, ticks=0, wset=())
        for e in (E("C", A, "space.create", O, {"policy": POLICY()}),
                  E("m1", A, "member.set", O, {"actor": P, "role": "member"}),
                  E("m2", A, "member.set", O, {"actor": X, "role": "member"})):
            st, r = append(st, e); assert r == "allow", (e, r)
        out.append(st)
    return out

def successors(st):
    out = []
    spaces = [A]
    out.append(("P propose", append(st, E("pr", A, "effect.propose", P, {"executor": X, "action": "pay", "planDigest": DG, "repeatable": False}))))
    for sp in spaces:
        S0 = state_of(st.logs, sp)
        e0 = S0["effects"].get(PR) if S0 else None
        if e0 and e0["phase"] in ("awaiting-approval", "ready") and not any(not a.get("consumedBy") and not a.get("dropped") for a in e0["approvals"]):
            out.append((f"O approve {sp[:2]}", append(st, E(f"ap-{sp[:2]}-{len(dict(st.logs).get(sp, ()))}", sp, "effect.approve", O, {"proposal": PR, "planDigest": DG}))))
        if SCENARIO == "single":
            out.append((f"O cancel {sp[:2]}", append(st, E(f"cx-{sp[:2]}", sp, "effect.cancel", O, {"proposal": PR}))))
        for r in ("failed", "succeeded"):
            s2, res = append(st, E(f"rs-{sp[:2]}-{r}-{len(dict(st.logs).get(sp, ()))}", sp, "effect.resolve", O, {"proposal": PR, "result": r}))
            if res == "allow" and r == "failed" and any(n for (s_, _), n in st.world if s_ == sp):
                s2 = s2.with_(false_resolves=s2.false_resolves + 1)      # a statement already false when made
            out.append((f"O resolve {sp[:2]} {r}", (s2, res)))
        for a in range(1, bound("attempts") + 1):
            out.append((f"O abandon {sp[:2]}{a}", append(st, E(f"ab-{sp[:2]}-{a}", sp, "effect.outcome", O,
                                                               {"claim": claim_id(sp, a), "result": "unknown", "started": "unknown"}))))
    if SCENARIO == "single":
        out.append(("O remove P", append(st, E("rmP", A, "member.set", O, {"actor": P, "role": "none"}))))
        if not st.keyrev: out.append(("P key revoked", (st.with_(keyrev=True), "allow")))
    if bound("setwindow") and state_of(st.logs, A)["policy"]["launchWindow"] == W:
        for nw in (W_SHORT, W_LONG):                      # the owner may shorten or lengthen the window mid-claim
            out.append((f"O sets launchWindow {nw}", append(st, E(f"w{nw}", A, "policy.set", O, {"policy": POLICY(nw)}))))
    if st.ticks < bound("ticks"):
        for dt in bound("ticksizes"):
            # the stall assumption (X-04): at most STALL seconds between a passed deadline check and the launch
            _, _, (en, ed) = st.clk                 # stall measured on the executor's clock
            if all((st.time + dt - t0) * en // ed <= STALL * DS for _, t0 in st.checked):
                out.append((f"tick +{dt}", (st.with_(time=st.time + dt, ticks=st.ticks + 1), "allow")))
    j, w, la, sent, chk = dict(st.journal), dict(st.world), dict(st.launch_at), dict(st.sent), dict(st.checked)
    # network: deliver an in-flight claim at any time (delay is arbitrary)
    for key in sorted(st.inflight):
        sp, a = key
        s2, res = append(st, claim_entry(st, sp, a))
        s2 = s2.with_(inflight=st.inflight - {key})
        if res not in ("allow", "dup") and j.get(key) == "claiming":
            s2 = s2.with_(journal=freeze({**j, key: "refused"}))
        out.append((f"net deliver claim {sp[:2]}{a}", (s2, "allow")))
    if st.x_up:
        if st.crashes < bound("crashes"):
            out.append(("X crash", (st.with_(x_up=False, x_seen=frozenset(), checked=(), crashes=st.crashes + 1), "allow")))
        for sp in spaces:
            S = state_of(st.logs, sp)
            eff = S["effects"].get(PR) if S else None
            for a in range(1, bound("attempts") + 1):
                key = (sp, a); cid = claim_id(sp, a)
                if key not in j and eff and eff["phase"] == "ready" and len(eff["claims"]) + 1 == a:
                    out.append((f"X journal claiming {sp[:2]}{a}", (st.with_(journal=freeze({**j, key: "claiming"}),
                                                                             wset=freeze({**dict(st.wset), key: S["policy"]["launchWindow"]})), "allow")))
                if j.get(key) == "claiming" and key not in sent:
                    s2 = st.with_(sent=freeze({**sent, key: exec_now(st)}), inflight=st.inflight | {key})
                    if not bound("delay"):                  # immediate delivery: send and home append in one step
                        s3, res = append(s2, claim_entry(s2, sp, a)); s2 = s3.with_(inflight=s2.inflight - {key})
                        if res not in ("allow", "dup"): s2 = s2.with_(journal=freeze({**j, key: "refused"}))
                    out.append((f"X send claim {sp[:2]}{a}", (s2, "allow")))
                if j.get(key) in ("claiming", "launching") and key not in st.x_seen and rec_index(st, sp, cid) >= 0:
                    out.append((f"X receives receipt {sp[:2]}{a}", (st.with_(x_seen=st.x_seen | {key}), "allow")))
                if j.get(key) == "claiming" and key in st.x_seen:
                    if MUT == "no-launch-journal":
                        if deadline_ok(st, sp, a):
                            out.append((f"X launch (no journal) {sp[:2]}{a}", (st.with_(world=freeze({**w, key: w.get(key, 0) + 1}),
                                         launch_at=freeze({**la, key: len(dict(st.logs)[sp])})), "allow")))
                    else:
                        out.append((f"X journal launching {sp[:2]}{a}", (st.with_(journal=freeze({**j, key: "launching"})), "allow")))
                if j.get(key) == "launching" and key in st.x_seen and key not in chk and not w.get(key):
                    if MUT == "no-window-check" or deadline_ok(st, sp, a):
                        out.append((f"X deadline ok {sp[:2]}{a}", (st.with_(checked=freeze({**chk, key: st.time})), "allow")))
                    else:
                        out.append((f"X journal expired {sp[:2]}{a}", (st.with_(journal=freeze({**j, key: "expired"})), "allow")))
                if key in chk:
                    c2 = dict(chk); del c2[key]
                    out.append((f"X launch {sp[:2]}{a}", (st.with_(checked=freeze(c2), world=freeze({**w, key: 1}),
                                                                 launch_at=freeze({**la, key: len(dict(st.logs)[sp])})), "allow")))
                if j.get(key) == "expired":
                    s2, _ = append(st, E(f"o-{sp[:2]}-{a}-late", sp, "effect.outcome", X, {"claim": cid, "result": "failed", "started": "no"}))
                    out.append((f"X report expired {sp[:2]}{a}", (s2.with_(journal=freeze({**j, key: "done"})), "allow")))
                if w.get(key) and j.get(key) in ("launching", "claiming") and key in st.x_seen:
                    for r in ("succeeded", "failed"):
                        s2, res = append(st, E(f"o-{sp[:2]}-{a}-{r}", sp, "effect.outcome", X, {"claim": cid, "result": r, "started": "yes"}))
                        if res != "allow":   # abandoned meanwhile → the executor states the fact via resolve
                            s2, res = append(st, E(f"rx-{sp[:2]}-{a}-{r}", sp, "effect.resolve", X, {"proposal": PR, "result": r}))
                            if res == "allow" and r == "failed": s2 = s2.with_(false_resolves=s2.false_resolves + 1)  # it did run
                        out.append((f"X report {sp[:2]}{a} {r}", (s2.with_(journal=freeze({**j, key: "done"})), "allow")))
    elif True:
        # restart + recovery (X-06), atomic
        s = st.with_(x_up=True)
        for (sp, a), phase in sorted(j.items()):
            cid = claim_id(sp, a)
            jj = dict(s.journal)
            if phase == "claiming":
                s, res = append(s, claim_entry(s, sp, a))                  # finish the claim's append (R-06)
                if res in ("allow", "dup"):
                    if MUT == "launch-after-restart":
                        ww, ll = dict(s.world), dict(s.launch_at)
                        ww[(sp, a)] = ww.get((sp, a), 0) + 1; ll[(sp, a)] = len(dict(s.logs)[sp])
                        jj[(sp, a)] = "launching"
                        s = s.with_(journal=freeze(jj), world=freeze(ww), launch_at=freeze(ll), x_seen=s.x_seen | {(sp, a)})
                        continue
                    s, _ = append(s, E(f"o-{sp[:2]}-{a}-rec", sp, "effect.outcome", X, {"claim": cid, "result": "failed", "started": "no"}))
                jj = dict(s.journal); jj[(sp, a)] = "done"; s = s.with_(journal=freeze(jj), inflight=s.inflight - {(sp, a)})
            elif phase == "expired":
                s, _ = append(s, E(f"o-{sp[:2]}-{a}-late", sp, "effect.outcome", X, {"claim": cid, "result": "failed", "started": "no"}))
                jj[(sp, a)] = "done"; s = s.with_(journal=freeze(jj))
            elif phase == "launching":
                s, _ = append(s, E(f"o-{sp[:2]}-{a}-unk", sp, "effect.outcome", X, {"claim": cid, "result": "unknown", "started": "unknown"}))
                jj[(sp, a)] = "done"; s = s.with_(journal=freeze(jj))
        out.append(("X restart+recover", (s, "allow")))
    return [(lbl, s2) for lbl, (s2, res) in out if res == "allow" and s2.key() != st.key()]

def check(st):
    w, la, logs = dict(st.world), dict(st.launch_at), dict(st.logs)
    for (sp, a), n in w.items():
        cid = claim_id(sp, a)
        if n > 1: return f"AtMostOneLaunchPerClaim: {sp}#{a} launched {n}×"
        ci = rec_index(st, sp, cid)
        if ci < 0 or ci >= la[(sp, a)]: return f"NoLaunchWithoutReceipt: {sp}#{a}"
        for i, x in enumerate(logs[sp]):
            e = json.loads(x[0])
            if e["kind"] == "effect.outcome" and e["body"]["claim"] == cid:
                if e["author"] == O and i < la[(sp, a)]: return f"NoLaunchAfterAbandon: {sp}#{a}"
                if e["body"]["started"] == "no": return f"HonestNotStarted: {sp}#{a} reported not started but launched"
            if i < ci and e["kind"] == "member.set" and e["body"] == {"actor": P, "role": "none"}:
                return f"RevokedBeforeClaimNeverLaunches: {sp}#{a} (membership)"
        if json.loads(logs[sp][ci][1])["profiles"][P]["keys"]["k1"]["status"] == "revoked":
            return f"RevokedBeforeClaimNeverLaunches: {sp}#{a} (key)"
    for sp, log in logs.items():
        for x in log:
            e = json.loads(x[0])
            if e["kind"] == "effect.outcome" and e["body"]["started"] == "yes":
                a = CLAIM_ATTEMPT[e["body"]["claim"]]
                if not w.get((sp, a)): return f"HonestStarted: {sp}#{a} reported started but never launched"
    if sum(w.values()) > 1 + st.false_resolves:
        return f"UnknownNeverSilentlyRetried: {sum(w.values())} launches, {st.false_resolves} false resolve(failed)"
    return None

def explore():
    patch()
    _fold_cache.clear(); _decide_cache.clear()
    starts = init_states()
    seen = {s.key(): None for s in starts}
    q = deque(starts)
    launched = False
    while q:
        s = q.popleft()
        launched = launched or any(n for _, n in s.world)
        err = check(s)
        if err:
            trace, k = [], s.key()
            while seen[k] is not None:
                pk, lbl = seen[k]; trace.append(lbl); k = pk
            return len(seen), err, [f"clocks (executor offset ds, home rate, executor rate) = {s.clk}"] + list(reversed(trace))
        for lbl, n in successors(s):
            if n.key() not in seen:
                seen[n.key()] = (s.key(), lbl)
                q.append(n)
    if not launched:
        return len(seen), "VACUOUS: no reachable state launches anything; the model is not exercising the protocol", []
    return len(seen), None, None

def main():
    global MUT, SCENARIO
    ap = argparse.ArgumentParser()
    ap.add_argument("--mutation", choices=sorted(MUTATIONS))
    ap.add_argument("--all-mutations", action="store_true")
    ap.add_argument("--scenario", choices=["timing", "drift", "single", "all"], default="all")
    args = ap.parse_args()
    if args.all_mutations:
        ok = True
        for m in sorted(MUTATIONS):
            sc = {"deadline-from-receipt-time": "timing", "no-window-check": "timing", "window-not-pinned": "timing",
                  "executor-uses-current-window": "timing", "no-quantization-margin": "timing", "one-sided-drift-margin": "drift"}.get(m, "single")
            r = subprocess.run([sys.executable, __file__, "--mutation", m, "--scenario", sc], capture_output=True, text=True, encoding="utf-8")
            caught = r.returncode == 1 and "VIOLATION" in r.stdout and "VACUOUS" not in r.stdout
            print(f"{'caught ' if caught else 'MISSED '} {m:28} {r.stdout.strip().splitlines()[0] if r.stdout.strip() else r.stderr[-300:]}")
            ok &= caught
        print("all mutations caught" if ok else "SOME MUTATIONS MISSED")
        return 0 if ok else 2
    MUT = args.mutation
    for sc in (["timing", "drift", "single"] if args.scenario == "all" else [args.scenario]):
        SCENARIO = sc
        n, err, trace = explore()
        if err:
            print(f"VIOLATION [{sc}] after {n} states: {err}")
            print("  trace: " + " → ".join(trace))
            return 1
        b = BOUNDS[sc]
        extra = {"timing": f"executor clock offsets {[c[0] / DS for c in b['clocks']]} s, arbitrary claim delay, "
                           f"stall<={STALL}s on the executor clock, whole-second receipts, launchWindow changed {W}→{W_SHORT} or {W_LONG} s at any time, abandon",
                 "drift": f"clocks {b['clocks']} (home 1.2x fast, executor 0.8x slow: drift exaggerated to ±20%), arbitrary claim delay, stall<={STALL}s on the executor clock, whole-second receipts, abandon",
                 "single": "revocation of membership and key, cancel, abandon"}[sc]
        print(f"[{sc}] {n} states explored; ALL PROPERTIES HOLD (attempts<={b['attempts']}, crashes<={b['crashes']}, "
              f"ticks<={b['ticks']} of {'/'.join('+%gs' % (t / DS) for t in b['ticksizes'])}; {extra})", flush=True)
    return 0

if __name__ == "__main__":
    sys.exit(main())

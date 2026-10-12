"""Fold totality and invariants under random entry sequences (the generator ref/fuzz_diff.py also uses).

World: an owner O, members M and PR, a guest G, two operators OP and OP2 (human members, separate from the owner), a service X and
agents BOT and BOT2 whose operators are OP, OP and OP2. Operators confirm and withdraw, keys are revoked and restored, owners
force guarding on and off, releases are signed with stale `seen`, and the fitting author (an assignee closing its own task, an
executor claiming, an operator releasing) is chosen more often than chance, so held agents finish their own work.

For every entry decide() allows, fold() must not raise, must not mutate its inputs, and the new state must satisfy:
  · every budget amount (space remainder, task remainders) is ≥ 0
  · the space remainder never exceeds the space budget, a task's remainder never exceeds its budget
  · only allowed phases occur, and a final phase never changes
  · decide() never raises
  · no effect has more than MAX_ATTEMPTS claims; `lastOutside` never shrinks; every held agent has a `lastOutside`
  · guard_status never raises and agrees with the state's sets; its `guard` is the operator in the pinned profile, confirmed
  · an entry that touches an effect leaves it no unconsumed approval that has expired, and no more than MAX_OPEN_APPROVALS + 1 + 3
  · the JCS size of every state item (effect, task, session, role history, ...) is within a bound derived from the grammar's
    maximum lengths and the Policy constants, so an item that cannot fit in a state page cannot occur
Usage: python ref/fuzz_fold.py [seeds] [steps]      (default 2000 seeds × 200 steps)
"""
import copy, hashlib, os, random, sys
from datetime import datetime, timedelta, timezone
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import policy as P

S = "fz:a.example"
O, M, PR, X, G, BOT = "@o:a.example", "@m:a.example", "@p:b.example", "@x:c.example", "@g:a.example", "@bot:b.example"
OP, OP2, BOT2 = "@op:b.example", "@op2:d.example", "@bot2:d.example"
ACTORS = [O, M, PR, X, G, BOT, OP, OP2, BOT2]
OPERATOR = {BOT: OP, BOT2: OP2, X: OP}
AGENTS = [BOT, BOT2, X]
BASE = datetime(2026, 10, 10, 12, 0, 0, tzinfo=timezone.utc)
PHASES = {"open", "claimed", "unknown", "succeeded", "failed", "cancelled"}

def _sessions(r):
    L = ["guest", "member", "owner", "nobody"]
    w = r.choice([0, 0, 1])
    return {"watch": L[w], "read": L[r.choice([w, 1, 2])] if w <= 1 else L[2], "steer": L[r.choice([w, 1, 1, 2, 3])] if w <= 1 else L[2],
            "modes": r.sample(["auto", "guarded"], r.randint(1, 2)), "forceGuarded": r.random() < .4}

def H(x): return hashlib.sha256(x.encode()).hexdigest()

def policy(r):
    return {"visibility": "members", "join": r.choice(["invite", "open"]), "membersMayInvite": r.random() < .5,
            "allowAgents": r.choice(["any", "with-operator"]),
            "approvals": r.choice([[], [{"action": "*", "approvers": ["owner"]}], [{"action": "pay", "approvers": ["owner", M]}],
                                   [{"action": "*", "approvers": ["member"]}]]),
            "separateApprover": r.random() < .3, "budget": r.choice([None, None, None, {"unit": "USD", "amount": r.randint(0, 200)}]),
            "maxDelegationDepth": r.randint(0, 4), "minDurability": "local", "launchWindow": r.choice([15, 30, 60]),
            "extensionsMinRole": r.choice(["guest", "member", "owner"]),
            "sessions": _sessions(r), "servers": {"deny": []}}

def profile(a, revoked=False, unconfirmed=False):
    kind = "service" if a == X else "agent" if a in (BOT, BOT2) else "human"
    return {"actor": a, "seq": 1, "actorKind": kind, "operatorConfirmed": kind != "human" and not unconfirmed,
            **({"operator": OPERATOR[a]} if kind != "human" else {}),
            "keys": {"k1": {"publicKey": "pk", "status": "revoked" if revoked else "valid"}, "k2": {"publicKey": "pk2", "status": "valid"}}}

def flip(r, revoked, unconf):
    """With small probability toggle one actor's key revocation or one agent's operator confirmation; returns the actors whose view changed."""
    out = []
    if r.random() < .03: a = r.choice(sorted(revoked) if revoked and r.random() < .75 else ACTORS); revoked ^= {a}; out.append(a)      # revoked keys come back, as a rotation would
    if r.random() < .06: a = r.choice(sorted(unconf) if unconf and r.random() < .6 else AGENTS); unconf ^= {a}; out.append(a)
    return out

def profiles_of(revoked, unconf): return {a: profile(a, a in revoked, a in unconf) for a in ACTORS}

def fmt(t): return (BASE + timedelta(seconds=t)).strftime("%Y-%m-%dT%H:%M:%SZ")

def random_entry(r, state, n):
    ids = list(state["entries"]) if state else []
    tasks, effects = list(state["tasks"]) if state else [], list(state["effects"]) if state else []
    sessions = list(state["sessions"]) if state else []
    claims = [c["id"] for e in (state["effects"].values() if state else []) for c in e["claims"]]
    pick = lambda xs, fallback: r.choice(xs) if xs and r.random() < .9 else fallback
    unit = "USD" if r.random() < .85 else "EUR"
    cost = {"unit": unit, "amount": r.randint(0, 120)}
    kind = r.choice(["member.set", "member.set", "message", "task", "task", "task", "task.update", "task.update", "task.cancel", "effect.propose",
                     "effect.propose", "effect.propose", "effect.approve", "effect.approve", "effect.approve", "effect.claim", "effect.claim", "effect.claim", "effect.cancel", "effect.outcome",
                     "effect.outcome", "effect.resolve", "policy.set", "checkpoint", "redact", "space.move", "x.ext",
                     "session.open", "session.open", "session.close", "session.input", "session.input", "session.input",
                     "agent.release", "agent.release", "agent.release"])
    pid, sid, tid = pick(effects, H("nope")), pick(sessions, H("nope")), pick(tasks, H("nope"))
    body = {
        "member.set": lambda: {"actor": r.choice(ACTORS), "role": r.choice(["owner", "member", "member", "member", "guest", "banned", "none"])},
        "message": lambda: {"to": [r.choice(ACTORS)]},
        "task": lambda: dict({"to": r.sample(ACTORS, r.choice([1, 1, 2]))}, **({"budget": cost} if r.random() < .4 else {}),
                             **({"parent": tid} if r.random() < .4 else {})),
        "task.update": lambda: {"task": tid, "state": r.choice(["accepted", "progress", "done", "failed", "declined", "cancelled"])},
        "task.cancel": lambda: {"task": tid},
        "effect.propose": lambda: dict({"executor": r.choice([X, X, M, BOT, BOT, BOT2]), "action": r.choice(["pay", "push"]), "planDigest": H("plan"),
                                         "repeatable": r.random() < .3}, **({"cost": cost} if r.random() < .6 else {}),
                                       **({"task": tid} if r.random() < .4 else {})),
        "effect.approve": lambda: dict({"proposal": pid, "planDigest": H("plan")},
                                       **({"expiresAt": r.choice([fmt(r.randint(0, 3000)), "2026-13-45T99:99:99Z", "2026-02-30T00:00:00Z"])} if r.random() < .3 else {})),
        "effect.claim": lambda: {"proposal": pid, "instance": "k1",
                                 "attempt": (len(state["effects"][pid]["claims"]) + 1 if pid in state["effects"] and r.random() < .85 else r.randint(1, 3)),
                                 "window": state["policy"]["launchWindow"] if r.random() < .9 else r.choice([15, 30, 60])},
        "effect.cancel": lambda: {"proposal": pid},
        "effect.outcome": lambda: dict({"claim": pick(claims, H("nope")), "result": r.choice(["succeeded", "failed", "cancelled", "unknown"]),
                                        "started": r.choice(["yes", "no", "unknown"])}, **({"cost": cost} if r.random() < .4 else {})),
        "effect.resolve": lambda: dict({"proposal": pid, "result": r.choice(["succeeded", "failed"])}, **({"cost": cost} if r.random() < .3 else {})),
        "policy.set": lambda: {"policy": policy(r)},
        "checkpoint": lambda: {"through": r.randint(0, 5 + n)},
        "redact": lambda: {"target": pick(ids, H("nope"))},
        "space.move": lambda: {"to": "b.example", "accept": {"keyId": "h1", "sig": "s"}},
        "x.ext": lambda: {"anything": r.randint(0, 9)},
        "session.open": lambda: dict({"steerMode": r.choice(["auto", "guarded", "guarded"])}, **({"task": tid} if r.random() < .3 else {})),
        "session.close": lambda: {"session": sid},
        "session.input": lambda: {"session": sid},
        "agent.release": lambda: dict({"agent": r.choice(list(state["held"]) if state and state["held"] and r.random() < .6 else AGENTS)},
                                      **({"unguard": True} if r.random() < .35 else {})),
    }[kind]()
    author = r.choice(ACTORS + [O, O, X, X])
    if r.random() < (.9 if kind in ("effect.claim", "effect.outcome", "effect.resolve") else .65):                                   # the fitting author: held agents finish their own work, operators release and approve
        st = state or {"tasks": {}, "effects": {}, "sessions": {}}
        if kind in ("task.update", "task.cancel") and body["task"] in st["tasks"]:
            t = st["tasks"][body["task"]]; author = r.choice(t["to"]) if kind == "task.update" else t["author"]
        elif kind in ("effect.claim", "effect.resolve", "effect.cancel") and body["proposal"] in st["effects"]:
            e = st["effects"][body["proposal"]]; author = e["executor"] if kind != "effect.cancel" else r.choice([e["proposer"], e["executor"], OP, OP2])
        elif kind == "effect.outcome":
            for e in st["effects"].values():
                if any(c["id"] == body["claim"] for c in e["claims"]): author = e["executor"]
        elif kind == "effect.approve":
            e = st["effects"].get(body["proposal"])
            author = r.choice([OP, OP2, O, M, PR] + ([OPERATOR[w] for w in (e["proposer"], e["executor"]) if w in OPERATOR] if e else []))
        elif kind == "agent.release": author = OPERATOR[body["agent"]] if r.random() < .8 else r.choice([O, body["agent"]])
        elif kind == "session.input" and body["session"] in st["sessions"]:
            author = r.choice([a for a in ACTORS if a != st["sessions"][body["session"]]["actor"]] + [st["sessions"][body["session"]]["actor"]])
        elif kind == "session.open": author = r.choice(AGENTS)
        elif kind == "task": author = r.choice([PR, M, O, OP, BOT])
        elif kind == "effect.propose": author = r.choice([PR, M, O, OP, BOT, BOT2])
    head = state["head"] if state else 0
    seen = r.choice([0, head, head, head, r.randint(0, head), max(0, head - r.randint(1, 4))]) if kind == "agent.release" else r.choice([0, head, head, r.randint(0, head)])
    return {"id": H(f"e{n}"), "v": 1, "space": S, "kind": kind, "author": author, "keyId": "k1", "body": body,
            "contentHash": "", "refs": [], "seen": seen, "nonce": f"n{n}", "time": "2026-10-10T12:00:00Z"}

def setup(r, state, ctx):
    """A populated room, so the effect lifecycle is reachable: yields the member.set entries to apply."""
    for i, a in enumerate([M, PR, X, BOT, OP, OP2, BOT2, G]):
        yield {"id": H(f"setup{i}"), "v": 1, "space": S, "kind": "member.set", "author": O, "keyId": "k1",
               "body": {"actor": a, "role": "guest" if a == G else "member"}, "contentHash": "", "refs": [], "seen": 0,
               "nonce": f"s{i}", "time": "2026-10-10T12:00:00Z"}

def policy_cut(s, prev):
    return prev and prev["policy"]["budget"] and s["policy"]["budget"] and s["policy"]["budget"]["amount"] < prev["policy"]["budget"]["amount"]

# ── size bounds, derived from the grammar's maximum lengths and the Policy constants ──
def _worst():
    ACT = "@" + "a" * 64 + ":" + "b" * 253                  # the longest actor id (§2)
    ID, KEY, TIME = "f" * 64, "k" * 64, "2026-10-10T12:00:00Z"
    BUD = {"unit": "u" * 32, "amount": 2 ** 53 - 1}
    approval = {"id": ID, "author": ACT, "keyId": KEY, "expiresAt": TIME, "consumedBy": ID}
    claim = {"id": ID, "attempt": 64, "instance": KEY, "time": TIME, "window": 3600, "approval": ID, "guardApprovals": [ID, ID], "reserved": BUD,
             "source": ["task", ID], "settled": True, "outcome": "unknown", "started": "unknown"}
    effect = {"proposer": ACT, "proposerKey": KEY, "executor": ACT, "action": "a" * 128, "planDigest": ID, "seq": 2 ** 53 - 1, "cost": BUD,
              "repeatable": False, "task": ID, "phase": "unknown", "cancelRequested": False, "expiresAt": TIME,
              "approvals": [approval] * (P.MAX_OPEN_APPROVALS + 1 + 3), "claims": [claim] * P.MAX_ATTEMPTS}
    task = {"author": ACT, "to": [ACT] * 64, "open": True, "state": "progress", "cancelRequested": False, "depth": 16, "seq": 2 ** 53 - 1,
            "parent": ID, "budget": BUD, "left": BUD}
    session = {"actor": ACT, "opened": TIME, "steerMode": "guarded", "steers": 2 ** 53 - 1, "task": ID}
    history = [{"from": 2 ** 53 - 1, "to": 2 ** 53 - 1}] * P.ROLE_INTERVALS
    return {"effects": len(P.jcs(effect)), "tasks": len(P.jcs(task)), "sessions": len(P.jcs(session)), "roleHistory": len(P.jcs(history))}
BOUND = _worst()

def check_state(s, prev, ctx, e=None):
    errs = []
    for x, c in (prev["lastOutside"] if prev else {}).items():
        if s["lastOutside"].get(x, 0) < c: errs.append(f"lastOutside[{x}] shrank")
    for x in s["held"]:
        if x not in s["lastOutside"]: errs.append(f"{x} is held but has no lastOutside")
    for a in ACTORS:
        st = P.guard_status(s, ctx, a)
        p = ctx["profiles"][a]
        if st["held"] != (a in s["held"]) or st["guarded"] != (a in s["guarded"]): errs.append(f"guard_status disagrees with the sets for {a}: {st}")
        if ("guard" in st) != (p["actorKind"] != "human" and p["operatorConfirmed"]) or st.get("guard", p.get("operator")) != p.get("operator"):
            errs.append(f"guard_status guard is not the confirmed pinned operator for {a}: {st}")
    if s["budgetLeft"] is not None:
        if s["budgetLeft"]["amount"] < 0 and not (prev and prev["budgetLeft"] is not None and prev["budgetLeft"]["amount"] < 0) and not policy_cut(s, prev):
            errs.append("space remainder went negative without a budget cut")
        if s["policy"]["budget"] and s["budgetLeft"]["amount"] > s["policy"]["budget"]["amount"]: errs.append("space remainder > budget")
    for tid, t in s["tasks"].items():
        if t.get("left"):
            if t["left"]["amount"] < 0: errs.append(f"task {tid[:6]} remainder < 0")
            if t["left"]["amount"] > t["budget"]["amount"]: errs.append(f"task {tid[:6]} remainder > budget")
        if len(P.jcs(t)) > BOUND["tasks"]: errs.append("a task exceeds its size bound")
    for pid, eff in s["effects"].items():
        if eff["phase"] not in PHASES: errs.append(f"bad phase {eff['phase']}")
        if len(eff["claims"]) > P.MAX_ATTEMPTS: errs.append("more claims than MAX_ATTEMPTS")
        if len(eff["approvals"]) > P.MAX_OPEN_APPROVALS + 1 + 3: errs.append(f"{len(eff['approvals'])} approvals on one effect")
        if len(P.jcs(eff)) > BOUND["effects"]: errs.append("an effect exceeds its size bound")
        old = prev["effects"].get(pid) if prev else None
        if old and old["phase"] in P.FINAL and eff["phase"] != old["phase"]: errs.append(f"final phase changed {old['phase']}→{eff['phase']}")
        if e is not None and e["kind"].startswith("effect.") and (pid == e["id"] or pid == e["body"].get("proposal")
                                                                  or any(c["id"] == e["body"].get("claim") for c in eff["claims"])):
            if any(not a.get("consumedBy") and a.get("expiresAt") and P.ts(a["expiresAt"]) <= P.ts(ctx["time"]) for a in eff["approvals"]):
                errs.append("an expired approval survived an entry that touched its effect")
    for sid, se in s["sessions"].items():
        if len(P.jcs(se)) > BOUND["sessions"]: errs.append("a session exceeds its size bound")
    for a, h in s["roleHistory"].items():
        if len(h) > P.ROLE_INTERVALS or len(P.jcs(h)) > BOUND["roleHistory"]: errs.append("a role history exceeds its bound")
    return errs

CLAIMS = [0]

def run(seed, steps):
    r = random.Random(seed)
    t, revoked, unconf = 0, set(), set()
    create = {"id": H("create"), "v": 1, "space": S, "kind": "space.create", "author": O, "keyId": "k1", "body": {"policy": policy(r)},
              "contentHash": "", "refs": [], "seen": 0, "nonce": "c", "time": "2026-10-10T12:00:00Z"}
    ctx = {"time": fmt(0), "profiles": profiles_of(revoked, unconf)}
    state = P.fold(None, {"entry": create, "content": None, "receipt": {"seq": 1, "time": ctx["time"]}}, ctx)
    for ms in setup(r, state, ctx):
        if P.decide(state, ms, ctx)["allow"]:
            state = P.fold(state, {"entry": ms, "content": None, "receipt": {"seq": state["head"] + 1, "time": ctx["time"]}}, ctx)
    allowed = 0
    for n in range(steps):
        t += r.choice([0, 1, 5, 20, 45])
        flip(r, revoked, unconf)
        ctx = {"time": fmt(t), "profiles": profiles_of(revoked, unconf)}
        e = random_entry(r, state, n)
        before, ctx0 = copy.deepcopy(state), copy.deepcopy(ctx)
        try:
            d = P.decide(state, e, ctx)
        except Exception as ex:
            return f"seed {seed} step {n}: decide raised {type(ex).__name__}: {ex} on {e['kind']}", allowed
        if state != before or ctx != ctx0: return f"seed {seed} step {n}: decide mutated input", allowed
        if not d["allow"]: continue
        try:
            new = P.fold(state, {"entry": e, "content": None, "receipt": {"seq": state["head"] + 1, "time": ctx["time"]}}, ctx)
        except Exception as ex:
            return f"seed {seed} step {n}: fold raised {type(ex).__name__}: {ex} on allowed {e['kind']} {e['body']}", allowed
        if state != before: return f"seed {seed} step {n}: fold mutated input", allowed
        errs = check_state(new, state, ctx, e)
        if errs: return f"seed {seed} step {n}: after {e['kind']} {e['body']}: {errs}", allowed
        state = new; allowed += 1
        if e["kind"] == "effect.claim": CLAIMS[0] += 1
    return None, allowed

def main():
    seeds = int(sys.argv[1]) if len(sys.argv) > 1 else 2000
    steps = int(sys.argv[2]) if len(sys.argv) > 2 else 200
    total = 0
    for seed in range(seeds):
        err, allowed = run(seed, steps)
        total += allowed
        if err:
            print("FAIL", err); return 1
    print(f"{seeds} seeds × {steps} steps: {total} allowed entries folded ({CLAIMS[0]} claims); decide/fold total, pure, budgets within bounds, "
          f"final phases stable, lastOutside monotone, expired approvals dropped, every state item within its derived size bound {BOUND}")
    return 0

if __name__ == "__main__":
    sys.exit(main())

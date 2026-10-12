"""Fold totality and budget invariants under random entry sequences.

For every entry decide() allows, fold() must not raise, must not mutate its inputs, and the new state must satisfy:
  · every budget amount (space remainder, task remainders) is ≥ 0
  · the space remainder never exceeds the space budget, a task's remainder never exceeds its budget
  · only allowed phases occur, and a final phase never changes
  · decide() never raises
  · no effect has more than MAX_ATTEMPTS claims, `cleared` never shrinks, and guard_status never raises and agrees with `held`
Usage: python ref/fuzz_fold.py [seeds] [steps]      (default 2000 seeds × 200 steps)
"""
import copy, hashlib, os, random, sys
from datetime import datetime, timedelta, timezone
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import policy as P

S = "fz:a.example"
O, M, PR, X, G, BOT = "@o:a.example", "@m:a.example", "@p:b.example", "@x:c.example", "@g:a.example", "@bot:b.example"
ACTORS = [O, M, PR, X, G, BOT]
BASE = datetime(2026, 10, 10, 12, 0, 0, tzinfo=timezone.utc)
PHASES = {"awaiting-approval", "ready", "claimed", "unknown", "orphaned", "succeeded", "failed", "cancelled"}

def _sessions(r):
    L = ["guest", "member", "owner", "nobody"]
    w = r.randrange(4)
    return {"watch": L[w], "read": L[r.randrange(w, 4)], "steer": L[r.randrange(w, 4)],
            "modes": r.sample(["auto", "guarded"], r.randint(1, 2)), "forceGuarded": r.random() < .4}

def H(x): return hashlib.sha256(x.encode()).hexdigest()

def policy(r):
    return {"visibility": "members", "join": r.choice(["invite", "open"]), "membersMayInvite": r.random() < .5,
            "allowAgents": r.choice(["any", "with-operator"]),
            "approvals": r.choice([[], [{"action": "*", "approvers": ["owner"]}], [{"action": "pay", "approvers": ["owner", M]}]]),
            "separateApprover": r.random() < .3, "budget": r.choice([None, {"unit": "USD", "amount": r.randint(0, 200)}]),
            "maxDelegationDepth": r.randint(0, 4), "minDurability": "local", "launchWindow": r.choice([15, 30, 60]),
            "extensionsMinRole": r.choice(["guest", "member", "owner"]),
            "sessions": _sessions(r), "servers": {"deny": []}}

def profile(a, revoked=False):
    kind = "service" if a == X else "agent" if a == BOT else "human"
    return {"actor": a, "seq": 1, "actorKind": kind, "operatorConfirmed": kind != "human", **({"operator": O} if kind != "human" else {}),
            "keys": {"k1": {"publicKey": "pk", "status": "revoked" if revoked else "valid"}, "k2": {"publicKey": "pk2", "status": "valid"}}}

def random_entry(r, state, n):
    ids = list(state["entries"]) if state else []
    tasks, effects = list(state["tasks"]) if state else [], list(state["effects"]) if state else []
    claims = [c["id"] for e in (state["effects"].values() if state else []) for c in e["claims"]]
    pick = lambda xs, fallback: r.choice(xs) if xs and r.random() < .9 else fallback
    unit = "USD" if r.random() < .85 else "EUR"
    cost = {"unit": unit, "amount": r.randint(0, 120)}
    kind = r.choice(["member.set", "member.set", "message", "task", "task", "task.update", "task.cancel", "effect.propose",
                     "effect.propose", "effect.approve", "effect.claim", "effect.claim", "effect.cancel", "effect.outcome",
                     "effect.outcome", "effect.resolve", "policy.set", "checkpoint", "redact", "space.move", "x.ext", "session.open", "session.close", "session.input", "agent.release"])
    body = {
        "member.set": lambda: {"actor": r.choice(ACTORS), "role": r.choice(["owner", "member", "member", "guest", "banned", "none"])},
        "message": lambda: {"to": [r.choice(ACTORS)]},
        "task": lambda: dict({"to": [r.choice(ACTORS)]}, **({"budget": cost} if r.random() < .6 else {}),
                             **({"parent": pick(tasks, H("nope"))} if r.random() < .5 else {})),
        "task.update": lambda: {"task": pick(tasks, H("nope")), "state": r.choice(["accepted", "progress", "done", "failed", "declined", "cancelled"])},
        "task.cancel": lambda: {"task": pick(tasks, H("nope"))},
        "effect.propose": lambda: dict({"executor": r.choice([X, X, M]), "action": r.choice(["pay", "push"]), "planDigest": H("plan"),
                                         "repeatable": r.random() < .3}, **({"cost": cost} if r.random() < .8 else {}),
                                       **({"task": pick(tasks, H("nope"))} if r.random() < .4 else {}),
),
        "effect.approve": lambda: dict({"proposal": pick(effects, H("nope")), "planDigest": H("plan")},
                                       **({"expiresAt": r.choice([(BASE + timedelta(seconds=r.randint(0, 3000))).strftime("%Y-%m-%dT%H:%M:%SZ"),
                                                                  "2026-13-45T99:99:99Z", "2026-02-30T00:00:00Z"])} if r.random() < .3 else {})),
        "effect.claim": lambda: (lambda pid: {"proposal": pid, "instance": "k1",
                                 "attempt": (len(state["effects"][pid]["claims"]) + 1 if pid in state["effects"] and r.random() < .85 else r.randint(1, 3)),
                                 "window": state["policy"]["launchWindow"] if r.random() < .9 else r.choice([15, 30, 60])})(pick(effects, H("nope"))),
        "effect.cancel": lambda: {"proposal": pick(effects, H("nope"))},
        "effect.outcome": lambda: dict({"claim": pick(claims, H("nope")), "result": r.choice(["succeeded", "failed", "cancelled", "unknown"]),
                                        "started": r.choice(["yes", "no", "unknown"])}, **({"cost": cost} if r.random() < .4 else {})),
        "effect.resolve": lambda: dict({"proposal": pick(effects, H("nope")), "result": r.choice(["succeeded", "failed"])},
                                       **({"cost": cost} if r.random() < .3 else {})),
        "policy.set": lambda: {"policy": policy(r)},
        "checkpoint": lambda: {"through": r.randint(0, 5 + n)},
        "redact": lambda: {"target": pick(ids, H("nope"))},
        "space.move": lambda: {"to": "b.example"},
        "x.ext": lambda: {"anything": r.randint(0, 9)},
        "session.open": lambda: dict({"steerMode": r.choice(["auto", "guarded"])}, **({"task": pick(tasks, H("nope"))} if r.random() < .3 else {})),
        "session.close": lambda: {"session": pick(list(state["sessions"]), H("nope"))},
        "session.input": lambda: {"session": pick(list(state["sessions"]), H("nope"))},
        "agent.release": lambda: dict({"agent": r.choice(ACTORS)}, **({"unguard": True} if r.random() < .3 else {})),
    }[kind]()
    author = r.choice(ACTORS + [O, O, X, X])
    head = state["head"] if state else 0
    return {"id": H(f"e{n}"), "v": 1, "space": S, "kind": kind, "author": author, "keyId": "k1", "body": body,
            "contentHash": "", "refs": [], "seen": r.choice([0, head, head, r.randint(0, head)]), "nonce": f"n{n}", "time": "2026-10-10T12:00:00Z"}

def policy_cut(s, prev):
    return prev and prev["policy"]["budget"] and s["policy"]["budget"] and s["policy"]["budget"]["amount"] < prev["policy"]["budget"]["amount"]

def check_state(s, prev, ctx):
    errs = []
    for x, c in (prev["cleared"] if prev else {}).items():
        if s["cleared"].get(x, 0) < c: errs.append(f"cleared[{x}] shrank")
    for a in ACTORS:
        st = P.guard_status(s, ctx, a)
        if st["held"] != (a in s["held"]) or (a in s["held"] and not st.get("guard") and a not in s["guarded"] and not st["forced"]): errs.append(f"guard_status disagrees with held for {a}: {st}")
    if s["budgetLeft"] is not None:
        if s["budgetLeft"]["amount"] < 0 and not (prev and prev["budgetLeft"] is not None and prev["budgetLeft"]["amount"] < 0) and not policy_cut(s, prev):
            errs.append("space remainder went negative without a budget cut")
        if s["policy"]["budget"] and s["budgetLeft"]["amount"] > s["policy"]["budget"]["amount"]: errs.append("space remainder > budget")
    for tid, t in s["tasks"].items():
        if t.get("left"):
            if t["left"]["amount"] < 0: errs.append(f"task {tid[:6]} remainder < 0")
            if t["left"]["amount"] > t["budget"]["amount"]: errs.append(f"task {tid[:6]} remainder > budget")
    for pid, e in s["effects"].items():
        if e["phase"] not in PHASES: errs.append(f"bad phase {e['phase']}")
        if len(e["claims"]) > P.MAX_ATTEMPTS: errs.append("more claims than MAX_ATTEMPTS")
        old = prev["effects"].get(pid) if prev else None
        if old and old["phase"] in P.FINAL and e["phase"] != old["phase"]: errs.append(f"final phase changed {old['phase']}→{e['phase']}")
    return errs

CLAIMS = [0]

def run(seed, steps):
    r = random.Random(seed)
    t = 0
    revoked = set()
    create = {"id": H("create"), "v": 1, "space": S, "kind": "space.create", "author": O, "keyId": "k1", "body": {"policy": policy(r)},
              "contentHash": "", "refs": [], "seen": 0, "nonce": "c", "time": "2026-10-10T12:00:00Z"}
    ctx = {"time": BASE.strftime("%Y-%m-%dT%H:%M:%SZ"), "profiles": {a: profile(a) for a in ACTORS}}
    state = P.fold(None, {"entry": create, "content": None, "receipt": {"seq": 1, "time": ctx["time"]}}, ctx)
    for i, a in enumerate([M, PR, X, BOT, G]):            # a populated room, so the effect lifecycle is reachable
        ms = {"id": H(f"setup{i}"), "v": 1, "space": S, "kind": "member.set", "author": O, "keyId": "k1",
              "body": {"actor": a, "role": "guest" if a == G else "member"}, "contentHash": "", "refs": [], "seen": 0,
              "nonce": f"s{i}", "time": "2026-10-10T12:00:00Z"}
        if P.decide(state, ms, ctx)["allow"]:
            state = P.fold(state, {"entry": ms, "content": None, "receipt": {"seq": state["head"] + 1, "time": ctx["time"]}}, ctx)
    allowed = 0
    for n in range(steps):
        t += r.choice([0, 1, 5, 20, 45])
        if r.random() < .03: revoked ^= {r.choice(ACTORS)}
        ctx = {"time": (BASE + timedelta(seconds=t)).strftime("%Y-%m-%dT%H:%M:%SZ"),
               "profiles": {a: profile(a, a in revoked) for a in ACTORS}}
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
        errs = check_state(new, state, ctx)
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
    print(f"{seeds} seeds × {steps} steps: {total} allowed entries folded ({CLAIMS[0]} claims); decide/fold total, pure, budgets within bounds, final phases stable")
    return 0

if __name__ == "__main__":
    sys.exit(main())

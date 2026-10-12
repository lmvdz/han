"""Fold totality and invariants under random entry sequences (the generator ref/fuzz_diff.py also uses).

World: an owner O, members M and PR, a guest G, two operators OP and OP2 (human members, separate from the owner), a service X and
agents BOT and BOT2 whose operators are OP, OP and OP2. Operators withdraw and confirm again (an operator never changes, §4.3),
keys are revoked and restored, owners force guarding on and off, agents choose guarding and their guards
unguard them, and the fitting author (an assignee closing its own task or delegating beneath it, an executor claiming, a guard
approving or unguarding) is chosen more often than chance, so delegation chains grow from guarded agents. Guarded agents also try
the kinds they may not use (approve, member.set, steering others' sessions) and leave. One seed in four is a crowd (M9): 14 more
human members, 23 actors in all, who approve the newest effect and are assigned tasks of up to 65 assignees, so the 16-approval cap
(P-08) and the 64-assignee bound (§2) are reached; the run reports how often, and how often the guard paths were taken.

For every entry decide() allows, fold() must not raise, must not mutate its inputs, and the new state must satisfy:
  · every budget amount (space remainder, task remainders) is ≥ 0
  · the space remainder never exceeds the space budget, a task's remainder never exceeds its budget
  · only allowed phases occur, and a final phase never changes
  · decide() never raises
  · no effect has more than MAX_ATTEMPTS claims
  · guard_status never raises; `guarded` is the state's set or forcing, `forced` is forcing on a non-human, and `guard` is the
    operator in the pinned profile, confirmed
  · P-15, recomputed here from the definitions: a new task's `guardedFor` is its parent's plus its author if guarded, a new effect's
    its task's plus its proposer and executor if guarded; a recorded `guardedFor` never changes; it holds each agent once and at
    most MAX_GUARDED_FOR; an allowed claim consumed an approval by the guard of every agent in `guardedFor` and of its proposer and
    executor if guarded at the claim, and never consumed as the rule's approval one whose author is guarded at the claim (R2); a
    guarded author is never allowed `effect.approve`, a `member.set` other than leaving (R5) or a steer into another's session; a
    guard's approval is never refused for the cap (R3)
  · an entry that touches an effect leaves it no unconsumed approval that has expired
  · the P-08 bounds, derived in policy.py and han.ts (R3): each author holds at most one unconsumed and one consumed approval on an
    effect; authors that can never be its guards hold at most MAX_OPEN_APPROVALS + 1; so an effect holds at most
    MAX_UNCONSUMED_APPROVALS unconsumed and MAX_APPROVALS in all
  · the JCS size of every state item (effect, task, session, role history, ...) is within a bound derived from the grammar's
    maximum lengths and the Policy constants, so an item that cannot fit in a state page cannot occur
Usage: python ref/fuzz_fold.py [seeds] [steps]      (default 2000 seeds × 200 steps)
"""
import collections, copy, hashlib, os, random, sys
from datetime import datetime, timedelta, timezone
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import policy as P

S = "fz:a.example"
O, M, PR, X, G, BOT = "@o:a.example", "@m:a.example", "@p:b.example", "@x:c.example", "@g:a.example", "@bot:b.example"
OP, OP2, BOT2 = "@op:b.example", "@op2:d.example", "@bot2:d.example"
ACTORS = [O, M, PR, X, G, BOT, OP, OP2, BOT2]
CROWD = [f"@h{i}:a.example" for i in range(14)]            # M9: a crowd seed's extra members, so the caps are reached

def actors_of(seed):
    """The world of a seed: one seed in four is a crowd of 23 actors."""
    return ACTORS + CROWD if seed % 4 == 3 else ACTORS
OPERATOR = {BOT: OP, BOT2: OP2, X: OP}                       # fixed: an operator only withdraws and confirms again (§4.3)
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

def flip(r, revoked, unconf, actors=ACTORS):
    """With small probability toggle one actor's key revocation or one agent's operator confirmation (a withdrawal, or a confirmation
    again); returns the actors whose view changed."""
    out = []
    if r.random() < .03: a = r.choice(sorted(revoked) if revoked and r.random() < .75 else actors); revoked ^= {a}; out.append(a)      # revoked keys come back, as a rotation would
    if r.random() < .06: a = r.choice(sorted(unconf) if unconf and r.random() < .6 else AGENTS); unconf ^= {a}; out.append(a)
    return out

def profiles_of(revoked, unconf, actors=ACTORS): return {a: profile(a, a in revoked, a in unconf) for a in actors}

def guarded(s, ctx, a):
    """P-15, from its definition: `a` chose guarding (the state's set) or owners force it and the pinned profile says it is not human."""
    p = ctx["profiles"].get(a)
    return a in s["guarded"] or bool(p and p["actorKind"] != "human" and s["policy"]["sessions"].get("forceGuarded"))

def guard_of(ctx, a):
    p = ctx["profiles"].get(a)
    return p["operator"] if p and p["actorKind"] != "human" and p.get("operatorConfirmed") and p.get("operator") else None

def guards_now(s, ctx, eff):
    """P-15 from its definitions: the guards of the agents in `guardedFor` and of the proposer and executor if guarded now."""
    need = list(eff["guardedFor"]) + [x for x in (eff["proposer"], eff["executor"]) if guarded(s, ctx, x)]
    return {guard_of(ctx, x) for x in need} - {None}

def fmt(t): return (BASE + timedelta(seconds=t)).strftime("%Y-%m-%dT%H:%M:%SZ")

def random_entry(r, state, n, actors=ACTORS):
    crowd = len(actors) > len(ACTORS)
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
                     "agent.unguard", "agent.unguard"])
    if crowd and r.random() < .25: kind = "effect.approve"
    pid, sid, tid = pick(effects, H("nope")), pick(sessions, H("nope")), pick(tasks, H("nope"))
    if crowd and effects and kind == "effect.approve" and r.random() < .8: pid = effects[-1]      # a crowd piles onto the newest effect
    body = {
        "member.set": lambda: {"actor": r.choice(actors), "role": r.choice(["owner", "member", "member", "member", "guest", "banned", "none"])},
        "message": lambda: {"to": [r.choice(actors)]},
        "task": lambda: dict({"to": r.choices(actors, k=r.choice([63, 64, 64, 65])) if crowd and r.random() < .3 else r.sample(actors + AGENTS * 2, r.choice([1, 1, 2]))},
                             **({"budget": cost} if r.random() < .4 else {}),
                             **({"parent": tid} if r.random() < .4 else {})),
        "task.update": lambda: {"task": tid, "state": r.choice(["accepted", "progress", "done", "failed", "declined", "cancelled"])},
        "task.cancel": lambda: {"task": tid},
        "effect.propose": lambda: dict({"executor": r.choice([X, X, M, BOT, BOT, BOT2]), "action": r.choice(["pay", "push"]), "planDigest": H("plan"),
                                         "repeatable": r.random() < .3}, **({"cost": cost} if r.random() < .6 else {}),
                                       **({"task": tid} if r.random() < .4 else {})),
        "effect.approve": lambda: dict({"proposal": pid, "planDigest": H("plan")},
                                       **({"expiresAt": r.choice([fmt(r.randint(0, 3000)), "2026-13-45T99:99:99Z", "2026-02-30T00:00:00Z"])} if r.random() < (.05 if crowd else .3) else {})),
        "effect.claim": lambda: {"proposal": pid, "instance": "k1",
                                 "attempt": (len(state["effects"][pid]["claims"]) + 1 if pid in state["effects"] and r.random() < .85 else r.randint(1, 3)),
                                 "window": state["policy"]["launchWindow"] if r.random() < .9 else r.choice([15, 30, 60])},
        "effect.cancel": lambda: {"proposal": pid},
        "effect.outcome": lambda: dict({"claim": pick(claims, H("nope")), "result": r.choice(["succeeded", "failed", "cancelled", "unknown"]),
                                        "started": r.choice(["yes", "no", "unknown"])}, **({"cost": cost} if r.random() < .4 else {})),
        "effect.resolve": lambda: dict({"proposal": pid, "result": r.choice(["succeeded", "failed"])}, **({"cost": cost} if r.random() < .3 else {})),
        "policy.set": lambda: {"policy": dict(policy(r), **({"approvals": [{"action": "*", "approvers": ["member"]}]} if crowd and r.random() < .8 else {}))},
        "checkpoint": lambda: {"through": r.randint(0, 5 + n)},
        "redact": lambda: {"target": pick(ids, H("nope"))},
        "space.move": lambda: {"to": "b.example", "accept": {"keyId": "h1", "sig": "s"}},
        "x.ext": lambda: {"anything": r.randint(0, 9)},
        "session.open": lambda: dict({"steerMode": r.choice(["auto", "guarded", "guarded"])}, **({"task": tid} if r.random() < .3 else {})),
        "session.close": lambda: {"session": sid},
        "session.input": lambda: {"session": sid},
        "agent.unguard": lambda: {"agent": r.choice(list(state["guarded"]) if state and state["guarded"] and r.random() < .7 else AGENTS)},
    }[kind]()
    author = r.choice(actors + [O, O, X, X])
    st = state or {"tasks": {}, "effects": {}, "sessions": {}, "guarded": {}}
    if r.random() < (.9 if kind in ("effect.claim", "effect.outcome", "effect.resolve") else .65):                                   # the fitting author: assignees work and delegate, guards approve and unguard
        if kind in ("task.update", "task.cancel") and body["task"] in st["tasks"]:
            t = st["tasks"][body["task"]]; author = r.choice(t["to"]) if kind == "task.update" else t["author"]
        elif kind in ("effect.claim", "effect.resolve", "effect.cancel") and body["proposal"] in st["effects"]:
            e = st["effects"][body["proposal"]]; author = e["executor"] if kind != "effect.cancel" else r.choice([e["proposer"], e["executor"], OP, OP2])
        elif kind == "effect.outcome":
            for e in st["effects"].values():
                if any(c["id"] == body["claim"] for c in e["claims"]): author = e["executor"]
        elif kind == "effect.approve":
            e = st["effects"].get(body["proposal"])
            author = r.choice([OP, OP2, O, M, PR] + ([OPERATOR[w] for w in (e["proposer"], e["executor"]) if w in OPERATOR] if e else []) + (CROWD * 2 if crowd else []))
        elif kind == "agent.unguard": author = r.choice([OP, OP, OP2]) if r.random() < .8 else r.choice([O, body["agent"]])
        elif kind == "session.input" and body["session"] in st["sessions"]:
            author = r.choice([a for a in actors if a != st["sessions"][body["session"]]["actor"]] + [st["sessions"][body["session"]]["actor"]])
        elif kind == "session.open": author = r.choice(AGENTS)
        elif kind == "task" and body.get("parent") in st["tasks"] and r.random() < .7: author = r.choice(st["tasks"][body["parent"]]["to"])     # delegating beneath it
        elif kind == "task": author = r.choice([PR, M, O, OP, BOT, BOT2, X])
        elif kind == "effect.propose" and body.get("task") in st["tasks"] and r.random() < .7: author = r.choice(st["tasks"][body["task"]]["to"])
        elif kind == "effect.propose": author = r.choice([PR, M, O, OP, BOT, BOT2])
    if st["guarded"] and kind in ("effect.approve", "member.set", "session.input", "redact", "task", "effect.propose") and r.random() < .12:
        author = r.choice(sorted(st["guarded"]))                                                             # a guarded agent tries it
    head = state["head"] if state else 0
    seen = r.choice([0, head, head, r.randint(0, head)])
    return {"id": H(f"e{n}"), "v": 1, "space": S, "kind": kind, "author": author, "keyId": "k1", "body": body,
            "contentHash": "", "refs": [], "seen": seen, "nonce": f"n{n}", "time": "2026-10-10T12:00:00Z"}

def setup(r, state, ctx, actors=ACTORS):
    """A populated room, so the effect lifecycle is reachable: yields the member.set entries to apply."""
    for i, a in enumerate([M, PR, X, BOT, OP, OP2, BOT2, G] + actors[len(ACTORS):]):
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
    claim = {"id": ID, "attempt": 64, "instance": KEY, "time": TIME, "window": 3600, "approval": ID, "guardApprovals": [ID] * P.MAX_GUARDED_FOR, "reserved": BUD,
             "source": ["task", ID], "settled": True, "outcome": "unknown", "started": "unknown"}
    effect = {"proposer": ACT, "proposerKey": KEY, "executor": ACT, "action": "a" * 128, "planDigest": ID, "seq": 2 ** 53 - 1, "cost": BUD,
              "repeatable": False, "task": ID, "phase": "unknown", "cancelRequested": False, "expiresAt": TIME, "guardedFor": [ACT] * P.MAX_GUARDED_FOR,
              "approvals": [approval] * P.MAX_APPROVALS, "claims": [claim] * P.MAX_ATTEMPTS}
    task = {"author": ACT, "to": [ACT] * 64, "open": True, "state": "progress", "cancelRequested": False, "depth": 16, "seq": 2 ** 53 - 1,
            "guardedFor": [ACT] * (P.MAX_GUARDED_FOR - 2), "parent": ID, "budget": BUD, "left": BUD}
    session = {"actor": ACT, "opened": TIME, "steerMode": "guarded", "steers": 2 ** 53 - 1, "task": ID}
    history = [{"from": 2 ** 53 - 1, "to": 2 ** 53 - 1}] * P.ROLE_INTERVALS
    return {"effects": len(P.jcs(effect)), "tasks": len(P.jcs(task)), "sessions": len(P.jcs(session)), "roleHistory": len(P.jcs(history))}
BOUND = _worst()

def check_guarding(s, prev, ctx, e):
    """P-15 (P2, P4, P5), recomputed from the definitions on the state before the entry (`prev`), never from Policy's helpers."""
    errs, k, b, a = [], e["kind"], e["body"], e["author"]
    leaving = k == "member.set" and b == {"actor": a, "role": "none"}
    if guarded(prev, ctx, a) and (k == "effect.approve" or (k == "member.set" and not leaving) or (k == "session.input" and prev["sessions"][b["session"]]["actor"] != a)):
        errs.append(f"guarded {a} was allowed {k}")
    if k == "task":
        parent = prev["tasks"].get(b.get("parent"))
        want = list(parent["guardedFor"] if parent else []) + ([a] if guarded(prev, ctx, a) and a not in (parent["guardedFor"] if parent else []) else [])
        if s["tasks"][e["id"]]["guardedFor"] != want: errs.append(f"task guardedFor {s['tasks'][e['id']]['guardedFor']} != {want}")
    if k == "effect.propose":
        want = list(prev["tasks"][b["task"]]["guardedFor"]) if "task" in b else []
        for x in (a, b["executor"]):
            if x not in want and guarded(prev, ctx, x): want.append(x)
        if s["effects"][e["id"]]["guardedFor"] != want: errs.append(f"effect guardedFor {s['effects'][e['id']]['guardedFor']} != {want}")
    if k == "effect.claim":
        eff, c = s["effects"][b["proposal"]], s["effects"][b["proposal"]]["claims"][-1]
        consumed = {x["id"]: x["author"] for x in eff["approvals"] if x.get("consumedBy") == c["id"]}
        need = list(eff["guardedFor"]) + [x for x in (eff["proposer"], eff["executor"]) if guarded(prev, ctx, x)]
        for x in need:
            g = guard_of(ctx, x)
            if g is None or not any(consumed.get(i) == g for i in c.get("guardApprovals", [])) or prev["roles"].get(g) not in P.READ_ROLES:
                errs.append(f"claim of {b['proposal'][:6]} allowed without an approval by the guard ({g}) of {x}")
        ruled = next((x["author"] for x in eff["approvals"] if x["id"] == c.get("approval")), None)
        if ruled and guarded(prev, ctx, ruled): errs.append(f"claim of {b['proposal'][:6]} consumed the approval of {ruled}, guarded at the claim (R2)")
    for tid, t in prev["tasks"].items():
        if s["tasks"][tid]["guardedFor"] != t["guardedFor"]: errs.append(f"task {tid[:6]} guardedFor changed")
    for pid, x in prev["effects"].items():
        if s["effects"][pid]["guardedFor"] != x["guardedFor"]: errs.append(f"effect {pid[:6]} guardedFor changed")
    for item in list(s["tasks"].values()) + list(s["effects"].values()):
        g = item["guardedFor"]
        if len(g) != len(set(g)) or len(g) > P.MAX_GUARDED_FOR: errs.append(f"guardedFor {g} repeats an agent or is longer than {P.MAX_GUARDED_FOR}")
    return errs

def approval_bounds(eff):
    """P-08 (R3), derived and asserted here: per author one unconsumed and one consumed approval; authors that can never be the
    effect's guards (operators never change) at most MAX_OPEN_APPROVALS + 1; MAX_UNCONSUMED_APPROVALS and MAX_APPROVALS in all."""
    errs, open_, used = [], [a["author"] for a in eff["approvals"] if not a.get("consumedBy")], [a["author"] for a in eff["approvals"] if a.get("consumedBy")]
    if len(open_) != len(set(open_)) or len(used) != len(set(used)): errs.append("an author holds two unconsumed or two consumed approvals")
    may_guard = {OPERATOR[x] for x in list(eff["guardedFor"]) + [eff["proposer"], eff["executor"]] if x in OPERATOR}
    others = sum(1 for a in eff["approvals"] if a["author"] not in may_guard)
    if others > P.MAX_OPEN_APPROVALS + 1: errs.append(f"{others} approvals by authors that are never guards")
    if len(open_) > P.MAX_UNCONSUMED_APPROVALS: errs.append(f"{len(open_)} unconsumed approvals")
    if len(eff["approvals"]) > P.MAX_APPROVALS: errs.append(f"{len(eff['approvals'])} approvals on one effect")
    return errs

def check_state(s, prev, ctx, e=None, actors=ACTORS):
    errs = check_guarding(s, prev, ctx, e) if prev and e else []
    for a in actors:
        st = P.guard_status(s, ctx, a)
        p = ctx["profiles"][a]
        if st["guarded"] != guarded(s, ctx, a) or st["forced"] != (p["actorKind"] != "human" and bool(s["policy"]["sessions"].get("forceGuarded"))):
            errs.append(f"guard_status disagrees with the definitions for {a}: {st}")
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
        errs += approval_bounds(eff)
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
STATS = {"crowd seeds": 0, "approvals refused at the cap": 0, "guard approvals beyond the cap": 0, "tasks with 64 assignees allowed": 0, "tasks with 65 assignees refused": 0}
GUARD_STATS = collections.Counter()

def count(stats, e, d, state=None, ctx=None):
    """M9: how often the generator reaches the 16-approval cap (and a guard approves beyond it, R3) and the 64-assignee bound."""
    if e["kind"] == "effect.approve" and not d["allow"] and "too many open approvals" in d.get("reason", ""): stats["approvals refused at the cap"] += 1
    if e["kind"] == "effect.approve" and d["allow"] and state and e["body"]["proposal"] in state["effects"]:
        eff = state["effects"][e["body"]["proposal"]]
        if e["author"] in guards_now(state, ctx, eff) and sum(1 for a in P.open_approvals(eff, ctx["time"]) if a["author"] != e["author"]) >= P.MAX_OPEN_APPROVALS:
            stats["guard approvals beyond the cap"] += 1
    n = len(e["body"]["to"]) if e["kind"] == "task" and isinstance(e["body"].get("to"), list) else 0
    if n == 64 and d["allow"]: stats["tasks with 64 assignees allowed"] += 1
    if n == 65 and not d["allow"]: stats["tasks with 65 assignees refused"] += 1

def count_guard(stats, prev, new, e, d, ctx=None):
    """P9: how often the guard paths were taken (keys start with `* ` so fuzz_diff lists them with its guard paths)."""
    k = e["kind"]
    if not d["allow"]:
        if "guarded author may not" in d.get("reason", ""): stats["* forbidden kind refused for a guarded author"] += 1
        if k == "effect.claim" and "guard" in d.get("reason", ""): stats["* claim refused for a guard"] += 1
        return
    if k == "task" and len(new["tasks"][e["id"]]["guardedFor"]) >= 1 and "parent" in e["body"]: stats["* subtask carrying a guarded agent"] += 1
    if k == "task" and len(new["tasks"][e["id"]]["guardedFor"]) >= 2: stats["* task carrying two guarded agents"] += 1
    if k == "effect.propose" and any(x not in (e["author"], e["body"]["executor"]) for x in new["effects"][e["id"]]["guardedFor"]): stats["* effect carrying a delegating agent"] += 1
    if k == "effect.claim" and new["effects"][e["body"]["proposal"]]["claims"][-1].get("guardApprovals"): stats["* claim with guard approvals"] += 1
    if k == "agent.unguard": stats["* unguard allowed"] += 1
    if k == "member.set" and e["body"] == {"actor": e["author"], "role": "none"} and ctx and guarded(prev, ctx, e["author"]): stats["* guarded agent left"] += 1
    if k == "policy.set" and prev["policy"]["sessions"].get("forceGuarded") != new["policy"]["sessions"].get("forceGuarded"): stats["* forcing switched"] += 1

def run(seed, steps):
    r = random.Random(seed)
    t, revoked, unconf = 0, set(), set()
    actors = actors_of(seed)
    STATS["crowd seeds"] += actors is not ACTORS
    create = {"id": H("create"), "v": 1, "space": S, "kind": "space.create", "author": O, "keyId": "k1", "body": {"policy": policy(r)},
              "contentHash": "", "refs": [], "seen": 0, "nonce": "c", "time": "2026-10-10T12:00:00Z"}
    if actors is not ACTORS: create["body"]["policy"]["approvals"] = [{"action": "*", "approvers": ["member"]}]
    ctx = {"time": fmt(0), "profiles": profiles_of(revoked, unconf, actors)}
    state = P.fold(None, {"entry": create, "content": None, "receipt": {"seq": 1, "time": ctx["time"]}}, ctx)
    for ms in setup(r, state, ctx, actors):
        if P.decide(state, ms, ctx)["allow"]:
            state = P.fold(state, {"entry": ms, "content": None, "receipt": {"seq": state["head"] + 1, "time": ctx["time"]}}, ctx)
    allowed = 0
    for n in range(steps):
        t += r.choice([0, 1, 5, 20, 45])
        before = set(unconf); flip(r, revoked, unconf, actors)
        GUARD_STATS["* operator confirmed again"] += bool(before - unconf)
        ctx = {"time": fmt(t), "profiles": profiles_of(revoked, unconf, actors)}
        e = random_entry(r, state, n, actors)
        before, ctx0 = copy.deepcopy(state), copy.deepcopy(ctx)
        try:
            d = P.decide(state, e, ctx)
        except Exception as ex:
            return f"seed {seed} step {n}: decide raised {type(ex).__name__}: {ex} on {e['kind']}", allowed
        if state != before or ctx != ctx0: return f"seed {seed} step {n}: decide mutated input", allowed
        count(STATS, e, d, state, ctx)
        if not d["allow"]:
            eff = state["effects"].get(e["body"].get("proposal")) if e["kind"] == "effect.approve" else None
            if eff and "too many open approvals" in d.get("reason", "") and e["author"] in guards_now(state, ctx, eff):
                return f"seed {seed} step {n}: the guard {e['author']} was refused for the cap (R3)", allowed
            count_guard(GUARD_STATS, state, None, e, d, ctx); continue
        try:
            new = P.fold(state, {"entry": e, "content": None, "receipt": {"seq": state["head"] + 1, "time": ctx["time"]}}, ctx)
        except Exception as ex:
            return f"seed {seed} step {n}: fold raised {type(ex).__name__}: {ex} on allowed {e['kind']} {e['body']}", allowed
        if state != before: return f"seed {seed} step {n}: fold mutated input", allowed
        errs = check_state(new, state, ctx, e, actors)
        if errs: return f"seed {seed} step {n}: after {e['kind']} by {e['author']} {e['body']}: {errs}", allowed
        count_guard(GUARD_STATS, state, new, e, d, ctx)
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
    print("caps reached (M9): " + "; ".join(f"{k} {v}" for k, v in STATS.items()))
    print("guard paths (P9): " + "; ".join(f"{k[2:]} {v}" for k, v in sorted(GUARD_STATS.items())))
    print(f"{seeds} seeds × {steps} steps: {total} allowed entries folded ({CLAIMS[0]} claims); decide/fold total, pure, budgets within bounds, "
          f"final phases stable, guardedFor recorded as defined and never changed, claims approved by every guard and by no guarded rule approver, guarded authors restricted, "
          f"guards never capped, expired approvals dropped, approvals within the derived bounds (unconsumed {P.MAX_UNCONSUMED_APPROVALS}, all {P.MAX_APPROVALS}), "
          f"every state item within its derived size bound {BOUND}")
    return 0

if __name__ == "__main__":
    sys.exit(main())

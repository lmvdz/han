"""Writes ref/vectors/*.json. Expectations below are hand-written from architecture.md, NOT computed by policy.py.
Each vector is fully expanded (every entry field, every profile, the full policy), so a stranger needs no macros.
"""
import copy, hashlib, json, os
from datetime import datetime, timedelta, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "vectors")
BASE = datetime(2026, 10, 10, 12, 0, 0, tzinfo=timezone.utc)
S = "s1:a.example"
O, M, P, X, G, Z, BOT, OP = "@o:a.example", "@m:a.example", "@p:b.example", "@x:c.example", "@g:a.example", "@z:d.example", "@bot:b.example", "@op:b.example"

def policy(**kw):
    p = {"visibility": "members", "join": "invite", "membersMayInvite": False, "allowAgents": "any", "approvals": [],
         "separateApprover": False, "budget": None, "maxDelegationDepth": 4, "minDurability": "local", "launchWindow": 30, "extensionsMinRole": "member", "sessions": {"watch": "member", "read": "member", "steer": "owner", "modes": ["auto", "guarded", "suggest"]},
         "servers": {"deny": []}}
    p.update(kw); return p

def human(keys=None):
    return {"actorKind": "human", "operatorConfirmed": False, "keys": keys or {"k1": {"publicKey": "pk", "status": "valid"}}}

def E(id, kind, author, body, *, key="k1", refs=None, space=S):
    return {"id": id, "v": 1, "space": space, "kind": kind, "author": author, "keyId": key, "body": body,
            "contentHash": "", "refs": refs or [], "seen": 0, "nonce": "n-" + id, "time": "2026-10-10T12:00:00Z"}

class V:
    def __init__(self, name, rules, desc):
        self.v = {"name": name, "rules": rules, "description": desc, "profiles": {}, "steps": []}
        self.t = 0
    def prof(self, actor, view):
        self.v["profiles"][actor] = view; return self
    def step(self, entry, expect, *, dt=1, at=None, check=None, profiles=None, predecessor=None, note=None):
        self.t = at if at is not None else self.t + dt
        st = {"entry": entry, "time": (BASE + timedelta(seconds=self.t)).strftime("%Y-%m-%dT%H:%M:%SZ"), "expect": expect}
        if check: st["check"] = check
        if profiles: st["profiles"] = profiles
        if predecessor: st["predecessor"] = predecessor
        if note: st["note"] = note
        self.v["steps"].append(st); return self
    def read(self, actor, seq, expect, space=S):
        self.v["steps"].append({"read": {"space": space, "actor": actor, "seq": seq}, "expect": expect}); return self
    def access(self, actor, session, expect, space=S):
        self.v["steps"].append({"sessionAccess": {"space": space, "actor": actor, "session": session}, "expect": expect}); return self
    def actors(self, entry, expect):
        self.v["steps"].append({"relevantActors": entry, "expect": expect}); return self
    def done(self):
        names = set()
        for st in self.v["steps"]:
            e = st.get("entry") or st.get("relevantActors")
            if e:
                names.add(e["author"])
                b = e["body"]
                for f in ("actor", "executor"):
                    if f in b: names.add(b[f])
                for a in b.get("to", []): names.add(a)
            if "read" in st: names.add(st["read"]["actor"])
        for n in sorted(names):
            if n not in self.v["profiles"]:
                self.v["profiles"][n] = human() if not n.startswith("@x") else dict(human(), actorKind="service")
        return hexify(self.v)

def create(pol=None, author=O, id="C"):
    return E(id, "space.create", author, {"policy": pol or policy()})

def ms(id, author, actor, role): return E(id, "member.set", author, {"actor": actor, "role": role})

def team(v, pol=None):
    """owner O; members M, P; executor X as member."""
    v.step(create(pol), "allow")
    v.step(ms("m1", O, M, "member"), "allow").step(ms("m2", O, P, "member"), "allow").step(ms("m3", O, X, "member"), "allow")
    return v

def prop(id, author=P, action="git.push", cost=None, repeatable=False, task=None, executor=X):
    b = {"executor": executor, "action": action, "planDigest": digest_of(id), "repeatable": repeatable}
    if cost: b["cost"] = cost
    if task: b["task"] = task
    return E(id, "effect.propose", author, b)

def claim(id, pr, attempt=1, author=X, instance="k1", key="k1", window=30):
    return E(id, "effect.claim", author, {"proposal": pr, "attempt": attempt, "instance": instance, "window": window}, key=key)

def outc(id, cl, result, started, author=X, cost=None, key="k1"):
    b = {"claim": cl, "result": result, "started": started}
    if cost: b["cost"] = cost
    return E(id, "effect.outcome", author, b, key=key)

def appr(id, pr, author=O, digest=None, expires=None):
    b = {"proposal": pr, "planDigest": digest_of(digest or pr)}
    if expires: b["expiresAt"] = expires
    return E(id, "effect.approve", author, b)

USD = lambda n: {"unit": "USD-cent", "amount": n}
ACCEPT = {"keyId": "h1", "sig": "signature-checked-by-Log-and-mirrors"}   # Policy checks only its shape (R-23)

def digest_of(label):
    return hashlib.sha256(("plan:" + label).encode()).hexdigest()

def receipt(space, seq, label):
    """A structurally complete Receipt (signatures are not checked by Policy; see ref/wire.py)."""
    return {"space": space, "seq": seq, "id": label, "prev": "0" * 64, "home": space.split(":")[1], "homeKey": "h1",
            "time": "2026-10-10T12:00:00Z", "durability": "local", "profiles": {}, "homeSig": "sig"}

ID_FIELDS = ("proposal", "claim", "task", "parent", "target", "session")

def hexify(vec):
    """Replace symbolic entry labels by 64-hex ids everywhere; keep the mapping in vec['ids'] for readers."""
    labels = set()
    for st in vec.get("steps", []):
        e = st.get("entry") or st.get("relevantActors")
        if e: labels.add(e["id"])
    h = {l: hashlib.sha256(("id:" + l).encode()).hexdigest() for l in sorted(labels)}
    def fix_entry(e):
        e["id"] = h[e["id"]]
        e["refs"] = [h.get(r, r) for r in e["refs"]]
        for f in ID_FIELDS:
            if f in e["body"] and e["body"][f] in h: e["body"][f] = h[e["body"][f]]
        if "succeeds" in e["body"]:
            t = e["body"]["succeeds"]["through"]
            t["id"] = h.get(t["id"], t["id"])
    def fix_val(v):
        if isinstance(v, str): return h.get(v, v)
        if isinstance(v, dict): return {h.get(k, k): fix_val(x) for k, x in v.items()}
        if isinstance(v, list): return [fix_val(x) for x in v]
        return v
    for st in vec.get("steps", []):
        if "entry" in st: fix_entry(st["entry"])
        if "relevantActors" in st: fix_entry(st["relevantActors"])
        if "sessionAccess" in st: st["sessionAccess"]["session"] = h.get(st["sessionAccess"]["session"], st["sessionAccess"]["session"])
        if "check" in st:
            st["check"] = {".".join(h.get(seg, seg) for seg in k.split(".")): fix_val(v) for k, v in st["check"].items()}
    vec["ids"] = {l: h[l] for l in sorted(labels)}
    return vec
VECTORS = []
add = VECTORS.append

# ── R-09 / R-05 / P-11 basics ──
v = V("create-once", ["P-11", "R-09"], "space.create only at seq 1; creator becomes owner")
v.step(create(), "allow", check={"roles.@o:a.example": "owner", "head": 1})
v.step(E("C2", "space.create", O, {"policy": policy()}), "invalid")
add(v.done())

v = V("closed-bodies", ["R-09"], "unknown, missing or ill-typed body fields are invalid")
team(v)
v.step(E("bad1", "message", M, {"to": [], "extra": 1}), "invalid", note="unknown field")
v.step(E("bad2", "task", M, {"budget": USD(5)}), "invalid", note="missing to")
v.step(E("bad3", "effect.propose", P, {"executor": X, "action": "a", "planDigest": "d", "repeatable": "yes"}), "invalid", note="repeatable not boolean")
v.step(E("bad4", "task", O, {"to": [M], "budget": {"unit": "USD-cent", "amount": 1.5}}), "invalid", note="non-integer amount")
v.step(E("bad5", "member.set", O, {"actor": "not-an-actor", "role": "member"}), "invalid")
add(v.done())

v = V("refs-and-seen", ["R-05"], "refs must resolve in this space; seen must not exceed head")
team(v)
v.step(E("r1", "message", M, {}, refs=[hashlib.sha256(b"absent").hexdigest()]), "invalid", note="well-formed id not in this space")
e = E("r2", "message", M, {}); e["seen"] = 99
v.step(e, "invalid")
v.step(E("r3", "message", M, {}, refs=["C"]), "allow")
add(v.done())

v = V("revoked-author-key", ["R-12"], "an entry signed by a revoked key is denied")
team(v)
v.step(E("k", "message", M, {}), "denied", profiles={M: human({"k1": {"publicKey": "pk", "status": "revoked"}})})
add(v.done())

# ── P-03 membership ──
v = V("membership-rules", ["P-03", "P-04"], "invites, demotion, last owner, self-leave")
v.step(create(), "allow").step(ms("a", O, M, "member"), "allow")
v.step(ms("b", M, G, "guest"), "denied", note="membersMayInvite is false")
v.step(E("pol", "policy.set", O, {"policy": policy(membersMayInvite=True)}), "allow")
v.step(ms("c", M, G, "guest"), "allow", note="invite target with no role")
v.step(ms("d", M, O, "guest"), "denied", note="member may not demote owner")
v.step(ms("e", M, G, "member"), "denied", note="invite only when target has no role")
v.step(ms("f", O, O, "none"), "denied", note="last owner cannot leave")
v.step(ms("g", G, G, "none"), "allow", check={"roles": {O: "owner", M: "member"}}, note="self-leave")
add(v.done())

v = V("open-join-and-ban", ["P-03"], "open join; banned actors cannot rejoin or self-unban")
v.step(create(policy(join="open")), "allow")
v.step(ms("j", Z, Z, "member"), "allow")
v.step(ms("j2", Z, Z, "owner"), "denied", note="open join is member only")
v.step(ms("b", O, Z, "banned"), "allow")
v.step(ms("u", Z, Z, "none"), "denied", note="banned cannot self-leave to escape ban")
v.step(ms("j3", Z, Z, "member"), "denied")
add(v.done())

v = V("agents-need-operator", ["P-03", "R-12"], "allowAgents with-operator requires a confirmed operator")
v.prof(BOT, {"actorKind": "agent", "operator": OP, "operatorConfirmed": False, "keys": {"k1": {"publicKey": "pk", "status": "valid"}}})
v.step(create(policy(allowAgents="with-operator")), "allow")
v.step(ms("a", O, BOT, "member"), "denied")
v.step(ms("b", O, BOT, "member"), "allow", profiles={BOT: {"actorKind": "agent", "operator": OP, "operatorConfirmed": True,
                                                           "keys": {"k1": {"publicKey": "pk", "status": "valid"}}}})
add(v.done())

v = V("agents-none", ["P-03"], "allowAgents none refuses agents")
v.prof(BOT, {"actorKind": "agent", "operator": OP, "operatorConfirmed": True, "keys": {"k1": {"publicKey": "pk", "status": "valid"}}})
v.step(create(policy(allowAgents="none")), "allow").step(ms("a", O, BOT, "member"), "denied")
add(v.done())

v = V("server-deny", ["P-03", "P-02"], "denied servers can neither join nor author")
v.step(create(policy(servers={"deny": ["d.example"]})), "allow")
v.step(ms("a", O, Z, "member"), "denied")
v.step(E("m", "message", Z, {}), "denied")
add(v.done())

# ── P-02 read ──
v = V("read-rights", ["P-02"], "members read; former members read up to removal; servers read via their members")
team(v)
v.step(ms("rm", O, M, "none"), "allow")
v.read(M, 3, True).read(M, 6, False).read(Z, 1, False).read(P, 6, True)
v.read("@:b.example", 6, True).read("@:d.example", 2, False)
add(v.done())

v = V("read-public", ["P-02"], "public spaces are readable by anyone")
v.step(create(policy(visibility="public")), "allow").read(Z, 1, True)
add(v.done())

# ── P-05 tasks & P-06 budgets ──
v = V("tasks-and-delegation", ["P-05", "P-04"], "who may create, update, cancel and delegate tasks")
team(v, policy(maxDelegationDepth=1))
v.step(ms("g", O, G, "guest"), "allow")
v.step(E("t0", "task", G, {"to": [M]}), "denied", note="guests cannot create tasks")
v.step(E("t1", "task", M, {"to": [Z]}), "denied", note="assignee holds no role")
v.step(E("t2", "task", M, {"to": [P]}), "allow")
v.step(E("t3", "task", M, {"to": [X], "parent": "t2"}), "denied", note="only an assignee delegates")
v.step(E("t4", "task", P, {"to": [X], "parent": "t2"}), "allow", check={"tasks.t2.depth": 0, "tasks.t4.depth": 1})
v.step(E("t5", "task", X, {"to": [M], "parent": "t4"}), "denied", note="depth 2 > maxDelegationDepth 1")
v.step(E("u1", "task.update", M, {"task": "t2", "state": "progress"}), "denied", note="not an assignee")
v.step(E("c1", "task.cancel", X, {"task": "t2"}), "denied", note="not asker or owner")
v.step(E("c2", "task.cancel", M, {"task": "t2"}), "allow", check={"tasks.t2.cancelRequested": True, "tasks.t2.open": True})
v.step(E("u2", "task.update", P, {"task": "t2", "state": "cancelled"}), "allow", check={"tasks.t2.open": False, "tasks.t2.state": "cancelled"})
v.step(E("u3", "task.update", P, {"task": "t2", "state": "progress"}), "denied", note="closed")
add(v.done())

v = V("task-budgets", ["P-05", "P-06"], "reservation from space and parent; refund on close")
team(v, policy(budget=USD(1000)))
v.step(E("t1", "task", O, {"to": [P], "budget": USD(600)}), "allow", check={"budgetLeft": USD(400)})
v.step(E("t2", "task", O, {"to": [P], "budget": USD(600)}), "denied", note="exceeds space remainder")
v.step(E("t3", "task", O, {"to": [P], "budget": {"unit": "EUR-cent", "amount": 1}}), "denied", note="unit must be the space's unit")
v.step(E("c1", "task", P, {"to": [X], "parent": "t1"}), "denied", note="child of budgeted task needs budget")
v.step(E("c2", "task", P, {"to": [X], "parent": "t1", "budget": USD(700)}), "denied", note="exceeds parent remainder")
v.step(E("c3", "task", P, {"to": [X], "parent": "t1", "budget": USD(200)}), "allow", check={"tasks.t1.left": USD(400)})
v.step(E("c4", "task", P, {"to": [X], "parent": "t1", "budget": USD(200)}), "allow", check={"tasks.t1.left": USD(200)})
v.step(E("d3", "task.update", X, {"task": "c3", "state": "done"}), "allow", check={"tasks.t1.left": USD(400)})
v.step(E("d1", "task.update", P, {"task": "t1", "state": "done"}), "allow", check={"budgetLeft": USD(800)},
       note="unspent 400 of t1 refunded; c4's 200 stays reserved inside closed t1")
add(v.done())

# ── effects ──
v = V("effect-happy-path", ["P-07", "P-09", "X-05"], "propose → claim → outcome succeeded without approval")
team(v)
v.step(prop("pr"), "allow", check={"effects.pr.phase": "ready"})
v.step(claim("cl", "pr"), "allow", check={"effects.pr.phase": "claimed"})
v.step(outc("o", "cl", "succeeded", "yes"), "allow", check={"effects.pr.phase": "succeeded"})
v.step(claim("cl2", "pr", attempt=2), "denied", note="final")
add(v.done())

v = V("E1-executor-instance", ["P-07"], "only the named executor, with the instance key that signs, may claim")
team(v)
v.step(prop("pr"), "allow")
v.step(claim("a", "pr", author=M), "denied")
v.step(claim("b", "pr", instance="k2"), "denied", note="instance must equal signing keyId")
v.step(claim("c", "pr", instance="k2", key="k2"), "allow",
       profiles={X: dict(human({"k1": {"publicKey": "pk", "status": "valid"}, "k2": {"publicKey": "pk2", "status": "valid"}}), actorKind="service")})
v.step(outc("o", "c", "succeeded", "yes", key="k1"), "denied", note="other instance of same actor may not report")
v.step(outc("o2", "c", "succeeded", "yes", key="k2"), "allow")
add(v.done())

v = V("E2-proposer-revoked", ["P-07", "R-12"], "removing the proposer, or revoking the proposal's key, blocks the claim")
team(v)
v.step(prop("pr"), "allow")
v.step(claim("c1", "pr"), "denied", profiles={P: human({"k1": {"publicKey": "pk", "status": "revoked"}})}, note="stolen key revoked")
v.step(claim("c2", "pr"), "allow", profiles={P: human()}, note="key restored by recovery (new pin)")
v2 = V("E2-proposer-removed", ["P-07"], "a proposer removed before the claim cannot have it run")
team(v2)
v2.step(prop("pr"), "allow").step(ms("rm", O, P, "none"), "allow").step(claim("c", "pr"), "denied")
add(v.done()); add(v2.done())

v = V("E3-approvals", ["P-07", "P-08"], "approval required, matching digest, right approver, single use")
team(v, policy(approvals=[{"action": "git.push", "approvers": ["owner"]}], separateApprover=True))
v.step(prop("pr"), "allow", check={"effects.pr.phase": "awaiting-approval"})
v.step(claim("c0", "pr"), "denied", note="no approval")
v.step(appr("a0", "pr", author=M), "denied", note="members are not approvers")
v.step(appr("a1", "pr", digest="other"), "invalid", note="wrong digest")
v.step(appr("a2", "pr"), "allow", check={"effects.pr.phase": "ready"})
v.step(claim("c1", "pr"), "allow", check={"effects.pr.approvals.0.consumedBy": "c1"})
v.step(outc("o1", "c1", "failed", "no"), "allow", check={"effects.pr.phase": "ready"}, note="not started → approval returned")
v.step(claim("c2", "pr", attempt=2), "allow")
v.step(outc("o2", "c2", "failed", "yes"), "allow", check={"effects.pr.phase": "failed"})
add(v.done())

v = V("E3-separate-approver", ["P-08"], "with separateApprover, an owner-proposer cannot approve their own proposal")
team(v, policy(approvals=[{"action": "*", "approvers": ["owner"]}], separateApprover=True))
v.step(prop("pr", author=O), "allow").step(appr("a", "pr", author=O), "denied")
add(v.done())

v = V("E3-expiry-and-demotion", ["P-07", "P-08"], "expired approvals and approvers who lost the role don't count")
team(v, policy(approvals=[{"action": "*", "approvers": ["owner", M]}]))
v.step(prop("pr"), "allow")
v.step(appr("a1", "pr", expires="2026-10-10T12:00:08Z"), "allow")
v.step(claim("c1", "pr"), "denied", dt=10, note="approval expired")
v.step(ms("up", O, Z, "owner"), "allow")
v.step(appr("a2", "pr", author=Z), "allow")
v.step(ms("dn", O, Z, "member"), "allow")
v.step(claim("c2", "pr"), "denied", note="approver no longer owner")
v.step(appr("a3", "pr", author=M), "allow", note="named actor approver")
v.step(claim("c3", "pr"), "allow")
add(v.done())

v = V("E3-approver-key-revoked", ["P-07", "R-12"], "an approval signed by a since-revoked key doesn't count")
team(v, policy(approvals=[{"action": "*", "approvers": ["owner"]}]))
v.step(prop("pr"), "allow").step(appr("a", "pr"), "allow")
v.step(claim("c", "pr"), "denied", profiles={O: human({"k1": {"publicKey": "pk", "status": "revoked"}})})
add(v.done())

v = V("E4-cancel", ["P-07", "§6.4"], "cancel before claim is final; cancel after claim is only a request")
team(v)
v.step(prop("p1"), "allow").step(E("x1", "effect.cancel", P, {"proposal": "p1"}), "allow", check={"effects.p1.phase": "cancelled"})
v.step(claim("c1", "p1"), "denied")
v.step(prop("p2"), "allow").step(claim("c2", "p2"), "allow")
v.step(E("x2", "effect.cancel", M, {"proposal": "p2"}), "denied", note="not proposer/approver/owner")
v.step(E("x3", "effect.cancel", O, {"proposal": "p2"}), "allow", check={"effects.p2.phase": "claimed", "effects.p2.cancelRequested": True})
v.step(outc("o2", "c2", "succeeded", "yes"), "allow", check={"effects.p2.phase": "succeeded"}, note="honest: it already ran")
v.step(prop("p3"), "allow").step(claim("c3", "p3"), "allow").step(E("x4", "effect.cancel", O, {"proposal": "p3"}), "allow")
v.step(outc("o3", "c3", "cancelled", "no"), "allow", check={"effects.p3.phase": "cancelled"})
add(v.done())

v = V("E5-attempts-and-repeatable", ["P-07", "P-09"], "attempt numbering; failed-after-start is final unless repeatable")
team(v)
v.step(prop("p1"), "allow").step(claim("c0", "p1", attempt=2), "denied", note="wrong attempt number")
v.step(claim("c1", "p1"), "allow").step(claim("c1b", "p1", attempt=2), "denied", note="already claimed")
v.step(outc("o1", "c1", "failed", "yes"), "allow", check={"effects.p1.phase": "failed"})
v.step(claim("c2", "p1", attempt=2), "denied")
v.step(prop("p2", repeatable=True), "allow").step(claim("d1", "p2"), "allow")
v.step(outc("q1", "d1", "unknown", "unknown"), "allow", check={"effects.p2.phase": "ready"})
v.step(claim("d2", "p2", attempt=2), "allow")
add(v.done())

v = V("E6-budget-reserve-settle", ["P-06", "P-07", "P-09"], "cost reserved at claim, settled at outcome")
team(v, policy(budget=USD(1000)))
v.step(prop("p1", cost=USD(600)), "allow").step(prop("p2", cost=USD(600)), "allow")
v.step(claim("c1", "p1"), "allow", check={"budgetLeft": USD(400)})
v.step(claim("c2", "p2"), "denied", note="600 > 400 left")
v.step(outc("o1", "c1", "succeeded", "yes", cost=USD(700)), "invalid", note="cost above reservation")
v.step(outc("o2", "c1", "succeeded", "yes", cost=USD(200)), "allow", check={"budgetLeft": USD(800)})
v.step(claim("c3", "p2"), "allow", check={"budgetLeft": USD(200)})
v.step(outc("o3", "c3", "failed", "no"), "allow", check={"budgetLeft": USD(800)}, note="not started → full refund")
add(v.done())

v = V("E6-task-budget", ["P-06"], "an effect in a budgeted task charges the task")
team(v, policy(budget=USD(1000)))
v.step(E("t", "task", O, {"to": [P], "budget": USD(300)}), "allow")
v.step(prop("p1", cost=USD(250), task="t"), "allow").step(claim("c1", "p1"), "allow", check={"tasks.t.left": USD(50), "budgetLeft": USD(700)})
v.step(prop("p2", cost=USD(100), task="t"), "allow").step(claim("c2", "p2"), "denied")
add(v.done())

v = V("outcome-rules", ["P-09", "X-03"], "consistency, abandon after 40 s, resolve")
team(v)
v.step(prop("p"), "allow").step(claim("c", "p"), "allow", at=100)
v.step(outc("bad", "c", "succeeded", "no"), "invalid", at=101)
v.step(outc("bad2", "c", "failed", "unknown"), "invalid", at=101)
v.step(outc("ab0", "c", "unknown", "unknown", author=O), "denied", at=132, note="32 s < abandon_after(30) = 33 s")
v.step(outc("ab1", "c", "unknown", "unknown", author=M), "denied", at=133, note="only owners abandon")
v.step(outc("ab2", "c", "unknown", "unknown", author=O), "allow", at=133, check={"effects.p.phase": "unknown"})
v.step(outc("late", "c", "succeeded", "yes"), "denied", at=142, note="claim already has an outcome")
v.step(claim("c2", "p", attempt=2), "denied", at=143, note="unknown blocks retry")
v.step(E("r0", "effect.resolve", M, {"proposal": "p", "result": "failed"}), "denied", at=144)
v.step(E("r1", "effect.resolve", X, {"proposal": "p", "result": "failed"}), "allow", at=145, check={"effects.p.phase": "ready"})
v.step(claim("c3", "p", attempt=2), "allow", at=146)
add(v.done())

# ── P-12 redact, P-11 checkpoint, move, policy ──
v = V("redact", ["P-12", "R-04"], "author or owner may redact; redact cannot be redacted; policy state unchanged")
team(v)
v.step(E("msg", "message", M, {}), "allow")
v.step(E("r0", "redact", P, {"target": "msg"}), "denied")
v.step(E("r1", "redact", M, {"target": "msg"}), "allow")
v.step(E("r2", "redact", O, {"target": "r1"}), "denied")
v.step(E("r3", "redact", O, {"target": "m1"}), "allow", check={"roles.@m:a.example": "member"}, note="header kept: role unchanged")
add(v.done())

v = V("checkpoint-move-policy", ["P-11", "P-04"], "checkpoint bounds; only owners move or set policy")
team(v)
v.step(E("ck0", "checkpoint", M, {"through": 99}), "invalid")
v.step(E("ck1", "checkpoint", M, {"through": 4}), "allow")
v.step(E("mv0", "space.move", M, {"to": "b.example", "accept": ACCEPT}), "denied")
v.step(E("ps0", "policy.set", M, {"policy": policy()}), "denied")
v.step(E("mvx", "space.move", O, {"to": "b.example"}), "invalid", note="no acceptance from the target")
v.step(E("mv1", "space.move", O, {"to": "host.example", "accept": ACCEPT}), "allow", check={"home": "host.example"})
v.read("@:host.example", 9, True).read("@:other.example", 9, False)
add(v.done())

v = V("policy-budget-change", ["P-06"], "raising a budget keeps what was spent")
team(v, policy(budget=USD(1000)))
v.step(E("t", "task", O, {"to": [P], "budget": USD(300)}), "allow")
v.step(E("ps", "policy.set", O, {"policy": policy(budget=USD(2000))}), "allow", check={"budgetLeft": USD(1700)})
add(v.done())

# ── P-13 forks, P-14 successors ──
v = V("fork-fresh-authority", ["P-13"], "a fork carries no members, approvals or effects")
team(v)
v.step(prop("pr"), "allow")
F = "f1:b.example"
v.step(E("FC", "space.create", P, {"policy": policy(), "forkOf": {"space": S, "through": 5}}, space=F), "allow",
       check={"roles": {P: "owner"}, "effects": {}}, note="new space; check applies to it")
v.step(E("fx", "effect.claim", X, {"proposal": "pr", "attempt": 1, "instance": "k1", "window": 30}, space=F), "invalid", note="parent proposal not in fork")
add(v.done())



# ── profile spaces (§4.3) ──
PS = "profile:@bot:b.example"
v = V("profile-space", ["§4.3", "R-12"], "server creates; actor or server manages keys; operator confirms")
v.prof("@:b.example", human()).prof(BOT, {"actorKind": "agent", "operator": OP, "operatorConfirmed": False, "keys": {"k1": {"publicKey": "pk", "status": "valid"}}})
v.step(E("PC0", "space.create", BOT, {"policy": policy(), "profile": {"actorKind": "agent", "operator": OP, "key": {"keyId": "k1", "publicKey": "pk"}}}, space=PS), "denied")
v.step(E("PC", "space.create", "@:b.example", {"policy": policy(visibility="public"), "profile": {"actorKind": "agent", "operator": OP, "key": {"keyId": "k1", "publicKey": "pk"}}}, space=PS), "allow")
v.step(E("ka", "key.add", BOT, {"keyId": "k2", "publicKey": "pk2"}, space=PS), "allow")
v.step(E("kz", "key.add", Z, {"keyId": "k3", "publicKey": "pk3"}, space=PS), "denied")
v.step(E("kr", "key.revoke", "@:b.example", {"keyId": "k1"}, space=PS), "allow", check={"profile.keys.k1.status": "revoked"})
v.step(E("oc0", "member.set", Z, {"actor": Z, "role": "owner"}, space=PS), "denied")
v.step(E("oc", "member.set", OP, {"actor": OP, "role": "owner"}, space=PS), "allow", check={"profile.operatorConfirmed": True})
v.step(E("msg", "message", BOT, {}, space=PS), "denied")
add(v.done())

# ── P-01 relevant actors ──
v = V("relevant-actors", ["P-01"], "the receipt must pin profiles for these actors")
team(v, policy(approvals=[{"action": "*", "approvers": ["owner", M]}]))
v.step(prop("pr"), "allow").step(appr("a1", "pr"), "allow").step(appr("a2", "pr", author=M), "allow")
v.actors(claim("c", "pr"), [X, P, O, M])
v.actors(ms("z", O, Z, "member"), [O, Z])
add(v.done())

# ── round-3 vectors: attacks found by review round 2 ──
v = V("budget-cannot-be-bypassed", ["P-06", "P-07 E6", "P-09"], "in a budgeted space every effect must carry a cost in a budgeted unit")
team(v, policy(budget=USD(100)))
v.step(prop("p0"), "denied", note="no cost")
v.step(prop("p1", cost={"unit": "USD2", "amount": 1}), "denied", note="unbudgeted unit")
v.step(prop("p2", cost=USD(40)), "allow").step(claim("c2", "p2"), "allow", check={"budgetLeft": USD(60)})
v.step(outc("o2", "c2", "succeeded", "yes", cost=USD(99999)), "invalid", note="cost above reservation")
v.step(outc("o3", "c2", "succeeded", "yes", cost=USD(40)), "allow", check={"budgetLeft": USD(60)})
add(v.done())

v = V("budget-added-later", ["P-06", "P-07 E6", "P-09"], "an effect proposed before budgets existed can't claim, or report an unreserved cost, once they do")
team(v)
v.step(prop("p"), "allow").step(claim("c", "p"), "allow")
v.step(E("ps", "policy.set", O, {"policy": policy(budget=USD(100))}), "allow")
v.step(outc("o0", "c", "succeeded", "yes", cost=USD(50)), "invalid", note="unreserved cost in a budgeted unit")
v.step(outc("o1", "c", "succeeded", "yes"), "allow", check={"budgetLeft": USD(100)})
v.step(prop("q"), "denied", note="now needs a cost")
add(v.done())

v = V("agent-must-charge-its-task", ["P-06"], "an agent assigned a budgeted task can't spend from the space budget instead")
v.prof(BOT, {"actorKind": "agent", "operator": OP, "operatorConfirmed": True, "keys": {"k1": {"publicKey": "pk", "status": "valid"}}})
team(v, policy(budget=USD(1000)))
v.step(ms("b", O, BOT, "member"), "allow")
v.step(E("t", "task", O, {"to": [BOT], "budget": USD(100)}), "allow")
v.step(prop("p1", author=BOT, cost=USD(50)), "denied", note="must name its budgeted task")
v.step(prop("p2", author=BOT, cost=USD(50), task="t"), "allow")
v.step(prop("p3", author=M, cost=USD(50), task="t"), "denied", note="non-assignee can't charge someone else's task")
add(v.done())

v = V("banned-loses-task-authority", ["P-04", "P-05"], "removing or banning an actor removes its task and cancel authority")
team(v, policy(budget=USD(100)))
v.step(E("t", "task", O, {"to": [M], "budget": USD(50)}), "allow")
v.step(E("t2", "task", M, {"to": [P]}), "allow")
v.step(prop("p", author=M, cost=USD(10)), "allow")
v.step(ms("ban", O, M, "banned"), "allow")
v.step(E("u", "task.update", M, {"task": "t", "state": "progress"}), "denied")
v.step(E("d", "task", M, {"to": [P], "parent": "t", "budget": USD(50)}), "denied", note="can't drain parent budget")
v.step(E("c", "task.cancel", M, {"task": "t2"}), "denied")
v.step(E("x", "effect.cancel", M, {"proposal": "p"}), "denied")
v.step(claim("cl", "p"), "denied", note="E2: proposer banned")
add(v.done())

v = V("banned-executor", ["P-07 E1", "P-09"], "a removed executor can't claim, but may still report the truth about a claim it already made")
team(v)
v.step(prop("p1"), "allow").step(prop("p2"), "allow").step(claim("c1", "p1"), "allow")
v.step(ms("ban", O, X, "banned"), "allow")
v.step(claim("c2", "p2"), "denied")
v.step(outc("o1", "c1", "succeeded", "yes"), "allow", note="outcomes are facts; reporting stays allowed")
add(v.done())


v = V("keyid-single-use", ["§4.3", "R-12"], "a revoked keyId can never become valid again")
v.prof("@:b.example", human()).prof(BOT, {"actorKind": "agent", "operator": OP, "operatorConfirmed": False, "keys": {"k1": {"publicKey": "pk", "status": "valid"}, "k2": {"publicKey": "pk2", "status": "valid"}}})
v.step(E("PC", "space.create", "@:b.example", {"policy": policy(visibility="public"), "profile": {"actorKind": "agent", "operator": OP, "key": {"keyId": "k1", "publicKey": "pk"}}}, space=PS), "allow")
v.step(E("ka", "key.add", BOT, {"keyId": "k2", "publicKey": "pk2"}, space=PS), "allow")
v.step(E("kr", "key.revoke", BOT, {"keyId": "k1"}, space=PS, key="k2"), "allow")
v.step(E("kb", "key.add", "@:b.example", {"keyId": "k1", "publicKey": "pk-new"}, space=PS), "invalid")
v.step(E("kc", "key.revoke", BOT, {"keyId": "k1"}, space=PS, key="k2"), "invalid", note="already revoked")
add(v.done())

v = V("repeatable-unknown-settles", ["P-09"], "a repeatable effect's unknown attempt is charged in full before the retry")
team(v, policy(budget=USD(100)))
v.step(prop("p", cost=USD(40), repeatable=True), "allow").step(claim("c1", "p"), "allow")
v.step(outc("o1", "c1", "unknown", "unknown"), "allow", check={"effects.p.phase": "ready", "budgetLeft": USD(60)})
v.step(claim("c2", "p", attempt=2), "allow", check={"budgetLeft": USD(20)})
v.step(outc("o2", "c2", "succeeded", "yes", cost=USD(10)), "allow", check={"budgetLeft": USD(50)})
add(v.done())

v = V("approval-rule-removed", ["P-08", "P-11"], "removing the approval rule releases effects waiting for approval")
team(v, policy(approvals=[{"action": "*", "approvers": ["owner"]}]))
v.step(prop("p"), "allow", check={"effects.p.phase": "awaiting-approval"})
v.step(E("ps", "policy.set", O, {"policy": policy()}), "allow", check={"effects.p.phase": "ready"})
v.step(claim("c", "p"), "allow")
add(v.done())

v = V("no-move-while-claimed", ["P-11", "§7.3"], "a space can't move while an effect is claimed (its abandon clock would change homes)")
team(v)
v.step(prop("p"), "allow").step(claim("c", "p"), "allow")
v.step(E("mv0", "space.move", O, {"to": "b.example", "accept": ACCEPT}), "denied")
v.step(outc("o", "c", "succeeded", "yes"), "allow")
v.step(E("mv1", "space.move", O, {"to": "b.example", "accept": ACCEPT}), "allow")
add(v.done())

v = V("cancel-request-once", ["§6.4"], "a cancel request on a claimed effect is recorded once; a second is denied")
team(v)
v.step(prop("p"), "allow").step(claim("c", "p"), "allow")
v.step(E("x1", "effect.cancel", O, {"proposal": "p"}), "allow").step(E("x2", "effect.cancel", P, {"proposal": "p"}), "denied")
add(v.done())

v = V("result-started-table", ["P-09"], "only the listed (result, started) pairs are valid")
team(v)
v.step(prop("p"), "allow").step(claim("c", "p"), "allow")
for i, (r, st_, exp) in enumerate([("succeeded", "no", "invalid"), ("succeeded", "unknown", "invalid"), ("unknown", "no", "invalid"),
                                   ("failed", "unknown", "invalid"), ("cancelled", "unknown", "invalid")]):
    v.step(outc(f"bad{i}", "c", r, st_), exp)
v.step(outc("ok", "c", "unknown", "yes"), "allow", check={"effects.p.phase": "unknown"}, note="launched, result lost")
add(v.done())

v = V("extension-kinds", ["§5", "R-09"], "extension kinds are opaque to Policy: members may append them; unknown core-prefixed kinds are invalid")
team(v)
v.step(E("d1", "doc.version", M, {"base": "anything", "n": 1}), "allow")
v.step(E("d2", "doc.version", Z, {}), "denied", note="not a member")
v.step(E("d3", "task.bogus", M, {}), "invalid", note="core prefix: unknown kind")
v.step(E("d4", "doc.version", M, {"n": -1}), "invalid", note="opaque Json: no negative numbers")
v.step(E("d5", "doc.version", M, {"x": 1.5}), "invalid", note="opaque Json: no fractions")
add(v.done())

v = V("worked-example-1.1", ["§1.1", "P-03", "P-05", "P-06", "P-07", "P-08", "P-09"], "the architecture's worked example, step by step")
LARS, FIX, GIT = "@ada:a.example", "@fixer:b.example", "@git:c.example"
v.prof(FIX, {"actorKind": "agent", "operator": "@ana:b.example", "operatorConfirmed": True, "keys": {"k1": {"publicKey": "pk", "status": "valid"}}})
v.prof(GIT, {"actorKind": "service", "operator": "@ops:c.example", "operatorConfirmed": True, "keys": {"k1": {"publicKey": "pk", "status": "valid"}}})
v.step(create(policy(allowAgents="with-operator", approvals=[{"action": "git.push", "approvers": ["owner"]}], budget=USD(500)), author=LARS), "allow")
v.step(ms("j1", LARS, FIX, "member"), "allow").step(ms("j2", LARS, GIT, "member"), "allow")
v.step(E("t", "task", LARS, {"to": [FIX], "budget": USD(300)}), "allow", check={"budgetLeft": USD(200)})
v.step(E("acc", "task.update", FIX, {"task": "t", "state": "accepted"}), "allow")
v.step(prop("push", author=FIX, executor=GIT, cost=USD(50), task="t"), "allow", check={"effects.push.phase": "awaiting-approval"})
v.step(appr("ok", "push", author=LARS), "allow", check={"effects.push.phase": "ready"})
v.step(claim("cl", "push", author=GIT), "allow", check={"tasks.t.left": USD(250)})
v.step(outc("out", "cl", "succeeded", "yes", author=GIT, cost=USD(0)), "allow", check={"tasks.t.left": USD(300), "effects.push.phase": "succeeded"})
v.step(E("done", "task.update", FIX, {"task": "t", "state": "done"}), "allow", check={"budgetLeft": USD(500)})
add(v.done())

# ── round-4 vectors: findings of review round 3 ──
v = V("budgeted-child-of-unbudgeted-task", ["P-05"], "a budgeted subtask under an unbudgeted parent reserves from the space")
team(v, policy(budget=USD(100)))
v.step(E("t", "task", O, {"to": [P]}), "allow")
v.step(E("c", "task", P, {"to": [X], "parent": "t", "budget": USD(30)}), "allow", check={"budgetLeft": USD(70), "tasks.c.left": USD(30)})
v.step(E("d", "task.update", X, {"task": "c", "state": "done"}), "allow", check={"budgetLeft": USD(100)})
add(v.done())

v = V("owner-force-closes-task", ["P-05"], "an owner may close any open task with a final update; its budget is refunded")
team(v, policy(budget=USD(100)))
v.step(E("t", "task", O, {"to": [M], "budget": USD(60)}), "allow", check={"budgetLeft": USD(40)})
v.step(ms("ban", O, M, "banned"), "allow")
v.step(E("u0", "task.update", O, {"task": "t", "state": "progress"}), "denied", note="owners may only close, not report progress")
v.step(E("u1", "task.update", O, {"task": "t", "state": "cancelled"}), "allow", check={"budgetLeft": USD(100), "tasks.t.open": False})
add(v.done())

v = V("operator-must-be-another-human", ["§4.3"], "an agent can't operate itself, and operators must be human; operators can withdraw")
PS2 = "profile:@bot2:b.example"
v.prof("@:b.example", human()).prof("@bot2:b.example", {"actorKind": "agent", "operatorConfirmed": False, "keys": {"k1": {"publicKey": "pk", "status": "valid"}}})
v.prof(BOT, {"actorKind": "agent", "operator": OP, "operatorConfirmed": True, "keys": {"k1": {"publicKey": "pk", "status": "valid"}}})
v.step(E("P0", "space.create", "@:b.example", {"policy": policy(visibility="public"), "profile": {"actorKind": "agent", "operator": "@bot2:b.example", "key": {"keyId": "k1", "publicKey": "pk"}}}, space=PS2), "invalid", note="its own operator")
v.step(E("P1", "space.create", "@:b.example", {"policy": policy(visibility="public"), "profile": {"actorKind": "agent", "operator": BOT, "key": {"keyId": "k1", "publicKey": "pk"}}}, space=PS2), "allow")
v.step(E("c0", "member.set", BOT, {"actor": BOT, "role": "owner"}, space=PS2), "denied", note="an agent can't be an operator")
v.step(E("P2", "space.create", "@:b.example", {"policy": policy(visibility="public"), "profile": {"actorKind": "agent", "operator": OP, "key": {"keyId": "k1", "publicKey": "pk"}}}, space="profile:@bot3:b.example"), "allow")
v.step(E("c1", "member.set", OP, {"actor": OP, "role": "owner"}, space="profile:@bot3:b.example"), "allow", check={"profile.operatorConfirmed": True})
v.step(E("c2", "member.set", OP, {"actor": OP, "role": "none"}, space="profile:@bot3:b.example"), "allow", check={"profile.operatorConfirmed": False})
add(v.done())

v = V("resolve-cost-rules", ["P-06", "P-10"], "resolve can't report a cost the claim didn't reserve")
team(v, policy(budget=USD(100)))
v.step(prop("p", cost=USD(40)), "allow").step(claim("c", "p"), "allow", at=100)
v.step(outc("ab", "c", "unknown", "unknown", author=O), "allow", at=133)
v.step(E("r0", "effect.resolve", O, {"proposal": "p", "result": "succeeded", "cost": USD(50)}), "invalid", note="more than reserved")
v.step(E("r1", "effect.resolve", O, {"proposal": "p", "result": "succeeded", "cost": USD(25)}), "allow", check={"budgetLeft": USD(75)})
add(v.done())



v = V("budget-unit-locked-while-reserved", ["P-06", "P-11"], "the budget unit can't change while anything is reserved")
team(v, policy(budget=USD(100)))
v.step(E("t", "task", O, {"to": [P], "budget": USD(10)}), "allow")
v.step(E("ps0", "policy.set", O, {"policy": policy(budget={"unit": "EUR-cent", "amount": 100})}), "denied")
v.step(E("ps1", "policy.set", O, {"policy": policy(budget=USD(200))}), "allow", check={"budgetLeft": USD(190)})
v.step(E("d", "task.update", P, {"task": "t", "state": "done"}), "allow")
v.step(E("ps2", "policy.set", O, {"policy": policy(budget={"unit": "EUR-cent", "amount": 100})}), "allow", check={"budgetLeft": {"unit": "EUR-cent", "amount": 100}})
add(v.done())

v = V("no-redacting-live-plans", ["P-12"], "the plan of an unfinished effect can't be redacted")
team(v)
v.step(prop("p"), "allow").step(E("r0", "redact", P, {"target": "p"}), "denied")
v.step(E("x", "effect.cancel", P, {"proposal": "p"}), "allow").step(E("r1", "redact", P, {"target": "p"}), "allow")
add(v.done())

v = V("launch-window-is-policy", ["P-09", "X-04"], "the abandon threshold follows policy.launchWindow")
team(v, policy(launchWindow=120))
v.step(prop("p"), "allow").step(claim("cw", "p", window=30), "denied", at=99, note="the claim must state the space's window")
v.step(claim("c", "p", window=120), "allow", at=100)
v.step(outc("a0", "c", "unknown", "unknown", author=O), "denied", at=224, note="abandon_after(120) = ⌈120·1.01/0.99⌉ + 2 = 125 s")
v.step(outc("a1", "c", "unknown", "unknown", author=O), "allow", at=225)
add(v.done())

v = V("budget-cut-below-committed", ["P-06", "P-11"], "cutting a budget below what is committed leaves a negative remainder; nothing new reserves until refunds")
team(v, policy(budget=USD(100)))
v.step(E("t", "task", O, {"to": [P], "budget": USD(60)}), "allow", check={"budgetLeft": USD(40)})
v.step(E("ps", "policy.set", O, {"policy": policy(budget=USD(50))}), "allow", check={"budgetLeft": {"unit": "USD-cent", "amount": -10}})
v.step(E("t2", "task", O, {"to": [P], "budget": USD(1)}), "denied", note="nothing left")
v.step(E("d", "task.update", P, {"task": "t", "state": "done"}), "allow", check={"budgetLeft": USD(50)})
add(v.done())

# ── round-5 vectors: findings of review round 4 ──
v = V("window-pinned-in-claim", ["P-09", "X-04"], "shortening launchWindow after a claim doesn't shorten that claim's abandon wait")
team(v, policy(launchWindow=3600))
v.step(prop("p"), "allow").step(claim("c", "p", window=3600), "allow", at=100, check={"effects.p.claims.0.window": 3600})
v.step(E("ps", "policy.set", O, {"policy": policy(launchWindow=15)}), "allow", at=101)
v.step(outc("a0", "c", "unknown", "unknown", author=O), "denied", at=117, note="the claim was made under a 3600 s window")
v.step(outc("a0b", "c", "unknown", "unknown", author=O), "denied", at=100 + 3674, note="abandon_after(3600) = 3675 s")
v.step(outc("a1", "c", "unknown", "unknown", author=O), "allow", at=100 + 3675)
add(v.done())

v = V("proposal-expiry", ["P-07 E5"], "a proposal with expiresAt can't be claimed after it")
team(v)
p_ = prop("p"); p_["body"]["expiresAt"] = "2026-10-10T12:01:00Z"
v.step(p_, "allow").step(claim("c0", "p"), "denied", at=60, note="at expiry").step(claim("c1", "p"), "allow", at=59)
add(v.done())

v = V("denied-server-users-can-be-removed", ["P-03", "P-04"], "denying a server never blocks removing its users, or its executor's fact reports")
v.prof("@x:d.example", dict(human(), actorKind="service", operatorConfirmed=True))
team(v)
v.step(ms("j", O, Z, "member"), "allow").step(ms("jx", O, "@x:d.example", "member"), "allow")
v.step(prop("p", executor="@x:d.example"), "allow").step(claim("c", "p", author="@x:d.example"), "allow")
v.step(E("ps", "policy.set", O, {"policy": policy(servers={"deny": ["d.example"]})}), "allow")
v.step(ms("b", O, Z, "banned"), "allow", check={"roles.@z:d.example": "banned"})
v.step(ms("b2", O, Z, "member"), "denied", note="can't be re-granted a role")
v.step(outc("o", "c", "succeeded", "yes", author="@x:d.example"), "allow", note="facts are still reported")
v.step(E("m", "message", "@x:d.example", {}), "denied", note="but nothing else")
add(v.done())

v = V("agent-cannot-self-fund", ["P-05", "P-06"], "an agent with a budgeted task can't open a new top-level budgeted task for itself")
v.prof(BOT, {"actorKind": "agent", "operator": OP, "operatorConfirmed": True, "keys": {"k1": {"publicKey": "pk", "status": "valid"}}})
team(v, policy(budget=USD(500)))
v.step(ms("b", O, BOT, "member"), "allow")
v.step(E("t", "task", O, {"to": [BOT], "budget": USD(300)}), "allow")
v.step(E("t2", "task", BOT, {"to": [BOT], "budget": USD(200)}), "denied", note="would escape its 300")
v.step(E("t3", "task", BOT, {"to": [BOT], "budget": USD(100), "parent": "t"}), "allow", note="beneath its own task is fine")
add(v.done())

v = V("operator-withdrawal-stops-agent", ["P-03", "P-07 E1"], "under with-operator, an agent whose confirmation is withdrawn can't propose or claim")
CONF = {"actorKind": "agent", "operator": OP, "operatorConfirmed": True, "keys": {"k1": {"publicKey": "pk", "status": "valid"}}}
v.prof(BOT, CONF).prof(X, dict(human(), actorKind="service", operatorConfirmed=True))
team(v, policy(allowAgents="with-operator"))
v.step(ms("b", O, BOT, "member"), "allow")
v.step(prop("p1", author=BOT), "allow")
v.step(prop("p2", author=BOT), "denied", profiles={BOT: dict(CONF, operatorConfirmed=False)})
add(v.done())

v = V("impossible-dates", ["R-09"], "times must be real dates")
team(v)
a_ = appr("a", "x"); a_["body"]["expiresAt"] = "2026-13-45T99:99:99Z"
v.step(prop("x"), "allow").step(a_, "invalid")
v.step(E("t", "task", O, {"to": [M], "deadline": "2026-02-30T00:00:00Z"}), "invalid")
v.step(E("t2", "task", O, {"to": [M], "deadline": "2028-02-29T23:59:59Z"}), "allow", note="a real leap day")
add(v.done())

v = V("agent-cannot-close-its-way-out", ["P-06"], "an agent that closes its own budgeted task still can't spend from the space budget")
v.prof(BOT, {"actorKind": "agent", "operator": OP, "operatorConfirmed": True, "keys": {"k1": {"publicKey": "pk", "status": "valid"}}})
team(v, policy(budget=USD(500)))
v.step(ms("b", O, BOT, "member"), "allow")
v.step(E("t", "task", O, {"to": [BOT], "budget": USD(100)}), "allow")
v.step(prop("p1", author=BOT, cost=USD(450), task="t"), "allow", note="proposing is fine; the claim would exceed the task")
v.step(E("d", "task.update", BOT, {"task": "t", "state": "done"}), "allow")
v.step(prop("p2", author=BOT, cost=USD(450)), "denied", note="agents spend only from a budgeted task assigned to them")
add(v.done())

v = V("extension-role", ["§5", "P-04"], "policy.extensionsMinRole limits who may append extension kinds")
team(v)
v.step(ms("g", O, G, "guest"), "allow")
v.step(E("x1", "doc.version", G, {}), "denied", note="default minimum role is member")
v.step(E("ps", "policy.set", O, {"policy": policy(extensionsMinRole="guest")}), "allow")
v.step(E("x2", "doc.version", G, {}), "allow")
add(v.done())

# ── round-7 vectors: findings of review round 6 ──
AG = {"actorKind": "agent", "operator": OP, "operatorConfirmed": True, "keys": {"k1": {"publicKey": "pk", "status": "valid"}}}
XS = dict(human(), actorKind="service", operatorConfirmed=True)

v = V("operator-withdrawn-before-claim", ["P-07 E2", "P-03"], "if an agent's operator withdraws between its proposal and the claim, the claim is denied")
v.prof(BOT, AG).prof(X, XS)
team(v, policy(allowAgents="with-operator"))
v.step(ms("b", O, BOT, "member"), "allow").step(prop("p", author=BOT), "allow")
v.step(claim("c", "p"), "denied", profiles={BOT: dict(AG, operatorConfirmed=False)})
add(v.done())

v = V("owners-are-human", ["P-03"], "an agent or service can't be made an owner")
v.prof(BOT, AG)
team(v)
v.step(ms("b", O, BOT, "member"), "allow").step(ms("b2", O, BOT, "owner"), "denied")
add(v.done())

v = V("no-closing-over-running-effects", ["P-05"], "an assignee can't close a task while an effect charged to it is claimed or unknown; an owner can force-close")
team(v)
v.step(E("t", "task", O, {"to": [P]}), "allow").step(prop("p", task="t"), "allow").step(claim("c", "p"), "allow")
v.step(E("d0", "task.update", P, {"task": "t", "state": "done"}), "denied")
v.step(outc("o", "c", "succeeded", "yes"), "allow").step(E("d1", "task.update", P, {"task": "t", "state": "done"}), "allow")
add(v.done())

v = V("agents-off-stops-agents", ["P-03"], "switching allowAgents to none stops agents already in the space from proposing")
v.prof(BOT, AG)
team(v)
v.step(ms("b", O, BOT, "member"), "allow").step(prop("p1", author=BOT), "allow")
v.step(E("ps", "policy.set", O, {"policy": policy(allowAgents="none")}), "allow").step(prop("p2", author=BOT), "denied")
add(v.done())

v = V("cant-deny-own-home", ["P-11"], "a policy.set can't deny the home's own server or every owner's server")
team(v)
v.step(E("ps0", "policy.set", O, {"policy": policy(servers={"deny": ["a.example"]})}), "denied")
v.step(E("ps1", "policy.set", O, {"policy": policy(servers={"deny": ["d.example"]})}), "allow")
v.step(E("mv", "space.move", O, {"to": "host.example", "accept": ACCEPT}), "allow")
v.step(E("ps2", "policy.set", O, {"policy": policy(servers={"deny": ["host.example"]})}), "denied", note="the home is host.example, which no owner is on")
add(v.done())

v = V("abandon-never-rereadies", ["P-09"], "an owner's abandon leaves even a repeatable effect unknown; only the executor's own unknown re-readies it")
team(v)
v.step(prop("p", repeatable=True), "allow").step(claim("c", "p"), "allow", at=100)
v.step(outc("ab", "c", "unknown", "unknown", author=O), "allow", at=133, check={"effects.p.phase": "unknown"})
add(v.done())

v = V("open-approvals-capped", ["P-08", "P-01"], "at most 16 unconsumed approvals per proposal")
team(v, policy(approvals=[{"action": "*", "approvers": ["owner", "member"]}]))
v.step(prop("p"), "allow")
for i in range(16):
    v.step(appr(f"a{i}", "p", author=O if i % 2 else M), "allow")
v.step(appr("a16", "p"), "denied")
add(v.done())

v = V("missing-references-invalid", ["R-05", "P-05"], "a body reference to an entry that isn't in the space is invalid, not denied")
team(v)
v.step(E("t", "task", M, {"to": [P], "parent": hashlib.sha256(b"nope-task").hexdigest()}), "invalid")
v.step(prop("p", task=hashlib.sha256(b"nope-task2").hexdigest()), "invalid")
add(v.done())

v = V("repeatable-cancelled-started", ["P-09"], "a repeatable effect stopped partway (cancelled, started:yes) with no cancel requested goes back to ready")
team(v)
v.step(prop("p", repeatable=True), "allow").step(claim("c", "p"), "allow")
v.step(outc("o", "c", "cancelled", "yes"), "allow", check={"effects.p.phase": "ready"})
add(v.done())

# ── round-8/9 vectors: live sessions, steering and taint ──
ALL = {"watch": True, "read": True, "steer": True}
NONE = {"watch": False, "read": False, "steer": False}
MODES = ["auto", "guarded", "suggest"]
def sess(w, r, st, modes=MODES): return {"watch": w, "read": r, "steer": st, "modes": list(modes)}
BOT2 = "@bot2:b.example"

v = V("session-rights", ["P-15"], "watch/read/steer follow policy.sessions; the actor has all three; a session whose actor lost its role is dead")
team(v, policy(sessions=sess("guest", "member", "owner")))
v.step(ms("g", O, G, "guest"), "allow")
v.step(E("s", "session.open", M, {"steerMode": "auto"}), "allow")
v.access(M, "s", ALL).access(O, "s", ALL).access(P, "s", {"watch": True, "read": True, "steer": False})
v.access(G, "s", {"watch": True, "read": False, "steer": False}).access(Z, "s", NONE)
v.step(E("i0", "session.input", P, {"session": "s"}), "denied", note="members may not steer here")
v.step(E("i1", "session.input", O, {"session": "s"}), "allow", check={"sessions.s.steers": 1})
v.step(ms("rm", O, M, "none"), "allow")
v.access(O, "s", NONE).step(E("i2", "session.input", O, {"session": "s"}), "denied", note="its actor lost its role: dead")
v.step(E("c0", "session.close", P, {"session": "s"}), "denied").step(E("c1", "session.close", O, {"session": "s"}), "allow")
add(v.done())

v = V("sessions-nobody", ["P-15"], "a right set to nobody is granted to no one but the session's actor")
team(v, policy(sessions=sess("nobody", "nobody", "nobody")))
v.step(E("s", "session.open", M, {"steerMode": "auto"}), "allow").access(O, "s", NONE).access(M, "s", ALL)
add(v.done())

v = V("session-policy-shape", ["P-15", "P-11"], "read and steer can't be wider than watch; modes is a non-empty set; only allowed modes open")
team(v)
v.step(E("p1", "policy.set", O, {"policy": policy(sessions=sess("member", "guest", "owner"))}), "invalid", note="read wider than watch")
v.step(E("p2", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member", []))}), "invalid", note="no modes")
v.step(E("p3", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member", ["guarded"]))}), "allow")
v.step(E("s1", "session.open", M, {"steerMode": "auto"}), "denied", note="owners require guarded")
v.step(E("s2", "session.open", M, {"steerMode": "guarded"}), "allow")
add(v.done())

def agents(v, pol):
    """team plus OP (human member) operating BOT and BOT2 (agents, members)."""
    v.prof(BOT, AG).prof(BOT2, AG).prof(X, XS)
    team(v, pol)
    v.step(ms("op", O, OP, "member"), "allow").step(ms("b", O, BOT, "member"), "allow").step(ms("b2", O, BOT2, "member"), "allow")

v = V("guarded-steer-holds", ["P-15", "P-07 E3", "R-24"], "an outside steer into a guarded agent session puts the agent on hold: everything it proposes needs its operator")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(prop("p0", author=BOT), "allow", check={"effects.p0.phase": "ready"}, note="untainted")
v.step(E("in", "session.input", M, {"session": "s"}), "allow")
v.step(prop("p", author=BOT), "allow", check={"effects.p.phase": "awaiting-approval", "effects.p.guards": [OP]}, note="no tag needed")
v.step(claim("c0", "p"), "denied", note="the operator hasn't approved")
v.step(appr("a0", "p", author=M), "denied", note="not the guard and no approval rule")
v.step(appr("a1", "p", author=OP, expires="2026-10-10T12:00:25Z"), "allow", check={"effects.p.phase": "ready"})
v.step(claim("c1", "p"), "denied", at=40, note="the guard's approval expired")
v.step(appr("a2", "p", author=OP), "allow")
v.step(claim("c2", "p"), "allow", check={"effects.p.phase": "claimed"})
add(v.done())

OP2 = "@op2:b.example"
AG2 = dict(AG, operator=OP2)

v = V("guard-takes-the-wheel", ["P-15"], "auto adds no hold; suggest holds like guarded; the agent's own input never releases; humans are never held")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s", "session.open", BOT, {"steerMode": "auto"}), "allow").step(E("i", "session.input", M, {"session": "s"}), "allow")
v.step(prop("p1", author=BOT), "allow", check={"effects.p1.phase": "ready"})
v.step(E("g", "session.open", BOT, {"steerMode": "suggest"}), "allow").step(E("i2", "session.input", M, {"session": "g"}), "allow")
v.step(prop("p2", author=BOT), "allow", check={"effects.p2.guards": [OP]}, note="suggest holds like guarded")
v.step(E("i3", "session.input", BOT, {"session": "g"}), "allow")
v.step(E("i3r", "session.input", BOT, {"session": "g", "release": True}), "denied", note="an agent can't release itself")
v.step(E("h", "session.open", M, {"steerMode": "guarded"}), "allow").step(E("i5", "session.input", P, {"session": "h"}), "allow")
v.step(prop("p5", author=M), "allow", check={"effects.p5.phase": "ready"}, note="humans are their own guard")
add(v.done())

v = V("held-agents-instruct-no-one", ["P-15"], "a held agent may talk, report and propose, but may not steer others, create tasks, approve or govern: nothing to launder through")
agents(v, policy(sessions=sess("member", "member", "member"), approvals=[{"action": "git.push", "approvers": ["member"]}]))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("s2", "session.open", BOT2, {"steerMode": "auto"}), "allow")
v.step(prop("pm", author=M), "allow")
v.step(E("in", "session.input", M, {"session": "s"}), "allow", check={"held": {BOT: OP}})
v.step(E("relay", "session.input", BOT, {"session": "s2"}), "denied", note="no relaying into another agent's session")
v.step(E("t", "task", BOT, {"to": [BOT2]}), "denied", note="no delegating")
v.step(appr("ap", "pm", author=BOT), "denied", note="a held agent can't approve")
v.step(E("x", "doc.edit", BOT, {}), "denied", note="nor extensions")
v.step(E("msg", "message", BOT, {"to": [BOT2]}), "allow", note="talking is data (R-24)")
v.step(E("own", "session.input", BOT, {"session": "s"}), "allow", note="working its own session")
v.step(prop("q", author=BOT), "allow", check={"effects.q.guards": [OP], "effects.q.phase": "awaiting-approval"})
add(v.done())

v = V("only-release-releases", ["P-15"], "a guard's ordinary input doesn't clear the hold; only an explicit release by that guard does, even on a closed session")
agents(v, policy(sessions=sess("member", "member", "owner")))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(E("in", "session.input", O, {"session": "s"}), "allow", check={"held": {BOT: OP}})
v.step(E("q", "session.input", OP, {"session": "s"}), "allow", check={"held": {BOT: OP}}, note="the guard may steer even though steer is owner-only; 'status?' doesn't release")
v.step(E("r0", "session.input", M, {"session": "s", "release": True}), "denied", note="only the guard releases")
v.step(E("c", "session.close", BOT, {"session": "s"}), "allow", check={"held": {BOT: OP}}, note="closing doesn't escape")
v.step(prop("p1", author=BOT), "allow", check={"effects.p1.guards": [OP]})
v.step(E("r1", "session.input", OP, {"session": "s", "release": True}), "allow", check={"held": {}})
v.step(E("r2", "session.input", OP, {"session": "s", "release": True}), "denied", note="not held")
v.step(prop("p2", author=BOT), "allow", check={"effects.p2.phase": "ready"})
add(v.done())

v = V("modes-apply-to-open-sessions", ["P-15"], "restricting modes changes how already-open sessions treat outside steers; agents with no guard then go dead")
v.prof(BOT, AG).prof(BOT2, dict(AG, operatorConfirmed=False))
team(v, policy(sessions=sess("member", "member", "member")))
v.step(ms("op", O, OP, "member"), "allow").step(ms("b", O, BOT, "member"), "allow").step(ms("b2", O, BOT2, "member"), "allow")
v.step(E("s", "session.open", BOT, {"steerMode": "auto"}), "allow").step(E("s2", "session.open", BOT2, {"steerMode": "auto"}), "allow")
v.step(E("ps", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member", ["guarded"]))}), "allow")
v.step(E("in", "session.input", M, {"session": "s"}), "allow", check={"held": {BOT: OP}}, note="its auto session now acts as guarded")
v.access(M, "s2", NONE).step(E("in2", "session.input", M, {"session": "s2"}), "denied", note="no guard: dead under guarded-only")
add(v.done())

v = V("two-operators", ["P-15"], "each agent is held for its own operator; another operator can neither release nor approve for it")
v.prof(BOT, AG).prof(BOT2, AG2).prof(X, XS)
team(v, policy(sessions=sess("member", "member", "member")))
v.step(ms("op", O, OP, "member"), "allow").step(ms("op2", O, OP2, "member"), "allow")
v.step(ms("b", O, BOT, "member"), "allow").step(ms("b2", O, BOT2, "member"), "allow")
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("s2", "session.open", BOT2, {"steerMode": "guarded"}), "allow")
v.step(E("in", "session.input", OP2, {"session": "s"}), "allow", check={"held": {BOT: OP}}, note="OP2 is an outsider to BOT")
v.step(E("r", "session.input", OP2, {"session": "s", "release": True}), "denied")
v.step(prop("p", author=BOT), "allow").step(appr("a2", "p", author=OP2), "denied", note="not BOT's guard, no rule")
v.step(appr("a1", "p", author=OP), "allow").step(claim("c", "p"), "allow")
add(v.done())

v = V("guards-are-checked", ["P-15", "P-04", "R-12", "P-07 E3"], "a guard's approval needs a role, a valid key and no expiry, like any approval; guards may refuse")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("in", "session.input", M, {"session": "s"}), "allow")
v.step(prop("p", author=BOT), "allow").step(appr("a1", "p", author=OP), "allow")
v.step(claim("c0", "p"), "denied", profiles={OP: human({"k1": {"publicKey": "pk", "status": "revoked"}})}, note="guard's key revoked")
v.step(ms("ban", O, OP, "none"), "allow")
v.step(claim("c1", "p"), "denied", profiles={OP: human()}, note="guard lost its role")
v.step(appr("a2", "p", author=OP), "denied", note="a guard with no role can't approve")
v.step(prop("p2", author=BOT), "allow")
v.step(ms("back", O, OP, "member"), "allow")
v.step(E("x", "effect.cancel", OP, {"proposal": "p2"}), "allow", profiles={OP: human()}, check={"effects.p2.phase": "cancelled"}, note="a guard may refuse")
add(v.done())

v = V("guarded-needs-an-operator", ["P-15"], "an agent may open a guarded or suggest session only if its confirmed operator holds a role")
v.prof(BOT, AG)
team(v, policy(sessions=sess("member", "member", "member")))
v.step(ms("b", O, BOT, "member"), "allow")
v.step(E("s1", "session.open", BOT, {"steerMode": "guarded"}), "denied", note="OP isn't in the space")
v.step(E("s2", "session.open", BOT, {"steerMode": "auto"}), "allow")
v.step(E("t", "task", O, {"to": [M]}), "allow").step(E("s3", "session.open", BOT, {"steerMode": "auto", "task": "t"}), "denied", note="not its task")
add(v.done())

# ── R-01 id derivation ──
ENC = {"name": "entry-id", "rules": ["R-01", "§2"], "description": "id = sha256('han-entry-v1\\n' + JCS(entry minus sig)); test the exact preimage",
       "entry": {"v": 1, "space": S, "kind": "message", "author": M, "keyId": "k1", "body": {"to": [O]}, "contentHash": "",
                 "refs": [], "seen": 3, "nonce": "aaaaaaaaaaaaaaaaaaaaaaaaaa", "time": "2026-10-10T12:00:00Z", "sig": "ignored"},
       "preimage": "han-entry-v1\n" + '{"author":"@m:a.example","body":{"to":["@o:a.example"]},"contentHash":"","keyId":"k1","kind":"message","nonce":"aaaaaaaaaaaaaaaaaaaaaaaaaa","refs":[],"seen":3,"space":"s1:a.example","time":"2026-10-10T12:00:00Z","v":1}'}

# RFC 8785 orders object keys by UTF-16 code units: U+1F600 (D83D DE00) sorts before U+FFFD. Hand-written preimage.
JCS16 = {"name": "jcs-utf16-key-order", "rules": ["§2"], "description": "keys are ordered by UTF-16 code units, not code points",
         "entry": {"v": 1, "space": S, "kind": "x.note", "author": M, "keyId": "k1", "body": {"�": 1, "\U0001F600": 2},
                   "contentHash": "", "refs": [], "seen": 0, "nonce": "aaaaaaaaaaaaaaaaaaaaaaaaaa", "time": "2026-10-10T12:00:00Z"},
         "preimage": "han-entry-v1\n" + '{"author":"@m:a.example","body":{"\U0001F600":2,"�":1},"contentHash":"","keyId":"k1","kind":"x.note","nonce":"aaaaaaaaaaaaaaaaaaaaaaaaaa","refs":[],"seen":0,"space":"s1:a.example","time":"2026-10-10T12:00:00Z","v":1}'}

if __name__ == "__main__":
    os.makedirs(OUT, exist_ok=True)
    for f in os.listdir(OUT):
        if f.endswith(".json"): os.remove(os.path.join(OUT, f))
    for i, vec in enumerate(VECTORS, 1):
        with open(os.path.join(OUT, f"{i:02d}-{vec['name']}.json"), "w", encoding="utf-8") as fh:
            json.dump(vec, fh, indent=1, ensure_ascii=False)
    with open(os.path.join(OUT, "00-entry-id.json"), "w", encoding="utf-8") as fh:
        json.dump(ENC, fh, indent=1, ensure_ascii=False)
    with open(os.path.join(OUT, "00-jcs-utf16-order.json"), "w", encoding="utf-8") as fh:
        json.dump(JCS16, fh, indent=1, ensure_ascii=False)
    print(f"wrote {len(VECTORS) + 2} vectors")

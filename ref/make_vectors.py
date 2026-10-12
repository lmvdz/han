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
         "separateApprover": False, "budget": None, "maxDelegationDepth": 4, "minDurability": "local", "launchWindow": 30, "extensionsMinRole": "member", "sessions": {"watch": "member", "read": "member", "steer": "owner", "modes": ["auto", "guarded"]},
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
        self.heads = {}                  # space → seq of its last allowed entry (for `seen`)
    def prof(self, actor, view):
        self.v["profiles"][actor] = view; return self
    def step(self, entry, expect, *, dt=1, at=None, check=None, profiles=None, predecessor=None, note=None):
        self.t = at if at is not None else self.t + dt
        st = {"entry": entry, "time": (BASE + timedelta(seconds=self.t)).strftime("%Y-%m-%dT%H:%M:%SZ"), "expect": expect}
        if check: st["check"] = check
        if profiles: st["profiles"] = profiles
        if predecessor: st["predecessor"] = predecessor
        if note: st["note"] = note
        if expect == "allow": self.heads[entry["space"]] = self.heads.get(entry["space"], 0) + 1
        self.v["steps"].append(st); return self
    def rel(self, id, author, agent, seen=None, space=S, unguard=False):
        """A guard's `agent.release`, signed having seen the space up to its current head (or `seen`); `unguard` also ends its agent's guarded choice."""
        e = E(id, "agent.release", author, dict({"agent": agent}, **({"unguard": True} if unguard else {})), space=space)
        e["seen"] = self.heads.get(space, 0) if seen is None else seen
        return e
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
ACCEPT = {"keyId": "h1", "sig": "signature-checked-by-Log-and-mirrors"}   # Policy checks only its shape (§7.3)

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

v = V("role-history-is-capped", ["P-02"], "roleHistory keeps the 64 most recent intervals per actor; a seq in a dropped interval is no longer readable (fails closed)")
team(v)                                           # M holds a role from seq 2
v.step(ms("r0", O, M, "none"), "allow")           # interval [2,5)
for k in range(1, 71):                            # interval k is [4+2k, 5+2k)
    v.step(ms(f"a{k}", O, M, "member"), "allow").step(ms(f"r{k}", O, M, "none"), "allow")
v.read(M, 2, False).read(M, 16, False).read(M, 18, True).read(M, 17, False).read(M, 144, True).read(M, 145, False)
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
MODES = ["auto", "guarded"]
def sess(w, r, st, modes=MODES, force=None): return dict({"watch": w, "read": r, "steer": st, "modes": list(modes)}, **({} if force is None else {"forceGuarded": force}))
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

v = V("guard-takes-the-wheel", ["P-15"], "auto adds no hold; one guarded session guards the whole agent; an agent can't release itself; humans are never held")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s", "session.open", BOT, {"steerMode": "auto"}), "allow").step(E("i", "session.input", M, {"session": "s"}), "allow")
v.step(prop("p1", author=BOT), "allow", check={"effects.p1.phase": "ready"})
v.step(E("g", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("i2", "session.input", M, {"session": "s"}), "allow", check={"held": {BOT: OP}})
v.step(prop("p2", author=BOT), "allow", check={"effects.p2.guards": [OP]}, note="steering its auto side-session holds it once any of its sessions is guarded")
v.step(E("i3", "session.input", BOT, {"session": "g"}), "allow")
v.step(v.rel("i3r", BOT, BOT), "denied", note="an agent can't release itself")
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

v = V("only-release-releases", ["P-15"], "a guard's ordinary input doesn't clear the hold; only an explicit agent.release by that guard does, with or without a session")
agents(v, policy(sessions=sess("member", "member", "owner")))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(E("in", "session.input", O, {"session": "s"}), "allow", check={"held": {BOT: OP}})
v.step(E("q", "session.input", OP, {"session": "s"}), "allow", check={"held": {BOT: OP}}, note="the guard may steer even though steer is owner-only; 'status?' doesn't release")
v.step(v.rel("r0", M, BOT), "denied", note="only the guard releases")
v.step(E("c", "session.close", BOT, {"session": "s"}), "allow", check={"held": {BOT: OP}}, note="closing doesn't escape")
v.step(prop("p1", author=BOT), "allow", check={"effects.p1.guards": [OP]})
v.step(v.rel("ro", O, BOT), "denied", note="owners can't override a guard who is still here")
v.step(v.rel("r1", OP, BOT), "allow", check={"held": {}})
v.step(v.rel("r2", OP, BOT), "denied", note="not held")
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
v.step(v.rel("r", OP2, BOT), "denied")
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
v.step(prop("p2", author=BOT), "allow")
v.step(appr("a2", "p2", author=OP), "denied", note="a guard with no role can't approve")
v.step(ms("back", O, OP, "member"), "allow")
v.step(E("x", "effect.cancel", OP, {"proposal": "p2"}), "allow", profiles={OP: human()}, check={"effects.p2.phase": "cancelled"}, note="a guard may refuse")
add(v.done())

v = V("guarded-needs-an-operator", ["P-15"], "an agent may open a guarded session only if its confirmed operator holds a role")
v.prof(BOT, AG)
team(v, policy(sessions=sess("member", "member", "member")))
v.step(ms("b", O, BOT, "member"), "allow")
v.step(E("s1", "session.open", BOT, {"steerMode": "guarded"}), "denied", note="OP isn't in the space")
v.step(E("s2", "session.open", BOT, {"steerMode": "auto"}), "allow")
v.step(E("t", "task", O, {"to": [M]}), "allow").step(E("s3", "session.open", BOT, {"steerMode": "auto", "task": "t"}), "denied", note="not its task")
add(v.done())

v = V("open-approvals-capped", ["P-08", "P-01"], "an author's new approval supersedes its old one; at most 16 per proposal besides the guard's, who is never shut out")
v.prof(BOT, AG)
team(v, policy(approvals=[{"action": "*", "approvers": ["owner", "member"]}], sessions=sess("member", "member", "member")))
v.step(ms("op", O, OP, "member"), "allow").step(ms("b", O, BOT, "member"), "allow")
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("in", "session.input", M, {"session": "s"}), "allow")
v.step(prop("p", author=BOT), "allow")
v.step(appr("a0", "p", author=M), "allow").step(appr("a0b", "p", author=M), "allow", check={"effects.p.approvals": [{"id": "a0b", "author": M, "keyId": "k1"}]}, note="a new approval supersedes M's own, and the old one is removed")
MEMBERS = [f"@m{i}:a.example" for i in range(1, 16)]
for i, a_ in enumerate(MEMBERS):
    v.step(ms(f"mm{i}", O, a_, "member"), "allow").step(appr(f"a{i + 1}", "p", author=a_, expires="2026-10-10T13:00:00Z"), "allow")
v.step(appr("a16", "p", author=O), "denied", note="16 open: the cap")
v.step(appr("g", "p", author=OP), "allow", note="the guard is exempt")
v.step(appr("a17", "p", author=O), "allow", at=4000, note="15 of them expired: they no longer count")
add(v.done())

v = V("owners-only-tighten-modes", ["P-15"], "removing a session's mode moves it to a more cautious allowed mode, or kills it; never looser")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(E("ps", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member", ["auto"]))}), "allow")
v.access(O, "s", NONE).step(E("in", "session.input", O, {"session": "s"}), "denied", note="guarded can't become auto: the session is dead")
v.step(prop("p", author=BOT), "allow", check={"effects.p.phase": "ready"}, note="no steer reached it")
v.step(E("ps2", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member", ["auto", "guarded"]))}), "allow")
v.step(E("in2", "session.input", O, {"session": "s"}), "allow", check={"held": {BOT: OP}}, note="guarded again: live, and the steer holds")
add(v.done())

v = V("guard-rights-need-a-role", ["P-15", "P-02"], "a guard who lost its role loses its rights on its agent's sessions; the hold waits for it")
agents(v, policy(sessions=sess("owner", "owner", "owner")))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("in", "session.input", O, {"session": "s"}), "allow", check={"held": {BOT: OP}})
v.access(OP, "s", ALL).access(M, "s", NONE)
v.step(ms("ban", O, OP, "none"), "allow")
v.step(E("s2", "session.open", BOT, {"steerMode": "auto"}), "allow")
v.access(OP, "s", NONE).access(OP, "s2", NONE).step(E("q", "session.input", OP, {"session": "s2"}), "denied", note="banned guard can't watch or steer even its agent's live auto session")
v.step(v.rel("r0", M, BOT), "denied", note="members can't release")
v.step(v.rel("r1", O, BOT), "denied", note="nobody stands in for a guard")
v.step(v.rel("r1b", OP, BOT), "denied", profiles={OP: human()}, note="the guard itself holds no role")
v.step(ms("back", O, OP, "member"), "allow").step(v.rel("r2", OP, BOT), "allow", check={"held": {}})
add(v.done())

v = V("held-may-cancel-its-delegations", ["P-15", "P-04"], "a held agent may still cancel tasks it delegated earlier (the safe direction)")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("t", "task", BOT, {"to": [BOT2]}), "allow")
v.step(E("in", "session.input", M, {"session": "s"}), "allow")
v.step(E("tc", "task.cancel", BOT, {"task": "t"}), "allow")
add(v.done())

v = V("expired-approvals-crowd-no-one", ["P-08"], "an already-expired approval is refused; expired ones don't count toward the cap and are dropped when their author approves again")
team(v, policy(approvals=[{"action": "*", "approvers": ["owner", "member"]}]))
v.step(prop("p"), "allow")
v.step(appr("a0", "p", author=M, expires="2000-01-01T00:00:00Z"), "denied", note="already expired")
v.step(appr("a1", "p", author=M, expires="2026-10-10T12:00:20Z"), "allow")
v.step(appr("a2", "p", author=M), "allow", at=30, check={"effects.p.approvals": [{"id": "a2", "author": M, "keyId": "k1"}]}, note="a1 expired: M may approve again, and a1 is removed")
add(v.done())

v = V("nobody-stands-in-for-a-guard", ["P-15"], "while a guard is gone, its held agent's proposals can be cancelled but not approved, and the hold waits for the guard")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("in", "session.input", O, {"session": "s"}), "allow")
v.step(prop("p", author=BOT), "allow", check={"effects.p.guards": [OP]})
v.step(ms("gone", O, OP, "none"), "allow")
v.access(M, "s", NONE).step(E("in2", "session.input", M, {"session": "s"}), "denied", note="guarded without its guard: dead")
v.step(appr("a1", "p", author=O), "denied", note="the steering owner can't approve its own steer by evicting the guard")
v.step(v.rel("r", O, BOT), "denied")
v.step(E("x", "effect.cancel", O, {"proposal": "p"}), "allow", check={"effects.p.phase": "cancelled"}, note="owners may always cancel")
add(v.done())

v = V("held-executors-need-their-guard", ["P-15", "P-07 E3"], "while an agent is held, an effect naming it as executor needs its guard's approval, whoever proposed it")
v.prof(BOT, AG).prof(BOT2, AG)
team(v, policy(sessions=sess("member", "member", "member")))
v.step(ms("op", O, OP, "member"), "allow").step(ms("b", O, BOT, "member"), "allow")
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(prop("d", author=M, executor=BOT), "allow", check={"effects.d.guards": [OP], "effects.d.phase": "awaiting-approval"}, note="naming a guarded executor is an instruction, recorded on the effect")
v.step(E("in", "session.input", M, {"session": "s"}), "allow", check={"held": {BOT: OP}})
v.step(claim("c0", "d", author=BOT), "denied", note="its executor is held: the guard must approve")
v.step(appr("a", "d", author=OP), "allow")
v.actors(claim("c1", "d", author=BOT), [BOT, M, OP])
v.step(claim("c1", "d", author=BOT), "allow")
add(v.done())

v = V("tasks-hold-guarded-agents", ["P-15", "R-24"], "a task naming a guarded agent is an instruction: from anyone but its guard it holds the agent")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(E("t0", "task", OP, {"to": [BOT]}), "allow", check={"held": {}}, note="its own guard's task")
v.step(E("t1", "task", M, {"to": [BOT]}), "allow", check={"held": {BOT: OP}})
v.step(prop("p", author=BOT, task="t1"), "allow", check={"effects.p.guards": [OP]})
add(v.done())

v = V("side-door-sessions-are-covered", ["P-15"], "an auto session opened before the agent's operator confirmed still counts once the agent is guarded")
v.prof(BOT, dict(AG, operatorConfirmed=False))
team(v, policy(sessions=sess("member", "member", "member")))
v.step(ms("op", O, OP, "member"), "allow").step(ms("b", O, BOT, "member"), "allow")
v.step(E("s1", "session.open", BOT, {"steerMode": "auto"}), "allow")
v.step(E("s2", "session.open", BOT, {"steerMode": "guarded"}), "allow", profiles={BOT: AG}, note="operator confirmed now")
v.step(E("in", "session.input", M, {"session": "s1"}), "allow", check={"held": {BOT: OP}}, note="held via its guarded session's guard")
add(v.done())

v = V("guarded-is-sticky", ["P-15"], "owners can't un-guard an agent: closing its session, evicting its guard or toggling modes leaves it guarded")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s1", "session.open", BOT, {"steerMode": "guarded"}), "allow", check={"guarded": {BOT: OP}})
v.step(E("s3", "session.open", BOT, {"steerMode": "auto"}), "allow")
v.step(E("c", "session.close", O, {"session": "s1"}), "allow", check={"guarded": {BOT: OP}}, note="an owner closing it doesn't un-guard")
v.step(E("t", "task", O, {"to": [BOT]}), "allow", check={"held": {BOT: OP}}, note="A1: close-then-task still holds")
v.step(v.rel("r", OP, BOT), "allow", check={"held": {}})
v.step(ms("ev", O, OP, "none"), "allow", check={"guarded": {BOT: OP}})
v.step(E("in", "session.input", O, {"session": "s3"}), "allow", check={"held": {BOT: OP}}, note="A2: evict-then-steer still holds")
v.step(prop("p", author=BOT), "allow", check={"effects.p.guards": [OP]})
v.step(appr("a", "p", author=O), "denied", note="nobody stands in")
add(v.done())

v = V("modes-toggle-cannot-unguard", ["P-15"], "switching modes to auto-only and back doesn't let an outside steer through")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s1", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("s3", "session.open", BOT, {"steerMode": "auto"}), "allow")
v.step(E("ps", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member", ["auto"]))}), "allow")
v.step(E("in", "session.input", P, {"session": "s3"}), "allow", check={"held": {BOT: OP}}, note="A3: still guarded")
add(v.done())

v = V("agents-cannot-unguard-themselves", ["P-15"], "closing sessions never unguards an agent, held or not; it can't unguard itself with a release either")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s1", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("s3", "session.open", BOT, {"steerMode": "auto"}), "allow")
v.step(E("c1", "session.close", BOT, {"session": "s1"}), "allow", check={"guarded": {BOT: OP}}, note="E1: not held, closing its last guarded session still doesn't unguard")
v.step(E("in", "session.input", P, {"session": "s3"}), "allow", check={"held": {BOT: OP}})
v.step(E("c3", "session.close", BOT, {"session": "s3"}), "allow", check={"guarded": {BOT: OP}}, note="held: nor does it")
v.step(E("s2", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("c2", "session.close", BOT, {"session": "s2"}), "allow", check={"guarded": {BOT: OP}})
v.step(v.rel("u0", BOT, BOT, unguard=True), "denied", note="an agent can't release or unguard itself")
v.step(v.rel("u1", O, BOT, unguard=True), "denied", note="nor can an owner")
v.step(E("c4", "session.close", O, {"session": "s1"}), "invalid", note="a closed session is no longer in the state: no such session")
v.step(E("c5", "session.close", BOT, {"session": "s1"}), "invalid", note="even for its actor")
add(v.done())

v = V("guard-unguards", ["P-15"], "only the guard ends an agent's guarded choice: agent.release with unguard; a plain release leaves it guarded; session.input carries no release flags")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s1", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("s3", "session.open", BOT, {"steerMode": "auto"}), "allow")
v.step(E("c1", "session.close", BOT, {"session": "s1"}), "allow", note="a live guarded session would still guard it")
v.step(E("in", "session.input", P, {"session": "s3"}), "allow", check={"held": {BOT: OP}})
v.step(E("bad", "session.input", OP, {"session": "s3", "unguard": True}), "invalid", note="F2: session.input is {session} only")
v.step(E("bad2", "session.input", OP, {"session": "s3", "release": True}), "invalid", note="F2: no release flag on a steer")
v.step(v.rel("r", OP, BOT), "allow", check={"held": {}, "guarded": {BOT: OP}}, note="a plain release leaves it guarded")
v.step(E("in2", "session.input", P, {"session": "s3"}), "allow", check={"held": {BOT: OP}})
v.step(v.rel("u", OP, BOT, unguard=True), "allow", check={"held": {}, "guarded": {}}, note="E1: names the agent, no session needed")
v.step(E("in3", "session.input", P, {"session": "s3"}), "allow", check={"held": {}}, note="no longer guarded: an outside steer doesn't hold")
add(v.done())

v = V("guard-unguards-without-a-hold", ["P-15"], "a guard may unguard an agent that isn't held and has no session; nobody else may")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s1", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("c", "session.close", BOT, {"session": "s1"}), "allow")
v.step(v.rel("u0", M, BOT, unguard=True), "denied", note="not the guard")
v.step(v.rel("u1", OP2, BOT, unguard=True), "denied", note="another operator")
v.step(v.rel("u2", OP, BOT, unguard=True), "allow", check={"guarded": {}, "held": {}})
add(v.done())

v = V("stale-unguards-are-refused", ["P-15"], "an unguard is a release: signed before a newer outside instruction arrived, it is refused and the agent stays guarded and held")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("c", "session.close", BOT, {"session": "s"}), "allow")
v.step(E("t0", "task", P, {"to": [BOT]}), "allow", check={"held": {BOT: OP}})
seen_at = v.heads[S]
v.step(E("t1", "task", P, {"to": [BOT]}), "allow")
v.step(v.rel("u0", OP, BOT, seen=seen_at, unguard=True), "denied", note="signed before t1 arrived")
v.step(v.rel("u1", OP, BOT, unguard=True), "allow", check={"held": {}, "guarded": {}})
add(v.done())

v = V("unguard-does-not-loosen-forcing", ["P-15"], "an unguard clears only the agent's own choice: owner forcing (forceGuarded) still guards it")
agents(v, policy(sessions=sess("member", "member", "member", MODES, True)))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow", check={"guarded": {BOT: OP}}).step(E("c", "session.close", BOT, {"session": "s"}), "allow")
v.step(E("t", "task", P, {"to": [BOT]}), "allow", check={"held": {BOT: OP}})
v.step(v.rel("u", OP, BOT, unguard=True), "allow", check={"held": {}, "guarded": {}})
v.step(E("t2", "task", P, {"to": [BOT]}), "allow", check={"held": {BOT: OP}}, note="still forced")
add(v.done())

# ── round 22 (E2), reworked in round 23 (F1): owner forcing decided per instruction, from the profile pinned for that entry ──
v = V("force-guarded-holds-session-less-agents", ["P-15"], "with forceGuarded, an agent with a confirmed operator is guarded without any session: a task or an executor slot holds it")
agents(v, policy(sessions=sess("member", "member", "member", MODES, True)))
v.step(E("t0", "task", OP, {"to": [BOT]}), "allow", check={"held": {}}, note="its own guard's task")
v.step(E("t", "task", P, {"to": [BOT]}), "allow", check={"held": {BOT: OP}}, note="no session needed")
v.step(prop("d", author=M, executor=BOT2), "allow", check={"held": {BOT: OP, BOT2: OP}, "effects.d.guards": [OP], "effects.d.phase": "awaiting-approval"}, note="so does naming it executor")
v.step(claim("c0", "d", author=BOT2), "denied", note="its guard must approve")
v.step(E("so", "session.open", BOT, {"steerMode": "auto"}), "allow", note="a held agent may still open a session")
v.step(v.rel("r", OP, BOT), "allow", check={"held": {BOT2: OP}}, note="F2: the release names the agent; no session is needed")
v.step(E("msg", "message", M, {}), "allow", check={"held": {BOT2: OP}})
v.step(v.rel("r2", OP, BOT2), "allow", check={"held": {}})
v.step(claim("c1", "d", author=BOT2), "denied", note="the effect recorded its guard: it still needs OP after the release")
v.step(appr("a", "d", author=OP), "allow").step(claim("c2", "d", author=BOT2), "allow")
add(v.done())

v = V("force-guarded-is-off-by-default", ["P-15"], "without forceGuarded (the default) a session-less agent is not guarded; switching it on guards instructions from then on, not the history")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("t", "task", P, {"to": [BOT]}), "allow", check={"held": {}})
v.step(E("pf", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member", MODES, "yes"))}), "invalid", note="forceGuarded is a boolean")
v.step(E("ps", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member", MODES, True))}), "allow", check={"held": {}}, note="F1: no fold-time snapshot: the earlier task is history")
v.step(E("t2", "task", P, {"to": [BOT]}), "allow", check={"held": {BOT: OP}}, note="the next instruction is judged by the pinned profile")
add(v.done())

v = V("force-guarded-spares-humans", ["P-15"], "forcing guards non-human targets of an instruction only: humans are never held, and a service named as executor is exempt")
agents(v, policy(sessions=sess("member", "member", "member", MODES, True)))
v.prof(X, dict(XS, operator=OP))
v.step(E("t", "task", P, {"to": [M, O]}), "allow", check={"held": {}}, note="humans")
v.step(prop("d", author=P, executor=X), "allow", check={"held": {}, "effects.d.phase": "ready"}, note="a service as executor: services execute, they take no instructions")
v.step(E("t2", "task", P, {"to": [BOT2]}), "allow", check={"held": {BOT2: OP}})
add(v.done())

SVC = "@svc:b.example"
v = V("service-tasked-or-steered-is-forced", ["P-15"], "CX2: a service that is tasked, or holds a session that is steered, is forced like an agent; only the executor slot is exempt")
agents(v, policy(sessions=sess("member", "member", "member", MODES, True)))
v.prof(SVC, dict(XS, operator=OP))
v.step(ms("svc", O, SVC, "member"), "allow")
v.step(E("s", "session.open", SVC, {"steerMode": "auto"}), "allow")
v.step(E("in", "session.input", M, {"session": "s"}), "allow", check={"held": {SVC: OP}}, note="steering an LLM labelled 'service' holds it")
v.step(v.rel("r", OP, SVC), "allow", check={"held": {}})
v.step(E("t", "task", P, {"to": [SVC]}), "allow", check={"held": {SVC: OP}}, note="so does a task")
v.step(v.rel("r2", OP, SVC), "allow", check={"held": {}})
v.step(prop("d", author=P, executor=SVC), "allow", check={"held": {}, "effects.d.phase": "ready"}, note="named as executor: exempt")
add(v.done())

NOOP = dict(AG, operatorConfirmed=False)
BOT3 = "@bot3:b.example"
v = V("forced-instructions-need-the-operator", ["P-15", "P-16"], "in a forced space an instruction to a non-human target with no confirmed operator holding a role is denied, in every slot but a service's executor slot")
v.prof(BOT, AG).prof(BOT2, NOOP).prof(X, dict(XS, operatorConfirmed=False)).prof(BOT3, dict(AG, operator="@gone:b.example"))
team(v, policy(sessions=sess("member", "member", "member", MODES, True)))
v.step(ms("op", O, OP, "member"), "allow").step(ms("b", O, BOT, "member"), "allow").step(ms("b2", O, BOT2, "member"), "allow").step(ms("b3", O, BOT3, "member"), "allow")
v.step(E("t1", "task", M, {"to": [BOT2]}), "denied", note="operator not confirmed")
v.step(E("t2", "task", M, {"to": [BOT, BOT2]}), "denied", note="one such assignee is enough")
v.step(E("t3", "task", M, {"to": [BOT3]}), "denied", note="its confirmed operator holds no role here")
v.step(E("t4", "task", M, {"to": [BOT]}), "allow", check={"held": {BOT: OP}})
v.step(prop("d1", author=M, executor=BOT2), "denied", note="as executor too")
v.step(prop("d2", author=M, executor=X), "allow", note="a service as executor is exempt even with no operator")
v.step(E("s", "session.open", BOT2, {"steerMode": "auto"}), "allow")
v.step(E("in", "session.input", M, {"session": "s"}), "denied", note="a steer into it too")
v.step(E("own", "session.input", BOT2, {"session": "s"}), "allow", note="its own input is no instruction")
v.step(E("self", "task", BOT2, {"to": [BOT2]}), "allow", note="nor a task to itself")
v.step(E("ps", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member"))}), "allow")
v.step(E("t5", "task", M, {"to": [BOT2]}), "allow", check={"held": {BOT: OP}}, note="not forced any more: allowed, and not held")
add(v.done())

v = V("force-guarded-late-confirmation", ["P-15", "P-01"], "CX1: an agent its operator confirms only after it joined is forced from then on; the profile pinned for each instruction decides")
v.prof(BOT, NOOP)
team(v, policy(sessions=sess("member", "member", "member", MODES, True)))
v.step(ms("op", O, OP, "member"), "allow").step(ms("b", O, BOT, "member"), "allow")
v.step(E("t0", "task", M, {"to": [BOT]}), "denied", note="not yet confirmed: a forced space can't instruct it")
v.step(E("t1", "task", M, {"to": [BOT]}), "allow", profiles={BOT: AG}, check={"held": {BOT: OP}}, note="confirmed now: held for OP")
v.step(prop("p", author=BOT, executor=X), "allow", check={"effects.p.phase": "awaiting-approval", "effects.p.guards": [OP]}, note="what it proposes waits for OP")
v.step(claim("c0", "p"), "denied", note="CX1 escaped here in round 22")
v.step(appr("a", "p", author=OP), "allow").step(claim("c1", "p"), "allow")
add(v.done())

v = V("force-guarded-role-change-during-withdrawal", ["P-15", "P-03"], "CX3: owners moving an agent to another role while its operator withdrew doesn't lose the forcing: nothing is snapshotted")
v.prof(BOT, AG).prof(X, XS)
team(v, policy(allowAgents="with-operator", sessions=sess("member", "member", "member", MODES, True)))
v.step(ms("op", O, OP, "member"), "allow").step(ms("b", O, BOT, "member"), "allow")
v.step(ms("b2", O, BOT, "guest"), "allow", profiles={BOT: NOOP}, note="operator withdrew")
v.step(E("w", "message", M, {}), "allow", profiles={BOT: AG}, note="operator confirms again")
v.step(E("t", "task", M, {"to": [BOT]}), "allow", check={"held": {BOT: OP}})
v.step(ms("b3", O, BOT, "member"), "allow", check={"held": {BOT: OP}}, note="back to member: the hold stands")
v.step(prop("p", author=BOT, executor=X), "allow", check={"effects.p.phase": "awaiting-approval", "effects.p.guards": [OP]})
v.step(claim("c0", "p"), "denied", note="CX3 escaped here in round 22")
add(v.done())

AG2 = dict(AG, operator=OP2)
v = V("forcing-uses-each-entrys-profile", ["P-15", "P-01"], "the guard of a forced instruction is the operator in the profile pinned for that entry")
v.prof(BOT, AG)
team(v, policy(sessions=sess("member", "member", "member", MODES, True)))
v.step(ms("op", O, OP, "member"), "allow").step(ms("op2", O, OP2, "member"), "allow").step(ms("b", O, BOT, "member"), "allow")
v.step(E("t", "task", M, {"to": [BOT]}), "allow", profiles={BOT: AG2}, check={"held": {BOT: OP2}}, note="pinned operator OP2")
v.step(v.rel("r", OP, BOT), "denied", note="OP is not the guard it was held for")
v.step(v.rel("r2", OP2, BOT), "allow", check={"held": {}})
v.step(E("t2", "task", M, {"to": [BOT]}), "allow", profiles={BOT: AG}, check={"held": {BOT: OP}}, note="the next instruction pins OP")
add(v.done())

ABSENT_SESSION = hashlib.sha256(b"absent session").hexdigest()
v = V("instruction-targets-are-pinned", ["P-01"], "relevantActors lists the targets of an instruction: the session's actor, each assignee, the executor")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s", "session.open", BOT, {"steerMode": "auto"}), "allow")
v.actors(E("in", "session.input", M, {"session": "s"}), [M, BOT])
v.actors(E("in2", "session.input", M, {"session": ABSENT_SESSION}), [M])
v.actors(E("t", "task", M, {"to": [BOT2, M, BOT]}), [M, BOT2, BOT])
v.actors(prop("d", author=M, executor=BOT), [M, BOT])
v.actors(E("r", "agent.release", OP, {"agent": BOT}), [OP])
add(v.done())

v = V("naming-a-guarded-executor-holds-it", ["P-15"], "a proposal naming a guarded agent as executor is an instruction: from anyone but its guard it holds the agent")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s1", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(prop("q", author=OP, executor=BOT), "allow", check={"held": {}}, note="its own guard's proposal")
v.step(prop("d", author=P, executor=BOT), "allow", check={"held": {BOT: OP}}, note="A4")
v.step(claim("c0", "d", author=BOT), "denied", note="its guard must approve")
add(v.done())

v = V("returned-approvals-stay-superseded", ["P-08"], "an approval returned by a not-started outcome is removed if its author approved again meanwhile")
team(v, policy(approvals=[{"action": "*", "approvers": ["owner"]}]))
v.step(prop("p"), "allow").step(appr("a1", "p", author=O), "allow").step(claim("c", "p"), "allow")
v.step(appr("a2", "p", author=O), "allow")
v.step(outc("o", "c", "failed", "no"), "allow", check={"effects.p.approvals": [{"id": "a2", "author": O, "keyId": "k1"}]}, note="A5: one open approval per author; the returned one is gone")
add(v.done())

v = V("owner-forced-guarding-covers-every-instruction", ["P-15"], "when owners' modes force an agent's session guarded, tasks and executor slots hold it too, not only steers")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s", "session.open", BOT, {"steerMode": "auto"}), "allow")
v.step(E("ps", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member", ["guarded"]))}), "allow")
v.step(E("t", "task", P, {"to": [BOT]}), "allow", check={"held": {BOT: OP}}, note="CE1: a task holds it")
v.step(v.rel("r", OP, BOT), "allow")
v.step(prop("d", author=P, executor=BOT), "allow", check={"held": {BOT: OP}}, note="so does naming it as executor")
add(v.done())

v = V("held-proposers-need-their-guard-for-older-proposals", ["P-15", "P-07 E3"], "a proposal made before the hold still needs the guard while its proposer is held")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(prop("p", author=BOT), "allow", check={"effects.p.phase": "ready"})
v.step(E("in", "session.input", P, {"session": "s"}), "allow")
v.step(claim("c0", "p"), "denied", note="CE2: its proposer is held now")
v.step(appr("a", "p", author=OP), "allow").step(claim("c1", "p"), "allow")
add(v.done())

v = V("guarding-holds-for-open-outside-instructions", ["P-15"], "becoming guarded with an outside task or executor slot still open holds the agent at once")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("t", "task", P, {"to": [BOT2, BOT]}), "allow")
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow", check={"held": {BOT: OP}}, note="CE6")
v.step(E("t2", "task", M, {"to": [BOT2, BOT]}), "allow")
v.step(E("s2", "session.open", BOT2, {"steerMode": "guarded"}), "allow", check={"held": {BOT: OP, BOT2: OP}}, note="every assignee")
add(v.done())

v = V("guarding-holds-for-open-effects", ["P-15"], "becoming guarded with an unfinished outside effect naming the agent as executor holds it at once and records the guard on the effect")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(prop("d", author=P, executor=BOT), "allow", check={"held": {}, "effects.d.phase": "ready"})
v.step(prop("d2", author=OP, executor=BOT), "allow", note="its own guard's proposal is no outside instruction")
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow", check={"held": {BOT: OP}, "effects.d.guards": [OP], "effects.d.phase": "awaiting-approval"})
v.step(claim("c0", "d", author=BOT), "denied", note="its guard must approve")
v.step(appr("a", "d", author=OP), "allow").step(claim("c1", "d", author=BOT), "allow")
add(v.done())

v = V("tasks-hold-every-guarded-assignee", ["P-15"], "a task naming several guarded agents holds each of them")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("s2", "session.open", BOT2, {"steerMode": "guarded"}), "allow")
v.step(E("t", "task", P, {"to": [BOT2, BOT]}), "allow", check={"held": {BOT: OP, BOT2: OP}})
add(v.done())

v = V("forced-guarding-holds-for-open-instructions", ["P-15"], "when owners force guarding, outside instructions still open hold the agent at once; release clears them")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s", "session.open", BOT, {"steerMode": "auto"}), "allow")
v.step(E("t", "task", P, {"to": [BOT]}), "allow", check={"held": {}})
v.step(E("ps", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member", ["guarded"]))}), "allow", check={"held": {BOT: OP}}, note="CX1")
v.step(E("t2", "task", BOT, {"to": [BOT2]}), "denied", note="held: can't delegate P's task")
v.step(v.rel("r", OP, BOT), "allow", check={"held": {}}, note="the guard reviewed it")
v.step(E("x", "message", M, {}), "allow", check={"held": {}}, note="cleared instructions don't re-hold")
add(v.done())

v = V("executor-guards-survive-release", ["P-15", "P-07 E3"], "an effect an outsider aimed at a guarded executor keeps needing its guard after a release")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(prop("d", author=P, executor=BOT), "allow", check={"held": {BOT: OP}, "effects.d.guards": [OP]})
v.step(v.rel("r", OP, BOT), "allow", check={"held": {}})
v.step(claim("c0", "d", author=BOT), "denied", note="CX3: still needs OP")
v.step(appr("a", "d", author=OP), "allow").step(claim("c1", "d", author=BOT), "allow")
add(v.done())

v = V("profile-is-reserved", ["§2", "R-09"], "a plain space named profile:<server> is invalid, not a crash")
v.step(E("C", "space.create", O, {"policy": policy()}, space="profile:a.example"), "invalid")
add(v.done())

v = V("abandon-never-rearms", ["P-09"], "an owner's abandon of a repeatable effect leaves it unknown, even when the owner is also the executor")
team(v)
v.step(prop("p", author=M, repeatable=True, executor=O), "allow").step(claim("c", "p", author=O, instance="k1"), "allow")
v.step(outc("ab", "c", "unknown", "unknown", author=O, key="k2"), "allow", at=200, profiles={O: human({"k1": {"publicKey": "pk", "status": "valid"}, "k2": {"publicKey": "pk2", "status": "valid"}})}, check={"effects.p.phase": "unknown"}, note="CX4: a different key is an abandon")
add(v.done())

v = V("closed-tasks-stop-their-effects", ["P-07 E5", "P-05"], "an effect charged to a closed task can't be claimed, budgeted or not")
team(v)
v.step(E("t", "task", O, {"to": [P]}), "allow").step(prop("p", author=P, task="t"), "allow")
v.step(E("tc", "task.update", O, {"task": "t", "state": "cancelled"}), "allow")
v.step(claim("c", "p"), "denied", note="CX5")
add(v.done())

v = V("held-executors-need-their-guard-even-for-its-proposals", ["P-15", "P-07 E3"], "while an agent is held, even an effect its own guard proposed for it needs that guard's approval to run")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(prop("d", author=OP, executor=BOT), "allow", check={"effects.d.phase": "ready", "held": {}})
v.step(E("own", "session.input", OP, {"session": "s"}), "allow", check={"held": {}}, note="its guard's own steer never holds it")
v.step(E("in", "session.input", P, {"session": "s"}), "allow", check={"held": {BOT: OP}})
v.step(claim("c0", "d", author=BOT), "denied", note="its executor is held")
v.step(appr("a", "d", author=M), "denied", note="not a guard, no rule")
add(v.done())

v = V("stale-releases-are-refused", ["P-15"], "a release covers only what its guard had seen: newer outside instructions keep the agent held")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("in", "session.input", P, {"session": "s"}), "allow")
seen_at = v.heads[S]
v.step(E("t1", "task", P, {"to": [BOT]}), "allow")
v.step(v.rel("r0", OP, BOT, seen=seen_at), "denied", note="D1: signed before t1 arrived")
v.step(v.rel("r1", OP, BOT), "allow", check={"held": {}})
add(v.done())

v = V("agents-never-draw-on-the-space-budget", ["P-05", "P-07 E6"], "an agent's effect without a budgeted task can't be claimed against the space budget, even if the budget appeared after it was proposed")
v.prof(BOT, AG)
team(v)
v.step(ms("op", O, OP, "member"), "allow").step(ms("b", O, BOT, "member"), "allow")
v.step(prop("p", author=BOT, cost=USD(400)), "allow")
v.step(E("ps", "policy.set", O, {"policy": policy(budget=USD(1000))}), "allow")
v.step(claim("c", "p"), "denied", note="D2")
add(v.done())

v = V("grammar-is-strict-ascii", ["§2", "R-09"], "trailing newlines and non-ASCII digits are invalid, as the §2 grammar says")
team(v)
v.step(E("m", "message", M + "\n", {}), "invalid", note="author with a trailing newline")
bad = prop("p"); bad["body"]["action"] = "git.push\n"
v.step(bad, "invalid", note="action with a trailing newline")
v.step(E("ps", "policy.set", O, {"policy": policy(approvals=[{"action": "git.push\n", "approvers": ["owner"]}])}), "invalid", note="an approval rule that would match nothing")
dl = E("t", "task", M, {"to": [P], "deadline": "\u0662\u0660\u0662\u0666-10-10T12:00:00Z"})
v.step(dl, "invalid", note="Arabic-Indic digits")
add(v.done())

v = V("releases-clear-only-what-was-seen", ["P-15"], "work that arrived before the agent became guarded, and after its guard last looked, stays pending after a release")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s", "session.open", BOT, {"steerMode": "auto"}), "allow")
seen_at = v.heads[S]
v.step(E("t", "task", P, {"to": [BOT]}), "allow", check={"held": {}}, note="not guarded yet")
v.step(E("ps", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member", ["guarded"]))}), "allow", check={"held": {BOT: OP}})
v.step(v.rel("r0", OP, BOT, seen=seen_at), "allow", check={"held": {BOT: OP}}, note="OP never saw t: still held")
v.step(v.rel("r1", OP, BOT), "allow", check={"held": {}})
add(v.done())

# ── round 18: spec precision (clean-room gaps, implementation notes). Each vector pins a rule architecture.md now states ──
REVOKED = lambda: human({"k1": {"publicKey": "pk", "status": "revoked"}})
ABSENT_ID = hashlib.sha256(b"absent").hexdigest()
O2, U, BOT4 = "@o2:e.example", "@u:f.example", "@bot4:b.example"

v = V("role-history-bounds", ["P-02"], "a role held at seq k was granted at or before k and ended after k (from <= k < to); a current role reads every seq")
team(v)                                                    # C=1, M=2, P=3, X=4
v.step(ms("rm", O, M, "banned"), "allow").step(ms("rp", O, P, "none"), "allow")      # 5, 6
v.read(M, 1, False).read(M, 2, True).read(M, 4, True).read(M, 5, False, ).read(M, 6, False)
v.read("@:b.example", 2, False).read("@:b.example", 3, True).read("@:b.example", 5, True).read("@:b.example", 6, False)
v.step(ms("back", O, M, "member"), "allow")
v.read(M, 1, True).read(M, 5, True)
add(v.done())

v = V("approver-roles-are-exact", ["P-08"], "a role in an approvers list matches holders of that role only: an owner doesn't satisfy [member]")
team(v, policy(approvals=[{"action": "*", "approvers": ["member"]}]))
v.step(prop("pr", author=P), "allow", check={"effects.pr.phase": "awaiting-approval"})
v.step(appr("a0", "pr", author=O), "denied", note="owner is not member")
v.step(appr("a1", "pr", author=M), "allow", check={"effects.pr.phase": "ready"})
v.step(claim("c", "pr"), "allow")
add(v.done())

v = V("owner-servers-rule", ["P-11"], "policy.set may deny some owners' servers, not all of them")
v.step(create(), "allow").step(ms("o2", O, O2, "owner"), "allow")
v.step(E("mv", "space.move", O, {"to": "host.example", "accept": ACCEPT}), "allow", check={"home": "host.example"}, note="the home is no longer a.example")
v.step(E("p1", "policy.set", O, {"policy": policy(servers={"deny": ["e.example"]})}), "allow", note="one owner's server")
v.step(E("p2", "policy.set", O, {"policy": policy(servers={"deny": ["a.example", "e.example"]})}), "denied", note="the server of every owner")
add(v.done())

v = V("agent-check-only-on-grant", ["P-03"], "the agent checks apply when a readable role is granted to an actor with none; not to role changes, bans or removals")
v.prof(BOT, AG)
team(v, policy(allowAgents="any"))
v.step(ms("b", O, BOT, "member"), "allow")
v.step(E("ps", "policy.set", O, {"policy": policy(allowAgents="none")}), "allow")
v.step(ms("g", O, BOT, "guest"), "allow", note="already holds a readable role: not a grant")
v.step(ms("x", O, BOT, "banned"), "allow", note="banning is never a grant")
v.step(ms("r", O, BOT, "member"), "denied", note="banned to member is a grant")
v.step(ms("n", O, BOT, "none"), "allow")
v.step(ms("r2", O, BOT, "guest"), "denied")
add(v.done())

v = V("redact-needs-no-role", ["P-12"], "the target's author may redact it without holding a role now; a stranger may not")
team(v)
v.step(E("msg", "message", M, {}), "allow").step(ms("ban", O, M, "banned"), "allow")
v.step(E("r0", "redact", M, {"target": "msg"}), "allow", note="banned, but the author")
v.step(E("r1", "redact", Z, {"target": "m1"}), "denied")
add(v.done())

v = V("owner-assignee-still-waits", ["P-05"], "an owner who is also an assignee closes like an assignee; an owner who is not one force-closes")
team(v)
v.step(E("t1", "task", O, {"to": [O]}), "allow").step(prop("p1", author=O, task="t1"), "allow").step(claim("c1", "p1"), "allow")
v.step(E("u1", "task.update", O, {"task": "t1", "state": "done"}), "denied", note="an assignee can't close over a claimed effect")
v.step(E("t2", "task", O, {"to": [P]}), "allow").step(prop("p2", author=P, task="t2"), "allow").step(claim("c2", "p2"), "allow")
v.step(E("u2", "task.update", O, {"task": "t2", "state": "cancelled"}), "allow", check={"tasks.t2.open": False}, note="a non-assignee owner force-closes")
add(v.done())

v = V("references-must-be-the-right-kind", ["P-16", "R-05"], "a reference to an entry of another kind, or to nothing, is invalid, not denied; so is an empty assignee list")
team(v)
v.step(E("msg", "message", M, {}), "allow").step(prop("pr", author=P), "allow")
v.step(E("k1", "task.update", M, {"task": "msg", "state": "progress"}), "invalid")
v.step(E("k2", "task.cancel", M, {"task": "msg"}), "invalid")
v.step(E("k3", "task", M, {"to": [P], "parent": "msg"}), "invalid")
v.step(E("k4", "effect.propose", P, {"executor": X, "action": "git.push", "planDigest": digest_of("k4"), "repeatable": False, "task": "msg"}), "invalid")
v.step(appr("k5", "msg", author=O), "invalid")
v.step(claim("k6", "msg"), "invalid")
v.step(E("k7", "effect.cancel", O, {"proposal": "msg"}), "invalid")
v.step(outc("k8", "pr", "failed", "no"), "invalid", note="a proposal is not a claim")
v.step(E("k9", "effect.resolve", O, {"proposal": "msg", "result": "failed"}), "invalid")
v.step(E("k10", "redact", O, {"target": ABSENT_ID}), "invalid")
v.step(E("k11", "task", M, {"to": []}), "invalid", note="empty to")
add(v.done())

v = V("profile-space-membership", ["P-03", "§4.3"], "in a profile space only the operator's confirmation and its withdrawal (by the operator or the server) are allowed member.sets")
v.prof("@:b.example", human()).prof(BOT4, dict(AG))
PS4 = "profile:@bot4:b.example"
KEY1 = {"keyId": "k1", "publicKey": "pk"}
v.step(E("n0", "space.create", O, {"policy": policy(), "profile": {"actorKind": "human", "key": KEY1}}), "invalid", note="profile only in profile spaces")
v.step(E("n1", "space.create", "@:b.example", {"policy": policy(visibility="public")}, space=PS4), "invalid", note="a profile space needs profile")
v.step(E("PC", "space.create", "@:b.example", {"policy": policy(visibility="public"), "profile": {"actorKind": "agent", "operator": OP, "key": KEY1}}, space=PS4),
       "allow", check={"roles": {BOT4: "owner", "@:b.example": "owner"}})
v.step(E("s1", "member.set", "@:b.example", {"actor": OP, "role": "owner"}, space=PS4), "denied", note="the server can't confirm")
v.step(E("s2", "member.set", BOT4, {"actor": OP, "role": "owner"}, space=PS4), "denied", note="nor the actor")
v.step(E("s3", "member.set", OP, {"actor": Z, "role": "owner"}, space=PS4), "denied", note="only for itself")
v.step(E("s4", "member.set", OP, {"actor": OP, "role": "member"}, space=PS4), "denied", note="only owner or none")
v.step(E("c1", "member.set", OP, {"actor": OP, "role": "owner"}, space=PS4), "allow", check={"profile.operatorConfirmed": True, "roles": {BOT4: "owner", "@:b.example": "owner"}})
v.step(E("w1", "member.set", BOT4, {"actor": OP, "role": "none"}, space=PS4), "denied", note="the actor can't withdraw its operator")
v.step(E("w2", "member.set", Z, {"actor": OP, "role": "none"}, space=PS4), "denied")
v.step(E("w3", "member.set", "@:b.example", {"actor": OP, "role": "none"}, space=PS4), "allow", check={"profile.operatorConfirmed": False}, note="the server may withdraw")
v.step(E("c2", "member.set", OP, {"actor": OP, "role": "owner"}, space=PS4), "allow", check={"profile.operatorConfirmed": True})
v.step(E("w4", "member.set", OP, {"actor": OP, "role": "none"}, space=PS4), "allow", check={"profile.operatorConfirmed": False})
add(v.done())

v = V("relevant-actors-order", ["P-01"], "author, target, proposer, then every approval candidate in order, each actor once")
team(v, policy(approvals=[{"action": "*", "approvers": ["owner", "member"]}]))
v.step(prop("pr"), "allow").step(appr("a1", "pr", author=P), "allow").step(appr("a2", "pr"), "allow")
v.step(appr("a3", "pr", author=M), "allow").step(appr("a4", "pr"), "allow", note="supersedes a2")
v.actors(claim("c", "pr"), [X, P, M, O])
v.actors(claim("c9", ABSENT_ID), [X])
v.actors(ms("self", O, O, "member"), [O])
add(v.done())

v = V("approval-time-boundaries", ["P-08", "P-07 E3"], "an approval whose expiresAt equals ctx.time is expired")
team(v, policy(approvals=[{"action": "*", "approvers": ["owner"]}]))
v.step(prop("pr"), "allow", at=10)
v.step(appr("a0", "pr", expires="2026-10-10T12:00:20Z"), "denied", at=20, note="expires at this very second")
v.step(appr("a1", "pr", expires="2026-10-10T12:00:25Z"), "allow", at=20)
v.step(claim("c0", "pr"), "denied", at=25, note="a1 expires at 25: not a candidate")
v.step(appr("a2", "pr"), "allow", at=26).step(claim("c1", "pr"), "allow", at=27)
add(v.done())

v = V("approve-phases-and-resolve", ["P-08", "P-10"], "approvals are accepted while awaiting, ready or claimed; resolve failed removes every approval, re-arms and leaves none in hand")
team(v, policy(approvals=[{"action": "*", "approvers": ["owner"]}]))
v.step(prop("pr"), "allow").step(appr("a1", "pr"), "allow", check={"effects.pr.phase": "ready"})
v.step(claim("c", "pr"), "allow").step(appr("a2", "pr"), "allow", check={"effects.pr.phase": "claimed"}, note="adds an approval while claimed")
v.step(outc("o", "c", "unknown", "yes"), "allow", check={"effects.pr.phase": "unknown"})
v.step(appr("a3", "pr"), "denied", note="unknown")
v.step(E("r", "effect.resolve", O, {"proposal": "pr", "result": "failed"}), "allow",
       check={"effects.pr.phase": "awaiting-approval", "effects.pr.approvals": []}, note="a2 removed, a1 consumed by the now settled claim: nothing in hand")
v.step(appr("a4", "pr"), "allow", check={"effects.pr.phase": "ready"})
v.step(claim("c2", "pr", attempt=2), "allow").step(outc("o2", "c2", "succeeded", "yes"), "allow", check={"effects.pr.phase": "succeeded"})
v.step(appr("a5", "pr"), "denied", note="final")
add(v.done())

v = V("cancel-by-past-approvers", ["P-04"], "anyone whose approval is still on the effect, even if an open claim consumed it, may cancel it while they hold a role")
team(v, policy(approvals=[{"action": "*", "approvers": [M]}]))
v.step(prop("pr"), "allow").step(appr("a1", "pr", author=M), "allow").step(claim("c", "pr"), "allow")
v.step(E("x0", "effect.cancel", Z, {"proposal": "pr"}), "denied", note="holds no role")
v.step(E("x1", "effect.cancel", M, {"proposal": "pr"}), "allow", check={"effects.pr.cancelRequested": True}, note="its approval was consumed by the claim")
add(v.done())

v = V("settled-claims-release-their-approvals", ["P-08", "P-10"], "a consumed approval leaves the state once its claim settles; a returned one stays; an unknown outcome keeps it until resolved")
team(v, policy(approvals=[{"action": "*", "approvers": ["owner", "member"]}]))
A = lambda i, a: {"id": i, "author": a, "keyId": "k1"}
AC = lambda i, a, c: dict(A(i, a), consumedBy=c)
v.step(prop("pr", repeatable=True), "allow").step(appr("a1", "pr", author=O), "allow").step(appr("a2", "pr", author=M), "allow")
v.step(claim("c1", "pr"), "allow", check={"effects.pr.approvals": [AC("a1", O, "c1"), A("a2", M)]})
v.step(outc("o1", "c1", "failed", "yes"), "allow", check={"effects.pr.approvals": [A("a2", M)], "effects.pr.phase": "ready"}, note="a1 consumed by a settled claim: gone")
v.step(claim("c2", "pr", attempt=2), "allow", check={"effects.pr.approvals": [AC("a2", M, "c2")]})
v.step(outc("o2", "c2", "unknown", "yes"), "allow", check={"effects.pr.approvals": [], "effects.pr.phase": "awaiting-approval"}, note="a repeatable unknown is settled and retried")
v.step(E("x", "effect.cancel", M, {"proposal": "pr"}), "denied", note="M's approval is gone: no longer an approver on record")
v.step(E("x2", "effect.cancel", O, {"proposal": "pr"}), "allow", note="an owner always may")
v.step(prop("pu", repeatable=False), "allow").step(appr("a3", "pu", author=O), "allow").step(claim("c3", "pu"), "allow")
v.step(outc("o3", "c3", "unknown", "yes"), "allow", check={"effects.pu.phase": "unknown", "effects.pu.approvals": [AC("a3", O, "c3")]}, note="unsettled: the approval stays")
v.step(E("r", "effect.resolve", O, {"proposal": "pu", "result": "succeeded"}), "allow", check={"effects.pu.approvals": []})
v.step(prop("pn"), "allow").step(appr("a4", "pn", author=O), "allow").step(claim("c4", "pn"), "allow")
v.step(outc("o4", "c4", "failed", "no"), "allow", check={"effects.pn.approvals": [A("a4", O)], "effects.pn.phase": "ready"}, note="not started: the approval is returned, not pruned")
v.step(claim("c5", "pn", attempt=2), "allow").step(outc("o5", "c5", "succeeded", "yes"), "allow", check={"effects.pn.approvals": [], "effects.pn.phase": "succeeded"})
add(v.done())

v = V("closed-sessions-leave-the-state", ["P-15"], "a closed session is removed from the state: steering or closing it again names nothing (invalid), and it grants no access")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s", "session.open", M, {"steerMode": "auto"}), "allow", check={"sessions.s.steers": 0})
v.step(E("s2", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(E("c", "session.close", M, {"session": "s"}), "allow", check={"sessions.s": "<missing>"})
v.access(M, "s", NONE)
v.step(E("in", "session.input", P, {"session": "s"}), "invalid", note="no such session")
v.step(E("c2", "session.close", O, {"session": "s"}), "invalid", note="no such session, even for an owner")
v.step(E("in2", "session.input", M, {"session": "s"}), "invalid", note="even for its actor")
v.step(E("t", "task", P, {"to": [BOT]}), "allow", check={"held": {BOT: OP}})
v.step(E("c3", "session.close", BOT, {"session": "s2"}), "allow", check={"held": {BOT: OP}, "guarded": {BOT: OP}})
v.step(E("in3", "session.input", BOT, {"session": "s2"}), "invalid", note="a held agent steering its closed session: nothing to steer, not a hold denial")
v.step(E("in4", "session.input", BOT, {"session": "s"}), "invalid", note="nor another one that is gone")
v.step(E("s3", "session.open", M, {"steerMode": "auto"}), "allow")
add(v.done())

v = V("agent-release-shape", ["P-15", "R-09", "§5"], "agent.release is {agent, unguard?: true}; the prefix agent is reserved; a release names an agent, not a session")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("in", "session.input", M, {"session": "s"}), "allow", check={"held": {BOT: OP}})
v.step(E("b0", "agent.release", OP, {}), "invalid", note="agent is required")
v.step(E("b1", "agent.release", OP, {"agent": BOT, "unguard": False}), "invalid", note="unguard is true or absent")
v.step(E("b2", "agent.release", OP, {"agent": BOT, "session": "s"}), "invalid", note="no session field")
v.step(E("b3", "agent.release", OP, {"agent": "not-an-actor"}), "invalid")
v.step(E("b4", "agent.note", OP, {"agent": BOT}), "invalid", note="agent.* is reserved: an unknown kind under a core prefix")
v.step(v.rel("h", BOT, BOT), "denied", note="a held agent's release is denied by the hold")
v.step(v.rel("n", OP, BOT2), "denied", note="BOT2 isn't held or guarded")
v.step(v.rel("m", OP, M), "denied", note="nor is a human")
v.step(v.rel("ok", OP, BOT), "allow", check={"held": {}, "cleared": {BOT: 9}, "guarded": {BOT: OP}})
add(v.done())

v = V("refunds-skip-closed-tasks", ["P-05", "P-06"], "a refund goes to the nearest open budgeted ancestor of the task it came from, starting at that task")
team(v, policy(budget=USD(1000)))
v.step(E("t", "task", O, {"to": [P], "budget": USD(600)}), "allow", check={"budgetLeft": USD(400)})
v.step(E("c", "task", P, {"to": [X], "parent": "t", "budget": USD(300)}), "allow", check={"tasks.t.left": USD(300)})
v.step(prop("p", author=X, executor=X, cost=USD(100), task="c"), "allow").step(claim("cl", "p"), "allow", check={"tasks.c.left": USD(200)})
v.step(E("fc", "task.update", O, {"task": "c", "state": "cancelled"}), "allow", check={"tasks.t.left": USD(500), "tasks.c.left": USD(0)}, note="the owner force-closes c: 200 goes to t")
v.step(outc("o", "cl", "succeeded", "yes", author=X, cost=USD(40)), "allow", check={"tasks.t.left": USD(560)}, note="the 60 refund skips closed c")
v.step(E("dt", "task.update", P, {"task": "t", "state": "done"}), "allow", check={"budgetLeft": USD(960)})
add(v.done())

v = V("reservations-block-budget-changes", ["P-06"], "a claim's unsettled reservation, like a task budget, locks the unit; raising the amount is fine")
team(v, policy(budget=USD(100)))
v.step(prop("p", cost=USD(40)), "allow").step(claim("c", "p"), "allow")
v.step(E("u1", "policy.set", O, {"policy": policy(budget={"unit": "EUR-cent", "amount": 100})}), "denied")
v.step(E("u2", "policy.set", O, {"policy": policy(budget=None)}), "denied")
v.step(E("u3", "policy.set", O, {"policy": policy(budget=USD(200))}), "allow", check={"budgetLeft": USD(160)})
v.step(outc("o", "c", "succeeded", "yes"), "allow", check={"budgetLeft": USD(160)})
v.step(E("u4", "policy.set", O, {"policy": policy(budget={"unit": "EUR-cent", "amount": 500})}), "allow", check={"budgetLeft": {"unit": "EUR-cent", "amount": 500}})
add(v.done())

v = V("repeatable-rearm-needs-approval", ["P-09", "P-10"], "after a repeatable attempt ends without success the effect is ready only if its approvals are in hand")
team(v, policy(approvals=[{"action": "*", "approvers": ["owner"]}]))
v.step(prop("p", repeatable=True), "allow").step(appr("a1", "p"), "allow").step(claim("c1", "p"), "allow")
v.step(outc("o1", "c1", "unknown", "unknown"), "allow", check={"effects.p.phase": "awaiting-approval"}, note="a1 is consumed")
v.step(appr("a2", "p"), "allow", check={"effects.p.phase": "ready"}).step(claim("c2", "p", attempt=2), "allow")
v.step(outc("o2", "c2", "failed", "yes"), "allow", check={"effects.p.phase": "awaiting-approval"})
v.step(appr("a3", "p"), "allow").step(claim("c3", "p", attempt=3), "allow")
v.step(outc("o3", "c3", "failed", "no"), "allow", check={"effects.p.phase": "ready"}, note="not started: a3 returned")
v.step(claim("c4", "p", attempt=4), "allow").step(E("x", "effect.cancel", O, {"proposal": "p"}), "allow")
v.step(outc("o4", "c4", "failed", "yes"), "allow", check={"effects.p.phase": "failed"}, note="a cancel was requested: no retry")
add(v.done())

v = V("outcome-cost-codes", ["P-06", "P-09", "P-10", "P-16"], "a cost the reservation can't cover, or in another unit, is invalid; so is any cost on resolve failed")
team(v, policy(budget=USD(100)))
v.step(prop("p", cost=USD(40)), "allow").step(claim("c", "p"), "allow")
v.step(outc("o0", "c", "succeeded", "yes", cost={"unit": "EUR-cent", "amount": 1}), "invalid", note="another unit")
v.step(outc("o1", "c", "unknown", "yes"), "allow", check={"effects.p.phase": "unknown"})
v.step(E("r0", "effect.resolve", X, {"proposal": "p", "result": "failed", "cost": USD(0)}), "invalid", note="cost only with succeeded")
v.step(E("r1", "effect.resolve", X, {"proposal": "p", "result": "succeeded", "cost": {"unit": "EUR-cent", "amount": 1}}), "invalid")
v.step(E("r2", "effect.resolve", X, {"proposal": "p", "result": "succeeded", "cost": USD(40)}), "allow", check={"budgetLeft": USD(60)})
add(v.done())

v = V("decision-order", ["P-16", "R-12", "R-05"], "the checks every kind shares run in a fixed order: shape, pins, author key, position (seq 1, seen, refs), denied server")
team(v, policy(servers={"deny": ["d.example"]}))
v.step(E("s1", "message", M, {"extra": 1}), "invalid", profiles={M: REVOKED()}, note="shape before the key")
v.step(E("s2", "message", M, {}, refs=[ABSENT_ID]), "denied", note="the key before references")
v.step(ms("s3", M, U, "member"), "invalid", note="pins before the key: U has no pinned profile")
v.step(E("s4", "space.create", M, {"policy": policy()}), "denied", note="the key before 'only at seq 1'")
v.step(E("s5", "space.create", M, {"policy": policy()}), "invalid", profiles={M: human()}, note="a valid key: now it is the second space.create")
v.step(E("s6", "message", Z, {}, refs=[ABSENT_ID]), "invalid", note="references before the denied server")
v.step(E("s7", "message", Z, {}), "denied", note="denied server")
v.step(E("s8", "message", O, {}, space="s2:a.example"), "invalid", note="the first entry of a space must be space.create")
vec = v.done(); del vec["profiles"][U]
add(vec)

v = V("kind-check-order", ["P-16"], "within a kind the checks run in the order of the P-16 table: references before state, state before authority only where the table says so")
team(v, policy(budget=USD(100), approvals=[{"action": "git.push", "approvers": ["owner"]}]))
v.step(ms("g", O, G, "guest"), "allow")
v.step(prop("pr1", cost=USD(10)), "allow").step(E("x1", "effect.cancel", O, {"proposal": "pr1"}), "allow", check={"effects.pr1.phase": "cancelled"})
v.step(appr("a0", "pr1", author=O, digest="other"), "invalid", note="digest before phase")
v.step(appr("a1", "pr1", author=O), "denied", note="cancelled")
v.step(prop("pr2", cost=USD(40)), "allow").step(appr("a2", "pr2"), "allow").step(claim("c2", "pr2"), "allow")
v.step(outc("o0", "c2", "succeeded", "yes", author=M, cost=USD(99)), "invalid", note="the cost before the author")
v.step(outc("o1", "c2", "succeeded", "yes", author=M, cost=USD(10)), "denied")
v.step(outc("o2", "c2", "succeeded", "yes"), "allow", check={"effects.pr2.phase": "succeeded"})
v.step(outc("o3", "c2", "succeeded", "no"), "denied", note="the claim is not open before the pair is judged")
v.step(prop("pr3", cost=USD(10)), "allow").step(appr("a3", "pr3"), "allow").step(claim("c3", "pr3"), "allow")
v.step(outc("o4", "c3", "unknown", "yes"), "allow", check={"effects.pr3.phase": "unknown"})
v.step(E("r0", "effect.resolve", M, {"proposal": "pr3", "result": "failed", "cost": USD(1)}), "denied", note="the author before the cost")
v.step(E("r1", "effect.resolve", X, {"proposal": "pr3", "result": "failed", "cost": USD(1)}), "invalid")
v.step(claim("c9", ABSENT_ID, author=M), "invalid", note="no such proposal, whoever asks")
v.step(claim("c8", "pr2", author=M), "denied")
v.step(E("ck0", "checkpoint", G, {"through": 99}), "denied", note="the role before the position")
v.step(E("ck1", "checkpoint", M, {"through": 99}), "invalid")
v.step(E("rd", "redact", Z, {"target": ABSENT_ID}), "invalid", note="the target exists before the author is judged")
add(v.done())

v = V("small-p04-rules", ["P-04", "P-05", "P-11"], "checkpoints need owner or member; task.cancel may repeat; a move to the current home is allowed")
team(v)
v.step(ms("g", O, G, "guest"), "allow")
v.step(E("ck", "checkpoint", G, {"through": 1}), "denied")
v.step(E("t", "task", M, {"to": [P]}), "allow").step(E("c1", "task.cancel", M, {"task": "t"}), "allow").step(E("c2", "task.cancel", M, {"task": "t"}), "allow")
v.step(E("mv", "space.move", O, {"to": "a.example", "accept": ACCEPT}), "allow", check={"home": "a.example"})
add(v.done())

v = V("owners-may-charge-any-task", ["P-06"], "naming a task needs being its assignee, for any task; owners are exempt")
team(v, policy(budget=USD(1000)))
v.step(E("t", "task", O, {"to": [P], "budget": USD(100)}), "allow").step(E("t2", "task", O, {"to": [P]}), "allow")
v.step(prop("p1", author=O, cost=USD(10), task="t"), "allow", note="an owner who is not an assignee")
v.step(prop("p2", author=M, cost=USD(10), task="t"), "denied")
v.step(prop("p3", author=M, cost=USD(10), task="t2"), "denied", note="also for a task without a budget")
v.step(prop("p4", author=P, cost=USD(10), task="t2"), "allow")
add(v.done())

v = V("decide-ignores-log-fields", ["R-25", "R-11"], "Policy reads no nonce, header time, contentHash or sig: those belong to the Log, and vectors may stub them")
team(v)
e = E("m", "message", M, {}); e["nonce"] = ""; e["time"] = "yesterday"; e["contentHash"] = "not-a-hash"; e["sig"] = "whatever"
v.step(e, "allow")
add(v.done())

v = V("only-humans-create-spaces", ["P-03", "P-11"], "a space is created by a human (its first owner) and can't deny its own home")
v.prof(BOT, AG)
v.step(create(author=BOT, id="C0"), "denied", note="an agent can't become an owner by creating a space")
v.step(create(policy(servers={"deny": ["a.example"]}), id="C1"), "invalid", note="denies its own home")
v.step(create(), "allow")
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

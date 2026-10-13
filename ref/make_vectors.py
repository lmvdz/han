"""Writes conformance/vectors/*.json. Expectations below are hand-written from spec/core/architecture.md, NOT computed by policy.py.
Each vector is fully expanded (every entry field, every profile, the full policy), so a stranger needs no macros.
"""
import copy, hashlib, json, os
from datetime import datetime, timedelta, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(os.path.dirname(HERE), "conformance", "vectors")
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

def own_guards(actor, expect):
    return expect if "guards" in expect else dict(expect, guards=[actor] if expect["guarded"] else [])

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
    def ung(self, id, author, agent, space=S):
        """A guard's `agent.unguard {agent}`: it ends the agent's own guarded choice (P-15)."""
        return E(id, "agent.unguard", author, {"agent": agent}, space=space)
    def read(self, actor, seq, expect, space=S):
        self.v["steps"].append({"read": {"space": space, "actor": actor, "seq": seq}, "expect": expect}); return self
    def access(self, actor, session, expect, space=S):
        self.v["steps"].append({"sessionAccess": {"space": space, "actor": actor, "session": session}, "expect": expect}); return self
    def guard(self, actor, expect, space=S):
        """guardStatus; `guards` (round 32, Z1), when the expectation leaves it out, is the actor alone if guarded: no carrying."""
        self.v["steps"].append({"guardStatus": {"space": space, "actor": actor}, "expect": own_guards(actor, expect)}); return self
    def actors(self, entry, expect, pinned=None):
        """relevantActors at the current time; `pinned`: only these actors' profiles are known yet (the sequencer's first rounds, P-01)."""
        st = {"relevantActors": entry, "time": (BASE + timedelta(seconds=self.t)).strftime("%Y-%m-%dT%H:%M:%SZ"), "expect": expect}
        if pinned is not None: st["pinned"] = pinned
        self.v["steps"].append(st); return self
    def guard_view(self, actor, view, expect):
        """guardStatus on a `state {agent}` reply as is (GuardInput is `{policy, guarded, tasks}`, which both the state and the reply satisfy)."""
        self.v["steps"].append({"guardStatus": {"view": view, "actor": actor}, "expect": own_guards(actor, expect)}); return self
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
        kinds = {a: p["actorKind"] for a, p in self.v["profiles"].items()}
        def kind_of_approvals(x):
            """An approval in the state records its author's actorKind (P-08, round 30): it is copied from the author's profile, an
            input of the vector, so the checks below need not repeat it."""
            if isinstance(x, list): return [kind_of_approvals(y) for y in x]
            if isinstance(x, dict) and {"id", "author", "keyId"} <= set(x) and "actorKind" not in x:
                return dict(x, actorKind=kinds.get(x["author"], "human"))
            return x
        for st in self.v["steps"]:
            if "check" in st: st["check"] = {k: kind_of_approvals(x) for k, x in st["check"].items()}
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

def NEST(n):
    """n nested objects around 1: {"a": {"a": … 1}} (n = 0 gives 1)."""
    v = 1
    for _ in range(n): v = {"a": v}
    return v

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
v.step(prop("pr"), "allow", check={"effects.pr.phase": "open"})
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
v.step(prop("pr"), "allow", check={"effects.pr.phase": "open"})
v.step(claim("c0", "pr"), "denied", note="no approval")
v.step(appr("a0", "pr", author=M), "denied", note="members are not approvers")
v.step(appr("a1", "pr", digest="other"), "invalid", note="wrong digest")
v.step(appr("a2", "pr"), "allow", check={"effects.pr.phase": "open"})
v.step(claim("c1", "pr"), "allow", check={"effects.pr.approvals.0.consumedBy": "c1"})
v.step(outc("o1", "c1", "failed", "no"), "allow", check={"effects.pr.phase": "open"}, note="not started → approval returned")
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
v.step(outc("q1", "d1", "unknown", "unknown"), "allow", check={"effects.p2.phase": "open"})
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
v.step(E("r1", "effect.resolve", X, {"proposal": "p", "result": "failed"}), "allow", at=145, check={"effects.p.phase": "open"})
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
v = V("relevant-actors", ["P-01"], "the receipt must pin profiles for these actors: with nothing pinned yet every approval candidate, with the profiles pinned the candidates up to the first valid key")
team(v, policy(approvals=[{"action": "*", "approvers": ["owner", M]}]))
v.step(prop("pr"), "allow").step(appr("a1", "pr"), "allow").step(appr("a2", "pr", author=M), "allow")
v.actors(claim("c", "pr"), [X, P, O, M], pinned=[])
v.actors(claim("c", "pr"), [X, P, O])
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
v.step(outc("o1", "c1", "unknown", "unknown"), "allow", check={"effects.p.phase": "open", "budgetLeft": USD(60)})
v.step(claim("c2", "p", attempt=2), "allow", check={"budgetLeft": USD(20)})
v.step(outc("o2", "c2", "succeeded", "yes", cost=USD(10)), "allow", check={"budgetLeft": USD(50)})
add(v.done())

v = V("approval-rule-removed", ["P-08", "P-11"], "removing the approval rule releases effects waiting for approval")
team(v, policy(approvals=[{"action": "*", "approvers": ["owner"]}]))
v.step(prop("p"), "allow", check={"effects.p.phase": "open"})
v.step(E("ps", "policy.set", O, {"policy": policy()}), "allow", check={"effects.p.phase": "open"})
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
v.step(prop("push", author=FIX, executor=GIT, cost=USD(50), task="t"), "allow", check={"effects.push.phase": "open"})
v.step(appr("ok", "push", author=LARS), "allow", check={"effects.push.phase": "open"})
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
XS = dict(human(), actorKind="service", operatorConfirmed=True, operator=OP)

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

v = V("repeatable-cancelled-started", ["P-09"], "a repeatable effect stopped partway (cancelled, started:yes) with no cancel requested goes back to open")
team(v)
v.step(prop("p", repeatable=True), "allow").step(claim("c", "p"), "allow")
v.step(outc("o", "c", "cancelled", "yes"), "allow", check={"effects.p.phase": "open"})
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
v.step(E("s2", "session.open", M, {"steerMode": "guarded"}), "allow", check={"guarded": {}}, note="a human's guarded session guards no one: humans are never guarded")
add(v.done())

def agents(v, pol):
    """team plus OP (human member) operating BOT and BOT2 (agents, members)."""
    v.prof(BOT, AG).prof(BOT2, AG).prof(X, XS)
    team(v, pol)
    v.step(ms("op", O, OP, "member"), "allow").step(ms("b", O, BOT, "member"), "allow").step(ms("b2", O, BOT2, "member"), "allow")

def nxt(v): return v.heads[S] + 1          # the seq the next allowed step gets
MEMBERS3 = sess("member", "member", "member")
OP2 = "@op2:b.example"
AG2 = dict(AG, operator=OP2)

# ── P-15: guarding is a fact about the work, recorded on it when it is created (round 27) ──
ABSENT_SESSION = hashlib.sha256(b"absent session").hexdigest()
BOT3 = "@bot3:b.example"

v = V("guarded-effects-need-the-guard", ["P-15", "P-07 E3"], "an effect whose proposer or executor is guarded needs that agent's guard at claim, its own operator's proposal included; work created after an unguard needs no one")
agents(v, policy(sessions=MEMBERS3))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(prop("q", author=OP, executor=BOT), "allow", check={"effects.q.guardedFor": [BOT]}, note="its operator's own proposal")
v.step(claim("c0", "q", author=BOT), "denied", note="the executor is guarded: OP must approve, even its own proposal")
v.step(appr("a0", "q", author=M), "denied", note="not the guard and no approval rule")
v.step(appr("aq", "q", author=OP), "allow").step(claim("cq", "q", author=BOT), "allow")
v.step(prop("h", author=M), "allow", check={"effects.h.guardedFor": []}).step(claim("ch", "h"), "allow", note="neither proposer nor executor is guarded")
v.step(v.ung("u", OP, BOT), "allow", check={"guarded": {}})
v.step(prop("d", author=P, executor=BOT), "allow", check={"effects.d.guardedFor": []})
v.step(claim("c2", "d", author=BOT), "allow", note="created after the unguard: no guard")
add(v.done())




v = V("modes-restrict-opening-only", ["P-15"], "policy.sessions.modes only limits which modes session.open may use: open sessions keep their mode, and guarding comes from the agent's choice alone")
v.prof(BOT, AG).prof(BOT2, dict(AG, operatorConfirmed=False))
team(v, policy(sessions=MEMBERS3))
v.step(ms("op", O, OP, "member"), "allow").step(ms("b", O, BOT, "member"), "allow").step(ms("b2", O, BOT2, "member"), "allow")
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("s2", "session.open", BOT2, {"steerMode": "auto"}), "allow")
v.step(E("ps", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member", ["auto"]))}), "allow", check={"guarded": {BOT: True}})
v.step(E("o1", "session.open", BOT2, {"steerMode": "guarded"}), "denied", note="no longer an allowed mode")
v.access(M, "s", ALL).step(E("in", "session.input", M, {"session": "s"}), "allow", check={"sessions.s.steerMode": "guarded"}, note="its guarded session stays live and guarded")
v.step(prop("p", author=BOT, executor=BOT), "allow", check={"effects.p.guardedFor": [BOT]}, note="and the agent stays guarded")
v.step(E("ps2", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member", ["guarded"]))}), "allow")
v.access(M, "s2", ALL).step(E("in2", "session.input", M, {"session": "s2"}), "allow", check={"sessions.s2.steerMode": "auto", "guarded": {BOT: True}}, note="an auto session stays live and auto; BOT2 chose nothing")
v.step(E("o2", "session.open", BOT2, {"steerMode": "auto"}), "denied")
add(v.done())

v = V("two-operators", ["P-15"], "each agent is guarded for its own operator; another operator can neither unguard nor approve for it")
v.prof(BOT, AG).prof(BOT2, AG2).prof(X, XS)
team(v, policy(sessions=MEMBERS3))
v.step(ms("op", O, OP, "member"), "allow").step(ms("op2", O, OP2, "member"), "allow")
v.step(ms("b", O, BOT, "member"), "allow").step(ms("b2", O, BOT2, "member"), "allow")
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("s2", "session.open", BOT2, {"steerMode": "guarded"}), "allow")
v.step(E("in", "session.input", OP2, {"session": "s"}), "allow", note="OP2 is an outsider to BOT: allowed, and inert")
v.step(v.ung("r", OP2, BOT), "denied")
v.step(prop("p", author=BOT, executor=BOT), "allow").step(appr("a2", "p", author=OP2), "denied", note="not BOT's guard, no rule")
v.step(appr("a1", "p", author=OP), "allow").step(claim("c", "p", author=BOT), "allow")
v.step(prop("q", author=BOT, executor=BOT2), "allow", check={"effects.q.guardedFor": [BOT, BOT2]})
v.step(appr("b1", "q", author=OP), "allow").step(claim("cq0", "q", author=BOT2), "denied", note="BOT2's guard OP2 hasn't approved")
v.step(appr("b2", "q", author=OP2), "allow").step(claim("cq", "q", author=BOT2), "allow", check={"effects.q.claims.0.guardApprovals": ["b1", "b2"]})
add(v.done())

v = V("guards-are-checked", ["P-15", "P-04", "R-12", "P-07 E3"], "a guard's approval needs a role, a valid key and no expiry, like any approval; guards may refuse")
agents(v, policy(sessions=MEMBERS3))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(prop("p", author=BOT, executor=BOT), "allow").step(appr("a1", "p", author=OP), "allow")
v.step(claim("c0", "p", author=BOT), "denied", profiles={OP: human({"k1": {"publicKey": "pk", "status": "revoked"}})}, note="guard's key revoked")
v.step(ms("ban", O, OP, "none"), "allow")
v.step(claim("c1", "p", author=BOT), "denied", profiles={OP: human()}, note="guard lost its role")
v.step(prop("p2", author=BOT, executor=BOT), "allow")
v.step(appr("a2", "p2", author=OP), "denied", note="a guard with no role can't approve")
v.step(ms("back", O, OP, "member"), "allow")
v.step(E("x", "effect.cancel", OP, {"proposal": "p2"}), "allow", profiles={OP: human()}, check={"effects.p2.phase": "cancelled"}, note="a guard may refuse")
add(v.done())

v = V("guarded-needs-an-operator", ["P-15"], "an agent may open a guarded session only if its confirmed operator holds a role")
v.prof(BOT, AG)
team(v, policy(sessions=MEMBERS3))
v.step(ms("b", O, BOT, "member"), "allow")
v.step(E("s1", "session.open", BOT, {"steerMode": "guarded"}), "denied", note="OP isn't in the space")
v.step(E("s2", "session.open", BOT, {"steerMode": "auto"}), "allow")
v.step(E("t", "task", O, {"to": [M]}), "allow").step(E("s3", "session.open", BOT, {"steerMode": "auto", "task": "t"}), "denied", note="not its task")
add(v.done())


v = V("open-approvals-capped", ["P-08", "P-01"], "an author's new approval supersedes its old one; at most 16 open approvals by other authors per proposal, guards' not counted (R3); expired ones don't count")
v.prof(BOT, AG)
team(v, policy(approvals=[{"action": "*", "approvers": ["owner", "member"]}], sessions=MEMBERS3))
v.step(ms("op", O, OP, "member"), "allow").step(ms("b", O, BOT, "member"), "allow")
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("in", "session.input", M, {"session": "s"}), "allow")
v.step(prop("p", author=BOT, executor=BOT), "allow")
v.step(appr("a0", "p", author=M), "allow").step(appr("a0b", "p", author=M), "allow", check={"effects.p.approvals": [{"id": "a0b", "author": M, "keyId": "k1"}]}, note="a new approval supersedes M's own, and the old one is removed")
v.step(appr("gok", "p", author=OP), "allow", note="the guard's approval is open, and never counted toward the cap")
MEMBERS = [f"@m{i}:a.example" for i in range(1, 16)]
for i, a_ in enumerate(MEMBERS):
    v.step(ms(f"mm{i}", O, a_, "member"), "allow").step(appr(f"a{i + 1}", "p", author=a_, expires="2026-10-10T13:00:00Z"), "allow")
v.step(appr("a16", "p", author=O), "denied", note="16 others that aren't guards are open: the cap")
v.step(appr("a17", "p", author=O), "allow", at=4000, check={"effects.p.approvals": [{"id": "a0b", "author": M, "keyId": "k1"}, {"id": "gok", "author": OP, "keyId": "k1"}, {"id": "a17", "author": O, "keyId": "k1"}]}, note="15 of them expired: they no longer count, and the touch dropped them")
add(v.done())

v = V("approvals-stay-bounded", ["P-08"], "any entry touching an effect drops its expired approvals: however many approvers come and go, the state holds only the live ones")
team(v, policy(join="open", approvals=[{"action": "pay", "approvers": ["member"]}]))
v.step(prop("p", action="pay"), "allow")
for i in range(20):
    a_ = f"@w{i}:a.example"
    v.step(ms(f"j{i}", a_, a_, "member"), "allow")
    live = [{"id": f"w{i}", "author": a_, "keyId": "k1", "expiresAt": (BASE + timedelta(seconds=v.t + 13)).strftime("%Y-%m-%dT%H:%M:%SZ")}]
    v.step(appr(f"w{i}", "p", author=a_, expires=live[0]["expiresAt"]), "allow", dt=10, check={"effects.p.approvals": live}, note="the earlier ones expired and were dropped")
v.step(E("x", "effect.cancel", "@w0:a.example", {"proposal": "p"}), "denied", note="the cancel right lapsed with the approval")
v.step(E("x2", "effect.cancel", P, {"proposal": "p"}), "allow", dt=10, check={"effects.p.approvals": []}, note="a touch drops the last expired approval")
add(v.done())

v = V("departed-guard", ["P-15", "P-02"], "while a guard holds no role nobody stands in for it: its agent's effects can be cancelled but not approved or claimed, and the guard can't unguard")
agents(v, policy(sessions=MEMBERS3))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(prop("p", author=BOT, executor=BOT), "allow")
v.step(ms("gone", O, OP, "none"), "allow")
v.step(appr("a1", "p", author=O), "denied", note="owners don't stand in")
v.step(v.ung("r", O, BOT), "denied")
v.step(v.ung("r1", OP, BOT), "denied", profiles={OP: human()}, note="the guard itself holds no role")
v.step(claim("c0", "p", author=BOT), "denied")
v.step(E("x", "effect.cancel", O, {"proposal": "p"}), "allow", check={"effects.p.phase": "cancelled"}, note="owners may always cancel")
v.step(ms("back", O, OP, "member"), "allow").step(v.ung("r2", OP, BOT), "allow", check={"guarded": {}})
add(v.done())


v = V("expired-approvals-are-gone", ["P-08", "P-04"], "an already-expired approval is refused; an approval expires out of the state with its cancel right on the next entry that touches the effect")
team(v, policy(approvals=[{"action": "*", "approvers": ["owner", "member"]}]))
v.step(prop("p"), "allow")
v.step(appr("a0", "p", author=M, expires="2000-01-01T00:00:00Z"), "denied", note="already expired")
v.step(appr("a1", "p", author=M, expires="2026-10-10T12:00:20Z"), "allow")
v.step(E("x0", "effect.cancel", M, {"proposal": "p"}), "denied", at=30, note="a1 expired: M's cancel right lapsed with it")
v.step(appr("a2", "p", author=O), "allow", check={"effects.p.approvals": [{"id": "a2", "author": O, "keyId": "k1"}]}, note="a1 is dropped by this touch")
add(v.done())




v = V("guarded-is-sticky", ["P-15"], "owners can't un-guard an agent: closing its session, evicting its guard or toggling modes leaves it guarded")
agents(v, policy(sessions=MEMBERS3))
v.step(E("s1", "session.open", BOT, {"steerMode": "guarded"}), "allow", check={"guarded": {BOT: True}})
v.step(E("s3", "session.open", BOT, {"steerMode": "auto"}), "allow")
v.step(E("c", "session.close", O, {"session": "s1"}), "allow", check={"guarded": {BOT: True}}, note="an owner closing it doesn't un-guard")
v.step(ms("ev", O, OP, "none"), "allow", check={"guarded": {BOT: True}}, note="nor does evicting its guard")
v.step(E("ps", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member", ["auto"]))}), "allow", check={"guarded": {BOT: True}}, note="nor do the modes")
v.step(prop("p", author=BOT), "allow", check={"effects.p.guardedFor": [BOT]})
v.step(appr("a", "p", author=O), "denied", note="nobody stands in")
v.step(claim("c0", "p"), "denied")
add(v.done())

v = V("modes-toggle-cannot-unguard", ["P-15"], "switching modes to auto-only and back doesn't unguard an agent")
agents(v, policy(sessions=MEMBERS3))
v.step(E("s1", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("s3", "session.open", BOT, {"steerMode": "auto"}), "allow")
v.step(E("ps", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member", ["auto"]))}), "allow")
v.step(E("ps2", "policy.set", O, {"policy": policy(sessions=MEMBERS3)}), "allow", check={"guarded": {BOT: True}})
v.step(prop("p", author=BOT, executor=BOT), "allow", check={"effects.p.guardedFor": [BOT]}, note="still guarded")
add(v.done())

v = V("agents-cannot-unguard-themselves", ["P-15"], "closing sessions never unguards an agent; it can't unguard itself, and nor can an owner")
agents(v, policy(sessions=MEMBERS3))
v.step(E("s1", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("s3", "session.open", BOT, {"steerMode": "auto"}), "allow")
v.step(E("c1", "session.close", BOT, {"session": "s1"}), "allow", check={"guarded": {BOT: True}}, note="closing its last guarded session doesn't unguard")
v.step(E("c3", "session.close", BOT, {"session": "s3"}), "allow", check={"guarded": {BOT: True}})
v.step(E("s2", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("c2", "session.close", BOT, {"session": "s2"}), "allow", check={"guarded": {BOT: True}})
v.step(v.ung("u0", BOT, BOT), "denied", note="an agent can't unguard itself")
v.step(v.ung("u1", O, BOT), "denied", note="nor can an owner")
v.step(E("c4", "session.close", O, {"session": "s1"}), "invalid", note="a closed session is no longer in the state: no such session")
v.step(E("c5", "session.close", BOT, {"session": "s1"}), "invalid", note="even for its actor")
add(v.done())

v = V("guard-unguards", ["P-15"], "only the guard ends an agent's guarded choice, with agent.unguard, needing no session; session.input carries no release flags")
agents(v, policy(sessions=MEMBERS3))
v.step(E("s1", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("s3", "session.open", BOT, {"steerMode": "auto"}), "allow")
v.step(E("c1", "session.close", BOT, {"session": "s1"}), "allow", note="a live guarded session would not matter either")
v.step(E("bad", "session.input", OP, {"session": "s3", "unguard": True}), "invalid", note="F2: session.input is {session} only")
v.step(E("bad2", "session.input", OP, {"session": "s3", "release": True}), "invalid", note="F2: no release flag on a steer")
v.step(v.ung("u", OP, BOT), "allow", check={"guarded": {}}, note="names the agent, no session needed")
v.step(v.ung("u2", OP, BOT), "denied", note="no longer guarded")
v.step(v.ung("v00", OP, BOT2), "denied", note="BOT2 was never guarded")
v.step(E("s4", "session.open", BOT2, {"steerMode": "guarded"}), "allow").step(E("c4", "session.close", BOT2, {"session": "s4"}), "allow")
v.step(v.ung("v0", M, BOT2), "denied", note="not the guard")
v.step(v.ung("v1", OP2, BOT2), "denied", note="another operator")
v.step(v.ung("v2", OP, BOT2), "allow", check={"guarded": {}}, note="no session: the guard unguards")
add(v.done())

v = V("unguard-does-not-loosen-forcing", ["P-15"], "an unguard clears only the agent's own choice: owner forcing (forceGuarded) still guards it")
agents(v, policy(sessions=sess("member", "member", "member", MODES, True)))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow", check={"guarded": {BOT: True}}).step(E("c", "session.close", BOT, {"session": "s"}), "allow")
v.step(v.ung("u", OP, BOT), "allow", check={"guarded": {}})
v.guard(BOT, {"guard": OP, "guarded": True, "forced": True})
v.step(prop("p", author=BOT, executor=BOT), "allow", check={"effects.p.guardedFor": [BOT]}, note="still forced")
v.step(v.ung("u2", OP, BOT), "denied", note="forcing is the owners' to switch off, not the guard's")
add(v.done())

# ── forced spaces: the operator in the profile pinned for each entry ──
v = V("force-guarded-agents-need-no-session", ["P-15"], "with forceGuarded, an agent with a confirmed operator is guarded without any session: its tasks and effects record it, and its effects need its operator")
agents(v, policy(sessions=sess("member", "member", "member", MODES, True)))
v.step(E("t0", "task", OP, {"to": [BOT]}), "allow", check={"tasks.t0.guardedFor": []}, note="its operator is human: never guarded")
v.step(E("t", "task", BOT, {"to": [BOT2]}), "allow", check={"tasks.t.guardedFor": [BOT]}, note="no session needed")
v.step(prop("d", author=M, executor=BOT2), "allow", check={"effects.d.guardedFor": [BOT2]}, note="a forced executor")
v.step(claim("c0", "d", author=BOT2), "denied", note="its guard must approve")
v.step(appr("a", "d", author=OP), "allow").step(claim("c1", "d", author=BOT2), "allow")
add(v.done())

v = V("force-guarded-is-off-by-default", ["P-15"], "without forceGuarded (the default) a session-less agent is not guarded; switching it on guards what is created from then on, and every claim from then on")
agents(v, policy(sessions=MEMBERS3))
v.step(prop("p", author=BOT, executor=BOT), "allow", check={"effects.p.guardedFor": []})
v.step(E("pf", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member", MODES, "yes"))}), "invalid", note="forceGuarded is a boolean")
v.step(E("ps", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member", MODES, True))}), "allow", check={"effects.p.guardedFor": []}, note="nothing recorded changes")
v.step(claim("c0", "p", author=BOT), "denied", note="its proposer is forced at the claim")
v.step(prop("q", author=BOT, executor=BOT), "allow", check={"effects.q.guardedFor": [BOT]})
v.step(appr("a", "p", author=OP), "allow").step(claim("c1", "p", author=BOT), "allow")
add(v.done())

v = V("force-guarded-spares-humans", ["P-15"], "forcing guards non-human actors only: humans are never guarded; a service is guarded like an agent, executor slot included")
agents(v, policy(sessions=sess("member", "member", "member", MODES, True)))
v.prof(X, dict(XS, operator=OP))
v.step(E("t", "task", P, {"to": [M, O]}), "allow", check={"tasks.t.guardedFor": []}, note="humans")
v.step(prop("h", author=M, executor=M), "allow", check={"effects.h.guardedFor": []})
v.step(prop("d", author=P, executor=X), "allow", check={"effects.d.guardedFor": [X]}, note="H6: a service as executor is guarded too; there is no self-declared exemption")
v.guard(M, {"guarded": False, "forced": False}).guard(X, {"guard": OP, "guarded": True, "forced": True})
add(v.done())

SVC = "@svc:b.example"
SVC = "@svc:b.example"
v = V("service-tasked-or-steered-is-forced", ["P-15"], "CX2, H6: a service is forced like an agent, as an author, an assignee or an executor; forced, it may not approve")
agents(v, policy(sessions=sess("member", "member", "member", MODES, True), approvals=[{"action": "git.push", "approvers": ["member"]}]))
v.prof(SVC, dict(XS, operator=OP))
v.step(ms("svc", O, SVC, "member"), "allow")
v.step(E("s", "session.open", SVC, {"steerMode": "auto"}), "allow")
v.step(E("in", "session.input", M, {"session": "s"}), "allow", note="steering an LLM labelled 'service' is allowed, and inert")
v.step(E("t", "task", SVC, {"to": [BOT]}), "allow", check={"tasks.t.guardedFor": [SVC]}, note="a forced author")
v.step(prop("d", author=P, executor=SVC), "allow", check={"effects.d.guardedFor": [SVC]}, note="a forced executor (H6)")
v.step(appr("ap", "d", author=SVC), "denied", note="a member may approve git.push here, but not a forced one")
v.step(appr("am", "d", author=M), "allow")
add(v.done())

NOOP = dict(AG, operatorConfirmed=False)

v = V("force-guarded-late-confirmation", ["P-15", "P-01"], "CX1: an agent its operator confirms only after it joined is forced all along; its effects wait until that operator approves")
v.prof(BOT, NOOP).prof(X, XS)
team(v, policy(sessions=sess("member", "member", "member", MODES, True)))
v.step(ms("op", O, OP, "member"), "allow").step(ms("b", O, BOT, "member"), "allow")
v.step(prop("p", author=BOT, executor=BOT), "allow", check={"effects.p.guardedFor": [BOT]}, note="forced, with no confirmed operator")
v.step(claim("c0", "p", author=BOT), "denied", note="no confirmed operator: it can't be claimed for")
v.step(appr("a0", "p", author=OP), "denied", note="OP is no guard until it confirms, and there is no rule")
v.step(E("w", "message", M, {}), "allow", profiles={BOT: AG}, note="OP confirms")
v.step(claim("c1", "p", author=BOT), "denied", note="the guard of a forced agent approves")
v.step(appr("a", "p", author=OP), "allow").step(claim("c2", "p", author=BOT), "allow")
add(v.done())


v = V("forcing-uses-each-entrys-profile", ["P-15", "P-01"], "the guard of a forced agent is the operator in the profile pinned for each entry: an approval, an unguard and a claim each read their own")
v.prof(BOT, AG)
team(v, policy(sessions=sess("member", "member", "member", MODES, True)))
v.step(ms("op", O, OP, "member"), "allow").step(ms("op2", O, OP2, "member"), "allow").step(ms("b", O, BOT, "member"), "allow")
v.step(prop("p", author=BOT, executor=BOT), "allow", profiles={BOT: AG2}, check={"effects.p.guardedFor": [BOT]}, note="pinned operator OP2")
v.step(appr("a1", "p", author=OP), "denied", note="OP is not the guard in the profile in force")
v.step(appr("a2", "p", author=OP2), "allow")
v.step(claim("c0", "p", author=BOT), "allow", note="OP2 is the guard at the claim too")
v.step(prop("q", author=BOT, executor=BOT), "allow", profiles={BOT: AG})
v.step(appr("b2", "q", author=OP2), "denied", note="the next entry pins OP: OP2 is an outsider now")
add(v.done())

v = V("instruction-targets-are-pinned", ["P-01"], "relevantActors lists the targets of an instruction (the session's actor, each assignee, the executor), the agent of an unguard, the proposer, executor and guardedFor of an approve, cancel or claim, and a claim's candidates")
agents(v, policy(sessions=MEMBERS3, approvals=[{"action": "*", "approvers": ["member"]}]))
v.step(E("s", "session.open", BOT, {"steerMode": "auto"}), "allow")
v.actors(E("in", "session.input", M, {"session": "s"}), [M, BOT])
v.actors(E("in2", "session.input", M, {"session": ABSENT_SESSION}), [M])
v.actors(E("t", "task", M, {"to": [BOT2, M, BOT]}), [M, BOT2, BOT])
v.actors(prop("d", author=M, executor=BOT), [M, BOT])
v.actors(E("r", "agent.unguard", OP, {"agent": BOT}), [OP, BOT])
v.step(prop("d", author=M, executor=BOT), "allow").step(appr("a", "d", author=P), "allow")
v.actors(appr("a2", "d", author=O), [O, M, BOT])
v.actors(E("x", "effect.cancel", O, {"proposal": "d"}), [O, M, BOT])
v.actors(claim("c", "d", author=BOT), [BOT, M, P])
v.step(E("g", "session.open", BOT2, {"steerMode": "guarded"}), "allow").step(E("tg", "task", BOT2, {"to": [BOT]}), "allow")
v.step(prop("e", author=BOT, executor=X, task="tg"), "allow", check={"effects.e.guardedFor": [BOT2]})
v.actors(appr("a3", "e", author=O), [O, BOT, X, BOT2])
add(v.done())


v = V("returned-approvals-stay-superseded", ["P-08"], "an approval returned by a not-started outcome is removed if its author approved again meanwhile")
team(v, policy(approvals=[{"action": "*", "approvers": ["owner"]}]))
v.step(prop("p"), "allow").step(appr("a1", "p", author=O), "allow").step(claim("c", "p"), "allow")
v.step(appr("a2", "p", author=O), "allow")
v.step(outc("o", "c", "failed", "no"), "allow", check={"effects.p.approvals": [{"id": "a2", "author": O, "keyId": "k1"}]}, note="A5: one open approval per author; the returned one is gone")
add(v.done())













v = V("readiness-is-decided-at-claim", ["P-15", "P-07 E3", "P-06"], "K3: an effect is `open` until a claim, an outcome or a cancel changes it; whether a claim may proceed is judged by E3 at that moment (reviewer's s3, s4 and fuzz seed 1248)")
agents(v, policy(sessions=MEMBERS3, approvals=[{"action": "pay", "approvers": ["member"]}]))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(prop("p", author=BOT, action="git.push", executor=X), "allow", check={"effects.p.phase": "open"})
v.step(claim("c0", "p"), "denied", note="s3: the claim is judged now: the guarded proposer's operator hasn't approved")
v.step(appr("a", "p", author=OP), "allow").step(claim("c1", "p"), "allow", check={"effects.p.phase": "claimed"})
v.step(outc("o1", "c1", "failed", "no"), "allow", check={"effects.p.phase": "open", "effects.p.approvals": [{"id": "a", "author": OP, "keyId": "k1"}]}, note="s4: back to open, the guard's approval returned")
v.step(appr("a2", "p", author=OP), "allow", check={"effects.p.approvals": [{"id": "a2", "author": OP, "keyId": "k1"}]}, note="it supersedes the returned one")
v.step(claim("c2", "p", attempt=2), "allow", note="s4: nothing is stuck")
v.step(outc("o2", "c2", "succeeded", "yes"), "allow")
v.step(prop("q", author=P, action="pay"), "allow", check={"effects.q.phase": "open"})
v.step(appr("aq", "q", author=M), "allow").step(ms("dm", O, M, "guest"), "allow", check={"effects.q.phase": "open"}, note="seed 1248: an approver's role change touches no phase")
v.step(claim("cq", "q"), "denied", note="M is no approver now: E3 finds no usable approval")
v.step(ms("rm", O, M, "member"), "allow").step(claim("cq2", "q"), "allow")
add(v.done())

v = V("guard-status", ["P-15"], "H8, K1, P1: guard_status(state, ctx, actor) = {guard?, guarded, forced}: guard is the confirmed operator in the profile pinned for the entry (absent for humans and unconfirmed operators); forced is the owners' flag for a non-human; guarded is forced or the agent's own choice")
agents(v, policy(sessions=MEMBERS3))
v.guard(BOT, {"guard": OP, "guarded": False, "forced": False}).guard(P, {"guarded": False, "forced": False})
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.guard(BOT, {"guard": OP, "guarded": True, "forced": False}).guard(BOT2, {"guard": OP, "guarded": False, "forced": False})
v.step(E("ps", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member", MODES, True))}), "allow")
v.guard(BOT, {"guard": OP, "guarded": True, "forced": True}).guard(BOT2, {"guard": OP, "guarded": True, "forced": True}).guard(P, {"guarded": False, "forced": False})
v.step(E("w", "message", M, {}), "allow", profiles={BOT2: dict(AG, operatorConfirmed=False)}, note="BOT2's operator withdraws")
v.guard(BOT2, {"guarded": True, "forced": True})
v.step(E("ps2", "policy.set", O, {"policy": policy(sessions=MEMBERS3)}), "allow", note="forcing off")
v.guard(BOT, {"guard": OP, "guarded": True, "forced": False}).guard(BOT2, {"guarded": False, "forced": False}).guard(P, {"guarded": False, "forced": False})
add(v.done())


# ── round 27 (DECISIONS-r27 P9): guarding recorded on the work; no holds ──
v = V("guarded-proposals-need-the-guard", ["P-15", "P-07 E3"], "N2: a guarded agent may propose work for a separate executor; the effect records it in guardedFor, and a claim needs its guard's own approval besides any rule")
agents(v, policy(sessions=MEMBERS3, approvals=[{"action": "git.push", "approvers": ["member"]}]))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow", check={"guarded": {BOT: True}})
v.step(prop("p", author=BOT), "allow", check={"effects.p.guardedFor": [BOT], "effects.p.phase": "open"}, note="N2: X is a separate executor (a git service)")
v.step(appr("a0", "p", author=M), "allow").step(claim("c0", "p"), "denied", note="the rule is met, the guard hasn't approved")
v.step(appr("a1", "p", author=OP, expires="2026-10-10T12:00:25Z"), "allow")
v.step(claim("c1", "p"), "denied", at=40, note="the guard's approval expired")
v.step(appr("a2", "p", author=OP), "allow")
v.step(claim("c2", "p"), "allow", check={"effects.p.claims.0.approval": "a0", "effects.p.claims.0.guardApprovals": ["a2"]})
add(v.done())

v = V("unguarding-frees-no-recorded-work", ["P-15"], "N1: what a guarded agent proposed or delegated keeps needing its guard after the guard unguards it; work created after the unguard does not")
agents(v, policy(sessions=MEMBERS3))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(prop("p", author=BOT, executor=BOT), "allow", check={"effects.p.guardedFor": [BOT]})
v.step(E("t", "task", BOT, {"to": [BOT2]}), "allow", check={"tasks.t.guardedFor": [BOT]})
v.step(v.ung("u", OP, BOT), "allow", check={"guarded": {}})
v.step(claim("c0", "p", author=BOT), "denied", note="N1: recorded when proposed; the unguard frees nothing")
v.step(prop("q", author=BOT2, executor=BOT2, task="t"), "allow", check={"effects.q.guardedFor": [BOT]}, note="t was created while BOT was guarded")
v.step(claim("cq", "q", author=BOT2), "denied")
v.step(prop("r", author=BOT, executor=BOT), "allow", check={"effects.r.guardedFor": []})
v.step(claim("cr", "r", author=BOT), "allow", note="proposed after the unguard: nobody's guard")
v.step(appr("a", "p", author=OP), "allow").step(claim("c1", "p", author=BOT), "allow")
add(v.done())

v = V("forcing-off-frees-no-recorded-work", ["P-15"], "an effect proposed by, or naming, a forced agent keeps needing its guard after owners switch forceGuarded off")
agents(v, policy(sessions=sess("member", "member", "member", MODES, True)))
v.step(prop("p", author=BOT, executor=BOT), "allow", check={"effects.p.guardedFor": [BOT]})
v.step(prop("d", author=M, executor=BOT2), "allow", check={"effects.d.guardedFor": [BOT2]}, note="a forced executor too")
v.step(prop("h", author=M, executor=M), "allow", check={"effects.h.guardedFor": []}, note="humans are never forced")
v.step(E("ps", "policy.set", O, {"policy": policy(sessions=MEMBERS3)}), "allow", note="forcing off")
v.step(claim("c0", "p", author=BOT), "denied", note="recorded when proposed")
v.step(claim("cd", "d", author=BOT2), "denied")
v.step(prop("q", author=BOT, executor=BOT), "allow", check={"effects.q.guardedFor": []})
v.step(claim("cq", "q", author=BOT), "allow")
v.step(appr("a", "p", author=OP), "allow").step(claim("c1", "p", author=BOT), "allow")
add(v.done())

v = V("operator-withdrawal-keeps-recorded-work-waiting", ["P-15", "P-03", "§4.3"], "R4: an operator never changes, it only confirms and withdraws; while it is withdrawn nothing recorded for its agent can be claimed and it can't approve as a guard, and once it confirms again its earlier approval counts")
agents(v, policy(sessions=MEMBERS3))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(prop("p", author=BOT, executor=BOT), "allow").step(appr("a1", "p", author=OP), "allow")
v.step(E("w", "message", M, {}), "allow", profiles={BOT: dict(AG, operatorConfirmed=False)}, note="OP withdraws")
v.step(claim("c0", "p", author=BOT), "denied", note="no confirmed operator: it can't be claimed for")
v.step(appr("a2", "p", author=OP), "denied", note="OP is no guard while withdrawn, and there is no rule")
v.step(E("w2", "message", M, {}), "allow", profiles={BOT: AG}, note="OP confirms again")
v.step(claim("c1", "p", author=BOT), "allow", check={"effects.p.claims.0.guardApprovals": ["a1"]}, note="the approval OP made before withdrawing counts")
add(v.done())

v = V("delegation-keeps-the-guard", ["P-15", "P-05", "P-01"], "P3: a guarded agent may delegate; every effect for the task or its subtasks carries it in guardedFor and needs its guard, besides the delegates' own guards")
v.prof(BOT, AG).prof(BOT2, AG2).prof(BOT3, AG).prof(X, XS)
team(v, policy(sessions=MEMBERS3))
v.step(ms("op", O, OP, "member"), "allow").step(ms("op2", O, OP2, "member"), "allow")
v.step(ms("b", O, BOT, "member"), "allow").step(ms("b2", O, BOT2, "member"), "allow").step(ms("b3", O, BOT3, "member"), "allow")
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(E("t", "task", BOT, {"to": [BOT2]}), "allow", check={"tasks.t.guardedFor": [BOT]}, note="a guarded agent delegates")
v.step(E("s2", "session.open", BOT2, {"steerMode": "guarded"}), "allow")
v.step(E("t2", "task", BOT2, {"to": [BOT3], "parent": "t"}), "allow", check={"tasks.t2.guardedFor": [BOT, BOT2]}, note="a subtask adds its guarded author")
v.step(prop("p", author=BOT3, task="t2"), "allow", check={"effects.p.guardedFor": [BOT, BOT2]}, note="BOT3 isn't guarded; the work it does for t2 is")
v.actors(claim("c", "p"), [X, BOT3, BOT, BOT2, OP, OP2])
v.step(appr("a1", "p", author=OP), "allow").step(claim("c0", "p"), "denied", note="OP2, BOT2's guard, hasn't approved")
v.step(appr("a2", "p", author=OP2), "allow").step(claim("c1", "p"), "allow", check={"effects.p.claims.0.guardApprovals": ["a1", "a2"]})
v.step(prop("q0", author=BOT3), "allow", check={"effects.q0.guardedFor": []}, note="V4: BOT3 hasn't accepted t2, so its work that names no task carries nothing")
v.step(E("acc", "task.update", BOT3, {"task": "t2", "state": "accepted"}), "allow", check={"tasks.t2.accepted": [BOT3]})
v.step(prop("q", author=BOT3), "allow", check={"effects.q.guardedFor": [BOT, BOT2]}, note="T1, V4: once accepted, work that names no task carries the task's guarding")
v.step(E("tc", "task.cancel", BOT, {"task": "t"}), "allow", note="a guarded agent may cancel what it delegated")
add(v.done())

v = V("guarded-agents-may-not-approve-govern-or-steer-others", ["P-15", "P-16"], "P5: a guarded author may not approve, govern or steer others' sessions (setting policy, unguarding and redacting others' entries need an owner or a guard anyway); it may delegate, propose, report, cancel, use extensions, redact its own entries and work its own sessions")
agents(v, policy(sessions=MEMBERS3, approvals=[{"action": "git.push", "approvers": ["member"]}], membersMayInvite=True))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("s2", "session.open", BOT2, {"steerMode": "auto"}), "allow")
v.step(prop("pm", author=M), "allow").step(E("mm", "message", M, {}), "allow")
v.step(appr("ap", "pm", author=BOT), "denied", note="no approving")
v.step(ms("inv", BOT, Z, "guest"), "denied", note="no governing: membersMayInvite would let a member invite")
v.step(E("ps", "policy.set", BOT, {"policy": policy(sessions=MEMBERS3)}), "denied", note="an owner's kind: never an agent's")
v.step(v.ung("u", BOT, BOT), "denied", note="a guard's kind: never an agent's")
v.step(E("rd", "redact", BOT, {"target": "mm"}), "denied", note="redacting others' entries is an owner's")
v.step(E("relay", "session.input", BOT, {"session": "s2"}), "denied", note="no steering another's session")
v.step(E("ghost", "session.input", BOT, {"session": ABSENT_SESSION}), "invalid", note="a session that isn't there is invalid, not denied")
v.step(E("own", "session.input", BOT, {"session": "s"}), "allow")
v.step(E("msg", "message", BOT, {"to": [BOT2]}), "allow").step(E("rd2", "redact", BOT, {"target": "msg"}), "allow", note="its own entry")
v.step(E("t", "task", BOT, {"to": [BOT2]}), "allow", note="delegating is allowed (P3)")
v.step(prop("q", author=BOT), "allow").step(E("x", "doc.edit", BOT, {}), "allow", note="so are proposals and extensions")
v.step(E("tc", "task.cancel", BOT, {"task": "t"}), "allow").step(E("qc", "effect.cancel", BOT, {"proposal": "q"}), "allow")
v.step(ms("inv2", M, Z, "guest"), "allow", note="the same invite by a member who isn't guarded")
add(v.done())

v = V("outside-instructions-are-inert", ["P-15", "R-24"], "P7: a steer, a task or an executor slot naming a guarded agent is allowed whoever sends it, even with its guard absent or unconfirmed; nothing it causes can be claimed without the guard")
agents(v, policy(sessions=MEMBERS3))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(ms("gone", O, OP, "none"), "allow", check={"guarded": {BOT: True}})
v.step(E("in", "session.input", M, {"session": "s"}), "allow", note="no denial, no hold")
v.step(E("t", "task", M, {"to": [BOT]}), "allow", check={"tasks.t.guardedFor": []})
v.step(prop("d", author=M, executor=BOT), "allow", check={"effects.d.guardedFor": [BOT]})
v.step(prop("p", author=BOT, executor=BOT, task="t"), "allow", check={"effects.p.guardedFor": [BOT]})
v.step(claim("c0", "d", author=BOT), "denied", note="its guard holds no role")
v.step(E("w", "message", M, {}), "allow", profiles={BOT: dict(AG, operatorConfirmed=False)}, note="OP withdraws")
v.step(ms("back", O, OP, "member"), "allow")
v.step(claim("c1", "p", author=BOT), "denied", note="no confirmed operator: it can't be claimed for")
v.step(appr("a1", "p", author=OP), "denied", note="OP is no guard while unconfirmed, and there is no rule")
v.step(E("w2", "message", M, {}), "allow", profiles={BOT: AG}, note="OP confirms again")
v.step(appr("a2", "p", author=OP), "allow").step(claim("c2", "p", author=BOT), "allow")
add(v.done())

v = V("guarded-at-the-claim-counts-too", ["P-15", "P-07 E3"], "an effect proposed before its proposer or executor became guarded needs the guard at the claim; guardedFor records only what was true at creation")
agents(v, policy(sessions=MEMBERS3))
v.step(prop("p", author=BOT, executor=BOT), "allow", check={"effects.p.guardedFor": []})
v.step(prop("d", author=M, executor=BOT2), "allow", check={"effects.d.guardedFor": []})
v.step(prop("x", author=BOT), "allow", check={"effects.x.guardedFor": []}, note="executor X is not guarded")
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(E("s2", "session.open", BOT2, {"steerMode": "guarded"}), "allow", check={"effects.p.guardedFor": [], "effects.d.guardedFor": []})
v.step(claim("c0", "p", author=BOT), "denied", note="its proposer is guarded at the claim")
v.step(claim("cd", "d", author=BOT2), "denied", note="so is the executor")
v.step(appr("a", "p", author=OP), "allow").step(claim("c1", "p", author=BOT), "allow")
v.step(appr("ad", "d", author=OP), "allow").step(claim("cd2", "d", author=BOT2), "allow")
v.step(claim("cx0", "x"), "denied", note="only its proposer is guarded at the claim").step(appr("ax", "x", author=OP), "allow").step(claim("cx", "x"), "allow")
add(v.done())

v = V("forced-agents-without-an-operator-are-inert", ["P-15", "P-16"], "in a forced space an agent or service with no confirmed operator holding a role may be instructed and named, but nothing recorded for it can be claimed, even after forcing is switched off")
v.prof(BOT, AG).prof(BOT2, NOOP).prof(X, dict(XS, operatorConfirmed=False)).prof(BOT3, dict(AG, operator="@gone:b.example")).prof("@gone:b.example", human())     # a reachable operator that holds no role here
team(v, policy(sessions=sess("member", "member", "member", MODES, True)))
v.step(ms("op", O, OP, "member"), "allow").step(ms("b", O, BOT, "member"), "allow").step(ms("b2", O, BOT2, "member"), "allow").step(ms("b3", O, BOT3, "member"), "allow")
v.step(E("t1", "task", M, {"to": [BOT2, BOT3]}), "allow", note="instructing them is allowed")
v.step(prop("d1", author=M, executor=BOT2), "allow", check={"effects.d1.guardedFor": [BOT2]})
v.step(prop("d2", author=M, executor=X), "allow", check={"effects.d2.guardedFor": [X]}, note="H6: a service as executor is not exempt")
v.step(prop("d3", author=M, executor=BOT3), "allow", check={"effects.d3.guardedFor": [BOT3]})
v.step(claim("c1", "d1", author=BOT2), "denied", note="operator not confirmed")
v.step(claim("c2", "d2"), "denied", note="nor the service's")
v.step(claim("c3", "d3", author=BOT3), "denied", note="its confirmed operator holds no role here")
v.step(E("ps", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member"))}), "allow", note="forcing off")
v.step(claim("c4", "d1", author=BOT2), "denied", note="recorded: switching forcing off frees nothing")
v.step(prop("d4", author=M, executor=BOT2), "allow", check={"effects.d4.guardedFor": []}).step(claim("c5", "d4", author=BOT2), "allow")
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

# ── round 24 ──
v = V("attempt-limit", ["P-07 E5"], "H2: at most 64 claims per effect: the 65th is denied, whatever the earlier outcomes were")
team(v)
v.step(prop("p"), "allow")
for k in range(1, 65):
    v.step(claim(f"c{k}", "p", attempt=k), "allow").step(outc(f"o{k}", f"c{k}", "failed", "no"), "allow")
v.step(claim("c65", "p", attempt=65), "denied", check={"effects.p.phase": "open"}, note="attempt limit: propose again")
v.step(prop("q"), "allow", note="a new proposal has a fresh count")
add(v.done())

v = V("differential-fuzz-pins", ["P-04", "P-07", "P-09", "P-15"], "three readings ref and the TS clean-room once differed on (ref/fuzz_diff.py): a cancel before any claim sets cancelRequested too; a claim with no reservation is settled by a not-started outcome; a session may not name a closed task")
team(v)
v.step(prop("p1"), "allow").step(E("x1", "effect.cancel", P, {"proposal": "p1"}), "allow", check={"effects.p1.phase": "cancelled", "effects.p1.cancelRequested": True})
v.step(prop("p2"), "allow").step(claim("c2", "p2"), "allow").step(outc("o2", "c2", "failed", "no"), "allow", check={"effects.p2.claims.0.settled": True})
v.step(E("t", "task", M, {"to": [P]}), "allow").step(E("tc", "task.cancel", M, {"task": "t"}), "allow").step(E("tu", "task.update", P, {"task": "t", "state": "cancelled"}), "allow", check={"tasks.t.open": False})
v.step(E("s", "session.open", P, {"steerMode": "auto", "task": "t"}), "denied", note="task closed")
add(v.done())

v = V("cancel-needs-a-standing-approval", ["P-04", "P-08"], "H4: effect.cancel is open to the author of an approval still on the effect; once the approval is gone (its claim settled), so is the right")
team(v, policy(approvals=[{"action": "*", "approvers": [M]}]))
v.step(prop("pr", repeatable=True), "allow").step(appr("a1", "pr", author=M), "allow").step(claim("c", "pr"), "allow")
v.step(E("x0", "effect.cancel", M, {"proposal": "pr"}), "allow", check={"effects.pr.cancelRequested": True}, note="its approval is consumed but still on the effect")
v.step(outc("o", "c", "failed", "yes"), "allow", check={"effects.pr.approvals": [], "effects.pr.phase": "failed"})
v.step(prop("q", repeatable=True), "allow").step(appr("a2", "q", author=M), "allow").step(claim("c2", "q"), "allow")
v.step(outc("o2", "c2", "failed", "yes"), "allow", check={"effects.q.approvals": [], "effects.q.phase": "open"}, note="the settled claim took the consumed approval with it")
v.step(E("x1", "effect.cancel", M, {"proposal": "q"}), "denied", note="M's approval is no longer on the effect")
v.step(E("x2", "effect.cancel", P, {"proposal": "q"}), "allow", note="the proposer still may")
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
v.step(prop("pr", author=P), "allow", check={"effects.pr.phase": "open"})
v.step(appr("a0", "pr", author=O), "denied", note="owner is not member")
v.step(appr("a1", "pr", author=M), "allow", check={"effects.pr.phase": "open"})
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

v = V("relevant-actors-order", ["P-01"], "author, target, proposer and executor, then the approval candidates in order (every one while none is known to be valid), each actor once")
team(v, policy(approvals=[{"action": "*", "approvers": ["owner", "member"]}]))
v.step(prop("pr"), "allow").step(appr("a1", "pr", author=P), "allow").step(appr("a2", "pr"), "allow")
v.step(appr("a3", "pr", author=M), "allow").step(appr("a4", "pr"), "allow", note="supersedes a2")
v.actors(claim("c", "pr"), [X, P, M, O], pinned=[])
v.actors(claim("c", "pr"), [X, P])                       # P's own approval is the first candidate, and its key is valid
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

v = V("approve-phases-and-resolve", ["P-08", "P-10"], "approvals are accepted while open or claimed; resolve failed removes every approval, reopens the effect and leaves none in hand")
team(v, policy(approvals=[{"action": "*", "approvers": ["owner"]}]))
v.step(prop("pr"), "allow").step(appr("a1", "pr"), "allow", check={"effects.pr.phase": "open"})
v.step(claim("c", "pr"), "allow").step(appr("a2", "pr"), "allow", check={"effects.pr.phase": "claimed"}, note="adds an approval while claimed")
v.step(outc("o", "c", "unknown", "yes"), "allow", check={"effects.pr.phase": "unknown"})
v.step(appr("a3", "pr"), "denied", note="unknown")
v.step(E("r", "effect.resolve", O, {"proposal": "pr", "result": "failed"}), "allow",
       check={"effects.pr.phase": "open", "effects.pr.approvals": []}, note="a2 removed, a1 consumed by the now settled claim: nothing in hand")
v.step(appr("a4", "pr"), "allow", check={"effects.pr.phase": "open"})
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
v.step(outc("o1", "c1", "failed", "yes"), "allow", check={"effects.pr.approvals": [A("a2", M)], "effects.pr.phase": "open"}, note="a1 consumed by a settled claim: gone")
v.step(claim("c2", "pr", attempt=2), "allow", check={"effects.pr.approvals": [AC("a2", M, "c2")]})
v.step(outc("o2", "c2", "unknown", "yes"), "allow", check={"effects.pr.approvals": [], "effects.pr.phase": "open"}, note="a repeatable unknown is settled and retried")
v.step(E("x", "effect.cancel", M, {"proposal": "pr"}), "denied", note="M's approval is gone: no longer an approver on record")
v.step(E("x2", "effect.cancel", O, {"proposal": "pr"}), "allow", note="an owner always may")
v.step(prop("pu", repeatable=False), "allow").step(appr("a3", "pu", author=O), "allow").step(claim("c3", "pu"), "allow")
v.step(outc("o3", "c3", "unknown", "yes"), "allow", check={"effects.pu.phase": "unknown", "effects.pu.approvals": [AC("a3", O, "c3")]}, note="unsettled: the approval stays")
v.step(E("r", "effect.resolve", O, {"proposal": "pu", "result": "succeeded"}), "allow", check={"effects.pu.approvals": []})
v.step(prop("pn"), "allow").step(appr("a4", "pn", author=O), "allow").step(claim("c4", "pn"), "allow")
v.step(outc("o4", "c4", "failed", "no"), "allow", check={"effects.pn.approvals": [A("a4", O)], "effects.pn.phase": "open"}, note="not started: the approval is returned, not pruned")
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
v.step(E("c3", "session.close", BOT, {"session": "s2"}), "allow", check={"guarded": {BOT: True}})
v.step(E("in3", "session.input", BOT, {"session": "s2"}), "invalid", note="a guarded agent steering its closed session: nothing to steer, not a denial")
v.step(E("in4", "session.input", BOT, {"session": "s"}), "invalid", note="nor another one that is gone")
v.step(E("s3", "session.open", M, {"steerMode": "auto"}), "allow")
add(v.done())

v = V("agent-unguard-shape", ["P-15", "R-09", "§5"], "agent.unguard is {agent}; the prefix agent is reserved, and the retired agent.release is an unknown kind under it")
agents(v, policy(sessions=sess("member", "member", "member")))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow", check={"guarded": {BOT: True}})
v.step(E("b0", "agent.unguard", OP, {}), "invalid", note="agent is required")
v.step(E("b1", "agent.unguard", OP, {"agent": BOT, "unguard": True}), "invalid", note="no other field")
v.step(E("b2", "agent.unguard", OP, {"agent": BOT, "session": "s"}), "invalid", note="no session field")
v.step(E("b3", "agent.unguard", OP, {"agent": "not-an-actor"}), "invalid")
v.step(E("b4", "agent.note", OP, {"agent": BOT}), "invalid", note="agent.* is reserved: an unknown kind under a core prefix")
v.step(E("b5", "agent.release", OP, {"agent": BOT}), "invalid", note="the round-26 kind is gone")
v.step(v.ung("h", BOT, BOT), "denied", note="a guarded agent may not unguard")
v.step(v.ung("n", OP, BOT2), "denied", note="BOT2 isn't guarded")
v.step(v.ung("m", OP, M), "denied", note="nor is a human")
v.step(v.ung("ok", OP, BOT), "allow", check={"guarded": {}})
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

v = V("repeatable-rearm-needs-approval", ["P-09", "P-10"], "after a repeatable attempt ends without success the effect is open again, and a claim needs approvals in hand (E3), not a stored phase")
team(v, policy(approvals=[{"action": "*", "approvers": ["owner"]}]))
v.step(prop("p", repeatable=True), "allow").step(appr("a1", "p"), "allow").step(claim("c1", "p"), "allow")
v.step(outc("o1", "c1", "unknown", "unknown"), "allow", check={"effects.p.phase": "open"}, note="a1 is consumed")
v.step(claim("c1x", "p", attempt=2), "denied", note="E3: no approval in hand")
v.step(appr("a2", "p"), "allow", check={"effects.p.phase": "open"}).step(claim("c2", "p", attempt=2), "allow")
v.step(outc("o2", "c2", "failed", "yes"), "allow", check={"effects.p.phase": "open"})
v.step(appr("a3", "p"), "allow").step(claim("c3", "p", attempt=3), "allow")
v.step(outc("o3", "c3", "failed", "no"), "allow", check={"effects.p.phase": "open"}, note="not started: a3 returned")
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

# ── round 26 ──



v = V("approval-cap-counts-open-approvals", ["P-08"], "M6 (reviewer's s3): the cap counts unexpired unconsumed approvals by other authors; 16 that have expired leave room for a new one, and the touch drops them")
CROWD = [f"@c{i}:a.example" for i in range(17)]
team(v, policy(approvals=[{"action": "*", "approvers": ["member"]}]))
for i, a_ in enumerate(CROWD): v.step(ms(f"cm{i}", O, a_, "member"), "allow")
v.step(prop("p"), "allow")
until = (BASE + timedelta(seconds=v.t + 20)).strftime("%Y-%m-%dT%H:%M:%SZ")
for i, a_ in enumerate(CROWD[:16]): v.step(appr(f"a{i}", "p", author=a_, expires=until), "allow")
v.step(appr("a16", "p", author=CROWD[16]), "denied", note="16 open approvals by others: the cap")
v.step(appr("a17", "p", author=CROWD[16]), "allow", dt=10, check={"effects.p.approvals": [{"id": "a17", "author": CROWD[16], "keyId": "k1"}]}, note="all 16 expired: they don't count, and this touch dropped them")
add(v.done())

v = V("relevant-actors-grow-with-the-pins", ["P-01", "P-15"], "M3: relevantActors(state, entry, ctx) is the sequencer's whole pin rule; with nothing pinned it names every candidate, with the profiles pinned it adds the guards and stops after the first candidate whose key is valid")
agents(v, policy(sessions=MEMBERS3, approvals=[{"action": "*", "approvers": ["member"]}]))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(prop("d", author=M, executor=BOT), "allow", check={"effects.d.guardedFor": [BOT]})
v.step(appr("a1", "d", author=P), "allow").step(appr("a2", "d", author=X), "allow").step(appr("a3", "d", author=OP), "allow")
v.actors(claim("c", "d", author=BOT), [BOT, M, P, X, OP], pinned=[])
v.actors(claim("c", "d", author=BOT), [BOT, M, OP, P, X], pinned=[BOT, M])
v.actors(claim("c", "d", author=BOT), [BOT, M, OP, P])
v.step(E("msg", "message", M, {}), "allow", profiles={P: human({"k1": {"publicKey": "pk", "status": "revoked"}})}, note="P's key is revoked")
v.actors(claim("c", "d", author=BOT), [BOT, M, OP, P, X])
v.step(claim("c", "d", author=BOT), "allow", check={"effects.d.claims.0.approval": "a2", "effects.d.claims.0.guardApprovals": ["a3"]}, note="P's approval is skipped, X's is consumed, and the guard's")
add(v.done())

v = V("guard-status-takes-the-agent-view", ["P-15", "R-24"], "M5, P8, Z1: guardStatus reads only policy, guarded and the tasks whose accepted lists the agent (han.ts GuardInput), so a host passes the state {agent} reply as is")
agents(v, policy(sessions=MEMBERS3))
VIEW = {"space": S, "head": 9, "home": "a.example", "policy": policy(sessions=MEMBERS3), "guarded": {BOT: True}, "tasks": {}}
v.guard_view(BOT, VIEW, {"guard": OP, "guarded": True, "forced": False})
v.guard_view(BOT2, dict(VIEW, guarded={}), {"guard": OP, "guarded": False, "forced": False})
v.guard_view(BOT2, dict(VIEW, guarded={}, policy=policy(sessions=sess("member", "member", "member", MODES, True))), {"guard": OP, "guarded": True, "forced": True})
TASKV = {"author": BOT, "to": [BOT2, M], "open": True, "state": "accepted", "cancelRequested": False, "depth": 0, "seq": 8, "accepted": [M, BOT2], "guardedFor": [BOT]}
v.guard_view(BOT2, dict(VIEW, guarded={}, tasks={"ab" * 32: TASKV}), {"guard": OP, "guarded": True, "forced": False, "guards": [BOT]})
v.guard_view(M, dict(VIEW, guarded={}, tasks={"ab" * 32: TASKV}), {"guarded": True, "forced": False, "guards": [BOT]})
add(v.done())

# ── round 28 (DECISIONS-r28): the edges of recorded guarding ──
v = V("guarded-approvals-do-not-count", ["P-15", "P-07 E3", "P-01", "P-08"], "R2 (Q2), V5: an approval by an author who becomes guarded, by its choice or by forcing, never counts: the fold drops it when the author chooses guarding or a policy.set turns forcing on (judged from the actorKind the approval recorded), so a claim never meets, skips or pins it")
agents(v, policy(sessions=MEMBERS3, approvals=[{"action": "pay", "approvers": ["member"]}]))
v.step(prop("p", author=P, action="pay"), "allow").step(appr("ab", "p", author=BOT), "allow", note="BOT isn't guarded yet: a rule approver like any member")
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow", check={"effects.p.approvals": []}, note="T2: choosing guarding drops BOT's open approvals")
v.step(claim("c0", "p"), "denied", note="BOT is guarded at the claim: its approval doesn't count")
v.step(appr("am", "p", author=M), "allow")
v.actors(claim("c1", "p"), [X, P, M])
v.step(claim("c1", "p"), "allow", check={"effects.p.claims.0.approval": "am"})
v.step(prop("q", author=P, action="pay", executor=M), "allow").step(appr("aq", "q", author=BOT2), "allow")
v.step(prop("r", author=P, action="pay", executor=M), "allow").step(appr("ar", "r", author=BOT2), "allow").step(appr("ar2", "r", author=OP), "allow")
v.step(E("ps", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member", MODES, True), approvals=[{"action": "pay", "approvers": ["member"]}])}), "allow",
       check={"effects.q.approvals": [], "effects.r.approvals": [{"id": "ar2", "author": OP, "keyId": "k1"}]}, note="V5: forcing on drops the open approvals of the agents it guards, judged from the actorKind each approval recorded")
v.step(claim("cq0", "q", author=M), "denied", note="BOT2's approval is gone")
v.actors(claim("cr", "r", author=M), [M, P, OP])
v.step(claim("cr", "r", author=M), "allow", check={"effects.r.claims.0.approval": "ar2"}, note="no forced candidate is left to skip or pin")
v.step(appr("aq2", "q", author=P), "allow", check={"effects.q.approvals": [{"id": "aq2", "author": P, "keyId": "k1"}]})
v.step(claim("cq", "q", author=M), "allow", check={"effects.q.claims.0.approval": "aq2"})
add(v.done())

v = V("guards-are-never-crowded-out", ["P-08", "P-15"], "R3 (Q3): the cap counts only approvals that aren't a guard's own: a guard's approval neither counts toward MAX_OPEN_APPROVALS nor is refused by it, and the guard holds one open approval")
CROWD28 = [f"@k{i}:a.example" for i in range(17)]
agents(v, policy(sessions=MEMBERS3, approvals=[{"action": "pay", "approvers": ["member"]}]))
for i, a_ in enumerate(CROWD28): v.step(ms(f"km{i}", O, a_, "member"), "allow")
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(prop("p", author=BOT, action="pay"), "allow", check={"effects.p.guardedFor": [BOT]})
for i, a_ in enumerate(CROWD28[:16]): v.step(appr(f"a{i}", "p", author=a_), "allow")
v.step(appr("g1", "p", author=OP), "allow", note="Q3: 16 open approvals by members don't block the guard")
v.step(appr("a16", "p", author=CROWD28[16]), "denied", note="the cap: 16 open approvals by other non-guards")
v.step(appr("g2", "p", author=OP), "allow", check={"effects.p.approvals.16": {"id": "g2", "author": OP, "keyId": "k1"}}, note="the guard refreshes its one open approval")
v.step(claim("c", "p"), "allow", check={"effects.p.claims.0.approval": "a0", "effects.p.claims.0.guardApprovals": ["g2"]})
v.step(prop("q", author=BOT, action="pay"), "allow")
for i, a_ in enumerate(CROWD28[:15]): v.step(appr(f"b{i}", "q", author=a_), "allow")
v.step(appr("gq", "q", author=OP), "allow")
v.step(appr("b15", "q", author=CROWD28[15]), "allow", note="the guard's approval doesn't count toward the cap: 15 others")
v.step(appr("b16", "q", author=CROWD28[16]), "denied")
add(v.done())

v = V("a-full-guard-chain-is-claimable", ["P-15", "P-08", "P-05"], "R3 (Q3): in a forced space a task chain of depth 16 and an effect make 19 agents in guardedFor with 19 guards; all 19 may approve, and then it can be claimed")
CHAIN = [f"@g{i}:b.example" for i in range(19)]
GUARDS = [f"@h{i}:b.example" for i in range(19)]
for a_, h_ in zip(CHAIN, GUARDS): v.prof(a_, dict(AG, operator=h_))
v.step(create(policy(maxDelegationDepth=16, sessions=sess("member", "member", "member", MODES, True))), "allow")
for i, a_ in enumerate(CHAIN + GUARDS): v.step(ms(f"cm{i}", O, a_, "member"), "allow")
v.step(E("t0", "task", CHAIN[0], {"to": [CHAIN[1]]}), "allow", check={"tasks.t0.guardedFor": CHAIN[:1]})
for i in range(1, 17): v.step(E(f"t{i}", "task", CHAIN[i], {"to": [CHAIN[i + 1]], "parent": f"t{i - 1}"}), "allow")
v.step(prop("p", author=CHAIN[17], executor=CHAIN[18], task="t16"), "allow", check={"tasks.t16.depth": 16, "effects.p.guardedFor": CHAIN})
for i, h_ in enumerate(GUARDS[:18]): v.step(appr(f"a{i}", "p", author=h_), "allow")
v.step(claim("c0", "p", author=CHAIN[18]), "denied", note="the executor's guard hasn't approved")
v.step(appr("a18", "p", author=GUARDS[18]), "allow", note="the 19th guard: no cap stops it")
v.step(claim("c", "p", author=CHAIN[18]), "allow", check={"effects.p.claims.0.guardApprovals": [f"a{i}" for i in range(19)]})
add(v.done())

v = V("guarded-agents-may-leave", ["P-15", "P-03"], "R5 (Q5): a guarded author may set its own role to none (leaving is not governing); every other member.set stays denied, its own included, and joining again is the owners' to grant")
agents(v, policy(join="open", sessions=sess("member", "member", "member", MODES, True)))
v.step(ms("g", BOT, BOT, "guest"), "denied", note="changing its own role is governing")
v.step(ms("l", BOT, BOT, "none"), "allow", check={"roles": {O: "owner", M: "member", P: "member", X: "member", OP: "member", BOT2: "member"}}, note="a forced agent leaves")
v.step(ms("l2", BOT2, BOT, "none"), "denied", note="removing another is still governing")
v.step(ms("x", X, X, "none"), "allow", note="so does a forced service")
v.step(ms("j", BOT, BOT, "member"), "denied", note="an open join is not leaving: an owner re-admits it")
v.step(ms("j2", O, BOT, "member"), "allow")
add(v.done())

v = V("assignees-carry-their-tasks-guarding", ["P-15", "R-24", "§7.5"], "T1 (S1), V1, V4, X1, X4: an effect that names no task holds carried(proposer), the guardedFor of every task the proposer accepted and hasn't closed for itself: an agent's later, task-less proposal (the reviewer's S1) and a human delegate's work both wait for the delegator's guard once they accept; an unaccepted assignment, or a progress report, burdens nothing; the accepting delegate's parentless subtask carries the delegator too; a co-assignee closing the task ends nothing for the others; work proposed after an assignee's own final update is free, work proposed before stays recorded")
agents(v, policy(sessions=MEMBERS3))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(E("t", "task", BOT, {"to": [BOT2, M]}), "allow", check={"tasks.t.guardedFor": [BOT], "tasks.t.accepted": []})
v.step(prop("p", author=BOT2, task="t"), "allow", check={"effects.p.guardedFor": [BOT]}, note="naming the task carries its guarding, accepted or not")
v.step(prop("n0", author=BOT2), "allow", check={"effects.n0.guardedFor": []}, note="V4: BOT2 hasn't accepted t: an assignment alone burdens nothing")
v.step(E("ab", "task.update", BOT2, {"task": "t", "state": "accepted"}), "allow", check={"tasks.t.accepted": [BOT2]})
v.step(prop("n", author=BOT2), "allow", check={"effects.n.guardedFor": [BOT]}, note="S1: a later proposal by the accepting assignee that names no task")
v.step(claim("cn0", "n"), "denied", note="E3: BOT's guard hasn't approved")
v.step(appr("an", "n", author=OP), "allow").step(claim("cn", "n"), "allow", check={"effects.n.claims.0.guardApprovals": ["an"]})
v.step(prop("hn0", author=M), "allow", check={"effects.hn0.guardedFor": []}, note="the human assignee hasn't accepted")
v.step(E("am0", "task.update", M, {"task": "t", "state": "progress"}), "allow", check={"tasks.t.accepted": [BOT2]}, note="X4 (W10): a progress report doesn't accept")
v.step(E("am", "task.update", M, {"task": "t", "state": "accepted"}), "allow", check={"tasks.t.accepted": [BOT2, M]}, note="only an explicit accept does")
v.step(prop("hn", author=M), "allow", check={"effects.hn.guardedFor": [BOT]}, note="a human assignee carries it too, once it accepted")
v.step(claim("chn0", "hn"), "denied")
v.step(E("tc", "task.cancel", BOT, {"task": "t"}), "allow")
v.step(prop("hc", author=M), "allow", check={"effects.hc.guardedFor": [BOT]}, note="a cancel request doesn't close the task")
v.step(E("t2", "task", BOT2, {"to": [X]}), "allow", check={"tasks.t2.guardedFor": [BOT]}, note="V1 (U1): the accepting delegate's subtask without a parent carries carried(BOT2)")
v.step(prop("x", author=X, executor=X, task="t2"), "allow", check={"effects.x.guardedFor": [BOT]})
v.step(E("done", "task.update", M, {"task": "t", "state": "done"}), "allow", check={"tasks.t.open": False, "tasks.t.accepted": [BOT2]}, note="X1: M's final update ends what M carries, and only that")
v.step(prop("h2", author=M), "allow", check={"effects.h2.guardedFor": []}, note="M closed the task for itself: its later work is free")
v.step(claim("ch2", "h2"), "allow")
v.step(prop("n2", author=BOT2), "allow", check={"effects.n2.guardedFor": [BOT]}, note="X1 (W1): a co-assignee closing the task ends nothing for BOT2")
v.step(E("bd", "task.update", BOT2, {"task": "t", "state": "done"}), "allow", check={"tasks.t.accepted": [], "tasks.t.open": False}, note="X1: BOT2's own final update on the closed task ends its part; nothing else changes")
v.step(prop("n3", author=BOT2), "allow", check={"effects.n3.guardedFor": []})
v.step(claim("chn1", "hn"), "denied", note="what was recorded before the close never shrinks")
add(v.done())

v = V("delegates-carry-the-limits", ["P-15", "P-16", "P-05"], "V1 (U1, the reviewer's three paths): while carried(X), the guardedFor of the tasks X accepted, is not empty, X may not approve, govern or steer others, like a guarded author; a task it creates without a parent carries carried(X); an owner that accepts carries them too; the limits end with each assignee's own final update (X1), and an unaccepted assignment imposes none")
v.prof(BOT3, AG)
agents(v, policy(sessions=MEMBERS3, approvals=[{"action": "*", "approvers": ["member", "owner"]}], membersMayInvite=True))
v.step(ms("b3", O, BOT3, "member"), "allow").step(ms("h", O, G, "member"), "allow")
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow").step(E("s3", "session.open", BOT3, {"steerMode": "auto"}), "allow")
v.step(E("t", "task", BOT, {"to": [BOT2, O]}), "allow", check={"tasks.t.guardedFor": [BOT]})
v.step(prop("hp", author=G), "allow")
v.step(appr("a0", "hp", author=BOT2), "allow", note="V4: BOT2 hasn't accepted t: no limits")
v.step(E("acc", "task.update", BOT2, {"task": "t", "state": "accepted"}), "allow")
v.step(appr("a1", "hp", author=BOT2), "denied", note="U1, approve: BOT2 carries BOT's limits")
v.step(E("i3", "session.input", BOT2, {"session": "s3"}), "denied", note="U1, steer: BOT2 may not steer others")
v.step(E("i4", "session.input", BOT, {"session": "s3"}), "denied", note="as BOT itself may not")
v.step(ms("inv", BOT2, Z, "guest"), "denied", note="no governing: membersMayInvite would let a member invite")
v.step(E("t3", "task", BOT2, {"to": [BOT3]}), "allow", check={"tasks.t3.guardedFor": [BOT], "tasks.t3.accepted": []}, note="U1, the parentless subtask: it carries carried(BOT2)")
v.step(prop("p3", author=BOT3, task="t3", executor=X), "allow", check={"effects.p3.guardedFor": [BOT]})
v.step(E("ts", "task", BOT2, {"to": [BOT2, BOT3], "parent": "t"}), "allow", check={"tasks.ts.accepted": [BOT2], "tasks.ts.guardedFor": [BOT]}, note="V4: an author assigning itself has accepted (an agent claiming an issue); its co-assignee has not")
v.step(appr("g3", "p3", author=G), "allow").step(claim("c30", "p3"), "denied", note="BOT's guard must approve the subtask's work")
v.step(appr("ao0", "hp", author=O), "allow", note="V4 (U4): an unsolicited assignment to an owner burdens nothing")
v.step(E("oacc", "task.update", O, {"task": "t", "state": "accepted"}), "allow", note="an owner accepts BOT's task")
v.step(E("ops", "policy.set", O, {"policy": policy(sessions=MEMBERS3, approvals=[])}), "denied", note="a carrying owner may not govern")
v.step(appr("ao", "hp", author=O), "denied", note="nor approve")
v.step(appr("op3", "p3", author=OP), "allow", note="BOT's guard approves; it carries nothing").step(claim("c3", "p3"), "allow")
v.step(E("done", "task.update", BOT2, {"task": "t", "state": "done"}), "allow")
v.step(E("dts", "task.update", BOT2, {"task": "ts", "state": "done"}), "allow")
v.step(appr("a2", "hp", author=BOT2), "allow", note="BOT2 closed t and ts itself: carried(BOT2) is empty")
v.step(E("i5", "session.input", BOT2, {"session": "s3"}), "allow")
v.step(E("ops1", "policy.set", O, {"policy": policy(sessions=MEMBERS3, approvals=[], membersMayInvite=True)}), "denied", note="X1: BOT2 closing t ends nothing for O, which accepted it too")
v.step(E("od", "task.update", O, {"task": "t", "state": "done"}), "allow", note="O, an assignee, closes its own part of the closed task")
v.step(E("ops2", "policy.set", O, {"policy": policy(sessions=MEMBERS3, approvals=[], membersMayInvite=True)}), "allow")
v.step(prop("p4", author=BOT3, task="t3"), "allow", check={"effects.p4.guardedFor": [BOT]}, note="what t3 recorded never shrinks")
add(v.done())



v = V("forced-service-executors-need-their-operator", ["P-15", "§11"], "R7 (Q4): in a forced space an effect's service executor is guarded too, even on an owner's own effect: its operator must hold a role there and approve")
agents(v, policy(sessions=sess("member", "member", "member", MODES, True)))
v.step(prop("p", author=O), "allow", check={"effects.p.guardedFor": [X]})
v.step(claim("c0", "p"), "denied", note="no service exemption: actorKind is self-declared")
v.step(appr("a", "p", author=OP), "allow").step(claim("c1", "p"), "allow", check={"effects.p.claims.0.guardApprovals": ["a"]})
add(v.done())

# ── round 29 (DECISIONS-r29): the host half of delegation becomes a Policy fact; the cap counts only what can still count ──
v = V("guarded-tasks-are-bounded", ["P-15", "P-16"], "T1, V2, X6: MAX_GUARDED_FOR (19) is a hard bound on every proposal, counting its proposer and executor whether guarded or not: a proposal whose guardedFor (carried(proposer) and carried(executor) included) with its proposer and executor would hold more than 19 agents is denied; a task's guardedFor may hold at most 17, so a proposer and an executor always fit; closing a task makes room")
FORCED29 = [f"@f{i}:b.example" for i in range(20)]
for a_ in FORCED29: v.prof(a_, AG)
v.prof(X, XS)
v.step(create(policy(sessions=sess("member", "member", "member", MODES, True))), "allow")
v.step(ms("m1", O, M, "member"), "allow").step(ms("op", O, OP, "member"), "allow").step(ms("x", O, X, "member"), "allow")
for i, a_ in enumerate(FORCED29): v.step(ms(f"fm{i}", O, a_, "member"), "allow")
for i, a_ in enumerate(FORCED29): v.step(E(f"t{i}", "task", a_, {"to": [M]}), "allow", check={f"tasks.t{i}.guardedFor": [a_]})
v.step(prop("p0", author=M, executor=X), "allow", check={"effects.p0.guardedFor": [X]}, note="V4: 20 tasks assigned, none accepted: they burden nothing")
for i in range(17): v.step(E(f"a{i}", "task.update", M, {"task": f"t{i}", "state": "accepted"}), "allow")
v.step(prop("p", author=M, executor=X), "allow", check={"effects.p.guardedFor": FORCED29[:17] + [X]}, note="17 carried, then the executor; with the proposer M, 19 agents: at the bound")
v.step(E("a17", "task.update", M, {"task": "t17", "state": "accepted"}), "allow")
v.step(prop("q", author=M, executor=X), "denied", note="V2: 18 carried, M and X make 20, though M is never guarded")
v.step(prop("q1", author=M, executor=M), "allow", check={"effects.q1.guardedFor": FORCED29[:18]}, note="18 carried and M: 19")
v.step(E("a18", "task.update", M, {"task": "t18", "state": "accepted"}), "allow")
v.step(prop("q2", author=M, executor=M), "denied", note="19 carried and M: 20")
v.step(prop("q3", author=M, executor=M, task="t19"), "denied", note="X3: M executes, so carried(M) joins t19's agent")
v.step(E("tm", "task", M, {"to": [OP]}), "denied", note="X6: a parentless task records carried(M), 19 agents; a task may hold 17")
v.step(E("d0", "task.update", M, {"task": "t0", "state": "done"}), "allow").step(E("d1", "task.update", M, {"task": "t1", "state": "done"}), "allow")
v.step(prop("q4", author=M, executor=M, task="t19"), "allow", check={"effects.q4.guardedFor": [FORCED29[19]] + FORCED29[2:19]}, note="the named task's agents first, then carried(executor) in the order the tasks were created (X3)")
v.step(prop("q5", author=M, executor=X, task="t19"), "denied", note="Z1: work named under any task carries guards(proposer) too: t19's agent, 17 carried, X and M make 20")
v.step(E("tm2", "task", M, {"to": [OP]}), "allow", check={"tasks.tm2.guardedFor": FORCED29[2:19]}, note="V1, X6: a parentless task records carried(author): 17, at the task bound")
add(v.done())

v = V("tasks-carry-an-external-reference", ["R-09", "P-05", "§5"], "V13: a task may name an external reference (an issue URL): printable ASCII without spaces, 1 to 1024 characters; Policy reads nothing else from it")
team(v)
v.step(E("t", "task", O, {"to": [M], "external": "https://github.com/lmvdz/han/issues/30"}), "allow")
v.step(E("t1", "task", O, {"to": [M], "external": "github:lmvdz/han#30"}), "allow")
v.step(E("t2", "task", O, {"to": [M], "external": ""}), "invalid", note="empty")
v.step(E("t3", "task", O, {"to": [M], "external": "an issue"}), "invalid", note="a space")
v.step(E("t4", "task", O, {"to": [M], "external": "x" * 1025}), "invalid", note="too long")
v.step(E("t5", "task", O, {"to": [M], "external": 30}), "invalid", note="not a string")
add(v.done())

v = V("later-guarding-stays-within-the-bound", ["P-15", "P-08"], "V2 (U2, the reviewer's counterexample): the proposer and executor count toward the bound at the proposal whether guarded or not, so when both become guarded later a claim still needs at most 19 guards, and the effect stays within MAX_UNCONSUMED_APPROVALS (36) and MAX_APPROVALS (55)")
BOTS30 = [f"@d{i}:b.example" for i in range(18)]
OPS30 = [f"@e{i}:b.example" for i in range(18)]
HUM30 = [f"@w{i}:a.example" for i in range(16)]
PR30, EX30, OPP30, OPE30 = "@prop:c.example", "@exec:c.example", "@opp:c.example", "@ope:c.example"
for a_, o_ in zip(BOTS30, OPS30): v.prof(a_, dict(AG, operator=o_))
v.prof(PR30, dict(AG, operator=OPP30)).prof(EX30, dict(XS, operator=OPE30))
v.step(create(policy(approvals=[{"action": "*", "approvers": ["member"]}], sessions=MEMBERS3)), "allow")
for i, a_ in enumerate([PR30, EX30, OPP30, OPE30] + OPS30 + BOTS30 + HUM30): v.step(ms(f"mm{i}", O, a_, "member"), "allow")
for i, a_ in enumerate(BOTS30):
    v.step(E(f"s{i}", "session.open", a_, {"steerMode": "guarded"}), "allow").step(E(f"t{i}", "task", a_, {"to": [PR30]}), "allow")
    v.step(E(f"a{i}", "task.update", PR30, {"task": f"t{i}", "state": "accepted"}), "allow")
v.step(prop("q", author=PR30, executor=EX30), "denied", note="18 carried, the unguarded proposer and executor make 20")
v.step(E("d17", "task.update", PR30, {"task": "t17", "state": "done"}), "allow")
v.step(prop("p", author=PR30, executor=EX30), "allow", check={"effects.p.guardedFor": BOTS30[:17]}, note="17 + 2 = 19")
v.step(E("sp", "session.open", PR30, {"steerMode": "guarded"}), "allow").step(E("se", "session.open", EX30, {"steerMode": "guarded"}), "allow", note="both become guarded after the proposal")
for i, o_ in enumerate(OPS30[:17] + [OPP30]): v.step(appr(f"g{i}", "p", author=o_), "allow")
v.step(claim("c0", "p", author=EX30), "denied", note="the executor's guard hasn't approved: 19 guards")
v.step(appr("g18", "p", author=OPE30), "allow")
for i, h_ in enumerate(HUM30): v.step(appr(f"h{i}", "p", author=h_), "allow")
v.step(claim("c", "p", author=EX30), "allow", check={"effects.p.claims.0.approval": "g0", "effects.p.claims.0.guardApprovals": [f"g{i}" for i in range(19)]},
       note="19 guard approvals, the rule approval being the first guard's own; 16 open by members, at most 36 unconsumed")
for i, o_ in enumerate(OPS30[:17] + [OPP30, OPE30]): v.step(appr(f"r{i}", "p", author=o_), "allow")
v.step(appr("hx", "p", author=HUM30[0]), "allow", note="a member refreshes its own")
add(v.done())


v = V("cap-counts-only-countable-approvals", ["P-08", "P-01", "P-15"], "T2, V3, V5 (S2, U3, U5): an open approval counts toward MAX_OPEN_APPROVALS only while it is an E3 candidate now (it matches the current rule and separateApprover, its author holds a role) and its author isn't guarded; forcing on drops the agents' open approvals, judged from the actorKind each recorded, so no approver is pinned for an approve and nothing counts again when forcing goes off; an approval's fold drops the open approvals that don't count")
AGENTS29 = [f"@a{i}:b.example" for i in range(16)]
HUM30C = [f"@u{i}:a.example" for i in range(16)]
for a_ in AGENTS29: v.prof(a_, AG)
team(v, policy(sessions=MEMBERS3, approvals=[{"action": "*", "approvers": ["member"]}]))
v.step(ms("g", O, G, "member"), "allow").step(ms("op", O, OP, "member"), "allow").step(ms("z", O, Z, "member"), "allow")
for i, a_ in enumerate(AGENTS29 + HUM30C): v.step(ms(f"am{i}", O, a_, "member"), "allow")
v.step(prop("p", author=P, executor=M), "allow").step(prop("q", author=P, executor=M), "allow")
for i, a_ in enumerate(AGENTS29): v.step(appr(f"a{i}", "p", author=a_), "allow").step(appr(f"b{i}", "q", author=a_), "allow")
v.step(appr("g0", "p", author=G), "denied", note="16 countable approvals by others")
v.step(E("f1", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member", MODES, True), approvals=[{"action": "*", "approvers": ["member"]}])}), "allow",
       check={"effects.p.approvals": [], "effects.q.approvals": []}, note="U5: forcing on drops the 16 agents' open approvals, judged from the actorKind each recorded")
v.actors(appr("g1", "p", author=G), [G, P, M])
v.step(appr("g1", "p", author=G), "allow", check={"effects.p.approvals": [{"id": "g1", "author": G, "keyId": "k1"}]}, note="no approver is pinned for an approve")
v.step(claim("c", "p", author=M), "allow", check={"effects.p.claims.0.approval": "g1"})
v.step(E("f0", "policy.set", O, {"policy": policy(sessions=MEMBERS3, approvals=[{"action": "*", "approvers": ["member"]}])}), "allow", note="forcing off")
v.step(appr("gq", "q", author=G), "allow", note="the dropped approvals never come back")
v.step(prop("r", author=P, executor=M), "allow")
for i, a_ in enumerate(HUM30C): v.step(appr(f"u{i}", "r", author=a_), "allow")
v.step(appr("z0", "r", author=Z), "denied")
v.step(E("ro", "policy.set", O, {"policy": policy(sessions=MEMBERS3, approvals=[{"action": "*", "approvers": ["owner"]}])}), "allow", note="U3: owners change the rule; the 16 member approvals stay in the state")
v.step(appr("or", "r", author=O), "allow", check={"effects.r.approvals": [{"id": "or", "author": O, "keyId": "k1"}]}, note="U3: they are no E3 candidates, so they don't count, and this approval's fold drops them")
v.step(claim("cr", "r", author=M), "allow", check={"effects.r.claims.0.approval": "or"})
v.step(E("rm", "policy.set", O, {"policy": policy(sessions=MEMBERS3, approvals=[{"action": "*", "approvers": ["member"]}])}), "allow")
v.step(prop("s", author=P, executor=M), "allow")
for i, a_ in enumerate(HUM30C): v.step(appr(f"v{i}", "s", author=a_), "allow")
for i, a_ in enumerate(HUM30C): v.step(ms(f"dg{i}", O, a_, "guest"), "allow")
v.step(appr("os", "s", author=OP), "allow", check={"effects.s.approvals": [{"id": "os", "author": OP, "keyId": "k1"}]}, note="U3: 16 approvers demoted to guest are no candidates of the rule member")
v.step(claim("cs", "s", author=M), "allow", check={"effects.s.claims.0.approval": "os"})
add(v.done())


v = V("lost-roles-drop-approvals", ["P-08", "P-03", "P-15"], "T2 (S2, the reviewer's second repro): the fold drops an author's unconsumed approvals when it loses its role (none or banned) or chooses guarding, so they don't block the cap and don't come back; a consumed one stays with its claim and is dropped if a not-started outcome returns it")
HUMANS29 = [f"@h{i}:b.example" for i in range(16)]
v.prof(BOT, AG).prof(BOT2, AG)
team(v, policy(sessions=MEMBERS3, approvals=[{"action": "*", "approvers": ["member"]}]))
v.step(ms("g", O, G, "member"), "allow").step(ms("op", O, OP, "member"), "allow").step(ms("b", O, BOT, "member"), "allow").step(ms("b2", O, BOT2, "member"), "allow")
for i, a_ in enumerate(HUMANS29): v.step(ms(f"hm{i}", O, a_, "member"), "allow")
v.step(prop("p", author=P, executor=M), "allow")
for i, a_ in enumerate(HUMANS29): v.step(appr(f"a{i}", "p", author=a_), "allow")
for i, a_ in enumerate(HUMANS29): v.step(ms(f"x{i}", O, a_, "none" if i < 8 else "banned"), "allow")
v.step(ms("back", O, HUMANS29[0], "member"), "allow", check={"effects.p.approvals": []}, note="removed or banned: their approvals are gone, and re-adding brings none back")
v.step(appr("m", "p", author=G), "allow").step(claim("c", "p", author=M), "allow")
v.step(prop("q", author=P, executor=M), "allow").step(appr("bq", "q", author=BOT), "allow")
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow", check={"effects.q.approvals": []}, note="choosing guarding drops BOT's open approvals")
v.step(appr("b2q", "q", author=BOT2), "allow").step(claim("cq", "q", author=M), "allow")
v.step(ms("x2", O, BOT2, "none"), "allow", check={"effects.q.approvals": [{"id": "b2q", "author": BOT2, "keyId": "k1", "consumedBy": "cq"}]}, note="a consumed approval stays with its claim")
v.step(outc("oq", "cq", "failed", "no", author=M), "allow", check={"effects.q.approvals": [], "effects.q.phase": "open"}, note="returned, its author holds no role: dropped")
add(v.done())

v = V("revoked-keys-still-count-until-superseded", ["P-08", "R-12"], "T2: key revocation isn't visible in the fold, so an approval signed with a since-revoked key still counts toward the cap until it expires or is superseded; its author supersedes it with an approval signed by a valid key")
KEYED29 = [f"@k{i}:b.example" for i in range(16)]
K2 = {"k1": {"publicKey": "pk", "status": "revoked"}, "k2": {"publicKey": "pk2", "status": "valid"}}
team(v, policy(approvals=[{"action": "*", "approvers": ["member"]}]))
v.step(ms("g", O, G, "member"), "allow")
for i, a_ in enumerate(KEYED29): v.step(ms(f"km{i}", O, a_, "member"), "allow")
v.step(prop("p", author=P, executor=M), "allow")
for i, a_ in enumerate(KEYED29): v.step(appr(f"a{i}", "p", author=a_), "allow")
v.step(appr("g0", "p", author=G), "denied", profiles={a_: human(K2) for a_ in KEYED29}, note="every approver's k1 is revoked: their approvals still count")
v.step(claim("c0", "p", author=M), "denied", note="E3: no usable approval")
v.step(E("r0", "effect.approve", KEYED29[0], {"proposal": "p", "planDigest": digest_of("p")}, key="k2"), "allow", note="an approver supersedes its own approval")
v.step(claim("c", "p", author=M), "allow", check={"effects.p.claims.0.approval": "r0"})
add(v.done())

v = V("guarded-approvals-grant-no-cancel", ["P-04", "P-15", "P-08"], "T7 (S7), V5: an approval whose author is guarded at the cancel's seq grants no cancel right (a consumed one stays with its claim); unforced again, the consumed approval grants it again; an open one is dropped when forcing goes on, and its right with it")
agents(v, policy(sessions=MEMBERS3, approvals=[{"action": "*", "approvers": ["member"]}]))
v.step(prop("p", author=P, executor=M), "allow").step(appr("ab", "p", author=BOT), "allow").step(claim("c", "p", author=M), "allow", check={"effects.p.claims.0.approval": "ab"})
v.step(E("f1", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member", MODES, True), approvals=[{"action": "*", "approvers": ["member"]}])}), "allow", note="forcing on: BOT is guarded; its consumed approval stays with the claim")
v.step(E("x0", "effect.cancel", BOT, {"proposal": "p"}), "denied", note="S7: BOT's approval grants no cancel right while BOT is guarded")
v.step(E("f0", "policy.set", O, {"policy": policy(sessions=MEMBERS3, approvals=[{"action": "*", "approvers": ["member"]}])}), "allow")
v.step(E("x1", "effect.cancel", BOT, {"proposal": "p"}), "allow", check={"effects.p.cancelRequested": True}, note="unforced, its approval grants the right again")
v.step(prop("q", author=P, executor=M), "allow").step(appr("bq", "q", author=BOT), "allow")
v.step(E("f2", "policy.set", O, {"policy": policy(sessions=sess("member", "member", "member", MODES, True), approvals=[{"action": "*", "approvers": ["member"]}])}), "allow", check={"effects.q.approvals": []}, note="V5: forcing on drops BOT's open approval")
v.step(E("f3", "policy.set", O, {"policy": policy(sessions=MEMBERS3, approvals=[{"action": "*", "approvers": ["member"]}])}), "allow")
v.step(E("x2", "effect.cancel", BOT, {"proposal": "q"}), "denied", note="its approval is gone")
add(v.done())


v = V("agents-name-an-operator", ["§4.3", "R-09", "P-16"], "T8 (S8): a non-human profile (agent or service) must name an operator at its profile space.create, else invalid; a human's may name none")
v.prof("@:b.example", human())
v.step(E("A0", "space.create", "@:b.example", {"policy": policy(visibility="public"), "profile": {"actorKind": "agent", "key": {"keyId": "k1", "publicKey": "pk"}}}, space="profile:@ghost:b.example"), "invalid", note="an agent with no operator")
v.step(E("S0", "space.create", "@:b.example", {"policy": policy(visibility="public"), "profile": {"actorKind": "service", "key": {"keyId": "k1", "publicKey": "pk"}}}, space="profile:@svc:b.example"), "invalid", note="a service with no operator")
v.step(E("A1", "space.create", "@:b.example", {"policy": policy(visibility="public"), "profile": {"actorKind": "agent", "operator": OP, "key": {"keyId": "k1", "publicKey": "pk"}}}, space="profile:@ghost:b.example"), "allow")
v.step(E("H0", "space.create", "@:b.example", {"policy": policy(visibility="public"), "profile": {"actorKind": "human", "key": {"keyId": "k1", "publicKey": "pk"}}}, space="profile:@hu:b.example"), "allow", note="a human has no operator")
add(v.done())

# ── round 31 (DECISIONS-r31): per-assignee closure, linked carrying, carrying executors, explicit acceptance, the task bound, denied servers ──
D31, E31, H31 = "@d:b.example", "@e:b.example", "@h:a.example"

v = V("closing-ends-only-the-closers-carry", ["P-15", "P-05", "P-16"], "X1, Z2 (the reviewer's p3_selfclose, the co-assignee decline, Q2 and Q5): an assignee's carry ends only with its own final update, when an owner who isn't an assignee makes a final update (a force-close, on a closed task too), or when the assignee leaves the space; the guarded author or another assignee closing or declining ends nothing, and neither does an owner's task.cancel; a closed task takes only final updates, which change only accepted")
v.prof(D31, AG).prof(E31, AG)
agents(v, policy(sessions=MEMBERS3, approvals=[{"action": "*", "approvers": ["member", "owner"]}]))
v.step(ms("d", O, D31, "member"), "allow").step(ms("e", O, E31, "member"), "allow").step(ms("h", O, H31, "member"), "allow")
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(E("t", "task", BOT, {"to": [BOT, D31]}), "allow", check={"tasks.t.accepted": [BOT], "tasks.t.guardedFor": [BOT]}, note="BOT assigns itself too: accepted at creation")
v.step(E("ad", "task.update", D31, {"task": "t", "state": "accepted"}), "allow", check={"tasks.t.accepted": [BOT, D31]})
v.step(E("bc", "task.update", BOT, {"task": "t", "state": "done"}), "allow", check={"tasks.t.open": False, "tasks.t.accepted": [D31]}, note="p3: the guarded delegator closes the task")
v.step(prop("pd", author=D31), "allow", check={"effects.pd.guardedFor": [BOT]}, note="X1: D still carries BOT's guarding")
v.step(appr("ad0", "pd", author=D31), "denied", note="and may not approve")
v.step(appr("ao", "pd", author=O), "allow").step(claim("cd0", "pd"), "denied", note="BOT's guard must approve D's task-less work")
v.step(E("dp", "task.update", D31, {"task": "t", "state": "progress"}), "denied", note="a closed task takes no update but a final one")
v.step(E("dd", "task.update", D31, {"task": "t", "state": "done"}), "allow", check={"tasks.t.accepted": [], "tasks.t.state": "done"}, note="D's own final update on the closed task")
v.step(E("dd2", "task.update", D31, {"task": "t", "state": "done"}), "allow", check={"tasks.t.accepted": [], "tasks.t.state": "done"}, note="Z2: another final update changes nothing")
v.step(prop("pd2", author=D31), "allow", check={"effects.pd2.guardedFor": []})
v.step(appr("ad1", "pd2", author=D31), "allow", note="D's carry ended with its own final update")
v.step(E("t2", "task", BOT, {"to": [D31, E31]}), "allow")
v.step(E("ad2", "task.update", D31, {"task": "t2", "state": "accepted"}), "allow")
v.step(E("ed", "task.update", E31, {"task": "t2", "state": "declined"}), "allow", check={"tasks.t2.open": False, "tasks.t2.accepted": [D31]}, note="a co-assignee that never accepted declines: the task closes")
v.step(prop("pd3", author=D31), "allow", check={"effects.pd3.guardedFor": [BOT]}, note="X1: E's decline ends nothing for D")
v.step(E("ep", "task.update", E31, {"task": "t2", "state": "progress"}), "denied", note="the task is closed")
v.step(E("dd3", "task.update", D31, {"task": "t2", "state": "failed"}), "allow", check={"tasks.t2.state": "declined", "tasks.t2.accepted": []}, note="D's own final update keeps the closed task's state")
v.step(E("t3", "task", BOT, {"to": [E31]}), "allow").step(E("ae3", "task.update", E31, {"task": "t3", "state": "accepted"}), "allow")
v.step(E("oc", "task.cancel", O, {"task": "t3"}), "allow", check={"tasks.t3.accepted": [E31], "tasks.t3.open": True, "tasks.t3.cancelRequested": True}, note="Z2 (Q2): an owner's task.cancel only asks; it ends no carry")
v.step(prop("pe", author=E31), "allow", check={"effects.pe.guardedFor": [BOT]}, note="E still carries BOT's guarding")
v.step(E("bc3", "task.cancel", BOT, {"task": "t2"}), "denied", note="a closed task takes no cancel")
v.step(E("of3", "task.update", O, {"task": "t3", "state": "cancelled"}), "allow", check={"tasks.t3.accepted": [], "tasks.t3.open": False}, note="an owner who isn't an assignee force-closes it for everyone")
v.step(E("t4", "task", BOT, {"to": [E31]}), "allow").step(E("ae4", "task.update", E31, {"task": "t4", "state": "accepted"}), "allow")
v.step(E("bc4", "task.cancel", BOT, {"task": "t4"}), "allow", check={"tasks.t4.accepted": [E31]}, note="the guarded author's own cancel request ends nothing")
v.step(E("of", "task.update", O, {"task": "t4", "state": "cancelled"}), "allow", check={"tasks.t4.accepted": [], "tasks.t4.open": False}, note="X1: an owner who isn't an assignee force-closes it for everyone")
v.step(prop("pe2", author=E31), "allow", check={"effects.pe2.guardedFor": []})
v.step(E("t5", "task", BOT, {"to": [D31, E31]}), "allow").step(E("ad5", "task.update", D31, {"task": "t5", "state": "accepted"}), "allow").step(E("ae5", "task.update", E31, {"task": "t5", "state": "accepted"}), "allow")
v.step(E("ed5", "task.update", E31, {"task": "t5", "state": "done"}), "allow", check={"tasks.t5.open": False, "tasks.t5.accepted": [D31]})
v.step(E("of5", "task.update", O, {"task": "t5", "state": "cancelled"}), "allow", check={"tasks.t5.accepted": [], "tasks.t5.state": "done", "tasks.t5.open": False}, note="Z2 (Q5, Y9): an owner's final update clears accepted on a closed task, and changes nothing else")
v.step(E("oc5", "task.cancel", O, {"task": "t5"}), "denied", note="a closed task still takes no cancel")
v.step(E("t6", "task", BOT, {"to": [D31]}), "allow").step(E("ad6", "task.update", D31, {"task": "t6", "state": "accepted"}), "allow", check={"tasks.t6.accepted": [D31]})
v.step(ms("dl", D31, D31, "none"), "allow", check={"tasks.t6.accepted": []}, note="Z2: an actor that leaves the space leaves every accepted list")
v.step(ms("dr", O, D31, "member"), "allow")
v.step(prop("pd6", author=D31), "allow", check={"effects.pd6.guardedFor": []}, note="re-admitted, D carries nothing")
v.step(E("ad7", "task.update", D31, {"task": "t6", "state": "accepted"}), "allow", check={"tasks.t6.accepted": [D31]}, note="until it accepts again")
v.step(ms("db", O, D31, "banned"), "allow", check={"tasks.t6.accepted": []}, note="banning removes it too")
add(v.done())

v = V("a-guarded-authors-work-all-carries", ["P-15", "R-24", "§7.5"], "Z1 (the reviewer's p4_cascade and Q4): while X carries a guarded agent's task, X is guarded by that agent's guard: everything X creates records guards(X), whatever parent or task it names, so the guarding reaches whoever accepts X's work")
agents(v, policy(sessions=MEMBERS3, approvals=[{"action": "*", "approvers": ["member", "owner"]}]))
v.step(ms("h", O, H31, "member"), "allow")
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(E("t1", "task", BOT, {"to": [O]}), "allow")
v.step(E("op1", "task.update", O, {"task": "t1", "state": "progress"}), "allow", check={"tasks.t1.accepted": []}, note="p4: the lead reports progress; that accepts nothing (X4)")
v.step(E("t2", "task", O, {"to": [H31]}), "allow", check={"tasks.t2.guardedFor": []})
v.step(E("ah2", "task.update", H31, {"task": "t2", "state": "accepted"}), "allow")
v.step(E("t3", "task", H31, {"to": [M]}), "allow", check={"tasks.t3.guardedFor": []})
v.step(E("am3", "task.update", M, {"task": "t3", "state": "accepted"}), "allow")
v.step(prop("dep", author=OP), "allow")
v.step(appr("ao", "dep", author=O), "allow").step(appr("ah", "dep", author=H31), "allow").step(appr("am", "dep", author=M), "allow", note="p4: none of the team is guarded")
v.step(E("ac1", "task.update", O, {"task": "t1", "state": "accepted"}), "allow", check={"tasks.t1.accepted": [O]}, note="now the lead accepts BOT's task")
v.step(E("u", "task", OP, {"to": [O]}), "allow").step(E("au", "task.update", O, {"task": "u", "state": "accepted"}), "allow", check={"tasks.u.guardedFor": []})
v.step(E("su", "task", O, {"to": [H31], "parent": "u"}), "allow", check={"tasks.su.guardedFor": [BOT]}, note="Z1: O's subtask of an unrelated parent records guards(O)")
v.step(E("ash", "task.update", H31, {"task": "su", "state": "accepted"}), "allow")
v.step(prop("hp", author=H31, task="su"), "allow", check={"effects.hp.guardedFor": [BOT]})
v.step(appr("ah2", "hp", author=H31), "denied", note="H accepted O's guarded work: H is guarded too")
v.step(prop("pu", author=O, executor=X, task="u"), "allow", check={"effects.pu.guardedFor": [BOT]}, note="Z1: O's effect named under the unrelated task u")
v.step(appr("ao2", "hp", author=O), "denied", note="O is guarded while O carries")
v.step(E("sl", "task", O, {"to": [M], "parent": "t1"}), "allow", check={"tasks.sl.guardedFor": [BOT]}, note="a subtask of O's accepted task")
v.step(E("sl2", "task", M, {"to": [M], "parent": "sl"}), "allow", check={"tasks.sl2.guardedFor": [BOT]}, note="and everything below it")
v.step(prop("pl", author=O, executor=X, task="t1"), "allow", check={"effects.pl.guardedFor": [BOT]})
v.step(E("tf", "task", O, {"to": [M]}), "allow", check={"tasks.tf.guardedFor": [BOT]}, note="a parentless task")
v.step(prop("pf", author=O, executor=X), "allow", check={"effects.pf.guardedFor": [BOT]}, note="and a task-less effect")
v.step(E("uq", "task", M, {"to": [O]}), "allow", check={"tasks.uq.guardedFor": [BOT]}, note="M accepted sl2, its own subtask of the guarded chain: M is guarded too")
v.step(E("s4", "task", O, {"to": [M], "parent": "uq"}), "allow", check={"tasks.s4.guardedFor": [BOT]}, note="Q4: a carrying author's subtask under a task it never accepted carries too")
add(v.done())

v = V("carrying-executors-need-the-guard", ["P-15", "P-07 E3"], "X3, Z1 (the reviewer's p1 W-a and Q1): an effect's guardedFor holds guards(executor) at the proposal, and E3 recomputes guards(proposer) and guards(executor) at the claim, so a carrying delegate can't execute anything without the delegator's guard, whenever it was proposed")
v.prof(D31, AG)
agents(v, policy(sessions=MEMBERS3, approvals=[{"action": "*", "approvers": ["owner"]}]))
v.step(ms("d", O, D31, "member"), "allow").step(ms("h", O, H31, "member"), "allow")
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(E("t", "task", BOT, {"to": [D31]}), "allow")
v.step(prop("e0", author=H31, executor=D31), "allow", check={"effects.e0.guardedFor": []}, note="D hasn't accepted: nothing carried")
v.step(E("ad", "task.update", D31, {"task": "t", "state": "accepted"}), "allow")
v.step(prop("e1", author=H31, executor=D31), "allow", check={"effects.e1.guardedFor": [BOT]}, note="X3: a human asks the carrying D to execute")
v.step(appr("a1", "e1", author=O), "allow").step(claim("c0", "e1", author=D31), "denied", note="BOT's guard hasn't approved")
v.step(appr("g1", "e1", author=OP), "allow").step(claim("c1", "e1", author=D31), "allow", check={"effects.e1.claims.0.guardApprovals": ["g1"]})
v.step(appr("a0", "e0", author=O), "allow").step(claim("c2", "e0", author=D31), "denied", note="Z1 (Q1): D carries BOT now, and e0 never counted BOT: propose again")
v.step(prop("e2", author=D31, executor=D31), "allow", check={"effects.e2.guardedFor": [BOT]}).step(appr("a2", "e2", author=O), "allow")
v.step(claim("c3", "e2", author=D31), "denied", note="BOT's guard hasn't approved")
v.step(E("dd", "task.update", D31, {"task": "t", "state": "done"}), "allow", check={"tasks.t.accepted": []})
v.step(claim("c4", "e0", author=D31), "allow", note="D's own final update ended the carry: e0 needs no guard")
v.step(claim("c5", "e2", author=D31), "denied", note="what e2 recorded never shrinks")
add(v.done())

v = V("only-an-explicit-accept-accepts", ["P-05", "P-15"], "X4 (W10): only task.update with state accepted puts its author in accepted; progress doesn't; an author assigning itself has accepted at creation")
agents(v, policy(sessions=MEMBERS3))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(E("t", "task", BOT, {"to": [M, BOT]}), "allow", check={"tasks.t.accepted": [BOT]})
v.step(E("p0", "task.update", M, {"task": "t", "state": "progress"}), "allow", check={"tasks.t.accepted": [BOT], "tasks.t.state": "progress"})
v.step(prop("n0", author=M), "allow", check={"effects.n0.guardedFor": []}, note="M only reported progress: it carries nothing")
v.step(E("a0", "task.update", M, {"task": "t", "state": "accepted"}), "allow", check={"tasks.t.accepted": [BOT, M]})
v.step(E("p1", "task.update", M, {"task": "t", "state": "progress"}), "allow", check={"tasks.t.accepted": [BOT, M]}, note="progress after accepting changes nothing")
v.step(prop("n1", author=M), "allow", check={"effects.n1.guardedFor": [BOT]})
add(v.done())

v = V("guarded-tasks-leave-room-to-work", ["P-15", "P-16"], "X6 (W11, the reviewer's p2_bound): a task's guardedFor may hold at most 17 agents at its creation, so a proposer and an executor always fit under the 19 of a proposal: a task at 18 is denied, and anyone may work a task at 17")
CHAIN31 = [f"@g{i}:b.example" for i in range(18)]
for a_ in CHAIN31: v.prof(a_, AG)
v.prof(X, XS).prof(D31, dict(human()))
v.step(create(policy(maxDelegationDepth=16, sessions=sess("member", "member", "member", MODES, True))), "allow")
for i, a_ in enumerate([OP, X, D31] + CHAIN31): v.step(ms(f"mm{i}", O, a_, "member"), "allow")
for i in range(16):
    body = {"to": [CHAIN31[i + 1]]}
    if i: body["parent"] = f"c{i - 1}"
    v.step(E(f"c{i}", "task", CHAIN31[i], body), "allow", check={f"tasks.c{i}.guardedFor": CHAIN31[:i + 1]})
v.step(E("c16", "task", CHAIN31[16], {"to": [D31], "parent": "c15"}), "allow", check={"tasks.c16.guardedFor": CHAIN31[:17]}, note="17 agents: at the task bound")
v.step(E("c16b", "task", CHAIN31[16], {"to": [CHAIN31[17]], "parent": "c15"}), "allow")
v.step(E("a17", "task.update", CHAIN31[17], {"task": "c16b", "state": "accepted"}), "allow")
v.step(E("c17", "task", CHAIN31[17], {"to": [D31]}), "denied", note="X6: carried(g17), 17 agents, and g17 itself: 18 is over the task bound (a subtask of c16b would be too deep)")
v.step(E("ad", "task.update", D31, {"task": "c16", "state": "accepted"}), "allow")
v.step(prop("w", author=D31, executor=X, task="c16"), "allow", check={"effects.w.guardedFor": CHAIN31[:17] + [X]}, note="17, the human proposer and the forced service executor: 19, workable")
v.step(prop("w2", author=D31, executor=D31, task="c16"), "allow")
v.step(prop("w3", author=O, executor=X, task="c16"), "allow", note="an owner works it too")
add(v.done())

v = V("denied-servers-approvals-are-dropped", ["P-08", "P-07 E3", "P-04"], "X7 (W12, the reviewer's p5_deny): the policy.set that denies a server drops the open approvals of its actors, so E3 never uses an approval whose author's server is denied; a consumed one a not-started outcome returns is dropped too")
team(v, policy(approvals=[{"action": "*", "approvers": ["member", "owner"]}]))
v.step(ms("z", O, Z, "member"), "allow").step(ms("g", O, G, "member"), "allow")
v.step(prop("e", author=O), "allow").step(appr("az", "e", author=Z), "allow")
v.step(prop("f", author=O), "allow").step(appr("az2", "f", author=Z), "allow").step(claim("cf", "f"), "allow", check={"effects.f.claims.0.approval": "az2"})
v.step(E("deny", "policy.set", O, {"policy": policy(approvals=[{"action": "*", "approvers": ["member", "owner"]}], servers={"deny": ["d.example"]})}), "allow",
       check={"effects.e.approvals": [], "effects.f.approvals": [{"id": "az2", "author": Z, "keyId": "k1", "consumedBy": "cf"}]}, note="p5: Z's open approval goes; its consumed one stays with the claim")
v.step(appr("az3", "e", author=Z), "denied", note="a denied server's actor approves nothing")
v.step(claim("ce", "e"), "denied", note="E3: no usable approval")
v.step(outc("of", "cf", "failed", "no"), "allow", check={"effects.f.approvals": [], "effects.f.phase": "open"}, note="returned, its author's server denied: dropped")
v.step(appr("ag", "e", author=G), "allow").step(claim("ce2", "e"), "allow", check={"effects.e.claims.0.approval": "ag"})
add(v.done())


v = V("grammar-bounds", ["§2", "R-09"], "X18 (Rust port clarifications): a Time's year is 0001 to 9999; a ServerName is at most 253 characters; opaque Json (an extension body) nests at most 32 arrays and objects, the outermost counted")
team(v)
v.step(E("t0", "task", O, {"to": [M], "deadline": "0000-12-31T23:59:59Z"}), "invalid", note="year 0000 is not a Time")
v.step(E("t1", "task", O, {"to": [M], "deadline": "0001-01-01T00:00:00Z"}), "allow").step(E("t9", "task", O, {"to": [M], "deadline": "9999-12-31T23:59:59Z"}), "allow")
LONG253 = ".".join(["a" * 63] * 3 + ["b" * 61])
v.step(ms("s253", O, "@u:" + LONG253, "none"), "allow", note="253 characters")
v.step(ms("s254", O, "@u:" + LONG253 + "c", "none"), "invalid", note="254 characters")
v.step(E("j32", "x.note", M, {"a": NEST(31)}), "allow", note="32 containers: the body and 31 below it")
v.step(E("j33", "x.note", M, {"a": NEST(32)}), "invalid", note="33 containers")
add(v.done())


# ── round 32 (DECISIONS-r32): carrying is guarding; a guard is not trapped by its agent ──
v = V("carrying-is-guarding", ["P-15", "P-16", "P-07 E3", "P-04"], "Z1 (the reviewer's Q1 and Q3): while X carries a guarded agent's task, X is guarded: E3 recomputes guards(proposer) and guards(executor) at the claim, and a carrying owner is a guarded author that may not force-close or cancel others' tasks, move the space or govern; guardStatus reports guards(X)")
v.prof(D31, AG)
agents(v, policy(sessions=MEMBERS3, approvals=[{"action": "*", "approvers": ["owner"]}]))
v.step(ms("d", O, D31, "member"), "allow").step(ms("h", O, H31, "owner"), "allow")
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(prop("e", author=D31, executor=D31), "allow", check={"effects.e.guardedFor": []}, note="Q1: proposed before D accepts anything")
v.step(prop("e1", author=M, executor=D31), "allow", check={"effects.e1.guardedFor": []}, note="and another member's effect that D executes")
v.step(E("t", "task", BOT, {"to": [D31]}), "allow").step(E("ad", "task.update", D31, {"task": "t", "state": "accepted"}), "allow")
v.guard(D31, {"guard": OP, "guarded": True, "forced": False, "guards": [BOT]})
v.step(appr("ae", "e", author=O), "allow").step(claim("ce", "e", author=D31), "denied", note="Z1, E3: guards(D) holds BOT at the claim, and the proposal never counted it: propose again")
v.step(appr("ae1", "e1", author=O), "allow").step(claim("ce1", "e1", author=D31), "denied", note="as the executor too")
v.step(appr("ge", "e", author=OP), "denied", note="BOT's guard is no guard of e, whose guardedFor is empty: nothing it approves can count")
v.step(E("t1", "task", BOT, {"to": [H31]}), "allow").step(E("ah", "task.update", H31, {"task": "t1", "state": "accepted"}), "allow")
v.guard(H31, {"guarded": True, "forced": False, "guards": [BOT]})
v.step(E("hc", "task.update", H31, {"task": "t", "state": "cancelled"}), "denied", note="Q3: a carrying owner may not force-close another's task")
v.step(E("hk", "task.cancel", H31, {"task": "t"}), "denied", note="nor cancel another's task")
v.step(E("hm", "space.move", H31, {"to": "b.example", "accept": ACCEPT}), "denied", note="nor move the space")
v.step(ms("hs", H31, M, "guest"), "denied", note="nor govern")
v.step(E("th", "task", H31, {"to": [M]}), "allow", check={"tasks.th.guardedFor": [BOT]}, note="its own work carries its guards")
v.step(E("hk2", "task.cancel", H31, {"task": "th"}), "allow", note="and it may cancel its own task")
v.step(E("hd", "task.update", H31, {"task": "t1", "state": "done"}), "allow", check={"tasks.t1.accepted": []}, note="its own final update ends the carry")
v.step(E("hc2", "task.update", H31, {"task": "t", "state": "cancelled"}), "allow", check={"tasks.t.accepted": [], "tasks.t.open": False}, note="now H may force-close, which ends D's carry")
v.guard(D31, {"guard": OP, "guarded": False, "forced": False, "guards": []})
v.step(claim("ce3", "e", author=D31), "allow", note="e needs no guard any more")
add(v.done())

v = V("a-guard-is-not-trapped-by-its-agent", ["P-15", "P-16", "P-08"], "Z3 (the reviewer's probe6): an approval whose author is a required guard of that effect is exempt from the guarded Limits: a guard that carries its own agent's task still approves that agent's effects, and its own work under the task, which needs its own approval")
agents(v, policy(sessions=MEMBERS3, approvals=[{"action": "*", "approvers": ["owner"]}]))
v.step(E("s", "session.open", BOT, {"steerMode": "guarded"}), "allow")
v.step(prop("e0", author=BOT), "allow", check={"effects.e0.guardedFor": [BOT]})
v.step(E("t", "task", BOT, {"to": [OP]}), "allow").step(E("ao", "task.update", OP, {"task": "t", "state": "accepted"}), "allow")
v.guard(OP, {"guarded": True, "forced": False, "guards": [BOT]})
v.step(appr("a0", "e0", author=O), "allow").step(appr("g0", "e0", author=OP), "allow", note="Z3: approving is the guard's role")
v.step(claim("c0", "e0"), "allow", check={"effects.e0.claims.0.guardApprovals": ["g0"]})
v.step(prop("e1", author=OP, task="t"), "allow", check={"effects.e1.guardedFor": [BOT]}, note="OP's own work under the task")
v.step(appr("a1", "e1", author=O), "allow").step(claim("c10", "e1"), "denied", note="needs BOT's guard: OP itself")
v.step(appr("g1", "e1", author=OP), "allow", note="a guard's approval of its own guarded work").step(claim("c1", "e1"), "allow", check={"effects.e1.claims.0.guardApprovals": ["g1"]})
v.step(prop("pm", author=M), "allow", check={"effects.pm.guardedFor": []}).step(appr("gm", "pm", author=OP), "denied", note="OP is no guard of M's effect: the Limits hold")
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

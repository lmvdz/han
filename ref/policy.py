"""HAN v1 reference Policy — executable form of architecture.md §6 (P-01…P-15) and the R-09 body schemas.

Six pure functions (R-11): decide(state, entry, ctx), fold(state, record, ctx), can_read(state, reader, seq),
relevant_actors(state, entry, time), session_access(state, actor, session), guard_status(state, ctx, actor). Entries and states are plain dicts shaped exactly like han.ts.
Signatures, content and the exact wire header are checked by Log/Identity (ref/wire.py), not here; Policy reads
only `kind`, `author`, `keyId`, `body`, `refs`, `seen`. Test vectors may carry an explicit "id" (see ref/README.md).
"""
from __future__ import annotations
import copy
import hashlib
import json
import re
from datetime import datetime, timezone

STALL_BOUND = 10            # seconds between the executor's deadline check and its launch (X-04)

DRIFT_PPM = 10_000          # R-03a: every clock's rate is within ±1% (10,000 ppm) of true time

def abandon_after(window: int, drift_ppm: int = None) -> int:
    """P-09: whole seconds after the claim's receipt time before an owner may abandon it.
    The executor launches at most `window` after sending, on its own clock: ≤ window·(1+d) true seconds if that clock
    runs slow by d. The home measures from a receipt time rounded down (≤ 1 s early) on a clock that may run fast by d.
    Safe iff A·(1−d) − 1 > window·(1+d), i.e. A > window·(1+d)/(1−d) + 1. We take the next whole second, plus one."""
    d = DRIFT_PPM if drift_ppm is None else drift_ppm
    num, den = window * (1_000_000 + d), (1_000_000 - d)
    return -(-num // den) + 2
READ_ROLES = ("owner", "member", "guest")
FINAL = ("succeeded", "failed", "cancelled")
TASK_FINAL = ("done", "failed", "declined", "cancelled")
MAX_OPEN_APPROVALS = 16       # unconsumed approvals per proposal (bounds receipt.profiles, P-01)
MAX_OPEN_SESSIONS = 64        # open sessions per actor per space (P-15)
MAX_ATTEMPTS = 64             # claims per effect (P-07 E5): the 65th is denied, propose again
ROLE_INTERVALS = 64           # P-02: role intervals kept per actor (the most recent)

# ───────────────────────── encoding ─────────────────────────
def _jcs_str(s: str) -> str:
    return json.dumps(s, ensure_ascii=False)

def _jcs(v) -> str:
    if v is None: return "null"
    if v is True: return "true"
    if v is False: return "false"
    if isinstance(v, int): return str(v)
    if isinstance(v, str): return _jcs_str(v)
    if isinstance(v, list): return "[" + ",".join(_jcs(x) for x in v) + "]"
    if isinstance(v, dict):
        keys = sorted(v, key=lambda k: k.encode("utf-16-be"))      # RFC 8785: order by UTF-16 code units
        return "{" + ",".join(_jcs_str(k) + ":" + _jcs(v[k]) for k in keys) + "}"
    raise TypeError(f"not JCS-encodable in HAN: {type(v)}")

def jcs(v) -> bytes:
    """RFC 8785 for the JSON subset HAN uses (no floats)."""
    return _jcs(v).encode("utf-8")

def entry_id(entry: dict) -> str:
    e = {k: v for k, v in entry.items() if k not in ("sig", "id")}
    return hashlib.sha256(b"han-entry-v1\n" + jcs(e)).hexdigest()

def ts(t: str) -> float:
    return datetime.strptime(t, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc).timestamp()

def server_of(actor_or_space: str) -> str:
    return actor_or_space.rsplit(":", 1)[1]

# ───────────────────────── R-09 grammar and schemas (mirrored in han.ts) ─────────────────────────
def _rx(pattern):
    """§2 grammar: ASCII only, and the whole string must match (Python's `$` would also accept a trailing newline)."""
    return re.compile(pattern, re.ASCII)

SERVER_RE = r"[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*"
SERVER = _rx(r"^" + SERVER_RE + r"$")
ACTOR = _rx(r"^@[a-z0-9._-]{0,64}:" + SERVER_RE + r"$")
SPACE = _rx(r"^((?!profile:)[a-z0-9]{1,26}:" + SERVER_RE + r"|profile:@[a-z0-9._-]{0,64}:" + SERVER_RE + r")$")   # "profile" is reserved
HEX = _rx(r"^[0-9a-f]{64}$")
KEYID = _rx(r"^[A-Za-z0-9_-]{1,64}$")
TIME = _rx(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$")
ACTION = _rx(r"^[a-z0-9][a-z0-9._-]{0,127}$")
UNIT = _rx(r"^[A-Za-z0-9_-]{1,32}$")
INT_MAX = 2 ** 53

def _int(v): return isinstance(v, int) and not isinstance(v, bool) and 0 <= v < INT_MAX
def _budget(v): return isinstance(v, dict) and set(v) == {"unit", "amount"} and isinstance(v["unit"], str) and bool(UNIT.fullmatch(v["unit"])) and _int(v["amount"])
def _actor(v): return isinstance(v, str) and bool(ACTOR.fullmatch(v))
def _id(v): return isinstance(v, str) and bool(HEX.fullmatch(v))
def _hex(v): return isinstance(v, str) and bool(HEX.fullmatch(v))
def _keyid(v): return isinstance(v, str) and bool(KEYID.fullmatch(v))
def _time(v):
    if not (isinstance(v, str) and TIME.fullmatch(v)): return False
    try:
        datetime.strptime(v, "%Y-%m-%dT%H:%M:%SZ"); return True       # a real proleptic Gregorian date, seconds 00–59
    except ValueError:
        return False
def _actors(v): return isinstance(v, list) and len(v) <= 64 and all(_actor(x) for x in v)
B64URL = _rx(r"^[A-Za-z0-9_-]{1,128}$")
def _pubkey(v): return isinstance(v, str) and bool(B64URL.fullmatch(v))
def _space(v): return isinstance(v, str) and bool(SPACE.fullmatch(v))
def json_ok(v, depth=0) -> bool:
    """Opaque Json (extension bodies, plan.args): integers in [0, 2^53) only, no floats, bounded nesting (R-09)."""
    if depth > 32: return False
    if v is None or isinstance(v, (bool, str)): return True
    if isinstance(v, int): return _int(v)
    if isinstance(v, list): return all(json_ok(x, depth + 1) for x in v)
    if isinstance(v, dict): return all(isinstance(k, str) and json_ok(x, depth + 1) for k, x in v.items())
    return False

POLICY_FIELDS = {"visibility", "join", "membersMayInvite", "allowAgents", "approvals", "separateApprover", "budget",
                 "maxDelegationDepth", "minDurability", "launchWindow", "extensionsMinRole", "sessions", "servers"}
SESSION_LEVELS = ("guest", "member", "owner", "nobody")      # least to most restrictive (P-15)
STEER_MODES = ("auto", "guarded")

def _policy(v):
    if not (isinstance(v, dict) and set(v) == POLICY_FIELDS): return False
    if v["visibility"] not in ("members", "public") or v["join"] not in ("invite", "open", "closed"): return False
    if not isinstance(v["membersMayInvite"], bool) or not isinstance(v["separateApprover"], bool): return False
    if v["allowAgents"] not in ("any", "with-operator", "none") or v["minDurability"] not in ("local", "replicated"): return False
    if not (isinstance(v["approvals"], list) and len(v["approvals"]) <= 64 and all(
            isinstance(a, dict) and set(a) == {"action", "approvers"}
            and isinstance(a["action"], str) and (a["action"] == "*" or bool(ACTION.fullmatch(a["action"])))
            and isinstance(a["approvers"], list) and 0 < len(a["approvers"]) <= 64
            and all(x in READ_ROLES or _actor(x) for x in a["approvers"]) for a in v["approvals"])): return False
    if not (v["budget"] is None or _budget(v["budget"])): return False
    if not (_int(v["maxDelegationDepth"]) and v["maxDelegationDepth"] <= 16): return False
    if not (_int(v["launchWindow"]) and 15 <= v["launchWindow"] <= 3600): return False
    if v["extensionsMinRole"] not in READ_ROLES: return False
    ss = v["sessions"]
    if not (isinstance(ss, dict) and set(ss) - {"forceGuarded"} == {"watch", "read", "steer", "modes"}
            and all(ss[k] in SESSION_LEVELS for k in ("watch", "read", "steer"))
            and isinstance(ss.get("forceGuarded", False), bool)): return False
    L = SESSION_LEVELS.index
    if L(ss["watch"]) > L(ss["read"]) or L(ss["watch"]) > L(ss["steer"]): return False   # read and steer need watch
    m = ss["modes"]
    if not (isinstance(m, list) and m and len(set(m)) == len(m) and all(x in STEER_MODES for x in m)): return False
    if not (isinstance(v["servers"], dict) and set(v["servers"]) == {"deny"} and isinstance(v["servers"]["deny"], list)
            and len(v["servers"]["deny"]) <= 256 and all(isinstance(x, str) and bool(SERVER.fullmatch(x)) for x in v["servers"]["deny"])): return False
    return True

RECEIPT_FIELDS = {"space", "seq", "id", "prev", "home", "homeKey", "time", "durability", "profiles", "homeSig"}
def _receipt(v):
    return (isinstance(v, dict) and set(v) == RECEIPT_FIELDS and _space(v["space"])
            and _int(v["seq"]) and v["seq"] >= 1 and _id(v["id"]) and (v["prev"] == "" or _hex(v["prev"]))
            and isinstance(v["home"], str) and bool(SERVER.fullmatch(v["home"])) and _keyid(v["homeKey"]) and _time(v["time"])
            and v["durability"] in ("local", "replicated") and isinstance(v["profiles"], dict)
            and all(_actor(a) and _int(n) for a, n in v["profiles"].items()) and isinstance(v["homeSig"], str))
def _profile(v):
    return (isinstance(v, dict) and set(v) <= {"actorKind", "operator", "key"} and {"actorKind", "key"} <= set(v)
            and v["actorKind"] in ("human", "agent", "service") and ("operator" not in v or _actor(v["operator"]))
            and isinstance(v["key"], dict) and set(v["key"]) == {"keyId", "publicKey"} and _keyid(v["key"]["keyId"]) and _pubkey(v["key"]["publicKey"]))

# kind → {field: (validator, required)}
SCHEMA = {
    "space.create": {"policy": (_policy, True),
                     "forkOf": (lambda v: isinstance(v, dict) and set(v) == {"space", "through"} and _space(v["space"]) and _int(v["through"]), False),
                     "profile": (_profile, False)},
    "member.set": {"actor": (_actor, True), "role": (lambda v: v in ("owner", "member", "guest", "banned", "none"), True)},
    "policy.set": {"policy": (_policy, True)},
    "key.add": {"keyId": (_keyid, True), "publicKey": (_pubkey, True)},
    "key.revoke": {"keyId": (_keyid, True)},
    "message": {"to": (_actors, False)},
    "task": {"to": (lambda v: _actors(v) and len(v) > 0, True), "budget": (_budget, False), "parent": (_id, False), "deadline": (_time, False)},
    "task.update": {"task": (_id, True), "state": (lambda v: v in ("accepted", "progress") + TASK_FINAL, True)},
    "task.cancel": {"task": (_id, True)},
    "effect.propose": {"executor": (_actor, True), "action": (lambda v: isinstance(v, str) and bool(ACTION.fullmatch(v)), True),
                       "planDigest": (_hex, True), "cost": (_budget, False), "repeatable": (lambda v: isinstance(v, bool), True), "task": (_id, False),
                       "expiresAt": (_time, False)},
    "effect.approve": {"proposal": (_id, True), "planDigest": (_hex, True), "expiresAt": (_time, False)},
    "effect.claim": {"proposal": (_id, True), "attempt": (lambda v: _int(v) and v >= 1, True), "instance": (_keyid, True),
                     "window": (lambda v: _int(v) and 15 <= v <= 3600, True)},
    "effect.cancel": {"proposal": (_id, True)},
    "effect.outcome": {"claim": (_id, True), "result": (lambda v: v in ("succeeded", "failed", "cancelled", "unknown"), True),
                       "started": (lambda v: v in ("yes", "no", "unknown"), True), "cost": (_budget, False)},
    "effect.resolve": {"proposal": (_id, True), "result": (lambda v: v in ("succeeded", "failed"), True), "cost": (_budget, False)},
    "checkpoint": {"through": (_int, True)},
    "redact": {"target": (_id, True)},
    "session.open": {"task": (_id, False), "steerMode": (lambda v: v in STEER_MODES, True)},
    "session.close": {"session": (_id, True)},
    "session.input": {"session": (_id, True)},
    "agent.release": {"agent": (_actor, True), "unguard": (lambda v: v is True, False)},
    "space.move": {"to": (lambda v: isinstance(v, str) and bool(SERVER.fullmatch(v)), True),
                   "accept": (lambda v: isinstance(v, dict) and set(v) == {"keyId", "sig"} and _keyid(v["keyId"]) and isinstance(v["sig"], str), True)},
}
CORE_PREFIXES = {"space", "member", "policy", "key", "message", "task", "effect", "checkpoint", "redact", "session", "agent"}
EXT = _rx(r"^[a-z][a-z0-9-]{0,31}\.[a-z0-9][a-z0-9._-]{0,63}$")

def is_extension(kind) -> bool:
    """Extension kinds: `<prefix>.<name>` with a prefix the core doesn't use (§5). Bodies are opaque to Policy."""
    return isinstance(kind, str) and bool(EXT.fullmatch(kind)) and kind.split(".")[0] not in CORE_PREFIXES

POLICY_HEADER = {"v", "space", "kind", "author", "keyId", "body", "contentHash", "refs", "seen", "nonce", "time", "sig", "id"}

def validate(entry) -> str | None:
    """R-09 for the fields Policy reads. The exact wire header is checked by wire.validate_header."""
    if not isinstance(entry, dict) or not set(entry) <= POLICY_HEADER: return "unknown header field"
    if entry.get("v") != 1: return "version"
    if not _actor(entry.get("author")): return "author"
    if not _keyid(entry.get("keyId")): return "keyId"
    if not _space(entry.get("space")): return "space"
    if not (isinstance(entry.get("refs"), list) and len(entry["refs"]) <= 64 and all(_id(r) for r in entry["refs"])): return "refs"
    if not _int(entry.get("seen")): return "seen"
    kind, body = entry.get("kind"), entry.get("body")
    if not isinstance(body, dict): return "body not object"
    if kind not in SCHEMA:
        if not is_extension(kind): return "unknown kind"
        return None if json_ok(body) else "extension body: only non-negative integers, no floats"
    sch = SCHEMA[kind]
    for f in body:
        if f not in sch: return f"unknown body field {f}"
    for f, (ok, req) in sch.items():
        if f in body:
            if not ok(body[f]): return f"bad {f}"
        elif req: return f"missing {f}"
    return None

# ───────────────────────── small helpers ─────────────────────────
def deny(reason): return {"allow": False, "code": "denied", "reason": reason}
def invalid(reason): return {"allow": False, "code": "invalid", "reason": reason}
ALLOW = {"allow": True}

def key_status(ctx, actor, key_id):
    p = ctx["profiles"].get(actor)
    if p is None: return None
    return p["keys"].get(key_id, {}).get("status", "absent")

def role_of(state, actor):
    return state["roles"].get(actor)

def approval_rule(policy, action):
    for r in policy["approvals"]:
        if r["action"] == action or r["action"] == "*":
            return r
    return None

def may_approve(state, rule, actor):
    role = role_of(state, actor)
    return role in READ_ROLES and any(x == actor or x == role for x in rule["approvers"])

def latest_claim(eff):
    return eff["claims"][-1] if eff["claims"] else None

def find_claim(state, claim_id):
    for pid, eff in state["effects"].items():
        for c in eff["claims"]:
            if c["id"] == claim_id:
                return pid, eff, c
    return None, None, None

def approval_candidates(state, eff, time):
    """E3 order: unconsumed, unexpired approvals whose author satisfies the rule now (no profiles needed)."""
    rule = approval_rule(state["policy"], eff["action"])
    out = []
    for a in eff["approvals"]:
        if a.get("consumedBy"): continue
        if time is not None and a.get("expiresAt") and ts(a["expiresAt"]) <= ts(time): continue
        if not rule or not may_approve(state, rule, a["author"]): continue
        if state["policy"]["separateApprover"] and a["author"] == eff["proposer"]: continue
        out.append(a)
    return out

def relevant_actors(state, entry, time=None) -> list[str]:
    """P-01: actors whose profiles the receipt must pin: the author; the target of member.set; the targets of an
    instruction (the actor of the session a steer goes into, each assignee of a task, the executor of a proposal); for a
    claim, the proposer and the author of every approval candidate (E3 order, unexpired at `time`), then the effect's guards."""
    out = [entry["author"]]
    b = entry["body"]
    if entry["kind"] == "member.set":
        out.append(b["actor"])
    if entry["kind"] == "task":
        out.extend(b["to"])
    if entry["kind"] == "effect.propose":
        out.append(b["executor"])
    if entry["kind"] == "session.input" and state:
        sess = state["sessions"].get(b["session"])
        if sess: out.append(sess["actor"])
    if entry["kind"] == "effect.claim" and state:
        eff = state["effects"].get(b["proposal"])
        if eff:
            out.append(eff["proposer"])
            out.extend(a["author"] for a in approval_candidates(state, eff, time))
            out.extend(guards_of(state, eff))
    return list(dict.fromkeys(out))


# ── budgets (P-05, P-06): one budget per space, one unit ──
def space_unit(state):
    b = state["policy"]["budget"]
    return b["unit"] if b else None

def task_source(state, parent_id):
    """Where a new task's budget is reserved from: its parent if the parent has a budget, else the space."""
    p = state["tasks"].get(parent_id) if parent_id else None
    return ("task", parent_id) if p and p.get("left") else ("space", None)

def effect_source(state, eff):
    """Where an effect's cost is charged: its task if that task has a budget, else the space."""
    t = state["tasks"].get(eff.get("task")) if eff.get("task") else None
    return ("task", eff["task"]) if t and t.get("left") else ("space", None)

def remaining(state, src):
    return state["tasks"][src[1]]["left"]["amount"] if src[0] == "task" else state["budgetLeft"]["amount"]

def has_reservations(state):
    if any(t["open"] and t.get("left") for t in state["tasks"].values()): return True
    for e in state["effects"].values():
        c = latest_claim(e)
        if c and c.get("reserved") and not c.get("settled"): return True
    return False

def rebudget(old_budget, old_left, new_budget):
    """Remaining budget after a policy change (P-06): same unit → new amount − committed (reserved or spent so far).
    The result may be negative when a budget is cut below what is already committed: nothing new can be reserved
    until refunds bring it back above zero. A unit change (only allowed with nothing reserved) starts fresh."""
    if new_budget is None: return None
    if old_budget and old_left and old_budget["unit"] == new_budget["unit"]:
        committed = old_budget["amount"] - old_left["amount"]
        return {"unit": new_budget["unit"], "amount": new_budget["amount"] - committed}
    return dict(new_budget)

def budget_change_ok(state, new_budget):
    old = state["policy"]["budget"]
    same = (old is None and new_budget is None) or (old is not None and new_budget is not None and old["unit"] == new_budget["unit"])
    return same or not has_reservations(state)

# ───────────────────────── decide ─────────────────────────
def decide(state, entry, ctx) -> dict:
    err = validate(entry)
    if err: return invalid(err)
    kind, author = entry["kind"], entry["author"]
    ra = relevant_actors(state, entry, ctx["time"])
    for actor in (ra[:2] if kind == "effect.claim" else ra):
        if actor not in ctx["profiles"]: return invalid(f"no pinned profile for {actor}")
    if key_status(ctx, author, entry["keyId"]) != "valid": return deny("author key not valid")

    if state is None:
        return _decide_create(entry, ctx)
    if kind == "space.create": return invalid("space.create only at seq 1")
    if entry["seen"] > state["head"]: return invalid("seen beyond head")
    for r in entry["refs"]:
        if r not in state["entries"]: return invalid("ref not in space")          # R-05
    if server_of(author) in state["policy"]["servers"]["deny"] and kind not in ("effect.outcome", "effect.resolve"):
        return deny("server denied")
    if state["space"].startswith("profile:"): return _decide_profile(state, entry, ctx)

    role = role_of(state, author)
    if author in state["held"] and not held_may(state, entry):
        return deny("held: an outside steer is waiting for this agent's human")       # P-15
    if is_extension(kind):
        need = state["policy"]["extensionsMinRole"]
        return ALLOW if role in READ_ROLES[:READ_ROLES.index(need) + 1] else deny(f"extensions need role {need} or higher")
    fn = {"member.set": _member_set, "policy.set": _policy_set, "message": _message, "task": _task,
          "task.update": _task_update, "task.cancel": _task_cancel, "effect.propose": _propose,
          "effect.approve": _approve, "effect.claim": _claim, "effect.cancel": _cancel,
          "effect.outcome": _outcome, "effect.resolve": _resolve, "checkpoint": _checkpoint,
          "redact": _redact, "space.move": _move,
          "session.open": _session_open, "session.close": _session_close, "session.input": _session_input,
          "agent.release": _agent_release}.get(kind)
    if fn is None: return deny(f"{kind} not allowed here")
    return fn(state, entry, ctx, role)

def _decide_create(entry, ctx):
    if entry["kind"] != "space.create": return invalid("first entry must be space.create")
    b, author, space = entry["body"], entry["author"], entry["space"]
    if space.startswith("profile:"):
        actor = space[len("profile:"):]
        if author != "@:" + server_of(actor): return deny("a profile space is created by its server")
        if "profile" not in b: return invalid("profile space needs profile")
        if b["profile"].get("operator") == actor: return invalid("an actor can't be its own operator")
        return ALLOW
    if "profile" in b: return invalid("profile only in profile spaces")
    if server_of(space) in b["policy"]["servers"]["deny"]: return invalid("a space can't deny its own home")
    if not is_human(ctx, author): return deny("owners are human: only a human creates a space (P-03)")
    return ALLOW

def _decide_profile(state, entry, ctx):
    kind, author, b = entry["kind"], entry["author"], entry["body"]
    actor = state["space"][len("profile:"):]
    server = "@:" + server_of(actor)
    keys = state["profile"]["keys"]
    if kind in ("key.add", "key.revoke"):
        if author not in (actor, server): return deny("only the actor or its server manages keys")
        if kind == "key.add" and b["keyId"] in keys: return invalid("keyIds are single-use")
        if kind == "key.revoke" and keys.get(b["keyId"], {}).get("status") != "valid": return invalid("no such valid key")
        return ALLOW
    if kind == "member.set":
        op = state["profile"].get("operator")
        if op and author == op and b == {"actor": op, "role": "owner"}:
            if ctx["profiles"][op]["actorKind"] != "human": return deny("an operator must be a human actor")
            return ALLOW
        if op and author in (op, server) and b == {"actor": op, "role": "none"}:
            return ALLOW                                                          # operator withdraws (or server revokes)
        return deny("only operator confirmation or withdrawal allowed")
    return deny(f"{kind} not allowed in a profile space")

def _member_set(state, entry, ctx, role):
    author, tgt, new = entry["author"], entry["body"]["actor"], entry["body"]["role"]
    cur = role_of(state, tgt)
    pol = state["policy"]
    owners = [a for a, r in state["roles"].items() if r == "owner"]
    if cur == "owner" and new != "owner" and len(owners) == 1: return deny("would leave zero owners")
    if new in READ_ROLES and server_of(tgt) in pol["servers"]["deny"]: return deny("target server denied")
    if new == "owner" and ctx["profiles"][tgt]["actorKind"] != "human": return deny("owners must be human")
    if new in READ_ROLES and cur not in READ_ROLES:
        prof = ctx["profiles"][tgt]
        if prof["actorKind"] != "human":
            if pol["allowAgents"] == "none": return deny("agents not allowed")
            if pol["allowAgents"] == "with-operator" and not prof.get("operatorConfirmed"): return deny("agent needs confirmed operator")
    if role == "owner": return ALLOW
    if author == tgt and new == "none" and cur not in (None, "banned"): return ALLOW                        # self-leave
    if author == tgt and new == "member" and cur is None and pol["join"] == "open": return ALLOW           # open join
    if role == "member" and pol["membersMayInvite"] and cur is None and new in ("guest", "member"): return ALLOW  # invite
    return deny("member.set not permitted")

def _policy_set(state, entry, ctx, role):
    if role != "owner": return deny("owners only")
    deny_list = entry["body"]["policy"]["servers"]["deny"]
    if state["home"] in deny_list: return deny("can't deny the home's own server")
    if all(server_of(a) in deny_list for a, r in state["roles"].items() if r == "owner"): return deny("can't deny every owner's server")
    if not budget_change_ok(state, entry["body"]["policy"]["budget"]): return deny("budget unit can't change while reservations exist")
    return ALLOW

def _message(state, entry, ctx, role):
    return ALLOW if role in READ_ROLES else deny("not a member")

def _task(state, entry, ctx, role):
    b, author = entry["body"], entry["author"]
    if role not in READ_ROLES: return deny("author holds no role")
    for a in b["to"]:
        if role_of(state, a) not in READ_ROLES: return deny("assignee holds no role")
    parent = state["tasks"].get(b["parent"]) if "parent" in b else None
    if "parent" in b:
        if not parent: return invalid("no such parent task")
        if not parent["open"]: return deny("parent task closed")
        if author not in parent["to"]: return deny("only an assignee may delegate")
        if parent["depth"] + 1 > state["policy"]["maxDelegationDepth"]: return deny("delegation too deep")
        if parent.get("left") and "budget" not in b: return deny("child of a budgeted task needs a budget")
    elif role not in ("owner", "member"):
        return deny("not allowed to create tasks")
    if "budget" in b and not _accountable(state, ctx, author):
        return deny("an agent without a confirmed operator can't draw budget")
    if "budget" in b and ctx["profiles"][author]["actorKind"] != "human":
        if b.get("parent") not in _budgeted_tasks_of(state, author): return deny("agents draw budget only beneath a budgeted task assigned to them")
    if "budget" in b:
        unit = space_unit(state)
        if unit is None: return deny("no space budget")
        if b["budget"]["unit"] != unit: return deny("budget unit must be the space's unit")
        if b["budget"]["amount"] > remaining(state, task_source(state, b.get("parent"))): return deny("budget exceeds what is left")
    for a in b["to"]:
        d = forced_denial(state, ctx, a, author)
        if d: return d
    return ALLOW

def _task_update(state, entry, ctx, role):
    t = state["tasks"].get(entry["body"]["task"])
    if not t: return invalid("no such task")
    if not t["open"]: return deny("task closed")
    if role not in READ_ROLES: return deny("author holds no role")
    final = entry["body"]["state"] in TASK_FINAL
    if role == "owner" and final and entry["author"] not in t["to"]: return ALLOW         # owner force-close (P-05)
    if entry["author"] in t["to"]:
        if final and any(e.get("task") == entry["body"]["task"] and e["phase"] in ("claimed", "unknown") for e in state["effects"].values()):
            return deny("task has effects still running or of unknown outcome")
        return ALLOW
    return deny("only assignees update")

def _task_cancel(state, entry, ctx, role):
    t = state["tasks"].get(entry["body"]["task"])
    if not t: return invalid("no such task")
    if not t["open"]: return deny("task closed")
    if role not in READ_ROLES: return deny("author holds no role")
    if entry["author"] != t["author"] and role != "owner": return deny("only asker or owner")
    return ALLOW

def _propose(state, entry, ctx, role):
    b, author = entry["body"], entry["author"]
    if role not in ("owner", "member"): return deny("not allowed to propose")
    if "task" in b:
        t = state["tasks"].get(b["task"])
        if not t: return invalid("no such task")
        if not t["open"]: return deny("task not open")
        if author not in t["to"] and role != "owner": return deny("only an assignee or owner charges a task")
    unit = space_unit(state)
    if unit and ("cost" not in b or b["cost"]["unit"] != unit): return deny("budgeted space: cost in the space's unit required")
    if not _accountable(state, ctx, author): return deny("agent's operator confirmation withdrawn")
    if unit and ctx["profiles"][author]["actorKind"] != "human":
        mine = _budgeted_tasks_of(state, author)
        if b.get("task") not in mine: return deny("agents spend only from a budgeted task assigned to them")
    return forced_denial(state, ctx, b["executor"], author) or ALLOW

def _budgeted_tasks_of(state, actor):
    return [tid for tid, t in state["tasks"].items() if t["open"] and t.get("left") and actor in t["to"]]

def _accountable(state, ctx, actor):
    """P-03: under allowAgents "with-operator", a non-human actor needs a confirmed operator at every propose/claim/budget draw."""
    p = ctx["profiles"][actor]
    if p["actorKind"] == "human": return True
    if state["policy"]["allowAgents"] == "none": return False
    return state["policy"]["allowAgents"] != "with-operator" or bool(p.get("operatorConfirmed"))

def _approve(state, entry, ctx, role):
    b, author = entry["body"], entry["author"]
    eff = state["effects"].get(b["proposal"])
    if not eff: return invalid("no such proposal")
    if b["planDigest"] != eff["planDigest"]: return invalid("planDigest mismatch")
    if eff["phase"] not in ("awaiting-approval", "ready", "claimed"): return deny(f"cannot approve in {eff['phase']}")
    rule = approval_rule(state["policy"], eff["action"])
    guards = guards_of(state, eff)
    is_guard = author in guards and role in READ_ROLES
    if b.get("expiresAt") and ts(b["expiresAt"]) <= ts(ctx["time"]): return deny("approval already expired")
    if not is_guard and (not rule or not may_approve(state, rule, author)): return deny("not an approver for this action")
    if not is_guard and state["policy"]["separateApprover"] and author == eff["proposer"]: return deny("separate approver required")
    others = [a for a in eff["approvals"] if not a.get("consumedBy") and a["author"] not in guards
              and a["author"] != author and not (a.get("expiresAt") and ts(a["expiresAt"]) <= ts(ctx["time"]))]
    if not is_guard and len(others) >= MAX_OPEN_APPROVALS: return deny("too many open approvals")   # a new one supersedes the author's own
    return ALLOW

def approvals_complete(state, eff):
    """Ready once the policy rule (if any) has an approval from an allowed approver and the guard (if any) has approved."""
    open_ = [a for a in eff["approvals"] if not a.get("consumedBy")]
    ok_rule = not approval_rule(state["policy"], eff["action"]) or bool(approval_candidates(state, eff, None))
    return ok_rule and all(any(a["author"] == g for a in open_) for g in guards_of(state, eff))

def rearm(state, eff):
    """The phase after an attempt ends without success: cancelled if asked, else ready iff approvals are in hand
    (not checking expiry or keys; E3 at claim is authoritative)."""
    return "cancelled" if eff["cancelRequested"] else ("ready" if approvals_complete(state, eff) else "awaiting-approval")

def pick_approval(state, eff, ctx):
    """E3: the first candidate whose signing key is valid. A candidate reached with no pinned profile → invalid."""
    for a in approval_candidates(state, eff, ctx["time"]):
        st = key_status(ctx, a["author"], a["keyId"])
        if st is None: return "unpinned", a
        if st == "valid": return "ok", a
    return "none", None

def _claim(state, entry, ctx, role):
    b, author = entry["body"], entry["author"]
    eff = state["effects"].get(b["proposal"])
    if not eff: return invalid("no such proposal")
    if author != eff["executor"] or b["instance"] != entry["keyId"]: return deny("E1 not the named executor instance")
    if role not in ("owner", "member"): return deny("E1 executor no longer a member")
    if not _accountable(state, ctx, author): return deny("E1 executor's operator confirmation withdrawn")
    if eff.get("expiresAt") and ts(eff["expiresAt"]) <= ts(ctx["time"]): return deny("E5 proposal expired")
    if role_of(state, eff["proposer"]) not in ("owner", "member") or key_status(ctx, eff["proposer"], eff["proposerKey"]) != "valid":
        return deny("E2 proposer no longer authorized")
    if not _accountable(state, ctx, eff["proposer"]): return deny("E2 proposer's operator confirmation withdrawn")
    if b["window"] != state["policy"]["launchWindow"]: return deny("E5 claim's window is not the space's launchWindow")
    if approval_rule(state["policy"], eff["action"]):
        res, _ = pick_approval(state, eff, ctx)
        if res == "unpinned": return invalid("approver profile not pinned")
        if res == "none": return deny("E3 no usable approval")
    for g in guards_of(state, eff):
        res, _ = guard_approval(state, eff, g, ctx)
        if res == "unpinned": return invalid("guard profile not pinned")
        if res == "none": return deny("E3 a guard hasn't approved")
    if eff["cancelRequested"]: return deny("E4 cancel requested")
    if eff["phase"] != "ready": return deny(f"E5 phase {eff['phase']}")
    if eff.get("task") and not state["tasks"][eff["task"]]["open"]: return deny("E5 the effect's task is closed")
    if b["attempt"] != len(eff["claims"]) + 1: return deny("E5 attempt number")
    if len(eff["claims"]) >= MAX_ATTEMPTS: return deny("E5 attempt limit: propose again")
    unit = space_unit(state)
    if unit:
        if not eff.get("cost") or eff["cost"]["unit"] != unit: return deny("E6 uncosted effect in budgeted space")
        if remaining(state, effect_source(state, eff)) < eff["cost"]["amount"]: return deny("E6 budget")
        if effect_source(state, eff)[0] == "space" and not is_human(ctx, eff["proposer"]):
            return deny("E6 agents spend only from a budgeted task assigned to them")
    return ALLOW

def _cancel(state, entry, ctx, role):
    eff = state["effects"].get(entry["body"]["proposal"])
    if not eff: return invalid("no such proposal")
    a = entry["author"]
    if role not in READ_ROLES: return deny("author holds no role")
    if not (a == eff["proposer"] or role == "owner" or a in guards_of(state, eff) or any(x["author"] == a for x in eff["approvals"])):
        return deny("not proposer/approver/owner")
    if eff["phase"] in FINAL: return deny("already final")
    if eff["phase"] in ("claimed", "unknown") and eff["cancelRequested"]: return deny("already requested")
    return ALLOW

VALID_PAIRS = {("succeeded", "yes"), ("failed", "yes"), ("failed", "no"), ("cancelled", "yes"), ("cancelled", "no"),
               ("unknown", "yes"), ("unknown", "unknown")}

def _cost_ok(state, cost, reserved):
    """P-06: a reported cost must fit the claim's reservation; with no reservation it is allowed only when the space
    has no budget (then it is informational)."""
    if reserved:
        return cost["unit"] == reserved["unit"] and cost["amount"] <= reserved["amount"]
    return space_unit(state) is None

def _outcome(state, entry, ctx, role):
    b, author = entry["body"], entry["author"]
    pid, eff, c = find_claim(state, b["claim"])
    if not c: return invalid("no such claim")
    if c is not latest_claim(eff) or eff["phase"] != "claimed": return deny("claim not open")
    if (b["result"], b["started"]) not in VALID_PAIRS: return invalid("inconsistent result/started")
    if "cost" in b:
        if b["started"] != "yes": return invalid("cost only with started:yes")
        if not _cost_ok(state, b["cost"], c.get("reserved")): return invalid("cost not covered by the reservation")
    if author == eff["executor"] and entry["keyId"] == c["instance"]: return ALLOW
    if role == "owner" and b["result"] == "unknown" and b["started"] == "unknown" and "cost" not in b:
        if ts(ctx["time"]) >= ts(c["time"]) + abandon_after(c["window"]): return ALLOW
        return deny("abandon before the launch window closed")
    return deny("only the claiming instance reports")

def _resolve(state, entry, ctx, role):
    b, author = entry["body"], entry["author"]
    eff = state["effects"].get(b["proposal"])
    if not eff: return invalid("no such proposal")
    if eff["phase"] != "unknown": return deny("nothing to resolve")
    if author != eff["executor"] and role != "owner": return deny("executor or owner only")
    if "cost" in b:
        if b["result"] == "failed": return invalid("no cost on failed")
        c = latest_claim(eff)
        if not _cost_ok(state, b["cost"], c.get("reserved") if c else None): return invalid("cost not covered by the reservation")
    return ALLOW

def _checkpoint(state, entry, ctx, role):
    if role not in ("owner", "member"): return deny("not allowed")
    return ALLOW if entry["body"]["through"] <= state["head"] else invalid("through beyond head")

def _redact(state, entry, ctx, role):
    tid = entry["body"]["target"]
    t = state["entries"].get(tid)
    if not t: return invalid("no such entry")
    if t["kind"] == "redact": return deny("cannot redact a redact")
    if t["kind"] == "effect.propose" and state["effects"][tid]["phase"] not in FINAL:
        return deny("the plan of an unfinished effect can't be redacted; cancel it first")
    return ALLOW if (entry["author"] == t["author"] or role == "owner") else deny("author or owner only")

def is_human(ctx, actor):
    return ctx["profiles"].get(actor, {}).get("actorKind") == "human"

HELD_KINDS = {"message", "effect.propose", "effect.claim", "effect.outcome", "effect.resolve", "effect.cancel",
              "task.update", "task.cancel", "session.open", "session.close"}

def held_may(state, entry):
    """P-15: a held agent may talk, report and propose (its proposals wait for its guard), and work its own sessions; it
    may not instruct anyone else (session.input into others' sessions, task), approve, or govern."""
    if entry["kind"] == "session.input":
        sess = state["sessions"].get(entry["body"]["session"])
        return not sess or sess["actor"] == entry["author"]        # a session that isn't there is `invalid`, whoever steers it
    return entry["kind"] in HELD_KINDS

def guard_status(state, ctx, x) -> dict:
    """P-15, the one guard function: `{guard?, held, forced}` for agent `x` at this entry. `forced`: owners' `forceGuarded` is on
    and the profile pinned for this entry says `x` is not human. `guard`: in a forced space the operator in that profile, if
    confirmed; else the guard `x` is held for, else the guard `x` chose by opening a `guarded` session (`guarded[x]`, sticky).
    `held`: `x` waits for its guard."""
    p = ctx["profiles"].get(x)
    forced = bool(state["policy"]["sessions"].get("forceGuarded")) and bool(p) and p["actorKind"] != "human"
    g = (p.get("operator") if forced and p.get("operatorConfirmed") else None) or state["held"].get(x) or state["guarded"].get(x)
    out = {"held": x in state["held"], "forced": forced}
    if g: out["guard"] = g
    return out

def forced_denial(state, ctx, x, author):
    """P-15: in a forced space an instruction to a non-human target whose pinned profile has no confirmed operator holding
    a role here is denied. An actor's own instruction to itself is not one."""
    if author == x or not guard_status(state, ctx, x)["forced"]: return None
    p = ctx["profiles"][x]
    if not p.get("operatorConfirmed") or role_of(state, p.get("operator")) not in READ_ROLES:
        return deny("a forced space needs every agent's operator here")
    return None

def pending_outside(s, x, g):
    """P-15: what holds agent `x` for its guard `g` since `g`'s last release: open tasks naming `x` and unfinished effects naming
    it as executor, from anyone but `x` and `g`; and the latest outside steer into one of its sessions (`outside[x]`, the one
    trace a steer leaves once its session closes). Returns (tasks and effects, steer seq or 0). The one list `settle_holds`
    and the stale-release check share."""
    since = s["cleared"].get(x, 0)
    outer = lambda who, seq: who not in (x, g) and seq > since
    items = [t for t in s["tasks"].values() if t["open"] and x in t["to"] and outer(t["author"], t["seq"])]
    items += [e for e in s["effects"].values() if e["phase"] not in FINAL and e["executor"] == x and outer(e["proposer"], e["seq"])]
    steer = s["outside"].get(x, 0)
    return items, (steer if steer > since else 0)

def instruct(s, ctx, x, author):
    """The one hold rule: an instruction to an agent that is guarded or forced for it (a steer into any of its sessions, a task
    naming it, or a proposal naming it as executor) from anyone but itself and its guard holds it for that guard.
    Returns that guard if the instruction was an outside one, else None."""
    g = guard_status(s, ctx, x).get("guard")
    if g and author not in (x, g):
        s["held"].setdefault(x, g)
        return g
    return None

def add_guard(s, eff, g):
    """Record `g` among an effect's guards and re-run the readiness test: `ready` iff every approval it needs, the guard's
    included, is in hand."""
    if g not in eff.setdefault("guards", []): eff["guards"].append(g)
    if eff["phase"] == "ready" and not approvals_complete(s, eff): eff["phase"] = "awaiting-approval"

def settle_holds(s):
    """P-15, after every entry: an agent that chose to be guarded and has outside work pending (`pending_outside`) is held, and
    each pending effect records the guard. One place, so every way of becoming guarded (choosing it, a guard regaining its
    role) behaves alike. `forceGuarded` is not here: it acts at the instruction, from the profile pinned then (`instruct`)."""
    for x in sorted(s["guarded"]):
        g = s["guarded"][x]
        items, _ = pending_outside(s, x, g)
        if items and x not in s["held"]: s["held"][x] = g
        for e in items:
            if "proposer" in e: add_guard(s, e, g)

def guards_of(state, eff):
    """Whose own approval the effect needs, besides any rule: the guard its proposer was held for when it proposed, and
    the guards its proposer and its executor are held for now. Nothing a held agent proposes or executes runs without
    its guard, whenever it was proposed."""
    g = list(eff.get("guards", []))
    for who in (eff["proposer"], eff["executor"]):
        h = state["held"].get(who)
        if h and h not in g: g.append(h)
    return g

def guard_approval(state, eff, g, ctx):
    """E3 for a guard: an unconsumed, unexpired approval by `g` itself, while `g` holds a role, signed with a
    valid key. Nobody stands in for a guard. No pinned profile → unpinned (invalid claim)."""
    if role_of(state, g) not in READ_ROLES: return "none", None
    for a in eff["approvals"]:
        if a["author"] != g or a.get("consumedBy"): continue
        if a.get("expiresAt") and ts(a["expiresAt"]) <= ts(ctx["time"]): continue
        st = key_status(ctx, a["author"], a["keyId"])
        if st is None: return "unpinned", a
        if st == "valid": return "ok", a
    return "none", None

def live(state, sess):
    """Live: (open: closed sessions are not in the state) its actor holds a role, and its actor is human, or the session is
    `auto`, or its guard holds a role."""
    return (bool(sess) and role_of(state, sess["actor"]) in READ_ROLES
            and (sess["human"] or sess["steerMode"] == "auto" or role_of(state, sess.get("guard")) in READ_ROLES))

def session_access(state, actor, session_id) -> dict:
    """P-15: what `actor` may do with a live session: watch its output, read its full context, steer it.
    The session's own actor always has all three. A level of "nobody" grants nothing to anyone else."""
    s = state["sessions"].get(session_id)
    if not live(state, s): return {"watch": False, "read": False, "steer": False}
    if actor == s["actor"] or (actor == s.get("guard") and role_of(state, actor) in READ_ROLES):
        return {"watch": True, "read": True, "steer": True}
    role = state["roles"].get(actor)
    out = {}
    for right in ("watch", "read", "steer"):
        need = state["policy"]["sessions"][right]
        out[right] = need != "nobody" and role in READ_ROLES and READ_ROLES.index(role) <= READ_ROLES.index(need)   # owner < member < guest
    return out

def _session_open(state, entry, ctx, role):
    b, author = entry["body"], entry["author"]
    if role not in READ_ROLES: return deny("author holds no role")
    if not _accountable(state, ctx, author): return deny("agent's operator confirmation withdrawn")
    if b["steerMode"] not in state["policy"]["sessions"]["modes"]: return deny("steer mode not allowed in this space")
    if b["steerMode"] != "auto" and not is_human(ctx, author):
        p = ctx["profiles"][author]
        if not (p.get("operatorConfirmed") and role_of(state, p.get("operator")) in READ_ROLES):
            return deny("a guarded agent session needs a confirmed operator who holds a role here")
    t = b.get("task")
    if t is not None:
        task = state["tasks"].get(t)
        if not task: return invalid("no such task")
        if not task["open"]: return deny("task closed")
        if author not in task["to"] and role != "owner": return deny("only an assignee or owner works a task")
    if sum(1 for x in state["sessions"].values() if x["actor"] == entry["author"]) >= MAX_OPEN_SESSIONS:
        return deny("too many open sessions for this actor")
    return ALLOW

def _session_close(state, entry, ctx, role):
    sess = state["sessions"].get(entry["body"]["session"])
    if not sess: return invalid("no such session")
    if entry["author"] != sess["actor"] and role != "owner": return deny("only the session's actor or an owner closes it")
    return ALLOW

def _session_input(state, entry, ctx, role):
    sess = state["sessions"].get(entry["body"]["session"])
    if not sess: return invalid("no such session")
    if not live(state, sess): return deny("session not live")
    if not _accountable(state, ctx, entry["author"]): return deny("agent's operator confirmation withdrawn")
    if not session_access(state, entry["author"], entry["body"]["session"])["steer"]: return deny("no steer right on this session")
    return forced_denial(state, ctx, sess["actor"], entry["author"]) or ALLOW

def _agent_release(state, entry, ctx, role):
    b = entry["body"]
    x = b["agent"]
    if not (state["held"].get(x) == entry["author"] or (b.get("unguard") and state["guarded"].get(x) == entry["author"])):
        return deny("only the guard holding this agent releases it")
    items, steer = pending_outside(state, x, entry["author"])
    if max([i["seq"] for i in items] + [steer]) > entry["seen"]:
        return deny("stale release: an outside instruction arrived after what the guard had seen")
    return ALLOW if role in READ_ROLES else deny("guard holds no role")

def _move(state, entry, ctx, role):
    if role != "owner": return deny("owners only")
    if any(e["phase"] == "claimed" for e in state["effects"].values()): return deny("cannot move while an effect is claimed")
    return ALLOW

# ───────────────────────── fold ─────────────────────────
def fold(state, record, ctx) -> dict:
    e, rc = record["entry"], record["receipt"]
    kind, author, b = e["kind"], e["author"], e["body"]
    eid = e.get("id") or entry_id(e)
    if state is None:
        return _fold_create(e, rc, ctx, eid)
    s = copy.deepcopy(state)
    s["head"] = rc["seq"]
    s["entries"][eid] = {"author": author, "kind": kind}
    seq = rc["seq"]
    profile_space = s["space"].startswith("profile:")

    if kind == "member.set" and not profile_space:
        tgt, new = b["actor"], b["role"]
        hist = s["roleHistory"].setdefault(tgt, [])
        before = s["roles"].get(tgt) in READ_ROLES
        if new == "none": s["roles"].pop(tgt, None)
        else: s["roles"][tgt] = new
        now = new in READ_ROLES
        if now and not before:
            hist.append({"from": seq})
            del hist[:-ROLE_INTERVALS]
        if before and not now and hist: hist[-1]["to"] = seq
    elif kind == "member.set":
        s["profile"]["operatorConfirmed"] = b["role"] == "owner"
    elif kind == "key.add":
        s["profile"]["keys"][b["keyId"]] = {"publicKey": b["publicKey"], "status": "valid"}
    elif kind == "key.revoke":
        s["profile"]["keys"][b["keyId"]]["status"] = "revoked"
    elif kind == "policy.set":
        s["budgetLeft"] = rebudget(s["policy"]["budget"], s["budgetLeft"], b["policy"]["budget"])
        s["policy"] = b["policy"]
        for eff in s["effects"].values():
            if eff["phase"] == "awaiting-approval" and approvals_complete(s, eff):
                eff["phase"] = "ready"
    elif kind == "task":
        parent = s["tasks"].get(b.get("parent")) if b.get("parent") else None
        t = {"author": author, "to": b["to"], "open": True, "state": "pending", "cancelRequested": False, "depth": parent["depth"] + 1 if parent else 0,
             "seq": rc["seq"]}
        if parent: t["parent"] = b["parent"]
        for x in b["to"]: instruct(s, ctx, x, author)
        if "budget" in b:
            _debit(s, task_source(s, b.get("parent")), b["budget"]["amount"])
            t["budget"] = dict(b["budget"]); t["left"] = dict(b["budget"])
        s["tasks"][eid] = t
    elif kind == "task.update":
        t = s["tasks"][b["task"]]
        t["state"] = b["state"]
        if b["state"] in TASK_FINAL:
            t["open"] = False
            if t.get("left"):
                amount = t["left"]["amount"]
                t["left"] = {"unit": t["left"]["unit"], "amount": 0}
                _credit(s, t.get("parent"), amount)
    elif kind == "task.cancel":
        s["tasks"][b["task"]]["cancelRequested"] = True
    elif kind == "effect.propose":
        eff = {"proposer": author, "proposerKey": e["keyId"], "executor": b["executor"], "action": b["action"],
               "planDigest": b["planDigest"], "repeatable": b["repeatable"], "cancelRequested": False,
               "approvals": [], "claims": [], "seq": rc["seq"],
               "phase": "awaiting-approval" if approval_rule(s["policy"], b["action"]) else "ready"}
        if "cost" in b: eff["cost"] = dict(b["cost"])
        if "task" in b: eff["task"] = b["task"]
        if "expiresAt" in b: eff["expiresAt"] = b["expiresAt"]
        if author in s["held"]: add_guard(s, eff, s["held"][author])
        g = instruct(s, ctx, b["executor"], author)
        if g: add_guard(s, eff, g)                                           # aimed at a guarded or forced executor: it needs g
        s["effects"][eid] = eff
    elif kind == "effect.approve":
        eff = s["effects"][b["proposal"]]
        eff["approvals"] = [old for old in eff["approvals"]       # a new approval supersedes (and drops) the author's open ones
                            if not (old["author"] == author and not old.get("consumedBy"))]
        a = {"id": eid, "author": author, "keyId": e["keyId"]}
        if "expiresAt" in b: a["expiresAt"] = b["expiresAt"]
        eff["approvals"].append(a)
        if eff["phase"] == "awaiting-approval" and approvals_complete(s, eff): eff["phase"] = "ready"
    elif kind == "effect.claim":
        eff = s["effects"][b["proposal"]]
        c = {"id": eid, "attempt": b["attempt"], "instance": b["instance"], "time": rc["time"], "window": b["window"], "settled": False}
        chosen = []
        if approval_rule(s["policy"], eff["action"]):
            _, a = pick_approval(s, eff, ctx)
            chosen.append(a); c["approval"] = a["id"]
        for g in guards_of(s, eff):
            a = next((x for x in chosen if x["author"] == g), None) or guard_approval(s, eff, g, ctx)[1]
            if a is None: continue                           # unreachable when decide allowed the claim
            if a not in chosen: chosen.append(a)
            c.setdefault("guardApprovals", []).append(a["id"])
        for a in chosen: a["consumedBy"] = eid
        if space_unit(s):
            src = effect_source(s, eff)
            _debit(s, src, eff["cost"]["amount"])
            c["reserved"] = dict(eff["cost"]); c["source"] = list(src)
        eff["claims"].append(c); eff["phase"] = "claimed"
    elif kind == "effect.cancel":
        eff = s["effects"][b["proposal"]]
        if eff["phase"] in ("awaiting-approval", "ready"):
            eff["phase"] = "cancelled"
        eff["cancelRequested"] = True
    elif kind == "effect.outcome":
        pid, eff, c = find_claim(s, b["claim"])
        c["outcome"], c["started"] = b["result"], b["started"]
        if b["started"] == "no":
            _settle(s, c, 0)
            back = set(c.get("guardApprovals", [])) | ({c["approval"]} if c.get("approval") else set())
            gone = []
            for a in eff["approvals"]:
                if a["id"] in back:
                    a.pop("consumedBy", None)
                    if any(n["author"] == a["author"] and not n.get("consumedBy") and n is not a and n not in gone
                           and eff["approvals"].index(n) > eff["approvals"].index(a) for n in eff["approvals"]):
                        gone.append(a)                         # superseded while it was consumed
            eff["approvals"] = [a for a in eff["approvals"] if a not in gone]
            eff["phase"] = rearm(s, eff)
        elif b["result"] == "unknown":
            if eff["repeatable"] and not eff["cancelRequested"] and author == eff["executor"] and e["keyId"] == c["instance"]:
                _settle(s, c, None)                    # charged in full, then retry
                eff["phase"] = rearm(s, eff)
            else:
                eff["phase"] = "unknown"
        else:
            _settle(s, c, b["cost"]["amount"] if "cost" in b else None)
            if b["result"] == "succeeded": eff["phase"] = "succeeded"
            elif eff["repeatable"] and not eff["cancelRequested"]: eff["phase"] = rearm(s, eff)
            else: eff["phase"] = "failed"
        prune_approvals(eff)
    elif kind == "effect.resolve":
        eff = s["effects"][b["proposal"]]
        c = latest_claim(eff)
        if b["result"] == "succeeded":
            _settle(s, c, b["cost"]["amount"] if "cost" in b else None)
            eff["phase"] = "succeeded"
        else:
            _settle(s, c, 0)
            eff["approvals"] = [a for a in eff["approvals"] if a.get("consumedBy")]
            eff["phase"] = rearm(s, eff)
        prune_approvals(eff)
    elif kind == "space.move":
        s["home"] = b["to"]
    elif kind == "session.open":
        s["sessions"][eid] = {"actor": author, "opened": rc["time"], "steers": 0, "steerMode": b["steerMode"],
                              "human": is_human(ctx, author)}
        if not is_human(ctx, author):
            p = ctx["profiles"][author]
            if p.get("operatorConfirmed") and p.get("operator"): s["sessions"][eid]["guard"] = p["operator"]
            if b["steerMode"] == "guarded":
                s["guarded"][author] = p["operator"]                            # sticky: only its guard unguards it
        if "task" in b: s["sessions"][eid]["task"] = b["task"]
    elif kind == "session.close":
        del s["sessions"][b["session"]]                                          # F3(b): closed sessions leave the state
    elif kind == "session.input":
        sess = s["sessions"][b["session"]]
        sess["steers"] += 1
        if instruct(s, ctx, sess["actor"], author): s["outside"][sess["actor"]] = rc["seq"]
    elif kind == "agent.release":
        x = b["agent"]
        s["held"].pop(x, None); s["cleared"][x] = max(s["cleared"].get(x, 0), e["seen"])   # reviewed: what the guard had seen (never less than before)
        if b.get("unguard"): s["guarded"].pop(x, None)                           # only the guard loosens its agent
    settle_holds(s)
    return s

def prune_approvals(eff):
    """F3(a): a consumed approval goes once its claim has settled (dropped ones are removed when they are dropped)."""
    done = {c["id"] for c in eff["claims"] if c.get("settled")}
    eff["approvals"] = [a for a in eff["approvals"] if a.get("consumedBy") not in done]

def _debit(s, src, amount):
    if src[0] == "task": s["tasks"][src[1]]["left"]["amount"] -= amount
    else: s["budgetLeft"]["amount"] -= amount

def _settle(s, claim, spent):
    """Settle a claim once (D14: `settled` is on every claim): spent=None → charge the reservation in full; spent=n → refund reserved−n (P-06).
    A claim without a reservation is marked settled and nothing moves."""
    if not claim or claim.get("settled"): return
    claim["settled"] = True
    if not claim.get("reserved"): return
    refund = 0 if spent is None else claim["reserved"]["amount"] - spent
    if refund: _credit(s, claim["source"][1] if claim["source"][0] == "task" else None, refund)

def _credit(s, task_id, amount):
    """Return `amount` to the nearest open budgeted task from task_id upward, else to the space (P-05, P-06)."""
    while task_id:
        t = s["tasks"].get(task_id)
        if t and t["open"] and t.get("left"):
            t["left"]["amount"] += amount; return
        task_id = t.get("parent") if t else None
    if s["budgetLeft"]: s["budgetLeft"]["amount"] += amount

def _fold_create(e, rc, ctx, eid):
    b, author, space = e["body"], e["author"], e["space"]
    base = {"space": space, "head": rc["seq"], "home": server_of(space), "policy": b["policy"], "roles": {}, "roleHistory": {},
            "tasks": {}, "effects": {}, "sessions": {}, "held": {}, "guarded": {}, "cleared": {}, "outside": {}, "budgetLeft": dict(b["policy"]["budget"]) if b["policy"]["budget"] else None,
            "entries": {eid: {"author": author, "kind": "space.create"}}}
    if space.startswith("profile:"):
        actor = space[len("profile:"):]
        p = b["profile"]
        base["roles"] = {actor: "owner", author: "owner"}
        base["profile"] = {"actorKind": p["actorKind"], "operator": p.get("operator"), "operatorConfirmed": False,
                           "keys": {p["key"]["keyId"]: {"publicKey": p["key"]["publicKey"], "status": "valid"}}}
        return base
    base["roles"] = {author: "owner"}                                      # plain or fork (P-13): fresh
    base["roleHistory"] = {author: [{"from": rc["seq"]}]}
    return base

# ───────────────────────── read rights (P-02) ─────────────────────────
def can_read(state, reader, seq) -> bool:
    if state["policy"]["visibility"] == "public" or state["space"].startswith("profile:"): return True
    def held(actor):
        if state["roles"].get(actor) in READ_ROLES: return True
        return any(h["from"] <= seq and ("to" not in h or seq < h["to"]) for h in state["roleHistory"].get(actor, []))
    if reader.startswith("@:"):
        srv = reader[2:]
        if srv == state["home"]: return True                                   # the home always reads its own space
        if srv in state["policy"]["servers"]["deny"]: return False
        return any(server_of(a) == srv and held(a) for a in set(state["roles"]) | set(state["roleHistory"]))
    return held(reader)

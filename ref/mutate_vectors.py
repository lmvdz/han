"""Shows the conformance vectors have teeth: each mutation breaks one Policy rule; the vectors MUST then fail.
Runs against a patched copy in memory; ref/policy.py on disk is never changed."""
import io, os, re, sys, types, contextlib, importlib.util
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
try:
    sys.stdout.reconfigure(encoding="utf-8")
except Exception:
    pass

MUTATIONS = [  # (rule, description, exact text in policy.py, replacement)
    ("P-07 E1", "claim by any actor", 'if author != eff["executor"] or b["instance"] != entry["keyId"]: return deny("E1 not the named executor instance")', "pass"),
    ("P-07 E1", "removed executor may claim", 'if role not in ("owner", "member"): return deny("E1 executor no longer a member")', "pass"),
    ("P-07 E2", "ignore proposer revocation", 'return deny("E2 proposer no longer authorized")', "pass"),
    ("P-07 E3", "approval not required", 'if res == "none": return deny("E3 no usable approval")', "pass"),
    ("P-07 E5", "ignore attempt numbering", 'if b["attempt"] != len(eff["claims"]) + 1: return deny("E5 attempt number")', "pass"),
    ("P-07 E6", "ignore budget", 'if remaining(state, effect_source(state, eff)) < eff["cost"]["amount"]: return deny("E6 budget")', "pass"),
    ("P-08", "self-approval allowed", 'if not is_guard and state["policy"]["separateApprover"] and author == eff["proposer"]: return deny("separate approver required")', "pass"),
    ("P-08", "expired approvals count", 'if time is not None and a.get("expiresAt") and ts(a["expiresAt"]) <= ts(time): continue', "pass"),
    ("P-09", "not-started keeps approval consumed", 'if a["id"] in back: a.pop("consumedBy", None)', "pass"),
    ("P-09", "inconsistent result/started accepted", 'if (b["result"], b["started"]) not in VALID_PAIRS: return invalid("inconsistent result/started")', "pass"),
    ("P-10", "anyone resolves", 'if author != eff["executor"] and role != "owner": return deny("executor or owner only")', "pass"),
    ("P-06", "uncosted effects in budgeted space", 'if unit and ("cost" not in b or b["cost"]["unit"] != unit): return deny("budgeted space: cost in the space\'s unit required")', "pass"),
    ("P-06", "agents may charge the space budget", 'if b.get("task") not in mine: return deny("agents spend only from a budgeted task assigned to them")', "pass"),
    ("P-06", "unreserved costs accepted", '    return space_unit(state) is None', '    return True'),
    ("P-06", "budget unit may change while reserved", '    return same or not has_reservations(state)', '    return True'),
    ("P-03", "members may demote owners", 'if role == "member" and pol["membersMayInvite"] and cur is None and new in ("guest", "member"): return ALLOW',
              'if role == "member" and pol["membersMayInvite"] and new in ("guest", "member"): return ALLOW'),
    ("P-03", "last owner may leave", 'if cur == "owner" and new != "owner" and len(owners) == 1: return deny("would leave zero owners")', "pass"),
    ("P-03", "agents unchecked", 'if pol["allowAgents"] == "with-operator" and not prof.get("operatorConfirmed"): return deny("agent needs confirmed operator")', "pass"),
    ("P-03", "banned may self-leave", 'if author == tgt and new == "none" and cur not in (None, "banned"): return ALLOW', 'if author == tgt and new == "none" and cur is not None: return ALLOW'),
    ("P-04", "removed actors keep task authority", 'def _task_update(state, entry, ctx, role):\n    t = state["tasks"].get(entry["body"]["task"])\n    if not t: return invalid("no such task")\n    if not t["open"]: return deny("task closed")\n    if role not in READ_ROLES: return deny("author holds no role")',
              'def _task_update(state, entry, ctx, role):\n    t = state["tasks"].get(entry["body"]["task"])\n    if not t: return invalid("no such task")\n    if not t["open"]: return deny("task closed")'),
    ("P-05", "owners may report progress", 'if role == "owner" and final and entry["author"] not in t["to"]: return ALLOW', 'if role == "owner" and entry["author"] not in t["to"]: return ALLOW'),
    ("P-05", "delegation depth unchecked", 'if parent["depth"] + 1 > state["policy"]["maxDelegationDepth"]: return deny("delegation too deep")', "pass"),
    ("P-05", "no refund on close", '_credit(s, t.get("parent"), amount)', "pass"),
    ("P-05", "child budget always from parent", '    return ("task", parent_id) if p and p.get("left") else ("space", None)', '    return ("task", parent_id) if p else ("space", None)'),
    ("P-12", "anyone redacts", 'return ALLOW if (entry["author"] == t["author"] or role == "owner") else deny("author or owner only")', "return ALLOW"),
    ("P-12", "live plans redactable", 'return deny("the plan of an unfinished effect can\'t be redacted; cancel it first")', "pass"),
    ("P-13", "fork inherits members", '    base["roles"] = {author: "owner"}                                      # plain or fork (P-13): fresh',
              '    base["roles"] = {author: "owner", "@m:a.example": "member"}'),
    ("P-11", "move while claimed", 'if any(e["phase"] == "claimed" for e in state["effects"].values()): return deny("cannot move while an effect is claimed")', "pass"),
    ("P-11", "policy.set leaves approvals stuck", 'if eff["phase"] == "awaiting-approval" and approvals_complete(s, eff):\n                eff["phase"] = "ready"',
              'if False:\n                pass'),
    ("P-02", "former members keep reading", 'return any(h["from"] <= seq and ("to" not in h or seq < h["to"]) for h in state["roleHistory"].get(actor, []))',
              'return any(h["from"] <= seq for h in state["roleHistory"].get(actor, []))'),
    ("§4.3", "keyIds reusable", 'if kind == "key.add" and b["keyId"] in keys: return invalid("keyIds are single-use")', "pass"),
    ("§4.3", "agents may operate agents", 'if ctx["profiles"][op]["actorKind"] != "human": return deny("an operator must be a human actor")', "pass"),
    ("§4.3", "self-operation allowed", 'if b["profile"].get("operator") == actor: return invalid("an actor can\'t be its own operator")', "pass"),
    ("R-05", "dangling refs accepted", 'if r not in state["entries"]: return invalid("ref not in space")', "pass"),
    ("R-09", "unknown fields accepted", 'if f not in sch: return f"unknown body field {f}"', "pass"),
    ("R-12", "revoked author keys accepted", 'if key_status(ctx, author, entry["keyId"]) != "valid": return deny("author key not valid")', "pass"),
    ("§2", "JCS sorts by code point", 'keys = sorted(v, key=lambda k: k.encode("utf-16-be"))', 'keys = sorted(v)'),
    ("P-09", "abandon any time", 'if ts(ctx["time"]) >= ts(c["time"]) + abandon_after(c["window"]): return ALLOW', "return ALLOW"),
    ("P-09", "abandon uses the current window", 'if ts(ctx["time"]) >= ts(c["time"]) + abandon_after(c["window"]): return ALLOW',
              'if ts(ctx["time"]) >= ts(c["time"]) + abandon_after(state["policy"]["launchWindow"]): return ALLOW'),
    ("P-09", "no rounding/drift margin", '    return -(-num // den) + 2', '    return window'),
    ("P-09", "margin covers one-sided drift only", '    return -(-num // den) + 2', '    return window + 1 + -(-window // 100)'),
    ("P-07 E5", "expired proposals claimable", 'if eff.get("expiresAt") and ts(eff["expiresAt"]) <= ts(ctx["time"]): return deny("E5 proposal expired")', "pass"),
    ("P-03", "denied servers' users can't be removed", 'if new in READ_ROLES and server_of(tgt) in pol["servers"]["deny"]: return deny("target server denied")',
              'if server_of(tgt) in pol["servers"]["deny"]: return deny("target server denied")'),
    ("P-04", "denied executor can't report facts", 'if server_of(author) in state["policy"]["servers"]["deny"] and kind not in ("effect.outcome", "effect.resolve"):',
              'if server_of(author) in state["policy"]["servers"]["deny"]:'),
    ("P-05", "agents may self-fund", 'if b.get("parent") not in _budgeted_tasks_of(state, author): return deny("agents draw budget only beneath a budgeted task assigned to them")', "pass"),
    ("P-03", "operator withdrawal ignored", '    return state["policy"]["allowAgents"] != "with-operator" or bool(p.get("operatorConfirmed"))', '    return True'),
    ("P-05", "top-level tasks have depth 1", '"depth": parent["depth"] + 1 if parent else 0}', '"depth": parent["depth"] + 1 if parent else 1}'),
    ("P-02", "home server can't read its space", '        if srv == state["home"]: return True                                   # the home always reads its own space', '        pass'),
    ("§5", "extension role ignored", 'return ALLOW if role in READ_ROLES[:READ_ROLES.index(need) + 1] else deny(f"extensions need role {need} or higher")', 'return ALLOW if role in READ_ROLES else deny("x")'),
    ("R-09", "floats in opaque Json", '        return None if json_ok(body) else "extension body: only non-negative integers, no floats"', '        return None'),
    ("R-09", "impossible dates accepted", '        datetime.strptime(v, "%Y-%m-%dT%H:%M:%SZ"); return True       # a real proleptic Gregorian date, seconds 00–59', '        return True'),
    ("P-07 E2", "proposer accountability unchecked", 'if not _accountable(state, ctx, eff["proposer"]): return deny("E2 proposer\'s operator confirmation withdrawn")', "pass"),
    ("P-07 E5", "claim window unchecked", 'if b["window"] != state["policy"]["launchWindow"]: return deny("E5 claim\'s window is not the space\'s launchWindow")', "pass"),
    ("P-03", "agents may be owners", 'if new == "owner" and ctx["profiles"][tgt]["actorKind"] != "human": return deny("owners must be human")', "pass"),
    ("P-03", "allowAgents none ignored for members", '    if state["policy"]["allowAgents"] == "none": return False', '    pass'),
    ("P-11", "home server may be denied", 'if state["home"] in deny_list: return deny("can\'t deny the home\'s own server")', "pass"),
    ("P-05", "close over running effects", '            return deny("task has effects still running or of unknown outcome")', '            pass'),
    ("P-09", "abandon re-readies repeatables", 'if eff["repeatable"] and not eff["cancelRequested"] and author == eff["executor"]:', 'if eff["repeatable"] and not eff["cancelRequested"]:'),
    ("P-08", "unbounded open approvals", '        return deny("too many open approvals")', '        pass'),
    ("R-05", "missing parent only denied", 'if not parent: return invalid("no such parent task")', 'if not parent: return deny("no such parent task")'),
    ('P-15', 'anyone may steer', 'if not session_access(state, entry["author"], entry["body"]["session"])["steer"]: return deny("no steer right on this session")', 'pass'),
    ('P-15', 'session rights ignore role order', 'READ_ROLES.index(role) <= READ_ROLES.index(need)', 'READ_ROLES.index(role) >= READ_ROLES.index(need)'),
    ('P-15', 'nobody grants to everyone', 'out[right] = need != "nobody" and role in READ_ROLES', 'out[right] = role in READ_ROLES'),
    ('P-15', 'anyone closes sessions', 'if entry["author"] != sess["actor"] and role != "owner": return deny("only the session\'s actor or an owner closes it")', 'pass'),
    ('P-15', 'dead sessions stay live', 'return bool(sess) and sess["open"] and role_of(state, sess["actor"]) in READ_ROLES', 'return bool(sess) and sess["open"]'),
    ('P-15', 'read wider than watch allowed', '    if L(ss["watch"]) > L(ss["read"]) or L(ss["watch"]) > L(ss["steer"]): return False   # read and steer need watch', '    pass'),
    ('P-15', 'any steer mode opens', '    if b["steerMode"] not in state["policy"]["sessions"]["modes"]: return deny("steer mode not allowed in this space")', '    pass'),
    ('P-15', 'guarded without operator', '            return deny("a guarded agent session needs a confirmed operator who holds a role here")', '            pass'),
    ('P-15', "sessions on others' tasks", 'if author not in task["to"] and role != "owner": return deny("only an assignee or owner works a task")', 'pass'),
    ('P-15', 'guards not required at claim', '        if res == "none": return deny("E3 a guard hasn\'t approved")', '        pass'),
    ('P-15', 'guard needs no role', '    if role_of(state, g) not in READ_ROLES: return "none", None', '    pass'),
    ('P-15', 'guard approval ignores expiry', '        if a.get("expiresAt") and ts(a["expiresAt"]) <= ts(ctx["time"]): continue\n        st = key_status(ctx, g, a["keyId"])', '        st = key_status(ctx, g, a["keyId"])'),
    ('P-15', 'guard approval ignores keys', '        if st == "valid": return "ok", a\n    return "none", None\n\ndef live', '        return "ok", a\n    return "none", None\n\ndef live'),
    ('P-15', 'relays drop taint', 'add = set(s["taint"].get(author, []))', 'add = set()'),
    ('P-15', 'auto taints', '            if sess["steerMode"] != "auto": add.add(sess["guard"])', '            add.add(sess["guard"])'),
    ('P-15', 'suggest does not taint', '            if sess["steerMode"] != "auto": add.add(sess["guard"])', '            if sess["steerMode"] == "guarded": add.add(sess["guard"])'),
    ('P-15', 'self input clears taint', '        if author == x: pass                                     # own input: no change', '        if author == x: s["taint"].pop(x, None)'),
    ('P-15', 'guard input does not clear', '        elif author == sess.get("guard"): s["taint"].pop(x, None)  # its human takes the wheel back: taint cleared', '        elif author == sess.get("guard"): pass'),
    ('P-15', 'tasks drop taint', '        if tg: t["guards"] = tg', '        pass'),
    ('P-15', 'tasks taint ignored by proposals', '    if task_id and task_id in state["tasks"]: g |= set(state["tasks"][task_id].get("guards", []))', '    pass'),
    ('P-15', 'guards cannot refuse', 'or a in eff.get("guards", []) or any(', 'or any('),
    ('P-15', 'guard approve without role', '    if is_guard and role not in READ_ROLES: return deny("guard holds no role")', '    pass'),
]

def load_mutant(old, new):
    src = open(os.path.join(HERE, "policy.py"), encoding="utf-8").read()
    if src.count(old) != 1: raise SystemExit(f"mutation anchor not found exactly once: {old[:70]}")
    mod = types.ModuleType("policy")
    exec(compile(src.replace(old, new), "policy_mutant.py", "exec"), mod.__dict__)
    return mod

def main():
    import run_vectors
    missed = 0
    for rule, desc, old, new in MUTATIONS:
        run_vectors.P = load_mutant(old, new)
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            rc = run_vectors.main()
        summary = buf.getvalue().strip().splitlines()[-1]
        caught = rc != 0
        missed += not caught
        print(f"{'caught' if caught else 'MISSED'}  {rule:8} {desc:38} {summary}")
    print(f"{len(MUTATIONS) - missed}/{len(MUTATIONS)} policy mutations caught by the vectors")
    return 1 if missed else 0

if __name__ == "__main__":
    sys.exit(main())

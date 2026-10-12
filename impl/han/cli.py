"""han: the command line over han.client. Argument parsing and printing only; every command is one or two Client calls.

  python -m han.cli [--url ws://…/han] [--actor @me:S] [--key KEYFILE] [--outbox DB] <command> …
(HAN_URL, HAN_ACTOR, HAN_KEY, HAN_OUTBOX are the defaults. Keys: ~/.han/keys/<actor>.json; outbox: ~/.han/outbox/<actor>.db.)

  keygen [--out PATH]                       space create SP [--name N] [--policy FILE] [--set FIELD=JSON]…
  member set SP ACTOR ROLE                  message SP TEXT [--to ACTOR]…
  task SP ASK --to ACTOR… [--deadline T]    propose SP --executor A --plan FILE [--repeatable] [--cost UNIT=N]
  approve SP PROPOSAL [--note T]            cancel SP ID [--task]
  session open SP [--mode auto|guarded]     session steer SP SESSION TEXT      session close SP SESSION
  agent release SP AGENT [--unguard]        (the guard's release; `seen` is the head this client has read, as `status` shows it)
  watch SP SESSION [--count N]              read SP [--after N] [--follow] [--json]        status SP
Ids may be given as a unique prefix (8+ hex digits)."""
from __future__ import annotations
import argparse, asyncio, hashlib, json, os, sys
from . import keys
from .client import Client, default_policy
from .core import P, W, WireError

# ── helpers ──
def out(*a): print(*a, flush=True)
def die(msg, code=1): print(f"han: {msg}", file=sys.stderr); return code

def load_json(path):
    with open(path, encoding="utf-8") as fh: return json.load(fh)

async def records(c, space, after=0):
    """Every readable record of `space` after `after` (this also tells the client the head, so `seen` is right: R-17)."""
    out_ = []
    while True:
        page = [i["record"] for i in await c.read_all(space, after) if i["type"] == "record"]
        out_ += page
        if len(page) < 1000: return out_
        after = page[-1]["receipt"]["seq"]

async def resolve(c, space, ref, recs=None):
    if P._id(ref): return ref
    if len(ref) < 8: raise ValueError(f"id prefix too short: {ref}")
    hits = [r["receipt"]["id"] for r in (recs if recs is not None else await records(c, space)) if r["receipt"]["id"].startswith(ref)]
    if len(hits) != 1: raise ValueError(f"id prefix {ref} matches {len(hits)} records")
    return hits[0]

async def put(c, kind, space, body, content=None, refs=()):
    r = await c.append(*c.make_entry(kind, space, body, content, refs))
    out(f"seq {r['seq']} id {r['id']}")
    return r

def summary(rec) -> str:
    e, ct = rec["entry"], rec.get("content") or {}
    b = e["body"]
    if rec.get("redacted"): what = "[redacted]"
    elif "text" in ct: what = ct["text"]
    elif "ask" in ct: what = f"{ct['ask']}  -> {', '.join(b['to'])}"
    elif "plan" in ct: what = f"{ct['plan']['action']} {ct['plan']['target']}  executor {b['executor']}"
    elif "name" in ct: what = ct["name"]
    else: what = " ".join(f"{k}={v}" for k, v in b.items() if k not in ("policy", "accept", "planDigest"))
    return f"#{rec['receipt']['seq']} {e['author']} {e['kind']} {rec['receipt']['id'][:8]}  {what}"

# ── commands ──
def cmd_keygen(a) -> int:
    path = a.out or (keys.default_key_path(a.actor) if a.actor else None)
    if not path: return die("keygen needs --out PATH or --actor")
    if os.path.exists(path) and not a.force: return die(f"{path} exists (use --force to replace it)")
    kp = keys.generate(path, a.key_id)
    out(f"wrote {path}\nkeyId {kp.key_id}\npublic {kp.pub}")
    out(f"register it with the server: python -m han.seq register --actor {a.actor or '@you:SERVER'} --pub {path} …")
    return 0

async def cmd_space_create(c, a):
    pol = load_json(a.policy) if a.policy else default_policy()
    for kv in a.set:
        k, _, v = kv.partition("=")
        try: pol[k] = json.loads(v)
        except ValueError: pol[k] = v
    await put(c, "space.create", a.space, {"policy": pol}, {"name": a.name or a.space})

async def cmd_member_set(c, a): await put(c, "member.set", a.space, {"actor": a.member, "role": a.role})

async def cmd_message(c, a): await put(c, "message", a.space, {"to": a.to} if a.to else {}, {"text": a.text})

async def cmd_task(c, a):
    body = {"to": a.to} | ({"deadline": a.deadline} if a.deadline else {}) | ({"parent": await resolve(c, a.space, a.parent)} if a.parent else {})
    if a.budget: unit, _, n = a.budget.partition("="); body["budget"] = {"unit": unit, "amount": int(n)}
    await put(c, "task", a.space, body, {"ask": a.ask})

async def cmd_propose(c, a):
    plan = load_json(a.plan)
    if not W._plan(plan): return die("the plan file is not a plan: {action, target, args?, argsBlob?}")
    body = {"executor": a.executor, "action": plan["action"], "planDigest": hashlib.sha256(P.jcs(plan)).hexdigest(), "repeatable": a.repeatable}
    if a.cost: unit, _, n = a.cost.partition("="); body["cost"] = {"unit": unit, "amount": int(n)}
    if a.task: body["task"] = await resolve(c, a.space, a.task)
    if a.expires: body["expiresAt"] = a.expires
    await put(c, "effect.propose", a.space, body, {"plan": plan})

async def cmd_approve(c, a):
    """P-08: show the plan and check its hash before signing an approval for it."""
    recs = await records(c, a.space)
    pid = await resolve(c, a.space, a.proposal, recs)
    rec = next((r for r in recs if r["receipt"]["id"] == pid and r["entry"]["kind"] == "effect.propose"), None)
    if not rec: return die(f"{a.proposal} is not an effect.propose in {a.space}")
    plan = (rec.get("content") or {}).get("plan")
    digest = rec["entry"]["body"]["planDigest"]
    if plan is None: return die("the plan is not available (redacted?); not approving")
    if hashlib.sha256(P.jcs(plan)).hexdigest() != digest: return die("the plan does not match its planDigest; not approving")
    out(f"approving {pid[:8]} by {rec['entry']['author']} (executor {rec['entry']['body']['executor']}), plan {digest[:12]}:\n{json.dumps(plan, indent=2)}")
    await put(c, "effect.approve", a.space, {"proposal": pid, "planDigest": digest} | ({"expiresAt": a.expires} if a.expires else {}),
              {"note": a.note} if a.note else None)

async def cmd_cancel(c, a):
    tid = await resolve(c, a.space, a.id)
    await put(c, "task.cancel" if a.task else "effect.cancel", a.space, {"task": tid} if a.task else {"proposal": tid})

async def cmd_session(c, a):
    if a.action == "open":
        body = {"steerMode": a.mode} | ({"task": await resolve(c, a.space, a.task)} if a.task else {})
        await put(c, "session.open", a.space, body, {"title": a.title} if a.title else None)
        return
    recs = await records(c, a.space)                                  # also sets `seen` to what we have read (R-17, P-15 release)
    sid = await resolve(c, a.space, a.session, recs)
    if a.action == "steer": await put(c, "session.input", a.space, {"session": sid}, {"text": a.text})
    else: await put(c, "session.close", a.space, {"session": sid})

async def cmd_agent(c, a):
    """F2: `agent.release {agent, unguard?}`, authored by the guard; `seen` is the head it has read (R-17), or --seen."""
    if not P._actor(a.agent): return die(f"not an actor id: {a.agent}")
    await records(c, a.space)                                         # reading sets `seen` to the head (R-17, P-15)
    body = {"agent": a.agent} | ({"unguard": True} if a.unguard else {})       # E1: only the guard's `unguard` ends a guarded choice
    e, ct = c.make_entry("agent.release", a.space, body, seen=a.seen)           # no content (§5)
    r = await c.append(e, ct); out(f"seq {r['seq']} id {r['id']}")

async def cmd_watch(c, a):
    sid = await resolve(c, a.space, a.session)
    n = 0
    async def run():
        nonlocal n
        async for it in c.watch(a.space, sid):
            out(json.dumps(it) if a.json else f"[{it['n']}] {it['type']}: " + (it.get("text") or it.get("state") or f"{it['what']} {it['blob']['hash'][:12]}"))
            n += 1
            if a.count and n >= a.count: return
    try: await asyncio.wait_for(run(), a.timeout or None)
    except asyncio.TimeoutError: pass

async def cmd_read(c, a):
    async def show(rec): out(json.dumps(rec) if a.json else summary(rec))
    if a.follow:
        async for it in c.read(a.space, a.after, follow=True):
            if it["type"] == "record": await show(it["record"])
    else:
        for rec in await records(c, a.space, a.after): await show(rec)

async def agent_lines(c, st):
    """F1, H8: per agent, `P.guard_status` (the one guard function) on the current state: `guarded` with its guard, or `forced` (the space forces
    guarding, so an instruction to a non-human is held for the operator in the profile pinned then; the profile read here is the current one, which
    `guardStatus` needs only in a forcing space)."""
    named = set(st["guarded"]) | set(st["held"]) | {s["actor"] for s in st["sessions"].values()}
    named |= {x for t in st["tasks"].values() for x in t["to"]} | {e["executor"] for e in st["effects"].values()}
    forcing, lines = bool(st["policy"]["sessions"].get("forceGuarded")), []
    for x in sorted(named):
        prof = None
        if forcing and not x.startswith("@:"):
            try: prof = (await c.read_state("profile:" + x))["profile"]
            except WireError: continue
        gs = P.guard_status(st, {"time": "", "profiles": {x: prof} if prof else {}}, x)
        held = "  held" if gs["held"] else ""
        if gs["forced"]:
            lines.append(f"  {x}  forced (operator {prof.get('operator')}{'' if prof.get('operatorConfirmed') else ', unconfirmed: instructions are denied'})" + held)
        elif gs.get("guard"): lines.append(f"  {x}  guarded (guard {gs['guard']})" + held)
    if lines: out("agents:"); [out(l) for l in lines]

async def cmd_status(c, a):
    try: st = await c.read_state(a.space)
    except WireError: return die("no state (needs read rights at the head)")
    pol = st["policy"]
    out(f"{a.space}  head {st['head']}  home {st['home']}  visibility {pol['visibility']}  join {pol['join']}")
    out("members: " + (", ".join(f"{x} {r}" for x, r in sorted(st["roles"].items()) if r != "none") or "none"))
    out("tasks:")
    for tid, t in st["tasks"].items(): out(f"  {tid[:8]} {'open' if t['open'] else 'closed'}  to {', '.join(t['to'])}")
    out("effects:")
    for eid, e in st["effects"].items():
        g = P.guards_of(st, e)
        out(f"  {eid[:8]} {e['phase']}  {e['action']}  proposer {e['proposer']}  executor {e['executor']}  approvals {sum(1 for x in e['approvals'] if not x.get('dropped'))}"
            + (f"  guards {', '.join(g)}" if g else "") + ("  cancel-requested" if e["cancelRequested"] else ""))
    out("sessions:")
    for sid, s in st["sessions"].items():
        out(f"  {sid[:8]} {s['actor']}  {'open' if P.live(st, s) else 'not live'}  {s['steerMode']}" + (f"  guard {s['guard']}" if s.get("guard") else ""))
    await agent_lines(c, st)
    out("held agents: " + (", ".join(f"{x} (for {g})" for x, g in sorted(st["held"].items())) or "none"))
    out("guarded agents: " + (", ".join(f"{x} (guard {g})" for x, g in sorted(st["guarded"].items())) or "none"))

# ── parsing ──
def parser() -> argparse.ArgumentParser:
    env = os.environ.get
    ap = argparse.ArgumentParser(prog="han", description=__doc__.split("\n")[0])
    ap.add_argument("--url", default=env("HAN_URL")); ap.add_argument("--actor", default=env("HAN_ACTOR"))
    ap.add_argument("--key", default=env("HAN_KEY")); ap.add_argument("--outbox", default=env("HAN_OUTBOX"))
    ap.add_argument("--server", help="the server name to dial (default: the actor's)")
    sub = ap.add_subparsers(dest="cmd", required=True)
    def cmd(name, fn, *args, parent=sub, **kw):
        p = parent.add_parser(name, **kw); p.set_defaults(fn=fn)
        for a in args:
            flags, opts = a if isinstance(a, tuple) else (a, {})
            p.add_argument(*([flags] if isinstance(flags, str) else flags), **opts)
        return p
    cmd("keygen", cmd_keygen, ("--out", {}), ("--key-id", {"default": "k1"}), ("--force", {"action": "store_true"}))
    sp = sub.add_parser("space").add_subparsers(dest="sub", required=True)
    cmd("create", cmd_space_create, "space", ("--name", {}), ("--policy", {"help": "PolicyDoc JSON file"}), ("--set", {"action": "append", "default": [], "metavar": "FIELD=JSON"}), parent=sp)
    mb = sub.add_parser("member").add_subparsers(dest="sub", required=True)
    cmd("set", cmd_member_set, "space", "member", ("role", {"choices": ["owner", "member", "guest", "banned", "none"]}), parent=mb)
    cmd("message", cmd_message, "space", "text", ("--to", {"action": "append"}))
    cmd("task", cmd_task, "space", "ask", ("--to", {"action": "append", "required": True}), ("--deadline", {}), ("--parent", {}), ("--budget", {"metavar": "UNIT=N"}))
    cmd("propose", cmd_propose, "space", ("--executor", {"required": True}), ("--plan", {"required": True, "help": "JSON file {action, target, args?}"}),
        ("--repeatable", {"action": "store_true"}), ("--cost", {"metavar": "UNIT=N"}), ("--task", {}), ("--expires", {}))
    cmd("approve", cmd_approve, "space", "proposal", ("--note", {}), ("--expires", {}))
    cmd("cancel", cmd_cancel, "space", "id", ("--task", {"action": "store_true", "help": "ID is a task (task.cancel), not an effect"}))
    ss = sub.add_parser("session").add_subparsers(dest="action", required=True)
    cmd("open", cmd_session, "space", ("--mode", {"choices": ["auto", "guarded"], "default": "auto"}), ("--task", {}), ("--title", {}), parent=ss)
    cmd("steer", cmd_session, "space", "session", "text", parent=ss)
    cmd("close", cmd_session, "space", "session", parent=ss)
    ag = sub.add_parser("agent").add_subparsers(dest="action", required=True)
    cmd("release", cmd_agent, "space", "agent", ("--seen", {"type": int, "help": "what the guard has seen (default: the head)"}),
        ("--unguard", {"action": "store_true", "help": "also end the agent's guarded state (the guard only; closing sessions never does)"}), parent=ag)
    cmd("watch", cmd_watch, "space", "session", ("--count", {"type": int, "default": 0}), ("--timeout", {"type": float, "default": 0}), ("--json", {"action": "store_true"}))
    cmd("read", cmd_read, "space", ("--after", {"type": int, "default": 0}), ("--follow", {"action": "store_true"}), ("--json", {"action": "store_true"}))
    cmd("status", cmd_status, "space")
    return ap

async def run(a) -> int:
    if not (a.url and a.actor): return die("need --url and --actor (or HAN_URL, HAN_ACTOR)")
    key = a.key or keys.default_key_path(a.actor)
    if not os.path.exists(key): return die(f"no key file {key} (run: han keygen --actor {a.actor})")
    box = a.outbox or os.path.join(os.path.expanduser("~"), ".han", "outbox", a.actor.replace(":", "_").replace("@", "") + ".db")
    os.makedirs(os.path.dirname(os.path.abspath(box)), exist_ok=True)
    c = Client(a.url, a.actor, key, box, server=a.server)
    try:
        await c.connect()
        return await a.fn(c, a) or 0
    except WireError as ex: return die(f"{ex.code}: {ex.message}")
    except (ValueError, OSError) as ex: return die(str(ex))
    finally: await c.close()

def main(argv=None) -> int:
    a = parser().parse_args(argv)
    if a.cmd == "keygen": return cmd_keygen(a)
    try: return asyncio.run(run(a))
    except KeyboardInterrupt: return 130

if __name__ == "__main__":
    sys.exit(main())

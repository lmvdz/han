"""Real Claude Code behind HAN: a guarded agent, a member's steer, a held proposal, an operator's approval, an executor's outcome.

    cd H:\\HAN\\v1\\impl
    .venv\\Scripts\\python.exe demo\\claude_demo.py [--model MODEL] [--keep DIR]

Uses the logged-in Claude Code through the Claude Agent SDK: two short model turns, a few cents. Everything else is local (a
sequencer on a free port, an executor confined to <dir>/output, the host with a workspace in <dir>/workspace)."""
from __future__ import annotations
import argparse, asyncio, hashlib, os, sys, tempfile, time
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
from han import keys
from han.client import Client, default_policy
from han.core import P
from han.exec import Executor
from han.host import Host, ClaudeCodeAgent
from han.seq import Sequencer, SeqServer

SERVER, SPACE = "a.localhost", "demo:a.localhost"
ALICE, CAROL, BOT, EXEC = (f"@{n}:{SERVER}" for n in ("alice", "carol", "bot", "exec"))
STEER_DEPLOY = "Create a file hello.txt in the shared output directory containing 'hi from HAN', using the deploy tool"
STEER_LOCAL = "In your workspace, create notes.txt containing 'local only'. Do not use the deploy tool. Then say done."

async def until(pred, timeout=120.0, what=""):
    async def poll():
        while True:
            r = pred()
            if asyncio.iscoroutine(r): r = await r
            if r: return r
            await asyncio.sleep(0.1)
    try: return await asyncio.wait_for(poll(), timeout)
    except asyncio.TimeoutError: raise AssertionError(f"timed out waiting for {what}") from None

async def run(root: str, say=print, model: str | None = None, max_turns: int = 4) -> dict:
    """Runs the whole flow under `root`; narrates with `say`; returns what happened (for the live test)."""
    t0 = time.time()
    def n(msg): say(f"[{time.time() - t0:5.1f}s] {msg}")
    out_dir, ws = os.path.join(root, "output"), os.path.join(root, "workspace")
    os.makedirs(ws, exist_ok=True)
    seq = Sequencer(SERVER, keys.load_or_generate(os.path.join(root, "server.key.json"), "h1"), os.path.join(root, "seq.db"))
    srv = await SeqServer(seq, "127.0.0.1", 0).start()
    kp = lambda name: os.path.join(root, f"{name}.key.json")
    for name, kind, op in (("alice", "human", None), ("carol", "human", None), ("bot", "agent", ALICE), ("exec", "service", None)):
        k = keys.load_or_generate(kp(name), "k1"); seq.register(f"@{name}:{SERVER}", kind, k.key_id, k.pub, op)
    async def client(name):
        c = Client(srv.url, f"@{name}:{SERVER}", kp(name), os.path.join(root, f"{name}.outbox.db"), server=SERVER)
        await c.connect(); return c
    alice, carol, botc = await client("alice"), await client("carol"), await client("bot")
    ex = host = None; host_task = None; watcher = None
    try:
        send = lambda c, *a, **kw: c.append(*c.make_entry(*a, **kw))
        pol = default_policy(sessions={"watch": "member", "read": "owner", "steer": "member", "modes": ["auto", "guarded"]})
        await send(alice, "space.create", SPACE, {"policy": pol}, {"name": "demo"})
        for who in (CAROL, BOT, EXEC): await send(alice, "member.set", SPACE, {"actor": who, "role": "member"})
        await send(alice, "member.set", "profile:" + BOT, {"actor": ALICE, "role": "owner"})            # alice confirms she operates the agent
        n(f"space {SPACE}: alice (owner, the agent's operator and guard), carol (member), the agent {BOT}, the executor {EXEC}")
        ex = Executor(srv.url, EXEC, kp("exec"), os.path.join(root, "exec.journal.db"), [SPACE], out_dir, ["file.write"], server=SERVER)
        await ex.start()
        n(f"han-exec runs file.write into {out_dir}")
        agent = ClaudeCodeAgent(ws, model=model, max_turns=max_turns)
        host = Host(botc, SPACE, agent, os.path.join(root, "bot.state.db"), "guarded", EXEC)
        host_task = asyncio.create_task(host.run())
        sid = await until(lambda: host.session if host.state and host.session in host.state["sessions"] else None, 20, "session")
        n(f"the host opened session {sid[:10]} in guarded mode, driving Claude Code in {ws}")
        items = []
        async def pump():
            async for it in alice.watch(SPACE, sid): items.append(it)
        watcher = asyncio.create_task(pump())
        async def outputs(): return [i["text"] for i in items if i["type"] == "output"]
        async def state(): return (await alice.read_all(SPACE, 10 ** 9, state=True))[0]["state"]

        n(f"carol steers: {STEER_DEPLOY!r}")
        await send(carol, "session.input", SPACE, {"session": sid}, {"text": STEER_DEPLOY})
        await until(lambda: [i for i in items if i["type"] == "output" and i["text"].startswith("proposed ")], what="a proposal")
        recs = [i["record"] for i in await alice.read_all(SPACE, 0) if i["type"] == "record"]
        prop = next(r for r in recs if r["entry"]["kind"] == "effect.propose")
        pid, digest = prop["receipt"]["id"], prop["entry"]["body"]["planDigest"]
        st = await state()
        n(f"claude called the world tool; the host proposed {prop['entry']['body']['action']} as {pid[:12]}: phase {st['effects'][pid]['phase']}, "
          f"guards {st['effects'][pid]['guards']}, agent held for {st['held'].get(BOT)}")
        n(f"the file is not there yet: {os.path.exists(os.path.join(out_dir, 'hello.txt'))}; the plan is {prop['content']['plan']}")
        await until(lambda: len(agent.results) >= 1 and items[-1]["type"] == "status" and items[-1].get("state") == "idle", what="turn end")
        for t in await outputs(): n(f"  session output: {t}")
        t_wait = time.time()
        await send(alice, "effect.approve", SPACE, {"proposal": pid, "planDigest": digest})
        n("alice approves the plan she was shown (P-08)")
        await until(lambda: _phase(state, pid, "succeeded"), 30, "outcome")
        written = open(os.path.join(out_dir, "hello.txt"), encoding="utf-8").read()
        n(f"executor wrote hello.txt = {written!r} (phase succeeded after {time.time() - t_wait:.1f}s)")

        await alice.read_all(SPACE, 0)                                     # so alice's `seen` covers the instruction
        await send(alice, "agent.release", SPACE, {"agent": BOT})
        n("alice releases the hold")
        n(f"alice steers (the guard's own steer is not outside input, so the turn is auto): {STEER_LOCAL!r}")
        before = len(await outputs()); effects_before = len((await state())["effects"])
        await send(alice, "session.input", SPACE, {"session": sid}, {"text": STEER_LOCAL})
        await until(lambda: len(agent.results) >= 2 and items[-1]["type"] == "status" and items[-1].get("state") == "idle", 120, "second turn")
        await asyncio.sleep(0.3)
        for t in (await outputs())[before:]: n(f"  session output: {t}")
        notes = os.path.join(ws, "notes.txt")
        local = open(notes, encoding="utf-8").read() if os.path.exists(notes) else None
        st = await state()
        n(f"notes.txt in the workspace = {local!r}; effects proposed in this turn: {len(st['effects']) - effects_before}; held: {st['held']}")
        usage = [{"turns": r.num_turns, "usd": round(r.total_cost_usd or 0, 4), "in": sum((r.usage or {}).get(k, 0) for k in ("input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens")), "out": (r.usage or {}).get("output_tokens")} for r in agent.results]
        n(f"claude turns/cost per HAN turn: {usage}; tool decisions: {agent.trace}")
        return {"proposal": pid, "phase": st["effects"][pid]["phase"], "written": written, "local": local, "held_after": st["held"],
                "effects": len(st["effects"]), "trace": list(agent.trace), "usage": usage, "seconds": time.time() - t0,
                "outputs": await outputs(), "modes": list(agent.modes)}
    finally:
        for t in (watcher, host_task):
            if t: t.cancel()
        if host: host.stop()
        if host and host.agent: await host.agent.close()
        if ex: await ex.stop()
        for c in (alice, carol, botc): await c.close()
        await srv.stop(); seq.close()

async def _phase(state, pid, phase): return (await state())["effects"][pid]["phase"] == phase

def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--model"); ap.add_argument("--keep", help="run in this directory and keep it")
    a = ap.parse_args()
    root = a.keep or tempfile.mkdtemp(prefix="han-claude-demo-")
    os.makedirs(root, exist_ok=True)
    print(f"working directory: {root}")
    res = asyncio.run(run(root, model=a.model))
    ok = res["written"] == "hi from HAN" and res["local"] and res["effects"] == 1 and res["phase"] == "succeeded"
    print("\nRESULT:", "ok" if ok else "UNEXPECTED", {k: v for k, v in res.items() if k not in ("outputs",)})
    return 0 if ok else 1

if __name__ == "__main__":
    raise SystemExit(main())

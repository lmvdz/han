"""Helpers for the host tests: a space with alice (owner, the agent's operator and guard), carol and dave (members), an agent `bot`,
an executor `exec`, and a host running a ScriptedAgent against the real sequencer."""
import asyncio, os
from conftest import S, mkspace, member
from han.client import default_policy
from han.host import Host, ScriptedAgent

EXEC = "@exec:a.localhost"
BOT = "@bot:a.localhost"
ALICE, CAROL, DAVE = "@alice:a.localhost", "@carol:a.localhost", "@dave:a.localhost"

async def until(pred, timeout=5.0):
    """Poll a (sync or async) predicate until it is true."""
    async def poll():
        while True:
            r = pred()
            if asyncio.iscoroutine(r): r = await r
            if r: return r
            await asyncio.sleep(0.02)
    return await asyncio.wait_for(poll(), timeout)

async def world(env, steer="member"):
    """Register the actors, create the space, confirm alice as bot's operator. Returns the clients by name (bot's is made by start_host)."""
    for n in ("alice", "carol", "dave"): env.actor(n)
    env.actor("bot", "agent", operator=ALICE); env.actor("exec", "service")
    cl = {n: await env.client(n) for n in ("alice", "carol", "dave", "exec")}
    pol = default_policy(sessions={"watch": "member", "read": "owner", "steer": steer, "modes": ["auto", "guarded"]})
    await mkspace(cl["alice"], policy=pol)
    for a in (CAROL, DAVE): await member(cl["alice"], a)
    bot = await env.client("bot")
    await cl["alice"].append(*cl["alice"].make_entry("member.set", "profile:" + BOT, {"actor": ALICE, "role": "owner"}))     # the operator confirms
    await member(cl["alice"], BOT, "member"); await member(cl["alice"], EXEC, "member")
    return cl, bot

def start_host(env, client, mode, agent=None, crash=None, state=None, executor=EXEC):
    host = Host(client, S, agent or ScriptedAgent(), state or os.path.join(env.tmp, "bot.state.db"), mode, executor, crash)
    return host, asyncio.create_task(host.run())

async def records(c, kind=None, author=None):
    out = [i["record"] for i in await c.read_all(S, 0) if i["type"] == "record"]
    return [r for r in out if (kind is None or r["entry"]["kind"] == kind) and (author is None or r["entry"]["author"] == author)]

async def state(c):
    return (await c.read_all(S, 10**9, state=True))[0]["state"]

async def session_id(host):
    await until(lambda: host.session and host.state and host.session in host.state["sessions"])
    return host.session

async def steer(c, sid, text, **body):
    e, ct = c.make_entry("session.input", S, {"session": sid} | body, {"text": text})
    return await c.append(e, ct)

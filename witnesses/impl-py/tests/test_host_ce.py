"""Host: the mode is read from the state at the input's receipt (R-24, CE3) and a budgeted space gets a cost and a task (P-05, P-06, CE4)."""
import asyncio
from conftest import S
from fixtures_host import world, start_host, until, records, state, session_id, steer, ALICE, CAROL, BOT
from han.client import default_policy
from han.core import P
from han.host import ScriptedAgent, World

def sessions(modes, steer_="member"): return {"watch": "member", "read": "owner", "steer": steer_, "modes": modes}

async def set_policy(c, **kw):
    await c.append(*c.make_entry("policy.set", S, {"policy": default_policy(**kw)}))

async def test_mode_comes_from_the_state_at_the_steer_not_the_head(env):
    """R-24, CE3, H8: the agent chose guarding (a `guarded` session), carol steers it (outside input), the guard unguards it, and only
    then does the host run. At the head nothing is guarded, but at the steer's seq `guardStatus` names a guard and the instruction was
    outside input: the turn must run strict."""
    cl, botc = await world(env)
    host, task = start_host(env, botc, "guarded")
    sid = await session_id(host)
    task.cancel(); host.stop(); await botc.close()                                  # the host is down while it all happens
    a = cl["alice"]
    r = await steer(cl["carol"], sid, "push release")
    assert (await state(a))["guarded"] == {BOT: True}
    await a.append(*a.make_entry("agent.unguard", S, {"agent": BOT}))
    st = await state(a)
    assert st["guarded"] == {} and P.guard_status(st, {"profiles": {}}, BOT) == {"guarded": False, "forced": False}      # the head alone would say auto
    past = await a.read_state(S, at=r["seq"])
    assert past["guarded"] == {BOT: True} and past["head"] == r["seq"]
    assert P.guard_status(past, {"profiles": {}}, BOT) == {"guarded": True, "forced": False}      # no pinned profile here, so no guard named
    prof = (await a.read_state("profile:" + BOT))["profile"]
    assert P.guard_status(past, {"profiles": {BOT: prof}}, BOT) == {"guard": ALICE, "guarded": True, "forced": False}      # K1: the guard is the pinned operator
    agent = ScriptedAgent()
    botc2 = await env.client("bot")
    host2, task2 = start_host(env, botc2, "auto", agent)
    await until(lambda: agent.calls)
    assert [m for _, m in agent.calls] == ["strict"]
    task2.cancel()

BUDGET = {"unit": "usd-cents", "amount": 1000}

async def budgeted(env, agent=None, cost=None):
    """A budgeted space (like architecture.md 1.1): the team plus a space budget; returns (clients, host, task, session id)."""
    cl, botc = await world(env)
    await set_policy(cl["alice"], sessions=sessions(["auto", "guarded"]), budget=BUDGET)
    host, task = start_host(env, botc, "auto", agent, cost=cost)
    return cl, host, task, await session_id(host)

async def test_host_charges_its_budgeted_task_with_a_cost(env):
    """P-05, P-06, CE4: in a budgeted space the host proposes with a cost (the action's own, else the configured default) and names
    the budgeted task assigned to the agent, so Policy allows it and the effect is ready."""
    spend = [(r"push (\S+)", lambda m: World("http.post", {"url": f"http://example.invalid/{m[1]}"})),
             (r"spend (\d+)", lambda m: World("http.post", {"url": "http://example.invalid/s"}, {"unit": "usd-cents", "amount": int(m[1])}))]
    cl, host, task, sid = await budgeted(env, ScriptedAgent(spend), cost={"unit": "usd-cents", "amount": 50})
    a = cl["alice"]
    e, c = a.make_entry("task", S, {"to": [BOT], "budget": {"unit": "usd-cents", "amount": 500}}, {"ask": "hello"})
    tid = (await a.append(e, c))["id"]
    await steer(cl["carol"], sid, "push a")
    await steer(cl["carol"], sid, "spend 70")
    props = await until(lambda: _two(a))
    bodies = [p["entry"]["body"] for p in props]
    assert [b["cost"]["amount"] for b in bodies] == [50, 70] and all(b["task"] == tid and b["cost"]["unit"] == "usd-cents" for b in bodies)
    st = await state(a)
    assert all(st["effects"][p["receipt"]["id"]]["phase"] == "open" for p in props)
    task.cancel()

async def _two(a):
    ps = await records(a, "effect.propose", BOT)
    return ps if len(ps) == 2 else None

async def test_host_without_a_budgeted_task_says_it_cannot_spend(env):
    """P-05, CE4: with no open budgeted task assigned to the agent the host does not send a proposal Policy would deny; it says in the
    session stream that it cannot spend. (A default cost does not change that.)"""
    cl, host, task, sid = await budgeted(env, cost={"unit": "usd-cents", "amount": 50})
    from conftest import Collector
    seen = Collector(cl["carol"].watch(S, sid)); await asyncio.sleep(0.2)
    await steer(cl["carol"], sid, "push a")
    await seen.wait_for(lambda i: i["type"] == "output" and i["text"].startswith("refused:") and "cannot spend" in i["text"])
    await asyncio.sleep(0.2)
    assert await records(cl["alice"], "effect.propose", BOT) == [] and any("cannot spend" in r for r in host.refused)
    seen.stop(); task.cancel()

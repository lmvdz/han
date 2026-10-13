"""Round 31 (X1, X4, R-24): the host's explicit accept, its own final update when it stops working a task, and the naming of the driving task."""
import os
from conftest import S
from fixtures_host import world, start_host, until, records, state, session_id, ALICE, DAVE, BOT
from han.core import P
from han.host import ScriptedAgent
from test_host_p15 import _bot2, _delegated

class Boom(ScriptedAgent):
    async def turn(self, input, mode): raise RuntimeError("boom")

async def test_host_accepts_with_an_explicit_accepted_update_and_progress_accepts_nothing(env):
    """X4: the host's acceptance is `task.update {state: accepted}`; a `progress` update by a co-assignee puts nobody into `accepted`,
    so it carries nothing, while the host's accepted assignee carries the delegator's limits."""
    cl, botc, b2, host, handle, tid = await _delegated(env, "ok")
    await until(lambda: records(cl["alice"], "task.update", b2.actor))
    ups = await records(cl["alice"], "task.update", b2.actor)
    assert [u["entry"]["body"]["state"] for u in ups] == ["accepted"]
    st = await state(cl["alice"])
    assert st["tasks"][tid]["accepted"] == [b2.actor] and P.carried(st, b2.actor) == [BOT]
    t2 = (await botc.append(*botc.make_entry("task", S, {"to": [DAVE]}, {"ask": "x"})))["id"]
    await cl["dave"].append(*cl["dave"].make_entry("task.update", S, {"task": t2, "state": "progress"}, None, refs=[t2]))
    st = await state(cl["alice"])
    assert st["tasks"][t2]["accepted"] == [] and P.carried(st, DAVE) == []
    handle.cancel()

async def test_host_makes_the_agents_own_final_update_when_it_finishes_a_task(env):
    """R-24, X1: the agent finishes its task; the host appends its final update after the proposal, `accepted` drops the agent, and the
    delegator's limits no longer carry."""
    cl, botc, b2, host, handle, tid = await _delegated(env, "push deploy; finish")
    await until(lambda: _two(cl["alice"], b2.actor))
    ups = await records(cl["alice"], "task.update", b2.actor)
    props = await records(cl["alice"], "effect.propose", b2.actor)
    assert [u["entry"]["body"]["state"] for u in ups] == ["accepted", "done"]
    assert props[0]["receipt"]["seq"] < ups[1]["receipt"]["seq"]
    st = await state(cl["alice"])
    assert st["tasks"][tid]["accepted"] == [] and st["tasks"][tid]["state"] == "done" and P.carried(st, b2.actor) == []
    handle.cancel()

async def _two(c, who):
    return len(await records(c, "task.update", who)) >= 2

async def test_host_ends_its_carry_on_a_task_a_co_assignee_already_closed(env):
    """R-24, X1: a co-assignee closes the task; the closed task still takes bot2's own final update, which is the host's duty, and it
    changes nothing but `accepted` (state stays `done`)."""
    cl, botc = await world(env)
    BOT2, b2 = await _bot2(env, cl)
    await botc.append(*botc.make_entry("session.open", S, {"steerMode": "guarded"}, {"title": "work"}))
    host, handle = start_host(env, b2, "auto", ScriptedAgent(), state=os.path.join(env.tmp, "bot2.state.db"))
    await session_id(host)
    tid = (await botc.append(*botc.make_entry("task", S, {"to": [BOT2, DAVE]}, {"ask": "ok"})))["id"]
    await until(lambda: records(cl["alice"], "task.update", BOT2))
    await cl["dave"].append(*cl["dave"].make_entry("task.update", S, {"task": tid, "state": "done"}, None, refs=[tid]))
    await until(lambda: _two(cl["alice"], BOT2))
    ups = await records(cl["alice"], "task.update", BOT2)
    assert [u["entry"]["body"]["state"] for u in ups] == ["accepted", "cancelled"]
    st = await state(cl["alice"])
    t = st["tasks"][tid]
    assert t["accepted"] == [] and t["state"] == "done" and not t["open"] and P.carried(st, BOT2) == []
    handle.cancel()

async def test_host_ends_its_carry_when_the_author_cancels_the_task(env):
    """R-24: the author's `task.cancel` (not an owner's force-close) leaves the task open until an assignee closes it; the host does (`cancelled`)."""
    cl, botc, b2, host, handle, tid = await _delegated(env, "ok")
    await until(lambda: records(cl["alice"], "task.update", b2.actor))
    await botc.append(*botc.make_entry("task.cancel", S, {"task": tid}, None, refs=[tid]))
    await until(lambda: _two(cl["alice"], b2.actor))
    st = await state(cl["alice"])
    assert [u["entry"]["body"]["state"] for u in await records(cl["alice"], "task.update", b2.actor)] == ["accepted", "cancelled"]
    assert st["tasks"][tid]["accepted"] == [] and not st["tasks"][tid]["open"]
    handle.cancel()

async def test_failed_turn_ends_in_the_agents_own_failed_update(env):
    cl, botc, b2, host, handle, tid = await _delegated(env, "ok", Boom())
    await until(lambda: _two(cl["alice"], b2.actor))
    assert [u["entry"]["body"]["state"] for u in await records(cl["alice"], "task.update", b2.actor)] == ["accepted", "failed"]
    assert (await state(cl["alice"]))["tasks"][tid]["accepted"] == []
    handle.cancel()

async def test_every_proposal_and_subtask_of_a_task_driven_turn_names_the_driving_task(env):
    """R-24: two proposals and a delegation in one task-driven turn: each effect names the task, and the subtask has it as parent."""
    cl, botc, b2, host, handle, tid = await _delegated(env, f"push a; push b; delegate to {DAVE} help")
    await until(lambda: records(cl["alice"], "task", b2.actor))
    props = await until(lambda: _props(cl["alice"], b2.actor))
    assert len(props) == 2 and all(p["entry"]["body"].get("task") == tid for p in props)
    subs = await records(cl["alice"], "task", b2.actor)
    assert [s["entry"]["body"]["parent"] for s in subs] == [tid]
    handle.cancel()

async def _props(c, who):
    ps = await records(c, "effect.propose", who)
    return ps if len(ps) >= 2 else None

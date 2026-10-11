"""R-17 / R-18: a host killed mid-turn proposes each intent exactly once after restart, and every entry names its inputs."""
import asyncio, pytest
from conftest import S
from fixtures_host import world, start_host, until, records, session_id, steer, ALICE, BOT
from han.host import ScriptedAgent, HostCrash

class Kill:
    """A crash hook: raises HostCrash the first time the named point is reached."""
    def __init__(self, point): self.point, self.hit = point, False
    def __call__(self, point, key=None):
        if point == self.point and key != "open" and not self.hit: self.hit = True; raise HostCrash(point)

@pytest.mark.parametrize("point", ["decided", "journaled", "appended"])
async def test_killed_mid_turn_proposes_exactly_once(env, point):
    """R-18, R-17: the host dies (a) after the agent decided but before the decision was stored, (b) after it was stored but before
    the append, (c) after the append but before it was noted. After restart the intent is in the log exactly once; the entry's
    seen/refs name the instruction that drove it."""
    cl, botc = await world(env)
    kill = Kill(point)
    agent1 = ScriptedAgent()
    host, task = start_host(env, botc, "auto", agent1, crash=kill)
    sid = await session_id(host)
    r = await steer(cl["alice"], sid, "push release")
    with pytest.raises(HostCrash): await asyncio.wait_for(task, 5)
    assert kill.hit
    host.stop(); await botc.close()                                   # the process is gone
    botc2 = await env.client("bot")                                    # same key, same outbox, same journal file
    agent2 = ScriptedAgent()
    host2, task2 = start_host(env, botc2, "auto", agent2)
    await until(lambda: records(cl["alice"], "effect.propose", BOT))
    await asyncio.sleep(0.5)                                           # room for a duplicate to show up
    props = await records(cl["alice"], "effect.propose", BOT)
    assert len(props) == 1
    e = props[0]["entry"]
    assert e["seen"] == r["seq"] and e["refs"] == [r["id"]]
    assert len(agent2.calls) == (1 if point == "decided" else 0)       # decided-but-unstored work is run again; stored work is never decided again
    assert host2.session == host.session or point == "decided"         # the same session continues (journal), never a second one
    assert len(await records(cl["alice"], "session.open", BOT)) == 1
    n = [i for i in (await cl["alice"].read_all(S, 0)) if i["type"] == "record" and i["record"]["entry"]["kind"] == "effect.propose"]
    assert len(n) == 1
    task2.cancel()

async def test_restart_does_not_replay_old_steers(env):
    """R-18: after a clean restart the cursor is kept: instructions already turned into intents are not run again."""
    cl, botc = await world(env)
    host, task = start_host(env, botc, "auto")
    sid = await session_id(host)
    await steer(cl["alice"], sid, "push one")
    await until(lambda: records(cl["alice"], "effect.propose", BOT))
    task.cancel(); host.stop(); await botc.close()
    botc2 = await env.client("bot")
    agent2 = ScriptedAgent()
    host2, task2 = start_host(env, botc2, "auto", agent2)
    await steer(cl["alice"], sid, "push two")
    await until(lambda: len(agent2.calls) == 1)
    await asyncio.sleep(0.3)
    assert [i.text for i, _ in agent2.calls] == ["push two"] and len(await records(cl["alice"], "effect.propose", BOT)) == 2
    task2.cancel()

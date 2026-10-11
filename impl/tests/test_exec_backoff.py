"""X-11: the executor backs off exponentially before re-claiming an effect after failed/not started, and an effect it won't run
is never claimed: the proposer gets one message instead."""
import asyncio, time
from conftest import S
from fixtures_exec import (execs, team, make_exec, file_plan, propose, approve, state, until, phase_is, kinds, records_of)   # noqa: F401
from fixtures_exec import ALICE

async def test_backoff_doubles_and_is_capped(env, execs):
    """X-11: three claims in a row fail as not started (each receipt arrives late on the executor's clock). The waits before the
    2nd, 3rd and 4th claim are at least base, 2 x base, then the cap; the 4th claim finally launches."""
    a = await team(env)
    plan = file_plan("bo.txt", "x")
    pid, _ = await propose(a, plan, f"@exec:{env.server}"); await approve(a, pid, plan)
    now, stamps = [100.0], []
    def hook(point, **_):
        if point == "after-receipt":
            stamps.append(time.monotonic())
            if len(stamps) <= 3: now[0] += 25                    # late: expired, failed/no
    ex = make_exec(env, execs, clock=lambda: now[0], hook=hook, backoff_base=0.25, backoff_cap=0.5); await ex.start()
    await until(phase_is(a, pid, "succeeded"), what="succeeded")
    gaps = [b - a_ for a_, b in zip(stamps, stamps[1:])]
    assert len(stamps) == 4 and len(ex.launched) == 1
    assert gaps[0] >= 0.25 and gaps[1] >= 0.5 and gaps[2] >= 0.5          # 0.25, 0.5, then capped at 0.5
    assert gaps[0] < 0.5 and gaps[2] < 1.0                                 # and not longer than they should be

async def test_unsupported_and_unsafe_effects_are_declined_once(env, execs):
    """X-11: an action not enabled, a path outside the sandbox and a URL outside the allow-list are never claimed; the proposer gets
    exactly one `message` per proposal citing it (refs), also after the executor restarts."""
    a = await team(env)
    exe = f"@exec:{env.server}"
    plans = [{"action": "http.post", "target": "http://127.0.0.1:9/x", "args": {}},          # not enabled below
             file_plan("../escape.txt"), {"action": "file.write", "target": "ok/../../e", "args": {"content": "y"}}]
    pids = []
    for p in plans:
        pid, _ = await propose(a, p, exe); await approve(a, pid, p); pids.append(pid)
    ex = make_exec(env, execs, actions=("file.write",)); await ex.start()
    msgs = lambda: records_of(a, "message", exe)
    await until(lambda: _count(msgs, 3), what="three messages")
    await ex.stop()
    ex2 = make_exec(env, execs, actions=("file.write",)); await ex2.start()
    await asyncio.sleep(0.6)
    ms = await msgs()
    assert len(ms) == 3 and sorted(m["entry"]["refs"][0] for m in ms) == sorted(pids)
    assert all(m["entry"]["body"]["to"] == [ALICE] and "declined" in m["content"]["text"] for m in ms)
    st = await state(a)
    assert all(st["effects"][p]["phase"] == "ready" and not st["effects"][p]["claims"] for p in pids)     # never claimed
    assert not [k for k, _ in await kinds(a) if k == "effect.claim"]

def _count(msgs, n):
    async def f(): return len(await msgs()) == n
    return f()

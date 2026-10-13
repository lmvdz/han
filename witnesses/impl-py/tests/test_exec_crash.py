"""Executor crash safety (X-02…X-07, X-10) and P-15: crashes at journal points, real process kills, the instance lock, guarded agents."""
import asyncio, os, sys
import pytest
from conftest import S, member
from fixtures_exec import (execs, team, make_exec, file_plan, propose, approve, state, until, phase_is, kinds, sandbox, crash_at,
                           crash, CountingHttp, run_cli, key_path)
from han.client import default_policy
from han.core import P
from han.exec import Journal, StartRefused
from fixtures_exec import APPROVE_ALL

def outcomes(ks): return [(b["result"], b["started"]) for k, b in ks if k == "effect.outcome"]
def journal_names(ex, cid): return [e for e, _ in ex.journal.events(cid)]

async def test_crash_after_receipt_restart_never_launches_that_claim(env, execs):
    """X-05, X-06: the executor dies after the claim has a receipt but before journaling `launching`. On restart it finishes the
    append (same receipt), reports failed/started no for that claim and never launches it; the effect is rearmed and a NEW claim runs."""
    a = await team(env)
    plan = file_plan("c1.txt", "x")
    pid, _ = await propose(a, plan, f"@exec:{env.server}"); await approve(a, pid, plan)
    ex1 = make_exec(env, execs, hook=crash_at("after-receipt")); await ex1.start()
    await crash(ex1)
    ks = await kinds(a)
    assert [k for k, _ in ks][-1] == "effect.claim" and not outcomes(ks)             # claimed, nothing reported
    assert not os.path.exists(os.path.join(sandbox(env), "c1.txt"))
    claim1 = P.entry_id(next(r["entry"] for r in await __import__("conftest").chain(a) if r["entry"]["kind"] == "effect.claim"))
    ex2 = make_exec(env, execs); await ex2.start()
    await until(phase_is(a, pid, "succeeded"))
    ks = await kinds(a)
    assert outcomes(ks) == [("failed", "no"), ("succeeded", "yes")]
    assert claim1 not in ex2.launched and len(ex2.launched) == 1 and ex2.launched[0] != claim1
    assert journal_names(ex2, claim1) == ["claiming", "reporting", "reported"]

async def test_crash_before_send_restart(env, execs):
    """X-02, X-06: the executor dies after journaling `claiming` but before the first send. On restart the journaled claim is
    sent (R-06) and reported failed/started no without launching it."""
    a = await team(env)
    plan = file_plan("c2.txt", "x")
    pid, _ = await propose(a, plan, f"@exec:{env.server}"); await approve(a, pid, plan)
    ex1 = make_exec(env, execs, hook=crash_at("after-claiming")); await ex1.start()
    await crash(ex1)
    assert not [k for k, _ in await kinds(a) if k == "effect.claim"]                  # never sent
    ex2 = make_exec(env, execs); await ex2.start()
    await until(phase_is(a, pid, "succeeded"))
    assert outcomes(await kinds(a)) == [("failed", "no"), ("succeeded", "yes")]
    assert len(ex2.launched) == 1
    first = [c for c in ex2.journal.claims() if c not in ex2.launched][0]
    assert journal_names(ex2, first) == ["claiming", "reporting", "reported"]

async def test_crash_after_launch_reports_unknown_and_never_relaunches(env, execs):
    """X-04, X-06, X-05: the executor dies after the world was touched but before journaling the result. Restart reports
    unknown/unknown; the effect becomes `unknown`, and the HTTP endpoint is hit exactly once, never twice."""
    a = await team(env)
    http = await CountingHttp().start()
    try:
        plan = {"action": "http.post", "target": http.url, "args": {"k": "v"}}
        pid, _ = await propose(a, plan, f"@exec:{env.server}"); await approve(a, pid, plan)
        allow = [http.url.rsplit("/", 1)[0] + "/"]
        ex1 = make_exec(env, execs, allow_urls=allow, hook=crash_at("after-launch")); await ex1.start()
        await crash(ex1)
        assert http.hits == 1
        ex2 = make_exec(env, execs, allow_urls=allow); await ex2.start()
        await until(phase_is(a, pid, "unknown"), what="unknown")
        await asyncio.sleep(0.5)
        assert http.hits == 1 and not ex2.launched
        assert outcomes(await kinds(a)) == [("unknown", "unknown")]
        assert (await state(a))["effects"][pid]["phase"] == "unknown"
    finally: await http.stop()

async def test_process_killed_after_launching(env, execs):
    """X-04, X-06, X-07: a real process (python -m han.exec) is killed at `launching` (os._exit). A new process on the same
    journal reports unknown/unknown, does not launch, and the file is never written."""
    a = await team(env)
    plan = file_plan("k.txt", "x")
    pid, _ = await propose(a, plan, f"@exec:{env.server}"); await approve(a, pid, plan)
    p = await run_cli(env, "--crash-at", "after-launching")
    assert await asyncio.wait_for(p.wait(), 30) == 77
    assert not os.path.exists(os.path.join(sandbox(env), "k.txt"))
    ex2 = make_exec(env, execs); await ex2.start()                                    # same journal, lock was released by the death
    await until(phase_is(a, pid, "unknown"), what="unknown")
    await asyncio.sleep(0.3)
    assert outcomes(await kinds(a)) == [("unknown", "unknown")]
    assert not os.path.exists(os.path.join(sandbox(env), "k.txt")) and not ex2.launched

async def test_second_instance_refuses_to_start(env, execs):
    """X-07: while one process holds the instance key's journal, a second process (and a second Executor object) with the same
    instance refuses to start."""
    await team(env)
    ex1 = make_exec(env, execs); await ex1.start()
    with pytest.raises(StartRefused): await make_exec(env, execs).start()
    p = await run_cli(env)
    out, err = await asyncio.wait_for(p.communicate(), 30)
    assert p.returncode == 3 and b"refusing to start" in err and b"already running" in err
    await ex1.stop()
    ex3 = make_exec(env, execs); await ex3.start()                                   # free again after a clean stop

async def test_key_without_its_journal_refuses(env, execs):
    """X-07: a key that has been used must not start without its journal (or with a different one); losing the journal means
    retiring the key."""
    await team(env)
    ex1 = make_exec(env, execs); await ex1.start(); await ex1.stop()
    jp = ex1.journal_path
    os.remove(jp)
    with pytest.raises(StartRefused, match="journal"): await make_exec(env, execs).start()
    assert os.path.exists(key_path(env, "exec") + ".used") and not os.path.exists(jp)
    with pytest.raises(StartRefused): Journal(os.path.join(env.tmp, "other.db"), "k1", key_path(env, "exec") + ".used")   # fresh journal, used key
    os.remove(key_path(env, "exec") + ".used")                                         # (a key never used) starts normally
    await make_exec(env, execs).start()

async def test_guarded_executor_effect_waits_for_its_guard(env, execs):
    """P-15, E3: an effect naming a guarded agent as executor stays open and is not claimed; once the guard (its
    operator) approves, the agent's executor claims and runs it. Proposed by another member, no approval rule in the policy."""
    alice, bob = env.actor("alice"), env.actor("bob")
    bot = env.actor("bot", "agent", operator=alice)
    a, b = await env.client("alice"), await env.client("bob")
    from conftest import mkspace
    await mkspace(a, policy=default_policy())
    await member(a, bob); await member(a, bot)
    await a.append(*a.make_entry("member.set", "profile:" + bot, {"actor": alice, "role": "owner"}))     # operator confirms
    botc = await env.client("bot")
    await botc.append(*botc.make_entry("session.open", S, {"steerMode": "guarded"}, {"title": "work"}))   # sticky guarded
    plan = file_plan("held.txt", "x")
    pid, _ = await propose(b, plan, bot)
    eff = (await state(a))["effects"][pid]
    assert eff["phase"] == "open" and eff["guardedFor"] == [bot]              # recorded when proposed: the executor was guarded
    ex = make_exec(env, execs, name="bot"); await ex.start()
    await asyncio.sleep(0.5)
    assert not [k for k, _ in await kinds(a) if k == "effect.claim"] and not ex.launched
    await approve(a, pid, plan)                                                         # the guard approves
    await until(phase_is(a, pid, "succeeded"), what="succeeded")
    assert len(ex.launched) == 1 and os.path.exists(os.path.join(sandbox(env), "held.txt"))
    assert outcomes(await kinds(a)) == [("succeeded", "yes")]

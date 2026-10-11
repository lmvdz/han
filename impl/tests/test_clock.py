"""R-03a (receipt time never goes backwards, also across restarts) and R-16 (abandon needs elapsed steady time, also after a restart)."""
import asyncio, pytest
from conftest import S, T0, FakeClock, mkspace, member, say, chain, plan_digest
from han.client import default_policy
from han.core import P, W, WireError

async def test_receipt_time_survives_restart_with_clock_behind(env):
    """R-03a: restart with the wall clock stepped back: the server answers unavailable until its clock catches up; receipt times never decrease."""
    env.actor("alice"); a = await env.client("alice")
    await mkspace(a)
    env.clock.advance(100)
    r_last = await say(a, "late")
    assert P.ts(r_last["time"]) == T0 + 100
    pending = a.make_entry("message", S, {}, {"text": "after"})
    behind = FakeClock(T0 + 10)                                         # restarted on a machine whose clock says T0 + 10
    await env.bounce(behind)
    t = asyncio.create_task(a.append(*pending))
    await asyncio.sleep(0.4)
    assert not t.done()                                                 # unavailable, retried; nothing ordered with an earlier time
    assert len(await chain(a)) == 2
    old = await _first_record_entry(a)
    assert (await a._request("append", {"entry": old[0], "content": old[1]}))["seq"] == 1       # a resend of an old entry is answered first (R-01)
    behind.advance(95)                                                  # T0 + 105 >= T0 + 100
    r = await asyncio.wait_for(t, 5)
    assert P.ts(r["time"]) >= P.ts(r_last["time"]) and r["seq"] == 3

async def _first_record_entry(a):
    rec = (await chain(a))[0]
    return rec["entry"], rec["content"]

async def test_wall_step_during_a_run_is_ignored(env):
    """R-03a: the receipt clock is anchored at startup and then steady; stepping the wall clock back changes nothing."""
    env.actor("alice"); a = await env.client("alice")
    r1 = await mkspace(a)
    env.clock.advance(50)
    r2 = await say(a, "x")
    env.clock.step_wall(-3600)                                          # NTP step / operator change
    env.clock.advance(1)
    r3 = await say(a, "y")
    assert P.ts(r1["time"]) <= P.ts(r2["time"]) <= P.ts(r3["time"]) and P.ts(r3["time"]) == P.ts(r2["time"]) + 1

async def test_receipt_time_non_decreasing_and_mirror_checks_it(env):
    """R-03a, R-14: receipts are non-decreasing over a restart that moves forward; a mirror accepts the whole chain."""
    env.actor("alice"); a = await env.client("alice")
    await mkspace(a); env.clock.advance(5); await say(a, "1")
    ahead = FakeClock(T0 + 500)
    await env.bounce(ahead)
    await say(a, "2")
    recs = await chain(a)
    times = [P.ts(r["receipt"]["time"]) for r in recs]
    assert times == sorted(times) and times[-1] == T0 + 500
    m = W.Mirror(env.seq.server_keys, lambda x, n: env.seq.profile(x, n))
    assert all(ok for ok, _ in (m.ingest(r) for r in recs))

# ── R-16 ──
async def _claimed(env):
    """alice (owner), bob (proposer), exec (service, the executor): bob proposes, exec claims. Returns clients and the claim id."""
    alice, bob, ex = env.actor("alice"), env.actor("bob"), env.actor("exec", "service")
    a, b, x = await env.client("alice"), await env.client("bob"), await env.client("exec")
    await mkspace(a, policy=default_policy(launchWindow=15))
    await member(a, bob); await member(a, ex)
    plan = {"action": "fs.write", "target": "f"}
    body = {"executor": ex, "action": "fs.write", "planDigest": plan_digest(plan), "repeatable": False}
    r = await b.append(*b.make_entry("effect.propose", S, body, {"plan": plan}))
    claim = await x.append(*x.make_entry("effect.claim", S, {"proposal": r["id"], "attempt": 1, "instance": "k1", "window": 15}))
    return a, b, x, claim

def _abandon(a, claim):
    return a.make_entry("effect.outcome", S, {"claim": claim["id"], "result": "unknown", "started": "unknown"})

async def test_r16_abandon_waits_a_full_window(env):
    """R-16: an owner's abandon is refused until A seconds of the sequencer's steady clock have passed since it ordered the claim."""
    a, b, x, claim = await _claimed(env)
    A = P.abandon_after(15)
    e, c = _abandon(a, claim)
    t = asyncio.create_task(a.append(e, c))
    await asyncio.sleep(0.3)
    assert not t.done()                                                 # unavailable (R-16), retried
    env.clock.advance(A - 1); await asyncio.sleep(0.3)
    assert not t.done()
    env.clock.advance(1)
    r = await asyncio.wait_for(t, 5)
    assert env.seq.home.spaces[S]["state"]["effects"][(await chain(a))[3]["receipt"]["id"]]["phase"] == "unknown"
    assert r["seq"] == 6

async def test_r16_after_restart_waits_a_full_window_again(env):
    """R-16: after a restart the sequencer can't measure across it: even though receipt times say A has long passed, an abandon of an earlier claim waits a full A of the new steady clock."""
    a, b, x, claim = await _claimed(env)
    A = P.abandon_after(15)
    env.clock.advance(A + 10)                                          # wall time since the claim is already > A
    await env.bounce()                                                 # restart: new steady clock starts now
    e, c = _abandon(a, claim)
    t = asyncio.create_task(a.append(e, c))
    await asyncio.sleep(0.4)
    assert not t.done() and len(await chain(a)) == 5                   # refused (unavailable)
    env.clock.advance(A - 1); await asyncio.sleep(0.3)
    assert not t.done()
    env.clock.advance(1)
    r = await asyncio.wait_for(t, 5)
    assert r["seq"] == 6

async def test_r16_executor_outcome_is_not_delayed(env):
    """R-16 applies only to an owner's abandon: the claiming instance reports at once."""
    a, b, x, claim = await _claimed(env)
    r = await x.append(*x.make_entry("effect.outcome", S, {"claim": claim["id"], "result": "failed", "started": "no"}))
    assert r["seq"] == 6

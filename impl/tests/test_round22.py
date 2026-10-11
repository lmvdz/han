"""Round 22 (interop/DECISIONS-r22.md): E5 order of verdicts, E6 host state, E8 paged state, E11 small items."""
import asyncio
import pytest
from conftest import S, T0, FakeClock, mkspace, member, say, chain, plan_digest
from test_clock import _claimed, _abandon
from han import keys
from han.client import Client, default_policy
from han.core import P, W, LIMITS, WireError
from fixtures_exec import team, propose, approve, file_plan, make_exec, phase_is, execs, until as xuntil
from han.entries import make_entry
from han.host import ScriptedAgent
from fixtures_host import world, start_host, until, state, session_id, steer, records, BOT, ALICE

def verdict(seq, entry, content=None):
    """The code `Sequencer.append` answers: "ok" or the WireError's code."""
    try: seq.append(entry, content); return "ok"
    except WireError as ex: return ex.code

def forged(env, name, entry):
    """`entry` re-signed with a key that is not `name`'s."""
    other = keys.load_or_generate(f"{env.tmp}/forger.key.json", "k1")
    return W.sign_entry({k: v for k, v in entry.items() if k not in ("sig", "id")}, other)

# ── E5: one order of verdicts: dedupe, routing, shape, signature, Policy, (limit), moment-dependent unavailable ──
async def test_e5_dedupe_before_routing(env):
    """(1) before (2): an ordered entry is answered with its receipt even after the space has moved; a new one is `moved`."""
    env.actor("alice"); a = await env.client("alice")
    e, c = a.make_entry("message", S, {}, {"text": "once"})
    await mkspace(a); r = await a.append(e, c)
    env.seq.home.moved[S] = "b.localhost"
    assert env.seq.append(e, c) == r
    e2, c2 = a.make_entry("message", S, {}, {"text": "twice"})
    with pytest.raises(WireError) as ex: env.seq.append(e2, c2)
    assert ex.value.code == "moved" and ex.value.moved_to == "b.localhost"

async def test_e5_routing_before_shape(env):
    """(2) before (3): a malformed entry for a space this server does not host is `moved`; for one it hosts, `invalid`."""
    env.actor("alice"); a = await env.client("alice"); await mkspace(a)
    foreign, _ = a.make_entry("message", "q1:other.localhost", {}, {"text": "x"})
    bad_foreign = dict(foreign); del bad_foreign["nonce"]
    with pytest.raises(WireError) as ex: env.seq.append(bad_foreign, None)
    assert ex.value.code == "moved" and ex.value.moved_to == "other.localhost"
    mine, c = a.make_entry("message", S, {}, {"text": "x"})
    bad_mine = dict(mine); del bad_mine["nonce"]
    assert verdict(env.seq, bad_mine, c) == "invalid"

async def test_e5_shape_before_signature_and_profile(env):
    """(3) before (4): a malformed entry from an author with no profile is `invalid`; a well-formed one cannot be judged: `unavailable`."""
    env.actor("alice"); a = await env.client("alice"); await mkspace(a)
    ghost = keys.load_or_generate(f"{env.tmp}/ghost.key.json", "k1")
    ok, c = make_entry(ghost, f"@ghost:{env.server}", "message", S, {}, {"text": "x"}, seen=1)
    assert verdict(env.seq, ok, c) == "unavailable"
    bad = dict(ok, extra="junk")
    assert verdict(env.seq, bad, c) == "invalid"

async def test_e5_signature_before_policy(env):
    """(4) before (5): a forged entry from a non-member is `invalid`; the same entry correctly signed is `denied`."""
    env.actor("alice"); env.actor("mallory"); a = await env.client("alice"); m = await env.client("mallory"); await mkspace(a)
    e, c = m.make_entry("message", S, {}, {"text": "let me in"})
    assert verdict(env.seq, forged(env, "mallory", e), c) == "invalid"
    assert verdict(env.seq, e, c) == "denied"

async def test_e5_unknown_key_is_denied_not_invalid(env):
    """(4): an entry naming a keyId the author's profile does not have is `denied` (R-12); a bad signature under a known key is `invalid`."""
    env.actor("alice"); a = await env.client("alice"); await mkspace(a)
    e, c = a.make_entry("message", S, {}, {"text": "x"})
    other = keys.load_or_generate(f"{env.tmp}/other.key.json", "k9")
    e2, c2 = make_entry(other, f"@alice:{env.server}", "message", S, {}, {"text": "x"}, seen=1)
    assert verdict(env.seq, e2, c2) == "denied" and verdict(env.seq, forged(env, "alice", e), c) == "invalid"

async def test_e5_policy_before_rate_limit_before_unavailable(env):
    """(5) before (6) before (7): with every author over its rate limit, an entry Policy refuses is `denied`, one it allows is `limit` with
    retryAfterMs, and with the clock also behind the chain (R-03a) it is still `limit`; the limit never hides a doomed entry."""
    env.actor("alice"); env.actor("mallory"); a = await env.client("alice"); m = await env.client("mallory")
    await mkspace(a); env.clock.advance(100); await say(a, "late")
    ok, no = a.make_entry("message", S, {}, {"text": "later"}), m.make_entry("message", S, {}, {"text": "let me in"})
    env.seq.home.limiter = lambda author: 500
    assert verdict(env.seq, *no) == "denied"
    with pytest.raises(WireError) as ex: env.seq.append(*ok)
    assert ex.value.code == "limit" and ex.value.retry_after_ms == 500
    await env.bounce(FakeClock(T0 + 10)); env.seq.home.limiter = lambda author: 500
    assert verdict(env.seq, *no) == "denied" and verdict(env.seq, *ok) == "limit"
    env.seq.home.limiter = None
    assert verdict(env.seq, *ok) == "unavailable"

async def test_e5_signature_before_r16(env):
    """(4) before (7): a forged early abandon is `invalid`, not `unavailable`."""
    a, b, x, claim = await _claimed(env)
    e, c = _abandon(a, claim)
    assert verdict(env.seq, forged(env, "alice", e), c) == "invalid"
    assert verdict(env.seq, e, c) == "unavailable"

async def test_e5_policy_before_r16(env):
    """(5) before (7): a non-owner's early abandon is `denied` (Policy), the owner's is `unavailable` (R-16)."""
    a, b, x, claim = await _claimed(env)
    e, c = b.make_entry("effect.outcome", S, {"claim": claim["id"], "result": "unknown", "started": "unknown"})
    assert verdict(env.seq, e, c) == "denied"
    e, c = _abandon(a, claim)
    assert verdict(env.seq, e, c) == "unavailable"

async def test_e5_policy_before_clock_behind(env):
    """(5) before (7): with the clock behind the chain (R-03a), an entry Policy refuses is `denied`; one it allows is `unavailable`."""
    env.actor("alice"); env.actor("mallory"); a = await env.client("alice"); m = await env.client("mallory")
    await mkspace(a); env.clock.advance(100); await say(a, "late")
    pending_ok = a.make_entry("message", S, {}, {"text": "later"})
    pending_no = m.make_entry("message", S, {}, {"text": "let me in"})
    await env.bounce(FakeClock(T0 + 10))
    assert verdict(env.seq, *pending_no) == "denied"
    assert verdict(env.seq, *pending_ok) == "unavailable"

# ── E6: the effective mode from the state at the input's seq; `held` and `guardedNow` from the head ──
class GatedAgent(ScriptedAgent):
    """The first turn waits for `gate`, so inputs queue up behind it."""
    def __init__(self): super().__init__(); self.gate = asyncio.Event(); self.first = True
    async def turn(self, input, mode):
        if self.first:
            self.first = False; await self.gate.wait()
        return await super().turn(input, mode)

async def test_e6_input_queued_before_a_hold_runs_strict_after_it(env):
    """R-24: alice's steer (the guard's own, so `auto` at its seq) is queued behind a running turn; carol's steer then holds the agent.
    When alice's turn runs, the head says held: it is strict, though the state at its seq says auto."""
    cl, botc = await world(env)
    agent = GatedAgent()
    host, task = start_host(env, botc, "guarded", agent)
    sid = await session_id(host)
    await steer(cl["alice"], sid, "first")                           # the turn that blocks
    await until(lambda: not agent.first)
    queued = await steer(cl["alice"], sid, "second")
    hold = await steer(cl["carol"], sid, "push release")
    assert hold["seq"] > queued["seq"] and (await state(cl["alice"]))["held"] == {BOT: ALICE}
    at = await host._state_at(queued["seq"])
    assert BOT not in at["held"]                                     # at the input's seq the agent was not held
    agent.gate.set()
    await until(lambda: len(agent.calls) >= 3)
    assert [(i.text, m) for i, m in agent.calls] == [("first", "auto"), ("second", "strict"), ("push release", "strict")]
    task.cancel()

# ── E8: the state in pages ──
async def _big_space(env, tasks=5, effects=3):
    """alice (owner) and exec (service): `tasks` open tasks and `effects` proposals. Returns (alice client, [proposal ids])."""
    a = await team(env)
    for i in range(tasks): await a.append(*a.make_entry("task", S, {"to": [f"@alice:{env.server}"]}, {"ask": f"job {i}"}))
    pids = [(await propose(a, file_plan(f"out/{i}.txt", str(i)), f"@exec:{env.server}"))[0] for i in range(effects)]
    return a, pids

async def test_e8_state_in_pages_with_cursor(env, monkeypatch):
    """E8: with maxStateItems = 2, `read {state}` answers the first two effects and tasks and `next`; `read {state, cursor}` gives the
    rest, pinned at the seq of the first page; `limit` is never the answer; the merged pages are the whole state."""
    monkeypatch.setitem(LIMITS, "maxStateItems", 2)
    a, pids = await _big_space(env)
    first = [i for i in await a.read_all(S, 2 ** 31, state=True, limit=1) if i["type"] == "state"]
    assert len(first) == 1 and set(first[0]) == {"type", "state", "next"}
    st = first[0]["state"]
    assert len(st["effects"]) == 2 and len(st["tasks"]) == 2 and list(st["effects"]) == pids[:2] and first[0]["next"] == f"{st['head']}.2.2"
    await say(a, "the head moves between pages")
    page2 = (await a.read_all(S, 0, state=True, cursor=first[0]["next"]))
    assert len(page2) == 1 and page2[0]["type"] == "state" and page2[0]["state"]["head"] == st["head"]       # pinned at the first page's seq
    assert list(page2[0]["state"]["effects"]) == pids[2:] and len(page2[0]["state"]["tasks"]) == 2 and page2[0]["next"] == f"{st['head']}.4.4"
    page3 = await a.read_all(S, 0, state=True, cursor=page2[0]["next"])
    assert "next" not in page3[0] and len(page3[0]["state"]["tasks"]) == 1 and page3[0]["state"]["effects"] == {}
    whole = await a.read_state(S, at=st["head"])
    assert list(whole["effects"]) == pids and len(whole["tasks"]) == 5 and whole["head"] == st["head"]
    assert [p["state"]["head"] async for p in a.state_pages(S)] == [st["head"] + 1] * 3          # the head page and its two cursor pages
    now = await a.read_state(S)
    assert list(now["effects"]) == pids and len(now["tasks"]) == 5 and "next" not in now

async def test_e8_one_effect_and_argument_errors(env, monkeypatch):
    """E8: `read {state, effect}` is that effect plus policy, roles, sessions, held, guarded and agents; bad combinations are `invalid`."""
    monkeypatch.setitem(LIMITS, "maxStateItems", 2)
    a, pids = await _big_space(env)
    st = await a.read_state(S, effect=pids[2])
    assert list(st["effects"]) == [pids[2]] and set(st) == {"space", "head", "home", "policy", "roles", "sessions", "held", "guarded", "agents", "effects"}
    with pytest.raises(WireError) as ex: await a.read_state(S, effect="0" * 64)
    assert ex.value.code == "not_found"
    head = st["head"]
    for kw in ({"effect": pids[0]}, {"cursor": f"{head}.0.0"}, {"state": True, "effect": pids[0], "cursor": f"{head}.0.0"}, {"state": True, "cursor": "banana"},
               {"state": True, "cursor": f"{head + 1}.0.0"}, {"state": True, "cursor": "0.0.0"}, {"state": True, "cursor": f"{head}.0.0", "at": 1},
               {"state": True, "effect": pids[0], "follow": True}):
        with pytest.raises(WireError) as ex: await a.read_all(S, 0, **kw)
        assert ex.value.code == "invalid", kw

async def test_e8_state_never_answers_limit_and_hello_names_the_page_size(env):
    env.actor("alice"); a = await env.client("alice")
    await mkspace(a)
    assert a.hello_info["limits"]["maxStateItems"] == 1000 == LIMITS["maxStateItems"]
    items = await a.read_all(S, 0, state=True, limit=1)                         # a state and one record: no `limit` error
    assert [i["type"] for i in items] == ["state", "record"]

async def test_e8_executor_runs_effects_beyond_the_first_page(env, execs, monkeypatch):
    """E8: the executor pages through the state: with one effect per page, all three ready effects are still claimed and run."""
    monkeypatch.setitem(LIMITS, "maxStateItems", 1)
    a = await team(env)
    pids = []
    for i in range(3):
        plan = file_plan(f"out/p{i}.txt", str(i)); pid, _ = await propose(a, plan, f"@exec:{env.server}"); await approve(a, pid, plan); pids.append(pid)
    ex = make_exec(env, execs); await ex.start()
    async def done(pid): return (await a.read_state(S, effect=pid))["effects"][pid]["phase"] == "succeeded"
    for pid in pids: await xuntil(lambda pid=pid: done(pid), what=f"{pid[:8]} done")
    assert len(ex.launched) == 3

async def test_e8_executor_reads_one_effect_with_effect_cursor(env, execs, monkeypatch):
    """E8, X-10: when an outcome is refused the executor reads just that effect (`read {state, effect}`), not the whole state."""
    calls = []
    real = Client.read_state
    async def spy(self, space, at=None, effect=None):
        calls.append(effect); return await real(self, space, at, effect)
    monkeypatch.setattr(Client, "read_state", spy)
    a = await team(env)
    plan = file_plan("ab.txt", "done")
    pid, _ = await propose(a, plan, f"@exec:{env.server}"); await approve(a, pid, plan)
    gate = asyncio.Event()
    async def hook(point, **_):
        if point == "before-report": await gate.wait()
    ex = make_exec(env, execs, hook=hook); await ex.start()
    await xuntil(lambda: ex.launched, what="launch")
    cid = ex.launched[0]
    await xuntil(lambda: [e for e, _ in ex.journal.events(cid)][-2:] == ["ran", "reporting"], what="stalled")
    env.clock.advance(40)
    await a.append(*a.make_entry("effect.outcome", S, {"claim": cid, "result": "unknown", "started": "unknown"}))
    gate.set()
    await xuntil(phase_is(a, pid, "succeeded"), what="resolved")
    assert pid in calls                                                         # the X-10 look at one effect

# ── E1/E2: only the operator loosens; owners can force guarding per agent ──
async def test_e2_session_less_agent_is_held_by_a_task_when_forced(env):
    """E2: with `forceGuarded` an agent with a confirmed operator is guarded (by that operator) whether or not it has a session: a member's
    task naming it holds it for the operator. (Without the flag the same task holds nothing.)"""
    cl, botc = await world(env, force=True)
    assert (await state(cl["alice"]))["agents"] == {BOT: ALICE} and (await state(cl["alice"]))["sessions"] == {}
    e, c = cl["carol"].make_entry("task", S, {"to": [BOT]}, {"ask": "push deploy"})
    await cl["carol"].append(e, c)
    st = await state(cl["alice"])
    assert st["held"] == {BOT: ALICE} and P.guarded_now(st, BOT) == ALICE and st["guarded"] == {}       # held through the policy, not through a choice

async def test_e2_unforced_session_less_agent_is_not_held_by_a_task(env):
    cl, botc = await world(env)
    await cl["carol"].append(*cl["carol"].make_entry("task", S, {"to": [BOT]}, {"ask": "push deploy"}))
    assert (await state(cl["alice"]))["held"] == {}

async def test_e2_host_runs_a_forced_agents_task_strict(env):
    """E2, R-24: the host reads `guardedNow` from the head, so a forced agent in an `auto` session runs an outside task strict."""
    cl, botc = await world(env, force=True)
    agent = ScriptedAgent()
    host, task = start_host(env, botc, "auto", agent)
    await session_id(host)
    await cl["carol"].append(*cl["carol"].make_entry("task", S, {"to": [BOT]}, {"ask": "push deploy"}))
    await until(lambda: agent.calls)
    assert [m for _, m in agent.calls] == ["strict"] and host.guarded_now == ALICE
    assert not [r for r in await records(cl["alice"], author=BOT) if {"release", "unguard"} & set(r["entry"]["body"])]       # the host never lets go
    task.cancel()

# ── E11 ──
def test_e11_x12_backoff_defaults_are_1s_to_30s():
    import inspect
    from han.exec import Executor
    d = inspect.signature(Executor.__init__).parameters
    assert (d["retry_delay"].default, d["max_retry_delay"].default) == (1.0, 30.0)
    d = inspect.signature(Client.__init__).parameters
    assert (d["retry_delay"].default, d["max_retry_delay"].default) == (1.0, 30.0)

async def test_e11_x02_executor_logs_which_clock_it_uses(env, execs, caplog):
    import logging, time
    from han.exec.journal import elapsed_clock_name
    a = await team(env)
    with caplog.at_level(logging.INFO, logger="han.exec"):
        ex = make_exec(env, execs); await ex.start()
    expect = "CLOCK_BOOTTIME" if hasattr(time, "CLOCK_BOOTTIME") else "time.monotonic"
    assert ex.clock_name == elapsed_clock_name() and expect in ex.clock_name
    assert any("elapsed-time clock (X-02)" in r.getMessage() and ex.clock_name in r.getMessage() for r in caplog.records)
    ex2 = make_exec(env, execs, name="exec2", clock=lambda: 0.0)
    assert ex2.clock_name.startswith("injected")

async def test_e11_blob_uploads_are_limited_per_client(env):
    """E11: an actor may hold at most MAX_OPEN_UPLOADS unfinished uploads; one more is `limit` with retryAfterMs, another actor is unaffected."""
    from han.seq.sequencer import MAX_OPEN_UPLOADS
    import hashlib
    env.actor("alice"); env.actor("bob")
    def part(i):
        data = bytes([i]) * 10
        return {"hash": hashlib.sha256(data).hexdigest(), "size": 10, "type": "text/plain"}, data
    for i in range(MAX_OPEN_UPLOADS):
        ref, data = part(i)
        assert env.seq.blob_put(f"@alice:{env.server}", ref, 0, data[:5]) == {"have": 5, "complete": False}
    ref, data = part(200)
    with pytest.raises(WireError) as ex: env.seq.blob_put(f"@alice:{env.server}", ref, 0, data[:5])
    assert ex.value.code == "limit" and ex.value.retry_after_ms
    assert env.seq.blob_put(f"@bob:{env.server}", ref, 0, data[:5])["have"] == 5
    ref0, data0 = part(0)
    assert env.seq.blob_put(f"@alice:{env.server}", ref0, 5, data0[5:])["complete"]        # finishing one frees a slot
    assert env.seq.blob_put(f"@alice:{env.server}", ref, 0, data[:5])["have"] == 5

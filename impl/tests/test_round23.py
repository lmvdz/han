"""Round 23 (interop/DECISIONS-r23.md): F1 forcing per instruction, F2 agent.release, F3 bounded state, F4 read combinations and notices."""
import asyncio
import pytest
from conftest import S, mkspace, member, say
from test_round21_wire import Raw, two, code
from han.client import default_policy
from han.core import P, W, LIMITS, WireError

def _size(reply):
    """The frame a `state` reply travels in: `{id, ok}` with a 16-digit id, as JCS bytes (§3)."""
    return len(P.jcs({"id": 10 ** 15, "ok": reply}))
from fixtures_exec import team, propose, approve, file_plan
from fixtures_host import world, state, session_id, start_host, steer, until, ALICE, CAROL, BOT, EXEC
from han.host import ScriptedAgent

# ── F1: forcing is decided at the instruction, from the profile pinned for it ──
async def test_f1_agent_confirmed_after_joining_is_held_by_a_task_in_a_forced_space(env):
    """F1, P-01: bot joins the forced space BEFORE its operator confirms. Until then an instruction naming it is denied (a forced space needs
    the operator here); once alice confirms, the same kind of task is accepted and holds bot for alice, with no `agents` map anywhere."""
    for n in ("alice", "carol"): env.actor(n)
    env.actor("bot", "agent", operator=ALICE)
    a, c = await env.client("alice"), await env.client("carol")
    pol = default_policy(sessions={"watch": "member", "read": "owner", "steer": "member", "modes": ["auto", "guarded"], "forceGuarded": True})
    await mkspace(a, policy=pol); await member(a, CAROL); await member(a, BOT)
    with pytest.raises(WireError) as ex: await c.append(*c.make_entry("task", S, {"to": [BOT]}, {"ask": "before the confirmation"}))
    assert ex.value.code == "denied" and "operator" in ex.value.message
    await a.append(*a.make_entry("member.set", "profile:" + BOT, {"actor": ALICE, "role": "owner"}))     # the operator confirms, after bot joined
    r = await c.append(*c.make_entry("task", S, {"to": [BOT]}, {"ask": "after the confirmation"}))
    st = await state(a)
    assert "agents" not in st and st["held"] == {BOT: True} and st["guarded"] == {} and r["seq"] == st["head"]

# ── F2: agent.release ──
async def test_f2_agent_release_by_the_guard_with_seen_and_unguard(env):
    """F2: `agent.release {agent}` by the guard ends the hold if its `seen` covers the outside instruction (else `denied`, stale); only the guard
    releases; `unguard` ends a chosen guard. `session.input` carries nothing but the session."""
    cl, botc = await world(env)
    host, task = start_host(env, botc, "guarded", ScriptedAgent())
    sid = await session_id(host)
    hold = await steer(cl["carol"], sid, "push to prod")
    assert (await state(cl["alice"]))["held"] == {BOT: True}
    rel = lambda c, seen, **b: c.make_entry("agent.release", S, {"agent": BOT} | b, seen=seen)
    with pytest.raises(WireError) as ex: await cl["carol"].append(*rel(cl["carol"], hold["seq"]))
    assert ex.value.code == "denied"                                                          # only the guard
    with pytest.raises(WireError) as ex: await cl["alice"].append(*rel(cl["alice"], hold["seq"] - 1))
    assert ex.value.code == "denied" and "stale" in ex.value.message                         # has not seen the instruction
    with pytest.raises(WireError) as ex: await cl["alice"].append(*cl["alice"].make_entry("session.input", S, {"session": sid, "release": True}, {"text": "x"}))
    assert ex.value.code == "invalid"                                                         # session.input is {session} only
    with pytest.raises(WireError) as ex: await cl["alice"].append(*cl["alice"].make_entry("agent.release", S, {"agent": BOT}, {"text": "x"}, seen=hold["seq"]))
    assert ex.value.code == "invalid"                                                         # agent.release has no content (§5)
    await cl["alice"].append(*rel(cl["alice"], hold["seq"]))
    st = await state(cl["alice"])
    assert st["held"] == {} and st["guarded"] == {BOT: True}
    await cl["alice"].append(*rel(cl["alice"], hold["seq"], unguard=True))
    assert (await state(cl["alice"]))["guarded"] == {}
    task.cancel()

# ── F3: closed sessions leave the state; the state is paged by bytes ──
async def test_f3_many_closed_sessions_and_reapprovals_page_within_maxframebytes(env, monkeypatch):
    """F3(a,b,c): sessions opened and closed leave the state and re-approvals do not pile up; with maxFrameBytes lowered to 9000 every page of
    the state of a space with many effects still fits it, the pages join to the whole state, and nothing is lost."""
    a = await team(env, sessions={"watch": "member", "read": "owner", "steer": "member", "modes": ["auto", "guarded"]})
    env.actor("bot", "agent", operator=ALICE); botc = await env.client("bot")
    await member(a, BOT); await a.append(*a.make_entry("member.set", "profile:" + BOT, {"actor": ALICE, "role": "owner"}))
    ids = []
    for i in range(25):
        r = await botc.append(*botc.make_entry("session.open", S, {"steerMode": "auto"}, {"title": f"s{i}"})); ids.append(r["id"])
    for sid in ids: await botc.append(*botc.make_entry("session.close", S, {"session": sid}))
    plans = [file_plan(f"out/{i}.txt", str(i)) for i in range(30)]
    pids = [(await propose(a, p, EXEC))[0] for p in plans]
    for _ in range(3): await approve(a, pids[0], plans[0])                                  # re-approvals of one proposal
    whole = await a.read_state(S)
    assert whole["sessions"] == {} and len(whole["effects"]) == 30                          # closed sessions have left
    assert len([x for x in whole["effects"][pids[0]]["approvals"] if not x.get("dropped")]) <= 3 and not any(
        x.get("dropped") for e in whole["effects"].values() for x in e["approvals"])           # dropped approvals are pruned
    monkeypatch.setitem(LIMITS, "maxFrameBytes", 9000)
    pages = [p async for p in a.state_pages(S)]
    assert len(pages) > 1 and all(_size(p) <= 9000 for p in pages)
    assert all(p["state"]["policy"] == whole["policy"] and p["state"]["head"] == whole["head"] for p in pages)       # scalars and policy repeat on every page
    assert sum(len(p["state"]["effects"]) for p in pages) == 30 and all(set(p["state"]) >= set(W.PAGED[:-1]) for p in pages)       # every map on every page, each item once
    merged = await a.read_state(S)
    assert merged == whole

async def test_f3_state_with_many_live_sessions_pages_by_bytes_and_effect_read_is_whole(env, monkeypatch):
    """F3(c), H3: many live sessions are cut into pages by bytes like every other map; `state {effect}` is the one effect and the repeated
    members, never cut, without `next`."""
    cl, botc = await world(env)
    a = cl["alice"]
    for i in range(40): await botc.append(*botc.make_entry("session.open", S, {"steerMode": "auto"}, {"title": f"s{i}"}))
    plan = file_plan("out/x.txt", "x")
    pid, _ = await propose(a, plan, EXEC)
    whole = await a.read_state(S)
    assert len(whole["sessions"]) == 40
    monkeypatch.setitem(LIMITS, "maxFrameBytes", 6000)
    pages = [p async for p in a.state_pages(S)]
    assert len(pages) > 1 and all(_size(p) <= 6000 for p in pages) and sum(len(p["state"]["sessions"]) for p in pages) == 40
    assert await a.read_state(S) == whole
    one = await a.state(S, effect=pid)
    assert _size(one) <= 6000 and "next" not in one and list(one["state"]["effects"]) == [pid]
    assert set(one["state"]) == {"space", "head", "home", "policy", "effects"}

async def test_f3_a_page_makes_progress_with_one_item_and_an_impossible_page_is_an_assertion(env, monkeypatch):
    a = await team(env)
    for i in range(3): await propose(a, file_plan(f"out/{i}.txt", str(i)), f"@exec:{env.server}")
    whole = await a.read_state(S)
    jl = lambda v: len(P.jcs(v))
    base = {k: v for k, v in whole.items() if k not in W.PAGED}
    base |= {p: {} for p in W.PAGED if p in whole}
    biggest = max(jl(k) + jl(v) + 2 for k, v in whole["effects"].items())
    monkeypatch.setitem(LIMITS, "maxFrameBytes", jl({"id": 10 ** 15, "ok": {"state": base}}) + biggest + W.FRAME_RESERVE + 120)
    pages = [p async for p in a.state_pages(S)]
    assert len(pages) >= 3 and sum(len(p["state"]["effects"]) for p in pages) == 3 and all(_size(p) <= LIMITS["maxFrameBytes"] for p in pages)
    assert await a.read_state(S) == whole
    monkeypatch.setitem(LIMITS, "maxFrameBytes", jl({"id": 10 ** 15, "ok": {"state": base}}) - 50)       # the repeated members alone do not fit
    with pytest.raises(AssertionError): env.seq.state(f"@alice:{env.server}", S, {})                    # impossible by construction: asserted, never `unavailable`

# ── F4: read combinations, notices, integers ──
async def test_f4_selectors_exclude_each_other_by_presence(env):
    """F4, H10: at most one of `at`, `cursor`, `effect`; two or more is `invalid` whatever the values (null included); each alone is fine."""
    a = await team(env)
    pid, _ = await propose(a, file_plan(), f"@exec:{env.server}")
    head = (await a.read_state(S))["head"]
    cur = f"{head}." + ".".join(["0"] * len(W.PAGED))
    for sel in ({"effect": pid, "at": 1}, {"effect": pid, "cursor": cur}, {"cursor": cur, "at": 1}, {"effect": None, "cursor": None}, {"at": 1, "cursor": cur, "effect": pid}):
        with pytest.raises(WireError) as ex: await a.state(S, **sel)
        assert ex.value.code == "invalid", sel
    assert (await a.state(S, effect=pid))["state"]["effects"].keys() == {pid}          # each alone is fine
    assert "state" in await a.state(S, cursor=cur) and (await a.state(S, at=1))["state"]["head"] == 1
    for call in ({"effect": pid, "follow": True}, {"cursor": cur, "follow": True}, {"state": True}):           # `read` takes none of them
        with pytest.raises(WireError) as ex: await a._request("read", {"space": S, "after": 0, **call})
        assert ex.value.code == "invalid", call

async def test_f4_notice_for_an_actor_without_a_live_subscription_is_dropped(env):
    """F4, G-03: a notice is a signal. Nothing is kept for an actor that is not subscribed to its profile, nothing is replayed on a later
    subscribe (not even after a restart), and a live one still arrives."""
    from conftest import Collector
    alice, bob = env.actor("alice"), env.actor("bob")
    a, b = await env.client("alice"), await env.client("bob")
    await mkspace(a)
    await member(a, bob)                                                      # bob is offline: dropped
    assert not env.seq.home.inbox.get(bob) and not env.seq.store.db.execute("SELECT name FROM sqlite_master WHERE name='notices'").fetchall()
    late = Collector(b.subscribe("profile:" + bob)); await asyncio.sleep(0.3)
    assert not [i for i in late.items if i["type"] == "signal"]
    await member(a, bob, "guest")                                             # a live subscription now: delivered
    await late.wait_for(lambda i: i["type"] == "signal")
    late.stop()
    await env.bounce()
    again = Collector(b.subscribe("profile:" + bob)); await asyncio.sleep(0.4)
    assert not [i for i in again.items if i["type"] == "signal"]
    again.stop()

@pytest.mark.parametrize("big", [2 ** 53, -2 ** 53, -(2 ** 53) - 5, 2 ** 53 + 1])
async def test_f4_integers_beyond_2_53_either_way_are_invalid(env, big):
    """F4, G-07 (1a): an integer of 2^53 or more, or below -(2^53 - 1), anywhere in a frame is `invalid`: with a usable id the reply carries
    it and the connection stays; without one it is id 0 and the connection is closed."""
    a, r, bob = await two(env)
    for args in ({"space": S, "after": big}, {"space": S, "after": 0, "limit": big}, {"space": S, "after": 0, "knownHeads": [{"x": [big]}]}):
        rep = await bob.call("read", args)
        assert code(rep) == "invalid" and "2^53" in rep["error"]["message"], (big, args)
    assert "end" in (await bob.items("read", {"space": S, "after": 0, "limit": 1}))[-1]    # the connection is still good
    await bob.send({"id": big, "call": "read", "args": {"space": S, "after": 0}})
    rep = await bob.recv()
    assert rep["id"] == 0 and code(rep) == "invalid" and await bob.closed()

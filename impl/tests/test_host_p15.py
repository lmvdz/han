"""The agent host over a real han-seq: P-15, R-24, T-07/G-05 (docstrings name the rule)."""
import asyncio, hashlib, os, pytest
from conftest import S, say, member, Collector
from fixtures_host import (world, start_host, until, records, state, session_id, steer, ALICE, CAROL, DAVE, BOT, EXEC)
from han.core import P, WireError
from han.host import ScriptedAgent
from han.host.host import CONTEXT_REQUEST

async def test_member_steers_guarded_agent_and_proposal_waits(env):
    """P-15, R-24: a member's steer of a guarded agent is allowed; the turn runs strict; the world action is an effect.propose whose
    `guardedFor` records the agent and whose guard is its operator; the executor's claim is denied until the operator approves."""
    cl, botc = await world(env)
    agent = ScriptedAgent()
    host, task = start_host(env, botc, "guarded", agent, executor=BOT)
    sid = await session_id(host)
    r = await steer(cl["carol"], sid, "push release")
    props = await until(lambda: records(cl["alice"], "effect.propose", BOT))
    assert [m for _, m in agent.calls] == ["strict"]
    st = await state(cl["alice"])
    eff = st["effects"][props[0]["receipt"]["id"]]
    assert st["guarded"] == {BOT: True} and eff["phase"] == "open" and eff["guardedFor"] == [BOT]
    assert P.guards_of(st, {"profiles": {BOT: (await cl["alice"].read_state("profile:" + BOT))["profile"]}}, eff) == [ALICE]
    assert eff["executor"] == BOT and host.guarded_now
    assert props[0]["entry"]["seen"] == r["seq"] and props[0]["entry"]["refs"] == [r["id"]]        # R-17
    # the executor's (here the agent's own) claim is denied; after the operator approves it is allowed
    ex, pid, digest = botc, props[0]["receipt"]["id"], props[0]["entry"]["body"]["planDigest"]
    claim = lambda: ex.make_entry("effect.claim", S, {"proposal": pid, "attempt": 1, "instance": ex.key.key_id, "window": 30})
    with pytest.raises(WireError) as exc: await ex.append(*claim())
    assert exc.value.code == "denied"
    await cl["alice"].append(*cl["alice"].make_entry("effect.approve", S, {"proposal": pid, "planDigest": digest}))
    assert (await ex.append(*claim()))["seq"] > 0
    task.cancel()

async def test_auto_agent_steered_by_member_runs_auto_and_still_proposes(env):
    """P-15, R-24: an auto session has no guard rule: the member's steer runs in auto mode, and a later steer by the guard does too;
    HAN still executes world actions only as effects (nobody is guarded, so `guardedFor` is empty and the proposal is ready)."""
    cl, botc = await world(env)
    agent = ScriptedAgent()
    host, task = start_host(env, botc, "auto", agent)
    sid = await session_id(host)
    await steer(cl["carol"], sid, "push build")
    props = await until(lambda: records(cl["alice"], "effect.propose", BOT))
    await steer(cl["alice"], sid, "hello")
    await until(lambda: len(agent.calls) == 2)
    st = await state(cl["alice"])
    eff = st["effects"][props[0]["receipt"]["id"]]
    assert [m for _, m in agent.calls] == ["auto", "auto"] and st["guarded"] == {} and host.tainted(sid)      # R-24: carol's steer is recorded as outside though nobody is guarded
    assert eff["phase"] == "open" and eff["guardedFor"] == []
    task.cancel()

async def test_guarded_agent_may_delegate_and_the_assignees_effect_needs_the_delegators_guard(env):
    """P-15, R-24: a guarded agent's Task is appended (no refusal); the task records the agent in `guardedFor`; the assignee's effect
    that names the task carries it, so the executor's claim needs the agent's guard (alice) even though the assignee is not guarded."""
    cl, botc = await world(env)
    host, task = start_host(env, botc, "guarded", executor=BOT)
    sid = await session_id(host)
    await steer(cl["carol"], sid, f"delegate to {DAVE} fix it")
    tasks = await until(lambda: records(cl["alice"], "task", BOT))
    tid = tasks[0]["receipt"]["id"]
    st = await state(cl["alice"])
    assert st["tasks"][tid]["guardedFor"] == [BOT] and host.refused == []
    d = cl["dave"]
    plan = {"action": "http.post", "target": "http://example.invalid/x", "args": {}}
    digest = hashlib.sha256(P.jcs(plan)).hexdigest()
    e, c = d.make_entry("effect.propose", S, {"executor": EXEC, "action": "http.post", "planDigest": digest, "repeatable": False, "task": tid}, {"plan": plan})
    pid = (await d.append(e, c))["id"]
    eff = (await state(cl["alice"]))["effects"][pid]
    assert eff["guardedFor"] == [BOT]                                       # dave and exec are not guarded; the task carries the delegator
    ex = cl["exec"]
    claim = lambda: ex.make_entry("effect.claim", S, {"proposal": pid, "attempt": 1, "instance": ex.key.key_id, "window": 30})
    with pytest.raises(WireError) as exc: await ex.append(*claim())
    assert exc.value.code == "denied"
    await cl["alice"].append(*cl["alice"].make_entry("effect.approve", S, {"proposal": pid, "planDigest": digest}))
    assert (await ex.append(*claim()))["seq"] > 0
    task.cancel()

async def test_guarded_agent_does_not_steer_others_and_reports_it_as_data(env):
    """P-15, R-24: a guarded agent's Steer into a session that is not its own is not appended; the host reports the refusal in the
    session stream, and Policy itself denies what the host would not send (a steer of another's session, an approval)."""
    cl, botc = await world(env)
    host, task = start_host(env, botc, "guarded", executor=BOT)
    sid = await session_id(host)
    dsid = (await cl["dave"].append(*cl["dave"].make_entry("session.open", S, {"steerMode": "auto"})))["id"]
    seen = Collector(cl["carol"].watch(S, sid)); await asyncio.sleep(0.2)
    await steer(cl["carol"], sid, f"steer {dsid} do it; push x")
    props = await until(lambda: records(cl["alice"], "effect.propose", BOT))
    await seen.wait_for(lambda i: i["type"] == "output" and i["text"].startswith("refused: guarded: cannot steer"))
    assert await records(cl["alice"], "session.input", BOT) == [] and any("cannot steer" in r for r in host.refused)
    with pytest.raises(WireError) as ex: await steer(botc, dsid, "x")
    assert ex.value.code == "denied"
    pid, digest = props[0]["receipt"]["id"], props[0]["entry"]["body"]["planDigest"]
    with pytest.raises(WireError) as ex: await botc.append(*botc.make_entry("effect.approve", S, {"proposal": pid, "planDigest": digest}))
    assert ex.value.code == "denied"
    await steer(botc, sid, "my own session is fine")                           # its own session is allowed
    seen.stop(); task.cancel()

async def test_guard_steering_its_guarded_agent_is_not_outside_input(env):
    """P-15, R-24: the guard's own steer does not need the stricter mode; the agent may delegate, and the task records it."""
    cl, botc = await world(env)
    agent = ScriptedAgent()
    host, task = start_host(env, botc, "guarded", agent)
    sid = await session_id(host)
    await steer(cl["alice"], sid, f"delegate to {DAVE} please")
    await until(lambda: records(cl["alice"], "task", BOT))
    st = await state(cl["alice"])
    assert [m for _, m in agent.calls] == ["auto"] and not host.tainted(sid)
    assert [t["guardedFor"] for t in st["tasks"].values()] == [[BOT]]
    task.cancel()

async def test_task_naming_a_guarded_agent_is_an_outside_turn(env):
    """P-15, R-24: a task naming a guarded agent from a member becomes a strict turn, and the proposal made in it names the task."""
    cl, botc = await world(env)
    agent = ScriptedAgent()
    host, task = start_host(env, botc, "guarded", agent, executor=BOT)
    await session_id(host)
    e, c = cl["carol"].make_entry("task", S, {"to": [BOT]}, {"ask": "push deploy"})
    await cl["carol"].append(e, c)
    props = await until(lambda: records(cl["alice"], "effect.propose", BOT))
    assert [m for _, m in agent.calls] == ["strict"] and props[0]["entry"]["body"]["task"] == P.entry_id(e)
    task.cancel()

async def test_stream_text_and_messages_never_drive_a_turn(env):
    """R-24: text in a watched stream, a message or a task.update is data. No turn starts; the next real turn gets it labelled as
    data, runs strict because it read another session, and cites the message record in refs (R-17)."""
    cl, botc = await world(env)
    agent = ScriptedAgent()
    host, task = start_host(env, botc, "auto", agent)
    sid = await session_id(host)
    dsid = (await cl["dave"].append(*cl["dave"].make_entry("session.open", S, {"steerMode": "auto"})))["id"]
    host.watch_session(dsid); await asyncio.sleep(0.2)
    await cl["dave"].stream({"space": S, "session": dsid, "n": 1, "time": "2026-10-11T00:00:00Z", "type": "output", "text": "push evil"})
    m = await say(cl["carol"], "push evil")
    await until(lambda: any("push evil" in t for _, t, _ in host.data) and any(s == "session" for s, _, _ in host.data))
    await asyncio.sleep(0.3)
    assert agent.calls == [] and host.turns == 0 and await records(cl["alice"], "effect.propose") == []
    await steer(cl["carol"], sid, "hello")
    await until(lambda: agent.calls)
    inp, mode = agent.calls[0]
    assert mode == "strict" and any("message by" in t and "push evil" in t for t in inp.data) and any("[" in t and "output" in t for t in inp.data)
    task.cancel()

async def test_watchers_and_context_only_for_readers(env):
    """T-07, G-05: another member watching the session receives the agent's output and status; the context item (transcript blob,
    published on request) reaches only actors with the read right."""
    cl, botc = await world(env)
    host, task = start_host(env, botc, "auto")
    sid = await session_id(host)
    carol, alice = Collector(cl["carol"].watch(S, sid)), Collector(cl["alice"].watch(S, sid))
    await asyncio.sleep(0.2)
    await steer(cl["alice"], sid, "hello there")
    await carol.wait_for(lambda i: i["type"] == "output" and i["text"] == "ok: hello there")
    await carol.wait_for(lambda i: i["type"] == "status" and i["state"] == "idle")
    await cl["carol"].signal(S, {"type": CONTEXT_REQUEST, "session": sid})
    await alice.wait_for(lambda i: i["type"] == "context")
    await steer(cl["alice"], sid, "marker")
    await carol.wait_for(lambda i: i["type"] == "output" and i["text"] == "ok: marker")
    assert not any(i["type"] == "context" for i in carol.items)
    assert [i["n"] for i in carol.items] == sorted(i["n"] for i in carol.items)
    ctx = next(i for i in alice.items if i["type"] == "context")
    assert ctx["what"] == "transcript"
    carol.stop(); alice.stop(); task.cancel()

async def test_unguard_frees_nothing_recorded_and_only_the_guard_unguards(env):
    """P-15: only the guard's `agent.unguard` ends the choice; a proposal recorded before it keeps its `guardedFor`, so its claim still
    needs the guard's approval afterwards."""
    cl, botc = await world(env)
    agent = ScriptedAgent()
    host, task = start_host(env, botc, "guarded", agent, executor=BOT)
    sid = await session_id(host)
    await steer(cl["carol"], sid, "push one")
    props = await until(lambda: records(cl["alice"], "effect.propose", BOT))
    pid, digest = props[0]["receipt"]["id"], props[0]["entry"]["body"]["planDigest"]
    with pytest.raises(WireError) as ex: await cl["carol"].append(*cl["carol"].make_entry("agent.unguard", S, {"agent": BOT}))
    assert ex.value.code == "denied"                                                   # only the guard unguards
    await cl["alice"].append(*cl["alice"].make_entry("agent.unguard", S, {"agent": BOT}))
    st = await state(cl["alice"])
    assert st["guarded"] == {} and st["effects"][pid]["guardedFor"] == [BOT]
    await until(lambda: not host.guarded_now)
    claim = lambda: botc.make_entry("effect.claim", S, {"proposal": pid, "attempt": 1, "instance": botc.key.key_id, "window": 30})
    with pytest.raises(WireError) as ex: await botc.append(*claim())
    assert ex.value.code == "denied"                                                   # recorded work is not freed
    await cl["alice"].append(*cl["alice"].make_entry("effect.approve", S, {"proposal": pid, "planDigest": digest}))
    assert (await botc.append(*claim()))["seq"] > 0
    with pytest.raises(WireError) as ex: await cl["alice"].append(*cl["alice"].make_entry("agent.unguard", S, {"agent": BOT}))
    assert ex.value.code == "denied"                                                   # no longer guarded
    task.cancel()

async def test_taint_survives_a_restart_and_an_unguard(env):
    """R-24: once an outside instruction has entered a guarded agent's session, every later turn of that session runs strict: after the
    host restarts (the taint is in its local store, not the log), after the agent is unguarded, and for the guard's own steer."""
    cl, botc = await world(env)
    agent1 = ScriptedAgent()
    host, task = start_host(env, botc, "guarded", agent1)
    sid = await session_id(host)
    await steer(cl["carol"], sid, "hello")
    await until(lambda: agent1.calls)
    assert [m for _, m in agent1.calls] == ["strict"] and host.tainted(sid)
    task.cancel(); host.stop(); await botc.close()                                     # the process is gone
    await cl["alice"].append(*cl["alice"].make_entry("agent.unguard", S, {"agent": BOT}))
    botc2 = await env.client("bot")
    agent2 = ScriptedAgent()
    host2, task2 = start_host(env, botc2, "auto", agent2)
    await until(lambda: host2.state and host2.session)
    assert host2.session == sid and host2.tainted(sid) and not host2.tainted("other") and not host2.guarded_now
    await steer(cl["alice"], sid, "hello again")                                       # the guard's own steer, the agent no longer guarded
    await until(lambda: agent2.calls)
    assert [m for _, m in agent2.calls] == ["strict"]
    task2.cancel()

async def test_untainted_session_of_a_guarded_agent_stays_auto_for_the_guard_after_a_restart(env):
    """R-24: without an outside instruction there is no taint; a restart does not invent one."""
    cl, botc = await world(env)
    host, task = start_host(env, botc, "guarded", ScriptedAgent())
    sid = await session_id(host)
    task.cancel(); host.stop(); await botc.close()
    botc2 = await env.client("bot")
    agent2 = ScriptedAgent()
    host2, task2 = start_host(env, botc2, "guarded", agent2)
    await steer(cl["alice"], sid, "hello")
    await until(lambda: agent2.calls)
    assert [m for _, m in agent2.calls] == ["auto"] and not host2.tainted(sid)
    task2.cancel()

async def test_proposal_in_a_task_driven_turn_names_its_task_and_carries_the_delegators_guard(env):
    """R-24 MUST: every effect.propose a host makes in a task-driven turn names the driving task, so the task's `guardedFor` (here the
    guarded delegator, which is neither the proposer nor the executor) carries to the effect."""
    cl, botc = await world(env)
    BOT2 = env.actor("bot2", "agent", operator=ALICE)
    await cl["alice"].append(*cl["alice"].make_entry("member.set", "profile:" + BOT2, {"actor": ALICE, "role": "owner"}))
    await member(cl["alice"], BOT2)
    await botc.append(*botc.make_entry("session.open", S, {"steerMode": "guarded"}, {"title": "work"}))      # bot chooses guarding
    b2 = await env.client("bot2")
    agent = ScriptedAgent()
    host, task = start_host(env, b2, "auto", agent, state=os.path.join(env.tmp, "bot2.state.db"))
    await session_id(host)
    e, c = botc.make_entry("task", S, {"to": [BOT2]}, {"ask": "push deploy"})
    tid = (await botc.append(e, c))["id"]
    props = await until(lambda: records(cl["alice"], "effect.propose", BOT2))
    assert props[0]["entry"]["body"]["task"] == tid
    st = await state(cl["alice"])
    assert st["tasks"][tid]["guardedFor"] == [BOT] and st["effects"][props[0]["receipt"]["id"]]["guardedFor"] == [BOT]
    assert [m for _, m in agent.calls] == ["strict"]                                    # bot2 itself is not guarded; its delegator is (R-24, round 28)
    task.cancel()

async def test_guarded_status_says_it_is_advisory_without_a_sandbox(env):
    """§8.4: with no OS sandbox the host's `status` items of a guarded session carry detail "guarded: advisory (no sandbox)"; an auto
    session's carry none."""
    cl, botc = await world(env)
    host, task = start_host(env, botc, "guarded")
    sid = await session_id(host)
    seen = Collector(cl["alice"].watch(S, sid)); await asyncio.sleep(0.2)
    await steer(cl["alice"], sid, "hello")                                  # the guard's own steer: working, then idle
    await seen.wait_for(lambda i: i["type"] == "status" and i["state"] == "idle" and i["n"] > 2)
    statuses = [i for i in seen.items if i["type"] == "status"]
    assert statuses and all(i["detail"] == "guarded: advisory (no sandbox)" for i in statuses)
    seen.stop(); task.cancel()

async def test_auto_status_has_no_advisory_detail(env):
    cl, botc = await world(env)
    host, task = start_host(env, botc, "auto")
    sid = await session_id(host)
    seen = Collector(cl["alice"].watch(S, sid)); await asyncio.sleep(0.2)
    await steer(cl["carol"], sid, "hello")
    await seen.wait_for(lambda i: i["type"] == "status" and i["state"] == "idle" and i["n"] > 2)
    assert all("detail" not in i for i in seen.items if i["type"] == "status")
    seen.stop(); task.cancel()

async def _bot2(env, cl):
    BOT2 = env.actor("bot2", "agent", operator=ALICE)
    await cl["alice"].append(*cl["alice"].make_entry("member.set", "profile:" + BOT2, {"actor": ALICE, "role": "owner"}))
    await member(cl["alice"], BOT2)
    return BOT2, await env.client("bot2")

async def test_unguarded_delegate_of_a_guarded_agent_runs_strict(env):
    """R-24 (round 28, R1): bot2 is not guarded, but the task's author bot is guarded at its seq, so bot2's host runs the turn strict; the
    host reads the author's guard facts at that seq (`state {agent: <author>, at}`), and the guard's own facts say not guarded."""
    cl, botc = await world(env)
    BOT2, b2 = await _bot2(env, cl)
    await botc.append(*botc.make_entry("session.open", S, {"steerMode": "guarded"}, {"title": "work"}))
    agent = ScriptedAgent()
    host, task = start_host(env, b2, "auto", agent, state=os.path.join(env.tmp, "bot2.state.db"))
    await session_id(host)
    r = await botc.append(*botc.make_entry("task", S, {"to": [BOT2]}, {"ask": "hello"}))
    await until(lambda: agent.calls)
    assert [m for _, m in agent.calls] == ["strict"] and not host.guarded_now
    assert await host._author_guarded(BOT, r["seq"], r["profiles"]) and not await host._author_guarded(ALICE, r["seq"], r["profiles"])
    task.cancel()

async def test_task_with_guarded_for_gives_a_strict_turn(env):
    """R-24 (round 28, R1): carol, who is not guarded and is an assignee, adds a subtask of a guarded agent's task for the unguarded bot2; the subtask carries
    `guardedFor` [bot], so the turn it drives is strict, though neither its author nor bot2 is guarded."""
    cl, botc = await world(env)
    BOT2, b2 = await _bot2(env, cl)
    await botc.append(*botc.make_entry("session.open", S, {"steerMode": "guarded"}, {"title": "work"}))
    parent = (await botc.append(*botc.make_entry("task", S, {"to": [BOT, CAROL]}, {"ask": "parent"})))["id"]
    agent = ScriptedAgent()
    host, task = start_host(env, b2, "auto", agent, state=os.path.join(env.tmp, "bot2.state.db"))
    await session_id(host)
    sub = (await cl["carol"].append(*cl["carol"].make_entry("task", S, {"to": [BOT2], "parent": parent}, {"ask": "hello"})))["id"]
    await until(lambda: agent.calls)
    st = await state(cl["alice"])
    assert st["tasks"][sub]["guardedFor"] == [BOT] and not host.guarded_now
    assert [m for _, m in agent.calls] == ["strict"]
    task.cancel()

async def test_outside_instruction_before_guarding_makes_later_turns_strict(env):
    """R-24 (round 28, R6): carol's steer is recorded as outside while bot is unguarded (and runs auto). Once bot chooses guarding, the guard's
    own steer of the same session runs strict; a session that took no outside instruction stays auto."""
    cl, botc = await world(env)
    agent = ScriptedAgent()
    host, task = start_host(env, botc, "auto", agent)
    sid = await session_id(host)
    await steer(cl["carol"], sid, "hello")
    await until(lambda: len(agent.calls) == 1)
    assert [m for _, m in agent.calls] == ["auto"] and host.tainted(sid) and not host.guarded_now
    await steer(cl["alice"], sid, "again")                                  # still unguarded: auto
    await until(lambda: len(agent.calls) == 2)
    await botc.append(*botc.make_entry("session.open", S, {"steerMode": "guarded"}, {"title": "other"}))
    await steer(cl["alice"], sid, "now guarded")
    await until(lambda: len(agent.calls) == 3)
    assert [m for _, m in agent.calls] == ["auto", "auto", "strict"]
    task.cancel()

async def test_taint_taken_while_unguarded_survives_a_restart(env):
    """R-24 (round 28, R6): the taint is in the host's store: carol's steer is taken while bot is unguarded, the host restarts, bot then
    chooses guarding, and the guard's own steer runs strict. The guard's steer of an untainted session of a guarded agent stays auto."""
    cl, botc = await world(env)
    agent1 = ScriptedAgent()
    host, task = start_host(env, botc, "auto", agent1)
    sid = await session_id(host)
    await steer(cl["carol"], sid, "hello")
    await until(lambda: agent1.calls)
    assert [m for _, m in agent1.calls] == ["auto"] and host.tainted(sid)
    task.cancel(); host.stop(); await botc.close()
    botc2 = await env.client("bot")
    await botc2.append(*botc2.make_entry("session.open", S, {"steerMode": "guarded"}, {"title": "other"}))
    agent2 = ScriptedAgent()
    host2, task2 = start_host(env, botc2, "auto", agent2)
    await until(lambda: host2.state and host2.session)
    assert host2.session == sid and host2.tainted(sid) and host2.guarded_now
    await steer(cl["alice"], sid, "hello again")
    await until(lambda: agent2.calls)
    assert [m for _, m in agent2.calls] == ["strict"]
    task2.cancel()

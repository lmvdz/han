"""The executor end to end against a real han-seq: happy path, the launch-deadline proof, abandon (X-10), actions."""
import asyncio, os
from conftest import S
from fixtures_exec import (execs, team, make_exec, file_plan, propose, approve, state, until, phase_is, kinds, sandbox,
                           CountingHttp)                     # noqa: F401  (execs is a fixture)
from han.core import P
from han.exec.actions import Actions, Refused

def outcomes(ks): return [(b["result"], b["started"]) for k, b in ks if k == "effect.outcome"]

async def test_happy_path(env, execs):
    """X-01…X-05, P-07, P-09: a human proposes and approves; the executor claims (instance key, window = launchWindow, attempt 1),
    launches file.write, reports succeeded/started yes; the file exists and the effect is succeeded."""
    a = await team(env)
    plan = file_plan("out/a.txt", "hello")
    pid, _ = await propose(a, plan, f"@exec:{env.server}")
    ex = make_exec(env, execs); await ex.start()
    await asyncio.sleep(0.3)
    assert not [k for k, _ in await kinds(a) if k == "effect.claim"]          # E3: not claimable before approval
    await approve(a, pid, plan)
    await until(phase_is(a, pid, "succeeded"), what="succeeded")
    with open(os.path.join(sandbox(env), "out", "a.txt"), encoding="utf-8") as fh: assert fh.read() == "hello"
    ks = await kinds(a)
    claim = next(b for k, b in ks if k == "effect.claim")
    assert claim == {"proposal": pid, "attempt": 1, "instance": "k1", "window": 30}
    assert outcomes(ks) == [("succeeded", "yes")] and len(ex.launched) == 1
    names = [e for e, _ in ex.journal.events(ex.launched[0])]
    assert names == ["claiming", "launching", "ran", "reporting", "reported"]

async def test_http_post_allow_list(env, execs):
    """Actions: http.post reaches an allow-listed URL (once) with the plan's body; a URL outside the list is never claimed."""
    a = await team(env)
    http = await CountingHttp().start()
    try:
        ok = {"action": "http.post", "target": http.url, "args": {"n": 1}}
        bad = {"action": "http.post", "target": "http://127.0.0.1:9/other", "args": {"n": 2}}
        p1, _ = await propose(a, ok, f"@exec:{env.server}"); await approve(a, p1, ok)
        p2, _ = await propose(a, bad, f"@exec:{env.server}"); await approve(a, p2, bad)
        ex = make_exec(env, execs, allow_urls=["http://127.0.0.1:%s/" % http.url.split(":")[2].split("/")[0]]); await ex.start()
        await until(phase_is(a, p1, "succeeded"))
        await asyncio.sleep(0.4)
        assert http.hits == 1 and http.bodies == ['{"n": 1}']
        assert (await state(a))["effects"][p2]["phase"] == "ready" and len((await state(a))["effects"][p2]["claims"]) == 0
    finally: await http.stop()

def test_sandbox_and_urls(tmp_path):
    """Actions: file.write is confined to the sandbox (no .., no absolute paths); http.post only to listed prefixes."""
    act = Actions(str(tmp_path / "sb"), ["http://ok/"])
    act.check({"action": "file.write", "target": "a/b.txt", "args": {"content": "x"}})
    for bad in ("../x", "a/../../x", "/etc/x", "C:\\x", "", "."):
        try: act.check({"action": "file.write", "target": bad, "args": {"content": "x"}}); raise AssertionError(bad)
        except Refused: pass
    act.check({"action": "http.post", "target": "http://ok/hook", "args": {}})
    for bad in ("http://evil/", "http://okay/"):
        try: act.check({"action": "http.post", "target": bad}); raise AssertionError(bad)
        except Refused: pass

async def test_deadline_refuses_late_launch(env, execs):
    """X-04, X-05: the receipt arrives after the window minus the stall bound (injected on the executor's own clock): it journals
    `expired`, does not launch, reports failed/started no; the next claim (attempt 2, a new window) launches."""
    a = await team(env)
    plan = file_plan("late.txt", "x")
    pid, _ = await propose(a, plan, f"@exec:{env.server}"); await approve(a, plan=plan, pid=pid)
    now, slow = [100.0], []
    def hook(point, claim=None, **_):
        if point == "after-receipt" and not slow: slow.append(claim); now[0] += 25          # 25 s ≥ 30 − 10
    ex = make_exec(env, execs, clock=lambda: now[0], hook=hook); await ex.start()
    await until(phase_is(a, pid, "succeeded"))
    ks = await kinds(a)
    assert [k for k, _ in ks if k.startswith("effect.") and k != "effect.propose" and k != "effect.approve"] == \
        ["effect.claim", "effect.outcome", "effect.claim", "effect.outcome"]
    assert outcomes(ks) == [("failed", "no"), ("succeeded", "yes")]
    assert ex.launched and slow[0] not in ex.launched and len(ex.launched) == 1
    first = [e for e, _ in ex.journal.events(slow[0])]
    assert first == ["claiming", "launching", "expired", "reporting", "reported"]
    assert [b["attempt"] for k, b in ks if k == "effect.claim"] == [1, 2]

async def test_deadline_boundary(env, execs):
    """X-04: exactly window − stallBound elapsed is already too late; just under it launches."""
    a = await team(env)
    plan = file_plan("edge.txt", "x")
    pid, _ = await propose(a, plan, f"@exec:{env.server}"); await approve(a, pid, plan)
    now = [0.0]
    def hook(point, **_):
        if point == "after-receipt": now[0] += 19.9
    ex = make_exec(env, execs, clock=lambda: now[0], hook=hook); await ex.start()
    await until(phase_is(a, pid, "succeeded"))
    assert outcomes(await kinds(a)) == [("succeeded", "yes")]

async def test_abandon_then_resolve(env, execs):
    """P-09, P-10, X-10, R-16: the executor stalls before reporting; an owner abandons after A = 33 s; the executor's outcome is
    denied and it reports what happened with effect.resolve (evidence attached); the effect ends succeeded, launched once."""
    a = await team(env)
    plan = file_plan("ab.txt", "done")
    pid, _ = await propose(a, plan, f"@exec:{env.server}"); await approve(a, pid, plan)
    gate = asyncio.Event()
    async def hook(point, **_):
        if point == "before-report": await gate.wait()
    ex = make_exec(env, execs, hook=hook); await ex.start()
    await until(lambda: ex.launched, what="launch")
    cid = ex.launched[0]
    await until(lambda: [e for e, _ in ex.journal.events(cid)][-2:] == ["ran", "reporting"], what="stalled")
    env.clock.advance(40)
    await a.append(*a.make_entry("effect.outcome", S, {"claim": cid, "result": "unknown", "started": "unknown"}))
    assert (await state(a))["effects"][pid]["phase"] == "unknown"
    gate.set()
    await until(phase_is(a, pid, "succeeded"), what="resolved")
    ks = await kinds(a)
    assert [k for k, _ in ks][-2:] == ["effect.outcome", "effect.resolve"] and ks[-1][1]["result"] == "succeeded"
    assert [e for e, _ in ex.journal.events(cid)][-2:] == ["refused", "resolved"] and len(ex.launched) == 1
    rec = (await a.read_all(S, 0))[-1]["record"]
    evidence = await a.blob_get(rec["content"]["evidence"], S)
    assert cid.encode() in evidence and b'"ran"' in evidence

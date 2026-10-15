"""The `han` CLI end to end: `python -m han.cli` as a real subprocess against a real han-seq (argument parsing only; the work is han.client)."""
import asyncio, json, os, subprocess, sys
from conftest import S
from han import keys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

async def han(env, name, *args, ok=True):
    """Run `han` as `name`; returns stdout. Fails the test on a non-zero exit unless ok=False (then returns (code, stdout, stderr))."""
    e = dict(os.environ, HAN_URL=env.srv.url, HAN_ACTOR=f"@{name}:{env.server}", HAN_KEY=os.path.join(env.tmp, f"{name}.key.json"),
             HAN_OUTBOX=os.path.join(env.tmp, f"{name}.cli-outbox.db"), PYTHONIOENCODING="utf-8")
    r = await asyncio.to_thread(subprocess.run, [sys.executable, "-m", "han.cli", *args], cwd=ROOT, env=e, capture_output=True, text=True, timeout=60)
    if ok:
        assert r.returncode == 0, f"han {' '.join(args)} -> {r.returncode}\n{r.stdout}\n{r.stderr}"
        return r.stdout
    return r.returncode, r.stdout, r.stderr

def last_id(out): return out.split("id ")[-1].split()[0]

async def test_keygen_space_member_propose_approve_status(env, tmp_path):
    """P-07, P-08, 6.4: keygen, create a space, add a member, propose with a plan file, approve (the plan is shown and hash-checked first);
    `status` shows the effect `ready`."""
    key = str(tmp_path / "ana.key.json")
    out = await han(env, "ana", "keygen", "--out", key)                       # the CLI makes the key; the server learns its public key
    pub = next(l.split()[1] for l in out.splitlines() if l.startswith("public"))
    assert keys.load(key).pub == pub and (await han(env, "ana", "keygen", "--out", key, ok=False))[0] == 1       # refuses to overwrite
    env.seq.register("@ana:a.localhost", "human", "k1", pub)
    env.actor("bob"); env.actor("exec", "service", operator="@ana:a.localhost")
    pol = '[{"action": "fs.write", "approvers": ["owner"]}]'
    assert "seq 1" in await han(env, "ana", "space", "create", S, "--name", "room", "--set", f"approvals={pol}")
    await han(env, "ana", "member", "set", S, "@bob:a.localhost", "member")
    await han(env, "ana", "member", "set", S, "@exec:a.localhost", "member")
    await han(env, "bob", "message", S, "ship the report", "--to", "@ana:a.localhost")
    plan = tmp_path / "plan.json"; plan.write_text(json.dumps({"action": "fs.write", "target": "out/report.txt", "args": {"text": "hi"}}))
    pid = last_id(await han(env, "bob", "propose", S, "--executor", "@exec:a.localhost", "--plan", str(plan)))
    st = await han(env, "bob", "status", S)
    assert " open " in st and "fs.write" in st and "@bob:a.localhost member" in st
    code, _, err = await han(env, "bob", "approve", S, pid[:10], ok=False)       # a member is not an approver here
    assert code == 1 and "denied" in err
    out = await han(env, "ana", "approve", S, pid[:10], "--note", "ok")
    assert "out/report.txt" in out                                               # the plan was displayed before signing
    st = await han(env, "ana", "status", S)
    assert f"{pid[:8]} open" in st and "approvals 1" in st
    listing = await han(env, "bob", "read", S)
    assert "effect.propose" in listing and "ship the report" in listing and "effect.approve" in listing
    pid2 = last_id(await han(env, "bob", "propose", S, "--executor", "@exec:a.localhost", "--plan", str(plan)))
    await han(env, "ana", "cancel", S, pid2)
    assert f"{pid2[:8]} cancelled" in await han(env, "ana", "status", S)
    assert "denied" in (await han(env, "bob", "member", "set", S, "@bob:a.localhost", "owner", ok=False))[2]
    # P-07a, G-01: the executor refuses its own open effect with a reason and an evidence file; a bystander's cancel is denied;
    # the stored cancel carries both in its content and the blob is then readable by a member
    await han(env, "ana", "member", "set", S, "@exec:a.localhost", "member")
    ev = tmp_path / "why.json"; ev.write_text('{"why": "the checks failed"}')
    pid3 = last_id(await han(env, "ana", "propose", S, "--executor", "@exec:a.localhost", "--plan", str(plan)))
    code, _, err = await han(env, "bob", "cancel", S, pid3[:10], "--reason", "no", ok=False)
    assert code == 1 and "denied" in err
    code, _, err = await han(env, "exec", "cancel", S, pid3[:10], "--reason", "x" * 1025, ok=False)
    assert code == 1 and "1,024" in err
    await han(env, "exec", "cancel", S, pid3[:10], "--reason", "cannot run this", "--evidence", str(ev))
    assert f"{pid3[:8]} cancelled" in await han(env, "ana", "status", S)
    rec = [json.loads(l) for l in (await han(env, "bob", "read", S, "--json")).splitlines()][-1]
    assert rec["entry"]["kind"] == "effect.cancel" and rec["content"]["reason"] == "cannot run this"
    assert json.loads(await (await env.client("bob")).blob_get(rec["content"]["evidence"], S)) == {"why": "the checks failed"}

async def test_task_and_read_json(env):
    """6.3, 3: `task` assigns work with an ask, `read --json` prints the records as JSON lines."""
    env.actor("ana"); env.actor("bob")
    await han(env, "ana", "space", "create", S)
    await han(env, "ana", "member", "set", S, "@bob:a.localhost", "member")
    await han(env, "ana", "task", S, "write the docs", "--to", "@bob:a.localhost")
    lines = (await han(env, "bob", "read", S, "--json", "--after", "1")).splitlines()
    recs = [json.loads(l) for l in lines]
    assert [r["entry"]["kind"] for r in recs] == ["member.set", "task"] and recs[1]["content"] == {"ask": "write the docs"}
    assert "open" in await han(env, "bob", "status", S)

async def test_session_steer_guarded_delegation_and_unguard(env):
    """P-15, R-24: a member steers a guarded agent through the CLI (allowed, inert), the agent delegates (`status` shows `guardedFor` on the
    task), only the guard ends the guarded choice with `agent unguard`, and `watch` prints the session's StreamItems."""
    ana = env.actor("ana"); carol = env.actor("carol"); bot = env.actor("bot", "agent", operator=ana)
    c_ana = await env.client("ana")
    pol = '{"watch": "member", "read": "member", "steer": "member", "modes": ["auto", "guarded"]}'
    await han(env, "ana", "space", "create", S, "--set", f"sessions={pol}")
    await han(env, "ana", "member", "set", S, carol, "member")
    await han(env, "ana", "member", "set", S, bot, "member")
    await c_ana.append(*c_ana.make_entry("member.set", "profile:" + bot, {"actor": ana, "role": "owner"}))     # the operator confirms
    sid = last_id(await han(env, "bot", "session", "open", S, "--mode", "guarded", "--title", "work"))
    st = await han(env, "ana", "status", S)
    assert f"{sid[:8]} {bot}  open  guarded" in st and "held" not in st
    assert f"{bot}  guarded (guard {ana})" in st and f"guarded agents: {bot}" in st             # F1: per agent, from the current state
    await han(env, "carol", "session", "steer", S, sid[:8], "push to prod")                    # an outside instruction is allowed (inert)
    await han(env, "bot", "task", S, "write the docs", "--to", carol)                           # a guarded agent may delegate
    st = await han(env, "ana", "status", S)
    assert f"to {carol}  guardedFor {bot}" in st                                                 # recorded on the work, at creation
    code, _, err = await han(env, "carol", "agent", "unguard", S, bot, ok=False)               # only the guard unguards
    assert code == 1 and "denied" in err
    code, _, err = await han(env, "ana", "agent", "unguard", S, ana, ok=False)                  # an agent that is not guarded can't be
    assert code == 1 and "denied" in err
    code, _, err = await han(env, "ana", "agent", "release", S, bot, ok=False)                  # the old kind is gone (argparse refuses it)
    assert code != 0
    await han(env, "ana", "agent", "unguard", S, bot)
    st = await han(env, "ana", "status", S)
    assert "guarded agents: none" in st and f"to {carol}  guardedFor {bot}" in st            # unguarded; recorded work keeps its guardedFor
    bc = await env.client("bot")
    watcher = asyncio.create_task(han(env, "carol", "watch", S, sid[:8], "--count", "1", "--timeout", "30"))
    n = 0
    while not watcher.done():                                                             # watch is live-only: publish until the subprocess has joined
        n += 1
        await bc.stream({"space": S, "session": sid, "n": n, "time": "2026-10-10T12:00:00Z", "type": "output", "text": "pushed"})
        await asyncio.sleep(0.4)
    assert "output: pushed" in await watcher
    await han(env, "bot", "session", "close", S, sid)
    assert sid[:8] not in await han(env, "ana", "status", S)                              # F3(b): a closed session has left the state

async def test_status_shows_forced_agents_per_agent_f1(env):
    """F1: in a space with forceGuarded the status names a non-human an instruction has reached as `forced (operator …)`, from its profile;
    nobody is listed as forced in a space without the flag, and a human is never forced."""
    ana = env.actor("ana"); carol = env.actor("carol"); bot = env.actor("bot", "agent", operator=ana)
    c_ana = await env.client("ana")
    pol = '{"watch": "member", "read": "member", "steer": "member", "modes": ["auto", "guarded"], "forceGuarded": true}'
    await han(env, "ana", "space", "create", S, "--set", f"sessions={pol}")
    await han(env, "ana", "member", "set", S, carol, "member"); await han(env, "ana", "member", "set", S, bot, "member")
    await c_ana.append(*c_ana.make_entry("member.set", "profile:" + bot, {"actor": ana, "role": "owner"}))     # the operator confirms, after bot joined
    await han(env, "carol", "task", S, "deploy", "--to", bot); await han(env, "carol", "task", S, "chat", "--to", ana)
    st = await han(env, "ana", "status", S)
    assert f"{bot}  forced (guard {ana})" in st and "held" not in st and f"{ana}  forced" not in st and "agents:" in st

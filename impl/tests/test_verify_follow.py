"""R-14: the verifier follows a live space, retains what it verified, and refuses a state file that no longer verifies."""
import asyncio, json, pytest
from conftest import S, mkspace, member, say
from fixtures_verify import verifier, until
from han.verify import StateError

async def test_follow_live_space_ok_through_head(env, tmp_path):
    """R-14: a mirror follows a live space (backlog, then new records) and reports `ok through head`; profile chains are retained (§4.3)."""
    env.actor("alice"); bob = env.actor("bob")
    a, b = await env.client("alice"), await env.client("bob")
    await mkspace(a); await member(a, bob); await say(b, "hello")
    v = verifier(env, await env.client("alice", fresh_outbox=True), str(tmp_path / "v.state"))
    seen = []
    task = asyncio.create_task(v.follow(S, seen.append))
    await until(lambda: v.status(S) == {"state": "ok", "through": 3})
    await say(a, "live 1"); await say(b, "live 2")
    await until(lambda: v.status(S)["through"] == 5)
    assert v.status(S) == {"state": "ok", "through": 5} and [s["state"] for s in seen] == ["ok"] * len(seen)
    assert v.c.head("profile:@alice:a.localhost") == 1 and v.c.head("profile:@bob:a.localhost") == 1      # retained with the space
    assert v.heads()[0]["seq"] == 5 and v.heads()[0]["space"] == S
    task.cancel()
    with pytest.raises(asyncio.CancelledError): await task
    assert v.status(S)["state"] == "stopped" and v.status(S)["through"] == 5
    v.close()

async def test_state_file_resumes_and_is_reverified(env, tmp_path):
    """R-14: what a verifier persisted is re-verified on start (a restarted verifier needs no re-download); a doctored state file is rejected."""
    env.actor("alice"); a = await env.client("alice")
    await mkspace(a); await say(a, "one"); await say(a, "two")
    path = str(tmp_path / "v.state")
    v = verifier(env, a, path)
    assert await v.verify(S) == {"state": "ok", "through": 3}; v.close()
    await say(a, "three")
    v2 = verifier(env, a, path)
    assert v2.status(S) == {"state": "ok", "through": 3}                    # loaded, no network
    assert await v2.verify(S) == {"state": "ok", "through": 4}; v2.close()
    text = open(path, encoding="utf-8").read()
    open(path, "w", encoding="utf-8").write(text.replace('"two"', '"2wo"'))
    with pytest.raises(StateError): verifier(env, a, path)

async def test_redaction_deletes_retained_content(env, tmp_path):
    """R-14, P-12: when a redact record is verified, the target's content disappears from the verifier's state file too."""
    env.actor("alice"); a = await env.client("alice")
    await mkspace(a)
    secret = await say(a, "a secret text")
    path = str(tmp_path / "v.state")
    v = verifier(env, a, path)
    assert (await v.verify(S))["through"] == 2 and "a secret text" in open(path, encoding="utf-8").read()
    await a.append(*a.make_entry("redact", S, {"target": secret["id"]}))
    assert await v.verify(S) == {"state": "ok", "through": 3}
    assert "a secret text" not in open(path, encoding="utf-8").read()
    v.close()
    v2 = verifier(env, a, path)                                           # and the redacted record still verifies through its proof
    assert v2.status(S) == {"state": "ok", "through": 3}; v2.close()

async def test_cli_follow_live_until_seq(env, tmp_path):
    """R-14, R-15: `python -m han.verify follow` (no --once) keeps following a live space, prints each status change and exits 0 at --until."""
    import os, subprocess, sys
    env.actor("alice"); a = await env.client("alice")
    await mkspace(a)
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    cmd = [sys.executable, "-m", "han.verify", "follow", "--url", env.srv.url, "--actor", "@alice:a.localhost", "--key", os.path.join(env.tmp, "alice.key.json"),
           "--space", S, "--until", "3", "--gossip-interval", "1", "--server-key", f"a.localhost={os.path.join(env.tmp, 'server-key.json')}"]
    run = asyncio.create_task(asyncio.to_thread(subprocess.run, cmd, cwd=root, capture_output=True, text=True, timeout=60))
    await asyncio.sleep(1.5)
    await say(a, "two"); await say(a, "three")
    r = await run
    assert r.returncode == 0 and r.stdout.splitlines()[0] == "ok through 1" and r.stdout.splitlines()[-1] == "ok through 3", r.stdout + r.stderr

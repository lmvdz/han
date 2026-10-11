"""R-14, R-15, R-02: a doctored sequencer is caught, and the proof is portable (checked offline with only the server's public key)."""
import asyncio, copy, json, os, subprocess, sys
from conftest import S, T0, mkspace, member, say
from fixtures_verify import verifier, until, forge_head, doctor_reads
from han import keys
from han.client import default_policy
from han.verify import Verifier, check_proof

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

def third_party(env):
    """What a stranger has: the server public key (never the proof's bundled copy)."""
    return {env.server: {"h1": {"publicKey": env.seq.key.pub, "from": "1970-01-01T00:00:00Z"}}}

def skip_policy(env):
    """Make the home order whatever it is given (a doctored sequencer). Returns a function that restores it."""
    orig = env.seq.home.append
    env.seq.home.append = lambda e, c, p, skip_policy=False: orig(e, c, p, skip_policy=True)
    def restore(): env.seq.home.append = orig
    return restore

async def test_forbidden_accept_proof_checks_offline(env, tmp_path):
    """R-14, 7.5 (sequencer accepts a forbidden entry): the verifier replays Policy, reports faulty forbidden-accept through the last good seq,
    and the proof verifies offline (library and `python -m han.verify proof`), but not after tampering or with other keys."""
    env.actor("alice"); bob = env.actor("bob")
    a, b = await env.client("alice"), await env.client("bob")
    await mkspace(a); await member(a, bob); await say(b, "fine")
    restore = skip_policy(env)
    await b.append(*b.make_entry("member.set", S, {"actor": bob, "role": "owner"}))                 # a member promotes himself
    restore()
    v = verifier(env, await env.client("alice", fresh_outbox=True), str(tmp_path / "v.state"))
    st = await v.verify(S)
    assert st["state"] == "faulty" and st["through"] == 3 and st["proof"]["kind"] == "forbidden-accept"
    assert st["proof"]["record"]["receipt"]["seq"] == 4
    ok, msg = check_proof(st["proof"], third_party(env))
    assert ok, msg
    proof_file = tmp_path / "p.json"; proof_file.write_text(json.dumps(st["proof"]))
    keyfile = os.path.join(env.tmp, "server-key.json")
    run = lambda *x: subprocess.run([sys.executable, "-m", "han.verify", "proof", str(proof_file), *x], cwd=ROOT, capture_output=True, text=True)
    r = await asyncio.to_thread(run, "--server-key", f"a.localhost={keyfile}")
    assert r.returncode == 0 and r.stdout.startswith("accepted"), r.stdout + r.stderr
    bad = copy.deepcopy(st["proof"]); bad["record"]["receipt"]["time"] = "2030-01-01T00:00:00Z"        # tampered: the receipt signature breaks
    assert not check_proof(bad, third_party(env))[0]
    mislabelled = copy.deepcopy(st["proof"]); mislabelled["kind"] = "bad-move"
    assert not check_proof(mislabelled, third_party(env))[0]
    other = {env.server: {"h1": {"publicKey": keys.generate().pub, "from": "1970-01-01T00:00:00Z"}}}
    assert not check_proof(st["proof"], other)[0]                              # a stranger with other keys is not convinced
    v.close()
    again = verifier(env, a, str(tmp_path / "v.state"))                         # the fault is persisted: a restart stays faulty
    assert again.status(S)["state"] == "faulty"; again.close()

async def test_cli_follow_exit_code_and_proof_file(env, tmp_path):
    """R-14: `python -m han.verify follow` exits 0 on a good space and 2 on a fault, prints a status line and writes the proof."""
    env.actor("alice"); bob = env.actor("bob")
    a, b = await env.client("alice"), await env.client("bob")
    await mkspace(a); await member(a, bob)
    def follow(state):
        return subprocess.run([sys.executable, "-m", "han.verify", "follow", "--url", env.srv.url, "--actor", "@alice:a.localhost", "--key",
                               os.path.join(env.tmp, "alice.key.json"), "--space", S, "--state", str(tmp_path / state), "--once",
                               "--server-key", f"a.localhost={os.path.join(env.tmp, 'server-key.json')}"], cwd=ROOT, capture_output=True, text=True)
    r = await asyncio.to_thread(follow, "ok.state")
    assert r.returncode == 0 and r.stdout.strip() == "ok through 2", r.stdout + r.stderr
    restore = skip_policy(env)
    await b.append(*b.make_entry("policy.set", S, {"policy": default_policy(visibility="public")}))
    restore()
    r = await asyncio.to_thread(follow, "bad.state")
    assert r.returncode == 2 and "faulty forbidden-accept through 2" in r.stdout, r.stdout + r.stderr
    proof = tmp_path / "bad.state.proof.json"
    assert proof.exists() and check_proof(json.loads(proof.read_text()), third_party(env))[0]

async def test_equivocation_found_by_head_gossip(env):
    """R-15, R-02: the sequencer signs two different records at one seq, one for each reader. Alone each mirror is satisfied; when they
    gossip heads in the room both report faulty equivocation and a third party accepts the proof."""
    env.actor("alice"); bob = env.actor("bob")
    a, b = await env.client("alice"), await env.client("bob")
    await mkspace(a); await member(a, bob)
    last = await say(a, "the truth")
    recs = [i["record"] for i in await a.read_all(S, 0) if i["type"] == "record"]
    fake = forge_head(env, "alice", recs[-1], "a lie")
    def lie(items):
        return [({"type": "record", "record": fake} if i["type"] == "record" and i["record"]["receipt"]["seq"] == 3 else
                 {"type": "heads", "heads": [fake["receipt"]]} if i["type"] == "heads" else i) for i in items]
    doctor_reads(env, bob, lie)
    va, vb = verifier(env, a, gossip_interval=0.1), verifier(env, b, gossip_interval=0.1)
    assert (await va.verify(S))["state"] == "ok" and (await vb.verify(S))["state"] == "ok"      # each, alone, sees a consistent chain
    assert va.heads()[0]["id"] != vb.heads()[0]["id"] and va.heads()[0]["seq"] == vb.heads()[0]["seq"] == 3
    ta, tb = asyncio.create_task(va.follow(S)), asyncio.create_task(vb.follow(S))
    await until(lambda: va.status(S)["state"] == "faulty" and vb.status(S)["state"] == "faulty")
    for v in (va, vb):
        st = v.status(S)
        assert st["proof"]["kind"] == "equivocation" and st["through"] == 2
        ok, msg = check_proof(st["proof"], third_party(env))
        assert ok, msg
        assert {r["id"] for r in st["proof"]["receipts"]} == {last["id"], fake["receipt"]["id"]}
    ta.cancel(); tb.cancel()

async def test_observe_equivocation_directly(env):
    """R-15: two mirrors that exchange head receipts out of band (no room) find the equivocation; a receipt that does not verify proves nothing."""
    env.actor("alice"); bob = env.actor("bob")
    a, b = await env.client("alice"), await env.client("bob")
    await mkspace(a); await member(a, bob); await say(a, "truth")
    recs = [i["record"] for i in await a.read_all(S, 0) if i["type"] == "record"]
    fake = forge_head(env, "alice", recs[-1], "lie")
    doctor_reads(env, bob, lambda items: [{"type": "record", "record": fake} if i["type"] == "record" and i["record"]["receipt"]["seq"] == 3 else i
                                          for i in items if i["type"] != "heads"])
    va, vb = verifier(env, a), verifier(env, b)
    await va.verify(S); await vb.verify(S)
    assert va.observe(va.heads()[0]) is None                                  # our own head: nothing to find
    bogus = dict(vb.heads()[0], homeSig=va.heads()[0]["homeSig"])
    assert va.observe(bogus) is None and va.status(S)["state"] == "ok"
    proof = va.observe(vb.heads()[0])
    assert proof and proof["kind"] == "equivocation" and check_proof(proof, third_party(env))[0]
    assert vb.observe(va.heads()[0])["kind"] == "equivocation"

async def test_wrong_home_receipt(env):
    """R-02, R-14: a receipt signed by a server that was not the home at that seq is reported faulty wrong-home, with a checkable proof;
    a bare wrong-home receipt gossiped by a peer is a proof too."""
    env.actor("alice"); a = await env.client("alice")
    await mkspace(a); await say(a, "one")
    other = keys.generate(None, "h1")
    env.seq.home.server, env.seq.home.key = "b.localhost", other              # the doctored home: another server signs from now on
    await say(a, "two")
    keysets = dict(env.seq.server_keys, **{"b.localhost": {"h1": {"publicKey": other.pub, "from": "1970-01-01T00:00:00Z"}}})
    st = await Verifier(a, keysets).verify(S)
    assert st["state"] == "faulty" and st["proof"]["kind"] == "wrong-home" and st["through"] == 2, st
    assert check_proof(st["proof"], keysets)[0]
    recs = [i["record"] for i in await a.read_all(S, 0) if i["type"] == "record"]
    p = Verifier(a, keysets)
    assert await p._ensure_profile("@alice:a.localhost", 1)
    for r in recs[:2]: assert p.c.ingest(r)[0]
    p._log = [(S, r) for r in recs[:2]]
    proof = p.observe(recs[2]["receipt"])
    assert proof["kind"] == "wrong-home" and "record" not in proof and check_proof(proof, keysets)[0]

async def test_future_dated_receipt(env):
    """7.5, R-14: a receipt dated beyond the verifier clock plus the skew is reported faulty future-time; the proof records when it was seen."""
    env.actor("alice"); a = await env.client("alice")
    env.clock.advance(7200)                                                    # the space is created two hours after the profile
    await mkspace(a); await say(a, "x")
    st = await verifier(env, a, now=lambda: T0 + 100).verify(S)                # a verifier whose clock is two hours behind
    assert st["state"] == "faulty" and st["proof"]["kind"] == "future-time" and st["through"] == 0
    assert check_proof(st["proof"], third_party(env))[0]
    assert (await verifier(env, a, now=lambda: T0 + 7200).verify(S))["state"] == "ok"

async def test_doctored_content_is_a_chain_fault(env):
    """R-14, R-09: a record whose content no longer matches its signed contentHash is a `chain` fault whose proof replays offline."""
    env.actor("alice"); a = await env.client("alice")
    await mkspace(a); await say(a, "original")
    def tamper(items):
        out = copy.deepcopy(items)
        for i in out:
            if i["type"] == "record" and i["record"]["receipt"]["seq"] == 2: i["record"]["content"] = {"text": "changed"}
        return out
    doctor_reads(env, "@alice:a.localhost", tamper)
    st = await verifier(env, a).verify(S)
    assert st["state"] == "faulty" and st["proof"]["kind"] == "chain" and "content hash" in st["proof"]["reason"]
    assert check_proof(st["proof"], third_party(env))[0]

async def test_gap_is_a_stop_not_a_fault(env):
    """R-14, P-02: records a reader may not read leave a seq gap; that is `stopped` (no proof exists), never a false fault."""
    env.actor("alice"); bob = env.actor("bob")
    a, b = await env.client("alice"), await env.client("bob")
    await mkspace(a); await say(a, "before bob"); await member(a, bob)
    await member(a, bob, "none"); await say(a, "after bob")
    st = await verifier(env, b).verify(S)
    assert st["state"] == "stopped" and "seq gap" in st["reason"]

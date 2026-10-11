"""End to end over a real WebSocket: spaces, members, messages, reads, follows, restart, and verification with wire.py's mirror."""
import asyncio, pytest
from conftest import S, mkspace, member, say, mirror, chain, Collector
from han.core import P, W, WireError

async def test_scenario_chain_verifies(env):
    """R-02, R-14, §3: create a space, add members, append messages, read them back; receipts chain and a wire.Mirror accepts every record."""
    alice, bob = env.actor("alice"), env.actor("bob")
    a, b = await env.client("alice"), await env.client("bob")
    r1 = await mkspace(a)
    await member(a, bob)
    r3 = await say(b, "hello from bob")
    r4 = await say(a, "hi bob")
    assert (r1["seq"], r3["seq"], r4["seq"]) == (1, 3, 4)
    assert r3["prev"] == W.receipt_hash((await chain(a))[1]["receipt"])
    recs = await chain(b)
    assert [r["receipt"]["seq"] for r in recs] == [1, 2, 3, 4]
    assert recs[2]["content"] == {"text": "hello from bob"} and recs[2]["entry"]["author"] == bob
    m = mirror(env)
    assert all(ok for ok, _ in (m.ingest(r) for r in recs))
    assert all(W.verify_receipt(r["receipt"], env.seq.server_keys) for r in recs)

async def test_read_options(env):
    """§3 read: `after`, `limit`, `state` (first item), `knownHeads` (heads item before records); unknown space is not_found."""
    env.actor("alice"); a = await env.client("alice")
    await mkspace(a)
    for i in range(5): await say(a, f"m{i}")
    items = await a.read_all(S, 2, limit=2, state=True, known_heads=[{"space": S}])
    assert [i["type"] for i in items] == ["state", "heads", "record", "record"]
    assert [i["record"]["receipt"]["seq"] for i in items[2:]] == [3, 4]
    assert "entries" not in items[0]["state"] and items[1]["heads"][0]["seq"] == 6
    with pytest.raises(WireError) as ex: await a.read_all("zz:a.localhost", 0)
    assert ex.value.code == "not_found"

async def test_follow_live_and_backlog(env):
    """§3 read follow, T-01: a follower gets the backlog then new records in order, nothing twice."""
    env.actor("alice"); env.actor("bob")
    a, b = await env.client("alice"), await env.client("bob")
    await mkspace(a); await member(a, "@bob:a.localhost")
    col = Collector(b.read(S, 0, follow=True))
    await col.wait_for(lambda i: i["record"]["receipt"]["seq"] == 2)
    await say(a, "live 1"); await say(a, "live 2")
    await col.wait_for(lambda i: i["record"]["receipt"]["seq"] == 4)
    assert [i["record"]["receipt"]["seq"] for i in col.items] == [1, 2, 3, 4]
    col.stop()

async def test_subscribe_resumes_after_restart(env):
    """T-01, R-07 (client side): subscribe reconnects and resumes after the last record it saw; the chain continues across a server restart."""
    env.actor("alice"); env.actor("bob")
    a, b = await env.client("alice"), await env.client("bob")
    await mkspace(a); await member(a, "@bob:a.localhost"); await say(a, "before")
    col = Collector(b.subscribe(S, after=0))
    await col.wait_for(lambda i: i["record"]["receipt"]["seq"] == 3)
    await env.bounce()                                       # clients stay and reconnect by themselves
    a2 = a
    r = await say(a2, "after restart")
    assert r["seq"] == 4
    await col.wait_for(lambda i: i["record"]["receipt"]["seq"] == 4)
    assert [i["record"]["receipt"]["seq"] for i in col.items] == [1, 2, 3, 4]
    m = mirror(env)
    assert all(ok for ok, _ in (m.ingest(x) for x in await chain(a2)))        # one verifiable chain across the restart
    col.stop()

async def test_state_and_former_member(env):
    """§3 read, P-02: a former member reads old records but gets no current state; the owner gets state."""
    env.actor("alice"); bob = env.actor("bob")
    a, b = await env.client("alice"), await env.client("bob")
    await mkspace(a); await member(a, bob); await say(a, "x")
    await member(a, bob, "none")
    items = await b.read_all(S, 0)
    assert 0 < len(items) < 4
    with pytest.raises(WireError) as ex: await b.read_all(S, 0, state=True)
    assert ex.value.code == "denied"
    assert (await a.read_all(S, 0, state=True))[0]["type"] == "state"

async def test_outsider_gets_not_found(env):
    """§3 read: a reader who may read no seq gets not_found, the same as for a missing space."""
    env.actor("alice"); env.actor("eve")
    a, e = await env.client("alice"), await env.client("eve")
    await mkspace(a)
    with pytest.raises(WireError) as ex: await e.read_all(S, 0)
    assert ex.value.code == "not_found"

async def test_hello_binds_server_and_key(env):
    """§3 hello: the proof binds challenge and dialed server; a client aborts when the challenge names another server; a wrong key is denied."""
    from han.client import Client
    import json, os
    from websockets.asyncio.client import connect
    env.actor("alice"); env.actor("mallory")
    base = dict(url=env.srv.url, actor="@alice:a.localhost", key_path=os.path.join(env.tmp, "alice.key.json"), outbox_db=os.path.join(env.tmp, "x.db"))
    c = Client(server="evil.localhost", **base)
    with pytest.raises(WireError) as ex: await c.connect()
    assert ex.value.code == "denied"
    c2 = Client(**dict(base, key_path=os.path.join(env.tmp, "mallory.key.json"), outbox_db=os.path.join(env.tmp, "y.db")))      # alice's id, mallory's key
    with pytest.raises(WireError) as ex: await c2.connect()
    assert ex.value.code == "denied"
    async with connect(env.srv.url) as ws:                                       # a call before hello
        await ws.recv()
        await ws.send(json.dumps({"id": 5, "call": "read", "args": {"space": S, "after": 0}}))
        assert json.loads(await ws.recv())["error"]["code"] == "denied"

async def test_frames_are_ijson(env):
    """§2 Encoding: duplicate keys and fractional numbers in a frame are invalid."""
    import json
    from websockets.asyncio.client import connect
    async with connect(env.srv.url) as ws:
        await ws.recv()
        await ws.send('{"id":1,"call":"hello","call":"x","args":{}}')
        assert json.loads(await ws.recv())["error"]["code"] == "invalid"
        await ws.send('{"id":1.0,"call":"hello","args":{}}')
        assert json.loads(await ws.recv())["error"]["code"] == "invalid"

async def test_head_gossip_only_for_readable(env):
    """R-15, §3 hello: knownHeads are answered only for spaces the asker named and may read."""
    env.actor("alice"); env.actor("eve")
    a = await env.client("alice")
    await mkspace(a)
    heads = lambda c, sp: c.hello_info["knownHeads"]
    from han.client import Client
    import os
    for who, expect in (("alice", 1), ("eve", 0)):
        c = Client(env.srv.url, f"@{who}:a.localhost", os.path.join(env.tmp, f"{who}.key.json"), os.path.join(env.tmp, f"{who}.g.db"), retry_delay=0.05)
        await c.connect(known_heads=[{"space": S}, {"space": "nope:a.localhost"}])
        assert len(c.hello_info["knownHeads"]) == expect
        await c.close()

"""Scenarios of ref/run_wire.py, run against the real server over WebSocket (the same wire.Home pipeline behind the frames)."""
import asyncio, copy, hashlib, pytest
from conftest import S, mkspace, member, say, mirror, chain, plan_digest, Collector
from han import keys
from han.client import default_policy
from han.core import P, W, WireError

async def base(env):
    """alice owner, bob member, over the network."""
    alice, bob = env.actor("alice"), env.actor("bob")
    a, b = await env.client("alice"), await env.client("bob")
    await mkspace(a); await member(a, bob)
    return alice, bob, a, b

async def test_content_validation(env):
    """R-09, §5: a plan that doesn't match planDigest, over-long text, unknown content fields and wrong content are invalid at the home; mirrors reject tampered content."""
    alice, bob, a, b = await base(env)
    env.actor("exec", "service", operator="@alice:a.localhost")                                          # P-01 pins the executor's profile (F1), so it must be one this server can see
    plan = {"action": "git.push", "target": "repo"}
    body = {"executor": f"@exec:{env.server}", "action": "git.push", "planDigest": plan_digest(plan), "repeatable": False}
    async def code(c, kind, body, content):
        with pytest.raises(WireError) as ex: await c.append(*c.make_entry(kind, S, body, content))
        return ex.value.code
    assert await code(b, "effect.propose", body, {"plan": dict(plan, target="other")}) == "invalid"
    assert await code(b, "message", {}, {"text": "x" * 40000}) == "invalid"
    assert await code(b, "message", {}, {"text": "x", "extra": 1}) == "invalid"
    assert await code(b, "message", {}, None) == "invalid"                 # content missing
    assert await code(b, "member.set", {"actor": alice, "role": "owner"}, {"name": "x"}) == "invalid"     # a kind without content
    r = await b.append(*b.make_entry("effect.propose", S, body, {"plan": plan}))
    assert r["seq"] == 3
    recs = await chain(b)
    bad = copy.deepcopy(recs); bad[-1]["content"] = {"plan": dict(plan, target="other")}
    m = mirror(env); out = [m.ingest(r) for r in bad]
    assert all(ok for ok, _ in out[:-1]), out
    assert not out[-1][0] and "content" in out[-1][1], out

async def test_header_is_exact_on_the_wire(env):
    """R-09: an extra header field or a malformed nonce is invalid at the home, and a mirror rejects a record carrying an extra field."""
    alice, bob, a, b = await base(env)
    e, c = b.make_entry("message", S, {}, {"text": "hi"})
    for bad in (dict(e, id="0" * 64), dict(e, nonce="NOT-BASE32"), {k: v for k, v in e.items() if k != "refs"}):
        with pytest.raises(WireError) as ex: await b._request("append", {"entry": bad, "content": c})
        assert ex.value.code == "invalid"
    await b.append(e, c)
    recs = copy.deepcopy(await chain(b)); recs[-1]["entry"]["id"] = "0" * 64
    m = mirror(env); assert not [m.ingest(r) for r in recs][-1][0]

async def test_equivocation_and_wrong_home_proofs(env):
    """R-02, R-14: a mirror accepts the honest chain; two receipts for one seq signed by the home are an equivocation proof; a receipt from a server that wasn't the home is a wrong-home proof and a mirror refuses a re-signed chain."""
    alice, bob, a, b = await base(env)
    await say(b, "genuine")
    recs = await chain(a)
    m = mirror(env)
    assert all(ok for ok, _ in (m.ingest(r) for r in recs))
    r1 = recs[-1]["receipt"]
    key = env.seq.key
    other = W.sign_receipt(dict(r1, id="1" * 64), key)                    # the home signs a second receipt for the same seq
    assert W.equivocation_proof(r1, other, env.seq.server_keys)
    assert not W.equivocation_proof(r1, r1, env.seq.server_keys)
    evil = keys.generate(None, "h1")
    keys_all = dict(env.seq.server_keys, **{"evil.localhost": {"h1": {"publicKey": evil.pub, "from": "1970-01-01T00:00:00Z"}}})
    forged = W.sign_receipt(dict(r1, home="evil.localhost"), evil)
    assert W.wrong_home_proof(recs[:-1], forged, keys_all) and not W.wrong_home_proof(recs[:-1], r1, keys_all)
    resigned, prev = [], None
    for rec in recs:                                                       # evil re-signs genuine records as the home
        r = W.sign_receipt(dict(rec["receipt"], home="evil.localhost", prev=W.receipt_hash(prev) if prev else ""), evil)
        resigned.append(dict(rec, receipt=r)); prev = r
    ok, why = W.Mirror(keys_all, lambda x, n: env.seq.profile(x, n)).ingest(resigned[0])
    assert not ok and "not the home" in why

async def test_server_cannot_hide_a_forbidden_accept(env):
    """R-11, R-14: if the home stored a record Policy forbids, replaying the network-read chain faults it."""
    alice, bob, a, b = await base(env)
    e, c = b.make_entry("policy.set", S, {"policy": default_policy()})      # bob is a member: Policy denies
    with pytest.raises(WireError) as ex: await b.append(e, c)
    assert ex.value.code == "denied"
    code, r = env.seq.home.append(e, c, {x: env.seq.profile(x) for x in (bob,)}, skip_policy=True)    # a doctored home
    assert code == "ok"
    m = mirror(env); out = [m.ingest(r) for r in await chain(a)]
    assert not out[-1][0] and "forbidden" in out[-1][1]

async def test_first_contact_notices(env):
    """G-03, G-04: giving an actor a role delivers a notice to its own profile follow (and only there); a stranger can't follow the notices or send one; a message `to` a non-member sends none."""
    alice, bob = env.actor("alice"), env.actor("bob")
    carol, eve = env.actor("carol"), env.actor("eve")
    a, b, c, e = [await env.client(n) for n in ("alice", "bob", "carol", "eve")]
    await mkspace(a)
    await say(a, "hey eve", to=[eve])                                       # eve holds no role: no notice
    spy = Collector(e.subscribe("profile:" + bob))                          # eve follows bob's profile
    mine = Collector(b.subscribe("profile:" + bob))
    await asyncio.sleep(0.2)
    await member(a, bob)
    await mine.wait_for(lambda i: i["type"] == "signal")
    note = [i for i in mine.items if i["type"] == "signal"][0]
    assert note["from"] == "@:a.localhost" and note["body"] == {"type": "notice", "space": S, "seq": 3}
    await asyncio.sleep(0.2)
    assert not [i for i in spy.items if i["type"] == "signal"]              # not delivered to other readers of the profile
    with pytest.raises(WireError) as ex: await c.signal("profile:" + bob, {"type": "notice", "space": S, "seq": 1})
    assert ex.value.code == "denied"                                        # only server notices on profile spaces
    late = Collector(b.subscribe("profile:" + bob)); await asyncio.sleep(0.3)
    assert not [i for i in late.items if i["type"] == "signal"]            # F4: a notice is a signal, never replayed to a later subscribe
    assert not env.seq.home.inbox.get(bob)                                 # nothing is kept for an actor without a subscription
    await env.bounce()
    again = Collector(b.subscribe("profile:" + bob)); await asyncio.sleep(0.4)
    assert not [i for i in again.items if i["type"] == "signal"]           # and nothing survives a restart
    for x in (spy, mine, late, again): x.stop()

async def test_blob_access_and_parts(env):
    """G-01: a blob uploaded in parts by the author is served to readers of a referencing record, to nobody else; mentioning someone's hash in your own space gets you nothing; corrupt bytes never complete."""
    alice, bob, a, b = await base(env)
    carol, eve = env.actor("carol"), env.actor("eve")
    c, e = await env.client("carol"), await env.client("eve")
    data = bytes(range(256)) * 4000                                         # ~1 MB: two parts
    ref = await b.blob_put(data, "application/octet-stream")
    assert ref["size"] == len(data)
    await b.append(*b.make_entry("message", S, {}, {"text": "spec", "blobs": [ref]}))
    assert await a.blob_get(ref, S) == data                                 # a reader of the referencing record
    for who in (e,):                                                        # eve reads nothing there
        with pytest.raises(WireError) as ex: await who.blob_get(ref, S)
        assert ex.value.code in ("denied", "not_found")
    T = "t2:a.localhost"                                                    # carol: her own space, mentions the hash
    await mkspace(c, T)
    await c.append(*c.make_entry("message", T, {}, {"text": "gimme", "blobs": [ref]}))
    with pytest.raises(WireError) as ex: await c.blob_get(ref, T)
    assert ex.value.code == "unavailable"                                   # she never uploaded the bytes (supply)
    r = await c._request("blobPut", {"ref": ref, "offset": 0, "data": W.b64(b"")})
    assert r["complete"] is False                                           # claiming a known hash without bytes
    bad = {"hash": hashlib.sha256(b"abc").hexdigest(), "size": 3, "type": "text/plain"}
    with pytest.raises(WireError) as ex: await c._request("blobPut", {"ref": bad, "offset": 0, "data": W.b64(b"xyz")})
    assert ex.value.code == "invalid"                                       # D8: the bytes do not hash to the ref
    await env.bounce()
    assert await a.blob_get(ref, S) == data                                 # blobs and supply persist

async def test_blob_put_empty_and_mismatch(env):
    """G-01: an empty blob completes; bytes that don't hash to the ref never complete."""
    alice, bob, a, b = await base(env)
    ref = await b.blob_put(b"")
    assert ref["size"] == 0
    bad = {"hash": hashlib.sha256(b"abc").hexdigest(), "size": 3, "type": "text/plain"}
    with pytest.raises(WireError) as ex: await b._request("blobPut", {"ref": bad, "offset": 0, "data": W.b64(b"xyz")})
    assert ex.value.code == "invalid"                                       # D8: and the uploader's parts for the ref are discarded
    with pytest.raises(WireError) as ex: await b._request("blobPut", {"ref": bad, "offset": 3, "data": W.b64(b"more")})
    assert ex.value.code == "invalid"

async def test_signals_rights(env):
    """G-04: followers receive signals as ReadItems (not the sender); strangers can neither signal nor follow; signals never touch the log."""
    alice, bob, a, b = await base(env)
    env.actor("eve"); e = await env.client("eve")
    col = Collector(a.subscribe(S)); await asyncio.sleep(0.2)
    await b.signal(S, {"typing": True})
    with pytest.raises(WireError) as ex: await e.signal(S, {"typing": True})
    assert ex.value.code == "denied"
    with pytest.raises(WireError) as ex:
        async for _ in e.subscribe(S): pass
    assert ex.value.code == "denied"
    await col.wait_for(lambda i: i["type"] == "signal")
    sig = col.items[0]
    assert sig == {"type": "signal", "from": bob, "body": {"typing": True}}
    await say(b, "hi"); await col.wait_for(lambda i: i["type"] == "record")
    assert len(await chain(a)) == 3                                         # create, member.set, message: no signal records
    col.stop()

async def test_dedupe_precedes_every_check(env):
    """R-01: after the author's key is revoked, resending an accepted entry returns its receipt; a new entry is denied."""
    alice, bob, a, b = await base(env)
    e, c = b.make_entry("message", S, {}, {"text": "x"})
    r1 = await b.append(e, c)
    rev, rc = make_server_entry(env, "key.revoke", "profile:" + bob, {"keyId": "k1"})
    env.seq.append(rev, rc)                                                 # the server revokes bob's key (account recovery)
    assert env.seq.profile(bob)["keys"]["k1"]["status"] == "revoked"
    assert await b._request("append", {"entry": e, "content": c}) == r1
    with pytest.raises(WireError) as ex: await b.append(*b.make_entry("message", S, {}, {"text": "new"}))
    assert ex.value.code == "denied"
    m = mirror(env)                                                         # the pinned profile seqs keep the old record verifiable
    assert all(ok for ok, _ in (m.ingest(r) for r in await chain(a)))

def make_server_entry(env, kind, space, body):
    from han.entries import make_entry
    return make_entry(env.seq.key, "@:" + env.server, kind, space, body, None, seen=env.seq.home.spaces[space]["state"]["head"])

async def test_registered_agent_needs_confirmed_operator(env):
    """P-03, §4.3: under allowAgents with-operator an agent joins only after its operator confirms in the agent's profile space (all over the wire)."""
    alice = env.actor("alice"); op = env.actor("ana"); bot = env.actor("bot", "agent", operator=op)
    a, ana, bt = await env.client("alice"), await env.client("ana"), await env.client("bot")
    await mkspace(a, policy=default_policy(allowAgents="with-operator"))
    with pytest.raises(WireError) as ex: await member(a, bot)
    assert ex.value.code == "denied"
    await ana.append(*ana.make_entry("member.set", "profile:" + bot, {"actor": op, "role": "owner"}))   # the operator confirms
    await member(a, bot)
    assert (await say(bt, "reporting")) ["seq"] == 3

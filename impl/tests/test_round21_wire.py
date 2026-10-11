"""Round 21 wire decisions (interop/DECISIONS-r21.md, D1-D14): one or more tests per decision, on raw frames where the decision is about
the wire, against a real han-seq (and a federated pair for `host`)."""
import asyncio, base64, hashlib, json, os, urllib.request
import pytest
from websockets.asyncio.client import connect
from websockets.exceptions import ConnectionClosed
from conftest import S, mkspace, member, chain
from fixtures_federation import pair, A, B, SA, until as fed_until      # noqa: F401  (pair is a fixture)
from fixtures_exec import (execs, team, make_exec, file_plan, propose, approve, state, until, phase_is, sandbox)      # noqa: F401
from han import keys
from han.client import Client, default_policy
from han.client.client import _CLOSED
from han.core import P, W, LIMITS, WireError
from han.seq import server as seqserver

HAN_TS_LIMITS = {"maxFrameBytes", "maxHeaderBytes", "maxContentBytes", "maxBlobBytes", "maxBlobPartBytes", "maxBlobRangeBytes",
                 "maxStreamItemBytes", "maxFollows", "maxClockSkewSeconds"}

class Raw:
    """A raw WebSocket to a han-seq: frames as the test writes them."""
    def __init__(self, ws, challenge): self.ws, self.challenge, self.n = ws, challenge, 0

    @classmethod
    async def open(cls, env, name=None, key_id="k1", auth=True):
        ws = await connect(env.srv.url)
        ch = json.loads(await ws.recv())
        raw = cls(ws, ch)
        if name and auth:
            kp = keys.load(os.path.join(env.tmp, f"{name}.key.json"))
            rep = await raw.call("hello", {"who": f"@{name}:{env.server}", "keyId": kp.key_id, "version": 1,
                                           "proof": W.hello_proof(kp, ch["challenge"], env.server)})
            assert "ok" in rep, rep
        return raw

    async def send(self, frame): await self.ws.send(frame if isinstance(frame, str) else json.dumps(frame))
    async def recv(self): return json.loads(await asyncio.wait_for(self.ws.recv(), 5))
    async def call(self, call, args):
        self.n += 1
        await self.send({"id": self.n, "call": call, "args": args})
        return await self.recv()
    async def items(self, call, args):
        """Replies of a streaming call up to its end or error."""
        self.n += 1
        await self.send({"id": self.n, "call": call, "args": args})
        out = []
        while True:
            m = await self.recv(); out.append(m)
            if "end" in m or "error" in m: return out
    async def closed(self):
        try: await asyncio.wait_for(self.ws.recv(), 5)
        except ConnectionClosed: return True
        return False
    async def close(self): await self.ws.close()

def code(rep): return rep.get("error", {}).get("code")

async def two(env):
    """alice (owner) and bob (member) in S, both connected as raw sockets."""
    env.actor("alice"); env.actor("bob")
    a = await env.client("alice")
    await mkspace(a)
    await member(a, f"@bob:{env.server}")
    return a, await Raw.open(env, "alice"), await Raw.open(env, "bob")

# ── D1: spaces this server does not host ──
async def test_d1_foreign_space_is_moved_for_every_call(env):
    env.actor("alice")
    r = await Raw.open(env, "alice")
    foreign = "q1:other.localhost"
    c = await env.client("alice")
    ent, ct = c.make_entry("space.create", foreign, {"policy": default_policy()}, {"name": "x"})
    rep = await r.call("append", {"entry": ent, "content": ct})
    assert code(rep) == "moved" and rep["error"]["movedTo"] == "other.localhost"
    ent, ct = c.make_entry("message", foreign, {}, {"text": "x"})
    rep = await r.call("append", {"entry": ent, "content": ct})
    assert code(rep) == "moved" and rep["error"]["movedTo"] == "other.localhost"
    h = hashlib.sha256(b"x").hexdigest()
    ref = {"hash": h, "size": 1, "type": "text/plain"}
    calls = [("read", {"space": foreign, "after": 0}), ("subscribe", {"space": foreign}), ("watch", {"space": foreign, "session": h}),
             ("blobGet", {"ref": ref, "space": foreign}), ("signal", {"space": foreign, "body": {}}),
             ("stream", {"item": {"space": foreign, "session": h, "n": 1, "time": "2026-01-01T00:00:00Z", "type": "output", "text": "x"}})]
    for call, args in calls:
        rep = (await r.items(call, args))[-1] if call in ("read", "subscribe", "watch") else await r.call(call, args)
        assert code(rep) == "moved" and rep["error"]["movedTo"] == "other.localhost", (call, rep)

async def test_d1_own_unknown_space_is_not_found(env):
    env.actor("alice")
    r = await Raw.open(env, "alice")
    mine = "nope:a.localhost"
    assert code((await r.items("read", {"space": mine, "after": 0}))[-1]) == "not_found"
    assert code((await r.items("subscribe", {"space": mine}))[-1]) == "not_found"
    assert code((await r.items("watch", {"space": mine, "session": "0" * 64}))[-1]) == "not_found"
    assert code(await r.call("signal", {"space": mine, "body": {}})) == "not_found"

async def test_d1_host_refuses_non_servers(env):
    env.actor("alice")
    r = await Raw.open(env, "alice")
    assert code(await r.call("host", {"space": S, "records": []})) == "denied"          # G-06: host needs a server actor @:A

async def test_d1_g06_handover_and_moved_after(pair):
    """G-06, §7.3: b takes records from a, signs its acceptance of the head, is no home until it holds the space.move to itself; then a
    answers `moved` with b and b serves the space."""
    pair.actor("alice", A)
    alice = await pair.client("alice", A)
    await mkspace(alice, SA)
    await pair.a.fed.ensure_keys(B)
    sc = Client(pair.b.srv.url, "@:" + A, "", ":memory:", server=B, key=pair.a.seq.key, retry_delay=0.05, max_retry_delay=0.2)
    pair.clients.append(sc)
    await sc.connect()
    recs = await chain(alice, SA)
    res = await sc._request("host", {"space": SA, "records": recs, "head": recs[-1]["receipt"]})
    assert res["have"] == len(recs) and res["accept"]["keyId"] == "h1"
    ab = await pair.client("alice", A, dial=B)                                       # b is not yet a home: it points back at a
    with pytest.raises(WireError) as ex: await ab.read_all(SA, 0)
    assert ex.value.code == "moved" and ex.value.moved_to == A
    with pytest.raises(WireError) as ex: await sc._request("host", {"space": SA, "records": recs})       # not contiguous from have + 1
    assert ex.value.code == "invalid"
    mv = alice.make_entry("space.move", SA, {"to": B, "accept": res["accept"]})
    receipt = await alice.append(*mv)
    with pytest.raises(WireError) as ex: await alice.read_all(SA, 0)                 # a moved the space away
    assert ex.value.code == "moved" and ex.value.moved_to == B
    more = pair.a.seq.home.spaces[SA]["records"][len(recs):]
    assert [r["receipt"]["seq"] for r in more] == [receipt["seq"]]
    res = await sc._request("host", {"space": SA, "records": more})
    assert res["have"] == receipt["seq"]
    got = await ab.read_all(SA, 0)                                                   # now b serves it
    assert [i["record"]["receipt"]["seq"] for i in got if i["type"] == "record"] == list(range(1, receipt["seq"] + 1))
    msg = await ab.append(*ab.make_entry("message", SA, {}, {"text": "from b"}))
    assert msg["home"] == B and msg["seq"] == receipt["seq"] + 1
    with pytest.raises(WireError) as ex: await sc._request("host", {"space": SA, "records": []})        # b is the home now
    assert ex.value.code == "denied"

# ── D2: request frames ──
async def test_d2_strict_frames(env):
    a, r, bob = await two(env)
    bad = [{"id": 5, "call": "cancel", "args": {"target": 1}, "extra": True},      # extra member
           {"id": 5, "call": "cancel"},                                            # no args
           {"id": 5, "call": "cancel", "args": []},                                # args not an object
           {"id": 5, "call": "nosuch", "args": {}},
           {"id": 5, "call": 7, "args": {}},
           {"id": 5, "call": "cancel", "args": {"target": 1, "bogus": 1}},         # undocumented argument
           {"id": 5, "args": {}, "call": "cancel", "x": None}]
    for f in bad:
        await r.send(f)
        rep = await r.recv()
        assert code(rep) == "invalid" and rep["id"] == 5, (f, rep)
    for f in ({"id": 0, "call": "cancel", "args": {}}, {"id": -3, "call": "cancel", "args": {}}, {"id": True, "call": "cancel", "args": {}},
              {"id": "7", "call": "cancel", "args": {}}, {"call": "cancel", "args": {}}):
        await r.send(f)
        rep = await r.recv()
        assert code(rep) == "invalid" and rep["id"] == 0, (f, rep)             # the id is unusable
    rep = (await r.items("read", {"space": S, "after": 0, "bogus": 1}))[-1]
    assert code(rep) == "invalid"
    assert code(await r.call("append", {"entry": {}, "bogus": 1})) == "invalid"
    assert (await r.call("cancel", {"target": 1})).get("ok", 1) is None         # the connection is still good

async def test_d2_not_ijson_is_answered_with_id_0_and_closed(env):
    a, r, bob = await two(env)
    for text in ('{"id":7,"call":"read","args":{"space":"s1:a.localhost","after":0.5}}',     # a fraction
                 '{"id":7,"call":"cancel","call":"cancel","args":{}}',                         # duplicate member
                 "not json"):
        r = await Raw.open(env, "alice")
        await r.send(text)
        rep = await r.recv()
        assert code(rep) == "invalid" and rep["id"] == 0, (text, rep)
        assert await r.closed()

async def test_d2_client_fails_open_requests_on_id_0_error(env):
    env.actor("alice")
    c = await env.client("alice")
    await mkspace(c)
    rid, q = await c._send("subscribe", {"space": S})                           # an open streaming request
    await asyncio.sleep(0.1)
    await c._ws.send("{{")                                                      # the server answers id 0 (and closes)
    seen = []
    while True:
        m = await asyncio.wait_for(q.get(), 3)
        seen.append(m)
        if m is _CLOSED: break
    assert _CLOSED in seen

# ── D3: an uploader's own unreferenced blobs, 24 hours ──
async def test_d3_uploader_reads_own_unreferenced_blob_for_24h(env):
    a, r, bob_raw = await two(env)
    b = await env.client("bob")
    ref = await a.blob_put(b"probe blob bytes", "text/plain")
    assert await a.blob_get(ref, S) == b"probe blob bytes"                       # nothing references it
    with pytest.raises(WireError) as ex: await b.blob_get(ref, S)                # someone else does not get it
    assert ex.value.code in ("denied", "not_found")
    rep = await r.call("blobGet", {"ref": ref, "space": S, "offset": 16})
    assert rep["ok"]["data"] == "" and rep["ok"]["size"] == 16                   # offset = size: empty
    assert code(await r.call("blobGet", {"ref": ref, "space": S, "offset": 9999})) == "invalid"
    assert code(await r.call("blobGet", {"ref": ref, "space": S, "length": 0})) == "invalid"
    big = await r.call("blobGet", {"ref": ref, "space": S, "offset": 0, "length": 99999999})
    assert big["ok"]["size"] == 16 and len(base64.urlsafe_b64decode(big["ok"]["data"] + "==")) == 16      # clamped
    env.clock.advance(86400 + 1)
    with pytest.raises(WireError) as ex: await a.blob_get(ref, S)                # the 24 hours are over
    assert ex.value.code in ("denied", "not_found")

async def test_d3_own_blob_needs_a_readable_space(env):
    a, r, bob_raw = await two(env)
    env.actor("eve")
    e = await env.client("eve")
    ref = await e.blob_put(b"eve's bytes", "text/plain")
    with pytest.raises(WireError) as ex: await e.blob_get(ref, S)                # eve reads nothing in S
    assert ex.value.code in ("denied", "not_found")

# ── D4: canonical base64url ──
def test_d4_unb64_is_strict():
    assert W.unb64("YWJj") == b"abc" and W.unb64("") == b""
    for bad in ("YWJj=", "YQ==", "YQ=", "Y", "YQ+/", "YWJj\n", "YR", "YWJjZ"):   # padding, other alphabet, length 1 mod 4, non-zero trailing bits
        with pytest.raises(ValueError): W.unb64(bad)
    assert W.unb64("YQ") == b"a" and not W.verify_sig("AA", "AA", b"x")

async def test_d4_blob_data_and_signature_must_be_canonical(env):
    a, r, bob_raw = await two(env)
    ref = {"hash": hashlib.sha256(b"probe blob bytes").hexdigest(), "size": 16, "type": "text/plain"}
    padded = base64.urlsafe_b64encode(b"probe blob bytes").decode()
    assert padded.endswith("=")
    assert code(await r.call("blobPut", {"ref": ref, "offset": 0, "data": padded})) == "invalid"
    ent, ct = a.make_entry("message", S, {}, {"text": "noncanonical sig"})
    last = ent["sig"][-1]
    alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
    assert W.b64(W.unb64(ent["sig"])) == ent["sig"]
    # the 86-character signature has 4 unused bits: the last character carries 2 data bits and 4 zero bits; flipping the lowest one is non-canonical
    flipped = dict(ent, sig=ent["sig"][:-1] + alphabet[alphabet.index(last) ^ 1])
    rep = await r.call("append", {"entry": flipped, "content": ct})
    assert code(rep) == "invalid"
    rep = await r.call("append", {"entry": ent, "content": ct})
    assert "ok" in rep

# ── D7: the /han path ──
async def test_d7_url_has_the_han_path_and_the_client_never_dials_a_bare_host(env):
    assert env.srv.url == f"ws://127.0.0.1:{env.srv.port}/han"
    env.actor("alice")
    bare = f"ws://127.0.0.1:{env.srv.port}"
    c = Client(bare, "@alice:a.localhost", os.path.join(env.tmp, "alice.key.json"), os.path.join(env.tmp, "bare.db"), server=env.server)
    assert c.url == bare + "/han"
    await c.connect(); await c.close()
    ws = await connect(f"ws://127.0.0.1:{env.srv.port}/han")
    assert json.loads(await ws.recv())["server"] == "a.localhost"
    await ws.close()

# ── D8: blobPut failures ──
async def test_d8_blob_put_failures(env):
    a, r, bob_raw = await two(env)
    data = b"0123456789abcdef"
    ref = {"hash": hashlib.sha256(data).hexdigest(), "size": 16, "type": "text/plain"}
    put = lambda off, d: r.call("blobPut", {"ref": ref, "offset": off, "data": W.b64(d)})
    assert code(await put(5, data[5:])) == "invalid"                              # a gap: offset past the bytes held
    assert (await put(0, data[:8]))["ok"] == {"have": 8, "complete": False}      # a valid partial upload
    assert code(await put(10, data[10:])) == "invalid"                            # gap after 8 bytes held
    assert code(await put(4, b"XXXX")) == "invalid"                               # overlaps with different bytes
    assert (await put(4, data[4:12]))["ok"] == {"have": 12, "complete": False}   # overlap with the same bytes extends
    assert (await put(12, data[12:]))["ok"] == {"have": 16, "complete": True}
    assert (await put(0, data[:8]))["ok"] == {"have": 16, "complete": True}      # a repeat after completion
    bad = {"hash": hashlib.sha256(b"abc").hexdigest(), "size": 3, "type": "text/plain"}
    assert code(await r.call("blobPut", {"ref": bad, "offset": 0, "data": W.b64(b"xyz")})) == "invalid"       # hash mismatch on completion
    assert (await r.call("blobPut", {"ref": bad, "offset": 0, "data": W.b64(b"ab")}))["ok"] == {"have": 2, "complete": False}      # discarded: starts over
    assert (await r.call("blobPut", {"ref": bad, "offset": 2, "data": W.b64(b"c")}))["ok"] == {"have": 3, "complete": True}

# ── D9: hello limits ──
async def test_d9_hello_carries_every_limit(env):
    env.actor("alice")
    c = await env.client("alice")
    assert set(c.hello_info["limits"]) == HAN_TS_LIMITS
    assert c.hello_info["limits"]["maxClockSkewSeconds"] == W.MAX_CLOCK_SKEW == LIMITS["maxClockSkewSeconds"]
    assert all(isinstance(v, int) for v in c.hello_info["limits"].values())

# ── D11: hello errors ──
async def test_d11_hello_error_codes(env):
    env.actor("alice")
    kp = keys.load(os.path.join(env.tmp, "alice.key.json"))
    async def hello(who, key_id="k1", sign=kp, challenge=None):
        r = await Raw.open(env)
        proof = W.hello_proof(sign, challenge or r.challenge["challenge"], env.server)
        return await r.call("hello", {"who": who, "keyId": key_id, "proof": proof, "version": 1})
    assert code(await hello("@nobody:a.localhost")) == "not_found"              # unknown actor
    assert code(await hello("@alice:a.localhost", key_id="k9")) == "denied"      # unknown key
    assert code(await hello("@alice:a.localhost", challenge="another")) == "denied"      # proof over another challenge
    assert code(await hello("@alice:a.localhost", sign=W.Key("someone else"))) == "denied"
    assert "ok" in await hello("@alice:a.localhost")

async def test_d11_revoked_key_is_denied(env):
    env.actor("alice")
    c = await env.client("alice")
    kp2 = keys.generate(None, "k2")
    prof = "profile:@alice:a.localhost"
    add = c.make_entry("key.add", prof, {"keyId": "k2", "publicKey": kp2.pub})
    await c.append(*add)
    await c.append(*c.make_entry("key.revoke", prof, {"keyId": "k1"}))
    r = await Raw.open(env)
    kp = keys.load(os.path.join(env.tmp, "alice.key.json"))
    rep = await r.call("hello", {"who": "@alice:a.localhost", "keyId": "k1", "proof": W.hello_proof(kp, r.challenge["challenge"], env.server), "version": 1})
    assert code(rep) == "denied"

# ── D12: the challenge ──
async def test_d12_challenge_is_32_random_bytes_single_use_and_expires(env):
    env.actor("alice")
    seen = set()
    for _ in range(3):
        r = await Raw.open(env)
        ch = r.challenge
        assert set(ch) == {"challenge", "server", "versions", "time"} and ch["versions"] == [1]
        assert len(ch["challenge"]) == 43 and len(W.unb64(ch["challenge"])) == 32
        seen.add(ch["challenge"]); await r.close()
    assert len(seen) == 3
    kp = keys.load(os.path.join(env.tmp, "alice.key.json"))
    hello = lambda r: {"who": "@alice:a.localhost", "keyId": "k1", "proof": W.hello_proof(kp, r.challenge["challenge"], env.server), "version": 1}
    r = await Raw.open(env)                                                       # single use: a failed attempt spends it
    assert code(await r.call("hello", dict(hello(r), keyId="k9"))) == "denied"
    assert code(await r.call("hello", hello(r))) == "denied"
    r = await Raw.open(env)                                                       # expires after 60 s
    env.clock.advance(61)
    assert code(await r.call("hello", hello(r))) == "denied"
    r = await Raw.open(env)
    env.clock.advance(59)
    assert "ok" in await r.call("hello", hello(r))

# ── D13: .well-known ──
def _get(url):
    with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(url, timeout=5) as resp:
        return resp.status, resp.headers.get_content_type(), json.loads(resp.read()), resp.headers.get_content_charset()

async def test_d13_well_known_documents(env):
    base = f"http://127.0.0.1:{env.srv.port}"
    st, ctype, doc, cs = await asyncio.to_thread(_get, base + "/.well-known/han")
    assert st == 200 and ctype == "application/json" and cs == "utf-8"
    assert set(doc) == {"server", "sequencer", "keys"} and doc["server"] == "a.localhost"
    assert doc["sequencer"] == f"ws://127.0.0.1:{env.srv.port}/han"
    assert [set(k) for k in doc["keys"]] == [{"keyId", "publicKey", "from"}]
    st, ctype, peers, _ = await asyncio.to_thread(_get, base + "/.well-known/han-peers")
    assert st == 200 and ctype == "application/json" and peers == {"peers": []}

async def test_d13_peers_lists_others_with_server_and_url(pair):
    await pair.a.fed.ensure_keys(B)
    _, _, peers, _ = await asyncio.to_thread(_get, pair.peers[A] + "/.well-known/han-peers")
    assert peers == {"peers": [{"server": B, "url": pair.peers[B] + "/.well-known/han"}]}

# ── D14: settled on every claim ──
def test_d14_wire_state_adds_settled_false_without_touching_the_state():
    from han.seq.sequencer import wire_state
    st = {"head": 1, "effects": {"p": {"phase": "claimed", "claims": [{"id": "c1"}, {"id": "c2", "settled": True}]}}}
    out = wire_state(st)
    assert [c["settled"] for c in out["effects"]["p"]["claims"]] == [False, True]
    assert st["effects"]["p"]["claims"][0] == {"id": "c1"}

async def test_d14_claim_is_settled_after_its_outcome(env, execs):
    a = await team(env)
    plan = file_plan()
    pid, _ = await propose(a, plan, f"@exec:{env.server}")
    await approve(a, pid, plan)
    ex = make_exec(env, execs); await ex.start()
    await until(phase_is(a, pid, "succeeded"), what="succeeded")
    claims = (await state(a))["effects"][pid]["claims"]
    assert len(claims) == 1 and claims[0]["outcome"] == "succeeded" and claims[0]["settled"] is True and "reserved" not in claims[0]

# ── D5: an executor started before its space exists keeps asking ──
async def test_d5_executor_retries_a_missing_space(env, execs):
    env.actor("alice"); env.actor("exec", "service")
    ex = make_exec(env, execs); await ex.start()                                 # S does not exist yet: not_found at once
    await asyncio.sleep(0.5)
    a = await team(env)
    plan = file_plan("out/early.txt", "early")
    pid, _ = await propose(a, plan, f"@exec:{env.server}")
    await approve(a, pid, plan)
    await until(phase_is(a, pid, "succeeded"), what="succeeded")
    with open(os.path.join(sandbox(env), "out", "early.txt"), encoding="utf-8") as fh: assert fh.read() == "early"

# ── G-07 / G-08 details the vectors rely on ──
async def test_g07_order_is_frame_then_authentication_then_args(env):
    env.actor("alice")
    r = await Raw.open(env)                                                       # not authenticated yet
    assert code(await r.call("read", {"space": S, "after": 0, "bogus": 1})) == "denied"      # authentication before args
    assert code(await r.call("nosuch", {})) == "invalid"                           # the frame comes before authentication
    assert code(await r.call("hello", {"who": "@alice:a.localhost", "extra": 1})) == "invalid"
    for text in ("[1]", "7", '"x"'):                                               # not an object: id 0 and closed
        r = await Raw.open(env)
        await r.send(text)
        rep = await r.recv()
        assert code(rep) == "invalid" and rep["id"] == 0 and await r.closed()

async def test_g07_open_request_id_and_max_follows(env):
    a, r, bob = await two(env)
    r.n = 0
    await r.send({"id": 1, "call": "subscribe", "args": {"space": S}})
    await r.send({"id": 1, "call": "subscribe", "args": {"space": S}})
    rep = await r.recv()
    assert rep["id"] == 1 and code(rep) == "invalid"                               # the open stream is unaffected
    for i in range(2, LIMITS["maxFollows"] + 1): await r.send({"id": i, "call": "subscribe", "args": {"space": S}})
    await r.send({"id": LIMITS["maxFollows"] + 1, "call": "subscribe", "args": {"space": S}})
    rep = await r.recv()
    assert rep["id"] == LIMITS["maxFollows"] + 1 and code(rep) == "limit"

async def test_g01_blob_without_access_is_not_found_and_parts_repeat_size_and_type(env):
    a, r, bob = await two(env)
    ref = await a.blob_put(b"abcdef", "text/plain")
    await a.append(*a.make_entry("message", S, {}, {"text": "x", "blobs": [ref]}))
    env.actor("eve"); e = await Raw.open(env, "eve")
    assert code(await e.call("blobGet", {"ref": ref, "space": S})) == "not_found"
    part = {"hash": hashlib.sha256(b"0123456789").hexdigest(), "size": 10, "type": "text/plain"}
    assert (await r.call("blobPut", {"ref": part, "offset": 0, "data": W.b64(b"01234")}))["ok"]["have"] == 5
    assert code(await r.call("blobPut", {"ref": dict(part, type="image/png"), "offset": 5, "data": W.b64(b"56789")})) == "invalid"
    assert (await r.call("blobPut", {"ref": part, "offset": 5, "data": W.b64(b"56789")}))["ok"] == {"have": 10, "complete": True}

# ── D10, G-09 ──
async def test_d10_signal_from_is_for_server_connections_only(env):
    a, r, bob = await two(env)
    assert code(await r.call("signal", {"space": S, "body": {"a": 1}, "from": "@bob:a.localhost"})) == "invalid"
    assert (await r.call("signal", {"space": S, "body": {"a": 1}}))["ok"] is None

async def test_g09_signals_ride_subscribe_never_read_follow(env):
    a, r, bob = await two(env)
    env.actor("carol")
    await member(a, f"@carol:{env.server}")
    carol = await Raw.open(env, "carol")
    await r.send({"id": 50, "call": "subscribe", "args": {"space": S}})
    await carol.send({"id": 50, "call": "read", "args": {"space": S, "after": 999999, "follow": True}})
    await asyncio.sleep(0.3)
    await (await env.client("bob")).signal(S, {"typing": True})
    assert (await asyncio.wait_for(r.recv(), 3))["item"]["type"] == "signal"
    with pytest.raises(asyncio.TimeoutError): await asyncio.wait_for(carol.recv(), 0.5)

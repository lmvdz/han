"""Federation between two sequencers (architecture.md §4.3, G-02, G-03, G-04, P-01, R-10): a.localhost and b.localhost as two servers on two ports."""
import asyncio, json, pytest, urllib.request
from conftest import mkspace, member, say, Collector
from fixtures_federation import A, B, SA, pair, until
from han.core import W, WireError
from han.entries import default_policy

def get(url):
    with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(url, timeout=5) as r: return r.status, json.loads(r.read())

def is_notice(space, seq=None): return lambda i: i["type"] == "signal" and i["body"].get("type") == "notice" and i["body"]["space"] == space and seq in (None, i["body"]["seq"])

async def test_well_known_served_on_the_websocket_port(pair):
    """§4.3: GET /.well-known/han on the sequencer's own port gives server name, sequencer endpoint and keys with windows; /han-peers lists the peers it verifies with."""
    st, doc = await asyncio.to_thread(get, pair.peers[A] + "/.well-known/han")
    assert st == 200 and doc["server"] == A and doc["sequencer"] == pair.a.srv.url
    assert doc["keys"] == [{"keyId": "h1", "publicKey": pair.a.seq.key.pub, "from": "1970-01-01T00:00:00Z"}]
    await pair.a.fed.ensure_keys(B)                                   # a has now used b's keys for verification
    _, peers = await asyncio.to_thread(get, pair.peers[A] + "/.well-known/han-peers")
    assert peers == {"peers": [{"server": B, "url": pair.peers[B] + "/.well-known/han"}]}           # D13: the others, not itself
    pair.actor("alice", A)
    c = await pair.client("alice", A)                                  # the same port still speaks HAN
    assert (await mkspace(c))["seq"] == 1

async def test_human_on_b_joins_space_on_a_and_gets_a_notice(pair):
    """§4.3 remote profiles, G-03, R-10: bob@b is added to a space homed on a, learns of it by a notice (only on his own follow of his profile),
    appends and reads at a, and the receipts pin his profile seq as fetched from b."""
    alice, bob, carol = pair.actor("alice", A), pair.actor("bob", B), pair.actor("carol", B)
    a = await pair.client("alice", A)
    bob_home, carol_home = await pair.client("bob", B), await pair.client("carol", B)
    nb = Collector(bob_home.subscribe("profile:" + bob)); nc = Collector(carol_home.subscribe("profile:" + carol))
    await asyncio.sleep(0.1)
    await mkspace(a)
    r = await member(a, bob)
    assert r["profiles"] == {alice: 1, bob: 1}                         # bob's profile seq 1 was read from b and pinned
    await nb.wait_for(is_notice(SA, r["seq"]), 5)                      # b delivered it to bob's session...
    await asyncio.sleep(0.3)
    assert not any(is_notice(SA)(i) for i in nc.items)                 # ...and to nobody else
    b_at_a = await pair.client("bob", B, dial=A)                       # bob dials the home: hello with a remote profile
    rb = await say(b_at_a, "hello from b")
    assert rb["seq"] == 3 and rb["profiles"] == {bob: 1}
    items = await b_at_a.read_all(SA, 0)
    assert [i["record"]["entry"]["author"] for i in items if i["type"] == "record"] == [alice, alice, bob]
    m = W.Mirror(pair.a.seq.server_keys, lambda x, n: pair.a.seq.profile(x, n))        # the chain verifies with b's keys for bob's profile
    assert all(ok for ok, _ in (m.ingest(i["record"]) for i in items if i["type"] == "record"))
    nb.stop(); nc.stop()

async def test_unknown_remote_actor_is_unavailable(pair):
    """§4.1 step 4: a member.set naming an actor whose profile cannot be had (no such actor on b) is `unavailable`, not a guess."""
    pair.actor("alice", A)
    a = await pair.client("alice", A)
    await mkspace(a)
    e, c = a.make_entry("member.set", SA, {"actor": "@ghost:b.localhost", "role": "member"})
    with pytest.raises(WireError) as ex: await a._request("append", {"entry": e, "content": c})
    assert ex.value.code == "unavailable"

async def test_agent_on_b_with_operator_on_a(pair):
    """P-03, §4.3: under allowAgents with-operator, an agent registered on b joins a's space only after its operator on a confirms in the agent's
    profile space on b; withdrawing the confirmation stops its writes once a re-reads the profile."""
    alice, bot = pair.actor("alice", A), pair.actor("bot", B, "agent", operator="@alice:a.localhost")
    a, bt = await pair.client("alice", A), await pair.client("bot", B, dial=A)
    await mkspace(a, policy=default_policy(allowAgents="with-operator"))
    with pytest.raises(WireError) as ex: await member(a, bot)
    assert ex.value.code == "denied"                                    # agent needs a confirmed operator
    a_at_b = await pair.client("alice", A, dial=B)                      # alice, a remote actor on b, confirms in profile:@bot:b
    await a_at_b.append(*a_at_b.make_entry("member.set", "profile:" + bot, {"actor": alice, "role": "owner"}))
    pair.clock.advance(301)                                             # a's copy of bot's profile is stale; the next use re-reads it
    await member(a, bot)
    r = await bt.append(*bt.make_entry("session.open", SA, {"steerMode": "auto"}))      # accountable: the confirmation is in the pinned profile
    assert r["seq"] == 3 and r["profiles"][bot] == 2
    await a_at_b.append(*a_at_b.make_entry("member.set", "profile:" + bot, {"actor": alice, "role": "none"}))     # alice withdraws
    pair.clock.advance(301)                                             # past the freshness window (R-10)
    with pytest.raises(WireError) as ex: await bt.append(*bt.make_entry("session.open", SA, {"steerMode": "auto"}))
    assert ex.value.code == "denied" and "operator" in ex.value.message
    assert pair.a.seq.profile(bot)["operatorConfirmed"] is False

async def test_blob_uploaded_on_b_is_used_in_a_space_on_a(pair):
    """G-02, G-01: bob uploads to b, references the blob in a space on a; a fetches it from the author's server, checks the hash, holds it for
    that space, and a reader on a gets it. A server asking b for an unreferenced blob gets nothing."""
    pair.actor("alice", A); bob = pair.actor("bob", B)
    a, bob_b, bob_a = await pair.client("alice", A), await pair.client("bob", B), await pair.client("bob", B, dial=A)
    await mkspace(a); await member(a, bob)
    data = b"bytes that only b has " * 100
    ref = await bob_b.blob_put(data, "text/plain")
    assert ref["hash"] not in pair.a.seq.home.blobs
    await bob_a.append(*bob_a.make_entry("message", SA, {}, {"text": "see attachment", "blobs": [ref]}))
    assert await a.blob_get(ref, SA) == data                             # a's blobGet waits for the fetch that is under way
    assert (SA, ref["hash"]) in pair.a.seq.home.supplied and not pair.a.seq.home.uploads.get(ref["hash"])     # supplied by fetch, not by an upload
    other = {"hash": "0" * 64, "size": 3, "type": "text/plain"}
    srv = await pair.a.fed.conn(B)
    with pytest.raises(WireError): await srv.blob_get(other, SA)          # b holds nothing for an unreferenced hash

async def test_blob_hash_mismatch_is_not_supplied(pair, monkeypatch):
    """G-02: the receiver verifies the hash; bytes that do not match are dropped and the blob is never supplied."""
    pair.actor("alice", A); bob = pair.actor("bob", B)
    a, bob_b, bob_a = await pair.client("alice", A), await pair.client("bob", B), await pair.client("bob", B, dial=A)
    await mkspace(a); await member(a, bob)
    ref = await bob_b.blob_put(b"genuine bytes", "text/plain")
    real = pair.b.seq.blob_range
    monkeypatch.setattr(type(pair.b.seq), "blob_range", staticmethod(lambda d, o, l: real(b"forged bytes!", o, l)))      # b serves other bytes
    await bob_a.append(*bob_a.make_entry("message", SA, {}, {"text": "x", "blobs": [ref]}))
    await until(lambda: not pair.a.fed.pending, 15)
    assert (SA, ref["hash"]) not in pair.a.seq.home.supplied
    with pytest.raises(WireError) as ex: await a.blob_get(ref, SA)
    assert ex.value.code == "unavailable"

async def test_signal_from_b_user_reaches_followers_on_a(pair):
    """G-04: bob (on b, connected only to b) signals in a space homed on a; b relays it over its @:b connection; a delivers it to its followers
    with `from` bob. A relay for an actor of another server is refused, and so is a signal by someone who cannot read the space."""
    alice, bob = pair.actor("alice", A), pair.actor("bob", B); pair.actor("eve", B)
    a, bob_b, eve_b = await pair.client("alice", A), await pair.client("bob", B), await pair.client("eve", B)
    await mkspace(a); await member(a, bob)
    got = Collector(a.subscribe(SA)); await asyncio.sleep(0.1)
    await bob_b.signal(SA, {"typing": True})
    await got.wait_for(lambda i: i["type"] == "signal" and i["from"] == bob and i["body"] == {"typing": True}, 5)
    with pytest.raises(WireError) as ex: await eve_b.signal(SA, {"typing": True})          # eve cannot read SA: the home refuses
    assert ex.value.code == "denied"
    srv = await pair.b.fed.conn(A)
    with pytest.raises(WireError) as ex: await srv.signal(SA, {"x": 1}, relay_from=alice)    # b may relay only for its own actors
    assert ex.value.code == "denied"
    got.stop()

async def test_notice_that_cannot_be_verified_is_dropped(pair):
    """G-03: a notice is a hint; b reads the space as @:b and drops one for an actor that holds no role there."""
    pair.actor("alice", A); bob = pair.actor("bob", B)
    a, bob_b = await pair.client("alice", A), await pair.client("bob", B)
    await mkspace(a)
    n = Collector(bob_b.subscribe("profile:" + bob)); await asyncio.sleep(0.1)
    srv = await pair.a.fed.conn(B)
    with pytest.raises(WireError) as ex: await srv.signal("profile:" + bob, {"type": "notice", "space": SA, "seq": 1})
    assert ex.value.code == "denied"
    await asyncio.sleep(0.2)
    assert not any(i["type"] == "signal" for i in n.items)
    n.stop()

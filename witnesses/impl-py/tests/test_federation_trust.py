"""Federation trust (architecture.md §4.3, R-10, R-12, §7.5): revocations and server keys across two sequencers, the verifier over a remote
profile, the han-peers config, and two real han-seq processes."""
import asyncio, os, socket, subprocess, sys, pytest
from conftest import mkspace, member, say, Collector
from fixtures_federation import A, B, SA, pair, until
from han import keys
from han.client import Client
from han.core import P, W, WireError
from han.seq.federation import Federation, load_peers
from han.verify import Verifier

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

def count_pulls(fed):
    n = {"pulls": 0}
    orig = fed._pull
    async def pull(*a, **kw): n["pulls"] += 1; return await orig(*a, **kw)
    fed._pull = pull
    return n

async def test_profile_is_cached_for_300_s_then_read_again(pair):
    """R-10, P-01: a fetched profile serves appends for 300 s without another read; after that it is read again before the next one is ordered."""
    pair.actor("alice", A); bob = pair.actor("bob", B)
    n = count_pulls(pair.a.fed)
    a, bt = await pair.client("alice", A), await pair.client("bob", B, dial=A)           # bob's hello reads his profile
    await mkspace(a); await member(a, bob)
    for i in range(3): await say(bt, f"m{i}")
    assert n["pulls"] == 1
    pair.clock.advance(299); await say(bt, "still fresh")
    assert n["pulls"] == 1
    pair.clock.advance(2); await say(bt, "stale: read again")
    assert n["pulls"] == 2

async def test_revoked_key_on_b_is_honoured_by_a_and_the_receipt_pins_the_new_seq(pair):
    """R-10, R-12, P-01: bob revokes k1 on b (k2 added first, k1 revoked by k2). Within the freshness window a may still hold the old view; once it
    is 300 s old, a re-reads, denies k1, accepts k2, and the receipt pins the newer profile seq. Old records stay verifiable with the seq they pinned."""
    pair.actor("alice", A); bob = pair.actor("bob", B)
    a, bob1 = await pair.client("alice", A), await pair.client("bob", B, dial=A)
    await mkspace(a); await member(a, bob)
    r1 = await say(bob1, "signed by k1")
    assert r1["profiles"][bob] == 1
    k2 = keys.generate(os.path.join(pair.b.tmp, "bob2.key.json"), "k2")
    bob_b1 = await pair.client("bob", B)
    await bob_b1.append(*bob_b1.make_entry("key.add", "profile:" + bob, {"keyId": "k2", "publicKey": k2.pub}))
    bob_b2 = Client(pair.b.srv.url, bob, os.path.join(pair.b.tmp, "bob2.key.json"), os.path.join(pair.b.tmp, "bob2.outbox.db"), server=B, retry_delay=0.05)
    pair.clients.append(bob_b2); await bob_b2.connect()
    await bob_b2.append(*bob_b2.make_entry("key.revoke", "profile:" + bob, {"keyId": "k1"}))     # profile seq 3
    pair.clock.advance(301)                                                                       # a's view of bob is now too old to use
    with pytest.raises(WireError) as ex: await say(bob1, "k1 again")
    assert ex.value.code == "denied"                                                              # R-12: a revoked key is not valid
    bob_a2 = Client(pair.a.srv.url, bob, os.path.join(pair.b.tmp, "bob2.key.json"), os.path.join(pair.b.tmp, "bob2.a.outbox.db"), server=A, retry_delay=0.05)
    pair.clients.append(bob_a2); await bob_a2.connect()
    r3 = await say(bob_a2, "signed by k2")
    assert r3["profiles"][bob] == 3 > r1["profiles"][bob]
    recs = [i["record"] for i in await a.read_all(SA, 0) if i["type"] == "record"]
    m = W.Mirror(pair.a.seq.server_keys, lambda x, n: pair.a.seq.profile(x, n))
    assert all(ok for ok, _ in (m.ingest(r) for r in recs))                                       # the old record still verifies against its pinned seq 1

async def test_compromised_server_key_stops_trust(pair):
    """§4.3, §7.5: b rotates away from h1 and marks it `compromised`. a keeps what it recorded before it saw the mark (bob's profile), takes
    nothing new signed by h1 (carol, registered under h1 but never read by a), refuses h1 as @:b, and accepts the new key h2. The decision survives a restart."""
    pair.actor("alice", A); bob = pair.actor("bob", B)
    a, bt = await pair.client("alice", A), await pair.client("bob", B, dial=A)
    await mkspace(a); await member(a, bob); await say(bt, "before")
    pair.clock.advance(10)
    pair.actor("carol", B)
    pair.clock.advance(10)
    old = pair.b.seq.key
    pair.b.seq.rotate_key(keys.generate(os.path.join(pair.b.tmp, "server-key-2.json"), "h2")); pair.b.seq.mark_compromised("h1")
    pair.clock.advance(301)
    pair.actor("dave", B)                                                                   # registered under h2
    with pytest.raises(WireError) as ex: await pair.client("carol", B, dial=A)                     # h1-signed profile, not recorded before the mark
    assert ex.value.code == "unavailable" and "key window" in ex.value.message
    await pair.client("dave", B, dial=A)                                                           # h2 is fine
    h1 = pair.a.seq.server_keys[B]["h1"]
    assert h1["compromised"] and h1["until"] > "1970" and pair.a.seq.profile(bob, 1)             # trust ends just after bob's profile receipt
    assert (await say(bt, "after"))["seq"] == 4                                                    # bob's cached profile still serves (re-read, no new records)
    imposter = Client(pair.a.srv.url, "@:" + B, "", ":memory:", server=A, key=old, retry_delay=0.05)
    with pytest.raises(WireError) as ex: await imposter.connect()
    assert ex.value.code == "denied"                                                               # the compromised key cannot act as @:b
    ok = Client(pair.a.srv.url, "@:" + B, "", ":memory:", server=A, key=pair.b.seq.key, retry_delay=0.05)
    await ok.connect(); await ok.close(); await imposter.close()
    again = Federation(pair.a.seq, pair.peers)                                                     # a restart: stored keys and records reload and verify
    assert again.profile(bob, 1) and pair.a.seq.server_keys[B]["h1"] == h1
    await again.close()

async def test_peer_key_changed_for_a_seen_window_is_a_fault(pair):
    """§4.3: a key that changes for an already-seen window is a fault: a stops trusting that server's documents."""
    pair.actor("alice", A); bob = pair.actor("bob", B)
    a, bt = await pair.client("alice", A), await pair.client("bob", B, dial=A)
    await mkspace(a); await member(a, bob)
    other = keys.generate().pub
    pair.b.seq.server_keys[B]["h1"]["publicKey"] = other                                             # b now claims another key for the same window
    with pytest.raises(WireError) as ex: await pair.a.fed.ensure_keys(B, force=True)
    assert ex.value.code == "unavailable" and B in pair.a.fed.bad
    pair.clock.advance(301)
    e, c = bt.make_entry("message", SA, {}, {"text": "x"})
    with pytest.raises(WireError) as ex: await bt._request("append", {"entry": e, "content": c})
    assert ex.value.code == "unavailable"

async def test_verifier_on_b_follows_a_space_on_a_and_reports_ok(pair, tmp_path):
    """R-14, §4.3: a verifier run by bob@b, connected to the home on a, follows a space whose receipts pin profiles of both servers; it fetches
    bob's profile chain (verified by a) and checks it with b's keys."""
    pair.actor("alice", A); bob = pair.actor("bob", B)
    a, bt = await pair.client("alice", A), await pair.client("bob", B, dial=A)
    await mkspace(a); await member(a, bob); await say(bt, "from b")
    v = Verifier(bt, {s: dict(k) for s, k in pair.a.seq.server_keys.items()}, str(tmp_path / "v.state"))
    assert set(v.keys) == {A, B}
    seen = []
    task = asyncio.create_task(v.follow(SA, seen.append))
    await until(lambda: v.status(SA) == {"state": "ok", "through": 3})
    await say(a, "live"); await say(bt, "live from b")
    await until(lambda: v.status(SA)["through"] == 5)
    assert v.status(SA)["state"] == "ok" and v.c.head("profile:" + bob) == 1 and v.c.head("profile:@alice:a.localhost") == 1
    task.cancel()
    with pytest.raises(asyncio.CancelledError): await task
    v.close()

async def test_a_remote_profile_is_readable_from_the_server_that_verified_it(pair):
    """§4.3: profile spaces are world-readable and verifiers serve the ones they pinned; a serves its verified copy of bob's profile."""
    pair.actor("alice", A); bob = pair.actor("bob", B)
    a, bt = await pair.client("alice", A), await pair.client("bob", B, dial=A)
    await mkspace(a); await member(a, bob)
    items = await a.read_all("profile:" + bob, 0)
    st = await a.read_state("profile:" + bob)
    assert st["profile"]["keys"]["k1"]["status"] == "valid"
    assert [i["record"]["receipt"]["seq"] for i in items] == [1] and items[0]["record"]["receipt"]["home"] == B

def test_han_peers_file(tmp_path):
    """han-peers: a JSON map of server name → URL; anything else is rejected."""
    p = tmp_path / "han-peers.json"
    p.write_text('{"b.localhost": "http://127.0.0.1:8702"}', encoding="utf-8")
    assert load_peers(str(p)) == {"b.localhost": "http://127.0.0.1:8702"}
    p.write_text('["b.localhost"]', encoding="utf-8")
    with pytest.raises(ValueError): load_peers(str(p))

# ── two real han-seq processes ──
def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0)); return s.getsockname()[1]

async def wait_listening(port, proc):
    for _ in range(100):
        if proc.poll() is not None: raise RuntimeError("han-seq exited")
        try:
            r, w = await asyncio.open_connection("127.0.0.1", port); w.close(); return
        except OSError: await asyncio.sleep(0.1)
    raise TimeoutError

async def test_two_han_seq_processes(tmp_path):
    """§4.3, G-03: two `python -m han.seq` processes with a han-peers file: alice@a creates a space, adds bob@b; bob learns of it from b, then
    appends at a. Nothing is shared but the two HTTP/WebSocket ports."""
    pa, pb = free_port(), free_port()
    peers = tmp_path / "han-peers.json"
    peers.write_text(f'{{"a.localhost": "http://127.0.0.1:{pa}", "b.localhost": "http://127.0.0.1:{pb}"}}', encoding="utf-8")
    for n in ("alice", "bob"): keys.generate(str(tmp_path / f"{n}.json"))
    def run(server, port, actor, key):
        return subprocess.Popen([sys.executable, "-m", "han.seq", "--server", server, "--port", str(port), "--db", str(tmp_path / f"{server}.db"),
                                 "--key", str(tmp_path / f"{server}.key"), "--peers", str(peers), "--register", f"{actor}={key}"],
                                cwd=ROOT, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    procs = [run(A, pa, "@alice:a.localhost", tmp_path / "alice.json"), run(B, pb, "@bob:b.localhost", tmp_path / "bob.json")]
    clients = []
    try:
        for port, p in zip((pa, pb), procs): await wait_listening(port, p)
        def cl(name, server, port):
            c = Client(f"ws://127.0.0.1:{port}", f"@{name}:{'a' if name == 'alice' else 'b'}.localhost", str(tmp_path / f"{name}.json"),
                       str(tmp_path / f"{name}.{server}.ob.db"), server=server, retry_delay=0.1); clients.append(c); return c
        alice, bob_home, bob_at_a = cl("alice", A, pa), cl("bob", B, pb), cl("bob", A, pa)
        for c in (alice, bob_home, bob_at_a): await c.connect()
        got = Collector(bob_home.subscribe("profile:@bob:b.localhost")); await asyncio.sleep(0.2)
        await mkspace(alice); r = await member(alice, "@bob:b.localhost")
        await got.wait_for(lambda i: i["type"] == "signal" and i["body"]["space"] == SA and i["body"]["seq"] == r["seq"], 10)
        assert (await say(bob_at_a, "from the other server"))["seq"] == 3
        got.stop()
    finally:
        for c in clients: await c.close()
        for p in procs: p.kill(); p.wait()

"""Fixes after the independent review: context blobs (CE1), state at a past seq (CE3), follow with a limit, verifier skew and gossip."""
import asyncio, pytest
from conftest import S, mkspace, member, say, Collector, T0
from fixtures_verify import verifier
from test_streams import setup, item
from han.client import default_policy
from han.core import P, WireError
from han.verify import check_proof

async def test_context_blob_is_fetched_by_readers_only(env):
    """T-07, G-05, G-01 (CE1): the transcript blob of a live session's `context` item is served by blobGet to the session's actor and to
    actors with the read right, never to a member who only watches, and no longer once the session is closed."""
    ids, cl, sid = await setup(env)                                   # watch: member, read: owner
    ref = await cl["bob"].blob_put(b"the transcript", "text/plain")
    await cl["bob"].stream(item(sid, 1, "context", blob=ref, what="transcript"))
    assert await cl["alice"].blob_get(ref, S) == b"the transcript"    # owner: has the read right
    assert await cl["bob"].blob_get(ref, S) == b"the transcript"      # the session's actor
    for who in ("carol", "dave"):                                     # members watch but don't read
        with pytest.raises(WireError) as ex: await cl[who].blob_get(ref, S)
        assert ex.value.code in ("denied", "unavailable")
    other = await cl["alice"].blob_put(b"unrelated")                  # a blob no item or record names is not served
    with pytest.raises(WireError): await cl["alice"].blob_get(other, S)
    await cl["bob"].append(*cl["bob"].make_entry("session.close", S, {"session": sid}))
    with pytest.raises(WireError): await cl["alice"].blob_get(ref, S)

async def test_context_blob_must_come_from_the_sessions_actor(env):
    """G-01 (CE1): a blob a reader uploaded herself is not served just because a context item names the same hash unless the session's
    actor supplied it; an item naming bytes nobody uploaded serves nothing."""
    ids, cl, sid = await setup(env)
    ref = {"hash": "ab" * 32, "size": 3, "type": "text/plain"}
    await cl["bob"].stream(item(sid, 1, "context", blob=ref, what="plan"))
    with pytest.raises(WireError): await cl["alice"].blob_get(ref, S)
    ref2 = await cl["alice"].blob_put(b"mine")                        # uploaded by the owner, not by the session's actor
    await cl["bob"].stream(item(sid, 2, "context", blob=ref2, what="files"))
    with pytest.raises(WireError): await cl["carol"].blob_get(ref2, S)

async def test_read_state_at_a_past_seq(env):
    """§3 (CE3): read(state, at=N) returns the state after record N; it needs read rights at N, and N must exist."""
    env.actor("alice"); bob = env.actor("bob"); env.actor("eve")
    a, b, e = await env.client("alice"), await env.client("bob"), await env.client("eve")
    await mkspace(a); await member(a, bob); await say(a, "x"); await member(a, bob, "none"); await say(a, "y")
    past = (await a.read_all(S, 5, state=True, at=2))[0]
    assert past["type"] == "state" and past["state"]["head"] == 2 and past["state"]["roles"].get(bob) == "member"
    now = (await a.read_all(S, 5, state=True))[0]["state"]
    assert now["head"] == 5 and bob not in now["roles"] or now["roles"].get(bob) == "none"
    assert (await b.read_all(S, 5, state=True, at=3))[0]["state"]["head"] == 3        # bob held a role at 3
    for who, at in ((b, 5), (e, 2)):                                                   # no longer / never a reader
        with pytest.raises(WireError) as ex: await who.read_all(S, 5, state=True, at=at)
        assert ex.value.code in ("denied", "not_found")
    for at in (6, 0):
        with pytest.raises(WireError) as ex: await a.read_all(S, 5, state=True, at=at)
        assert ex.value.code == "invalid"
    with pytest.raises(WireError) as ex: await a.read_all(S, 0, at=2)                  # `at` without `state`
    assert ex.value.code == "invalid"

async def test_follow_with_limit_rereads_the_gap(env):
    """§3: a follow whose backlog is cut at `limit` leaves a gap on the wire; the client re-reads it, so the follower sees every
    record once, in order, and then the live ones."""
    env.actor("alice"); a = await env.client("alice")
    await mkspace(a)
    for i in range(6): await say(a, f"m{i}")
    col = Collector(a.read(S, 0, follow=True, limit=2))
    await col.wait_for(lambda i: i["type"] == "record" and i["record"]["receipt"]["seq"] == 7)
    await say(a, "live")
    await col.wait_for(lambda i: i["type"] == "record" and i["record"]["receipt"]["seq"] == 8)
    assert [i["record"]["receipt"]["seq"] for i in col.items if i["type"] == "record"] == list(range(1, 9))
    col.stop()

async def test_skew_is_sixty_seconds(env):
    """7.5, R-14: a receipt may lead the verifier's clock by maxClockSkewSeconds (60 s) and no more."""
    env.actor("alice"); a = await env.client("alice")
    env.clock.advance(7200)
    await mkspace(a)
    t = T0 + 7200
    assert (await verifier(env, a, now=lambda: t - 59).verify(S))["state"] == "ok"
    st = await verifier(env, a, now=lambda: t - 120).verify(S)
    assert st["state"] == "faulty" and st["proof"]["kind"] == "future-time"
    assert check_proof(st["proof"], env.seq.server_keys)[0]

async def test_verifier_gossip_signals(env):
    """R-15, R-14, T-05: verifiers gossip heads in the room as {type: heads, receipts: [...]} and announce a fault as {type: fault, proof}."""
    env.actor("alice"); bob = env.actor("bob")
    a, b = await env.client("alice"), await env.client("bob")
    await mkspace(a); await member(a, bob)
    col = Collector(b.subscribe(S)); await asyncio.sleep(0.2)
    v = verifier(env, a)
    await v.verify(S); await v.gossip(S)
    await col.wait_for(lambda i: i["type"] == "signal" and i["body"]["type"] == "heads")
    body = next(i["body"] for i in col.items if i["type"] == "signal")
    assert set(body) == {"type", "receipts"} and body["receipts"] == [v.heads()[0]]
    col.stop()

"""The Matrix binding against a real Synapse (podman): T-01/T-06, T-02, reject spoofing, R-01, read, redaction."""
import pytest
from fixtures_matrix import synapse, mx, until, open_space, say, verify_room, LOOP     # noqa: F401  (fixtures)
from han.core import P, W, WireError
from han.entries import make_entry
from han.matrix.api import MatrixAPI, MatrixError
from han.matrix.proto import REJECT, RECEIPT, SPACE, relates

pytestmark = pytest.mark.asyncio(loop_scope=LOOP)

async def test_room_bound_to_space_two_users_verify(mx):
    """T-01, T-06: a room bound to a space; two Matrix users post entries and both see receipts; the chain verifies with Mirror."""
    alice, bob = await mx.person("alice"), await mx.person("bob")
    room, space = await open_space(mx, alice, invite=[bob])
    r1 = await say(alice, room, space, "hello from alice")
    r2 = await say(bob, room, space, "hello from bob")
    assert r2["seq"] == r1["seq"] + 1
    for p in (alice, bob):
        await until(lambda p=p: len(p.client.records(room)) == 4, what=f"{p.actor} sees 4 records")
        assert [r["entry"]["kind"] for r in p.client.records(room)] == ["space.create", "member.set", "message", "message"]
        assert p.client.rooms[room].space == space and p.client.rooms[room].sequencer == mx.svc.user    # T-06
    m = verify_room(mx, bob.client, room)
    assert m.spaces[space]["state"]["head"] == 4

async def test_authorship_is_the_han_signature_not_the_sender(mx):
    """T-02, R-12: bob posts an entry that says it is alice's but is signed with bob's key: rejected. Bob relaying an entry
    alice really signed is accepted (authorship is the signature; the Matrix sender does not matter)."""
    alice, bob = await mx.person("alice"), await mx.person("bob")
    room, space = await open_space(mx, alice, invite=[bob])
    forged, ct = make_entry(bob.client.key, alice.actor, "message", space, {}, {"text": "alice says pay me"}, seen=2)
    with pytest.raises(WireError) as ex: await bob.client.append(forged, ct, room=room, timeout=30)
    assert ex.value.code in ("invalid", "denied")
    assert not [r for r in alice.client.records(room) if r["entry"]["kind"] == "message"]
    real, ct = alice.client.make_entry("message", space, {}, {"text": "genuinely alice"})
    receipt = await bob.client.append(real, ct, room=room, timeout=30)            # relayed by bob, signed by alice
    assert receipt["id"] == P.entry_id(real)
    await until(lambda: any(r["entry"]["author"] == alice.actor and r["entry"]["kind"] == "message" for r in alice.client.records(room)))
    verify_room(mx, alice.client, room)

async def test_fake_reject_receipt_and_binding_are_ignored(mx):
    """§8.2: a reject counts only from the sequencer's user. While the sequencer is held back, a member posts a fake reject (and a
    fake receipt, and a fake org.han.space naming itself); the pending entry stays pending, and then gets its real receipt."""
    alice, bob = await mx.person("alice"), await mx.person("bob")
    room, space = await open_space(mx, alice, invite=[bob])
    pl = await alice.client.api.get_state(room, "m.room.power_levels")            # bob is a moderator: he can set state, even a forged binding
    pl["users"][bob.actor] = 50
    await alice.client.api.put_state(room, "m.room.power_levels", pl)
    e, ct = alice.client.make_entry("message", space, {}, {"text": "important"})
    mx.svc.gate.clear()                                                            # the sequencer is slow
    try:
        sub = await alice.client.submit(e, ct, room=room)
        eid = sub.entry_id
        await bob.client.api.send(room, REJECT, {"id": eid, "code": "denied", "reason": "bob says no", "m.relates_to": relates(sub.event_id)}, "fake1")
        await bob.client.api.send(room, RECEIPT, {"receipt": {"space": space, "seq": 99, "id": eid}, "m.relates_to": relates(sub.event_id)}, "fake2")
        await bob.client.api.put_state(room, SPACE, {"spaceId": space, "sequencer": bob.actor})
        rv = alice.client.rooms[room]
        await until(lambda: len(rv.ignored) >= 3, what="alice sees the three forgeries")
        assert {k for k, _, _ in rv.ignored} == {"reject", "receipt", "space"} and all(s == bob.actor for _, s, _ in rv.ignored)
        assert not sub.future.done() and eid not in rv.rejects and eid not in rv.receipts      # still pending
        assert rv.sequencer == mx.svc.user
    finally: mx.svc.gate.set()
    receipt = await sub.result(20)
    assert receipt["id"] == eid and receipt["seq"] == 3                                  # the real one
    verify_room(mx, alice.client, room)

async def test_genuine_reject_from_the_sequencer(mx):
    """§8.2 refusal: Policy's `denied` comes back as org.han.reject from the service user, related to the entry's event; unbound or
    mismatched rooms are refused too."""
    alice, bob = await mx.person("alice"), await mx.person("bob")
    room, space = await open_space(mx, alice, invite=[bob])
    outsider = await mx.person("carol")
    e, ct = bob.client.make_entry("member.set", space, {"actor": outsider.actor, "role": "owner"})     # a member may not make owners
    sub = await bob.client.submit(e, ct, room=room)
    with pytest.raises(WireError) as ex: await sub.result(20)
    assert ex.value.code in ("denied", "invalid")
    await until(lambda: sub.entry_id in alice.client.rooms[room].rejects, what="alice sees the reject")
    rj = alice.client.rooms[room].rejects[sub.entry_id]
    assert rj["m.relates_to"]["event_id"] == sub.event_id
    e, ct = bob.client.make_entry("message", "tother:a.localhost", {}, {"text": "wrong space"})
    with pytest.raises(WireError) as ex: await bob.client.append(e, ct, room=room, timeout=20)
    assert ex.value.code == "invalid" and space in ex.value.message
    room2 = await alice.client.create_space_room("unbound")
    e, ct = alice.client.make_entry("message", space, {}, {"text": "not in this room"})
    with pytest.raises(WireError) as ex: await alice.client.append(e, ct, room=room2, timeout=20)
    assert ex.value.code == "not_found"

async def test_resend_gets_the_same_receipt(mx):
    """R-01: the same entry sent again as a new Matrix event gets the same receipt (and the log gains nothing)."""
    alice, bob = await mx.person("alice"), await mx.person("bob")
    room, space = await open_space(mx, alice, invite=[bob])
    e, ct = bob.client.make_entry("message", space, {}, {"text": "once"})
    s1 = await bob.client.submit(e, ct, room=room); r1 = await s1.result(20)
    s2 = await bob.client.submit(e, ct, room=room); r2 = await s2.result(20)
    assert s1.event_id != s2.event_id and r1 == r2
    await until(lambda: len(bob.client.records(room)) == 3)
    async def receipts_in_room():
        msgs = await alice.client.api.messages(room, None, 100)
        return [ev["content"]["receipt"] for ev in msgs["chunk"] if ev["type"] == "org.han.receipt" and ev["content"]["receipt"]["id"] == r1["id"]]
    async def two():
        return len(await receipts_in_room()) == 2
    await until(two, what="a receipt for each of the two events")
    receipts = await receipts_in_room()
    assert len(receipts) == 2 and receipts[0] == receipts[1]                       # one receipt event per Matrix event, identical
    assert mx.seq.home.spaces[space]["state"]["head"] == 3
    verify_room(mx, alice.client, room)

async def test_read_endpoint_needs_han_auth_and_read_rights(mx):
    """§8.2 read: GET /_han/v1/read with `Authorization: HAN <actor> <keyId> <proof>` over a challenge; P-02 applies."""
    alice, bob = await mx.person("alice"), await mx.person("bob")
    room, space = await open_space(mx, alice, invite=[bob])
    await say(bob, room, space, "hi")
    items = await alice.client.read_http(space, 0, state=True)
    assert items[0]["type"] == "state" and [i["record"]["receipt"]["seq"] for i in items[1:]] == [1, 2, 3]
    assert [i["record"]["receipt"]["seq"] for i in await alice.client.read_http(space, 2)] == [3]
    stranger = await mx.person("dave", start=False)
    with pytest.raises(WireError) as ex: await stranger.client.read_http(space, 0)
    assert ex.value.code == "not_found"                                           # not a member: same as a missing space
    svc = MatrixAPI(f"http://127.0.0.1:{mx.as_port}")
    ch = await svc.request("GET", "/_han/v1/challenge")
    bad = W.hello_proof(bob.client.key, ch["challenge"], ch["server"])            # bob's proof, but claiming to be alice
    for hdr in ({"Authorization": f"HAN {alice.actor} k1 {bad}"}, {"Authorization": "HAN nobody"}, {}):
        with pytest.raises(MatrixError) as mex: await svc.request("GET", "/_han/v1/read", params={"space": space}, headers=hdr)
        assert mex.value.status == 401
    ch2 = await svc.request("GET", "/_han/v1/challenge")
    ok = W.hello_proof(alice.client.key, ch2["challenge"], ch2["server"])
    hdr = {"Authorization": f"HAN {alice.actor} k1 {ok}"}                          # no HAN-Challenge header: any open challenge is tried
    assert (await svc.request("GET", "/_han/v1/read", params={"space": space}, headers=hdr))["items"]
    with pytest.raises(MatrixError): await svc.request("GET", "/_han/v1/read", params={"space": space}, headers=hdr)   # single use
    await svc.close()

async def test_redaction_reaches_matrix(mx):
    """P-12 on Matrix (§8.2): after a `redact` receipt the original event's content is gone from Synapse; the header stays (R-04) and
    a client that joins later still verifies the chain, with the record marked redacted."""
    alice = await mx.person("alice")
    room, space = await open_space(mx, alice)
    r = await say(alice, room, space, "secret text")
    e, ct = alice.client.make_entry("redact", space, {"target": r["id"]}, {"reason": "oops"})
    await alice.client.append(e, ct, room=room, timeout=30)
    async def gone():
        ms = (await alice.client.api.messages(room, None, 100))["chunk"]
        return "secret text" not in str(ms)
    await until(gone, what="Synapse redacted the original event")
    carol = await mx.person("carol")
    await alice.client.api.invite(room, carol.actor); await carol.client.join(room)
    await until(lambda: len(carol.client.records(room)) == 3, what="carol sees all three records")
    rec = next(x for x in carol.client.records(room) if x["receipt"]["id"] == r["id"])
    assert rec["redacted"] and rec["content"] is None and rec["redactedBy"]["entry"]["kind"] == "redact"
    verify_room(mx, carol.client, room)

async def test_oversized_entry_is_refused_by_matrix_not_retried_forever(mx):
    """Matrix events are limited to 64 KiB; a HAN entry may be bigger (header 16 KiB + content 64 KiB). Such an entry cannot go over
    Matrix: the client fails it at once (use the generic binding), it does not loop."""
    alice = await mx.person("alice")
    room, space = await open_space(mx, alice)
    e, ct = alice.client.make_entry("message", space, {}, {"text": "x" * 32000, "blobs": [{"hash": "0" * 64, "size": 1, "type": "text/plain", "uri": "mxc://a.localhost/" + "y" * 900} for _ in range(40)]})
    with pytest.raises(WireError) as ex: await alice.client.append(e, ct, room=room, timeout=20)
    assert ex.value.code == "invalid" and "M_TOO_LARGE" in ex.value.message

async def test_outbox_resumes_after_a_restart(mx):
    """R-07 over Matrix: an entry stored in the outbox but never answered is sent again by a restarted client and gets its receipt."""
    from han.matrix.client import MatrixClient
    alice = await mx.person("alice")
    room, space = await open_space(mx, alice)
    e, ct = alice.client.make_entry("message", space, {}, {"text": "survives a restart"})
    mx.svc.gate.clear()
    try:
        await alice.client.submit(e, ct, room=room)
        await alice.client.close()
    finally: mx.svc.gate.set()
    local = alice.actor[1:].split(":")[0]
    again = MatrixClient(mx.syn.base_url, alice.actor, alice.token, alice.key_path, f"{mx.tmp}/{local}.outbox.db", service_url=f"http://127.0.0.1:{mx.as_port}")
    mx.clients.append(again)
    await again.start()
    await until(lambda: any(r["entry"]["body"] == {} and r["content"] == {"text": "survives a restart"} for r in again.records(room)), what="receipt for the stored entry")
    assert again.outbox.get(P.entry_id(e))["state"] == "done"

async def test_records_ordered_while_matrix_was_unreachable_are_published_on_restart(mx):
    """T-01 catch-up: records appended to the sequencer while the appservice was down (here: through the sequencer directly, as
    the generic binding would) reach the room when the appservice starts again."""
    from han.matrix.appservice import AppService
    import os
    alice = await mx.person("alice")
    room, space = await open_space(mx, alice)
    await mx.svc.stop()
    e, ct = alice.client.make_entry("message", space, {}, {"text": "ordered while down"})
    mx.seq.append(e, ct)
    mx.svc = await AppService(mx.seq, mx.syn.base_url, mx.syn.as_token, mx.syn.hs_token, os.path.join(mx.tmp, "as.db")).start("127.0.0.1", mx.as_port)
    await until(lambda: any(r["content"] == {"text": "ordered while down"} for r in alice.client.records(room)), what="the record reaches the room")
    verify_room(mx, alice.client, room)

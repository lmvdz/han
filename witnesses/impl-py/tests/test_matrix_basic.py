"""The Matrix binding against a real Synapse (podman): T-01/T-06, T-02, reject spoofing, R-01, read, redaction."""
import asyncio, json
import pytest
from fixtures_matrix import synapse, mx, until, open_space, say, verify_room, LOOP     # noqa: F401  (fixtures)
from han.core import P, W, WireError
from han.entries import make_entry
from han.matrix.api import MatrixAPI, MatrixError
from han.matrix.proto import ENTRY, REJECT, RECEIPT, SPACE, relates

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
    m = await verify_room(mx, bob.client, room)
    assert m.spaces[space]["state"]["head"] == 4

async def test_authorship_is_the_han_signature_not_the_sender(mx):
    """T-02, R-12: bob posts an entry that says it is alice's but is signed with bob's key: rejected. Bob cannot even upload an entry
    alice really signed (E4: the authenticated actor of `POST /_han/v1/entry` is the entry's author); alice's own append is accepted."""
    alice, bob = await mx.person("alice"), await mx.person("bob")
    room, space = await open_space(mx, alice, invite=[bob])
    forged, ct = make_entry(bob.client.key, alice.actor, "message", space, {}, {"text": "alice says pay me"}, seen=2)
    with pytest.raises(WireError) as ex: await bob.client.append(forged, ct, room=room, timeout=30)
    assert ex.value.code in ("invalid", "denied")
    assert not [r for r in alice.client.records(room) if r["entry"]["kind"] == "message"]
    await alice.client.fill_content(room)
    assert not [r for r in alice.client.records(room) if r["entry"]["kind"] == "message"]
    real, ct = alice.client.make_entry("message", space, {}, {"text": "genuinely alice"})
    with pytest.raises(WireError) as ex: await bob.client.append(real, ct, room=room, timeout=30)             # relayed by bob, signed by alice
    assert ex.value.code == "denied"
    receipt = await alice.client.append(real, ct, room=room, timeout=30)
    assert receipt["id"] == P.entry_id(real)
    await until(lambda: any(r["entry"]["author"] == alice.actor and r["entry"]["kind"] == "message" for r in alice.client.records(room)))
    await verify_room(mx, alice.client, room)

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
    await verify_room(mx, alice.client, room)

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
    await verify_room(mx, alice.client, room)

async def test_read_endpoint_needs_han_auth_and_read_rights(mx):
    """§8.2 read: GET /_han/v1/read with `Authorization: HAN <actor> <keyId> <proof>` over a challenge; P-02 applies."""
    alice, bob = await mx.person("alice"), await mx.person("bob")
    room, space = await open_space(mx, alice, invite=[bob])
    await say(bob, room, space, "hi")
    items = await alice.client.read_http(space, 0)
    assert [i["record"]["receipt"]["seq"] for i in items] == [1, 2, 3]
    assert (await alice.client.state(space))["state"]["head"] == 3                         # GET state (H10)
    with pytest.raises(WireError) as ex: await alice.client.state(space, at=1, effect="0" * 64)         # two selectors: invalid by presence
    assert ex.value.code == "invalid"
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
    hdr = {"Authorization": f"HAN {alice.actor} k1 {ok}"}
    with pytest.raises(MatrixError) as mex: await svc.request("GET", "/_han/v1/read", params={"space": space}, headers=hdr)    # the challenge must be named
    assert mex.value.status == 401
    ch2 = await svc.request("GET", "/_han/v1/challenge")
    ok = W.hello_proof(alice.client.key, ch2["challenge"], ch2["server"])
    hdr = {"Authorization": f"HAN {alice.actor} k1 {ok}", "HAN-Challenge": ch2["challenge"]}
    assert (await svc.request("GET", "/_han/v1/read", params={"space": space}, headers=hdr))["items"]       # {items: [...]}
    with pytest.raises(MatrixError): await svc.request("GET", "/_han/v1/read", params={"space": space}, headers=hdr)   # single use
    await svc.close()

async def test_redact_over_matrix_keeps_headers_and_proof(mx):
    """P-12, R-04 on Matrix (P-02 binding): the room never held a header or a body, so a redact leaves nothing to strip there; the HTTP
    read gives the record marked redacted with its proof. A late joiner with no HAN role sees the receipts and no record."""
    alice = await mx.person("alice")
    room, space = await open_space(mx, alice)
    r = await say(alice, room, space, "secret text")
    e, ct = alice.client.make_entry("redact", space, {"target": r["id"]}, {"reason": "oops"})
    await alice.client.append(e, ct, room=room, timeout=30)
    assert "secret text" not in str((await alice.client.api.messages(room, None, 100))["chunk"])
    carol = await mx.person("carol")
    await alice.client.api.invite(room, carol.actor); await carol.client.join(room)
    await until(lambda: len(carol.client.rooms[room].receipts) == 3, what="carol sees the three receipts")
    assert carol.client.records(room) == []                                       # no role: no header, no body (E4)
    items = await alice.client.read_http(space, 0)
    rec = next(x["record"] for x in items if x["record"]["receipt"]["id"] == r["id"])
    assert rec["redacted"] and rec["content"] is None and rec["redactedBy"]["entry"]["kind"] == "redact"
    await verify_room(mx, alice.client, room)

async def test_room_carries_no_content_and_no_reader_beyond_p02(mx):
    """P-02, E4 (§8.2): `org.han.entry` events are `{id}` (a client's) or `{id, kind, author, seq}` (the sequencer's) and receipts carry
    no header or body, so the room shows none. Bob, removed from the space, reads only the seqs he held a role at (HTTP read) and has the
    receipts of what came after; carol, who joined the Matrix room late with no HAN role, has receipts and ids, no record at all, and the
    HTTP read says not_found."""
    alice, bob = await mx.person("alice"), await mx.person("bob")
    room, space = await open_space(mx, alice, invite=[bob])
    await say(alice, room, space, "while bob was a member")
    e, ct = alice.client.make_entry("member.set", space, {"actor": bob.actor, "role": "none"})
    await alice.client.append(e, ct, room=room, timeout=30)
    await say(alice, room, space, "after bob was removed")
    carol = await mx.person("carol")
    await alice.client.api.invite(room, carol.actor); await carol.client.join(room)
    chunk = (await alice.client.api.messages(room, None, 100))["chunk"]
    assert "while bob" not in str(chunk) and "after bob" not in str(chunk)
    entry_events = [ev["content"] for ev in chunk if ev["type"] == ENTRY]
    assert entry_events and all(set(c) in ({"id"}, {"id", "kind", "author", "seq"}) for c in entry_events)
    await until(lambda: len(bob.client.rooms[room].receipts) == 5 and len(carol.client.rooms[room].receipts) == 5, what="both see all five receipts")
    await bob.client.fill_content(room)
    texts = {r["receipt"]["seq"]: (r["content"] or {}).get("text") for r in bob.client.records(room) if r["entry"]["kind"] == "message"}
    assert texts == {3: "while bob was a member"}                               # removed: what came after his role ended is not his to read
    assert carol.client.records(room) == []
    with pytest.raises(WireError) as ex: await carol.client.read_http(space, 0)
    assert ex.value.code == "not_found"
    assert all(r["content"] is None for r in carol.client.records(room))

async def test_oversized_entry_is_refused_by_matrix_not_retried_forever(mx):
    """A HAN entry may be bigger (header 16 KiB + content 64 KiB) than the 60 KiB the Matrix binding carries. The client refuses it at
    once with a clear error (use the generic binding), it does not loop."""
    alice = await mx.person("alice")
    room, space = await open_space(mx, alice)
    e, ct = alice.client.make_entry("message", space, {}, {"text": "x" * 32000, "blobs": [{"hash": "0" * 64, "size": 1, "type": "text/plain", "uri": "mxc://a.localhost/" + "y" * 900} for _ in range(40)]})
    with pytest.raises(WireError) as ex: await alice.client.append(e, ct, room=room, timeout=20)
    assert ex.value.code == "invalid" and "generic binding" in ex.value.message

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
    await until(lambda: P.entry_id(e) in again.rooms[room].receipts, what="receipt for the stored entry")
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
    await until(lambda: any(r["receipt"]["id"] == P.entry_id(e) for r in alice.client.records(room)), what="the record reaches the room")
    await verify_room(mx, alice.client, room)

# ── E4: the whole entry goes to the service, the room gets only ids ──
SECRET = "the launch code is 12345"
HEADER_FIELDS = {"entry", "content", "contentHash", "sig", "nonce", "keyId", "body", "refs", "seen", "time", "space", "v"}

async def test_e4_member_who_cannot_read_the_log_sees_no_header_or_body(mx):
    """E4, P-02: carol is in the Matrix room with no HAN role. In no room event she can read is there a header field or a body: client
    entry events are `{id}`, the sequencer's are `{id, kind, author, seq}` (also for a record ordered over another binding, T-01),
    receipts carry ids and hashes. She holds no record, and the service answers her read with not_found."""
    alice = await mx.person("alice")
    room, space = await open_space(mx, alice)
    carol = await mx.person("carol")
    await alice.client.api.invite(room, carol.actor); await carol.client.join(room)
    await say(alice, room, space, SECRET)
    e, ct = alice.client.make_entry("message", space, {}, {"text": SECRET + " (generic binding)"})
    mx.seq.append(e, ct)                                                           # ordered by another binding: flushed to the room by the service
    await until(lambda: P.entry_id(e) in alice.client.rooms[room].receipts, what="the generic-binding record reaches the room")
    chunk = (await carol.client.api.messages(room, None, 100))["chunk"]
    assert "launch code" not in str(chunk)
    entry_events = [ev["content"] for ev in chunk if ev["type"] == ENTRY]
    assert len(entry_events) >= 3 and all(set(c) in ({"id"}, {"id", "kind", "author", "seq"}) for c in entry_events)
    assert any(set(c) == {"id", "kind", "author", "seq"} and c["id"] == P.entry_id(e) for c in entry_events)       # the flush, not {entry, ...}
    for ev in chunk:
        c = ev["content"]
        if ev["type"] == RECEIPT: assert set(c["receipt"]) <= {"space", "seq", "id", "prev", "home", "homeKey", "time", "durability", "profiles", "homeSig"}
        if ev["type"] in (ENTRY, RECEIPT, REJECT): assert not HEADER_FIELDS & set(c) - {"receipt"}, ev
    await until(lambda: len(carol.client.rooms[room].receipts) >= 3, what="carol sees the receipts")
    assert carol.client.records(room) == [] and carol.client.rooms[room].entries == {}
    with pytest.raises(WireError) as ex: await carol.client.read_http(space, 0)
    assert ex.value.code == "not_found"

async def test_e4_unavailable_is_a_reject_that_is_not_final(mx):
    """E4: `unavailable` is posted as `org.han.reject {id, code: "unavailable", retryAfterMs}`. The client treats it as non-final: it
    keeps no reject for the entry, waits retryAfterMs (far less than its 10 s resend timer) and gets the receipt."""
    alice = await mx.person("alice")
    room, space = await open_space(mx, alice)
    real, fails = mx.seq.append, [2]
    def flaky(entry, content=None):
        if entry["kind"] == "message" and fails[0] > 0:
            fails[0] -= 1; raise WireError("unavailable", "storage is catching up", retry_after_ms=400)
        return real(entry, content)
    mx.seq.append = flaky
    try:
        e, ct = alice.client.make_entry("message", space, {}, {"text": "eventually"})
        t0 = asyncio.get_running_loop().time()
        receipt = await alice.client.append(e, ct, room=room, timeout=8)
        took = asyncio.get_running_loop().time() - t0
    finally: mx.seq.append = real
    assert receipt["id"] == P.entry_id(e) and fails[0] == 0 and took < 8
    rv = alice.client.rooms[room]
    assert P.entry_id(e) not in rv.rejects                                         # not final: nothing remembered as a refusal
    rejects = [ev["content"] for ev in (await alice.client.api.messages(room, None, 100))["chunk"] if ev["type"] == REJECT]
    assert len(rejects) == 2 and all(r["code"] == "unavailable" and r["retryAfterMs"] == 400 and r["id"] == P.entry_id(e) for r in rejects)
    assert alice.client.outbox.get(P.entry_id(e))["state"] == "done"

async def test_e4_entry_upload_is_authenticated_and_bounded(mx):
    """E4: `POST /_han/v1/entry` needs HAN authentication and applies the header and content limits; the old `/content` is gone."""
    alice = await mx.person("alice")
    room, space = await open_space(mx, alice)
    svc = alice.client.service
    with pytest.raises(MatrixError) as mex: await svc.request("POST", "/_han/v1/entry", body={"entry": {}})
    assert mex.value.status == 401
    e, ct = alice.client.make_entry("message", space, {}, {"text": "x"})
    assert (await alice.client._han("POST", "/_han/v1/entry", body={"entry": e, "content": ct})) == {"id": P.entry_id(e)}
    big, _ = alice.client.make_entry("message", space, {"pad": "p" * 17000}, {"text": "x"})
    with pytest.raises(WireError) as ex: await alice.client._han("POST", "/_han/v1/entry", body={"entry": big, "content": ct})
    assert ex.value.code == "invalid" and "header" in ex.value.message
    e2, _ = alice.client.make_entry("message", space, {}, {"text": "y" * 70000})
    with pytest.raises(WireError) as ex: await alice.client._han("POST", "/_han/v1/entry", body={"entry": e2, "content": {"text": "y" * 70000}})
    assert ex.value.code == "invalid" and "content" in ex.value.message
    with pytest.raises(MatrixError) as mex: await alice.client._han("POST", "/_han/v1/content", body={"text": "x"})
    assert mex.value.status in (404, 405)

# ── E11: Retry-After, per-client limits ──
async def test_e11_503_has_retry_after_and_limits_are_per_client(mx):
    from han.matrix.appservice import AppService, MAX_CHALLENGES_PER_CLIENT, MAX_ENTRIES_PER_ACTOR
    r = AppService._fail(None, WireError("unavailable", "later", retry_after_ms=2500))
    assert r.status == 503 and r.headers["Retry-After"] == "3" and json.loads(r.body)["error"]["retryAfterMs"] == 2500
    assert AppService._fail(None, WireError("unavailable", "later")).headers["Retry-After"] == "1"
    assert "Retry-After" not in AppService._fail(None, WireError("denied", "no")).headers
    alice = await mx.person("alice")
    room, space = await open_space(mx, alice)
    svc = alice.client.service
    mx.svc.challenges.clear()
    for _ in range(MAX_CHALLENGES_PER_CLIENT): await svc.request("GET", "/_han/v1/challenge")
    with pytest.raises(MatrixError) as mex: await svc.request("GET", "/_han/v1/challenge")             # one more open challenge from this address
    assert mex.value.status == 429 and mex.value.body["error"]["code"] == "limit"
    mx.svc.challenges.clear()
    for i in range(MAX_ENTRIES_PER_ACTOR):
        e, ct = alice.client.make_entry("message", space, {}, {"text": f"pending {i}"})
        await alice.client._han("POST", "/_han/v1/entry", body={"entry": e, "content": ct})
    e, ct = alice.client.make_entry("message", space, {}, {"text": "one too many"})
    with pytest.raises(WireError) as ex: await alice.client._han("POST", "/_han/v1/entry", body={"entry": e, "content": ct})
    assert ex.value.code == "limit" and ex.value.retry_after_ms

"""T-07 over Matrix against a real Synapse: a watch room and a read room per session, membership following sessionAccess,
context blobs as encrypted attachments. (End-to-end encryption of the rooms is not implemented: see han/matrix/NOTES.md.)"""
import pytest
from fixtures_matrix import synapse, mx, until, open_space, verify_room, LOOP     # noqa: F401  (fixtures)
from han.client import default_policy
from han.core import P, WireError
from han.entries import fmt_time
from han.matrix.api import MatrixError
from han.matrix.proto import SESSION, STREAM

pytestmark = pytest.mark.asyncio(loop_scope=LOOP)

async def members_of(mx, room):
    """Who is invited or joined, as the service user sees the room."""
    return {u for u, m in (await mx.svc.api.members(room)).items() if m in ("join", "invite")}

def members_are(mx, room, want):
    async def f(): return await members_of(mx, room) == set(want) | {mx.svc.user}
    return f

async def setup(mx):
    """alice (owner), bob and carol (members), a bot (agent, member) in one space whose policy lets members watch and only owners
    read; the bot opens an `auto` session. Returns everything the tests need."""
    alice, bob, carol, bot = await mx.person("alice"), await mx.person("bob"), await mx.person("carol"), await mx.person("bot", "agent")
    pol = default_policy(sessions={"watch": "member", "read": "owner", "steer": "owner", "modes": ["auto", "guarded"]})
    room, space = await open_space(mx, alice, invite=[bob, carol, bot], policy=pol)
    e, ct = bot.client.make_entry("session.open", space, {"steerMode": "auto"})
    r = await bot.client.append(e, ct, room=room, timeout=30)
    return alice, bob, carol, bot, room, space, r["id"]

def item(space, sid, n, type="output", **kw):
    return {"space": space, "session": sid, "n": n, "time": fmt_time(1_790_000_000 + n), "type": type} | kw

async def test_rooms_created_with_the_stated_settings(mx):
    """T-07: on the session.open receipt the service user creates two rooms, links them from the space room (org.han.session), with
    the stated power levels, history_visibility joined, join_rule invite, guest_access forbidden."""
    alice, bob, carol, bot, room, space, sid = await setup(mx)
    rooms = await alice.client.session_rooms(room, sid)
    svc = mx.svc.user
    for which in ("watch", "read"):
        r = rooms[which]
        pl = await mx.svc.api.get_state(r, "m.room.power_levels")
        assert pl["users"][svc] == 100 and pl["users"][bot.actor] == 50 and pl["users"].get(alice.actor, 0) == 0
        assert pl["events"][STREAM] == 50 and all(pl[k] == 100 for k in ("invite", "kick", "ban", "redact", "state_default"))
        assert (await mx.svc.api.get_state(r, "m.room.history_visibility"))["history_visibility"] == "joined"
        assert (await mx.svc.api.get_state(r, "m.room.join_rules"))["join_rule"] == "invite"
        assert (await mx.svc.api.get_state(r, "m.room.guest_access"))["guest_access"] == "forbidden"
    link = await alice.client.api.get_state(room, SESSION, sid)
    assert link == {"session": sid, "watch": rooms["watch"], "read": rooms["read"]}
    # membership equals who sessionAccess grants: watch = every member; read = owners (and the session's own actor)
    await until(members_are(mx, rooms["watch"], [alice.actor, bob.actor, carol.actor, bot.actor]), what="watch room members")
    await until(members_are(mx, rooms["read"], [alice.actor, bot.actor]), what="read room members")

async def test_items_reach_only_the_granted_and_membership_follows_sessionAccess(mx):
    """T-07: output reaches watchers, context only readers; after a receipt removes a right (a role change, a removal) the
    publisher's next item does not reach that actor; session.close removes everyone."""
    alice, bob, carol, bot, room, space, sid = await setup(mx)
    svc = mx.svc.user
    watch = await bot.client.join_session(room, sid, "watch"); read = await bot.client.join_session(room, sid, "read")
    await alice.client.join_session(room, sid, "watch"); aread = await alice.client.join_session(room, sid, "read")
    bw = await bob.client.join_session(room, sid, "watch"); cw = await carol.client.join_session(room, sid, "watch")
    with pytest.raises(MatrixError) as ex: await bob.client.join_session(room, sid, "read", wait=1.5)     # not invited to the read room
    assert ex.value.errcode == "M_FORBIDDEN"

    await bot.client.publish(room, item(space, sid, 1, text="step 1"))
    snapshot = b"transcript bytes " * 100
    await bot.client.publish(room, item(space, sid, 2, "context", what="transcript"), blob=snapshot)
    for p, r in ((alice, watch), (bob, bw), (carol, cw)):
        await until(lambda p=p, r=r: [i["text"] for _, i, _ in p.client.rooms[r].stream if i["type"] == "output"] == ["step 1"], what=f"{p.actor} sees step 1")
    await until(lambda: any(i["type"] == "context" for _, i, _ in alice.client.rooms[aread].stream), what="alice (owner) sees the context item")
    ctx = next(i for _, i, _ in alice.client.rooms[aread].stream if i["type"] == "context")
    assert ctx["blob"]["uri"].startswith("mxc://") and await alice.client.fetch_context(ctx) == snapshot       # decrypts, hash checked
    raw = await alice.client.api.download(ctx["blob"]["uri"])
    assert snapshot[:40] not in raw                                                                            # the mxc bytes are ciphertext
    assert not any(i["type"] == "context" for r in (bw, cw) for _, i, _ in (bob if r == bw else carol).client.rooms[r].stream)   # never in the watch room
    assert {sender for sender, _, _ in alice.client.rooms[watch].stream} == {bot.actor}                          # only the actor posts

    # a role change removes carol's watch right (member → guest); the removal of bob (role none) too, later
    e, ct = alice.client.make_entry("member.set", space, {"actor": carol.actor, "role": "guest"})
    await alice.client.append(e, ct, room=room, timeout=30)
    await bot.client.publish(room, item(space, sid, 3, text="after carol was demoted"))          # waits for the kick to land first
    assert await members_of(mx, watch) == {alice.actor, bob.actor, bot.actor, svc}
    for p, r in ((alice, watch), (bob, bw)):
        await until(lambda p=p, r=r: any(i.get("text") == "after carol was demoted" for _, i, _ in p.client.rooms[r].stream), what=f"{p.actor} sees item 3")
    texts = [i.get("text") for _, i, _ in carol.client.rooms[cw].stream]
    assert "after carol was demoted" not in texts and "step 1" in texts                          # what she had stays received
    history = await carol.client.api.messages(watch, None, 100)                                  # and Synapse itself no longer serves it
    assert "after carol was demoted" not in str(history["chunk"])

    e, ct = alice.client.make_entry("member.set", space, {"actor": bob.actor, "role": "none"})
    await alice.client.append(e, ct, room=room, timeout=30)
    await bot.client.publish(room, item(space, sid, 4, text="after bob left"))
    await until(lambda: any(i.get("text") == "after bob left" for _, i, _ in alice.client.rooms[watch].stream))
    assert "after bob left" not in str((await bob.client.api.messages(watch, None, 100))["chunk"])
    assert await members_of(mx, watch) == {alice.actor, bot.actor, svc}

    e, ct = bot.client.make_entry("session.close", space, {"session": sid})
    await bot.client.append(e, ct, room=room, timeout=30)
    for r in (watch, read):
        await until(members_are(mx, r, []), what="everyone kicked on session.close")
    with pytest.raises(MatrixError): await bot.client.publish(room, item(space, sid, 5, text="too late"))     # the actor is out too
    await verify_room(mx, alice.client, room)

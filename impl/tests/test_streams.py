"""G-05 / T-07 / P-15: session streams are delivered per right, evaluated at the head for every item."""
import asyncio, pytest
from conftest import S, mkspace, member, say, Collector
from han.client import default_policy
from han.core import P, WireError

TIME = "2026-10-10T12:00:00Z"
def item(session, n, type="output", **kw):
    base = {"space": S, "session": session, "n": n, "time": TIME, "type": type}
    return base | ({"text": f"line {n}"} if type == "output" and not kw else kw)

async def setup(env, sessions=None):
    """alice (owner), bob (opens the session), carol and dave (members). Rights: watch member, read owner, steer owner."""
    names = ["alice", "bob", "carol", "dave"]
    ids = {n: env.actor(n) for n in names}
    cl = {n: await env.client(n) for n in names}
    pol = default_policy(sessions=sessions or {"watch": "member", "read": "owner", "steer": "owner", "modes": ["auto", "guarded"]})
    await mkspace(cl["alice"], policy=pol)
    for n in names[1:]: await member(cl["alice"], ids[n])
    r = await cl["bob"].append(*cl["bob"].make_entry("session.open", S, {"steerMode": "auto"}))
    return ids, cl, r["id"]

async def test_watchers_get_items_and_context_only_readers(env):
    """G-05, T-07: output reaches everyone with the watch right; context items only those with the read right."""
    ids, cl, sid = await setup(env)
    col = {n: Collector(cl[n].watch(S, sid)) for n in ("alice", "carol")}
    await asyncio.sleep(0.2)
    pub = cl["bob"]
    await pub.stream(item(sid, 1))
    ref = {"hash": "0" * 64, "size": 1, "type": "text/plain"}
    await pub.stream(item(sid, 2, "context", blob=ref, what="transcript"))
    await pub.stream(item(sid, 3, "status", state="working"))
    await col["carol"].wait_for(lambda i: i["n"] == 3); await col["alice"].wait_for(lambda i: i["n"] == 3)
    assert [i["n"] for i in col["alice"].items] == [1, 2, 3]            # owner reads: sees context
    assert [i["n"] for i in col["carol"].items] == [1, 3]               # member watches only
    for c in col.values(): c.stop()

async def test_removed_watcher_gets_nothing_after_the_receipt(env):
    """T-07 / G-05: after a member.set removes a watcher, items published afterwards never reach it; others still get them."""
    ids, cl, sid = await setup(env)
    dave, carol = Collector(cl["dave"].watch(S, sid)), Collector(cl["carol"].watch(S, sid))
    await asyncio.sleep(0.2)
    await cl["bob"].stream(item(sid, 1))
    await dave.wait_for(lambda i: i["n"] == 1)
    await member(cl["alice"], ids["dave"], "none")                      # dave loses his role
    await cl["bob"].stream(item(sid, 2)); await cl["bob"].stream(item(sid, 3))
    await carol.wait_for(lambda i: i["n"] == 3)
    await asyncio.sleep(0.1)
    assert [i["n"] for i in dave.items] == [1]
    assert [i["n"] for i in carol.items] == [1, 2, 3]
    dave.stop(); carol.stop()

async def test_policy_change_revokes_read_right_for_context(env):
    """G-05: a policy.set that raises the read level stops context items for a watcher who still watches."""
    ids, cl, sid = await setup(env, {"watch": "member", "read": "member", "steer": "owner", "modes": ["auto"]})
    carol = Collector(cl["carol"].watch(S, sid)); await asyncio.sleep(0.2)
    ref = {"hash": "0" * 64, "size": 1, "type": "text/plain"}
    await cl["bob"].stream(item(sid, 1, "context", blob=ref, what="plan"))
    await carol.wait_for(lambda i: i["n"] == 1)
    pol = default_policy(sessions={"watch": "member", "read": "owner", "steer": "owner", "modes": ["auto"]})
    await cl["alice"].append(*cl["alice"].make_entry("policy.set", S, {"policy": pol}))
    await cl["bob"].stream(item(sid, 2, "context", blob=ref, what="plan")); await cl["bob"].stream(item(sid, 3))
    await carol.wait_for(lambda i: i["n"] == 3)
    assert [i["n"] for i in carol.items] == [1, 3]
    carol.stop()

async def test_only_the_sessions_actor_publishes_in_order(env):
    """G-05: stream is accepted only from the session's actor, with n one more than the last item; and never from a stranger."""
    ids, cl, sid = await setup(env)
    with pytest.raises(WireError) as ex: await cl["carol"].stream(item(sid, 1))
    assert ex.value.code == "denied"
    with pytest.raises(WireError) as ex: await cl["bob"].stream(item(sid, 2))
    assert ex.value.code == "invalid"
    await cl["bob"].stream(item(sid, 1))
    await cl["bob"].stream(item(sid, 1))                                  # a resend of the last item is harmless
    with pytest.raises(WireError) as ex: await cl["bob"].stream(item(sid, 1, text="different"))
    assert ex.value.code == "invalid"
    with pytest.raises(WireError) as ex: await cl["bob"].stream(dict(item(sid, 2), extra=1))
    assert ex.value.code == "invalid"
    with pytest.raises(WireError) as ex: await cl["bob"].stream(item("f" * 64, 1))
    assert ex.value.code == "not_found"

async def test_watch_needs_the_right_and_a_live_session(env):
    """G-05, P-15: watch is denied without the watch right; after session.close (the session has left the state) the actor can't publish."""
    ids, cl, sid = await setup(env, {"watch": "owner", "read": "owner", "steer": "owner", "modes": ["auto"]})
    with pytest.raises(WireError) as ex:
        async for _ in cl["carol"].watch(S, sid): pass
    assert ex.value.code == "denied"
    await cl["bob"].stream(item(sid, 1))
    await cl["bob"].append(*cl["bob"].make_entry("session.close", S, {"session": sid}))
    with pytest.raises(WireError) as ex: await cl["bob"].stream(item(sid, 2))
    assert ex.value.code == "not_found"                 # F3(b): a closed session has left the state

async def test_stream_counter_survives_restart(env):
    """G-05: n continues after a server restart (stream items are persisted)."""
    ids, cl, sid = await setup(env)
    await cl["bob"].stream(item(sid, 1)); await cl["bob"].stream(item(sid, 2))
    await env.bounce()
    with pytest.raises(WireError) as ex: await cl["bob"].stream(item(sid, 2) | {"text": "other"})
    assert ex.value.code == "invalid"
    await cl["bob"].stream(item(sid, 3))

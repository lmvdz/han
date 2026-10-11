"""R-01, R-06, R-07: dedupe, unknown outcome, and the client's durable outbox."""
import asyncio, pytest
from conftest import S, mkspace, member, say, chain
from han.core import P, W, WireError
from han.seq import DropConnection

async def test_dedupe_same_receipt(env):
    """R-01: the same entry sent twice gets the same receipt and is stored once."""
    env.actor("alice"); a = await env.client("alice")
    await mkspace(a)
    e, c = a.make_entry("message", S, {}, {"text": "once"})
    r1 = await a.append(e, c)
    r2 = await a._request("append", {"entry": e, "content": c})       # a raw resend on the wire
    assert r1 == r2
    assert len([r for r in await chain(a) if r["entry"]["nonce"] == e["nonce"]]) == 1

async def test_connection_dropped_after_receive_retries(env):
    """R-06: the connection drops after the server stored the entry but before the receipt; the client resends the same entry and gets the same receipt."""
    env.actor("alice"); a = await env.client("alice")
    await mkspace(a)
    calls = []
    async def hook(receipt):
        calls.append(receipt["seq"])
        if len(calls) == 1: raise DropConnection()
    env.hook = hook
    e, c = a.make_entry("message", S, {}, {"text": "x"})
    r = await asyncio.wait_for(a.append(e, c), 5)
    assert len(calls) == 2 and r["seq"] == 2 and r["id"] == P.entry_id(e)
    assert len(await chain(a)) == 2                                    # one record, not two

async def test_client_killed_between_send_and_receipt(env):
    """R-01, R-06, R-07: the client dies after the server received the entry; a restarted client resends from its outbox, gets the same receipt, exactly one record exists."""
    env.actor("alice"); a = await env.client("alice")
    await mkspace(a)
    seen = asyncio.Event()
    drop = [True]
    async def hook(receipt):
        if drop[0]: seen.set(); raise DropConnection()               # keep the receipt from ever arriving
    env.hook = hook
    e, c = a.make_entry("message", S, {}, {"text": "in flight"})
    task = asyncio.create_task(a.append(e, c))
    await asyncio.wait_for(seen.wait(), 5)
    task.cancel()                                                      # the client process is killed here
    with pytest.raises(asyncio.CancelledError): await task
    row = a.outbox.get(P.entry_id(e))
    assert row["state"] == "pending" and row["receipt"] is None        # stored before sending, never confirmed
    await a.close()
    drop[0] = False
    a2 = await env.client("alice")                                     # restart: connect() finishes the outbox (R-07)
    row = a2.outbox.get(P.entry_id(e))
    assert row["state"] == "done"
    recs = [r for r in await chain(a2) if r["entry"]["nonce"] == e["nonce"]]
    assert len(recs) == 1 and row["receipt"] == recs[0]["receipt"]
    assert (await chain(a2))[-1]["receipt"]["seq"] == 2

async def test_outbox_sent_in_order_before_new_appends(env):
    """R-07: after a restart every stored, unconfirmed entry is sent before anything new to that space."""
    env.actor("alice"); a = await env.client("alice")
    await mkspace(a)
    pend = [a.make_entry("message", S, {}, {"text": t}) for t in ("p1", "p2")]
    for e, c in pend: a.outbox.add(P.entry_id(e), S, e, c)             # stored, process died before sending
    await a.close()
    a2 = await env.client("alice")
    e3, c3 = a2.make_entry("message", S, {}, {"text": "new"})
    await a2.append(e3, c3)
    texts = [r["content"]["text"] for r in (await chain(a2))[1:]]
    assert texts == ["p1", "p2", "new"]

async def test_refusal_is_definitive(env):
    """R-06: denied/invalid end the retrying, are kept in the outbox, and a repeat raises the same error without a send."""
    env.actor("alice"); env.actor("eve"); e_ = await env.client("eve"); a = await env.client("alice")
    await mkspace(a)
    e, c = e_.make_entry("message", S, {}, {"text": "let me in"})
    with pytest.raises(WireError) as ex: await e_.append(e, c)
    assert ex.value.code == "denied"
    assert e_.outbox.get(P.entry_id(e))["state"] == "rejected"
    with pytest.raises(WireError): await e_.append(e, c)
    assert len(await chain(a)) == 1

async def test_pending_survives_server_restart(env):
    """R-06, R-07: a server restart mid-session is just an unknown outcome; the client reconnects and the entry lands exactly once."""
    env.actor("alice"); a = await env.client("alice")
    await mkspace(a)
    await env.bounce()
    e, c = a.make_entry("message", S, {}, {"text": "after bounce"})
    r = await asyncio.wait_for(a.append(e, c), 5)
    assert r["seq"] == 2
    await env.bounce()
    assert await a.append(e, c) == r                                   # R-01 across restarts: the id is in the reloaded log

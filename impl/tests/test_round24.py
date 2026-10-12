"""Round 24 (interop/DECISIONS-r24.md): H3 every map paged, H10 the call `state`, test-mode limits at call time, read by a stranger."""
import pytest
from conftest import S, mkspace, member, say
from han.core import P, W, LIMITS, WireError
from fixtures_exec import team, propose, file_plan

async def test_h10_reader_who_may_read_nothing_gets_not_found_from_read_and_state(env):
    """§3: `read` and `state` of a space the reader may not read at all are `not_found`, as for a missing space."""
    env.actor("alice"); env.actor("eve")
    a, e = await env.client("alice"), await env.client("eve")
    await mkspace(a); await say(a, "private")
    for call in (lambda: e.read_all(S, 0), lambda: e.state(S), lambda: e.state(S, at=1), lambda: e.state(S, effect="0" * 64)):
        with pytest.raises(WireError) as ex: await call()
        assert ex.value.code == "not_found"
    with pytest.raises(WireError) as ex: await e.read_all("zz:a.localhost", 0)
    assert ex.value.code == "not_found"

async def test_h3_every_map_is_paged_in_order_and_each_item_is_on_one_page(env, monkeypatch):
    """H3: with maxStateItems = 1 every paged map is cut, one item per page, the cursor holds one offset per map (decimal), and the union of the
    pages is the state."""
    monkeypatch.setitem(LIMITS, "maxStateItems", 1)
    a = await team(env)
    for i in range(2): await propose(a, file_plan(f"out/{i}.txt", str(i)), f"@exec:{env.server}")
    pages = [p async for p in a.state_pages(S)]
    whole = await a.read_state(S)
    assert len(pages) == max(len(whole[m]) for m in W.PAGED if m in whole and m != "profile.keys") and len(pages) >= 2
    for m in W.PAGED[:-1]:
        seen = [k for p in pages for k in p["state"][m]]
        assert len(seen) == len(set(seen)) == len(whole[m]) and all(len(p["state"][m]) <= 1 for p in pages)       # each item once, at most maxStateItems a page
    assert all(set(p["state"]) >= set(W.PAGED[:-1]) | {"space", "head", "home", "policy"} for p in pages)         # every map on every page, as {} when empty
    offs = [list(map(int, p["next"].split(".")[1:])) for p in pages if "next" in p]
    assert all(len(o) == len(W.PAGED) for o in offs) and offs == sorted(offs) and "next" not in pages[-1]

async def test_hello_and_state_pages_read_maxframebytes_at_call_time(env, monkeypatch):
    """G-11 test.limits: the value in LIMITS now is what hello reports and what pages are cut to; no restart."""
    a = await team(env)
    for i in range(12): await propose(a, file_plan(f"out/{i}.txt", str(i)), f"@exec:{env.server}")
    whole = await a.read_state(S)
    assert len(P.jcs({"id": 10 ** 15, "ok": {"state": whole}})) > 2048
    monkeypatch.setitem(LIMITS, "maxFrameBytes", 2048)
    b = await env.client("alice")                                            # a new connection: hello reports the lowered value
    assert b.hello_info["limits"]["maxFrameBytes"] == 2048
    pages = [p async for p in b.state_pages(S)]
    assert len(pages) > 1 and all(len(P.jcs({"id": 10 ** 15, "ok": p})) <= 2048 for p in pages)
    assert await b.read_state(S) == whole
    monkeypatch.setitem(LIMITS, "maxFrameBytes", 1310720)
    assert "next" not in await a.state(S)

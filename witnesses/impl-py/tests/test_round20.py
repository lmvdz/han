"""Round 20: read {state, at} is judged on the current state (P-02), R-16 answers Policy's verdict at once unless Policy would allow the
abandon later, and a claim pins approvers only up to the first valid key (P-01)."""
import pytest
from conftest import S, mkspace, member, say, plan_digest
from fixtures_federation import A, B, SA, pair                       # noqa: F401  (fixture)
from test_clock import _claimed, _abandon
from han.core import P, WireError
from han.entries import default_policy

# ── P-02: the state at a past seq ──
async def test_state_at_a_seq_before_one_joined_is_readable_by_a_current_member(env):
    """P-02: the right to read a seq is judged on the current state, so carol, added at seq 3, may read the state at seq 2 (before she was a
    member); a stranger may not."""
    carol = env.actor("carol"); env.actor("alice"); env.actor("eve")
    a, c, e = await env.client("alice"), await env.client("carol"), await env.client("eve")
    await mkspace(a); await say(a, "before carol")
    await member(a, carol)                                                    # seq 3
    past = (await c.state(S, at=2))["state"]
    assert past["head"] == 2 and carol not in past["roles"]
    with pytest.raises(WireError) as ex: await e.state(S, at=2)
    assert ex.value.code in ("denied", "not_found")

async def test_denied_server_reads_nothing_not_even_the_past(pair):
    """P-02: a server the policy denies now reads nothing, including the state at a seq when its members held a role."""
    alice, bob = pair.actor("alice", A), pair.actor("bob", B)
    a = await pair.client("alice", A)
    await mkspace(a); await member(a, bob); await say(a, "while b was welcome")
    seq, srv = pair.a.seq, "@:" + B
    assert seq.state(srv, SA, {"at": 2})["state"]["head"] == 2             # b's server: bob holds a role, so it reads the state at 2
    await a.append(*a.make_entry("policy.set", SA, {"policy": default_policy(servers={"deny": [B]})}))
    with pytest.raises(WireError) as ex: seq.state(srv, SA, {"at": 2})
    assert ex.value.code in ("denied", "not_found")
    with pytest.raises(WireError) as ex: seq.read(srv, SA, 0)
    assert ex.value.code in ("denied", "not_found")

# ── R-16 ──
async def test_abandon_after_success_is_denied_at_once_not_unavailable(env):
    """R-16: an owner's abandon of a claim that already succeeded is Policy's `denied` (the claim is not open) now, not `unavailable`
    with a wait that could never end well."""
    a, b, x, claim = await _claimed(env)
    await x.append(*x.make_entry("effect.outcome", S, {"claim": claim["id"], "result": "succeeded", "started": "yes"}))
    e, c = _abandon(a, claim)
    with pytest.raises(WireError) as ex: env.seq.append(e, c)
    assert ex.value.code == "denied" and "not open" in ex.value.message

async def test_abandon_of_an_open_claim_is_still_unavailable_until_due(env):
    """R-16: while the claim is open and the author an owner, Policy would allow the abandon at claim.time + A, so it waits."""
    a, b, x, claim = await _claimed(env)
    e, c = _abandon(a, claim)
    with pytest.raises(WireError) as ex: env.seq.append(e, c)
    assert ex.value.code == "unavailable" and ex.value.retry_after_ms
    e2, c2 = b.make_entry("effect.outcome", S, {"claim": claim["id"], "result": "unknown", "started": "unknown"})      # bob is no owner
    with pytest.raises(WireError) as ex: env.seq.append(e2, c2)
    assert ex.value.code == "denied"

# ── P-01: pin approvers up to the first valid key ──
async def test_unreachable_second_approver_does_not_block_a_claim(pair):
    """P-01, E3: approvals are tried in order and the claim pins only up to the first whose key is valid. The second approver's server is
    down (and its cached profile stale), yet a claim with a valid first approval succeeds and pins nobody from b."""
    alice, carol, ex_ = pair.actor("alice", A), pair.actor("carol", A), pair.actor("exec", A, "service", operator="@alice:a.localhost")
    bob = pair.actor("bob", B)
    a, c, x = await pair.client("alice", A), await pair.client("carol", A), await pair.client("exec", A)
    bt = await pair.client("bob", B, dial=A)
    await mkspace(a, policy=default_policy(approvals=[{"action": "file.write", "approvers": [alice, bob]}]))
    for who in (carol, ex_, bob): await member(a, who)
    plan = {"action": "file.write", "target": "f"}
    digest = plan_digest(plan)
    r = await c.append(*c.make_entry("effect.propose", SA, {"executor": ex_, "action": "file.write", "planDigest": digest, "repeatable": False}, {"plan": plan}))
    await a.append(*a.make_entry("effect.approve", SA, {"proposal": r["id"], "planDigest": digest}))
    await bt.append(*bt.make_entry("effect.approve", SA, {"proposal": r["id"], "planDigest": digest}))
    pair.clock.advance(301)                                                    # bob's cached profile at a is stale now
    await pair.b.srv.stop()                                                    # b is down
    claim = await x.append(*x.make_entry("effect.claim", SA, {"proposal": r["id"], "attempt": 1, "instance": x.key.key_id, "window": 30}), timeout=10)
    assert set(claim["profiles"]) == {ex_, carol, alice}                       # bob (the second candidate) was never consulted

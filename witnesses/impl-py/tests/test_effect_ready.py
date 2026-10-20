"""Readiness uses current Policy without appending or consuming an approval."""
import pytest
from conftest import S, member
from fixtures_exec import team, propose, approve, state
from han.core import WireError
from han.client import default_policy


@pytest.mark.asyncio
async def test_readiness_over_the_wire(env):
    owner = await team(env, policy=default_policy(sessions={"watch": "member", "read": "member", "steer": "owner", "modes": ["auto", "guarded"], "forceGuarded": True},
                       approvals=[{"action": "vcs.try", "approvers": ["owner"], "proposer": True}]))
    await member(owner, "@alice:a.localhost", "owner", space="profile:@exec:a.localhost")
    executor = await env.client("exec")
    plan = {"action": "vcs.try", "target": "github:o/r", "args": {}}
    proposal, _ = await propose(owner, plan, "@exec:a.localhost")
    args = {"space": S, "proposal": proposal, "instance": "k1"}
    with pytest.raises(WireError) as ex:
        await executor._request("effectReady", args)
    assert ex.value.code == "denied"
    await approve(owner, proposal, plan)
    before = await state(owner)
    assert await executor._request("effectReady", args) is None
    assert await executor._request("effectReady", args) is None
    assert await state(owner) == before
    with pytest.raises(WireError) as ex:
        await owner._request("effectReady", args)
    assert ex.value.code == "denied"
    with pytest.raises(WireError) as ex:
        await executor._request("effectReady", {**args, "extra": True})
    assert ex.value.code == "invalid"
    await member(owner, "@exec:a.localhost", "none")
    with pytest.raises(WireError) as ex:
        await executor._request("effectReady", args)
    assert ex.value.code == "not_found"

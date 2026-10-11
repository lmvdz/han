"""An effect over Matrix against a real Synapse: propose and approve as Matrix room events, a han-exec executor on the generic
binding of the same sequencer claims and reports, and every record reaches the room (T-01) and verifies (R-14)."""
import hashlib, os, pytest
from fixtures_matrix import synapse, mx, until, open_space, verify_room, LOOP     # noqa: F401  (fixtures)
from han.client import default_policy
from han.core import P
from han.exec import Executor

pytestmark = pytest.mark.asyncio(loop_scope=LOOP)

async def test_propose_approve_claim_outcome(mx):
    alice, bot = await mx.person("alice"), await mx.person("bot", "agent")
    execp = await mx.person("exec", "service", start=False)                  # a HAN actor; it talks to the generic binding, not Matrix
    pol = default_policy(approvals=[{"action": "*", "approvers": ["owner"]}])
    room, space = await open_space(mx, alice, invite=[bot], policy=pol)
    e, ct = alice.client.make_entry("member.set", space, {"actor": execp.actor, "role": "member"})
    await alice.client.append(e, ct, room=room, timeout=30)
    plan = {"action": "file.write", "target": "out/matrix.txt", "args": {"content": "hello over matrix"}}
    digest = hashlib.sha256(P.jcs(plan)).hexdigest()
    e, ct = bot.client.make_entry("effect.propose", space, {"executor": execp.actor, "action": "file.write", "planDigest": digest, "repeatable": False}, {"plan": plan})
    pid = P.entry_id(e)
    await bot.client.append(e, ct, room=room, timeout=30)                       # the agent proposes
    sandbox = os.path.join(mx.tmp, "sandbox")
    ex = Executor(mx.ws.url, execp.actor, execp.key_path, os.path.join(mx.tmp, "exec.journal.db"), [space], sandbox, server=mx.server,
                  retry_delay=0.05, max_retry_delay=0.2, backoff_base=0.05, backoff_cap=0.2)
    await ex.start()
    try:
        phase = lambda: mx.seq.home.spaces[space]["state"]["effects"][pid]["phase"]
        assert phase() != "ready"                                              # P-08: no approval yet, nothing to claim
        e, ct = alice.client.make_entry("effect.approve", space, {"proposal": pid, "planDigest": digest})
        await alice.client.append(e, ct, room=room, timeout=30)                 # the owner approves
        await until(lambda: phase() == "succeeded", 20, "effect succeeded")
    finally: await ex.stop()
    with open(os.path.join(sandbox, "out", "matrix.txt"), encoding="utf-8") as fh: assert fh.read() == "hello over matrix"
    # the executor's records came in over the generic binding; the service published them to the room, entry and receipt
    await until(lambda: len(alice.client.records(room)) == len(mx.seq.home.spaces[space]["records"]), what="alice's room view is complete")
    kinds = [(r["entry"]["kind"], r["entry"]["author"]) for r in alice.client.records(room)]
    assert kinds[-4:] == [("effect.propose", bot.actor), ("effect.approve", alice.actor), ("effect.claim", execp.actor), ("effect.outcome", execp.actor)]
    verify_room(mx, alice.client, room)
    verify_room(mx, bot.client, room)

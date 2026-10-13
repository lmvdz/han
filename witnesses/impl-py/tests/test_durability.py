"""R-03 / R-07, durability `local`: han-seq killed hard (TerminateProcess / SIGKILL) in the middle of a burst of appends."""
import asyncio, subprocess, sys
from conftest import S, mkspace, say, chain
from test_cli import ROOT, free_port, wait_listening, run_server
from han import keys
from han.client import Client
from han.core import W

async def test_hard_kill_during_a_burst_loses_no_acknowledged_receipt(tmp_path):
    """R-03, R-07: every receipt a client was given before the process was killed is in the log after the restart, byte for byte; the
    chain verifies; no seq is used twice; and the entries the client was still sending arrive once each (dedupe), on seqs after those."""
    kp = keys.generate(str(tmp_path / "alice.json"))
    reg = subprocess.run([sys.executable, "-m", "han.seq", "register", "--server", "a.localhost", "--db", str(tmp_path / "seq.db"),
                          "--key", str(tmp_path / "server.json"), "--actor", "@alice:a.localhost", "--pub", str(tmp_path / "alice.json")],
                         cwd=ROOT, capture_output=True, text=True)
    assert reg.returncode == 0, reg.stderr
    port = free_port()
    proc = run_server(tmp_path, port)
    try:
        await wait_listening(port, proc)
        c = Client(f"ws://127.0.0.1:{port}", "@alice:a.localhost", str(tmp_path / "alice.json"), str(tmp_path / "ob.db"),
                   retry_delay=0.05, max_retry_delay=0.3)
        await c.connect()
        await mkspace(c)
        acked: dict[int, dict] = {}                                  # seq → the receipt the client was given
        async def send(i):
            r = await say(c, f"m{i}"); acked[r["seq"]] = r
        sent = 0
        for round_ in range(2):                                      # kill twice, each time inside a burst
            before = len(acked)
            burst = [asyncio.create_task(send(sent + i)) for i in range(80)]; sent += 80
            while len(acked) < before + 15: await asyncio.sleep(0.005)
            proc.kill(); proc.wait()                                 # hard kill with appends in flight
            killed_with = dict(acked)
            assert any(not t.done() for t in burst)                  # the burst really was interrupted
            proc = run_server(tmp_path, port)
            await wait_listening(port, proc)
            await asyncio.wait_for(asyncio.gather(*burst), 60)       # the outbox retries the same entries
            recs = await chain(c)
            by_seq = {r["receipt"]["seq"]: r for r in recs}
            for seq, receipt in killed_with.items():                 # nothing acknowledged was lost
                assert by_seq[seq]["receipt"] == receipt
        recs = await chain(c)
        assert [r["receipt"]["seq"] for r in recs] == list(range(1, len(recs) + 1))        # no gap, no reuse
        assert len({r["receipt"]["id"] for r in recs}) == len(recs) == 1 + 160                # space.create + the messages
        assert sorted(acked) == [r["receipt"]["seq"] for r in recs][1:] and all(acked[s]["id"] == by_seq[s]["receipt"]["id"] for s in acked)
        server_keys = {"a.localhost": {"h1": {"publicKey": keys.load(str(tmp_path / "server.json")).pub, "from": "1970-01-01T00:00:00Z"}}}
        view = {"actor": "@alice:a.localhost", "seq": 1, "actorKind": "human", "operatorConfirmed": False,
                "keys": {"k1": {"publicKey": kp.pub, "status": "valid"}}}
        m = W.Mirror(server_keys, lambda a, n: view)
        assert all(ok for ok, _ in (m.ingest(x) for x in recs))
        await c.close()
    finally:
        proc.kill(); proc.wait()

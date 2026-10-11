"""The shipped entry point: `python -m han.seq` as a real subprocess, a client over TCP, and a restart that continues the chain (R-03, R-07)."""
import asyncio, os, socket, subprocess, sys
from conftest import S, mkspace, say, mirror, chain
from han import keys
from han.client import Client
from han.core import W

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0)); return s.getsockname()[1]

async def wait_listening(port, proc):
    for _ in range(100):
        if proc.poll() is not None: raise RuntimeError("han-seq exited: " + proc.stderr.read().decode())
        try:
            r, w = await asyncio.open_connection("127.0.0.1", port); w.close(); return
        except OSError: await asyncio.sleep(0.1)
    raise TimeoutError

def run_server(tmp, port, *extra):
    return subprocess.Popen([sys.executable, "-m", "han.seq", "--server", "a.localhost", "--port", str(port), "--db", str(tmp / "seq.db"),
                             "--key", str(tmp / "server.json"), *extra], cwd=ROOT, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)

async def test_han_seq_process_and_restart(tmp_path):
    """R-03, R-07: han-seq as a process: register an actor, append over TCP, kill and restart it, and the chain continues and verifies."""
    kp = keys.generate(str(tmp_path / "alice.json"))
    reg = subprocess.run([sys.executable, "-m", "han.seq", "register", "--server", "a.localhost", "--db", str(tmp_path / "seq.db"),
                          "--key", str(tmp_path / "server.json"), "--actor", "@alice:a.localhost", "--pub", str(tmp_path / "alice.json")],
                         cwd=ROOT, capture_output=True, text=True)
    assert reg.returncode == 0, reg.stderr
    port = free_port()
    proc = run_server(tmp_path, port)
    try:
        await wait_listening(port, proc)
        c = Client(f"ws://127.0.0.1:{port}", "@alice:a.localhost", str(tmp_path / "alice.json"), str(tmp_path / "ob.db"))
        await c.connect()
        await mkspace(c); await say(c, "one")
        proc.kill(); proc.wait()                                       # crash, not a clean stop
        proc = run_server(tmp_path, port)
        await wait_listening(port, proc)
        r = await asyncio.wait_for(say(c, "two"), 10)                  # the client reconnects on its own
        assert r["seq"] == 3
        recs = await chain(c)
        server_keys = {"a.localhost": {"h1": {"publicKey": keys.load(str(tmp_path / "server.json")).pub, "from": "1970-01-01T00:00:00Z"}}}
        from han.core import W
        # the alice profile (seq 1) is what the receipts pin; rebuild the view from the key file
        view = {"actor": "@alice:a.localhost", "seq": 1, "actorKind": "human", "operatorConfirmed": False,
                "keys": {"k1": {"publicKey": kp.pub, "status": "valid"}}}
        m = W.Mirror(server_keys, lambda a, n: view)
        assert all(ok for ok, _ in (m.ingest(x) for x in recs))
        await c.close()
    finally:
        proc.kill(); proc.wait()

"""Thin adapter: the Python han-seq with a frozen clock, for golden transcripts.
   python py_host.py --server a.localhost --port P --db PATH --key KEYFILE --actors actors.json --freeze 2026-01-01T00:00:00Z
(The real, unmodified `python -m han.seq` is used for every other run; this only replaces the Clock.)"""
import argparse, asyncio, calendar, json, os, sys, time
IMPL = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "impl"))
sys.path.insert(0, IMPL)
from han import keys
from han.seq.sequencer import Sequencer
from han.seq.server import SeqServer

ap = argparse.ArgumentParser()
ap.add_argument("--server", required=True); ap.add_argument("--port", type=int, required=True)
ap.add_argument("--db", required=True); ap.add_argument("--key", required=True)
ap.add_argument("--actors", required=True); ap.add_argument("--freeze")
a = ap.parse_args()

class Frozen:
    def __init__(self, t): self.t = t
    def wall(self): return self.t
    def mono(self): return 0.0

clock = Frozen(calendar.timegm(time.strptime(a.freeze, "%Y-%m-%dT%H:%M:%SZ"))) if a.freeze else None
seq = Sequencer(a.server, keys.load_or_generate(a.key, "h1"), a.db, clock=clock)
for x in json.load(open(a.actors)):
    seq.register(x["actor"], x["kind"], x["keyId"], x["public"], x.get("operator"))
async def run():
    srv = await SeqServer(seq, "127.0.0.1", a.port).start()
    print(f"READY py {a.server} {srv.url}", flush=True)
    await asyncio.Event().wait()
try: asyncio.run(run())
except KeyboardInterrupt: pass
finally: seq.close()

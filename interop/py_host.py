"""Thin adapter: the Python han-seq with a test clock, for golden transcripts and the wire vectors.
   python py_host.py --server a.localhost --port P --db PATH --key KEYFILE --actors actors.json --freeze 2026-01-01T00:00:00Z
(The real, unmodified `python -m han.seq` is used for every other run; this only replaces the Clock.)

Test mode (architecture.md G-11): the clock starts at --freeze and moves only by the control call `test.clock {wall?, steadyAdvance?}`, which this host adds
to the server in process and the stock server does not have (outside test mode an unknown call is `invalid`)."""
import argparse, asyncio, calendar, json, os, sys, time
IMPL = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "impl"))
sys.path.insert(0, IMPL)
from han import keys
from han.core import WireError
from han.seq import server as srv_mod
from han.seq.sequencer import Sequencer
from han.seq.server import SeqServer

ap = argparse.ArgumentParser()
ap.add_argument("--server", required=True); ap.add_argument("--port", type=int, required=True)
ap.add_argument("--db", required=True); ap.add_argument("--key", required=True)
ap.add_argument("--actors", required=True); ap.add_argument("--freeze")
a = ap.parse_args()

class Frozen:
    """Two readings the test controls: `t` (wall, epoch seconds) and `m` (steady, seconds). Neither moves by itself."""
    def __init__(self, t): self.t, self.m = t, 0.0
    def wall(self): return self.t
    def mono(self): return self.m

def fmt(sec): return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(sec))
def parse_time(s):
    """A HAN Time (2026-01-01T00:00:00Z, a real date) as epoch seconds, or None."""
    try: t = calendar.timegm(time.strptime(s, "%Y-%m-%dT%H:%M:%SZ"))
    except (ValueError, TypeError, OverflowError): return None
    return t if fmt(t) == s else None

clock = Frozen(calendar.timegm(time.strptime(a.freeze, "%Y-%m-%dT%H:%M:%SZ"))) if a.freeze else None
seq = Sequencer(a.server, keys.load_or_generate(a.key, "h1"), a.db, clock=clock)
if clock: seq.now = lambda: clock.t            # receipt and hello times are the test's wall reading, not the startup anchor plus elapsed steady time
srv_mod.CALL_ARGS["test.clock"] = {"wall", "steadyAdvance"}
for x in json.load(open(a.actors)):
    seq.register(x["actor"], x["kind"], x["keyId"], x["public"], x.get("operator"))

class TestServer(SeqServer):
    async def _call(self, ws, conn, send, rid, call, args, challenge):
        if call != "test.clock" or not clock: return await super()._call(ws, conn, send, rid, call, args, challenge)
        if not conn["who"]: raise WireError("denied", "hello first")
        if set(args) - {"wall", "steadyAdvance"}: raise WireError("invalid", "unknown args for test.clock")
        wall, adv = args.get("wall"), args.get("steadyAdvance", 0)
        t = parse_time(wall) if "wall" in args else None
        if "wall" in args and t is None: raise WireError("invalid", "wall is not a Time")
        if isinstance(adv, bool) or not isinstance(adv, int) or adv < 0: raise WireError("invalid", "steadyAdvance is a non-negative integer (seconds)")
        if t is not None: clock.t = t
        clock.m += adv
        return await send({"id": rid, "ok": {"wall": fmt(clock.t), "steady": int(clock.m)}})

async def run():
    srv = await TestServer(seq, "127.0.0.1", a.port).start()
    print(f"READY py {a.server} {srv.url}", flush=True)
    await asyncio.Event().wait()
try: asyncio.run(run())
except KeyboardInterrupt: pass
finally: seq.close()

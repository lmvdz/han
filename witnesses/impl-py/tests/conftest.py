"""Test harness: a real han-seq (the Sequencer behind a SeqServer on a free port, SQLite on disk) and real clients over WebSocket."""
import asyncio, hashlib, os, sys
import pytest_asyncio

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
from han import keys                                   # noqa: E402
from han.client import Client, default_policy          # noqa: E402
from han.core import P, W                              # noqa: E402
from han.seq import Sequencer, SeqServer               # noqa: E402

T0 = 1_790_000_000.0          # 2026-09-21T..Z

class FakeClock:
    """Injectable clock: `wall` and `mono` move together with advance(); step_wall() moves only the wall clock."""
    def __init__(self, wall=T0): self._wall, self._mono = wall, 1000.0
    def wall(self): return self._wall
    def mono(self): return self._mono
    def advance(self, dt): self._wall += dt; self._mono += dt
    def step_wall(self, dt): self._wall += dt

class Env:
    def __init__(self, tmp, clock, server="a.localhost"):
        self.tmp, self.clock, self.server = str(tmp), clock, server
        self.key_path = os.path.join(self.tmp, "server-key.json")
        self.db = os.path.join(self.tmp, "seq.db")
        self.seq = self.srv = None
        self.clients: list[Client] = []
        self.hook = None
        self.nonce_n = 0

    async def start(self, clock=None, port=0):
        self.clock = clock or self.clock
        self.seq = Sequencer(self.server, keys.load_or_generate(self.key_path, "h1"), self.db, self.clock)
        self.srv = await SeqServer(self.seq, "127.0.0.1", port, after_append=lambda r: self.hook(r) if self.hook else asyncio.sleep(0)).start()
        return self

    async def stop(self):
        for c in self.clients:
            try: await c.close()
            except Exception: pass
        self.clients.clear()
        await self.srv.stop(); self.seq.close()

    async def restart(self, clock=None):
        """Stop the server (clients are closed too) and start a new one on the same database."""
        await self.stop()
        return await self.start(clock)

    async def bounce(self, clock=None):
        """Restart only the server, on the same port and database; clients stay and reconnect by themselves."""
        port = self.srv.port
        await self.srv.stop(); self.seq.close()
        return await self.start(clock, port)

    def actor(self, name, kind="human", operator=None, server=None) -> str:
        """Create a key file for `name` and register its profile on the server. Returns the ActorId."""
        actor = f"@{name}:{server or self.server}"
        path = os.path.join(self.tmp, f"{name}.key.json")
        kp = keys.load_or_generate(path, "k1")
        self.seq.register(actor, kind, kp.key_id, kp.pub, operator)
        return actor

    async def client(self, name, fresh_outbox=False, **kw) -> Client:
        """A connected client for `name` (outbox kept per name, so a second client with the same name resumes it)."""
        c = Client(self.srv.url, f"@{name}:{self.server}", os.path.join(self.tmp, f"{name}.key.json"),
                   os.path.join(self.tmp, f"{name}.outbox.db"), server=self.server, retry_delay=0.05, max_retry_delay=0.2, **kw)
        self.clients.append(c)
        await c.connect()
        return c

@pytest_asyncio.fixture
async def env(tmp_path):
    e = await Env(tmp_path, FakeClock()).start()
    yield e
    await e.stop()

S = "s1:a.localhost"

async def mkspace(c, space=S, policy=None, name="room"):
    e, ct = c.make_entry("space.create", space, {"policy": policy or default_policy()}, {"name": name})
    return await c.append(e, ct)

async def member(c, actor, role="member", space=S):
    e, ct = c.make_entry("member.set", space, {"actor": actor, "role": role})
    return await c.append(e, ct)

async def say(c, text, space=S, **body):
    e, ct = c.make_entry("message", space, body, {"text": text})
    return await c.append(e, ct)

def mirror(env):
    return W.Mirror(env.seq.server_keys, lambda a, n: env.seq.profile(a, n))

async def chain(c, space=S):
    """All records of a space as read over the network."""
    return [i["record"] for i in await c.read_all(space, 0) if i["type"] == "record"]

def plan_digest(plan): return hashlib.sha256(P.jcs(plan)).hexdigest()

class Collector:
    """Collects the items of an async iterator in the background."""
    def __init__(self, agen):
        self.items = []
        self.task = asyncio.create_task(self._run(agen))
    async def _run(self, agen):
        try:
            async for it in agen: self.items.append(it)
        except Exception as ex:
            self.error = ex
    async def wait_for(self, pred, timeout=3.0):
        async def poll():
            while not any(pred(i) for i in self.items): await asyncio.sleep(0.01)
        await asyncio.wait_for(poll(), timeout)
    def stop(self): self.task.cancel()

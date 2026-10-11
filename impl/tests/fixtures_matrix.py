"""Fixtures for the Matrix binding tests: one real Synapse in Podman for the whole session (skipped, with the reason, if Podman
is not reachable), one application-service sequencer in front of it, and Matrix users that are also HAN actors."""
import asyncio, os, sys, uuid
import pytest, pytest_asyncio

IMPL = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
sys.path.insert(0, os.path.join(IMPL, "matrix-dev"))
import synapse_dev                                                    # noqa: E402
from han import keys                                                  # noqa: E402
from han.client import default_policy                                 # noqa: E402
from han.core import P, W                                             # noqa: E402
from han.matrix.api import register_shared_secret                     # noqa: E402
from han.matrix.appservice import AppService                          # noqa: E402
from han.matrix.client import MatrixClient                            # noqa: E402
from han.seq import Sequencer, SeqServer                              # noqa: E402

LOOP = "session"                                                      # one event loop for Synapse-facing fixtures and tests
pytestmark_session = pytest.mark.asyncio(loop_scope=LOOP)

@pytest.fixture(scope="session")
def synapse(tmp_path_factory):
    """A real Synapse in Podman, one per pytest session, on free ports and under a unique container name (parallel runs coexist).
    HAN_SKIP_MATRIX=1 skips the Matrix tests."""
    if os.environ.get("HAN_SKIP_MATRIX"): pytest.skip("HAN_SKIP_MATRIX is set")
    why = synapse_dev.unavailable()
    if why: pytest.skip(f"Matrix binding tests need Podman and the Synapse image: {why}")
    name = f"han-synapse-t{os.getpid()}-{uuid.uuid4().hex[:6]}"
    try:
        s = synapse_dev.up(port=0, as_url=f"http://127.0.0.1:{synapse_dev.free_port()}", state=str(tmp_path_factory.mktemp("synapse")), name=name)
    except Exception:
        synapse_dev.down(name); raise
    yield s
    synapse_dev.down(name)                                            # the container and its volume are removed at the end of the session

class Person:
    def __init__(self, actor, token, key_path, client): self.actor, self.token, self.key_path, self.client = actor, token, key_path, client

class MxEnv:
    def __init__(self, synapse, tmp):
        self.syn, self.tmp, self.server = synapse, str(tmp), synapse.server_name
        self.as_port = int(synapse.as_url.rsplit(":", 1)[1])
        self.seq = self.svc = self.ws = None
        self.people: list[Person] = []
        self.clients: list[MatrixClient] = []

    async def start(self):
        self.seq = Sequencer(self.server, keys.load_or_generate(os.path.join(self.tmp, "server-key.json"), "h1"), os.path.join(self.tmp, "seq.db"))
        self.svc = await AppService(self.seq, self.syn.base_url, self.syn.as_token, self.syn.hs_token, os.path.join(self.tmp, "as.db")).start("127.0.0.1", self.as_port)
        self.ws = await SeqServer(self.seq, "127.0.0.1", 0).start()      # the generic binding on the same sequencer
        return self

    async def stop(self):
        for c in self.clients:
            try: await c.close()
            except Exception: pass
        await self.ws.stop(); await self.svc.stop(); self.seq.close()

    async def person(self, name, kind="human", start=True) -> Person:
        """A Matrix user that is also a HAN actor of the same id, with a key, a profile and (if `start`) a started client."""
        local = f"{name}-{uuid.uuid4().hex[:6]}"
        actor = f"@{local}:{self.server}"
        user, token = await register_shared_secret(self.syn.base_url, self.syn.shared_secret, local)
        assert user == actor
        path = os.path.join(self.tmp, f"{local}.key.json")
        kp = keys.load_or_generate(path, "k1")
        self.seq.register(actor, kind, kp.key_id, kp.pub)
        c = MatrixClient(self.syn.base_url, actor, token, path, os.path.join(self.tmp, f"{local}.outbox.db"), service_url=f"http://127.0.0.1:{self.as_port}")
        self.clients.append(c)
        if start: await c.start()
        p = Person(actor, token, path, c); self.people.append(p); return p

    def new_space(self) -> str: return f"t{uuid.uuid4().hex[:12]}:{self.server}"

    def mirror(self): return W.Mirror(self.seq.server_keys, lambda a, n: self.seq.profile(a, n))

@pytest_asyncio.fixture(scope=LOOP, loop_scope=LOOP)
async def mx(synapse, tmp_path_factory):
    """Per test: a fresh sequencer and appservice (users and spaces are unique, so Synapse state is shared)."""
    e = await MxEnv(synapse, tmp_path_factory.mktemp("mx")).start()
    yield e
    await e.stop()

async def until(pred, timeout=15.0, what=""):
    async def poll():
        while True:
            r = pred()
            if asyncio.iscoroutine(r): r = await r
            if r: return r
            await asyncio.sleep(0.05)
    try: return await asyncio.wait_for(poll(), timeout)
    except asyncio.TimeoutError: raise AssertionError(f"timed out waiting for {what or pred}") from None

async def open_space(mx, owner, space=None, invite=(), policy=None, name="team"):
    """`owner` creates a room for the service and `invite`, and opens a space in it. Returns (room, space)."""
    space = space or mx.new_space()
    room = await owner.client.create_space_room(name, invite=[p.actor for p in invite])
    e, ct = owner.client.make_entry("space.create", space, {"policy": policy or default_policy()}, {"name": name})
    await owner.client.append(e, ct, room=room, timeout=30)
    for p in invite:
        await p.client.join(room)
        e, ct = owner.client.make_entry("member.set", space, {"actor": p.actor, "role": "member"})
        await owner.client.append(e, ct, room=room, timeout=30)
    return room, space

async def say(p, room, space, text):
    e, ct = p.client.make_entry("message", space, {}, {"text": text})
    return await p.client.append(e, ct, room=room, timeout=30)

async def verify_room(mx, client, room):
    """Feed every record the client sees in `room` to a Mirror (R-14); the room has headers only (P-02), so the content is
    fetched through the HTTP read first. Returns the Mirror."""
    await client.fill_content(room)
    m = mx.mirror()
    for rec in client.records(room):
        ok, why = m.ingest(rec)
        assert ok, why
    return m

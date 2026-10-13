"""Helpers for the federation tests: two real han-seq servers (a.localhost, b.localhost) on different ports, one clock, each with a
Federation that knows the other through its han-peers map; actors registered at their own server and clients that dial either."""
import asyncio, os
import pytest_asyncio
from conftest import Env, FakeClock
from han import keys
from han.client import Client
from han.seq.federation import Federation

A, B = "a.localhost", "b.localhost"
SA = "s1:a.localhost"          # a space homed on a

class FedEnv(Env):
    async def start(self, clock=None, port=0, peers=None):
        await super().start(clock, port)
        self.fed = Federation(self.seq, peers if peers is not None else {})
        return self

    async def stop(self):
        await self.fed.close()
        await super().stop()

class Pair:
    """`a` and `b` share one FakeClock (advance it to age profile caches). `http(server)` is the base URL of its .well-known."""
    def __init__(self, tmp_path):
        self.clock = FakeClock()
        (tmp_path / "a").mkdir(); (tmp_path / "b").mkdir()
        self.a, self.b = FedEnv(tmp_path / "a", self.clock, A), FedEnv(tmp_path / "b", self.clock, B)
        self.peers = {}                          # the shared han-peers map: name → http base URL
        self.clients: list[Client] = []

    async def start(self):
        await self.a.start(peers=self.peers); await self.b.start(peers=self.peers)
        self.peers[A], self.peers[B] = f"http://127.0.0.1:{self.a.srv.port}", f"http://127.0.0.1:{self.b.srv.port}"
        return self

    async def stop(self):
        for c in self.clients:
            try: await c.close()
            except Exception: pass
        await self.a.stop(); await self.b.stop()

    def env(self, server): return self.a if server == A else self.b

    def actor(self, name, server, kind="human", operator=None) -> str:
        return self.env(server).actor(name, kind, operator)

    async def client(self, name, home_of, dial=None) -> Client:
        """A connected client for `name` (registered on `home_of`'s server) dialling server `dial` (default: its own)."""
        env, to = self.env(home_of), self.env(dial or home_of)
        c = Client(to.srv.url, f"@{name}:{home_of}", os.path.join(env.tmp, f"{name}.key.json"),
                   os.path.join(env.tmp, f"{name}.{dial or home_of}.outbox.db"), server=dial or home_of, retry_delay=0.05, max_retry_delay=0.2)
        self.clients.append(c)
        await c.connect()
        return c

async def until(pred, timeout=5.0):
    async def poll():
        while not pred(): await asyncio.sleep(0.02)
    await asyncio.wait_for(poll(), timeout)

@pytest_asyncio.fixture
async def pair(tmp_path):
    p = await Pair(tmp_path).start()
    yield p
    await p.stop()

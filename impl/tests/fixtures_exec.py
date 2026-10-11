"""Helpers for the executor tests: a space with an owner and an executor actor, proposals, polling, a counting HTTP server,
simulated crashes, and subprocess runs of `python -m han.exec`."""
import asyncio, os, sys
import pytest_asyncio
from conftest import S, mkspace, member, chain, plan_digest
from han.client import default_policy
from han.core import P
from han.exec import Executor, Crash

ALICE = "@alice:a.localhost"
EXEC_ID = "@exec:a.localhost"
IMPL = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
APPROVE_ALL = [{"action": "*", "approvers": ["owner"]}]

@pytest_asyncio.fixture
async def execs():
    """Executors started by a test; all are stopped at the end."""
    made: list[Executor] = []
    yield made
    for x in made:
        try: await x.stop()
        except Exception: pass

def key_path(env, name): return os.path.join(env.tmp, f"{name}.key.json")
def sandbox(env): return os.path.join(env.tmp, "sandbox")

def make_exec(env, execs, name="exec", spaces=(S,), journal=None, start=False, **kw) -> Executor:
    kw.setdefault("backoff_base", 0.05); kw.setdefault("backoff_cap", 0.2)
    ex = Executor(env.srv.url, f"@{name}:{env.server}", key_path(env, name), journal or os.path.join(env.tmp, f"{name}.journal.db"),
                  list(spaces), sandbox(env), server=env.server, retry_delay=0.05, max_retry_delay=0.2, **kw)
    execs.append(ex)
    return ex

async def team(env, policy=None, **policy_over):
    """alice (owner, human) and an executor actor `exec` (service, member) in S. Returns alice's client."""
    env.actor("alice"); env.actor("exec", "service")
    a = await env.client("alice")
    await mkspace(a, policy=policy or default_policy(approvals=APPROVE_ALL, **policy_over))
    await member(a, f"@exec:{env.server}")
    return a

def file_plan(target="out/a.txt", content="hello"):
    return {"action": "file.write", "target": target, "args": {"content": content}}

async def propose(c, plan, executor, repeatable=False, space=S, **body):
    """effect.propose; returns (proposal id, receipt)."""
    e, ct = c.make_entry("effect.propose", space, {"executor": executor, "action": plan["action"], "planDigest": plan_digest(plan),
                                                    "repeatable": repeatable, **body}, {"plan": plan})
    r = await c.append(e, ct)
    return P.entry_id(e), r

async def approve(c, pid, plan, space=S):
    return await c.append(*c.make_entry("effect.approve", space, {"proposal": pid, "planDigest": plan_digest(plan)}))

async def state(c, space=S):
    return (await c.read_all(space, 2 ** 31, state=True))[0]["state"]

async def until(pred, timeout=8.0, what=""):
    """Poll an async or sync predicate."""
    async def poll():
        while True:
            r = pred()
            if asyncio.iscoroutine(r): r = await r
            if r: return r
            await asyncio.sleep(0.03)
    try: return await asyncio.wait_for(poll(), timeout)
    except asyncio.TimeoutError: raise AssertionError(f"timed out waiting for {what or pred}") from None

def phase_is(c, pid, phase):
    async def f(): return (await state(c))["effects"][pid]["phase"] == phase
    return f

async def kinds(c, space=S):
    """[(kind, body)] of the whole chain."""
    return [(r["entry"]["kind"], r["entry"]["body"]) for r in await chain(c, space)]

async def records_of(c, kind, author=None, space=S):
    """Records of one kind (optionally by one author) in a space."""
    return [r for r in await chain(c, space) if r["entry"]["kind"] == kind and (author is None or r["entry"]["author"] == author)]

def crash_at(point):
    """A hook that simulates the process dying (once) at the named journal point."""
    done = []
    def hook(p, **_):
        if p == point and not done: done.append(1); raise Crash()
    return hook

async def crash(ex):
    """Wait for the simulated crash, then free the dead process's resources."""
    await asyncio.wait_for(ex.crashed.wait(), 8)
    await ex.stop()

class CountingHttp:
    """A tiny asyncio HTTP server that counts POSTs and answers 200."""
    def __init__(self): self.hits, self.bodies, self.server = 0, [], None
    async def start(self):
        self.server = await asyncio.start_server(self._handle, "127.0.0.1", 0)
        self.url = f"http://127.0.0.1:{self.server.sockets[0].getsockname()[1]}/hook"
        return self
    async def _handle(self, reader, writer):
        head = await reader.readuntil(b"\r\n\r\n")
        n = int([l.split(b":")[1] for l in head.split(b"\r\n") if l.lower().startswith(b"content-length")][0])
        self.bodies.append((await reader.readexactly(n)).decode()); self.hits += 1
        writer.write(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"); await writer.drain(); writer.close()
    async def stop(self):
        self.server.close(); await self.server.wait_closed()

async def run_cli(env, *extra, name="exec", journal=None, spaces=S):
    """Start `python -m han.exec` as a subprocess; returns the process (stdout/stderr piped)."""
    args = [sys.executable, "-m", "han.exec", "--url", env.srv.url, "--actor", f"@{name}:{env.server}", "--key", key_path(env, name),
            "--server", env.server, "--journal", journal or os.path.join(env.tmp, f"{name}.journal.db"), "--spaces", spaces,
            "--sandbox", sandbox(env), *extra]
    return await asyncio.create_subprocess_exec(*args, cwd=IMPL, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)

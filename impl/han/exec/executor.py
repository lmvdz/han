"""The executor (architecture.md §4.5, M5): an Actor fed by `subscribe`. It claims `ready` effects that name it, launches the
plan under X-01…X-10 and reports the outcome. Everything it must remember across a crash is in the journal (journal.py).

Per claim the journal holds, in order: `claiming` (X-02: signed claim + elapsed-clock reading, before the first send) →
`launching` (X-04) → [`expired`] → `ran` (the action's own result) → `reporting` (the signed outcome entry) → `reported`.
Other terminal events: `void` (the claim was refused), `refused` (the outcome was refused) then `resolved`/`unresolved` (X-10)."""
from __future__ import annotations
import asyncio, hashlib, json, logging, os
from .. import keys
from ..client import Client
from ..core import P, WireError
from .actions import Actions, Refused
from .journal import Journal, StartRefused, elapsed_clock

STALL_BOUND = 10          # seconds (§4.5)
log = logging.getLogger("han.exec")

class Crash(BaseException):
    """Raised by a test hook to simulate the process dying at a named point; nothing after it runs."""

class Executor:
    def __init__(self, url: str, actor: str, key_path: str, journal: str, spaces, sandbox: str, actions=("file.write", "http.post"),
                 allow_urls=(), instance: str | None = None, server: str | None = None, clock=None, hook=None,
                 retry_delay: float = 0.2, max_retry_delay: float = 2.0):
        """`clock` is the elapsed-time clock (X-02); `hook(point, **info)` may be sync or async and may raise Crash (tests)."""
        self.url, self.actor, self.key_path, self.journal_path = url, actor, key_path, journal
        self.spaces, self.instance, self.server = list(spaces), instance, server
        self.actions = Actions(sandbox, allow_urls, actions)
        self.clock, self.hook = clock or elapsed_clock(), hook
        self.retry = (retry_delay, max_retry_delay)
        self.client = self.journal = None
        self.launched: list[str] = []                  # claim ids this process launched
        self.crashed = asyncio.Event()
        self._tasks: list[asyncio.Task] = []
        self._jobs: dict[str, asyncio.Task] = {}       # proposal id → running claim job
        self._wake: dict[str, asyncio.Event] = {}
        self._denied: dict[str, int] = {}              # proposal id → head at which our claim was refused
        self._skip: set[str] = set()                   # proposals this executor will not run (bad plan)
        self._plans: dict[str, dict] = {}

    # ── lifecycle ──
    async def start(self):
        kp = keys.load(self.key_path)
        if self.instance and self.instance != kp.key_id: raise StartRefused(f"--instance {self.instance} is not the key's id {kp.key_id}")
        self.instance = kp.key_id
        self.journal = Journal(self.journal_path, kp.key_id, self.key_path + ".used")       # X-07: lock, marker (may refuse)
        try:
            self.client = Client(self.url, self.actor, self.key_path, self.journal_path + ".outbox.db", server=self.server,
                                 retry_delay=self.retry[0], max_retry_delay=self.retry[1])
            await self.client.connect()
        except BaseException:
            await self.stop(); raise
        self._tasks.append(asyncio.create_task(self._guard(self._boot)))
        return self

    async def stop(self):
        for t in self._tasks + list(self._jobs.values()): t.cancel()
        await asyncio.gather(*self._tasks, *self._jobs.values(), return_exceptions=True)
        self._tasks.clear(); self._jobs.clear()
        if self.client: await self.client.close(); self.client = None
        if self.journal: self.journal.close(); self.journal = None

    async def run(self):
        await self.start()
        try: await asyncio.Event().wait()
        finally: await self.stop()

    async def _guard(self, fn, *args):
        try: await fn(*args)
        except Crash:
            self.crashed.set()
            for t in list(self._tasks) + list(self._jobs.values()):
                if t is not asyncio.current_task(): t.cancel()
        except asyncio.CancelledError: raise
        except Exception: log.exception("executor task failed")

    async def _at(self, point: str, **info):
        if self.hook:
            r = self.hook(point, **info)
            if asyncio.iscoroutine(r): await r

    async def _boot(self):
        await self._recover()                            # X-06 before anything new
        for sp in self.spaces:
            self._wake[sp] = asyncio.Event()
            self._tasks.append(asyncio.create_task(self._guard(self._space_loop, sp)))

    # ── X-06: restart ──
    async def _recover(self):
        """For every claim without a terminal event: finish what the journal proves, never launch."""
        for cid, evs in self.journal.claims().items():
            names = [e for e, _ in evs]
            data = dict(evs)                                          # last data per event name
            info = data["claiming"]
            if any(n in names for n in ("reported", "void", "resolved", "unresolved")): continue
            if "refused" in names: await self._x10(cid); continue
            if "reporting" in names:                                  # the outcome was signed: send the same entry again (R-06)
                await self._send_outcome(cid, data["reporting"]["entry"], data["reporting"]["content"]); continue
            if "ran" in names: await self._report(cid, data["ran"]["result"], "yes"); continue
            if "expired" in names: await self._report(cid, "failed", "no"); continue
            if "launching" in names: await self._report(cid, "unknown", "unknown"); continue   # nothing proves what happened
            try: await self.client.append(info["entry"], info["content"])    # `claiming` only: finish the append (R-06)
            except WireError as ex:
                self.journal.add(cid, "void", {"error": ex.to_wire()}); continue
            await self._report(cid, "failed", "no")

    # ── feeding: subscribe → reconcile ──
    async def _space_loop(self, space):
        delay = self.retry[0]
        while True:
            try: head = await self._reconcile(space); break
            except WireError as ex: log.warning("%s: %s", space, ex); await asyncio.sleep(delay)
        ev = self._wake[space]
        self._tasks.append(asyncio.create_task(self._guard(self._follow, space, head, ev)))
        while True:
            await ev.wait(); ev.clear()
            try: await self._reconcile(space)
            except WireError as ex: log.warning("%s: %s", space, ex); await asyncio.sleep(delay); ev.set()

    async def _follow(self, space, head, ev):
        async for it in self.client.subscribe(space, after=head):
            if it["type"] == "record": ev.set()

    async def _reconcile(self, space) -> int:
        items = await self.client.read_all(space, 2 ** 31, state=True)
        st = items[0]["state"]
        for pid, eff in st["effects"].items():
            if (eff["executor"] != self.actor or eff["phase"] != "ready" or eff["action"] not in self.actions.enabled
                    or pid in self._jobs or pid in self._skip or self._denied.get(pid, -1) >= st["head"]): continue
            self._jobs[pid] = asyncio.create_task(self._guard(self._job, space, pid, eff, st))
        return st["head"]

    # ── X-09: the plan ──
    async def _load_plan(self, space, pid, eff) -> dict | None:
        if pid in self._plans: return self._plans[pid]
        for it in await self.client.read_all(space, eff["seq"] - 1, limit=1):
            if it["type"] != "record" or it["record"]["receipt"]["seq"] != eff["seq"]: continue
            content = it["record"].get("content")
            plan = content.get("plan") if isinstance(content, dict) else None
            if plan is None: return None                              # redacted: can't be run
            if hashlib.sha256(P.jcs(plan)).hexdigest() != eff["planDigest"]: return None
            self._plans[pid] = plan
            return plan
        return None

    # ── X-02 … X-05: one claim ──
    async def _job(self, space, pid, eff, st):
        try:
            plan = await self._load_plan(space, pid, eff)
            try:
                if plan is None: raise Refused("plan missing or does not hash to planDigest (X-09)")
                self.actions.check(plan)
            except Refused as ex:
                log.warning("not running %s: %s", pid, ex); self._skip.add(pid); return
            window = st["policy"]["launchWindow"]
            unit = (st["policy"].get("budget") or {}).get("unit") if eff.get("cost") else None
            entry, content = self.client.make_entry("effect.claim", space, {"proposal": pid, "attempt": len(eff["claims"]) + 1,
                                                                            "instance": self.instance, "window": window})
            cid = P.entry_id(entry)
            t0 = self.clock()                                         # X-02: the reading at the first send (before it, so conservative)
            self.journal.add(cid, "claiming", {"space": space, "proposal": pid, "entry": entry, "content": content, "t0": t0,
                                               "window": window, "cost_unit": unit})
            await self._at("after-claiming", claim=cid)
            try: await self.client.append(entry, content)            # X-03: receipt in hand before acting
            except WireError as ex:
                self.journal.add(cid, "void", {"error": ex.to_wire()}); self._denied[pid] = st["head"]; return
            await self._at("after-receipt", claim=cid)
            self.journal.add(cid, "launching")                        # X-04
            await self._at("after-launching", claim=cid)
            if self.clock() - t0 >= window - STALL_BOUND:             # the last step before launching
                self.journal.add(cid, "expired")
                await self._report(cid, "failed", "no"); return
            self.launched.append(cid)
            try: result, evidence = "succeeded", await asyncio.to_thread(self.actions.run, plan)
            except Exception as ex: result, evidence = "failed", {"error": f"{type(ex).__name__}: {ex}"}
            await self._at("after-launch", claim=cid)
            self.journal.add(cid, "ran", {"result": result, "evidence": evidence})
            await self._at("after-ran", claim=cid)
            await self._report(cid, result, "yes")
        finally:
            self._jobs.pop(pid, None)
            if space in self._wake: self._wake[space].set()

    # ── X-05, X-10: reporting ──
    async def _report(self, cid, result, started):
        info = dict(self.journal.events(cid))["claiming"]
        body = {"claim": cid, "result": result, "started": started}
        if started == "yes" and info.get("cost_unit"): body["cost"] = {"unit": info["cost_unit"], "amount": 0}
        entry, content = self.client.make_entry("effect.outcome", info["space"], body)
        self.journal.add(cid, "reporting", {"entry": entry, "content": content})
        await self._at("before-report", claim=cid)
        await self._send_outcome(cid, entry, content)

    async def _send_outcome(self, cid, entry, content):
        try: receipt = await self.client.append(entry, content)
        except WireError as ex:
            self.journal.add(cid, "refused", {"error": ex.to_wire()})
            await self._x10(cid); return
        self.journal.add(cid, "reported", {"seq": receipt["seq"]})

    async def _x10(self, cid):
        """X-10: our outcome was refused. If an owner abandoned the claim, say what really happened with effect.resolve."""
        evs = self.journal.events(cid); names = [e for e, _ in evs]; data = dict(evs)
        info = data["claiming"]; space, pid = info["space"], info["proposal"]
        st = (await self.client.read_all(space, 2 ** 31, state=True))[0]["state"]
        eff = st["effects"].get(pid)
        claim = next((c for c in (eff or {}).get("claims", []) if c["id"] == cid), None)
        if not (eff and claim and eff["phase"] == "unknown" and eff["claims"][-1]["id"] == cid and claim.get("outcome") == "unknown"):
            log.warning("outcome of %s refused, not an abandon: %s", cid, data["refused"]["error"]); return
        if "ran" in names: result = data["ran"]["result"]
        elif "launching" not in names or "expired" in names:
            result = "failed"                                         # never launched
        else:
            self.journal.add(cid, "unresolved", {"why": "launched, outcome unknown"}); return
        evidence = {"instance": self.instance, "claim": cid, "journal": [[e, d] for e, d in evs if e != "claiming"] + [["claiming", {
            k: v for k, v in info.items() if k not in ("entry", "content")}]]}
        ref = await self.client.blob_put(json.dumps(evidence, sort_keys=True).encode(), "application/json")
        body = {"proposal": pid, "result": result}
        if result == "succeeded" and info.get("cost_unit"): body["cost"] = {"unit": info["cost_unit"], "amount": 0}
        entry, content = self.client.make_entry("effect.resolve", space, body, {"evidence": ref})
        try: await self.client.append(entry, content)
        except WireError as ex:
            self.journal.add(cid, "unresolved", {"error": ex.to_wire()}); return
        self.journal.add(cid, "resolved", {"result": result})

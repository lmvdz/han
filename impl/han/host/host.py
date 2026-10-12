"""The agent host (architecture.md §4.6 M6, P-15, R-17, R-18, R-24, §8.4): an actor that runs one agent in one space.

What drives a turn: a `session.input` with a receipt into the agent's own session, or a `task` naming it. Nothing else does:
messages, task.updates, outcomes, other sessions' streams and signals are data (R-24), collected and handed to the next turn
labelled as data. Which mode a turn runs in is decided from the state alone (`_mode_for`). What the agent decides becomes
journaled intents (journal.py) and only then entries: the host never acts in the world, a World action is an `effect.propose`
naming the configured executor."""
from __future__ import annotations
import asyncio, hashlib, time
from ..core import P, WireError
from ..entries import fmt_time
from .agent import ADVISORY, Agent, Say, Steer, Task, TurnInput, World
from .journal import Journal

MAX_DATA = 50
MAX_REFS = 64
CONTEXT_REQUEST = "han.context.request"       # a signal {type, session}; a request, not an instruction: it only publishes a snapshot

class HostCrash(Exception):
    """Raised by the test hook `crash(point)` to stop the host as a kill would."""

class Host:
    def __init__(self, client, space: str, agent: Agent, state_path: str, mode: str = "auto", executor: str | None = None,
                 crash=None, cost: dict | None = None):
        assert mode in ("auto", "guarded")
        self.client, self.space, self.agent, self.steer_mode, self.executor = client, space, agent, mode, executor
        self.default_cost = cost                              # {unit, amount} for a World action that names none (budgeted spaces)
        self.me = client.actor
        self.j = Journal(state_path)
        self.crash = crash                                    # test hook: crash(point, intent key) may raise HostCrash
        self.state: dict | None = None
        self.session: str | None = None
        self.data: list[tuple[str, str, str | None]] = []     # (source, text, record id): data, never instruction
        self.refused: list[str] = []                          # things the agent asked for that the host did not do
        self.turns = 0
        self._watchers: list[asyncio.Task] = []
        self._last_context = 0.0
        self._ctx: tuple[TurnInput, list[str]] | None = None  # the running turn and its refs, for propose hook calls
        self._made: dict[int, tuple] = {}                     # id(World) → (entry, content) signed while the turn ran
        agent.propose = self._propose_now

    # ── what the state says ──
    @property
    def held(self) -> bool: return bool(self.state) and self.me in self.state["held"]
    @property
    def guarded_now(self) -> str | None: return P.guarded_now(self.state, self.me) if self.state else None

    def _guarded_session(self) -> bool:
        sess = ((self.state or {}).get("sessions") or {}).get(self.session)
        return self.steer_mode == "guarded" or bool(sess and P.effective_mode(self.state, sess) == "guarded")

    def _crash(self, point, key=None):
        if self.crash: self.crash(point, key)

    async def _refresh(self):
        self.state = await self.client.read_state(self.space)

    async def _state_at(self, seq: int) -> dict | None:
        """The state after record `seq` (R-24: the mode is read from the state at the input's receipt, not the head)."""
        try: return await self.client.read_state(self.space, at=seq)
        except WireError: return None

    def _my_sessions(self) -> list[str]:
        return [i for i, s in self.state["sessions"].items() if s["actor"] == self.me]

    def _mode_for(self, author: str, sess_id: str | None, from_sessions: bool, at: dict | None = None) -> str:
        """R-24: `strict` while held (every turn); while guarded for any instruction by someone other than the guard; in a session whose
        effective mode is `guarded` for outside input; and when the turn carries another session's stream or context as data. The
        session's effective mode comes from the state at the input's seq (`at`); `held` and `guardedNow` come from the head."""
        head, me = self.state, self.me
        g = P.guarded_now(head, me)
        if me in head["held"]: return "strict"
        at = at or head
        sess = at["sessions"].get(sess_id) if sess_id else None
        if sess and author not in (me, sess.get("guard")) and P.effective_mode(at, sess) == "guarded": return "strict"
        if g and author not in (me, g): return "strict"
        if from_sessions: return "strict"              # SHOULD for any agent, MUST when guarded (R-24)
        return "auto"

    # ── life cycle ──
    async def run(self):
        """Connect, finish what the journal holds (R-18: outbox first), read the state, open the session, then follow the space."""
        await self.client.connect()
        await self._finish_journal()
        await self._refresh()
        await self._ensure_session()
        async for it in self.client.subscribe(self.space, after=self.j.cursor()):
            if it["type"] == "record": await self._on_record(it["record"])
            elif it["type"] == "signal": await self._on_signal(it)

    def stop(self):
        for t in self._watchers: t.cancel()
        self._watchers.clear()
        self.j.close()

    async def _finish_journal(self):
        for i in self.j.intents():
            if i["key"] == "open": self.session = P.entry_id(i["payload"]["entry"])
        await self._deliver_pending()
        await self._flush_items()

    async def _ensure_session(self):
        if self.session is None:
            e, c = self.client.make_entry("session.open", self.space, {"steerMode": self.steer_mode}, None, seen=self.state["head"])
            self.j.commit_turn(self.j.cursor(), [("open", "entry", {"entry": e, "content": c})])
            self.session = P.entry_id(e)
            await self._deliver_pending()
        await self._emit({"type": "status", "state": "idle"})

    # ── following ──
    async def _on_record(self, rec):
        e, rcpt = rec["entry"], rec["receipt"]
        seq = rcpt["seq"]
        if seq <= self.j.cursor(): return
        await self._refresh()
        eid, b, who = rcpt["id"], e["body"], e["author"]
        content = rec.get("content") or {}
        text = content.get("text") or content.get("ask")
        if who != self.me:
            if e["kind"] == "session.input" and b["session"] in self._my_sessions():
                return await self._turn(TurnInput("steer", who, text or "", eid, seq, b["session"]))
            if e["kind"] == "task" and self.me in b["to"] and self.state["tasks"].get(eid, {}).get("open"):
                return await self._turn(TurnInput("task", who, text or "", eid, seq, self.session, task=eid))
            if text:
                self._data("record", f"{e['kind']} by {who}: {text}", eid)      # a message, a task.update, an outcome: data
        self.j.commit_turn(seq, [])

    def _data(self, source, text, rid=None):
        self.data.append((source, text, rid))
        del self.data[:-MAX_DATA]

    async def _on_signal(self, it):
        b = it.get("body")
        if isinstance(b, dict) and b.get("type") == CONTEXT_REQUEST and b.get("session") == self.session and time.monotonic() - self._last_context > 1:
            self._last_context = time.monotonic()
            await self.publish_context()

    def watch_session(self, session: str, space: str | None = None):
        """Read another session's stream as data (T-07, R-24): it is collected for the next turn and never starts one."""
        async def pump():
            async for item in self.client.watch(space or self.space, session):
                self._data("session", f"[{session[:8]} {item['type']}] " + str(item.get("text") or item.get("state") or item.get("what")))
        self._watchers.append(asyncio.create_task(pump()))

    # ── a turn ──
    async def _turn(self, inp: TurnInput):
        from_sessions = any(s == "session" for s, _, _ in self.data)
        at = await self._state_at(inp.seq)
        mode = self._mode_for(inp.author, inp.session, from_sessions, at) if at else "strict"      # no state at the receipt: be strict
        data, self.data = self.data, []
        inp.data = [t for _, t, _ in data]
        refs = [inp.trigger] + [r for _, _, r in data if r][-(MAX_REFS - 1):]
        self._ctx, self._made = (inp, refs), {}
        await self._emit({"type": "status", "state": "working"})
        self.turns += 1
        try:
            actions = await self.agent.turn(inp, mode)
        except Exception as ex:
            self._ctx = None
            await self._emit({"type": "status", "state": "error", "detail": str(ex)[:200]})
            return self.j.commit_turn(inp.seq, [])
        self._ctx = None
        self._crash("decided")
        await self._refresh()                          # the state may have changed during the turn (a hold, a release)
        intents = self._plan(actions, inp, refs)
        self.j.log(f"{mode} turn by {inp.author} at {inp.seq}: {inp.text}")
        for a in actions: self.j.log(f"  -> {a}")
        self.j.commit_turn(inp.seq, intents)           # the decision and the cursor move are one transaction
        self._crash("journaled")
        await self._deliver_pending()
        await self._emit({"type": "status", "state": "idle"})

    def _plan(self, actions, inp: TurnInput, refs) -> list[tuple[str, str, dict]]:
        """Actions → intents [(key, kind, payload)], keyed by trigger and position, so a decision is made once. Entries are signed now
        (R-17: `seen` is the instruction's seq, `refs` name it and the data records read)."""
        out, held = [], self.held
        def refuse(key, why):
            self.refused.append(why); out.append((key + "r", "say", {"text": "refused: " + why}))
        def entry(kind, body, content):
            assert kind != "agent.release" and not {"release", "unguard"} & set(body)        # E1, F2: only the guard releases or unguards; the host never does, for itself least of all
            return self.client.make_entry(kind, self.space, body, content, refs=refs, seen=inp.seq)
        for i, a in enumerate(actions):
            key = f"{inp.trigger}:{i}"
            if isinstance(a, Say):
                out.append((key, "say", {"text": a.text}))
            elif isinstance(a, World):
                made = self._made.get(id(a)) or self._world_entry(a, inp, refs)
                if isinstance(made, str): refuse(key, made); continue
                e, c = made
                out.append((key, "entry", {"entry": e, "content": c}))
                out.append((key + "s", "say", {"text": f"proposed {a.action} as {P.entry_id(e)}"}))
            elif isinstance(a, Task):
                if held: refuse(key, f"held: cannot delegate to {a.to}"); continue
                e, c = entry("task", {"to": [a.to]}, {"ask": a.text})
                out.append((key, "entry", {"entry": e, "content": c}))
            elif isinstance(a, Steer):
                if held and a.session not in self._my_sessions(): refuse(key, f"held: cannot steer {a.session[:8]}"); continue
                e, c = entry("session.input", {"session": a.session}, {"text": a.text})
                out.append((key, "entry", {"entry": e, "content": c}))
        return out

    def _world_entry(self, a: World, inp: TurnInput, refs):
        """A World action → the signed effect.propose (entry, content), or the reason it cannot be proposed (a str)."""
        if not self.executor: return f"{a.action}: no executor configured"
        if not P.ACTION.fullmatch(a.action): return f"bad action name {a.action!r}"
        plan = {"action": a.action, "target": str(a.args.get("url") or a.args.get("target") or a.args.get("path") or a.action)[:1024], "args": a.args}
        body = {"executor": self.executor, "action": a.action, "planDigest": hashlib.sha256(P.jcs(plan)).hexdigest(), "repeatable": False}
        if P.space_unit(self.state):                       # budgeted space: P-05/P-06 need a cost and a budgeted task of ours
            cost = a.cost or self.default_cost
            task = self._budget_task(inp.task, cost)
            if not cost or cost["unit"] != P.space_unit(self.state): return f"{a.action}: cannot spend, no cost in the space's unit ({P.space_unit(self.state)})"
            if not task: return f"{a.action}: cannot spend, no open budgeted task is assigned to me"
            body["cost"], body["task"] = dict(cost), task
        elif inp.task: body["task"] = inp.task
        return self.client.make_entry("effect.propose", self.space, body, {"plan": plan}, refs=refs, seen=inp.seq)

    def _propose_now(self, a: World) -> str | None:
        """The agent's hook, called while its turn runs (8.4: the model is answered at once, nothing blocks): sign the proposal now, keep
        it for `_plan`, and return what the model is told. The entry is only appended after the turn's decision is journaled."""
        if not self._ctx: return None
        made = self._world_entry(a, *self._ctx)
        if isinstance(made, str): return "refused: " + made
        self._made[id(a)] = made
        guard = (self.state or {}).get("held", {}).get(self.me)
        return f"proposed as {P.entry_id(made[0])}" + (f", waiting for {guard}" if guard else "")

    def _budget_task(self, preferred: str | None, cost: dict | None) -> str | None:
        """An open budgeted task assigned to this agent: the one that drove the turn if it qualifies, else one with enough left."""
        mine = {tid: t for tid, t in self.state["tasks"].items() if t["open"] and t.get("left") and self.me in t["to"]}
        if preferred in mine: return preferred
        need = cost["amount"] if cost else 0
        return next((tid for tid, t in mine.items() if t["left"]["amount"] >= need), next(iter(mine), None))

    async def _deliver_pending(self):
        for it in self.j.pending():
            if it["kind"] == "say":
                await self._emit({"type": "output", "text": it["payload"]["text"]})
                self.j.settle(it["key"], "done")
                continue
            try:
                r = await self.client.append(it["payload"]["entry"], it["payload"]["content"])
            except WireError as ex:                    # Policy said no (a stale view, a revoked right): report it as data, once
                self.j.settle(it["key"], "rejected", ex.to_wire())
                self.refused.append(f"{it['key']}: {ex}")
                await self._emit({"type": "output", "text": f"refused by the space: {ex.message}"})
                continue
            self._crash("appended", it["key"])
            self.j.settle(it["key"], "done", r)

    # ── the session stream (G-05, T-07) ──
    async def _emit(self, fields: dict):
        """Journal the next item (n and time fixed), then send it; an unsent item is resent after a restart (a resend of the last item is
        harmless, NOTES 20)."""
        if fields["type"] == "status" and "detail" not in fields and self._guarded_session() and not self.agent.sandboxed:
            fields = fields | {"detail": ADVISORY}                       # §8.4: without an OS sandbox `guarded` is advisory, and the status says so
        self.j.add_item(lambda n: {"space": self.space, "session": self.session, "n": n, "time": fmt_time(time.time())} | fields)
        await self._flush_items()

    async def _flush_items(self):
        for item in self.j.unsent_items():
            try: await self.client.stream(item)
            except WireError as ex:
                if ex.code not in ("denied", "not_found"): raise      # a closed session or lost right: the item is dropped
            except (ConnectionError, OSError): return
            self.j.sent(item["n"])

    async def publish_context(self, what: str = "transcript"):
        """T-07: a `context` item whose blob is the transcript; the sequencer delivers it only to actors with the read right."""
        ref = await self.client.blob_put(self.j.transcript().encode() or b"-", "text/plain")
        await self._emit({"type": "context", "blob": ref, "what": what})

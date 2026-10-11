"""A HAN actor's client over the Matrix binding (architecture.md §8.2): entries are header-only `org.han.entry` room events (their
content goes ahead by `POST /_han/v1/content`), answered by `org.han.receipt` or `org.han.reject` from the sequencer's
application-service user. Matrix's sender field is never authority: a reject or receipt counts only from the user named in the room's
`org.han.space` state, and only when that state itself was sent by that user and names the conventional service user of the space's
server (NOTES.md, "binding trust"). Content is read back through `GET /_han/v1/read`, which enforces P-02; the room never shows it.
The class also satisfies `han.client.ClientProtocol` (append, read, subscribe, signal, blob_put, blob_get, stream, watch), so
han-exec and han-host run over it.

    c = MatrixClient("http://127.0.0.1:18008", "@alice:a.localhost", token, "alice.key.json", "alice.outbox.db", service_url="http://127.0.0.1:9300")
    await c.start()
    room = await c.create_space_room("team")
    receipt = await c.append(*c.make_entry("space.create", "s1:a.localhost", {"policy": default_policy()}, {"name": "team"}), room=room)
"""
from __future__ import annotations
import asyncio, hashlib, json, logging, time
import aiohttp
from .. import keys
from ..core import P, W, LIMITS, WireError
from ..client.outbox import Outbox
from ..entries import make_entry, fmt_time
from . import media
from .api import MatrixAPI, MatrixError
from .proto import ENTRY, RECEIPT, REJECT, SPACE, SESSION, STREAM, SIGNAL, entry_header, service_user

log = logging.getLogger("han.matrix.client")
MAX_MATRIX_ENTRY = 60 * 1024         # header + content (JCS bytes) above this go over the generic binding
SYNC_FILTER = {"room": {"timeline": {"limit": 100}}, "presence": {"types": []}}

def default_sequencer_for(space: str) -> str:
    """The convention: the sequencer of a space is `@han-seq:` on the server named in its id."""
    return service_user(P.server_of(space))

class RoomView:
    """What this client has seen of one room."""
    def __init__(self):
        self.space = self.sequencer = None                   # the pinned binding (org.han.space)
        self.entries: dict[str, dict] = {}                   # entry id → {entry, content, redacted}
        self.receipts: dict[str, dict] = {}                  # entry id → receipt (from the sequencer only)
        self.rejects: dict[str, dict] = {}                   # entry id → org.han.reject content (from the sequencer only)
        self.sent: dict[str, str] = {}                       # our event id → entry id
        self.provisional: dict[str, str] = {}                # entry id → the sequencer expected to answer it in a room not bound yet
        self.waiters: dict[str, list[asyncio.Future]] = {}
        self.sessions: dict[str, dict] = {}                  # session id → {watch, read} (org.han.session from the sequencer)
        self.listeners: list[asyncio.Queue] = []
        self.emitted: set[str] = set()
        self.ignored: list[tuple] = []                       # (kind, sender, why): events we refused to honour
        self.stream: list[tuple[str, dict, dict]] = []       # (sender, StreamItem, event content) in a session room

class Submission:
    """One sent entry event, not yet answered."""
    def __init__(self, room, event_id, entry_id, fut): self.room, self.event_id, self.entry_id, self.future = room, event_id, entry_id, fut
    async def result(self, timeout: float | None = None) -> dict:
        return await asyncio.wait_for(asyncio.shield(self.future), timeout)

class MatrixClient:
    def __init__(self, homeserver: str, user_id: str, access_token: str, key_path: str, outbox_db: str, service_url: str | None = None,
                 resend_after: float = 10.0, sequencer_for=default_sequencer_for):
        self.actor = user_id                                 # T-02: the HAN actor is the Matrix user of the same id
        self.api = MatrixAPI(homeserver, access_token)
        self.key = keys.load(key_path)
        self.outbox = Outbox(outbox_db)
        self.service = MatrixAPI(service_url) if service_url else None
        self.resend_after, self.sequencer_for = resend_after, sequencer_for
        self.rooms: dict[str, RoomView] = {}
        self.heads: dict[str, int] = {}
        self.context_files: dict[str, dict] = {}             # blob hash → encrypted-file info of a context item
        self._blobs: dict[str, bytes] = {}                   # the last blobs we uploaded, so `stream` of a context item can attach them
        self._started = False
        self.sync_events = asyncio.Event()
        self._since = None
        self._sync_task = None
        self._stop = False

    # ── lifecycle ──
    async def connect(self, known_heads=None):
        """`ClientProtocol.connect`: `start` once."""
        if not self._started: await self.start()

    async def start(self):
        """First sync (with backfill), then keep syncing in the background, then finish every unconfirmed outbox entry (R-07)."""
        self._started = True
        assert await self.api.whoami() == self.actor, "access token belongs to another user"
        await self._sync_once(0)
        while await self._sync_once(0): pass            # Synapse may answer the first sync from a cache: catch up until nothing is new
        self._sync_task = asyncio.create_task(self._sync_loop())
        for row in self.outbox.pending():
            room = self.room_of(row["space"])
            if room:
                try: await self.append(row["entry"], row["content"], room=room, timeout=30)
                except (WireError, asyncio.TimeoutError): pass
        return self

    async def close(self):
        self._stop = True
        if self._sync_task: self._sync_task.cancel()
        await self.api.close()
        if self.service: await self.service.close()
        self.outbox.close()

    # ── sync: the only way events reach us ──
    async def _sync_loop(self):
        while not self._stop:
            try: await self._sync_once(10000)
            except asyncio.CancelledError: raise
            except Exception as ex:
                log.warning("sync: %s", ex); await asyncio.sleep(1)

    async def _sync_once(self, timeout_ms: int):
        r = await self.api.sync(self._since, timeout_ms, SYNC_FILTER)
        self._since = r["next_batch"]
        for room, j in r.get("rooms", {}).get("join", {}).items():
            tl = j.get("timeline", {})
            if tl.get("limited") and tl.get("prev_batch"): await self._backfill(room, tl["prev_batch"])
            for ev in j.get("state", {}).get("events", []) + tl.get("events", []): self._on_event(room, ev)
        self.sync_events.set(); self.sync_events = asyncio.Event()
        return bool(r.get("rooms", {}).get("join"))

    async def _backfill(self, room: str, token: str):
        """Walk the timeline back to its start (the sync gave only the newest events) and process it oldest first."""
        evs = []
        while token:
            page = await self.api.messages(room, token, 100)
            evs += page.get("chunk", [])
            token = page.get("end") if page.get("chunk") else None
        for ev in reversed(evs): self._on_event(room, ev)

    # ── events in ──
    def _view(self, room) -> RoomView: return self.rooms.setdefault(room, RoomView())

    def _on_event(self, room: str, ev: dict):
        rv, t, sender, c = self._view(room), ev.get("type"), ev.get("sender"), ev.get("content") or {}
        if t == SPACE and ev.get("state_key") == "": self._on_space(rv, sender, c)
        elif t == SESSION and ev.get("state_key") is not None:
            if sender == rv.sequencer and isinstance(c.get("watch"), str) and isinstance(c.get("read"), str): rv.sessions[ev["state_key"]] = c
            else: rv.ignored.append(("session", sender, "not from the bound sequencer"))
        elif t == ENTRY: self._on_entry(rv, sender, c)
        elif t == RECEIPT: self._on_receipt(rv, sender, ev, c)
        elif t == REJECT: self._on_reject(rv, sender, ev, c)
        elif t == STREAM and isinstance(c.get("type"), str): rv.stream.append((sender, {k: v for k, v in c.items() if k != "file"}, c)); self._note_file(c)
        elif t == SIGNAL: self._emit(rv, {"type": "signal", "from": sender, "body": c.get("body")})

    def _on_space(self, rv: RoomView, sender, c):
        sp, seqr = c.get("spaceId"), c.get("sequencer")
        if not (isinstance(sp, str) and isinstance(seqr, str)) or sender != seqr:
            return rv.ignored.append(("space", sender, "malformed, or not sent by the sequencer it names"))
        try: ok = seqr == self.sequencer_for(sp)
        except Exception: ok = False
        if not ok: return rv.ignored.append(("space", sender, f"{seqr} is not the sequencer of {sp}"))
        if rv.sequencer is not None and (seqr != rv.sequencer or sp != rv.space):
            return rv.ignored.append(("space", sender, "a room keeps its first binding"))
        rv.space, rv.sequencer = sp, seqr

    def _on_entry(self, rv: RoomView, sender, c):
        entry = c.get("entry")
        if not isinstance(entry, dict) or c.get("contentHash") != entry.get("contentHash"): return      # junk, or not a header-only event
        try: eid = P.entry_id(entry)
        except Exception: return
        rv.entries.setdefault(eid, {"entry": entry, "content": None, "redacted": False})        # content comes from fill_content, never from the room
        self._try_emit(rv, eid)

    def _from_sequencer(self, rv: RoomView, sender) -> bool:
        return rv.sequencer is not None and sender == rv.sequencer

    def _on_receipt(self, rv: RoomView, sender, ev, c):
        r = c.get("receipt")
        if not self._from_sequencer(rv, sender) or not isinstance(r, dict) or not isinstance(r.get("id"), str) or r.get("space") != rv.space:
            return rv.ignored.append(("receipt", sender, "not from the bound sequencer"))
        rv.receipts.setdefault(r["id"], r)
        if isinstance(r.get("seq"), int) and r["seq"] > self.heads.get(r["space"], 0): self.heads[r["space"]] = r["seq"]
        for f in rv.waiters.pop(r["id"], []):
            if not f.done(): f.set_result(r)
        self._try_emit(rv, r["id"])

    def _on_reject(self, rv: RoomView, sender, ev, c):
        eid = c.get("id") or rv.sent.get(((c.get("m.relates_to") or {}).get("event_id")))
        # §8.2: a reject counts only from the sequencer's user. A room with no binding yet (a refused space.create, an entry sent
        # to the wrong room) has none, so there the user expected for the entry's space speaks (NOTES.md, "binding trust").
        if not (self._from_sequencer(rv, sender) or (rv.sequencer is None and eid and sender == rv.provisional.get(eid))):
            return rv.ignored.append(("reject", sender, "not from the bound sequencer"))
        if not eid: return
        rv.rejects.setdefault(eid, c)
        for f in rv.waiters.pop(eid, []):
            if not f.done(): f.set_exception(WireError(c.get("code", "invalid"), c.get("reason", "")))

    def _try_emit(self, rv: RoomView, eid: str):
        if eid in rv.emitted or eid not in rv.receipts or eid not in rv.entries: return
        rv.emitted.add(eid)
        self._emit(rv, {"type": "record", "record": self._record(rv, eid)})

    def _emit(self, rv: RoomView, item: dict):
        for q in rv.listeners: q.put_nowait(item)

    def _note_file(self, c):
        if isinstance(c.get("file"), dict) and isinstance(c.get("blob"), dict): self.context_files[c["blob"].get("hash")] = c["file"]

    # ── the room as a log (T-01) ──
    def _record(self, rv: RoomView, eid: str, redactions: dict | None = None) -> dict:
        e = rv.entries[eid]
        rec = {"entry": e["entry"], "content": e["content"], "redacted": False, "receipt": rv.receipts[eid]}
        by = (redactions if redactions is not None else self._redactions(rv)).get(eid)
        if by: rec |= {"content": None, "redacted": True, "redactedBy": by}
        return rec

    def _redactions(self, rv: RoomView) -> dict:
        """target entry id → the redact record that deleted its content (R-04)."""
        out = {}
        for eid, r in rv.receipts.items():
            e = rv.entries.get(eid)
            if e and e["entry"]["kind"] == "redact": out.setdefault(e["entry"]["body"].get("target"), {"entry": e["entry"], "receipt": r})
        return out

    def records(self, room: str) -> list[dict]:
        """Every LogRecord the room shows, in seq order: entry events joined to the sequencer's receipts. Verify them with a Mirror."""
        rv = self._view(room); red = self._redactions(rv)
        return sorted((self._record(rv, eid, red) for eid in rv.receipts if eid in rv.entries), key=lambda r: r["receipt"]["seq"])

    async def fill_content(self, room: str):
        """Fetch the content of the room's records through `GET /_han/v1/read` (P-02: only what this actor may read) and keep it, so
        `records(room)` carries it. Records the actor may not read stay header-only."""
        rv, last = self._view(room), 0
        if rv.space is None: return
        while True:
            recs = [i["record"] for i in await self.read_http(rv.space, last, limit=1000) if i["type"] == "record"]
            for rec in recs:
                have = rv.entries.get(rec["receipt"]["id"])
                if have and have["content"] is None and rec.get("content") is not None: have["content"] = rec["content"]
            if len(recs) < 1000: return
            last = recs[-1]["receipt"]["seq"]

    def room_of(self, space: str) -> str | None:
        return next((r for r, v in self.rooms.items() if v.space == space), None)

    async def follow(self, room: str):
        """ReadItems of a room as they arrive: records (receipt joined to its entry) and `org.han.signal`s (T-05)."""
        rv = self._view(room); q = asyncio.Queue(); rv.listeners.append(q)
        try:
            for r in self.records(room): q.put_nowait({"type": "record", "record": r})
            while True: yield await q.get()
        finally: rv.listeners.remove(q)

    # ── rooms (T-04, T-06) ──
    async def create_space_room(self, name: str, invite=(), sequencer: str | None = None, wait: float = 20.0) -> str:
        """A private room that invites the sequencer's service user (and `invite`), with the power to publish the binding and
        the session links; returns once the service user has joined, so nothing posted next is missed."""
        seqr = sequencer or service_user(P.server_of(self.actor))
        room = await self.api.create_room({
            "name": name, "preset": "private_chat", "visibility": "private", "invite": [seqr, *invite],
            "power_level_content_override": {"users": {self.actor: 100, seqr: 100}},
            "initial_state": [{"type": "m.room.history_visibility", "state_key": "", "content": {"history_visibility": "shared"}}]})
        await self.until_joined(room, seqr, wait)
        return room

    async def until_joined(self, room: str, user: str, wait: float = 20.0):
        deadline = time.monotonic() + wait
        while (await self.api.members(room)).get(user) != "join":
            if time.monotonic() > deadline: raise TimeoutError(f"{user} did not join {room}")
            await asyncio.sleep(0.1)

    async def join(self, room: str, wait: float = 20.0):
        """Join a room we were invited to (retrying while the invite is still on its way), then take in its history."""
        deadline = time.monotonic() + wait
        while True:
            try: await self.api.join(room); break
            except MatrixError as ex:
                if ex.errcode != "M_FORBIDDEN" or time.monotonic() > deadline: raise
                await asyncio.sleep(0.2)
        await self.until_seen(lambda: room in self.rooms, wait)

    async def until_seen(self, pred, timeout: float = 20.0):
        deadline = time.monotonic() + timeout
        while not pred():
            left = deadline - time.monotonic()
            if left <= 0: raise TimeoutError("condition not reached")
            try: await asyncio.wait_for(self.sync_events.wait(), min(left, 1.0))
            except asyncio.TimeoutError: pass

    # ── append (§3 over Matrix) ──
    def make_entry(self, kind, space, body, content=None, refs=(), seen=None):
        return make_entry(self.key, self.actor, kind, space, body, content, refs, self.heads.get(space, 0) if seen is None else seen)

    async def submit(self, entry: dict, content=None, room: str | None = None, txn: str | None = None) -> Submission:
        """Store in the outbox (R-07), send the event once and return without waiting."""
        eid = P.entry_id(entry)
        room = room or self.room_of(entry["space"])
        if room is None: raise WireError("not_found", "no room is bound to that space; pass room=")
        rv = self._view(room)
        if rv.sequencer is None:
            try: rv.provisional[eid] = self.sequencer_for(entry["space"])
            except Exception: pass
        self.outbox.add(eid, entry["space"], entry, content)
        if content is not None: await self._han("POST", "/_han/v1/content", body=content)       # P-02: content never enters the room
        fut = asyncio.get_running_loop().create_future()
        if eid in rv.receipts: fut.set_result(rv.receipts[eid])
        elif eid in rv.rejects: fut.set_exception(WireError(rv.rejects[eid].get("code", "invalid"), rv.rejects[eid].get("reason", "")))
        else: rv.waiters.setdefault(eid, []).append(fut)
        ev_id = await self.api.send(room, ENTRY, entry_header(entry), txn or f"{eid}-{time.time_ns()}")
        rv.sent[ev_id] = eid
        return Submission(room, ev_id, eid, fut)

    async def append(self, entry: dict, content=None, timeout: float | None = None, room: str | None = None) -> dict:
        """Send and wait for the receipt (pending until then or a reject). Resends the same entry as a new event every
        `resend_after` seconds (R-06; the sequencer's dedupe makes that harmless, R-01). A reject from the bound sequencer raises
        WireError. An entry over 60 KiB (header + content) is refused at once: send it over the generic binding."""
        size = len(P.jcs(entry)) + (0 if content is None else len(P.jcs(content)))
        if size > MAX_MATRIX_ENTRY:
            raise WireError("invalid", f"entry is {size} bytes, over the {MAX_MATRIX_ENTRY} the Matrix binding carries; use the generic binding (WebSocket)")
        deadline = None if timeout is None else time.monotonic() + timeout
        eid = P.entry_id(entry)
        while True:
            try: sub = await self.submit(entry, content, room)
            except WireError as ex:
                if ex.code not in ("unavailable", "limit"): self.outbox.rejected(eid, ex.to_wire()); raise
                await asyncio.sleep(0.5); continue
            except MatrixError as ex:
                if ex.status < 500 and ex.status != 429:        # Matrix itself refused the event (too large, not in the room): final
                    err = WireError("invalid", f"Matrix refused the event: {ex.errcode} {ex.message}")
                    self.outbox.rejected(eid, err.to_wire()); raise err from None
                await asyncio.sleep(0.5); continue              # R-06: the outcome is unknown, try again
            except (aiohttp.ClientError, OSError, asyncio.TimeoutError): await asyncio.sleep(0.5); continue
            wait = self.resend_after if deadline is None else min(self.resend_after, deadline - time.monotonic())
            try:
                receipt = await sub.result(max(wait, 0.01))
                self.outbox.done(eid, receipt); return receipt
            except asyncio.TimeoutError:
                if deadline is not None and time.monotonic() >= deadline: raise
            except WireError as ex:
                self.outbox.rejected(eid, ex.to_wire()); raise

    # ── the service's HTTP side: `Authorization: HAN <actor> <keyId> <proof>` + `HAN-Challenge` (a fresh single-use challenge) ──
    async def _han(self, method: str, path: str, params: dict | None = None, body=None):
        assert self.service, "service_url not set"
        ch = await self.service.request("GET", "/_han/v1/challenge")
        if ch["server"] != P.server_of(self.actor): raise WireError("denied", f"challenge names {ch['server']}")
        proof = W.hello_proof(self.key, ch["challenge"], ch["server"])
        try:
            return await self.service.request(method, path, body=body, params=params,
                                              headers={"Authorization": f"HAN {self.actor} {self.key.key_id} {proof}", "HAN-Challenge": ch["challenge"]})
        except MatrixError as ex:
            if "error" in ex.body: raise WireError.from_wire(ex.body["error"]) from None
            raise

    async def read_http(self, space: str, after: int = 0, state: bool = False, limit: int | None = None, at: int | None = None) -> list[dict]:
        """GET /_han/v1/read → `{items: [ReadItem]}`; P-02 is enforced by the service, so this is where content comes from."""
        params = {"space": space, "after": str(after)} | ({"state": "true"} if state else {}) | ({} if limit is None else {"limit": str(limit)}) \
            | ({} if at is None else {"at": str(at)})
        return (await self._han("GET", "/_han/v1/read", params))["items"]

    # ── ClientProtocol: read, subscribe (records through the HTTP read, signals from the room) ──
    async def read(self, space, after=0, follow=False, state=False, limit=None, known_heads=None, at=None):
        """ReadItems after `after`, as the generic binding gives them. With `follow`, waits for the room's receipts (polling once a
        second as a fallback) and reads what is new; a backlog cut by `limit` is read on at once. Ends only when closed."""
        if known_heads is not None: raise WireError("invalid", "knownHeads is not served by the HTTP read")
        last, first = after, True
        while True:
            seen = self.heads.get(space, 0)
            try: items = await self.read_http(space, last, state=state and first, limit=limit, at=at if first else None)
            except (aiohttp.ClientError, OSError, asyncio.TimeoutError):
                if not follow: raise
                await asyncio.sleep(0.5); continue
            except WireError as ex:
                if not follow or ex.code != "unavailable": raise
                await asyncio.sleep(0.5); continue
            first = False
            n = 0
            for it in items:
                if it["type"] == "record":
                    last = it["record"]["receipt"]["seq"]; n += 1; self._note(space, last)
                yield it
            if not follow: return
            if limit and n >= limit: continue
            deadline = time.monotonic() + 1.0
            while self.heads.get(space, 0) <= seen and time.monotonic() < deadline:
                try: await asyncio.wait_for(self.sync_events.wait(), max(deadline - time.monotonic(), 0.01))
                except asyncio.TimeoutError: pass

    def _note(self, space, seq):
        if seq > self.heads.get(space, 0): self.heads[space] = seq

    async def read_all(self, space, after=0, **kw) -> list:
        return [it async for it in self.read(space, after, **kw)]

    async def subscribe(self, space, after=None):
        """Records (read over HTTP, so P-02 holds) and other members' room signals, live; with `after`, the records after it first."""
        room = self.room_of(space)
        sig = asyncio.Queue()
        rv = self._view(room) if room else None
        if rv: rv.listeners.append(sig)
        if after is None: after = (await self.read_http(space, 2 ** 31, state=True))[0]["state"]["head"]
        records = self.read(space, after, follow=True)
        nxt, nsig = asyncio.ensure_future(records.__anext__()), asyncio.ensure_future(sig.get())
        try:
            while True:
                done, _ = await asyncio.wait({nxt, nsig}, return_when=asyncio.FIRST_COMPLETED)
                if nsig in done:
                    it = nsig.result(); nsig = asyncio.ensure_future(sig.get())
                    if it["type"] == "signal" and it["from"] != self.actor: yield it
                if nxt in done:
                    it = nxt.result(); nxt = asyncio.ensure_future(records.__anext__())
                    yield it
        finally:
            for f in (nxt, nsig): f.cancel()
            await asyncio.gather(nxt, nsig, return_exceptions=True)
            await records.aclose()
            if rv: rv.listeners.remove(sig)

    # ── signals (T-05) ──
    async def signal(self, space: str, body):
        """A room signal. `space` is a space id (its room is looked up) or a room id."""
        room = space if space.startswith("!") else self.room_of(space)
        if room is None: raise WireError("not_found", "no room is bound to that space")
        await self.api.send(room, SIGNAL, {"body": body}, f"sg-{time.time_ns()}")

    # ── blobs over the service's HTTP side (G-01 is the Sequencer's own check) ──
    async def blob_put(self, data: bytes, type: str = "application/octet-stream") -> dict:
        ref = {"hash": hashlib.sha256(data).hexdigest(), "size": len(data), "type": type}
        offset, part = 0, LIMITS["maxBlobPartBytes"]
        while True:
            res = await self._han("PUT", "/_han/v1/blob", body={"ref": ref, "offset": offset, "data": W.b64(data[offset:offset + part])})
            if res["complete"]: break
            if res["have"] == offset or res["have"] > len(data): raise WireError("invalid", "upload did not complete")
            offset = res["have"]
        self._blobs[ref["hash"]] = data
        for k in list(self._blobs)[:-16]: del self._blobs[k]
        return ref

    async def blob_get(self, ref: dict, space: str) -> bytes:
        out = bytearray()
        while True:
            r = await self._han("GET", "/_han/v1/blob", {"hash": ref["hash"], "size": str(ref["size"]), "type": ref["type"], "space": space, "offset": str(len(out))})
            out += W.unb64(r["data"])
            if len(out) >= r["size"]: break
            if not r["data"]: raise WireError("unavailable", "empty range")
        data = bytes(out)
        if hashlib.sha256(data).hexdigest() != ref["hash"] or len(data) != ref["size"]: raise ValueError("blob does not match its reference")
        return data

    # ── session streams (T-07) ──
    async def session_rooms(self, space_room: str, session: str, wait: float = 20.0) -> dict:
        await self.until_seen(lambda: session in self._view(space_room).sessions, wait)
        return self._view(space_room).sessions[session]

    async def join_session(self, space_room: str, session: str, which: str = "watch", wait: float = 20.0) -> str:
        """Join the session's watch or read room (we are invited once `sessionAccess` grants it) and start receiving its items."""
        room = (await self.session_rooms(space_room, session, wait))[which]
        await self.join(room, wait)
        return room

    async def stream(self, item: dict):
        """`ClientProtocol.stream`: publish to the session's watch room (output, status) or read room (context; its blob must have
        been uploaded by this client with `blob_put`, and goes out as an encrypted attachment)."""
        room = self.room_of(item["space"])
        if room is None: raise WireError("not_found", "no room is bound to that space")
        blob = None
        if item.get("type") == "context":
            blob = self._blobs.get(item["blob"]["hash"])
            if blob is None: raise WireError("invalid", "a context item's blob must come from this client's blob_put")
        await self.publish(room, item, blob)

    async def watch(self, space: str, session: str):
        """`ClientProtocol.watch`: join the session's watch room (we are invited once `sessionAccess` grants it) and yield its items."""
        room = self.room_of(space)
        if room is None: raise WireError("not_found", "no room is bound to that space")
        async for it in self.watch_room(await self.join_session(room, session, "watch")): yield it

    async def watch_room(self, room: str, start: int = 0):
        """StreamItems posted in a session room we have joined, from item index `start` on (context items carry an encrypted
        blob: `fetch_context`)."""
        rv, i = self._view(room), start
        while True:
            while i < len(rv.stream):
                yield rv.stream[i][1]; i += 1
            await self.sync_events.wait()

    async def fetch_context(self, item: dict) -> bytes:
        """Download and decrypt a context item's blob, then check it against its BlobRef (T-03)."""
        ref, info = item["blob"], self.context_files.get(item["blob"]["hash"])
        if info is None: raise WireError("not_found", "no key for this blob")
        data = media.decrypt(await self.api.download(ref["uri"]), info)
        if hashlib.sha256(data).hexdigest() != ref["hash"] or len(data) != ref["size"]: raise ValueError("blob does not match its reference")
        return data

    async def publish(self, space_room: str, item: dict, blob: bytes | None = None, wait: float = 15.0):
        """T-07 publisher: send one StreamItem to the session's watch room (output, status) or read room (context). Before it
        sends, it reads the state at the sequencer's latest receipt and waits until each room's members are within what
        `sessionAccess` grants, so an actor whose right a receipt removed is out of the room (kick landed) before the item
        exists. (Without end-to-end encryption this is the only thing that keeps the item from it; NOTES.md.)"""
        space, sid = item["space"], item["session"]
        rooms = await self.session_rooms(space_room, sid)
        kind = "read" if item["type"] == "context" else "watch"
        deadline = time.monotonic() + wait
        while True:
            st = (await self.read_http(space, 2 ** 31, state=True))[0]["state"]
            cands = set(st["roles"]) | {st["sessions"][sid]["actor"]} | ({st["sessions"][sid]["guard"]} if st["sessions"][sid].get("guard") else set())
            want = {a for a in cands if P.session_access(st, a, sid)[kind]}
            have = {u for u, m in (await self.api.members(rooms[kind])).items() if m in ("join", "invite")} - {service_user(P.server_of(space))}
            if have <= want: break
            if time.monotonic() > deadline: raise TimeoutError(f"members {sorted(have - want)} are still in the {kind} room")
            await asyncio.sleep(0.1)
        content = dict(item)
        if blob is not None:
            ct, info = media.encrypt(blob)
            uri = await self.api.upload(ct)
            content["blob"] = {"hash": hashlib.sha256(blob).hexdigest(), "size": len(blob), "type": "application/octet-stream", "uri": uri}
            content["file"] = info
        return await self.api.send(rooms[kind], STREAM, content, f"st-{sid}-{item['n']}")

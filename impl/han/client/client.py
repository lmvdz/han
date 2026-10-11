"""The HAN client over the generic binding: han.ts `Client` for one actor. It hides framing, the hello proof, reconnects and the
outbox (R-06, R-07). One WebSocket at a time; requests are multiplexed by frame id."""
from __future__ import annotations
import asyncio, hashlib, json, os
from urllib.parse import urlsplit
from websockets.asyncio.client import connect as ws_connect
from websockets.exceptions import ConnectionClosed, WebSocketException
from .. import keys
from ..core import P, W, LIMITS, WireError
from ..entries import make_entry
from .outbox import Outbox
from .state import StateReads

_CLOSED = object()
PATH = "/han"                 # D7: the generic binding's WebSocket path
_NET = (ConnectionError, OSError, ConnectionClosed, WebSocketException, asyncio.TimeoutError)

class Client(StateReads):
    def __init__(self, url: str, actor: str, key_path: str, outbox_db: str, server: str | None = None,
                 retry_delay: float = 1.0, max_retry_delay: float = 30.0, key=None):
        self.url, self.actor = (url.rstrip("/") + PATH if urlsplit(url).path in ("", "/") else url), actor      # D7: never a bare host
        self.server = server or P.server_of(actor)               # the name we dial: bound into the hello proof
        self.key = key or keys.load(key_path)                     # `key`: an already loaded key (a server's own, for @:server connections)
        self.outbox = Outbox(outbox_db)
        self.retry_delay, self.max_retry_delay = retry_delay, max_retry_delay
        self.heads: dict[str, int] = {}                          # space → highest seq seen (for `seen`, R-17)
        self.hello_info: dict | None = None
        self._ws = self._reader = None
        self._pending: dict[int, asyncio.Queue] = {}
        self._next = 0
        self._conn_lock, self._flush_lock = asyncio.Lock(), asyncio.Lock()
        self._flushed = False
        self._space_locks: dict[str, asyncio.Lock] = {}

    # ── connection ──
    def _alive(self) -> bool:
        return self._ws is not None and self._reader is not None and not self._reader.done()

    async def _open(self, known_heads=None):
        ws = await ws_connect(self.url, max_size=LIMITS["maxFrameBytes"])
        try:
            ch = json.loads(await asyncio.wait_for(ws.recv(), 10))
            if ch.get("server") != self.server:                   # §3: abort if the challenge names another server
                raise WireError("denied", f"dialed {self.server} but the server is {ch.get('server')}")
            proof = W.hello_proof(self.key, ch["challenge"], self.server)
            args = {"who": self.actor, "keyId": self.key.key_id, "proof": proof, "version": 1}
            if known_heads: args["knownHeads"] = known_heads
            await ws.send(json.dumps({"id": 1, "call": "hello", "args": args}))
            rep = json.loads(await asyncio.wait_for(ws.recv(), 10))
            if "error" in rep: raise WireError.from_wire(rep["error"])
        except BaseException:
            await ws.close(); raise
        self.hello_info, self._next = rep["ok"], 1
        self._ws = ws
        self._reader = asyncio.create_task(self._read_loop(ws))

    async def _read_loop(self, ws):
        try:
            async for raw in ws:
                msg = json.loads(raw)
                if msg.get("id") == 0 and "error" in msg:                  # D2: a frame the server could not attribute; every open request failed
                    for q in list(self._pending.values()): q.put_nowait(_CLOSED)
                    continue
                q = self._pending.get(msg.get("id"))
                if q: q.put_nowait(msg)
        except _NET + (ValueError,): pass
        finally:
            for q in list(self._pending.values()): q.put_nowait(_CLOSED)

    async def _reopen(self):
        async with self._conn_lock:
            if not self._alive(): await self._open()

    async def connect(self, known_heads=None):
        """Connect and authenticate, then finish sending every stored, unconfirmed entry (R-07)."""
        async with self._conn_lock:
            if not self._alive(): await self._open(known_heads)
        await self._ready()

    async def _ready(self):
        async with self._flush_lock:
            if not self._alive(): await self._reopen()
            if not self._flushed:
                for row in self.outbox.pending():
                    try: await self._deliver(row)
                    except WireError: pass                        # refused: recorded in the outbox
                self._flushed = True

    async def close(self):
        if self._reader: self._reader.cancel()
        if self._ws: await self._ws.close()
        self._ws = self._reader = None
        self.outbox.close()

    # ── requests ──
    async def _send(self, call, args) -> tuple[int, asyncio.Queue]:
        if not self._alive(): await self._reopen()
        self._next += 1
        rid, q = self._next, asyncio.Queue()
        self._pending[rid] = q
        try: await self._ws.send(json.dumps({"id": rid, "call": call, "args": args}))
        except BaseException: self._pending.pop(rid, None); raise
        return rid, q

    async def _request(self, call, args):
        rid, q = await self._send(call, args)
        try:
            msg = await q.get()
            if msg is _CLOSED: raise ConnectionError("connection lost")
            if "error" in msg: raise WireError.from_wire(msg["error"])
            return msg["ok"]
        finally: self._pending.pop(rid, None)

    async def _stream(self, call, args):
        """Yield the `item`s of a streaming call until `end`; closing the generator cancels it at the server."""
        rid, q = await self._send(call, args)
        try:
            while True:
                msg = await q.get()
                if msg is _CLOSED: raise ConnectionError("connection lost")
                if "error" in msg: raise WireError.from_wire(msg["error"])
                if msg.get("end"): return
                yield msg["item"]
        finally:
            self._pending.pop(rid, None)
            if self._alive():
                try: await self._ws.send(json.dumps({"id": rid + 1_000_000_000, "call": "cancel", "args": {"target": rid}}))
                except _NET: pass

    def _note(self, space, seq):
        if seq > self.heads.get(space, 0): self.heads[space] = seq

    # ── entries ──
    def make_entry(self, kind, space, body, content=None, refs=(), seen=None):
        """A signed entry by this actor: fills id-relevant fields, nonce, time, contentHash, keyId. `seen` defaults to the
        highest seq of the space this client has seen (R-17)."""
        return make_entry(self.key, self.actor, kind, space, body, content, refs, self.heads.get(space, 0) if seen is None else seen)

    async def append(self, entry: dict, content=None, timeout: float | None = None) -> dict:
        """Store in the outbox, send, retry on a lost connection or `unavailable` with the same entry (R-06, R-07); return the
        receipt. Raises WireError on a definitive refusal (denied, invalid, not_found, moved). On timeout/cancel the entry
        stays in the outbox and is resent after the next `connect()`."""
        await self._ready()                                   # older stored entries first (R-07)
        eid = P.entry_id(entry)
        row = self.outbox.add(eid, entry["space"], entry, content)
        return await asyncio.wait_for(self._deliver(row), timeout)

    async def _deliver(self, row: dict) -> dict:
        eid = row["id"]
        async with self._space_locks.setdefault(row["space"], asyncio.Lock()):    # one at a time per space, in order
            row = self.outbox.get(eid)
            if row["state"] == "done": return row["receipt"]
            if row["state"] == "rejected": raise WireError.from_wire(row["error"])
            args = {"entry": row["entry"]} | ({} if row["content"] is None else {"content": row["content"]})
            delay = self.retry_delay
            while True:
                try:
                    receipt = await self._request("append", args)
                except WireError as ex:
                    if ex.code in ("unavailable", "limit"):
                        await asyncio.sleep(min(self.max_retry_delay, max(delay, (ex.retry_after_ms or 0) / 1000)))
                        delay = min(delay * 2, self.max_retry_delay); continue
                    if ex.code != "moved": self.outbox.rejected(eid, ex.to_wire())     # moved: keep, the caller re-aims the client
                    raise
                except _NET:
                    await asyncio.sleep(delay); delay = min(delay * 2, self.max_retry_delay); continue
                self.outbox.done(eid, receipt); self._note(receipt["space"], receipt["seq"])
                return receipt

    # ── reading ──
    async def read(self, space, after=0, follow=False, state=False, limit=None, known_heads=None, at=None, cursor=None, effect=None):
        """ReadItems after `after`. `state=True, at=N`: the StateView after record N (needs read rights at N). With `follow`
        a backlog cut by `limit` is re-read from the last seq seen, so a follower never skips a record; a lost
        connection ends the stream with an error, as before."""
        args = {"space": space, "after": after} | {k: v for k, v in (("follow", follow or None), ("state", state or None), ("limit", limit),
                                                                       ("knownHeads", known_heads), ("at", at), ("cursor", cursor), ("effect", effect)) if v is not None}
        last, got = after, 0
        async for it in self._stream("read", args):
            if it["type"] == "record":
                seq = it["record"]["receipt"]["seq"]
                if follow and seq > last + 1:                      # a live record after a gap: fill it first
                    async for g in self._fill(space, last, limit, seq):
                        last = g["record"]["receipt"]["seq"]; yield g
                if seq <= last: continue
                last, got = seq, got + 1
                self._note(space, seq)
                yield it
                if follow and limit and got == limit:              # the backlog may have been cut here: catch up now, not at the next live record
                    got = -1
                    async for g in self._fill(space, last, limit, None):
                        last = g["record"]["receipt"]["seq"]; yield g
                continue
            yield it

    async def _fill(self, space, last, limit, below):
        """Re-read records after `last` (below seq `below`, or up to the head) in pages of `limit`; a hole nobody may read ends it (P-02)."""
        while True:
            page = [g async for g in self.read(space, last, limit=limit)
                    if g["type"] == "record" and (below is None or g["record"]["receipt"]["seq"] < below)]
            if not page: return
            for g in page: self._note(space, g["record"]["receipt"]["seq"]); yield g
            last = page[-1]["record"]["receipt"]["seq"]

    async def read_all(self, space, after=0, **kw) -> list:
        return [it async for it in self.read(space, after, **kw)]

    async def subscribe(self, space, after=None):
        """Records and signals of `space` as ReadItems (and notices, on one's own profile). Reconnects and resumes after the
        last record seen, so no record is skipped."""
        last = after
        while True:
            try:
                args = {"space": space} | ({} if last is None else {"after": last})
                async for it in self._stream("subscribe", args):
                    if it["type"] == "record": last = it["record"]["receipt"]["seq"]; self._note(space, last)
                    yield it
                return
            except WireError as ex:
                if ex.code != "unavailable": raise
            except _NET: pass
            await asyncio.sleep(self.retry_delay)
            if last is None: last = self.heads.get(space)

    # ── the transport (T-03, T-05, T-07) ──
    async def signal(self, space, body, relay_from: str | None = None):
        """`relay_from`: a server connection (@:S) signals on behalf of its own actor (G-04); the home accepts it only for S's actors."""
        await self._request("signal", {"space": space, "body": body} | ({} if relay_from is None else {"from": relay_from}))

    async def blob_put(self, data: bytes, type: str = "application/octet-stream") -> dict:
        ref = {"hash": hashlib.sha256(data).hexdigest(), "size": len(data), "type": type}
        offset, part = 0, LIMITS["maxBlobPartBytes"]
        while True:
            res = await self._request("blobPut", {"ref": ref, "offset": offset, "data": W.b64(data[offset:offset + part])})
            if res["complete"]: return ref
            if res["have"] == offset or res["have"] > len(data):      # no progress: the bytes the server holds don't match
                raise WireError("invalid", "upload did not complete")
            offset = res["have"]                         # accepted: next part; refused: resume where the server says

    async def blob_get(self, ref: dict, space: str) -> bytes:
        out = bytearray()
        while True:
            r = await self._request("blobGet", {"ref": ref, "space": space, "offset": len(out)})
            out += W.unb64(r["data"])
            if len(out) >= r["size"]: break
            if not r["data"]: raise WireError("unavailable", "empty range")
        data = bytes(out)
        if hashlib.sha256(data).hexdigest() != ref["hash"] or len(data) != ref["size"]:      # T-03: verify before use
            raise ValueError("blob does not match its reference")
        return data

    async def stream(self, item: dict):
        await self._request("stream", {"item": item})

    async def watch(self, space, session):
        async for it in self._stream("watch", {"space": space, "session": session}):
            yield it

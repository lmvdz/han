"""han-seq over the generic binding (§8.3): one WebSocket per connection, JSON frames per han.ts (RequestFrame / ReplyFrame).
The server speaks first with a Challenge; the client answers with `hello`. Streaming calls (read, subscribe, watch) reply
with `item` frames and finish with `end`; `cancel {target}` stops one. Everything else is han.seq.sequencer."""
from __future__ import annotations
import asyncio, json, logging, os
from websockets.asyncio.server import serve
from websockets.exceptions import ConnectionClosed
from ..core import P, W, LIMITS, WireError

log = logging.getLogger("han.seq")

class DropConnection(Exception):
    """Raised by the `after_append` test hook: close the socket without replying (the receipt is lost in transit)."""

def _pairs(pairs):
    d = {}
    for k, v in pairs:
        if k in d: raise ValueError("duplicate key")
        d[k] = v
    return d

def _bad(_): raise ValueError("not I-JSON")

def _int(s):
    if s.startswith("-0"): raise ValueError("-0")
    return int(s)

def parse(raw: str):
    """I-JSON (RFC 7493) as §2 asks: no duplicate keys, no fractions or exponents, no lone surrogates."""
    v = json.loads(raw, object_pairs_hook=_pairs, parse_float=_bad, parse_constant=_bad, parse_int=_int)
    json.dumps(v, ensure_ascii=False).encode("utf-8")           # raises on a lone surrogate
    return v

class SeqServer:
    def __init__(self, seq, host="127.0.0.1", port=0, after_append=None):
        self.seq, self.host, self.port, self.after_append = seq, host, port, after_append
        self._srv = None

    async def start(self):
        self._srv = await serve(self._handle, self.host, self.port, max_size=LIMITS["maxFrameBytes"])
        self.port = self._srv.sockets[0].getsockname()[1]
        return self

    async def stop(self):
        if self._srv: self._srv.close(); await self._srv.wait_closed()

    @property
    def url(self): return f"ws://{self.host}:{self.port}"

    async def _handle(self, ws):
        conn = {"who": None, "tasks": {}, "subs": set()}
        challenge = W.b64(os.urandom(24))
        async def send(obj):
            try: await ws.send(json.dumps(obj, separators=(",", ":"), ensure_ascii=False))
            except ConnectionClosed: pass
        try:
            await send({"challenge": challenge, "server": self.seq.server, "versions": [1], "time": self.seq.now_str()})
            async for raw in ws:
                if isinstance(raw, bytes): await ws.close(1003, "text frames only"); return
                try:
                    req = parse(raw)
                    rid, call, args = req["id"], req["call"], req.get("args", {})
                    if not (P._int(rid) and isinstance(call, str) and isinstance(args, dict)): raise ValueError("frame")
                except (ValueError, KeyError, TypeError, UnicodeEncodeError, RecursionError):
                    await send({"id": 0, "error": WireError("invalid", "bad frame").to_wire()}); continue
                try:
                    await self._call(ws, conn, send, rid, call, args, challenge)
                except WireError as ex:
                    await send({"id": rid, "error": ex.to_wire()})
                except DropConnection:
                    await ws.close(); return
                except Exception:
                    log.exception("internal error in %s", call)
                    await send({"id": rid, "error": WireError("unavailable", "internal error").to_wire()})
        except ConnectionClosed:
            pass
        finally:
            for t in conn["tasks"].values(): t.cancel()
            for s in conn["subs"]: self.seq.unsubscribe(s)

    async def _call(self, ws, conn, send, rid, call, args, challenge):
        S = self.seq
        if call == "hello":
            if conn["who"]: raise WireError("invalid", "already authenticated")
            ok = S.hello(args.get("who"), args.get("keyId"), args.get("proof"), args.get("version"), challenge, args.get("knownHeads"))
            conn["who"] = args["who"]
            return await send({"id": rid, "ok": ok})
        who = conn["who"]
        if not who: raise WireError("denied", "hello first")
        if call == "cancel":
            t = conn["tasks"].pop(args.get("target"), None)
            if t: t.cancel()
            return await send({"id": rid, "ok": None})
        if call == "append":
            receipt = S.append(args.get("entry"), args.get("content"))
            if self.after_append: await self.after_append(receipt)
            return await send({"id": rid, "ok": receipt})
        if call == "signal":
            S.signal(who, args.get("space"), args.get("body"))
            return await send({"id": rid, "ok": None})
        if call == "blobPut":
            try: data = W.unb64(args["data"])
            except (KeyError, ValueError, TypeError): raise WireError("invalid", "blobPut data")
            return await send({"id": rid, "ok": S.blob_put(who, args.get("ref"), args.get("offset"), data)})
        if call == "blobGet":
            r = S.blob_get(who, args.get("ref"), args.get("space"), args.get("offset", 0), args.get("length"))
            return await send({"id": rid, "ok": {"data": W.b64(r["data"]), "size": r["size"], "offset": r["offset"]}})
        if call == "stream":
            S.stream(who, args.get("item"))
            return await send({"id": rid, "ok": None})
        if call == "read":
            items, sub = S.read(who, args.get("space"), args.get("after"), args.get("follow", False), args.get("state", False),
                                args.get("limit"), args.get("knownHeads"))
        elif call == "subscribe":
            items, sub = S.subscribe(who, args.get("space"), args.get("after"))
        elif call == "watch":
            items, sub = [], S.watch(who, args.get("space"), args.get("session"))
        else:
            raise WireError("invalid", f"unknown call {call}")
        if rid in conn["tasks"]: raise WireError("invalid", "duplicate request id")
        if sub: conn["subs"].add(sub)
        conn["tasks"][rid] = asyncio.create_task(self._drain(send, rid, items, sub, conn))

    async def _drain(self, send, rid, items, sub, conn):
        try:
            for it in items: await send({"id": rid, "item": it})
            if sub is None: return await send({"id": rid, "end": True})
            while True:
                it = await sub.q.get()
                if it is None: return await send({"id": rid, "error": WireError("unavailable", "subscriber too slow; re-read").to_wire()})
                await send({"id": rid, "item": it})
        except asyncio.CancelledError:
            pass
        finally:
            conn["tasks"].pop(rid, None)
            if sub: self.seq.unsubscribe(sub); conn["subs"].discard(sub)

"""The HAN sequencer as a Matrix application service (architecture.md §8.2).

It owns the user @han-seq:<server>, joins the rooms it is invited to, and for each `org.han.entry` event runs the entry through
the §4.1 pipeline (han.seq.Sequencer, unchanged) and answers with `org.han.receipt` or `org.han.reject` related to that event.
A resend gets the same receipt (R-01, the sequencer's dedupe). Records that arrive by another binding (the generic one) are
published to the room too, so the timeline carries the whole log (T-01). It keeps `org.han.space` (T-06), creates and keeps
the watch and read rooms of every live session (T-07), and serves `/_han/v1/...` with HAN authentication.

P-02 and E4 (§8.2): a room carries only `org.han.entry {id}` (from a client), `org.han.entry {id, kind, author, seq}` (from the
sequencer), `org.han.receipt {receipt}` and `org.han.reject`, never a header or a body. A client uploads the whole entry (header and
content) with `POST /_han/v1/entry` before it posts the event, and reads records back with `GET /_han/v1/read`, which enforces P-02; so a
room member who cannot read the log sees ids, kinds, authors and receipts, nothing else.

    python -m han.matrix.appservice --registration REG --homeserver URL --server a.localhost --listen 127.0.0.1:9300 --db seq.db --key key.json"""
from __future__ import annotations
import asyncio, json, logging, os, time
import aiohttp
from aiohttp import web
from ..core import P, W, LIMITS, WireError
from ..seq import Sequencer
from .api import MatrixAPI, MatrixError, register_appservice_user
from .proto import ENTRY, RECEIPT, REJECT, SPACE, SESSION, STREAM, entry_event, relates, service_user, txn_safe
from .store import AsStore

log = logging.getLogger("han.matrix")
CHALLENGE_TTL, MAX_CHALLENGES, MAX_CHALLENGES_PER_CLIENT = 120.0, 4096, 64      # E11: open challenges, in all and per remote address
ENTRY_TTL, MAX_ENTRIES, MAX_ENTRIES_PER_ACTOR, MAX_ENTRIES_PER_CLIENT = 600.0, 1024, 64, 256      # uploaded entries waiting for their event: in all, per uploader, per address
MAX_ENTRY_BYTES_PER_ACTOR = 4 * 1024 * 1024                                # E11: and the bytes one uploader may have waiting
DEFAULT_RETRY_MS = 1000
_RETRY = (MatrixError, aiohttp.ClientError, OSError, asyncio.TimeoutError)      # Matrix is unreachable or erred: let Synapse redeliver

class AppService:
    def __init__(self, seq: Sequencer, homeserver: str, as_token: str, hs_token: str, db_path: str):
        self.seq, self.server = seq, seq.server
        self.user = service_user(self.server)
        self.api = MatrixAPI(homeserver, as_token, as_user=self.user)
        self.hs_token = hs_token
        self.store = AsStore(db_path)
        self.challenges: dict[str, tuple[float, str]] = {}    # challenge -> (expiry, remote address that asked)
        self.gate = asyncio.Event(); self.gate.set()          # test hook: clear it to hold back transaction processing
        self.port = None
        self._locks: dict[str, asyncio.Lock] = {}
        self._tasks: set = set()
        self._dirty: set[str] = set()                         # spaces whose last publish failed
        self._runner = None
        self._pump_task = None
        self.entries: dict[str, tuple[dict, object, float, str, str, int]] = {}     # entry id -> (entry, content, expiry, uploader, address, bytes): uploaded ahead of the event
        seq.add_ordered_hook(self._on_ordered)                # records appended by any binding reach the room (T-01)

    # ── lifecycle ──
    async def start(self, host: str = "127.0.0.1", port: int = 9300) -> "AppService":
        await register_appservice_user(self.api.base, self.api.token, "han-seq")
        app = web.Application(client_max_size=LIMITS["maxFrameBytes"])
        app.add_routes([web.put("/_matrix/app/v1/transactions/{txn}", self._txn),
                        web.get("/_matrix/app/v1/users/{user}", self._ok), web.get("/_matrix/app/v1/rooms/{alias}", self._not_found),
                        web.get("/_han/v1/challenge", self._challenge), web.get("/_han/v1/read", self._read), web.get("/_han/v1/state", self._state),
                        web.post("/_han/v1/entry", self._put_entry), web.put("/_han/v1/blob", self._blob_put), web.get("/_han/v1/blob", self._blob_get)])
        self._runner = web.AppRunner(app, access_log=None); await self._runner.setup()
        site = web.TCPSite(self._runner, host, port); await site.start()
        self.port = site._server.sockets[0].getsockname()[1]
        self._pump_task = asyncio.create_task(self._pump())
        for sp in self.store.spaces(): self._kick(sp)         # catch up: anything ordered while we were down
        return self

    async def stop(self):
        if self._pump_task: self._pump_task.cancel()
        for t in list(self._tasks): t.cancel()
        if self._runner: await self._runner.cleanup()
        await self.api.close()
        self.seq.remove_ordered_hook(self._on_ordered)
        self.store.close()

    # ── publishing the log to the room (T-01) ──
    def _on_ordered(self, entry, content, state, receipt):
        if isinstance(entry.get("space"), str): self._kick(entry["space"])

    def _kick(self, space: str):
        t = asyncio.get_running_loop().create_task(self._flush_safe(space))
        self._tasks.add(t); t.add_done_callback(self._tasks.discard)

    async def _flush_safe(self, space: str):
        try: await self.flush(space)
        except asyncio.CancelledError: raise
        except Exception as ex:
            log.warning("publishing %s failed (%s); will retry", space, ex); self._dirty.add(space)

    async def _pump(self):
        while True:
            await asyncio.sleep(1.0)
            for sp in list(self._dirty):
                self._dirty.discard(sp); self._kick(sp)

    async def flush(self, space: str):
        """Publish every record of `space` not yet in its room (an `{id, kind, author, seq}` entry event if none carried it, then the receipt), then bring the session rooms in line with the state (T-07)."""
        async with self._locks.setdefault(space, asyncio.Lock()):
            room, S = self.store.space_room(space), self.seq.home.spaces.get(space)
            if not room or not S: return
            for rec in S["records"][self.store.published(space):]:
                r, e = rec["receipt"], rec["entry"]
                origin = self.store.origin(r["id"])
                if origin is None:                                     # ordered by another binding: the room still shows that it exists (T-01)
                    origin = await self.api.send(room, ENTRY, entry_event(r["id"], rec), f"en-{r['id']}")
                    self.store.add_event(origin, room, r["id"])
                await self.api.send(room, RECEIPT, {"receipt": r, "m.relates_to": relates(origin)}, f"rc-{txn_safe(origin)}")
                self.store.set_published(space, r["seq"])
            await self.sync_sessions(space, room, S["state"])

    # ── session streams (T-07) ──
    async def sync_sessions(self, space: str, space_room: str, state: dict):
        for sid, sess in state["sessions"].items():
            row = self.store.session(space, sid)
            if row and row["done"]: continue
            live = P.session_access(state, sess["actor"], sid)["watch"]
            if row is None:
                if not live: continue
                row = await self._open_session(space, space_room, sid, sess)
            cands = set(state["roles"]) | {sess["actor"]}
            for kind in ("watch", "read"):
                want = {a for a in cands if not a.startswith("@:") and P.session_access(state, a, sid)[kind]}
                await self._set_members(row[kind], want)
            if not live: self.store.finish_session(space, sid)       # everyone is out of both rooms
        for sid, row in self.store.open_sessions(space).items():
            if sid in state["sessions"]: continue                   # F3(b): a closed session has left the state, and with it everyone leaves its rooms
            for kind in ("watch", "read"): await self._set_members(row[kind], set())
            self.store.finish_session(space, sid)

    def _stream_room_body(self, name: str, actor: str) -> dict:
        return {"name": name, "preset": "private_chat", "visibility": "private",
                "power_level_content_override": {"users": {self.user: 100, actor: 50}, "users_default": 0, "events": {STREAM: 50},
                                                 "events_default": 0, "state_default": 100, "invite": 100, "kick": 100, "ban": 100,
                                                 "redact": 100},
                "initial_state": [{"type": "m.room.history_visibility", "state_key": "", "content": {"history_visibility": "joined"}},
                                  {"type": "m.room.guest_access", "state_key": "", "content": {"guest_access": "forbidden"}},
                                  {"type": "m.room.join_rules", "state_key": "", "content": {"join_rule": "invite"}}]}

    async def _open_session(self, space: str, space_room: str, sid: str, sess: dict) -> dict:
        short = sid[:8]
        watch = await self.api.create_room(self._stream_room_body(f"HAN watch {short}", sess["actor"]))
        read = await self.api.create_room(self._stream_room_body(f"HAN read {short}", sess["actor"]))
        self.store.add_session(space, sid, watch, read)
        await self.api.put_state(space_room, SESSION, {"session": sid, "watch": watch, "read": read}, sid)
        return {"watch": watch, "read": read, "done": False}

    async def _set_members(self, room: str, want: set[str]):
        """Make the room's invited-or-joined set equal `want` (the service user stays)."""
        have = {u for u, m in (await self.api.members(room)).items() if m in ("join", "invite")} - {self.user}
        for u in sorted(want - have):
            try: await self.api.invite(room, u)
            except MatrixError as ex:
                if ex.errcode != "M_FORBIDDEN" or "already in the room" not in ex.message: log.warning("invite %s to %s: %s", u, room, ex)
        for u in sorted(have - want): await self.api.kick(room, u, "HAN: sessionAccess no longer grants this")

    # ── Matrix events in ──
    def _authed(self, request: web.Request) -> bool:
        auth = request.headers.get("Authorization", "")
        return (auth == f"Bearer {self.hs_token}") or request.query.get("access_token") == self.hs_token

    async def _txn(self, request: web.Request):
        if not self._authed(request): return web.json_response({"errcode": "M_FORBIDDEN"}, status=403)
        try: events = (await request.json()).get("events", [])
        except ValueError: return web.json_response({"errcode": "M_BAD_JSON"}, status=400)
        await self.gate.wait()
        for ev in events:
            try: await self.handle(ev)
            except _RETRY as ex:
                log.warning("transaction %s: %s; asking Synapse to redeliver", request.match_info["txn"], ex)
                return web.json_response({"errcode": "M_UNKNOWN"}, status=500)
            except Exception:
                log.exception("event %s dropped", ev.get("event_id"))
        return web.json_response({})

    async def _ok(self, request): return web.json_response({}) if self._authed(request) else web.json_response({"errcode": "M_FORBIDDEN"}, status=403)
    async def _not_found(self, request): return web.json_response({"errcode": "M_NOT_FOUND"}, status=404)

    async def handle(self, ev: dict):
        t, room, sender = ev.get("type"), ev.get("room_id"), ev.get("sender")
        if sender == self.user or not isinstance(room, str): return
        if t == "m.room.member" and ev.get("state_key") == self.user and (ev.get("content") or {}).get("membership") == "invite":
            await self.api.join(room)
        elif t == ENTRY: await self.on_entry(ev)

    async def on_entry(self, ev: dict):
        room, ev_id, c = ev["room_id"], ev["event_id"], ev.get("content") or {}
        eid = c.get("id")
        if not isinstance(eid, str) or not P._id(eid): return await self._reject(room, ev_id, "", "invalid", "an entry event is {id}")
        if set(c) != {"id"}: return await self._reject(room, ev_id, eid, "invalid", "entry events carry only the id; POST /_han/v1/entry first")
        got = self.entries.get(eid)
        if got is not None and got[2] <= time.monotonic(): self.entries.pop(eid, None); got = None
        if got is not None: entry, content = got[0], got[1]
        else:                                                      # not uploaded (or expired): fine if it is already ordered (a resend, R-01)
            at = self._ordered(eid)
            if at is None:
                log.info("entry %s: not uploaded yet (no answer; the client resends, R-06)", eid); return
            entry, content = at["entry"], None
        sp, bound = entry.get("space"), self.store.room_space(room)
        if bound is None:                                          # T-06: the first entry of a room must open its space
            if entry.get("kind") != "space.create" or not isinstance(sp, str):
                return await self._final(room, ev_id, eid, "not_found", "this room is not bound to a space; open it with space.create")
            other = self.store.space_room(sp)
            if other and other != room: return await self._final(room, ev_id, eid, "denied", "that space is bound to another room")
        elif sp != bound:
            return await self._final(room, ev_id, eid, "invalid", f"this room is bound to {bound}")
        try: receipt = self.seq.append(entry, content)        # §4.1: dedupe (R-01), routing, shape, signature, Policy, durable, receipt
        except WireError as ex:
            if ex.code in ("denied", "invalid", "not_found", "moved"): return await self._final(room, ev_id, eid, ex.code, ex.message)
            if ex.code == "unavailable":                          # not final: the client waits `retryAfterMs`, then sends the event again
                return await self._reject(room, ev_id, eid, "unavailable", ex.message, ex.retry_after_ms or DEFAULT_RETRY_MS)
            log.info("entry %s: %s (no answer; the client resends, R-06)", eid, ex); return
        self.entries.pop(eid, None)
        self.store.add_event(ev_id, room, eid)                     # before any await: the ordered hook's flush must find the origin
        if bound is None:
            async with self._locks.setdefault(sp, asyncio.Lock()):    # no receipt before the binding: a client honours receipts only once it has seen org.han.space
                self.store.bind(room, sp)
                try: await self.api.put_state(room, SPACE, {"spaceId": sp, "sequencer": self.user})
                except MatrixError as ex: log.warning("could not set org.han.space in %s (give %s the power to): %s", room, self.user, ex)
        await self.flush(sp)                                       # also answers this event when it carried the record
        if self.store.origin(eid) != ev_id:                        # a resend (R-01): same receipt, related to the new event
            await self.api.send(room, RECEIPT, {"receipt": receipt, "m.relates_to": relates(ev_id)}, f"rc-{txn_safe(ev_id)}")

    def _ordered(self, eid: str):
        """The record with this id if some space of ours has ordered it, else None."""
        for sp, ids in self.seq.ids.items():
            if eid in ids: return self.seq.home.spaces[sp]["records"][ids[eid] - 1]
        return None

    async def _final(self, room, ev_id, entry_id, code, reason):
        """A final refusal: the uploaded entry is not needed any more."""
        self.entries.pop(entry_id, None)
        await self._reject(room, ev_id, entry_id, code, reason)

    async def _reject(self, room, ev_id, entry_id, code, reason, retry_after_ms: int | None = None):
        body = {"id": entry_id, "code": code, "reason": reason} | ({} if retry_after_ms is None else {"retryAfterMs": retry_after_ms})
        await self.api.send(room, REJECT, body | {"m.relates_to": relates(ev_id)}, f"rj-{txn_safe(ev_id)}")

    # ── GET /_han/v1/... ──
    async def _challenge(self, request: web.Request):
        now = time.monotonic()
        for k in [k for k, (t, _) in self.challenges.items() if t < now]: del self.challenges[k]
        who = request.remote or ""
        if len(self.challenges) >= MAX_CHALLENGES or sum(1 for _, w in self.challenges.values() if w == who) >= MAX_CHALLENGES_PER_CLIENT:
            return self._fail(WireError("limit", "too many open challenges", retry_after_ms=DEFAULT_RETRY_MS))       # E11: per client, and in all
        ch = W.b64(os.urandom(24)); self.challenges[ch] = (now + CHALLENGE_TTL, who)
        return web.json_response({"challenge": ch, "server": self.server, "versions": [1], "time": self.seq.now_str()})

    def _authenticate(self, request: web.Request) -> str:
        """`Authorization: HAN <actor> <keyId> <proof>` and `HAN-Challenge: <challenge>`: the proof signs the challenge we issued
        (single use, 120 s)."""
        parts = request.headers.get("Authorization", "").split(" ")
        if len(parts) != 4 or parts[0] != "HAN": raise WireError("denied", "Authorization: HAN <actor> <keyId> <proof> required")
        _, actor, key_id, proof = parts
        now = time.monotonic()
        ch = request.headers.get("HAN-Challenge")
        if self.challenges.get(ch, (0, ""))[0] < now: raise WireError("denied", "HAN-Challenge missing, unknown or expired")
        del self.challenges[ch]                                    # single use, whatever the outcome
        self.seq.hello(actor, key_id, proof, 1, ch)
        return actor

    async def _read(self, request: web.Request):
        try:
            who = self._authenticate(request)
            p = request.query
            def num(k, default=None):
                try: return int(p[k]) if k in p else default
                except ValueError: raise WireError("invalid", f"{k} must be an integer")
            items, _ = self.seq.read(who, p.get("space"), num("after", 0), False, num("limit"), None)
            return web.Response(text=json.dumps({"items": items}, separators=(",", ":"), ensure_ascii=False), content_type="application/json")
        except WireError as ex: return self._fail(ex)

    async def _state(self, request: web.Request):
        """`GET /_han/v1/state?space=&[at=|cursor=|effect=]` -> the `StateReply` `{state, next?}` of the call `state` (§3). Selectors count by presence."""
        try:
            who = self._authenticate(request)
            p = request.query
            sel = {k: p[k] for k in ("at", "cursor", "effect") if k in p}
            if "at" in sel:
                try: sel["at"] = int(sel["at"])
                except ValueError: raise WireError("invalid", "at must be an integer")
            return web.Response(text=json.dumps(self.seq.state(who, p.get("space"), sel), separators=(",", ":"), ensure_ascii=False), content_type="application/json")
        except WireError as ex: return self._fail(ex)

    def _fail(self, ex: WireError):
        status = {"denied": 401, "invalid": 400, "not_found": 404, "limit": 429, "unavailable": 503, "moved": 421}.get(ex.code, 400)
        if status not in (429, 503): return web.json_response({"error": ex.to_wire()}, status=status)
        ms = ex.retry_after_ms or DEFAULT_RETRY_MS                  # E11: 503 (and 429) carry `Retry-After` in seconds and `retryAfterMs` in the body
        return web.json_response({"error": dict(ex.to_wire(), retryAfterMs=ms)}, status=status, headers={"Retry-After": str(max(1, -(-ms // 1000)))})

    async def _put_entry(self, request: web.Request):
        """`POST /_han/v1/entry` (body: `{entry, content?}`, the whole entry), HAN-authenticated: the header and size limits apply. It is held
        ten minutes under the entry's id until the `org.han.entry {id}` event naming it arrives. The event, not this call, is what appends."""
        try:
            who = self._authenticate(request)
            try: b = await request.json()
            except ValueError: raise WireError("invalid", "the body must be JSON")
            if not (isinstance(b, dict) and set(b) <= {"entry", "content"} and isinstance(b.get("entry"), dict)): raise WireError("invalid", "body is {entry, content?}")
            entry, content = b["entry"], b.get("content")
            if not (P.json_ok(entry) and P.json_ok(content)): raise WireError("invalid", "not I-JSON")
            if entry.get("author") != who: raise WireError("denied", "the authenticated actor must be the entry's author")
            size = len(P.jcs(entry))
            if size > LIMITS["maxHeaderBytes"]: raise WireError("invalid", "header too large")
            if content is not None:
                size += len(P.jcs(content))
                if len(P.jcs(content)) > LIMITS["maxContentBytes"]: raise WireError("invalid", "content too large")
            try: eid = P.entry_id(entry)
            except (KeyError, TypeError, ValueError, AttributeError): raise WireError("invalid", "malformed entry")
            now, remote = time.monotonic(), request.remote or ""
            for k in [k for k, v in self.entries.items() if v[2] < now]: del self.entries[k]
            if eid not in self.entries:                                    # E11: bounded per uploader (count and bytes), per address and in all
                mine = [v for v in self.entries.values() if v[3] == who]
                if (len(mine) >= MAX_ENTRIES_PER_ACTOR or sum(v[5] for v in mine) + size > MAX_ENTRY_BYTES_PER_ACTOR
                        or sum(1 for v in self.entries.values() if v[4] == remote) >= MAX_ENTRIES_PER_CLIENT or len(self.entries) >= MAX_ENTRIES):
                    raise WireError("limit", "too many entries waiting for their event", retry_after_ms=DEFAULT_RETRY_MS)
            self.entries[eid] = (entry, content, now + ENTRY_TTL, who, remote, size)
            return web.json_response({"id": eid})
        except WireError as ex: return self._fail(ex)

    async def _blob_put(self, request: web.Request):
        """`PUT /_han/v1/blob {ref, offset, data}` -> {have, complete}: Sequencer.blob_put (G-01) for the authenticated actor."""
        try:
            who = self._authenticate(request)
            try: b = await request.json(); data = W.unb64(b["data"]); ref, offset = b["ref"], b["offset"]
            except (ValueError, KeyError, TypeError): raise WireError("invalid", "blob put body")
            return web.json_response(self.seq.blob_put(who, ref, offset, data))
        except WireError as ex: return self._fail(ex)

    async def _blob_get(self, request: web.Request):
        """`GET /_han/v1/blob?hash=&size=&type=&space=&offset=` -> {data (base64url), size, offset}: Sequencer.blob_get (G-01)."""
        try:
            who = self._authenticate(request)
            p = request.query
            try: ref, offset = {"hash": p["hash"], "size": int(p["size"]), "type": p["type"]}, int(p.get("offset", "0"))
            except (KeyError, ValueError): raise WireError("invalid", "blob get query")
            r = self.seq.blob_get(who, ref, p.get("space"), offset)
            return web.json_response(dict(r, data=W.b64(r["data"])))
        except WireError as ex: return self._fail(ex)

if __name__ == "__main__":
    import sys
    from .appservice_main import main
    sys.exit(main())

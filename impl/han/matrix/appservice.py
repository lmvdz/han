"""The HAN sequencer as a Matrix application service (architecture.md §8.2).

It owns the user @han-seq:<server>, joins the rooms it is invited to, and for each `org.han.entry` event runs the entry through
the §4.1 pipeline (han.seq.Sequencer, unchanged) and answers with `org.han.receipt` or `org.han.reject` related to that event.
A resend gets the same receipt (R-01, the sequencer's dedupe). Records that arrive by another binding (the generic one) are
published to the room too, so the timeline carries the whole log (T-01). It keeps `org.han.space` (T-06), creates and keeps
the watch and read rooms of every live session (T-07), and serves `GET /_han/v1/read` with HAN authentication.

    python -m han.matrix.appservice --registration REG --homeserver URL --server a.localhost --listen 127.0.0.1:9300 --db seq.db --key key.json"""
from __future__ import annotations
import asyncio, json, logging, os, time
import aiohttp
from aiohttp import web
from ..core import P, W, WireError
from ..seq import Sequencer
from .api import MatrixAPI, MatrixError, register_appservice_user
from .proto import ENTRY, RECEIPT, REJECT, SPACE, SESSION, STREAM, relates, service_user, txn_safe
from .store import AsStore

log = logging.getLogger("han.matrix")
CHALLENGE_TTL, MAX_CHALLENGES = 120.0, 4096
_RETRY = (MatrixError, aiohttp.ClientError, OSError, asyncio.TimeoutError)      # Matrix is unreachable or erred: let Synapse redeliver

class AppService:
    def __init__(self, seq: Sequencer, homeserver: str, as_token: str, hs_token: str, db_path: str):
        self.seq, self.server = seq, seq.server
        self.user = service_user(self.server)
        self.api = MatrixAPI(homeserver, as_token, as_user=self.user)
        self.hs_token = hs_token
        self.store = AsStore(db_path)
        self.challenges: dict[str, float] = {}
        self.gate = asyncio.Event(); self.gate.set()          # test hook: clear it to hold back transaction processing
        self.port = None
        self._locks: dict[str, asyncio.Lock] = {}
        self._tasks: set = set()
        self._dirty: set[str] = set()                         # spaces whose last publish failed
        self._runner = None
        self._pump_task = None
        self._append = seq.append
        seq.append = self._append_and_publish                 # records appended by any binding reach the room (T-01)

    # ── lifecycle ──
    async def start(self, host: str = "127.0.0.1", port: int = 9300) -> "AppService":
        await register_appservice_user(self.api.base, self.api.token, "han-seq")
        app = web.Application(client_max_size=2 * 1024 * 1024)
        app.add_routes([web.put("/_matrix/app/v1/transactions/{txn}", self._txn),
                        web.get("/_matrix/app/v1/users/{user}", self._ok), web.get("/_matrix/app/v1/rooms/{alias}", self._not_found),
                        web.get("/_han/v1/challenge", self._challenge), web.get("/_han/v1/read", self._read)])
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
        self.seq.append = self._append
        self.store.close()

    # ── publishing the log to the room (T-01) ──
    def _append_and_publish(self, entry, content=None):
        receipt = self._append(entry, content)
        if isinstance(entry, dict) and isinstance(entry.get("space"), str): self._kick(entry["space"])
        return receipt

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
        """Publish every record of `space` not yet in its room (entry event if none carried it, then the receipt), redact what
        P-12 redacted, then bring the session rooms in line with the state (T-07)."""
        async with self._locks.setdefault(space, asyncio.Lock()):
            room, S = self.store.space_room(space), self.seq.home.spaces.get(space)
            if not room or not S: return
            for rec in S["records"][self.store.published(space):]:
                r, e = rec["receipt"], rec["entry"]
                origin = self.store.origin(r["id"])
                if origin is None:
                    origin = await self.api.send(room, ENTRY, {"entry": e} | ({} if rec["content"] is None else {"content": rec["content"]}),
                                                 f"en-{r['id']}")
                    self.store.add_event(origin, room, r["id"])
                await self.api.send(room, RECEIPT, {"receipt": r, "m.relates_to": relates(origin)}, f"rc-{txn_safe(origin)}")
                if e["kind"] == "redact": await self._redact_events(room, space, e["body"]["target"])
                self.store.set_published(space, r["seq"])
            await self.sync_sessions(space, room, S["state"])

    async def _redact_events(self, room: str, space: str, target: str):
        """P-12 on Matrix: Matrix redaction strips a whole event, header included, so first post a header-only copy of the
        entry (R-04: headers stay), then redact every event that carried the content."""
        n = self.seq.ids.get(space, {}).get(target)
        if not n: return
        header = self.seq.home.spaces[space]["records"][n - 1]["entry"]
        await self.api.send(room, ENTRY, {"entry": header, "redacted": True}, f"rd-{target}")
        for ev in self.store.events_of(target): await self.api.redact(room, ev, f"rx-{txn_safe(ev)}", "HAN redact")

    # ── session streams (T-07) ──
    async def sync_sessions(self, space: str, space_room: str, state: dict):
        for sid, sess in state["sessions"].items():
            row = self.store.session(space, sid)
            if row and row["done"]: continue
            live = P.session_access(state, sess["actor"], sid)["watch"]
            if row is None:
                if not live: continue
                row = await self._open_session(space, space_room, sid, sess)
            cands = set(state["roles"]) | {sess["actor"]} | ({sess["guard"]} if sess.get("guard") else set())
            for kind in ("watch", "read"):
                want = {a for a in cands if not a.startswith("@:") and P.session_access(state, a, sid)[kind]}
                await self._set_members(row[kind], want)
            if not live: self.store.finish_session(space, sid)       # everyone is out of both rooms

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
        elif t == ENTRY and not (ev.get("content") or {}).get("redacted"): await self.on_entry(ev)

    async def on_entry(self, ev: dict):
        room, ev_id, c = ev["room_id"], ev["event_id"], ev.get("content") or {}
        entry, content = c.get("entry"), c.get("content")
        if not isinstance(entry, dict): return await self._reject(room, ev_id, "", "invalid", "no entry in the event")
        try: eid = P.entry_id(entry)
        except Exception: return await self._reject(room, ev_id, "", "invalid", "malformed entry")
        sp, bound = entry.get("space"), self.store.room_space(room)
        if bound is None:                                          # T-06: the first entry of a room must open its space
            if entry.get("kind") != "space.create" or not isinstance(sp, str):
                return await self._reject(room, ev_id, eid, "not_found", "this room is not bound to a space; open it with space.create")
            other = self.store.space_room(sp)
            if other and other != room: return await self._reject(room, ev_id, eid, "denied", "that space is bound to another room")
        elif sp != bound:
            return await self._reject(room, ev_id, eid, "invalid", f"this room is bound to {bound}")
        try: receipt = self._append(entry, content)           # §4.1: dedupe (R-01), shape, profiles, signature, Policy, durable, receipt
        except WireError as ex:
            if ex.code in ("denied", "invalid", "not_found", "moved"): return await self._reject(room, ev_id, eid, ex.code, ex.message)
            log.info("entry %s: %s (no answer; the client resends, R-06)", eid, ex); return
        if bound is None:
            self.store.bind(room, sp)
            try: await self.api.put_state(room, SPACE, {"spaceId": sp, "sequencer": self.user})
            except MatrixError as ex: log.warning("could not set org.han.space in %s (give %s the power to): %s", room, self.user, ex)
        self.store.add_event(ev_id, room, eid)
        await self.flush(sp)                                       # also answers this event when it carried the record
        if self.store.origin(eid) != ev_id:                        # a resend (R-01): same receipt, related to the new event
            await self.api.send(room, RECEIPT, {"receipt": receipt, "m.relates_to": relates(ev_id)}, f"rc-{txn_safe(ev_id)}")

    async def _reject(self, room, ev_id, entry_id, code, reason):
        await self.api.send(room, REJECT, {"id": entry_id, "code": code, "reason": reason, "m.relates_to": relates(ev_id)}, f"rj-{txn_safe(ev_id)}")

    # ── GET /_han/v1/... ──
    async def _challenge(self, request: web.Request):
        now = time.monotonic()
        for k in [k for k, t in self.challenges.items() if t < now]: del self.challenges[k]
        if len(self.challenges) >= MAX_CHALLENGES: return web.json_response({"error": WireError("limit", "too many open challenges").to_wire()}, status=429)
        ch = W.b64(os.urandom(24)); self.challenges[ch] = now + CHALLENGE_TTL
        return web.json_response({"challenge": ch, "server": self.server, "versions": [1], "time": self.seq.now_str()})

    def _authenticate(self, request: web.Request) -> str:
        """`Authorization: HAN <actor> <keyId> <proof>`; the proof signs a challenge we issued (single use). The header does not
        say which one, so an optional `HAN-Challenge` header names it; without it every open challenge is tried (NOTES.md)."""
        parts = request.headers.get("Authorization", "").split(" ")
        if len(parts) != 4 or parts[0] != "HAN": raise WireError("denied", "Authorization: HAN <actor> <keyId> <proof> required")
        _, actor, key_id, proof = parts
        now = time.monotonic()
        named = request.headers.get("HAN-Challenge")
        for ch in ([named] if named else list(self.challenges)):
            if self.challenges.get(ch, 0) < now: continue
            try: self.seq.hello(actor, key_id, proof, 1, ch)
            except WireError: continue
            del self.challenges[ch]; return actor
        raise WireError("denied", "authentication failed")

    async def _read(self, request: web.Request):
        try:
            who = self._authenticate(request)
            p = request.query
            def num(k, default=None):
                try: return int(p[k]) if k in p else default
                except ValueError: raise WireError("invalid", f"{k} must be an integer")
            items, _ = self.seq.read(who, p.get("space"), num("after", 0), False, p.get("state") == "true", num("limit"), None, num("at"))
            return web.Response(text=json.dumps({"items": items}, separators=(",", ":"), ensure_ascii=False), content_type="application/json")
        except WireError as ex:
            status = {"denied": 401, "invalid": 400, "not_found": 404, "limit": 429, "unavailable": 503, "moved": 421}.get(ex.code, 400)
            return web.json_response({"error": ex.to_wire()}, status=status)

if __name__ == "__main__":
    import sys
    from .appservice_main import main
    sys.exit(main())

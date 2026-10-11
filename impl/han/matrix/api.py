"""A small async Matrix client-server API over aiohttp: only what the HAN binding needs (no SDK). One `MatrixAPI` is one identity
(an access token; an application service passes `as_user` to act as its user)."""
from __future__ import annotations
import asyncio, hashlib, hmac, json
from urllib.parse import quote
import aiohttp

def q(s: str) -> str: return quote(s, safe="")

class MatrixError(Exception):
    def __init__(self, status: int, errcode: str, message: str = "", body: dict | None = None):
        super().__init__(f"{status} {errcode}: {message}")
        self.status, self.errcode, self.message, self.body = status, errcode, message, body or {}

class MatrixAPI:
    def __init__(self, base: str, token: str | None = None, as_user: str | None = None):
        self.base, self.token, self.as_user = base.rstrip("/"), token, as_user
        self._http: aiohttp.ClientSession | None = None

    async def close(self):
        if self._http: await self._http.close(); self._http = None

    async def request(self, method: str, path: str, body=None, params: dict | None = None, data: bytes | None = None,
                      ctype: str | None = None, raw: bool = False, timeout: float = 60.0, headers: dict | None = None):
        if self._http is None: self._http = aiohttp.ClientSession()
        params = dict(params or {})
        if self.as_user: params["user_id"] = self.as_user
        headers = ({"Authorization": f"Bearer {self.token}"} if self.token else {}) | (headers or {})
        if ctype: headers["Content-Type"] = ctype
        kw = {"data": data} if data is not None else ({"json": body} if body is not None else {})
        for attempt in range(6):
            async with self._http.request(method, self.base + path, params=params, headers=headers,
                                          timeout=aiohttp.ClientTimeout(total=timeout), **kw) as r:
                payload = await r.read()
                if r.status == 429 and attempt < 5:
                    await asyncio.sleep(min(5.0, json.loads(payload).get("retry_after_ms", 500) / 1000)); continue
                if r.status >= 400:
                    try: j = json.loads(payload)
                    except ValueError: j = {}
                    raise MatrixError(r.status, j.get("errcode", "M_UNKNOWN"), j.get("error", payload[:200].decode("utf-8", "replace")), j)
                return payload if raw else (json.loads(payload) if payload else {})

    # ── identity ──
    async def whoami(self) -> str: return (await self.request("GET", "/_matrix/client/v3/account/whoami"))["user_id"]

    # ── rooms ──
    async def create_room(self, body: dict) -> str: return (await self.request("POST", "/_matrix/client/v3/createRoom", body))["room_id"]
    async def join(self, room: str): await self.request("POST", f"/_matrix/client/v3/join/{q(room)}", {})
    async def invite(self, room: str, user: str): await self.request("POST", f"/_matrix/client/v3/rooms/{q(room)}/invite", {"user_id": user})
    async def kick(self, room: str, user: str, reason: str = ""):
        await self.request("POST", f"/_matrix/client/v3/rooms/{q(room)}/kick", {"user_id": user, "reason": reason})
    async def members(self, room: str) -> dict[str, str]:
        """user → membership (join, invite, leave, ban) for every user the room has a membership event for."""
        r = await self.request("GET", f"/_matrix/client/v3/rooms/{q(room)}/members")
        return {e["state_key"]: e["content"]["membership"] for e in r["chunk"]}

    async def send(self, room: str, etype: str, content: dict, txn: str) -> str:
        """Send a message event; the same `txn` is the same event (Matrix's idempotency), so retries are safe."""
        r = await self.request("PUT", f"/_matrix/client/v3/rooms/{q(room)}/send/{q(etype)}/{q(txn)}", content)
        return r["event_id"]
    async def put_state(self, room: str, etype: str, content: dict, state_key: str = "") -> str:
        r = await self.request("PUT", f"/_matrix/client/v3/rooms/{q(room)}/state/{q(etype)}/{q(state_key)}", content)
        return r["event_id"]
    async def get_state(self, room: str, etype: str, state_key: str = "") -> dict:
        return await self.request("GET", f"/_matrix/client/v3/rooms/{q(room)}/state/{q(etype)}/{q(state_key)}")
    async def redact(self, room: str, event: str, txn: str, reason: str = ""):
        await self.request("PUT", f"/_matrix/client/v3/rooms/{q(room)}/redact/{q(event)}/{q(txn)}", {"reason": reason})
    async def messages(self, room: str, frm: str | None = None, limit: int = 100, direction: str = "b") -> dict:
        """One page of the timeline: {"chunk": [...], "end": token (absent at the end)}."""
        params = {"dir": direction, "limit": limit} | ({"from": frm} if frm else {})
        return await self.request("GET", f"/_matrix/client/v3/rooms/{q(room)}/messages", params=params)

    async def sync(self, since: str | None, timeout_ms: int = 25000, room_filter: dict | None = None) -> dict:
        params = {"timeout": timeout_ms}
        if since: params["since"] = since
        if room_filter: params["filter"] = json.dumps(room_filter)
        return await self.request("GET", "/_matrix/client/v3/sync", params=params, timeout=timeout_ms / 1000 + 30)

    # ── media (authenticated, Matrix 1.11) ──
    async def upload(self, data: bytes, ctype: str = "application/octet-stream") -> str:
        return (await self.request("POST", "/_matrix/media/v3/upload", data=data, ctype=ctype))["content_uri"]
    async def download(self, mxc: str) -> bytes:
        assert mxc.startswith("mxc://"), mxc
        server, media = mxc[6:].split("/", 1)
        return await self.request("GET", f"/_matrix/client/v1/media/download/{q(server)}/{q(media)}", raw=True)

# ── accounts ──
async def register_shared_secret(base: str, secret: str, username: str, password: str = "pw-" + "x" * 12) -> tuple[str, str]:
    """Register a user through Synapse's shared-secret admin endpoint. Returns (user_id, access_token)."""
    api = MatrixAPI(base)
    try:
        nonce = (await api.request("GET", "/_synapse/admin/v1/register"))["nonce"]
        mac = hmac.new(secret.encode(), b"\0".join([nonce.encode(), username.encode(), password.encode(), b"notadmin"]), hashlib.sha1).hexdigest()
        r = await api.request("POST", "/_synapse/admin/v1/register",
                              {"nonce": nonce, "username": username, "password": password, "admin": False, "mac": mac})
        return r["user_id"], r["access_token"]
    finally: await api.close()

async def register_appservice_user(base: str, as_token: str, localpart: str) -> str:
    """Make sure the application service's own user exists (idempotent). Returns the user id."""
    api = MatrixAPI(base, as_token)
    try:
        try: r = await api.request("POST", "/_matrix/client/v3/register", {"type": "m.login.application_service", "username": localpart})
        except MatrixError as ex:
            if ex.errcode != "M_USER_IN_USE": raise
            return None
        return r["user_id"]
    finally: await api.close()

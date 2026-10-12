"""Federation between sequencers (architecture.md §4.3, G-02, G-03, G-04, P-01, R-10). The Sequencer core stays synchronous; this
module does the network part around it: the server asks it (in han.seq.server) to prepare an append or a hello, and it fetches and
verifies whatever the pipeline will need, so that `Sequencer.profile()` can answer from memory.

 * Peer keys: `/.well-known/han` of each peer, over HTTP(S), merged into `seq.server_keys` (windows, `compromised`).
 * Remote profiles: the profile space of an actor on another server is read as `@:me` over a server connection, checked record by
   record with wire.Mirror (receipt signature and window under the peer's keys, hash chain, entry signature, Policy replay), kept
   (SQLite) and re-read once it is 300 s old (R-10). Receipts pin its seq like a local profile.
 * Notices (G-03), blob transfer (G-02), signal relay (G-04): each over one `@:server` connection to the peer (the generic binding)."""
from __future__ import annotations
import asyncio, json, logging, urllib.request
from websockets.exceptions import WebSocketException
from ..client import Client
from ..core import P, W, WireError
from ..entries import fmt_time
from .sequencer import _Chains

log = logging.getLogger("han.seq.federation")
FRESH = 300          # R-10: seconds a fetched profile may be used
KEYS_TTL = 30        # seconds before a peer's key document is fetched again (when a profile or hello needs it)
CALL_TIMEOUT = 15
_NET = (OSError, asyncio.TimeoutError, WebSocketException, WireError, ValueError)

class PeerKeyFault(Exception):
    """A peer's key document changed an already-seen window (§4.3): the peer is not trusted any more."""

def load_peers(path: str) -> dict:
    """han-peers config: a JSON object {"b.localhost": "http://127.0.0.1:8702", ...}: where to fetch each server's /.well-known/han.
    A server not listed is looked up at https://<name> (the real-world rule)."""
    with open(path, encoding="utf-8") as fh: d = json.load(fh)
    if not (isinstance(d, dict) and all(isinstance(k, str) and isinstance(v, str) for k, v in d.items())): raise ValueError("han-peers: {name: url}")
    return d

def _get(url: str):
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))         # a peer is never reached through a proxy
    with opener.open(url, timeout=5) as r: return json.loads(r.read(1 << 20))

class Federation:
    def __init__(self, seq, peers: dict | None = None):
        self.seq, self.peers = seq, {} if peers is None else peers          # live: a caller may add peers later
        self.docs: dict[str, dict] = seq.store.get_meta("peer_keys", {})        # server → {keys: [raw key docs], sequencer, cut: {keyId: time}}
        self.chains = seq.chains = _Chains(seq)                      # verified profile chains of other servers' actors (and spaces handed over, G-06)
        self.fetched: dict[str, float] = {}                                      # actor → steady time its profile was last read
        self.fetched_keys: dict[str, float] = {}
        self.bad: dict[str, str] = {}                                            # server → why it is not trusted
        self.conns: dict[str, Client] = {}
        self.pending: dict[tuple, asyncio.Task] = {}                             # (space, hash) → running G-02 fetch
        self.tasks: set[asyncio.Task] = set()
        self._locks: dict[str, asyncio.Lock] = {}
        for server in self.docs: self._apply(server)
        for rec in seq.store.peer_records():
            ok, why = self.chains.ingest(rec)
            if not ok: raise RuntimeError(f"stored profile record {rec['receipt']['space']}#{rec['receipt']['seq']} no longer verifies: {why}")
        seq.fed, seq.on_ordered = self, self.on_ordered

    async def close(self):
        for t in list(self.tasks): t.cancel()
        for c in list(self.conns.values()):
            try: await c.close()
            except Exception: pass
        self.conns.clear()

    def _lock(self, name) -> asyncio.Lock: return self._locks.setdefault(name, asyncio.Lock())
    def _spawn(self, coro):
        t = asyncio.create_task(coro); self.tasks.add(t); t.add_done_callback(self.tasks.discard); return t
    def _age(self, table: dict, k) -> float: return self.seq.now() - table.get(k, -1e12)

    # ── peer keys: /.well-known/han (§4.3) ──
    def _apply(self, server: str):
        """Rebuild seq.server_keys[server] from the stored document: a `compromised` key is trusted only up to its cut."""
        d, out = self.docs[server], {}
        for k in d["keys"]:
            e = {"publicKey": k["publicKey"], "from": k["from"]}
            until, cut = k.get("until"), d["cut"].get(k["keyId"])
            if cut: until = min(until, cut) if until else cut
            if until: e["until"] = until
            if k.get("compromised"): e["compromised"] = True
            out[k["keyId"]] = e
        self.seq.server_keys[server] = out

    def _cut(self, server: str, key_id: str, from_: str) -> str:
        """The end of trust for a key first seen `compromised`: just after the latest receipt of it that we recorded; none → nothing."""
        latest = None
        for S in self.chains.mirror.spaces.values():
            for rec in S["records"]:
                r = rec["receipt"]
                if r["home"] == server and r["homeKey"] == key_id and (latest is None or r["time"] > latest): latest = r["time"]
        return fmt_time(P.ts(latest) + 1) if latest else from_

    def _merge(self, server: str, doc):
        if not (isinstance(doc, dict) and doc.get("server") == server and isinstance(doc.get("keys"), list)
                and isinstance(doc.get("sequencer"), str) and doc["sequencer"].startswith(("ws://", "wss://"))): raise ValueError("bad /.well-known/han")
        new = {}
        for k in doc["keys"]:
            if not (isinstance(k, dict) and P._keyid(k.get("keyId")) and P._pubkey(k.get("publicKey")) and P._time(k.get("from"))
                    and (k.get("until") is None or P._time(k["until"])) and k.get("compromised") in (None, True) and k["keyId"] not in new):
                raise ValueError("bad key in /.well-known/han")
            new[k["keyId"]] = {f: k[f] for f in ("keyId", "publicKey", "from", "until", "compromised") if k.get(f)}
        old = self.docs.get(server, {"keys": [], "cut": {}})
        for o in old["keys"]:                                                    # a seen window never changes; a key never disappears
            n = new.get(o["keyId"])
            if (not n or n["publicKey"] != o["publicKey"] or n["from"] != o["from"] or (o.get("until") and n.get("until") != o["until"])
                    or (o.get("compromised") and not n.get("compromised"))):
                raise PeerKeyFault(f"{server} changed key {o['keyId']} for a window already seen")
        cut = dict(old["cut"])
        for kid, n in new.items():
            if n.get("compromised") and kid not in cut: cut[kid] = self._cut(server, kid, n["from"])
        self.docs[server] = {"keys": list(new.values()), "sequencer": doc["sequencer"], "cut": cut}
        self._apply(server)
        self.seq.store.set_meta("peer_keys", self.docs); self.seq.store.commit()

    async def ensure_keys(self, server: str, force: bool = False):
        if server in self.bad: raise WireError("unavailable", f"{server} is not trusted: {self.bad[server]}")
        if server in self.docs and self._age(self.fetched_keys, server) < KEYS_TTL and not force: return
        async with self._lock("keys:" + server):
            t0 = self.seq.now()
            if server in self.docs and self.fetched_keys.get(server, -1e12) >= t0 - 1 and not force: return
            try:
                doc = await asyncio.to_thread(_get, self.peers.get(server, f"https://{server}").rstrip("/") + "/.well-known/han")
                self._merge(server, doc)
            except PeerKeyFault as ex:
                self.bad[server] = str(ex); raise WireError("unavailable", str(ex))
            except (OSError, ValueError) as ex:
                raise WireError("unavailable", f"no keys for {server}: {ex}")
            self.fetched_keys[server] = t0

    def server_view(self, actor: str):
        """The synthetic ProfileView of a peer's server actor `@:S` (§4.3): the keys valid now, once their document is fresh."""
        server = actor[2:]
        ks = self.seq.server_keys.get(server)
        if not ks or server in self.bad or self._age(self.fetched_keys, server) > FRESH: return None
        now = self.seq.now_str()
        keys = {k: {"publicKey": v["publicKey"], "status": "valid"} for k, v in ks.items()
                if v["from"] <= now and now < v.get("until", "9999") and not v.get("compromised")}
        return {"actor": actor, "seq": 0, "actorKind": "service", "operatorConfirmed": False, "keys": keys}

    # ── connections ──
    async def conn(self, server: str) -> Client:
        """The authenticated connection to `server` as `@:me`, dialled at the endpoint its .well-known names."""
        c = self.conns.get(server)
        if c and c._alive(): return c
        await self.ensure_keys(server)
        c = Client(self.docs[server]["sequencer"], "@:" + self.seq.server, "", ":memory:", server=server, key=self.seq.key,
                   retry_delay=0.1, max_retry_delay=1.0)
        try: await asyncio.wait_for(c.connect(), CALL_TIMEOUT)
        except BaseException:
            await c.close(); raise
        old = self.conns.get(server)
        self.conns[server] = c
        if old and old is not c: await old.close()
        return c

    async def _remote(self, server: str, what: str, fn):
        """Run fn(client) against `server`; network trouble becomes `unavailable` and drops the connection."""
        try: return await asyncio.wait_for(self._call(server, fn), CALL_TIMEOUT * 2)
        except WireError: raise
        except _NET + (ConnectionError,) as ex:
            self.conns.pop(server, None)
            raise WireError("unavailable", f"{what}: {server}: {type(ex).__name__} {ex}")

    async def _call(self, server, fn): return await fn(await self.conn(server))

    # ── remote profiles (§4.3, P-01, R-10) ──
    def profile(self, actor: str, seq: int | None = None):
        """ProfileView of an actor on another server from the verified copy. Head views (seq None) only while fresh (≤ 300 s)."""
        recs = self.chains.records("profile:" + actor)
        if not recs: return None
        if seq is None:
            if self._age(self.fetched, actor) > FRESH: return None
            seq = len(recs)
        return self.chains.profile(actor, seq) if seq <= len(recs) else None

    def _usable(self, actor: str, key_id: str | None) -> bool:
        v = self.profile(actor)
        return bool(v) and (key_id is None or v["keys"].get(key_id, {}).get("status") == "valid")

    async def ensure_profile(self, actor: str, key_id: str | None = None, via: tuple = ()):
        """Have a fresh, verified view of `actor`. An unknown or revoked-in-cache key makes it re-read at once (never staler)."""
        if self._usable(actor, key_id): return
        t0 = self.seq.now()
        async with self._lock("profile:" + actor):
            if self.fetched.get(actor, -1e12) >= t0 and self._usable(actor, None): return          # someone just read it
            server = P.server_of(actor)
            await self.ensure_keys(server)
            await self._remote(server, f"profile of {actor}", lambda c: self._pull(c, actor, t0, via + (actor,)))

    async def _pull(self, c: Client, actor: str, t0: float, via: tuple):
        sp = "profile:" + actor
        try:
            while True:
                got = [i["record"] async for i in c.read(sp, len(self.chains.records(sp))) if i["type"] == "record"]
                for rec in got:
                    for a, n in rec["receipt"]["profiles"].items():              # the chains of its authors (third servers) first
                        if not a.startswith("@:") and P.server_of(a) != self.seq.server and a not in via                                 and len(self.chains.records("profile:" + a)) < n:
                            await self.ensure_profile(a, via=via)
                    ok, why = self.chains.ingest(rec)
                    if not ok:
                        self.chains.mirror.spaces[sp].pop("faulty", None)           # not a verdict on the peer; just not usable
                        raise WireError("unavailable", f"profile of {actor} does not verify: {why}")
                    self.seq.store.add_peer_record(rec)
                if len(got) < 1000: break
        except WireError as ex:
            if ex.code == "not_found": raise WireError("unavailable", f"no profile for {actor}")
            raise
        finally: self.seq.store.commit()
        if not self.chains.records(sp): raise WireError("unavailable", f"no profile for {actor}")
        self.fetched[actor] = t0

    def read_profile(self, space: str, after: int, limit: int) -> list:
        """`read` of another server's profile space, answered from the verified copy (profile spaces are world-readable)."""
        S = self.chains.mirror.spaces.get(space)
        if not S or not S["records"]: raise WireError("not_found", "no such space")
        return [{"type": "record", "record": r} for r in S["records"][after:]][:limit]

    def profile_state(self, space: str) -> dict:
        """The state of another server's profile space, from the verified copy (`state`)."""
        S = self.chains.mirror.spaces.get(space)
        if not S or not S["records"]: raise WireError("not_found", "no such space")
        return S["state"]

    # ── what the server layer asks before it calls the Sequencer ──
    async def prepare_hello(self, who, key_id):
        if not (isinstance(who, str) and P._actor(who)): return
        if who.startswith("@:"):
            if who[2:] != self.seq.server: await self.ensure_keys(who[2:])
        elif P.server_of(who) != self.seq.server:
            await self.ensure_profile(who, key_id if isinstance(key_id, str) else None)

    async def prepare_append(self, entry):
        """P-01: the profiles of the author and the other relevant actors, when they live on other servers. A profile that cannot
        be had is left out: the pipeline then answers `unavailable` itself (§4.1 step 4)."""
        try:
            S = self.seq.home.spaces.get(entry["space"])
            author, key_id = entry["author"], entry["keyId"]
            state, e = (S["state"] if S else None), dict(entry, id=P.entry_id(entry))
        except (KeyError, TypeError, ValueError, AttributeError): return
        got = set()
        async def have(a):
            if a in got or a.startswith("@:") or P.server_of(a) == self.seq.server: return
            got.add(a)
            try: await self.ensure_profile(a, key_id if a == author and isinstance(key_id, str) else None)
            except WireError as ex: log.info("profile of %s unavailable: %s", a, ex.message)
        try:
            for _ in range(P.MAX_UNCONSUMED_APPROVALS + 4):                  # E3 order: fetch candidates one by one, stop at the first valid key
                before = len(got)
                need = self.seq.pin_actors(state, e)
                for a in need: await have(a)
                if len(got) == before: break
        except (KeyError, TypeError, ValueError, AttributeError): return

    async def prepare_host(self, who, records):
        """G-06: the caller's keys and the profiles its records pin (other servers' actors), so the verification can run from memory."""
        if not (isinstance(who, str) and who.startswith("@:") and isinstance(records, list)): return
        try:
            await self.ensure_keys(who[2:])
            actors = {a for r in records for a in r["receipt"]["profiles"]}
        except (WireError, KeyError, TypeError, AttributeError): return
        for a in sorted(actors):
            if a.startswith("@:") or not P._actor(a) or P.server_of(a) == self.seq.server: continue
            try: await self.ensure_profile(a)
            except WireError as ex: log.info("profile of %s unavailable: %s", a, ex.message)

    async def prepare_read(self, space):
        if isinstance(space, str) and space.startswith("profile:") and P._space(space) and P.server_of(space) != self.seq.server \
                and space not in self.chains.mirror.spaces:
            try: await self.ensure_profile(space[len("profile:"):])
            except WireError: pass

    # ── G-03 notices ──
    def on_ordered(self, entry, content, state, receipt):
        """After a record is ordered: notices to the servers of newly named actors (G-03), blob fetches for its references (G-02)."""
        try: asyncio.get_running_loop()
        except RuntimeError: return
        space, b = entry["space"], entry["body"]
        named = [b["actor"]] if entry["kind"] == "member.set" and b["role"] in P.READ_ROLES else []
        if isinstance(b.get("to"), list): named += [a for a in b["to"] if state["roles"].get(a) in P.READ_ROLES]
        for a in dict.fromkeys(named):
            if P.server_of(a) != self.seq.server: self._spawn(self._notify(a, space, receipt["seq"]))
        if P.server_of(entry["author"]) != self.seq.server:
            for ref in W.blob_refs(entry["kind"], content):
                key = (space, ref["hash"])
                if key not in self.seq.home.supplied and key not in self.pending:
                    self.pending[key] = self._spawn(self._fetch_blob(ref, space, entry["author"]))

    async def _notify(self, actor: str, space: str, seq: int):
        """Send the notice to the actor's server; it is a hint, so a few tries and then give up."""
        for delay in (0, 0.5, 1.5):
            await asyncio.sleep(delay)
            try:
                await self._remote(P.server_of(actor), "notice", lambda c: c.signal("profile:" + actor, {"type": "notice", "space": space, "seq": seq}))
                return
            except WireError as ex:
                if ex.code in ("denied", "invalid", "not_found"): return

    async def verify_notice(self, who: str, space: str, body):
        """A notice is a hint: read the space as `@:me` and drop it unless the actor holds a role there (G-03)."""
        actor = space[len("profile:"):]
        if not (isinstance(body, dict) and set(body) == {"type", "space", "seq"} and body["type"] == "notice" and P._space(body["space"])
                and P._int(body["seq"]) and who[2:] == P.server_of(body["space"]) and P.server_of(actor) == self.seq.server):
            raise WireError("denied", "not a notice this server can use")
        try: st = await self._remote(who[2:], "notice check", lambda c: c.read_state(body["space"]))
        except WireError: raise WireError("denied", "notice cannot be verified")
        if st["roles"].get(actor) not in P.READ_ROLES or st["head"] < body["seq"]: raise WireError("denied", "notice cannot be verified")

    # ── G-04 signal relay ──
    async def relay_signal(self, actor: str, space: str, body):
        await self._remote(P.server_of(space), "signal relay", lambda c: c.signal(space, body, relay_from=actor))

    # ── G-02 blob transfer ──
    async def _fetch_blob(self, ref: dict, space: str, author: str):
        """Ask the author's server, then the home (never ourselves); the client checks the hash; what arrives is held for `space`."""
        try:
            for attempt in range(4):
                for srv in dict.fromkeys([P.server_of(author), P.server_of(space)]):
                    if srv == self.seq.server: continue
                    try: data = await self._remote(srv, "blob", lambda c: c.blob_get(ref, space))
                    except (WireError, ValueError, OSError): continue
                    self.seq.supply(space, ref, data); return
                await asyncio.sleep(0.5 * 2 ** attempt)
            log.warning("blob %s for %s could not be fetched", ref["hash"][:12], space)
        finally: self.pending.pop((space, ref["hash"]), None)

    async def await_blob(self, space, ref):
        t = self.pending.get((space, ref.get("hash") if isinstance(ref, dict) else None))
        if t:
            try: await asyncio.wait_for(asyncio.shield(t), CALL_TIMEOUT)
            except Exception: pass

    async def serve_blob(self, who: str, ref: dict, space: str) -> bytes:
        """blobGet from another server for a space homed elsewhere (G-01, G-02): the home's records say whether `who` may read a
        record that references the blob; we hold the bytes for that space if the record's author uploaded them here, or we fetched them."""
        if not (who.startswith("@:") and who[2:] != self.seq.server and W._blobref(ref) and P._space(space) and P.server_of(space) != self.seq.server):
            raise WireError("denied", "blob not available")
        async def look(c):
            items, after = [], 0
            state = await c.read_state(space)
            while True:
                page = [i async for i in c.read(space, after)]
                items += page
                recs = [i for i in page if i["type"] == "record"]
                if len(recs) < 1000: return state, items
                after = recs[-1]["record"]["receipt"]["seq"]
        state, items = await self._remote(P.server_of(space), "blob check", look)
        recs = [i["record"] for i in items if i["type"] == "record"]
        refs = [r for r in recs if state and not r.get("redacted") and P.can_read(state, who, r["receipt"]["seq"])
                and any(b["hash"] == ref["hash"] for b in W.blob_refs(r["entry"]["kind"], r["content"]))]
        if not refs: raise WireError("denied", "blob not available")
        home = self.seq.home
        if ref["hash"] not in home.blobs or not ((space, ref["hash"]) in home.supplied
                                                 or any(r["entry"]["author"] in home.uploads.get(ref["hash"], ()) for r in refs)):
            raise WireError("unavailable", "blob not held for this space")
        return home.blobs[ref["hash"]]

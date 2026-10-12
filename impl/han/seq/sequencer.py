"""The sequencer core (no network): the §4.1 pipeline over wire.Home plus storage, a steady clock, profile pinning, R-16,
fan-out, blobs, notices and session streams. han.seq.server is only the WebSocket framing around this.
Everything here is synchronous (one event loop, so each call is atomic, like the per-space serial transaction of §4.1)."""
from __future__ import annotations
import asyncio, hashlib, logging, re, time
from ..core import P, W, LIMITS, WireError
from ..entries import fmt_time, make_entry, default_policy
from ..verify.chains import Chains
from .store import Store, BlobStore

log = logging.getLogger("han.seq")
EPOCH = "1970-01-01T00:00:00Z"
PAGED, CURSOR, EFFECT_VIEW = W.PAGED, W.CURSOR, W.EFFECT_VIEW    # H3: every map of a state page, in cursor and fill order; the cursor `<head>.<o1>.….<o7>`; what `state {effect}` and `state {agent}` hold besides the effect
MAX_OPEN_UPLOADS = 16        # E11: unfinished blob uploads one actor may hold at a time
OWN_BLOB_SECONDS = 86400      # G-01 / D3: an uploader may read its own unreferenced blob this long
STREAM_FIELDS = {"output": {"text"}, "status": {"state"}, "context": {"blob", "what"}}      # + space, session, n, time, type (+ status.detail)

class Clock:
    """The two clocks a sequencer reads: `wall` (epoch seconds, may be stepped) and `mono` (steady, counts only forward)."""
    wall = staticmethod(time.time)
    mono = staticmethod(time.monotonic)

class Sub:
    """One live subscription (a read-follow, subscribe or watch); han.seq.server drains `q` onto the socket."""
    def __init__(self, actor, space=None, cutoff=0, signals=False):
        self.actor, self.space, self.cutoff, self.q, self.dead = actor, space, cutoff, asyncio.Queue(), False
        self.signals = signals                  # G-09: signals ride `subscribe`, never `read` with follow
    def push(self, item):
        if self.dead: return
        if self.q.qsize() > 10000: self.dead = True; self.q.put_nowait(None)      # a consumer that doesn't keep up is cut off (it can re-read)
        else: self.q.put_nowait(item)

class Notices(list):
    """An actor's inbox on its profile (G-03): Home appends signals here and we push them to the actor's live subscriptions. Notices are
    signals (F4): never stored and never replayed, so one for an actor with no live subscription is dropped (the list stays empty)."""
    def __init__(self, seq, actor): super().__init__(); self.seq, self.actor = seq, actor
    def append(self, item):
        for s in list(self.seq.notice_subs.get(self.actor, ())): s.push(item)

class Inbox(dict):
    """Home.signal files a profile notice with `inbox.setdefault(actor, []).append(...)`: ours is a fresh `Notices` each time, never kept
    (the live subscriptions are `Sequencer.notice_subs`), so nothing accumulates for actors that are not subscribed."""
    def __init__(self, seq): super().__init__(); self.seq = seq
    def setdefault(self, k, default=None): return Notices(self.seq, k)

def key_list(keys: dict) -> list[dict]:
    """A server's key set as published (§4.3): [{keyId, publicKey, from, until?, compromised?}]."""
    return [{"keyId": k, "publicKey": v["publicKey"], "from": v["from"]} | {f: v[f] for f in ("until", "compromised") if v.get(f)}
            for k, v in keys.items()]

def _invalid(why): return WireError("invalid", why)

class _Chains(Chains):
    """Verified copies of chains this server does not (yet) order: other servers' profile spaces and spaces handed over by `host`
    (G-06). A record there may be authored or pinned by one of OUR actors (an operator's confirmation in an agent's profile space
    on a peer), so views of local actors come from the sequencer itself."""
    def __init__(self, seq):
        self.seq = seq
        super().__init__(seq.server_keys)
    def profile(self, actor, seq):
        if actor == "@:" + self.seq.server or (not actor.startswith("@:") and P.server_of(actor) == self.seq.server):
            return self.seq.profile(actor, seq) or {"actor": actor, "seq": seq, "actorKind": "human", "operatorConfirmed": False, "keys": {}}
        return super().profile(actor, seq)

def wire_state(state: dict) -> dict:
    """The StateView as served (D14): `settled` is present on every claim (a copy; the sequencer's own state is untouched)."""
    return dict(state, effects={pid: dict(e, claims=[dict(c, settled=bool(c.get("settled"))) for c in e["claims"]])
                                for pid, e in state["effects"].items()})

class Sequencer:
    def __init__(self, server: str, key, db_path: str, clock=None, limiter=None):
        self.server, self.key, self.key_id = server, key, key.key_id
        self.clock = clock or Clock()
        self._anchor_wall, self._anchor_mono = self.clock.wall(), self.clock.mono()     # R-03a: a clock anchored at startup...
        self.boot_mono = self._anchor_mono                                              # ...and R-16: when this run began
        self.store = Store(db_path)
        self.server_keys = {server: self._own_keys()}      # {server: {keyId: {publicKey, from, until?, compromised?}}}; peers' keys join via .fed
        self.home = W.Home(server, key, key.key_id, "local", clock=self.now_str, server_keys=self.server_keys,
                            steady=self.clock.mono, limiter=limiter)           # R-16 is Home's, on the steady clock; `limiter(author)` -> retryAfterMs (§4.1 step 6)
        self.home.blobs, self.home.inbox = BlobStore(self.store), Inbox(self)
        self.home.uploads, self.home.supplied = self.store.uploads(), self.store.supplied()
        self.part_refs: dict[tuple, tuple] = {}                                         # (hash, uploader) → (size, type) of the first part (D8)
        self.upload_times = self.store.upload_times()                                   # (hash, actor) → when it completed (D3)
        self.chains = _Chains(self)                                                    # profile chains of peers and spaces being handed over (G-06)
        self._supplied_saved = set(self.home.supplied)
        self.subs: dict[str, set] = {}              # space → live read/subscribe subscriptions
        self.notice_subs: dict[str, set] = {}       # actor → subscriptions to its own profile (notices)
        self.watchers: dict[tuple, set] = {}        # (space, session) → watch subscriptions
        self.ids: dict[str, dict] = {}              # space → {entry id: seq}  (R-01 without a scan)
        self.stream_n: dict[tuple, int] = {}        # (space, session) → last accepted StreamItem n
        self.stream_last: dict[tuple, dict] = {}
        self.broken = False
        self.fed = None                             # han.seq.federation.Federation, when this server talks to peers
        self.on_ordered = None                      # (entry, content, state, receipt) after a record is ordered: federation duties (G-02, G-03)
        self.ordered_hooks: list = []               # more (entry, content, state, receipt) callbacks, added by bindings (add_ordered_hook)
        self._load()

    def add_ordered_hook(self, fn):
        """Call `fn(entry, content, state, receipt)` after every newly ordered record, whichever binding appended it (not on a dedupe hit)."""
        self.ordered_hooks.append(fn)

    def remove_ordered_hook(self, fn):
        if fn in self.ordered_hooks: self.ordered_hooks.remove(fn)

    # ── clock (R-03a) ──
    def now(self) -> float:
        """Steady, slewed: the wall clock read once at startup plus elapsed steady time. A later wall step has no effect."""
        return self._anchor_wall + (self.clock.mono() - self._anchor_mono)
    def now_str(self) -> str: return fmt_time(self.now())

    # ── this server's keys (§4.3): a window per key, published at /.well-known/han ──
    def _own_keys(self) -> dict:
        ks = self.store.get_meta("own_keys", {})
        cur = ks.get(self.key_id)
        if cur and (cur.get("until") or cur.get("compromised") or cur["publicKey"] != self.key.pub):
            raise RuntimeError(f"server key {self.key_id} is retired or belongs to another key; start with the current key file")
        ks.setdefault(self.key_id, {"publicKey": self.key.pub, "from": EPOCH})
        self.store.set_meta("own_keys", ks); self.store.commit()
        return ks

    def rotate_key(self, key):
        """Sign from now on with `key` (a new keyId); the old key's window ends now. The caller keeps `key` in the key file."""
        ks = self.server_keys[self.server]
        if key.key_id in ks: raise ValueError("keyIds are single-use")
        now = self.now_str()
        ks[self.key_id]["until"] = now; ks[key.key_id] = {"publicKey": key.pub, "from": now}
        self.key, self.key_id, self.home.key, self.home.key_id = key, key.key_id, key, key.key_id
        self.store.set_meta("own_keys", ks); self.store.commit()

    def mark_compromised(self, key_id: str):
        """Publish `compromised` for a retired key (peers then trust only the receipts they recorded before they saw the mark)."""
        if key_id == self.key_id: raise ValueError("rotate away from the key first")
        self.server_keys[self.server][key_id]["compromised"] = True
        self.store.set_meta("own_keys", self.server_keys[self.server]); self.store.commit()

    def wellknown(self, endpoint: str) -> dict:
        """The document served at /.well-known/han (§4.3)."""
        return {"server": self.server, "sequencer": endpoint, "keys": key_list(self.server_keys[self.server])}

    def peers_doc(self) -> dict:
        """The document at /.well-known/han-peers (D13): the other servers whose keys this one verifies with, and the
        /.well-known/han document of each (where the peers map puts it, else https://<server>)."""
        peers = self.fed.peers if self.fed else {}
        return {"peers": [{"server": s, "url": peers.get(s, f"https://{s}").rstrip("/") + "/.well-known/han"}
                          for s in sorted(self.server_keys) if s != self.server]}

    # ── startup: reload the chain, check it, rebuild state ──
    def _load(self):
        for sp in self.store.spaces(): self._load_space(sp)

    def _load_space(self, sp):
        recs, state, prev = [], None, None
        for rec, pinned in self.store.records(sp):
            e, r = rec["entry"], rec["receipt"]
            eid = P.entry_id(e)
            if not (r["seq"] == len(recs) + 1 and r["id"] == eid and r["prev"] == (W.receipt_hash(prev) if prev else "")
                    and W.verify_receipt(r, self.server_keys)):
                raise RuntimeError(f"stored chain of {sp} is corrupt at seq {r['seq']}; refusing to start")
            state = P.fold(state, {"entry": dict(e, id=eid), "content": None, "receipt": r}, {"time": r["time"], "profiles": pinned})
            recs.append(rec); prev = r
            self.ids.setdefault(sp, {})[eid] = r["seq"]
            if e["kind"] == "space.move":
                if e["body"]["to"] == self.server: self.home.moved.pop(sp, None)        # handed to us (G-06): we are the home again
                else: self.home.moved[sp] = e["body"]["to"]
        self.home.spaces[sp] = {"records": recs, "state": state}

    def close(self): self.store.close()

    # ── identity: profile pinning (P-01, R-10) — profiles are spaces on this server ──
    def profile(self, actor: str, seq: int | None = None):
        """ProfileView of `actor` at profile seq `seq` (default: now), or None if this server can't produce it."""
        if actor.startswith("@:"):
            if actor[2:] != self.server: return self.fed.server_view(actor) if self.fed else None
            return {"actor": actor, "seq": 0, "actorKind": "service", "operatorConfirmed": False,
                    "keys": {self.key_id: {"publicKey": self.key.pub, "status": "valid"}}}
        if P.server_of(actor) != self.server: return self.fed.profile(actor, seq) if self.fed else None     # a verified copy of the other server's chain
        S = self.home.spaces.get("profile:" + actor)
        if not S: return None
        st = S["state"]
        if seq is not None and seq != st["head"]:                          # an older view: fold the profile space through `seq`
            st = None
            for rec in S["records"][:seq]:
                st = P.fold(st, {"entry": dict(rec["entry"], id=rec["receipt"]["id"]), "content": None, "receipt": rec["receipt"]},
                            {"time": rec["receipt"]["time"], "profiles": {}})
            if st is None: return None
        p = st["profile"]
        v = {"actor": actor, "seq": st["head"], "actorKind": p["actorKind"], "operatorConfirmed": p["operatorConfirmed"],
             "keys": {k: dict(x) for k, x in p["keys"].items()}}
        if p.get("operator"): v["operator"] = p["operator"]
        return v

    def register(self, actor: str, kind: str, key_id: str, public_key: str, operator: str | None = None):
        """Create `profile:<actor>` (the server actor appends it, §4.3). Idempotent. An agent's operator then confirms
        with its own member.set in that space."""
        sp = "profile:" + actor
        if sp in self.home.spaces: return self.home.spaces[sp]["records"][0]["receipt"]
        prof = {"actorKind": kind, "key": {"keyId": key_id, "publicKey": public_key}}
        if operator: prof["operator"] = operator
        e, c = make_entry(self.key, "@:" + self.server, "space.create", sp,
                          {"policy": default_policy(visibility="public"), "profile": prof}, {"name": actor}, time=self.now_str())
        return self.append(e, c)

    # ── hello (§3) ──
    def hello(self, who, key_id, proof, version, challenge, known_heads=None):
        """D11: an actor this server has no profile for is `not_found`; an unknown or revoked key or a bad proof is `denied`."""
        if version != 1: raise _invalid("unsupported version")                                        # R-08
        if not (isinstance(who, str) and P._actor(who) and isinstance(key_id, str) and isinstance(proof, str)): raise _invalid("hello args")
        view = self.profile(who)
        if view is None: raise WireError("not_found", "no such actor")
        k = view["keys"].get(key_id)
        if not k or k["status"] != "valid" or not W.verify_hello(k["publicKey"], proof, challenge, self.server):
            raise WireError("denied", "authentication failed")
        return {"limits": dict(LIMITS), "durability": self.home.durability, "time": self.now_str(), "knownHeads": self._heads_for(who, known_heads or [])}

    def _heads_for(self, who, known):
        """R-15: the latest receipt of each space the asker named and may read."""
        if not isinstance(known, list) or len(known) > LIMITS["maxFollows"]: raise _invalid("knownHeads")
        out, seen = [], set()
        for h in known:
            sp = h.get("space") if isinstance(h, dict) else None
            S = self.home.spaces.get(sp) if isinstance(sp, str) else None
            if S and sp not in seen and P.can_read(S["state"], who, S["state"]["head"]):
                seen.add(sp); out.append(S["records"][-1]["receipt"])
        return out

    # ── routing (D1) ──
    def route(self, space):
        """A server answers every call naming a space it does not host with `moved` and `movedTo` = the server that does: the one
        that took it over, or the one named in the space id. (Callers that follow with `not_found` for a missing local space.)
        Profile spaces of other servers' actors are served from the verified copy when federating."""
        if not (isinstance(space, str) and P._space(space)): return
        to = self.home.moved.get(space)
        if to: raise WireError("moved", "this space moved", moved_to=to)
        if space in self.home.spaces or P.server_of(space) == self.server: return
        if self.fed and space.startswith("profile:"): return
        raise WireError("moved", "this server does not host the space", moved_to=P.server_of(space))

    # ── append: §4.1 ──
    def append(self, entry, content=None):
        if self.broken: raise WireError("unavailable", "storage failure; restart")
        if not isinstance(entry, dict): raise _invalid("entry")
        try:
            eid = P.entry_id(entry)
            sp = entry.get("space")
            hit = self.ids.get(sp, {}).get(eid) if isinstance(sp, str) else None
            if hit: return self.home.spaces[sp]["records"][hit - 1]["receipt"]           # 1 dedupe, before any other check (R-01)
            self.route(sp)                                                               # 2 not home: moved (D1), for space.create too
            err = W.validate_header(entry)                                               # 3 shape and size (Home repeats it)
            if err: raise _invalid(err)
            if len(P.jcs(entry)) > LIMITS["maxHeaderBytes"]: raise _invalid("header too large")
            if content is not None and len(P.jcs(content)) > LIMITS["maxContentBytes"]: raise _invalid("content too large")
            author = self.profile(entry["author"])                                       # 4 the author's profile (pinned or cached), then the signature
            if author is None: raise WireError("unavailable", "profile")               # the only `unavailable` before Policy: no verdict without it
            if entry["keyId"] not in author["keys"]: raise WireError("denied", "no valid key")      # R-12: nothing to verify against
            if not W.verify_entry(entry, author): raise _invalid("signature")
            S = self.home.spaces.get(sp)
            state = S["state"] if S else None
            now = self.now_str()                                                         # read once: the pin loop, Policy's ctx.time and the receipt time (§4.1 step 4(c))
            need = self.pin_actors(state, dict(entry, id=eid), now=now)                  # the profiles to pin (all of P-01)
        except WireError: raise
        except (TypeError, KeyError, ValueError, AttributeError, RecursionError) as ex:
            raise _invalid(f"malformed: {type(ex).__name__}")
        profiles = {a: v for a in need if (v := self.profile(a))}
        before = len(S["records"]) if S else 0
        try:
            code, res = self.home.append(entry, content, profiles, now=now)              # 5 Policy, 6 rate limit, 7 the moment-dependent `unavailable`
        except (TypeError, KeyError, ValueError, AttributeError) as ex:                 # a shape the validators let through: invalid, not a crash
            raise _invalid(f"malformed: {type(ex).__name__}")
        if code != "ok":
            if code == "moved": raise WireError("moved", "this space moved", moved_to=res)
            if code == "unavailable" and isinstance(res, str) and res.startswith("clock behind"):
                last = P.ts(S["records"][-1]["receipt"]["time"])
                raise WireError("unavailable", res, retry_after_ms=max(1000, int((last - self.now() + 1) * 1000)))
            if isinstance(res, dict):                                                     # R-16, a rate limit: {reason, retryAfterMs}
                raise WireError(code, res.get("reason", ""), retry_after_ms=res.get("retryAfterMs"))
            raise WireError(code, res if isinstance(res, str) else str(res))
        S = self.home.spaces[sp]
        if len(S["records"]) == before: return res
        rec = S["records"][-1]
        try:                                                                              # 8 durable before the receipt leaves (R-03)
            self.store.add_record(rec, {a: profiles[a] for a in res["profiles"]})
            if entry["kind"] == "redact":
                old = next(x for x in S["records"] if x["receipt"]["id"] == entry["body"]["target"])
                self.store.mark_redacted(sp, old["receipt"]["id"], old["redactedBy"])
            for sh in self.home.supplied - self._supplied_saved: self.store.add_supplied(*sh)
            self._supplied_saved = set(self.home.supplied)
            self.store.commit()
        except Exception:
            self.broken = True                                                            # memory is ahead of disk: stop serving
            raise WireError("unavailable", "storage failure; restart")
        self.ids.setdefault(sp, {})[eid] = res["seq"]
        st = S["state"]
        for sub in list(self.subs.get(sp, ())):                                           # 9 fan out (T-01), P-02 per record
            if res["seq"] > sub.cutoff and P.can_read(st, sub.actor, res["seq"]): sub.push({"type": "record", "record": rec})
        for hook in ([self.on_ordered] if self.on_ordered else []) + list(self.ordered_hooks):
            try: hook(entry, content, st, res)
            except Exception: log.exception("ordered hook failed")        # the record is durable and receipted; a hook cannot undo it
        return res

    def pin_actors(self, state, entry, fetch=None, now=None) -> list[str]:
        """P-01 (M3): Policy's relevant_actors is the whole pin rule; this only fetches. Call it with the views fetched so far, fetch what
        it names that was not tried yet, and call again until nothing new (three rounds at most). The last list is what the receipt pins;
        an actor in it that could not be fetched makes the entry `unavailable` (the caller). `fetch(actor)` gives a ProfileView or None (default: self.profile).
        `now` is the append's one clock reading, which Policy then gets as ctx.time and the receipt carries (§4.1 step 4(c)); default: read it here."""
        fetch = fetch or self.profile
        now = self.now_str() if now is None else now
        fetched, tried = {}, set()
        need = []
        for _ in range(3):
            need = P.relevant_actors(state, entry, {"time": now, "profiles": fetched})
            new = [a for a in need if a not in tried]
            if not new: break
            tried.update(new)
            fetched.update({a: v for a in new if (v := fetch(a))})
        return need

    # ── read (§3) ──
    def _state_at(self, S, at):
        """The SpaceState after record `at`: the chain folded through `at` with the profile views its receipts pinned."""
        st = None
        for rec in S["records"][:at]:
            r = rec["receipt"]
            pinned = {a: self.profile(a, n) for a, n in r["profiles"].items()}
            st = P.fold(st, {"entry": dict(rec["entry"], id=r["id"]), "content": None, "receipt": r}, {"time": r["time"], "profiles": pinned})
        return st

    def read(self, who, space, after, follow=False, limit=None, known_heads=None):
        """→ (items, sub or None). `sub` is registered atomically with the backlog, so a follower misses nothing. Entries only (§3, G-09):
        the state is the call `state`. A reader who may read no seq of the space gets `not_found`, as for a missing space."""
        if not (isinstance(space, str) and P._space(space) and P._int(after)): raise _invalid("read args")
        self.route(space)
        limit = 1000 if limit is None else limit
        if not (P._int(limit) and 1 <= limit <= 1000): raise _invalid("limit")
        S = self.home.spaces.get(space)
        if not S and self.fed and space.startswith("profile:") and P.server_of(space) != self.server:
            return self.fed.read_profile(space, after, limit), None                    # another server's profile, as verified here (§4.3)
        if not S or not self.home.readable_any(S["state"], who): raise WireError("not_found", "no such space")
        st = S["state"]
        code, items = self.home.read(space, after, who, limit)
        if code != "ok": raise WireError(code, items or "")
        recs = [i for i in items if i["type"] == "record"]
        out = ([{"type": "heads", "heads": self._heads_for(who, known_heads)}] if known_heads is not None else []) + recs[:limit]
        sub = None
        if follow:
            sub = Sub(who, space, st["head"]); self.subs.setdefault(space, set()).add(sub)
        return out, sub

    def state(self, who, space, sel: dict) -> dict:
        """§3 `state`: one page of the StateView as `{state, next?}`. `sel` holds the selectors that are PRESENT in the request, whatever their
        values (`at`, `cursor`, `effect`, `agent`); `at` may join `agent` only, any other two or more is `invalid` by presence. The paging (H3) is wire.Home's, whose limits are set from LIMITS
        for this call (test mode lowers maxFrameBytes). A page that cannot be cut is impossible by construction, so it is asserted."""
        if not (isinstance(space, str) and P._space(space)): raise _invalid("state args")
        self.route(space)
        S = self.home.spaces.get(space)
        remote = not S and self.fed and space.startswith("profile:") and P.server_of(space) != self.server
        if remote:                                                                     # another server's profile, as verified here (§4.3)
            st, view_at = self.fed.profile_state(space), None
        elif not S: raise WireError("not_found", "no such space")
        else: st, view_at = S["state"], lambda n: self._state_at(S, n)
        if sum(k in sel for k in ("cursor", "effect", "agent")) > 1 or ("at" in sel and ("cursor" in sel or "effect" in sel)):
            raise _invalid("at, cursor, effect and agent exclude each other (at may join agent)")        # by presence, before any value is looked at
        at, cursor, effect, agent = (sel.get(k) for k in ("at", "cursor", "effect", "agent"))
        offs, where = dict.fromkeys(PAGED, 0), st["head"]
        if "at" in sel:
            if isinstance(at, bool) or not isinstance(at, int) or not 1 <= at <= st["head"]: raise _invalid("at must be 1 <= at <= head")
            where = at
        if "cursor" in sel:
            if not (isinstance(cursor, str) and CURSOR.fullmatch(cursor)): raise _invalid("cursor")
            nums = list(map(int, cursor.split(".")))
            if not 1 <= nums[0] <= st["head"]: raise _invalid("cursor")
            where, offs = nums[0], dict(zip(PAGED, nums[1:]))
        if "effect" in sel and not isinstance(effect, str): raise _invalid("effect must be an EntryId")
        if "agent" in sel and not P._actor(agent): raise _invalid("agent must be an ActorId")
        if remote:
            if where != st["head"]: raise _invalid("at and cursor are for spaces this server orders")
        elif not P.can_read(st, who, where):
            if self.home.readable_any(st, who): raise WireError("denied", "state needs read rights at " + ("the head" if where == st["head"] else "that seq"))
            raise WireError("not_found", "no such space")
        view = wire_state(st if where == st["head"] else view_at(where))
        view = {k: v for k, v in view.items() if k != "entries"}
        if "effect" in sel:
            if effect not in view["effects"]: raise WireError("not_found", "no such effect")
            one = dict({k: view[k] for k in EFFECT_VIEW}, effects={effect: view["effects"][effect]})
            assert len(P.jcs({"id": W.MAX_INT, "ok": {"state": one}})) <= LIMITS["maxFrameBytes"], "one effect does not fit maxFrameBytes (H3)"
            return {"state": one}
        if "agent" in sel:
            one = dict({k: view[k] for k in EFFECT_VIEW}, **W.agent_facts(view, agent))
            assert len(P.jcs({"id": W.MAX_INT, "ok": {"state": one}})) <= LIMITS["maxFrameBytes"], "the agent view does not fit maxFrameBytes (H3)"
            return {"state": one}
        if any(offs[p] > len(W._items(view, p)) for p in PAGED): raise _invalid("cursor offset beyond the map")
        self.home.max_frame_bytes, self.home.max_state_items = LIMITS["maxFrameBytes"], LIMITS["maxStateItems"]
        page, offs, more = self.home._page(view, offs)
        out = {"state": page}
        if more: out["next"] = f"{where}." + ".".join(str(offs[p]) for p in PAGED)
        return out

    def subscribe(self, who, space, after=None):
        """T-01/T-05 for `who`: records and signals of `space` (and, on one's own profile, notices, live only: F4). Live from the head,
        or from after `after` first."""
        if not (isinstance(space, str) and P._space(space)) or (after is not None and not P._int(after)): raise _invalid("subscribe args")
        self.route(space)
        S = self.home.spaces.get(space)
        if not S and not space.startswith("profile:"): raise WireError("not_found", "no such space")
        if not S or not P.can_read(S["state"], who, S["state"]["head"]): raise WireError("denied", "cannot follow this space")
        st = S["state"]
        items = [{"type": "record", "record": r} for r in S["records"][after:] if P.can_read(st, who, r["receipt"]["seq"])] if after is not None else []
        sub = Sub(who, space, st["head"], signals=True); self.subs.setdefault(space, set()).add(sub)
        if space == "profile:" + who:                                                   # G-03/G-04: only the actor gets its notices
            self.notice_subs.setdefault(who, set()).add(sub)
        return items, sub

    def unsubscribe(self, sub):
        for table in (self.subs, self.notice_subs, self.watchers):
            for s in table.values(): s.discard(sub)

    # ── signals (G-04) ──
    def signal(self, who, space, body):
        if not (isinstance(space, str) and P._space(space)) or not P.json_ok(body) or len(P.jcs(body)) > 65536: raise _invalid("signal args")
        self.route(space)
        code, detail = self.home.signal(space, who, body)
        if code != "ok": raise WireError(code, detail if isinstance(detail, str) else "")
        st = self.home.spaces[space]["state"]
        for sub in list(self.subs.get(space, ())):
            if sub.signals and sub.actor != who and P.can_read(st, sub.actor, st["head"]): sub.push({"type": "signal", "from": who, "body": body})

    # ── blobs (G-01, G-02) ──
    def blob_put(self, who, ref, offset, data):
        if not (W._blobref(ref) and P._int(offset) and isinstance(data, bytes)) or len(data) > LIMITS["maxBlobPartBytes"]:
            raise _invalid("blobPut args")
        if offset + len(data) > ref["size"]: raise _invalid("bytes beyond ref.size")
        if who in self.home.uploads.get(ref["hash"], ()):                                # this uploader already completed it
            return {"have": ref["size"], "complete": True}
        key = (ref["hash"], who)
        if key not in self.home.parts and sum(1 for k in self.home.parts if k[1] == who) >= MAX_OPEN_UPLOADS:
            raise WireError("limit", "too many uploads in progress", retry_after_ms=1000)             # E11: per client
        if self.part_refs.setdefault(key, (ref["size"], ref["type"])) != (ref["size"], ref["type"]): raise _invalid("ref.size and ref.type must repeat")
        buf = self.home.parts.setdefault(key, bytearray())
        if offset > len(buf): raise _invalid(f"gap: part starts after the bytes held ({len(buf)})")       # D8: parts are contiguous
        over = bytes(buf[offset:offset + len(data)])
        if data[:len(over)] != over: raise _invalid("part differs from bytes already held")
        buf.extend(data[len(over):])
        if len(buf) < ref["size"]: return {"have": len(buf), "complete": False}                        # a valid partial upload
        self.home.parts.pop(key, None); self.part_refs.pop(key, None)
        if hashlib.sha256(bytes(buf)).hexdigest() != ref["hash"]: raise _invalid("bytes do not hash to ref.hash")      # parts discarded
        self.home.blobs[ref["hash"]] = bytes(buf); self.home.uploads.setdefault(ref["hash"], set()).add(who)
        self.upload_times[key] = self.now()
        self.store.add_upload(ref["hash"], who, self.upload_times[key]); self.store.commit()
        return {"have": ref["size"], "complete": True}

    def supply(self, space, ref, data: bytes):
        """G-02: bytes fetched from another server and already hash-checked are held for `space` from now on."""
        self.home.blobs[ref["hash"]] = data
        self.home.supplied.add((space, ref["hash"])); self._supplied_saved.add((space, ref["hash"]))
        self.store.add_supplied(space, ref["hash"]); self.store.commit()

    def blob_get(self, who, ref, space, offset=0, length=None):
        length = LIMITS["maxBlobRangeBytes"] if length is None else length
        if not (W._blobref(ref) and isinstance(space, str) and P._space(space) and P._int(offset) and P._int(length) and length >= 1):
            raise _invalid("blobGet args")
        self.route(space)
        code, data = self.home.blob_get(ref, space, who)
        if code != "ok" and self._context_blob(who, ref, space): code, data = "ok", self.home.blobs[ref["hash"]]
        if code != "ok" and self._own_blob(who, ref, space): code, data = "ok", self.home.blobs[ref["hash"]]
        if code != "ok": raise WireError("not_found" if code == "denied" else code, "blob not available")       # never confirm that a blob exists
        return self.blob_range(data, offset, length)

    def _own_blob(self, who, ref, space) -> bool:
        """D3, G-01: an uploader may read its own completed upload for 24 hours whether or not a record references it, with `space`
        any space it can read."""
        S = self.home.spaces.get(space)
        at = self.upload_times.get((ref["hash"], who))
        return bool(S and at is not None and self.now() - at < OWN_BLOB_SECONDS and ref["hash"] in self.home.blobs
                    and P.can_read(S["state"], who, S["state"]["head"]))

    @staticmethod
    def blob_range(data: bytes, offset: int, length: int) -> dict:
        if offset > len(data): raise _invalid("offset past the end")
        return {"data": data[offset:offset + min(length, LIMITS["maxBlobRangeBytes"])], "size": len(data), "offset": offset}

    def _context_blob(self, who, ref, space) -> bool:
        """A `context` StreamItem's blob is served to actors holding the read right on its live session (T-07, G-05); the
        bytes must have been uploaded by the session's actor, who supplies them."""
        S = self.home.spaces.get(space)
        if not S or ref["hash"] not in self.home.blobs: return False
        st = S["state"]
        for sid, sess in st["sessions"].items():
            if sess["actor"] not in self.home.uploads.get(ref["hash"], ()) or not P.session_access(st, who, sid)["read"]: continue
            if any(i["type"] == "context" and i["blob"]["hash"] == ref["hash"] for i in self.store.stream_items(space, sid)): return True
        return False

    # ── session streams (G-05) ──
    def _session(self, space, session):
        S = self.home.spaces.get(space) if isinstance(space, str) else None
        if not (S and isinstance(session, str) and session in S["state"]["sessions"]): raise WireError("not_found", "no such session")
        return S["state"], S["state"]["sessions"][session]

    def _valid_item(self, item) -> bool:
        if not (isinstance(item, dict) and item.get("type") in STREAM_FIELDS): return False
        t = item["type"]
        if set(item) - {"detail"} != {"space", "session", "n", "time", "type"} | STREAM_FIELDS[t] or ("detail" in item and t != "status"): return False
        return (P._space(item["space"]) and P._id(item["session"]) and P._int(item["n"]) and item["n"] >= 1 and P._time(item["time"])
                and (t != "output" or isinstance(item["text"], str))
                and (t != "status" or (item["state"] in ("idle", "working", "blocked", "done", "error") and isinstance(item.get("detail", ""), str)))
                and (t != "context" or (W._blobref(item["blob"]) and item["what"] in ("transcript", "files", "plan"))))

    def stream(self, who, item):
        if not self._valid_item(item) or len(P.jcs(item)) > LIMITS["maxStreamItemBytes"]: raise _invalid("StreamItem")
        space, session = item["space"], item["session"]
        self.route(space)
        st, sess = self._session(space, session)
        if sess["actor"] != who: raise WireError("denied", "only the session's actor publishes")
        if not P.session_access(st, who, session)["watch"]: raise WireError("denied", "session not live")
        key = (space, session)
        last = self.stream_n.get(key)
        if last is None: last = self.stream_n[key] = self.store.last_stream_n(space, session)
        if item["n"] == last and self.stream_last.get(key) == item: return                  # a resend of the last item
        if item["n"] != last + 1: raise _invalid(f"n must be {last + 1}")
        self.store.add_stream_item(space, session, item["n"], item); self.store.commit()
        self.stream_n[key] = item["n"]; self.stream_last[key] = item
        for sub in list(self.watchers.get(key, ())):                                         # rights at the head, per item (T-07)
            acc = P.session_access(st, sub.actor, session)
            if acc["watch"] and (item["type"] != "context" or acc["read"]): sub.push(item)

    def watch(self, who, space, session):
        self.route(space)
        st, _ = self._session(space, session)
        if not P.session_access(st, who, session)["watch"]: raise WireError("denied", "no watch right on this session")
        sub = Sub(who, space); self.watchers.setdefault((space, session), set()).add(sub)
        return sub

    # ── move handover (G-06, §7.3) ──
    def host(self, who, space, records, head=None):
        """Server to server, as `@:A` the current home: take the next contiguous records of `space`, verified (R-14) with wire.Mirror
        (the chain is held in `self.chains`, not in `home.spaces`: this server serves no reads and orders nothing for the space
        yet). `head` asks for our signed acceptance of the chain through it. When the last record held is a `space.move` to
        this server the space becomes ours. -> {have, accept?}"""
        if not (isinstance(who, str) and who.startswith("@:") and who[2:] != self.server): raise WireError("denied", "host is for a home server acting as @:A")
        if not (isinstance(space, str) and P._space(space) and isinstance(records, list) and len(records) <= 1000
                and all(isinstance(r, dict) for r in records) and (head is None or isinstance(head, dict))): raise _invalid("host args")
        if space in self.home.spaces or self.home.moved.get(space): raise WireError("denied", "this server already has a home record of the space")
        held = self.chains.records(space)
        if (W.home_at(held, len(held) + 1) if held else P.server_of(space)) != who[2:]:
            raise WireError("denied", "the caller is not the home of the space at the last record held")
        for rec in records:
            try:
                if rec["entry"]["space"] != space or rec["receipt"]["space"] != space: raise _invalid("record of another space")
                ok, why = self.chains.mirror.ingest(rec, self.now_str())
            except (KeyError, TypeError, ValueError, AttributeError, IndexError, RecursionError): ok, why = False, "malformed record"
            if not ok:
                S = self.chains.mirror.spaces.get(space)
                if S: S.pop("faulty", None)                                             # not a verdict on the caller; the chain held stays usable
                raise _invalid(f"record does not verify: {why}")
            self.store.add_peer_record(rec)
        self.store.commit()
        held = self.chains.records(space)
        if head is not None and not (held and head == held[-1]["receipt"]): raise _invalid("head is not the last record held")
        out = {"have": len(held)}
        if held and held[-1]["entry"]["kind"] == "space.move" and held[-1]["entry"]["body"]["to"] == self.server:
            self._promote(space); out["have"] = len(self.home.spaces[space]["records"])
        if head is not None:
            out["accept"] = {"keyId": self.key_id, "sig": W.accept_move(self.key, space, head)}
        return out

    def _promote(self, space):
        """The chain handed over becomes ours: stored as home records (pinning the profile views its receipts name), then loaded."""
        for rec in self.chains.records(space):
            self.store.add_record(rec, {a: self.chains.profile(a, n) for a, n in rec["receipt"]["profiles"].items()})
            if rec.get("redacted"): self.store.mark_redacted(space, rec["receipt"]["id"], rec["redactedBy"])
        self.store.commit()
        self.chains.mirror.spaces.pop(space, None)
        self._load_space(space)

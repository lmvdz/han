"""The sequencer core (no network): the §4.1 pipeline over wire.Home plus storage, a steady clock, profile pinning, R-16,
fan-out, blobs, notices and session streams. han.seq.server is only the WebSocket framing around this.
Everything here is synchronous (one event loop, so each call is atomic, like the per-space serial transaction of §4.1)."""
from __future__ import annotations
import asyncio, math, time
from ..core import P, W, LIMITS, WireError
from ..entries import fmt_time, make_entry, default_policy
from .store import Store, BlobStore

EPOCH = "1970-01-01T00:00:00Z"
STREAM_FIELDS = {"output": {"text"}, "status": {"state"}, "context": {"blob", "what"}}      # + space, session, n, time, type (+ status.detail)

class Clock:
    """The two clocks a sequencer reads: `wall` (epoch seconds, may be stepped) and `mono` (steady, counts only forward)."""
    wall = staticmethod(time.time)
    mono = staticmethod(time.monotonic)

class Sub:
    """One live subscription (a read-follow, subscribe or watch); han.seq.server drains `q` onto the socket."""
    def __init__(self, actor, space=None, cutoff=0):
        self.actor, self.space, self.cutoff, self.q, self.dead = actor, space, cutoff, asyncio.Queue(), False
    def push(self, item):
        if self.dead: return
        if self.q.qsize() > 10000: self.dead = True; self.q.put_nowait(None)      # a consumer that doesn't keep up is cut off (it can re-read)
        else: self.q.put_nowait(item)

class Notices(list):
    """An actor's inbox on its profile (G-03): Home appends signals here; we persist them and push them to live subscribers."""
    def __init__(self, seq, actor): super().__init__(); self.seq, self.actor = seq, actor
    def append(self, item):
        super().append(item)
        self.seq.store.add_notice(self.actor, item)
        if len(self) > 256: del self[0]
        for s in list(self.seq.notice_subs.get(self.actor, ())): s.push(item)

class Inbox(dict):
    def __init__(self, seq): super().__init__(); self.seq = seq
    def setdefault(self, k, default=None):
        if k not in self: self[k] = Notices(self.seq, k)
        return self[k]

def _invalid(why): return WireError("invalid", why)

class Sequencer:
    def __init__(self, server: str, key, db_path: str, clock=None):
        self.server, self.key, self.key_id = server, key, key.key_id
        self.clock = clock or Clock()
        self._anchor_wall, self._anchor_mono = self.clock.wall(), self.clock.mono()     # R-03a: a clock anchored at startup...
        self.boot_mono = self._anchor_mono                                              # ...and R-16: when this run began
        self.store = Store(db_path)
        self.server_keys = {server: {key.key_id: {"publicKey": key.pub, "from": EPOCH}}}
        self.home = W.Home(server, key, key.key_id, "local", clock=self.now_str, server_keys=self.server_keys)
        self.home.blobs, self.home.inbox = BlobStore(self.store), Inbox(self)
        self.home.uploads, self.home.supplied = self.store.uploads(), self.store.supplied()
        self._supplied_saved = set(self.home.supplied)
        self.subs: dict[str, set] = {}              # space → live read/subscribe subscriptions
        self.notice_subs: dict[str, set] = {}       # actor → subscriptions to its own profile (notices)
        self.watchers: dict[tuple, set] = {}        # (space, session) → watch subscriptions
        self.ids: dict[str, dict] = {}              # space → {entry id: seq}  (R-01 without a scan)
        self.claim_mono: dict[str, float] = {}      # claim id → steady time when this run ordered it (R-16)
        self.stream_n: dict[tuple, int] = {}        # (space, session) → last accepted StreamItem n
        self.stream_last: dict[tuple, dict] = {}
        self.broken = False
        self._load()

    # ── clock (R-03a) ──
    def now(self) -> float:
        """Steady, slewed: the wall clock read once at startup plus elapsed steady time. A later wall step has no effect."""
        return self._anchor_wall + (self.clock.mono() - self._anchor_mono)
    def now_str(self) -> str: return fmt_time(self.now())

    # ── startup: reload the chain, check it, rebuild state ──
    def _load(self):
        for sp in self.store.spaces():
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
                if e["kind"] == "space.move": self.home.moved[sp] = e["body"]["to"]
            self.home.spaces[sp] = {"records": recs, "state": state}
        for actor, items in self.store.notices().items():
            n = Notices(self, actor); list.extend(n, items); self.home.inbox[actor] = n

    def close(self): self.store.close()

    # ── identity: profile pinning (P-01, R-10) — profiles are spaces on this server ──
    def profile(self, actor: str, seq: int | None = None):
        """ProfileView of `actor` at profile seq `seq` (default: now), or None if this server can't produce it."""
        if actor.startswith("@:"):
            if actor[2:] != self.server: return None
            return {"actor": actor, "seq": 0, "actorKind": "service", "operatorConfirmed": False,
                    "keys": {self.key_id: {"publicKey": self.key.pub, "status": "valid"}}}
        if P.server_of(actor) != self.server: return None                 # no federation in v1-minimal
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
        if version != 1: raise _invalid("unsupported version")                                        # R-08
        view = self.profile(who) if isinstance(who, str) and P._actor(who) else None
        k = view["keys"].get(key_id) if view and isinstance(key_id, str) else None
        if not k or k["status"] != "valid" or not isinstance(proof, str) or not W.verify_hello(k["publicKey"], proof, challenge, self.server):
            raise WireError("denied", "authentication failed")
        return {"limits": LIMITS, "durability": self.home.durability, "time": self.now_str(), "knownHeads": self._heads_for(who, known_heads or [])}

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

    # ── append: §4.1 ──
    def append(self, entry, content=None):
        if self.broken: raise WireError("unavailable", "storage failure; restart")
        if not isinstance(entry, dict): raise _invalid("entry")
        try:
            eid = P.entry_id(entry)
            sp = entry.get("space")
            hit = self.ids.get(sp, {}).get(eid) if isinstance(sp, str) else None
            if hit: return self.home.spaces[sp]["records"][hit - 1]["receipt"]           # 1 dedupe, before any other check (R-01)
            err = W.validate_header(entry)                                               # 3 shape (Home repeats it; this precedes the profile lookup)
            if err: raise _invalid(err)
            if len(P.jcs(entry)) > LIMITS["maxHeaderBytes"]: raise _invalid("header too large")
            if content is not None and len(P.jcs(content)) > LIMITS["maxContentBytes"]: raise _invalid("content too large")
            S = self.home.spaces.get(sp)
            state = S["state"] if S else None
            need = P.relevant_actors(state, dict(entry, id=eid))                         # 4 profiles (all of P-01)
        except WireError: raise
        except (TypeError, KeyError, ValueError, AttributeError, RecursionError) as ex:
            raise _invalid(f"malformed: {type(ex).__name__}")
        profiles = {a: v for a in need if (v := self.profile(a))}
        if state and entry["kind"] == "effect.outcome": self._r16(state, entry)
        before = len(S["records"]) if S else 0
        try:
            code, res = self.home.append(entry, content, profiles)
        except (TypeError, KeyError, ValueError, AttributeError) as ex:                 # a shape the validators let through: invalid, not a crash
            raise _invalid(f"malformed: {type(ex).__name__}")
        if code != "ok":
            if code == "moved": raise WireError("moved", "this space moved", moved_to=res)
            if code == "unavailable" and isinstance(res, str) and res.startswith("clock behind"):
                last = P.ts(S["records"][-1]["receipt"]["time"])
                raise WireError("unavailable", res, retry_after_ms=max(1000, int((last - self.now() + 1) * 1000)))
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
        if entry["kind"] == "effect.claim": self.claim_mono[eid] = self.clock.mono()
        st = S["state"]
        for sub in list(self.subs.get(sp, ())):                                           # 9 fan out (T-01), P-02 per record
            if res["seq"] > sub.cutoff and P.can_read(st, sub.actor, res["seq"]): sub.push({"type": "record", "record": rec})
        return res

    def _r16(self, state, entry):
        """R-16: an owner's abandon (P-09) needs A seconds of this sequencer's own steady clock since it ordered the claim;
        a claim ordered before this run began counts from the start of this run."""
        b = entry["body"]
        _, eff, c = P.find_claim(state, b["claim"])
        if not c or (entry["author"] == eff["executor"] and entry["keyId"] == c["instance"]): return
        if not (b.get("result") == "unknown" and b.get("started") == "unknown" and "cost" not in b): return
        if state["roles"].get(entry["author"]) != "owner": return                          # Policy denies everyone else
        due = self.claim_mono.get(c["id"], self.boot_mono) + P.abandon_after(c["window"])
        wait = due - self.clock.mono()
        if wait > 0:
            raise WireError("unavailable", "R-16: an abandon needs a full launch window of steady time", retry_after_ms=math.ceil(wait * 1000))

    # ── read (§3) ──
    def _readable_somewhere(self, st, who):
        return st["policy"]["visibility"] == "public" or any(P.can_read(st, who, s) for s in range(1, st["head"] + 1))

    def read(self, who, space, after, follow=False, state=False, limit=None, known_heads=None):
        """→ (items, sub or None). `sub` is registered atomically with the backlog, so a follower misses nothing."""
        if not (isinstance(space, str) and P._space(space) and P._int(after)): raise _invalid("read args")
        limit = 1000 if limit is None else limit
        if not (P._int(limit) and 1 <= limit <= 1000): raise _invalid("limit")
        S = self.home.spaces.get(space)
        if not S or not self._readable_somewhere(S["state"], who): raise WireError("not_found", "no such space")
        st = S["state"]
        code, items = self.home.read(space, after, who, state)
        if code != "ok": raise WireError(code, items or "")
        head = [i for i in items if i["type"] == "state"]
        recs = [i for i in items if i["type"] == "record"]
        out = head + ([{"type": "heads", "heads": self._heads_for(who, known_heads)}] if known_heads is not None else []) + recs[:limit]
        sub = None
        if follow:
            sub = Sub(who, space, st["head"]); self.subs.setdefault(space, set()).add(sub)
        return out, sub

    def subscribe(self, who, space, after=None):
        """T-01/T-05 for `who`: records and signals of `space` (and, on one's own profile, notices). Live from the head,
        or from after `after` first."""
        if not (isinstance(space, str) and P._space(space)) or (after is not None and not P._int(after)): raise _invalid("subscribe args")
        S = self.home.spaces.get(space)
        if not S or not P.can_read(S["state"], who, S["state"]["head"]): raise WireError("denied", "cannot follow this space")
        st = S["state"]
        items = [{"type": "record", "record": r} for r in S["records"][after:] if P.can_read(st, who, r["receipt"]["seq"])] if after is not None else []
        sub = Sub(who, space, st["head"]); self.subs.setdefault(space, set()).add(sub)
        if space == "profile:" + who:                                                   # G-03/G-04: only the actor gets its notices
            self.notice_subs.setdefault(who, set()).add(sub)
            items += list(self.home.inbox.get(who, []))
        return items, sub

    def unsubscribe(self, sub):
        for table in (self.subs, self.notice_subs, self.watchers):
            for s in table.values(): s.discard(sub)

    # ── signals (G-04) ──
    def signal(self, who, space, body):
        if not (isinstance(space, str) and P._space(space)) or not P.json_ok(body): raise _invalid("signal args")
        code, detail = self.home.signal(space, who, body)
        if code != "ok": raise WireError(code, detail if isinstance(detail, str) else "")
        st = self.home.spaces[space]["state"]
        for sub in list(self.subs.get(space, ())):
            if sub.actor != who and P.can_read(st, sub.actor, st["head"]): sub.push({"type": "signal", "from": who, "body": body})

    # ── blobs (G-01, G-02) ──
    def blob_put(self, who, ref, offset, data):
        if not (W._blobref(ref) and P._int(offset) and isinstance(data, bytes)) or len(data) > LIMITS["maxBlobPartBytes"]:
            raise _invalid("blobPut args")
        if offset + len(data) > ref["size"]: raise _invalid("bytes beyond ref.size")
        if who in self.home.uploads.get(ref["hash"], ()):                                # this uploader already completed it
            return {"have": ref["size"], "complete": True}
        res = self.home.blob_put_part(ref, offset, data, who)
        if res["complete"]:
            self.home.parts.pop((ref["hash"], who), None)
            self.store.add_upload(ref["hash"], who); self.store.commit()
        return res

    def blob_get(self, who, ref, space, offset=0, length=None):
        length = LIMITS["maxBlobRangeBytes"] if length is None else length
        if not (W._blobref(ref) and isinstance(space, str) and P._space(space) and P._int(offset) and P._int(length) and length >= 1):
            raise _invalid("blobGet args")
        code, data = self.home.blob_get(ref, space, who)
        if code != "ok": raise WireError(code, "blob not available")
        return {"data": data[offset:offset + min(length, LIMITS["maxBlobRangeBytes"])], "size": len(data), "offset": offset}

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
        st, _ = self._session(space, session)
        if not P.session_access(st, who, session)["watch"]: raise WireError("denied", "no watch right on this session")
        sub = Sub(who, space); self.watchers.setdefault((space, session), set()).add(sub)
        return sub

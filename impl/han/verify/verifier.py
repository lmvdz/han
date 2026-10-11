"""M4 Verifier (architecture.md §4.4): follow a space, check every record (R-14), retain what was verified, gossip heads (R-15).

`Verifier(client, server_keys, state_path)` is a mirror of the spaces it follows. `verify(space)` pulls to the current head;
`follow(space)` then stays subscribed. Status is han.ts VerifierStatus plus `ok` (han.ts has no variant for "all good"):
  {state: "ok", through} | {state: "faulty", through, proof} | {state: "stopped", through, reason} | {state: "moved", through, to}
A fault stops trust above `through` for good (the proof is kept, announced to the room as a `fault` signal, and persisted).
`stopped` is not a fault: the verifier could not continue (connection, a gap it cannot read across, a profile it cannot get)."""
from __future__ import annotations
import asyncio, json, os, time
from ..core import P, W, WireError
from . import proof as pf
from .chains import Chains, classify

_NET = (ConnectionError, OSError, asyncio.TimeoutError)

class StateError(Exception):
    """The state file does not verify against the server keys given."""

def _line(d) -> str: return json.dumps(d, separators=(",", ":"), ensure_ascii=False) + "\n"

class Verifier:
    def __init__(self, client, server_keys: dict, state_path: str | None = None, skew: int = pf.SKEW,
                 gossip_interval: float = 600.0, now=time.time):
        self.client, self.keys, self.skew, self.gossip_interval, self.now = client, server_keys, skew, gossip_interval, now
        self.c = Chains(server_keys)
        self.faults: dict[str, dict] = {}          # space → proof
        self.stopped: dict[str, str] = {}          # space → reason
        self.moved: dict[str, str] = {}            # space → new home
        self.peer_heads: dict[str, dict[int, dict]] = {}     # space → seq → a valid receipt our chain has not reached yet
        self.announced: set[str] = set()
        self._log: list[tuple[str, dict]] = []     # every verified record in verification order (profile chains included)
        self._loading: set[str] = set()
        self.state_path = state_path
        self._fh = None
        if state_path: self._load(state_path)

    # ── persistence: JSON lines, append-only; a redaction rewrites the file so the content is really gone ──
    def _load(self, path):
        if os.path.exists(path):
            with open(path, encoding="utf-8") as fh:
                for line in fh:
                    if not line.strip(): continue
                    d = json.loads(line)
                    if d["t"] == "rec":
                        ok, why = self.c.ingest(d["rec"])
                        if not ok: raise StateError(f"{path}: stored record {d['rec']['receipt']['seq']} of {d['rec']['entry']['space']} no longer verifies: {why}")
                        self._keep(d["rec"], persist=False)
                    elif d["t"] == "fault": self.faults[d["space"]] = d["proof"]
        self._fh = open(path, "a", encoding="utf-8")

    def _write(self, d):
        if not self._fh: return
        self._fh.write(_line(d)); self._fh.flush(); os.fsync(self._fh.fileno())

    def _rewrite(self):
        if not self._fh: return
        self._fh.close()
        tmp = self.state_path + ".tmp"
        with open(tmp, "w", encoding="utf-8") as fh:
            for _, rec in self._log: fh.write(_line({"t": "rec", "rec": rec}))
            for sp, proof in self.faults.items(): fh.write(_line({"t": "fault", "space": sp, "proof": proof}))
            fh.flush(); os.fsync(fh.fileno())
        os.replace(tmp, self.state_path)
        self._fh = open(self.state_path, "a", encoding="utf-8")

    def close(self):
        if self._fh: self._fh.close(); self._fh = None

    def _keep(self, rec, persist=True):
        e = rec["entry"]
        self._log.append((e["space"], rec))
        if persist: self._write({"t": "rec", "rec": rec})
        if e["kind"] == "space.move": self.moved[e["space"]] = e["body"]["to"]
        if e["kind"] == "redact":                      # P-12: the target's content is deleted wherever honoured
            for old in self.c.records(e["space"]):
                if old["receipt"]["id"] == e["body"]["target"] and not old.get("redacted"):
                    old["content"], old["redacted"], old["redactedBy"] = None, True, {"entry": e, "receipt": rec["receipt"]}
                    if persist: self._rewrite()

    # ── status ──
    def spaces(self) -> list[str]:
        return list(dict.fromkeys(sp for sp, _ in self._log if not sp.startswith("profile:")))

    def status(self, space: str) -> dict:
        through = self.c.head(space)
        if space in self.faults:
            p = self.faults[space]
            return {"state": "faulty", "through": min(through, p["through"]), "proof": p}
        if space in self.moved: return {"state": "moved", "through": through, "to": self.moved[space]}
        if space in self.stopped: return {"state": "stopped", "through": through, "reason": self.stopped[space]}
        return {"state": "ok", "through": through}

    def heads(self) -> list[dict]:
        """Our latest receipt per followed space (R-15), for gossip."""
        return [self.c.records(sp)[-1]["receipt"] for sp in self.spaces()]

    # ── faults ──
    def _profiles_for(self, records: list) -> dict:
        out = {}
        for rec in records:
            for a in rec["receipt"]["profiles"]:
                sp = "profile:" + a
                if not a.startswith("@:") and sp not in out and self.c.records(sp): out[sp] = list(self.c.records(sp))
        return out

    def _fault(self, space, kind, why, receipts, record=None, through=None, chain=None, observed_at=None) -> dict:
        chain = list(self.c.records(space)) if chain is None else chain
        through = len(chain) if through is None else through
        bare = kind in ("equivocation", "future-time")           # two receipts / one receipt are the whole evidence
        p = pf.make(kind, space, through, why, receipts, self.keys, record, None if bare else chain,
                    {} if bare else self._profiles_for(chain + ([record] if record else [])), observed_at)
        self.faults[space] = p
        self._write({"t": "fault", "space": space, "proof": p})
        return p

    def observe(self, r: dict) -> dict | None:
        """R-15: compare a head receipt (from a peer, or the sequencer's own `heads` item) with our chain.
        Returns a new proof, or None. A receipt beyond our chain is kept and compared when we reach its seq."""
        if not P._receipt(r) or not W.verify_receipt(r, self.keys): return None
        space, seq = r["space"], r["seq"]
        if space in self.faults: return None
        recs = self.c.records(space)
        if seq <= len(recs):
            mine = recs[seq - 1]["receipt"]
            if W.receipt_hash(mine) == W.receipt_hash(r): return None
            if W.equivocation_proof(mine, r, self.keys):
                return self._fault(space, "equivocation", f"two receipts for {space} #{seq}", [mine, r], through=seq - 1)
        if seq <= len(recs) + 1 and W.wrong_home_proof(recs[:seq - 1], r, self.keys):
            return self._fault(space, "wrong-home", f"receipt {seq} signed by {r['home']}, which was not the home", [r],
                               through=seq - 1, chain=list(recs[:seq - 1]))
        if seq > len(recs):
            held = self.peer_heads.setdefault(space, {})
            if len(held) < 1024: held[seq] = r
        return None

    def _adopt(self, space, p) -> bool:
        """A fault proof from the room: believe it only if it checks offline against our own server keys."""
        if space in self.faults or not isinstance(p, dict) or p.get("space") != space: return False
        ok, _ = pf.check(p, self.keys)
        if not ok: return False
        p = dict(p, serverKeys=self.keys, through=min(p.get("through", 0), self.c.head(space)))
        self.faults[space] = p
        self._write({"t": "fault", "space": space, "proof": p})
        self.announced.add(space)
        return True

    # ── taking records in ──
    async def _ensure_profile(self, actor, n) -> bool:
        """Have the profile chain of `actor` through seq `n` (§4.3: verifiers retain the profiles their receipts pin)."""
        if actor.startswith("@:") or n < 1: return True
        sp = "profile:" + actor
        if sp in self._loading or sp in self.faults: return sp not in self.faults and self.c.head(sp) >= n
        self._loading.add(sp)
        try:
            while self.c.head(sp) < n:
                got = 0
                async for it in self.client.read(sp, self.c.head(sp)):
                    if it["type"] != "record": continue
                    got += 1
                    if not await self._take(sp, it["record"]): return False
                if not got: return False
            return True
        finally: self._loading.discard(sp)

    async def _take(self, space, rec) -> bool:
        """Check and retain one record. False means the space can't go on (a fault or a stop; see status)."""
        if space in self.faults: return False
        try:
            r, e = rec["receipt"], rec["entry"]
            seq = r["seq"]
            if not (isinstance(seq, int) and e["space"] == space and isinstance(r["profiles"], dict)): raise KeyError("shape")
        except (KeyError, TypeError):
            self._fault(space, "chain", "malformed record", [], rec); return False
        have = self.c.head(space)
        if seq <= have:                                           # a repeat: it must be the record we hold
            return W.receipt_hash(self.c.records(space)[seq - 1]["receipt"]) == W.receipt_hash(r) or self.observe(r) is None
        if seq != have + 1:
            self.stopped[space] = f"seq gap: have {have}, got {seq} (the reader may not read the records between, or they were withheld)"
            return False
        pins = dict(r["profiles"])
        rb = rec.get("redactedBy")                                # a redaction proof pins the redactor's profile too
        if isinstance(rb, dict) and isinstance(rb.get("receipt"), dict) and isinstance(rb["receipt"].get("profiles"), dict):
            for a, n in rb["receipt"]["profiles"].items(): pins[a] = max(pins.get(a, 0), n)
        for a, n in pins.items():
            if isinstance(n, int) and not await self._ensure_profile(a, n):
                bad = "profile:" + a
                self.stopped[space] = f"profile of {a} through {n} unavailable" + (f" (its chain is faulty: {self.faults[bad]['reason']})" if bad in self.faults else "")
                return False
        if W.verify_receipt(r, self.keys) and P.ts(r["time"]) > self.now() + self.skew:           # §7.5: receipts dated in the future
            self._fault(space, "future-time", f"receipt {seq} dated {r['time']}", [r], through=have, observed_at=pf.fmt(self.now()))
            return False
        ok, why = self.c.ingest(rec)
        if not ok:
            self._fault(space, classify(why), why, [r], rec); return False
        self._keep(rec)
        held = self.peer_heads.get(space, {})
        for s in [s for s in held if s <= seq]:
            if self.observe(held.pop(s)): return False
        return True

    # ── verify / follow ──
    async def verify(self, space: str) -> dict:
        """Pull to the sequencer's current head, checking everything; also compare the head it reports (R-15)."""
        self.stopped.pop(space, None)
        if space in self.faults or space in self.moved: return self.status(space)
        try:
            while True:
                got = 0
                async for it in self.client.read(space, self.c.head(space), known_heads=[{"space": space}]):
                    if it["type"] == "heads":
                        for r in it["heads"]: self.observe(r)
                    elif it["type"] == "record":
                        got += 1
                        if not await self._take(space, it["record"]): return self.status(space)
                    if space in self.faults: return self.status(space)
                if got < 1000: break
        except WireError as ex:
            if ex.code == "moved" and ex.moved_to: self.moved[space] = ex.moved_to
            else: self.stopped[space] = f"{ex.code}: {ex.message}"
        except _NET as ex:
            self.stopped[space] = f"connection: {ex}"
        return self.status(space)

    async def gossip(self, space: str):
        """R-15 in the room: signal our head; the other readers of the space compare it with theirs."""
        recs = self.c.records(space)
        if recs and space not in self.faults:
            try: await self.client.signal(space, {"type": "heads", "receipts": [recs[-1]["receipt"]]})
            except (WireError,) + _NET: pass

    async def _announce(self, space):
        """R-14: tell the room (T-05); the proof is also kept."""
        if space in self.announced or space not in self.faults: return
        self.announced.add(space)
        try: await self.client.signal(space, {"type": "fault", "proof": self.faults[space]})
        except (WireError,) + _NET: pass

    def _on_signal(self, space, body):
        if not isinstance(body, dict): return
        if body.get("type") == "heads" and isinstance(body.get("receipts"), list):
            for r in body["receipts"][:16]:
                if isinstance(r, dict) and r.get("space") == space and self.observe(r): break
        elif body.get("type") == "fault": self._adopt(space, body.get("proof"))

    async def _gossip_loop(self, space):
        while True:
            await self.gossip(space)
            await asyncio.sleep(self.gossip_interval)

    async def follow(self, space: str, on_status=None) -> dict:
        """verify, then stay subscribed until a fault, a move, a stop or cancellation; return the final status."""
        last = [None]
        def emit():
            st = self.status(space)
            if st != last[0]:
                last[0] = st
                if on_status: on_status(st)
            return st
        st = await self.verify(space); emit()
        if st["state"] == "faulty": await self._announce(space)
        if st["state"] != "ok": return st
        gossip = asyncio.create_task(self._gossip_loop(space))
        try:
            async for it in self.client.subscribe(space, after=self.c.head(space)):
                if it["type"] == "record": await self._take(space, it["record"])
                elif it["type"] == "signal": self._on_signal(space, it["body"])
                st = emit()
                if space in self.faults: await self._announce(space)
                if st["state"] != "ok": break
        except WireError as ex:
            if ex.code == "moved" and ex.moved_to: self.moved[space] = ex.moved_to
            else: self.stopped[space] = f"{ex.code}: {ex.message}"
        except _NET as ex:
            self.stopped[space] = f"connection: {ex}"
        except asyncio.CancelledError:
            self.stopped.setdefault(space, "stopped")
            raise
        finally:
            gossip.cancel()
        return emit()

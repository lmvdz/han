"""HAN v1 reference for the signed layer and server behaviour, with real Ed25519 (`cryptography` package).

Covers: exact header and content validation (R-09, §5), ids and signatures (§2), receipts and chains (R-02),
server key validity windows, a reference home (the §4.1 append pipeline, reads with read rights, signals on follow
streams G-04, first-contact notices G-03, head gossip R-15, per-space blob supply G-01), a verifying mirror (R-14),
and wrong-home proofs (R-02: equivocation, and receipts signed by a server that wasn't the home at that seq).

Preimages (architecture.md §2, all UTF-8):
  id          = sha256("han-entry-v1\\n"    + JCS(entry minus sig))           → lowercase hex
  sig         = Ed25519(author key,  "han-sig-v1\\n"      + id)
  receiptHash = sha256("han-receipt-v1\\n"  + JCS(receipt minus homeSig))     → lowercase hex
  homeSig     = Ed25519(home key,    "han-home-sig-v1\\n" + receiptHash)
  helloProof  = Ed25519(actor key,   "han-hello-v1\\n" + challenge + "\\n" + dialed server name)
"""
from __future__ import annotations
import base64, datetime, hashlib, re
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey, Ed25519PublicKey
from cryptography.hazmat.primitives import serialization
from cryptography.exceptions import InvalidSignature
import policy as P

MAX_CLOCK_SKEW = 60        # han.ts Limits.maxClockSkewSeconds: the one future-time tolerance (R-03a)

def secs(t: str) -> int:
    """Seconds since the epoch of a HAN Time string."""
    return int(datetime.datetime.strptime(t, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=datetime.timezone.utc).timestamp())

def fmt(sec: int) -> str:
    """A HAN Time string for seconds since the epoch."""
    return datetime.datetime.fromtimestamp(sec, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

def b64(b: bytes) -> str: return base64.urlsafe_b64encode(b).rstrip(b"=").decode()
def unb64(s: str) -> bytes: return base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))

class Key:
    """Deterministic test key from a seed string."""
    def __init__(self, seed: str):
        self.sk = Ed25519PrivateKey.from_private_bytes(hashlib.sha256(("key:" + seed).encode()).digest())
        self.pub = b64(self.sk.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw))
    def sign(self, msg: bytes) -> str: return b64(self.sk.sign(msg))

def verify_sig(pub: str, sig: str, msg: bytes) -> bool:
    try:
        Ed25519PublicKey.from_public_bytes(unb64(pub)).verify(unb64(sig), msg); return True
    except (InvalidSignature, ValueError):
        return False

# ── header (R-09) ──
ENTRY_FIELDS = {"v", "space", "kind", "author", "keyId", "body", "contentHash", "refs", "seen", "nonce", "time", "sig"}
NONCE = re.compile(r"^[a-z2-7]{26}$", re.ASCII)

def validate_header(entry: dict) -> str | None:
    """Exact wire header (han.ts Entry): no missing or extra fields, every field in its grammar."""
    if not isinstance(entry, dict) or set(entry) != ENTRY_FIELDS: return "header fields"
    if not (isinstance(entry["nonce"], str) and NONCE.fullmatch(entry["nonce"])): return "nonce"
    if not P._time(entry["time"]): return "time"
    if not (entry["contentHash"] == "" or P._hex(entry["contentHash"])): return "contentHash"
    if not isinstance(entry["sig"], str): return "sig"
    return P.validate(entry)

# ── content (R-09, §5) — mirrored from han.ts Contents ──
MIME = re.compile(r"^[a-z0-9][a-z0-9!#$&^_.+-]{0,63}/[a-z0-9][a-z0-9!#$&^_.+-]{0,63}$", re.ASCII)
def _str(n): return lambda v: isinstance(v, str) and len(v.encode("utf-8")) <= n
def _blobref(v):
    return (isinstance(v, dict) and {"hash", "size", "type"} <= set(v) <= {"hash", "size", "type", "uri"}
            and ("uri" not in v or (isinstance(v["uri"], str) and len(v["uri"].encode("utf-8")) <= 1024)) and P._hex(v["hash"]) and P._int(v["size"])
            and v["size"] <= 4 * 2**30 and isinstance(v["type"], str) and bool(MIME.fullmatch(v["type"])))
def _blobs(v): return isinstance(v, list) and len(v) <= 64 and all(_blobref(b) for b in v)
def _links(v): return isinstance(v, list) and len(v) <= 64 and all(
    isinstance(l, dict) and set(l) == {"space", "seq", "id"} and P._space(l["space"]) and P._int(l["seq"]) and P._id(l["id"]) for l in v)
def _plan(v):
    return (isinstance(v, dict) and {"action", "target"} <= set(v) <= {"action", "target", "args", "argsBlob"}
            and isinstance(v["action"], str) and bool(P.ACTION.fullmatch(v["action"])) and _str(1024)(v["target"])
            and ("argsBlob" not in v or _blobref(v["argsBlob"])) and ("args" not in v or P.json_ok(v["args"])))

CONTENT = {   # kind → {field: (validator, required)};  None = the kind has no content
    "space.create": {"name": (_str(256), True)},
    "member.set": None, "policy.set": None, "key.add": None, "key.revoke": None, "task.cancel": None,
    "effect.claim": None, "effect.cancel": None, "space.move": None,
    "message": {"text": (_str(32768), True), "blobs": (_blobs, False), "links": (_links, False)},
    "task": {"ask": (_str(32768), True), "blobs": (_blobs, False), "links": (_links, False)},
    "task.update": {"text": (_str(32768), False), "blobs": (_blobs, False)},
    "effect.propose": {"plan": (_plan, True)},
    "effect.approve": {"note": (_str(4096), False)},
    "effect.outcome": {"evidence": (_blobref, False)},
    "effect.resolve": {"evidence": (_blobref, True)},
    "checkpoint": {"blobs": (_blobs, True)},
    "redact": {"reason": (_str(1024), False)},
    "session.open": {"title": (_str(256), False)},
    "session.close": None,
    "session.input": {"text": (_str(32768), True), "blobs": (_blobs, False)},
}

def validate_content(entry: dict, content) -> str | None:
    kind = entry["kind"]
    if P.is_extension(kind):                          # opaque, but `blobs` (if present) is the generic reference position
        if content is None: return None
        if not isinstance(content, dict) or not P.json_ok(content): return "extension content must be an object of plain Json"
        if "blobs" in content and not _blobs(content["blobs"]): return "bad blobs"
        return None
    sch = CONTENT[kind]
    if sch is None: return None if content is None else "this kind has no content"
    if content is None:                                # content may be omitted when every content field is optional
        return None if not any(req for _, req in sch.values()) else "content missing"
    if not isinstance(content, dict): return "content must be an object"
    for f in content:
        if f not in sch: return f"unknown content field {f}"
    for f, (ok, req) in sch.items():
        if f in content:
            if not ok(content[f]): return f"bad content {f}"
        elif req: return f"missing content {f}"
    if kind == "effect.propose":
        if content["plan"]["action"] != entry["body"]["action"]: return "plan.action ≠ action"
        if hashlib.sha256(P.jcs(content["plan"])).hexdigest() != entry["body"]["planDigest"]: return "planDigest mismatch"
    return None

def content_hash(content) -> str:
    return "" if content is None else hashlib.sha256(P.jcs(content)).hexdigest()

def entry_id(entry: dict) -> str:
    return P.entry_id(entry)

def sign_entry(entry: dict, key: Key) -> dict:
    e = {k: v for k, v in entry.items() if k not in ("sig", "id")}
    e["sig"] = key.sign(b"han-sig-v1\n" + entry_id(e).encode())
    return e

def verify_entry(entry: dict, view: dict) -> bool:
    """Identity.verify: the signature checks against the keyId's public key in the pinned view.
    Key *status* (revoked or not) is a Policy question (R-12 → `denied`), so it is not checked here."""
    k = view["keys"].get(entry["keyId"])
    return bool(k) and verify_sig(k["publicKey"], entry["sig"], b"han-sig-v1\n" + entry_id(entry).encode())

# ── receipts and server keys ──
def receipt_hash(r: dict) -> str:
    return hashlib.sha256(b"han-receipt-v1\n" + P.jcs({k: v for k, v in r.items() if k != "homeSig"})).hexdigest()

def sign_receipt(r: dict, key: Key) -> dict:
    r = dict(r); r.pop("homeSig", None)
    r["homeSig"] = key.sign(b"han-home-sig-v1\n" + receipt_hash(r).encode())
    return r

def verify_receipt(r: dict, server_keys: dict) -> bool:
    """server_keys: {server: {keyId: {publicKey, from, until?}}} from https://<server>/.well-known/han.
    The receipt time must fall in the key's [from, until) window."""
    k = server_keys.get(r["home"], {}).get(r["homeKey"])
    if not k or r["time"] < k["from"] or ("until" in k and r["time"] >= k["until"]): return False
    return verify_sig(k["publicKey"], r["homeSig"], b"han-home-sig-v1\n" + receipt_hash(r).encode())

def hello_proof(key: Key, challenge: str, dialed: str) -> str:
    return key.sign(("han-hello-v1\n" + challenge + "\n" + dialed).encode())

def verify_hello(pub: str, proof: str, challenge: str, server: str) -> bool:
    return verify_sig(pub, proof, ("han-hello-v1\n" + challenge + "\n" + server).encode())

# ── move acceptance (§7.3): the target server signs that it will continue the chain from the current head ──
def accept_move(key: Key, space: str, head_receipt: dict) -> str:
    return key.sign(("han-accept-move-v1\n" + space + "\n" + receipt_hash(head_receipt)).encode())

def verify_accept(server_keys: dict, to: str, accept: dict, space: str, head_receipt: dict, time: str) -> bool:
    k = server_keys.get(to, {}).get(accept["keyId"])
    if not k or time < k["from"] or ("until" in k and time >= k["until"]): return False
    return verify_sig(k["publicKey"], accept["sig"], ("han-accept-move-v1\n" + space + "\n" + receipt_hash(head_receipt)).encode())

# ── proofs (R-02) ──
def home_at(records: list[dict], seq: int) -> str:
    """The home for `seq`: the space id's server, changed by every space.move ordered before `seq`."""
    home = P.server_of(records[0]["entry"]["space"]) if records else None
    for rec in records:
        if rec["receipt"]["seq"] >= seq: break
        if rec["entry"]["kind"] == "space.move": home = rec["entry"]["body"]["to"]
    return home

def equivocation_proof(r1: dict, r2: dict, server_keys: dict) -> bool:
    """Two valid receipts, same space and seq, same home server, different receipt hashes."""
    return (r1["space"] == r2["space"] and r1["seq"] == r2["seq"] and r1["home"] == r2["home"]
            and receipt_hash(r1) != receipt_hash(r2) and verify_receipt(r1, server_keys) and verify_receipt(r2, server_keys))

def wrong_home_proof(records: list[dict], r: dict, server_keys: dict) -> bool:
    """A valid receipt at seq k signed by a server that wasn't the home at k (given verified records before k)."""
    return verify_receipt(r, server_keys) and home_at(records, r["seq"]) not in (None, r["home"])

# ── blob references (G-01): every BlobRef position, per kind; `blobs` is generic for every kind ──
BLOB_PATHS = {"effect.propose": [("plan", "argsBlob")], "effect.outcome": [("evidence",)], "effect.resolve": [("evidence",)]}

def blob_refs(kind: str, content) -> list[dict]:
    if not isinstance(content, dict): return []
    out = []
    for path in [("blobs", "*")] + BLOB_PATHS.get(kind, []):
        cur = [content]
        for step in path:
            nxt = []
            for c in cur:
                if step == "*" and isinstance(c, list): nxt.extend(c)
                elif isinstance(c, dict) and step in c: nxt.append(c[step])
            cur = nxt
        out.extend(x for x in cur if isinstance(x, dict) and "hash" in x)
    return out

# ── a reference home ──
class Home:
    """The §4.1 append pipeline plus the server behaviours G-01, R-15, G-03, G-02, G-04."""
    def __init__(self, server: str, key: Key, key_id="h1", durability="local", clock=None, network=None, server_keys=None, steady=None):
        self.server, self.key, self.key_id, self.durability = server, key, key_id, durability
        self.steady = steady      # R-16: a callable returning steady-clock seconds; None switches the elapsed-time check off
        self.run_start = steady() if steady else 0       # claims ordered before this run count from here (restart, failover)
        self.claim_steady = {}    # claim id -> steady reading when this run ordered it
        self.context_blobs = {}   # (space, hash) -> session id: blobs of `context` StreamItems (G-01, G-05)
        self.server_keys = server_keys or {}
        self.parts = {}           # (hash, uploader) -> bytearray of that uploader's own bytes (G-01)
        self.spaces = {}          # space -> {"records": [...], "state": SpaceState}
        self.clock = clock or (lambda: "2026-10-10T12:00:00Z")
        self.moved = {}           # space -> new home
        self.network = network if network is not None else {}
        self.network[server] = self
        self.followers = {}       # space -> {actor: [ReadItem]}
        self.inbox = {}           # actor -> [notices] — delivered only to that actor's own sessions (G-03)
        self.blobs = {}           # hash -> bytes
        self.uploads = {}         # hash -> set of actors who uploaded it here
        self.supplied = set()     # (space, hash): bytes this server may serve for that space (G-01)

    def append(self, entry: dict, content, profiles: dict, skip_policy=False):
        sp = entry.get("space")
        S = self.spaces.get(sp)
        eid = entry_id(entry)
        if S:                                                               # 1 dedupe first (R-01)
            for rec in S["records"]:
                if rec["receipt"]["id"] == eid: return "ok", rec["receipt"]
        if sp in self.moved: return "moved", self.moved[sp]                 # 2 not home
        if not S and P.server_of(sp) != self.server: return "invalid", "space id names another home"
        err = validate_header(entry)                                        # 3 shape
        if err: return "invalid", err
        if (content is None) != (entry["contentHash"] == ""): return "invalid", "content presence"
        if content_hash(content) != entry["contentHash"]: return "invalid", "contentHash"
        err = validate_content(entry, content)
        if err: return "invalid", err
        state = S["state"] if S else None
        now = self.clock()                                                  # read once per append
        pol = state["policy"] if state else entry["body"].get("policy", {})
        rescue = entry["kind"] in ("policy.set", "space.move")             # R-08: these can always be ordered
        if pol.get("minDurability") == "replicated" and self.durability != "replicated" and not rescue:
            return "unavailable", "minDurability not reachable"
        if entry["kind"] in ("policy.set", "space.create") and entry["body"]["policy"]["minDurability"] == "replicated" \
                and self.durability != "replicated":
            return "unavailable", "this home can't meet the requested minDurability"
        if entry["kind"] == "space.move" and S:
            head = S["records"][-1]["receipt"]
            if not verify_accept(self.server_keys, entry["body"]["to"], entry["body"]["accept"], sp, head, now):
                return "invalid", "no valid acceptance from the target server"
        pinned = self.pin(state, dict(entry, id=eid), profiles, now)        # 4 profiles (P-01: candidates up to the first valid key)
        if pinned is None: return "unavailable", "profile"
        if not verify_entry(entry, profiles[entry["author"]]): return "invalid", "signature"          # 5
        if S and now < S["records"][-1]["receipt"]["time"]: return "unavailable", "clock behind the chain; wait (R-03a)"
        ctx = {"time": now, "profiles": pinned}
        wait = self.abandon_wait(state, entry, eid, pinned, now)                       # R-16: an early owner abandon is `unavailable`, not `denied`
        if wait > 0 and not skip_policy: return "unavailable", {"reason": "abandon before the launch window closed (R-16)", "retryAfterMs": int(wait * 1000)}
        if not skip_policy:                                                 # 6
            d = P.decide(state, dict(entry, id=eid), ctx)
            if not d["allow"]: return d["code"], d["reason"]
        prev = S["records"][-1]["receipt"] if S else None                    # 7
        r = {"space": sp, "seq": (prev["seq"] + 1) if prev else 1, "id": eid, "prev": receipt_hash(prev) if prev else "",
             "home": self.server, "homeKey": self.key_id, "time": ctx["time"], "durability": self.durability,
             "profiles": {a: v["seq"] for a, v in pinned.items()}}
        r = sign_receipt(r, self.key)
        rec = {"entry": entry, "content": content, "redacted": False, "receipt": r}
        new_state = P.fold(state, {"entry": dict(entry, id=eid), "content": None, "receipt": r}, ctx)   # 8
        if not S: S = self.spaces[sp] = {"records": [], "state": None, "pins": []}
        S.setdefault("pins", [None] * len(S["records"])).append(pinned)
        S["records"].append(rec); S["state"] = new_state
        if entry["kind"] == "effect.claim" and self.steady: self.claim_steady[eid] = self.steady()
        if entry["kind"] == "space.move": self.moved[sp] = entry["body"]["to"]
        for ref in blob_refs(entry["kind"], content):                       # G-01: the author supplied these bytes
            if entry["author"] in self.uploads.get(ref["hash"], ()): self.supplied.add((sp, ref["hash"]))
        if entry["kind"] == "redact":
            for old in S["records"]:
                if old["receipt"]["id"] == entry["body"]["target"]:
                    old["content"], old["redacted"], old["redactedBy"] = None, True, {"entry": entry, "receipt": r}
        for q in self.followers.get(sp, {}).values(): q.append({"type": "record", "record": rec})       # 9 fan out
        self._notify(entry, sp, r["seq"], new_state)
        return "ok", r

    def restart(self):
        """R-16: a steady clock can't measure across a restart or a failover, so every earlier claim counts from now."""
        if self.steady: self.run_start = self.steady()
        self.claim_steady = {}

    def pin(self, state, entry, profiles, now):
        """P-01: pin the author, the target, the proposer and the guards, and the approval candidates in E3 order up to and
        including the first whose signing key is valid. Later candidates are not required. None if a required view is missing."""
        need = P.relevant_actors(state, entry, now)
        required, cand = {entry["author"]}, {}
        if entry["kind"] == "member.set": required.add(entry["body"]["actor"])
        eff = state["effects"].get(entry["body"]["proposal"]) if entry["kind"] == "effect.claim" and state else None
        if eff:
            required |= {eff["proposer"], *P.guards_of(state, eff)}
            cand = {a["author"]: a["keyId"] for a in P.approval_candidates(state, eff, now)}
        pinned, found_valid = {}, False
        for a in need:
            if found_valid and a not in required: continue
            if a not in profiles: return None
            pinned[a] = profiles[a]
            if a in cand and P.key_status({"profiles": profiles}, a, cand[a]) == "valid": found_valid = True
        return pinned

    def abandon_wait(self, state, entry, eid, pinned, now):
        """R-16, P-09: seconds an owner's abandon must still wait. Non-zero only if Policy would allow the entry at claim.time + A
        (latest claim, phase `claimed`, right key and author): then it is early, by this home's steady clock (A since it ordered the
        claim, or since this run began) or by the receipt time it would get. Anything else gets Policy's own verdict now."""
        b = entry["body"]
        if not state or entry["kind"] != "effect.outcome": return 0
        _pid, eff, c = P.find_claim(state, b.get("claim"))
        if not c: return 0
        if entry["author"] == eff["executor"] and entry["keyId"] == c["instance"]: return 0       # the executor's own report
        A = P.abandon_after(c["window"])
        due = secs(c["time"]) + A
        if not P.decide(state, dict(entry, id=eid), {"time": fmt(due), "profiles": pinned})["allow"]: return 0
        steady_wait = 0
        if self.steady is not None:
            started = self.claim_steady.get(c["id"], self.run_start)
            steady_wait = A - (self.steady() - started)
        return max(steady_wait, due - secs(now), 0)

    def adopt(self, space, records, state):
        """Become home again after a move back (§7.3): take the verified chain and its state (from a mirror's replay),
        and stop answering `moved` once the last move names this server."""
        self.spaces[space] = {"records": list(records), "state": state, "pins": [None] * len(records)}
        if home_at(records, records[-1]["receipt"]["seq"] + 1) == self.server: self.moved.pop(space, None)

    def _notify(self, entry, space, seq, state):
        """G-03: tell the server of every actor given a role, or named in `to` while holding a role."""
        b = entry["body"]
        named = [b["actor"]] if entry["kind"] == "member.set" and b["role"] in P.READ_ROLES else []
        if isinstance(b.get("to"), list): named += [a for a in b["to"] if state["roles"].get(a) in P.READ_ROLES]
        for a in dict.fromkeys(named):
            target = self.network.get(P.server_of(a))
            if target: target.signal("profile:" + a, "@:" + self.server, {"type": "notice", "space": space, "seq": seq})

    def state_at(self, space, at):
        """The SpaceState after record `at`: a fold of the stored records with the profiles each receipt pinned."""
        S = self.spaces[space]
        pins = S.get("pins", [])
        if len(pins) != len(S["records"]) or any(p is None for p in pins[:at]): return None
        st = None
        for rec, pin in zip(S["records"][:at], pins):
            e = dict(rec["entry"], id=rec["receipt"]["id"])
            st = P.fold(st, {"entry": e, "content": None, "receipt": rec["receipt"]}, {"time": rec["receipt"]["time"], "profiles": pin})
        return st

    def read(self, space, after, as_, state=False, at=None, limit=1000):
        """§3 read. `at` (with `state`) gives the state after record `at`, and needs read rights at `at` (P-02), not at the head."""
        S = self.spaces.get(space)
        if not S: return "not_found", None
        st = S["state"]
        if not (isinstance(limit, int) and 1 <= limit <= 1000): return "invalid", "limit"
        if at is not None and (not state or not isinstance(at, int) or not 1 <= at <= st["head"]): return "invalid", "at needs state and 1 <= at <= head"
        items = []
        if state:
            where = st["head"] if at is None else at
            if not P.can_read(st, as_, where): return "denied", "state needs read rights at " + ("head" if at is None else "at")
            view = st if at is None else self.state_at(space, at)
            if view is None: return "unavailable", "no stored profile pins for this seq"
            items.append({"type": "state", "state": {k: v for k, v in view.items() if k != "entries"}})
        items += [{"type": "record", "record": r} for r in S["records"][after:] if P.can_read(st, as_, r["receipt"]["seq"])][:limit]
        return "ok", items

    def read_follow(self, space, after, as_, limit=1000):
        """§3 read with follow: `limit` backlog records, then the live records from the head at registration. If the backlog was
        longer, the client sees a seq gap between the last backlog record and the first live one and MUST re-read it."""
        code, items = self.read(space, after, as_, limit=limit)
        if code != "ok": return code, items, None
        q = self.followers.setdefault(space, {})[as_] = []
        return "ok", items, q

    def follow(self, space, as_):
        if space.startswith("profile:"):
            if space[len("profile:"):] != as_: return "denied", "only the actor may follow its own profile, for its notices"
            return "ok", self.inbox.setdefault(as_, [])
        S = self.spaces.get(space)
        if not S or not P.can_read(S["state"], as_, S["state"]["head"]): return "denied", None
        q = self.followers.setdefault(space, {}).setdefault(as_, [])
        return "ok", q

    def signal(self, space: str, actor: str, body: dict, relayed_by: str = None):
        """G-04; on a profile space only server notices are accepted, and they reach only the actor's own inbox (G-03)."""
        if space.startswith("profile:"):
            if not (actor.startswith("@:") and isinstance(body, dict) and body.get("type") == "notice"):
                return "denied", "only server notices on profile spaces"
            self.inbox.setdefault(space[len("profile:"):], []).append({"type": "signal", "from": actor, "body": body})
            return "ok", None
        S = self.spaces.get(space)
        if not S: return "not_found", space
        if relayed_by is not None and P.server_of(actor) != relayed_by[2:]:
            return "denied", "a server may relay signals only from its own actors"
        if not P.can_read(S["state"], actor, S["state"]["head"]): return "denied", "cannot read this space"
        for who, q in self.followers.get(space, {}).items():
            if who != actor: q.append({"type": "signal", "from": actor, "body": body})
        return "ok", None

    def heads(self):
        return [S["records"][-1]["receipt"] for S in self.spaces.values() if S["records"]]

    def hello(self, peer_heads, server_keys, peer=None):
        """R-15: exchange heads only for spaces the peer named and may read; return (my heads for those, proofs found)."""
        proofs, mine_out = [], []
        for pr in peer_heads:
            S0 = self.spaces.get(pr["space"])
            if S0 and (peer is None or P.can_read(S0["state"], "@:" + peer, S0["state"]["head"])):
                mine_out.append(S0["records"][-1]["receipt"])
            S = self.spaces.get(pr["space"])
            if S and pr["seq"] <= len(S["records"]):
                mine = S["records"][pr["seq"] - 1]["receipt"]
                if equivocation_proof(mine, pr, server_keys): proofs.append(("equivocation", mine, pr))
            if S and wrong_home_proof(S["records"][:pr["seq"] - 1], pr, server_keys): proofs.append(("wrong-home", pr))
        return mine_out, proofs

    # blobs (G-01, G-02)
    def blob_put_part(self, ref: dict, offset: int, data: bytes, as_: str):
        """blobPut (G-01): completion counts only bytes received from this uploader, and they must hash-match."""
        buf = self.parts.setdefault((ref["hash"], as_), bytearray())
        if offset != len(buf): return {"have": len(buf), "complete": False}
        buf.extend(data)
        done = len(buf) == ref["size"] and hashlib.sha256(bytes(buf)).hexdigest() == ref["hash"]
        if done:
            self.blobs[ref["hash"]] = bytes(buf); self.uploads.setdefault(ref["hash"], set()).add(as_)
        return {"have": len(buf), "complete": done}

    def blob_put(self, data: bytes, as_: str):
        h = hashlib.sha256(data).hexdigest()
        self.blobs[h] = data; self.uploads.setdefault(h, set()).add(as_)
        return h

    def stream(self, item, as_):
        """G-05: only the session's actor, on a live session, with `n` one more than its last item. A `context` item names a
        blob that G-01 then serves to every actor with `read` on the live session; supply is the session's actor's own upload."""
        S = self.spaces.get(item["space"])
        sess = S["state"]["sessions"].get(item["session"]) if S else None
        if not sess or sess["actor"] != as_ or not P.session_access(S["state"], as_, item["session"])["watch"]: return "denied", None
        key = (item["space"], item["session"])
        last = self.__dict__.setdefault("_stream_n", {}).get(key, 0)
        if item["n"] != last + 1: return "invalid", "n"
        self._stream_n[key] = item["n"]
        if item["type"] == "context":
            h = item["blob"]["hash"]
            self.context_blobs[(item["space"], h)] = item["session"]
            if as_ in self.uploads.get(h, ()): self.supplied.add((item["space"], h))
        return "ok", None

    def blob_get(self, ref, space, as_):
        S = self.spaces.get(space)
        if not S: return "not_found", None
        sid = self.context_blobs.get((space, ref["hash"]))
        by_context = sid is not None and P.session_access(S["state"], as_, sid)["read"]
        if not by_context and not blob_readable(S["records"], S["state"], ref, as_): return "denied", None
        if (space, ref["hash"]) not in self.supplied or ref["hash"] not in self.blobs: return "unavailable", None
        return "ok", self.blobs[ref["hash"]]

def fetch_blob(ref, record, home: str, mirrors: list[str], network: dict, me: str):
    """G-02: ask the author's server, then the home, then mirrors, as the server actor; verify the hash. What a server
    fetched for a space it may serve for that space."""
    space = record["entry"]["space"]
    for srv in blob_fetch_order(record, home, mirrors):
        h = network.get(srv)
        if not h or srv == me: continue
        code, data = h.blob_get(ref, space, "@:" + me)
        if code == "ok" and hashlib.sha256(data).hexdigest() == ref["hash"]:
            mine = network.get(me)
            if mine: mine.blobs[ref["hash"]] = data; mine.supplied.add((space, ref["hash"]))
            return srv, data
    return None, None

def blob_fetch_order(record: dict, home: str, mirrors: list[str]) -> list[str]:
    return list(dict.fromkeys([P.server_of(record["entry"]["author"]), home] + mirrors))

def blob_readable(records: list[dict], state: dict, ref: dict, reader: str) -> bool:
    for rec in records:
        if rec.get("redacted"): continue
        if any(b["hash"] == ref["hash"] for b in blob_refs(rec["entry"]["kind"], rec["content"])) \
                and P.can_read(state, reader, rec["receipt"]["seq"]):
            return True
    return False

# ── a verifying mirror (R-14) ──
class Mirror:
    def __init__(self, server_keys: dict, profile_source):
        """profile_source(actor, seq) → ProfileView at that profile seq (the mirror fetches profile spaces)."""
        self.server_keys, self.profile_source = server_keys, profile_source
        self.spaces = {}

    def ingest(self, rec: dict, now: str | None = None) -> tuple[bool, str]:
        """`now` is the verifier's own clock; a receipt dated more than MAX_CLOCK_SKEW after it is a fault (R-03a, future-time)."""
        e, r = rec["entry"], rec["receipt"]
        sp = e["space"]
        S = self.spaces.setdefault(sp, {"records": [], "state": None, "lastProfiles": {}})
        if S.get("faulty"): return False, "space faulty"
        def fault(why):
            S["faulty"] = why; return False, why
        err = validate_header(e)
        if err: return fault(f"header: {err}")
        eid = entry_id(e)
        if r["id"] != eid: return fault("receipt id mismatch")
        if r["space"] != sp: return fault("receipt for another space")
        expected = home_at(S["records"], r["seq"]) or P.server_of(sp)
        if r["home"] != expected: return fault("receipt from a server that is not the home")              # R-14
        if not verify_receipt(r, self.server_keys): return fault("receipt signature or key window")
        if now is not None and secs(r["time"]) > secs(now) + MAX_CLOCK_SKEW: return fault("future-time")           # R-03a
        prev = S["records"][-1]["receipt"] if S["records"] else None
        if r["seq"] != (prev["seq"] + 1 if prev else 1): return fault("seq gap")
        if r["prev"] != (receipt_hash(prev) if prev else ""): return fault("prev mismatch")
        if prev and r["time"] < prev["time"]: return fault("receipt time decreased")                      # R-03a
        for a, n in r["profiles"].items():                                                                  # R-10
            if n < S["lastProfiles"].get(a, 0): return fault(f"profile seq for {a} went backwards")
        views = {a: self.profile_source(a, n) for a, n in r["profiles"].items()}
        if e["author"] not in views or not verify_entry(e, views[e["author"]]): return fault("entry signature")
        if rec.get("redacted"):                                       # R-04: a redaction must carry its signed redact record
            rb = rec.get("redactedBy")
            if not rb: return fault("redacted without proof")
            re_, rr = rb["entry"], rb["receipt"]
            if not (re_["kind"] == "redact" and re_["space"] == sp and re_["body"]["target"] == eid and rr["id"] == entry_id(re_)
                    and rr["space"] == sp and rr["seq"] > r["seq"] and verify_receipt(rr, self.server_keys)
                    and verify_entry(re_, self.profile_source(re_["author"], rr["profiles"].get(re_["author"], 0)))):
                return fault("redaction proof invalid")
            S.setdefault("redactions", {})[rr["seq"]] = receipt_hash(rr)
        else:
            if content_hash(rec["content"]) != e["contentHash"]: return fault("content hash")
            err = validate_content(e, rec["content"])
            if err: return fault(f"content: {err}")
        if r["seq"] in S.get("redactions", {}) and S["redactions"][r["seq"]] != receipt_hash(r):
            return fault("a redaction proof named a different receipt for this seq")
        if e["kind"] == "space.move" and prev and not verify_accept(self.server_keys, e["body"]["to"], e["body"]["accept"], sp, prev, r["time"]):
            return fault("move without the target's acceptance")
        ctx = {"time": r["time"], "profiles": views}
        d = P.decide(S["state"], dict(e, id=eid), ctx)                                                     # replay
        if not d["allow"]: return fault(f"home accepted a forbidden entry: {d['reason']}")
        S["state"] = P.fold(S["state"], {"entry": dict(e, id=eid), "content": None, "receipt": r}, ctx)
        S["records"].append(rec)
        S["lastProfiles"].update(r["profiles"])
        return True, "ok"


# ── gossip signals (R-15, T-05): heads and faults travel as in-space signals and carry no authority ──
def heads_signal(receipts: list[dict]) -> dict: return {"type": "heads", "receipts": receipts}
def fault_signal(proof: dict) -> dict: return {"type": "fault", "proof": proof}

def receive_gossip(body: dict, records_by_space: dict, server_keys: dict) -> tuple[str, object]:
    """What a verifier does with a gossip signal. `records_by_space`: the verified records it holds. `server_keys`: its OWN keys.
    heads -> ("proofs", [fault kinds found]); fault -> ("adopted", kind) only if the proof checks against our keys, else ("ignored", why).
    Only the proof kinds that need no profile chains are checked here (equivocation, wrong-home, future-time)."""
    if not isinstance(body, dict): return "ignored", "not an object"
    if body.get("type") == "heads":
        rs = body.get("receipts")
        if not isinstance(rs, list) or len(rs) > 256: return "ignored", "bad heads"
        found = []
        for r in rs:
            mine = records_by_space.get(r.get("space"), [])
            if 1 <= r.get("seq", 0) <= len(mine) and equivocation_proof(mine[r["seq"] - 1]["receipt"], r, server_keys): found.append("equivocation")
            elif mine and wrong_home_proof(mine[:r.get("seq", 1) - 1], r, server_keys): found.append("wrong-home")
        return "proofs", found
    if body.get("type") == "fault":
        pr = body.get("proof")
        if not isinstance(pr, dict): return "ignored", "no proof"
        rs = pr.get("receipts", [])
        if pr.get("kind") == "equivocation" and len(rs) == 2 and equivocation_proof(rs[0], rs[1], server_keys): return "adopted", "equivocation"
        if pr.get("kind") == "wrong-home" and len(rs) == 1 and wrong_home_proof(pr.get("chain", []), rs[0], server_keys): return "adopted", "wrong-home"
        if (pr.get("kind") == "future-time" and len(rs) == 1 and pr.get("observedAt") and verify_receipt(rs[0], server_keys)
                and secs(rs[0]["time"]) > secs(pr["observedAt"]) + MAX_CLOCK_SKEW): return "adopted", "future-time"
        return "ignored", "the proof does not check against our keys"
    return "ignored", "not a gossip signal"

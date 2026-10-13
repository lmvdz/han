"""HAN v1 reference for the pure parts of the `han/1` carrier (architecture.md §8.2, round 31 X9–X16, X18): no QUIC stack needed.

  admit(conn, spaces)            N-02: the peer filter, a pure function of the connection's facts and the spaces a node serves (X9)
  encode_frame / decode_frame    G-07: a 4-byte big-endian length, then that many bytes of UTF-8 I-JSON
  request_verdict(text)          G-07 (1), (1a), (2) for one frame of a call stream (ref/wire.py frame_verdict, with han/1's calls)
  challenge_verdict(...)         G-07, G-08: the client's check of the control stream and of the node it reached
  hello_message(challenge, dialed)   G-08: the bytes a hello proof signs
  parse_wellknown / parse_ticket §4.3, N-03: node ids with their direct addresses and relays (X10), a ticket's server (X13)
  stream_item_verdict / stream_right   T-07, G-05: StreamItem shapes, `profile` items (X11), `han.status/1` bodies, and the right
                                 each type needs
  bulk_message / verify_bulk     X15: the bytes a bulk batch's signature covers, and its check
Signatures use ref/wire.py's strict Ed25519 (§2). The vectors are in conformance/carrier-vectors/ (ref/make_carrier_vectors.py writes
them, ref/run_carrier_vectors.py checks them)."""
from __future__ import annotations
import ipaddress, json, re
import policy as P
import wire as W

READ_ROLES = P.READ_ROLES
MAX_FRAME_BYTES = W.MAX_FRAME_BYTES
READ_CALLS = ("read", "subscribe", "state", "blobGet")       # the calls a stranger may make on a public or profile space (X9)

# ───────────────────────── N-02: admit (X9) ─────────────────────────
def admit(conn: dict, spaces: dict) -> dict:
    """N-02, the peer filter, judged per call before the call's own checks (G-07 (3)).
    conn:   {node, call, space?, hello?: {who, nodes}}: the peer's node id (its TLS key); the call's name; the space it names (`args.space`,
            or `entry.space` of an append); and after a successful `hello` the authenticated actor and the node ids that vouch for it:
            the `valid` keys of its pinned profile (N-01), or for a server actor `@:S` the `sequencer` node ids of S's /.well-known/han.
    spaces: {SpaceId: {policy: {visibility, join, ...}, roles: {actor: role}}}: the spaces this node serves (han.ts `AdmitSpaces`, a
            Pick of each SpaceState). A profile space's roles are its actor and `@:server`, so serving an actor's profile makes its
            devices members here.
    Returns {allow: True, tier} with tier "member" (every call; the call's own rules judge it), "server" (likewise, for a server's
    node), or "open" (a stranger's call, rate-limited per node by the host), or {allow: False, reason}. Nothing else is read."""
    call, hello = conn["call"], conn.get("hello")
    if call == "hello": return {"allow": True, "tier": "open"}                       # any node may say hello (X9)
    if not hello: return {"allow": False, "reason": "hello first"}                   # G-07 (3); `join` too (X12)
    who, bound = hello["who"], conn["node"] in hello["nodes"]
    if bound and who.startswith("@:"): return {"allow": True, "tier": "server"}     # G-10: a node listed by the server it speaks for
    if bound and any(s["roles"].get(who) in READ_ROLES for s in spaces.values()):
        return {"allow": True, "tier": "member"}                                     # a registered device of a role holder here (N-01)
    if call in ("join", "cancel"): return {"allow": True, "tier": "open"}            # N-03 after hello; ending one's own stream
    sp = conn.get("space")
    if call in READ_CALLS:
        s = spaces.get(sp)
        if s is None or sp.startswith("profile:") or s["policy"]["visibility"] == "public":
            return {"allow": True, "tier": "open"}                                   # public reads (P-02); a space not held: §4.1 step 2
        return {"allow": False, "reason": "members only"}
    if call == "append" and sp in spaces and spaces[sp]["policy"]["join"] == "open":
        return {"allow": True, "tier": "open"}                                       # an open join (P-03) is a stranger's append
    return {"allow": False, "reason": "not a member's device"}

# ───────────────────────── G-07: frames ─────────────────────────
def encode_frame(text: str) -> bytes:
    """A frame on a stream: the 4-byte big-endian length of the UTF-8 bytes, then the bytes."""
    b = text.encode("utf-8")
    return len(b).to_bytes(4, "big") + b

def decode_frame(buf: bytes, max_bytes: int = MAX_FRAME_BYTES):
    """("frame", text, rest) for a complete frame; ("more",) when the buffer holds less than one frame; ("close", reason) when the length
    exceeds max_bytes (judged from the 4 length bytes alone, before any payload arrives) or the payload isn't UTF-8."""
    if len(buf) < 4: return ("more",)
    n = int.from_bytes(buf[:4], "big")
    if n > max_bytes: return ("close", "frame too long")
    if len(buf) < 4 + n: return ("more",)
    try:
        text = buf[4:4 + n].decode("utf-8")
    except UnicodeDecodeError:
        return ("close", "not UTF-8")
    return ("frame", text, buf[4 + n:])

def request_verdict(text: str, test_mode: bool = False):
    """G-07 (1), (1a), (2) for a frame on a call stream: None if it is a request frame, else (id, closes)."""
    return W.frame_verdict(text, test_mode)

# ───────────────────────── G-07, G-08: the control stream ─────────────────────────
CHALLENGE = re.compile(r"^[A-Za-z0-9_-]{22,128}$", re.ASCII)

def challenge_verdict(frames: list, dialed: str, node: str, nodes: list):
    """The client's checks before it sends `hello` (G-07, G-08): `frames` are the texts of the control stream so far. The first is the
    bare Challenge, exactly {challenge, server, versions, time}, with a challenge of 22–128 base64url characters, versions holding 1 and
    a Time; its `server` equals `dialed`; the node reached is one of `nodes` (dialed's well-known `sequencer` node ids, or a ticket's
    node for a space whose `server` is dialed). Any further control frame is a protocol error. Returns "ok" or ("abort", reason)."""
    if not frames: return ("abort", "no challenge")
    if len(frames) > 1: return ("abort", "the control stream carries only the challenge")
    try:
        c = json.loads(frames[0], object_pairs_hook=W._pairs, parse_float=W._bad, parse_constant=W._bad, parse_int=W._int_form)
    except ValueError:
        return ("abort", "not I-JSON")
    if not isinstance(c, dict) or set(c) != {"challenge", "server", "versions", "time"}: return ("abort", "not a bare Challenge")
    if not (isinstance(c["challenge"], str) and CHALLENGE.fullmatch(c["challenge"])): return ("abort", "challenge grammar")
    if not (isinstance(c["versions"], list) and 1 in c["versions"] and all(P._int(v) for v in c["versions"])): return ("abort", "no version 1")
    if not P._time(c["time"]): return ("abort", "time")
    if c["server"] != dialed: return ("abort", "server is not the dialed ServerName")
    if node not in nodes: return ("abort", "node is not one of dialed's")
    return "ok"

def hello_message(challenge: str, dialed: str) -> bytes:
    return ("han-hello-v1\n" + challenge + "\n" + dialed).encode("utf-8")

# ───────────────────────── §4.3, N-03: well-known and tickets (X10, X13) ─────────────────────────
RELAY = re.compile(r"^https://[!-~]{1,1016}$", re.ASCII)
MAX_ADDRS = 16

def node_id_ok(v) -> bool:
    """N-01: a node id is a public key HAN accepts: 43 characters of canonical base64url, a curve point not of small order (§2)."""
    try:
        return isinstance(v, str) and len(v) == 43 and W.strict_key(W.unb64(v)) is None
    except ValueError:
        return False

def addr_ok(v) -> bool:
    """`ip:port`: an IPv4 address, or an IPv6 address in brackets, then a port 1–65535, all in canonical text form."""
    if not isinstance(v, str): return False
    m = re.fullmatch(r"(\[([0-9a-f:.]+)\]|([0-9.]+)):([1-9][0-9]{0,4})", v, re.ASCII)
    if not m or int(m.group(4)) > 65535: return False
    try:
        ip = ipaddress.ip_address(m.group(2) or m.group(3))
    except ValueError:
        return False
    return (ip.version == 6) == bool(m.group(2)) and str(ip) == (m.group(2) or m.group(3))

def node_addr_verdict(v) -> str | None:
    """han.ts NodeAddr {node, addrs, relays}: a node id, at most 16 direct addresses and 16 relay URLs (https, printable ASCII, at most
    1024 characters), at least one of the two, no repeats."""
    if not (isinstance(v, dict) and set(v) == {"node", "addrs", "relays"}): return "NodeAddr members"
    if not node_id_ok(v["node"]): return "node id"
    for k, ok in (("addrs", addr_ok), ("relays", lambda u: isinstance(u, str) and bool(RELAY.fullmatch(u)))):
        x = v[k]
        if not (isinstance(x, list) and len(x) <= MAX_ADDRS and all(ok(a) for a in x) and len(set(x)) == len(x)): return k
    if not v["addrs"] and not v["relays"]: return "no address and no relay"
    return None

def parse_wellknown(doc) -> str | None:
    """§4.3 `/.well-known/han` (han.ts WellKnownHan): {server, sequencer: NodeAddr[] (1–16, distinct nodes), keys}. None if well formed."""
    if not (isinstance(doc, dict) and set(doc) == {"server", "sequencer", "keys"}): return "members"
    if not (isinstance(doc["server"], str) and P.SERVER.fullmatch(doc["server"])): return "server"
    seq = doc["sequencer"]
    if not (isinstance(seq, list) and 1 <= len(seq) <= 16): return "sequencer: 1 to 16 nodes"
    for n in seq:
        err = node_addr_verdict(n)
        if err: return f"sequencer: {err}"
    if len({n["node"] for n in seq}) != len(seq): return "sequencer: a node twice"
    if not isinstance(doc["keys"], list) or not doc["keys"]: return "keys"
    for k in doc["keys"]:
        if not (isinstance(k, dict) and {"keyId", "publicKey", "from"} <= set(k) <= {"keyId", "publicKey", "from", "until", "compromised"}): return "key members"
        if not (P._keyid(k["keyId"]) and node_id_ok(k["publicKey"]) and P._time(k["from"])): return "key"
        if "until" in k and not (P._time(k["until"]) and k["until"] > k["from"]): return "key until"
        if "compromised" in k and k["compromised"] is not True: return "key compromised"
    return None

def ticket_text(t: dict) -> str:
    return "han1" + W.b64(P.jcs(t))

def parse_ticket(text):
    """N-03: "han1" + base64url(JCS(ticket)), ticket = {server, node: NodeAddr, space, secret}. Returns the ticket, or ("invalid", reason).
    The text must be exactly that form: canonical base64url of the ticket's JCS bytes."""
    if not (isinstance(text, str) and text.startswith("han1")): return ("invalid", "prefix")
    try:
        raw = W.unb64(text[4:])
        t = json.loads(raw.decode("utf-8"), object_pairs_hook=W._pairs, parse_float=W._bad, parse_constant=W._bad, parse_int=W._int_form)
    except (ValueError, UnicodeDecodeError):
        return ("invalid", "not base64url of I-JSON")
    if not (isinstance(t, dict) and set(t) == {"server", "node", "space", "secret"}): return ("invalid", "members")
    if P.jcs(t) != raw: return ("invalid", "not JCS")
    if not (isinstance(t["server"], str) and P.SERVER.fullmatch(t["server"])): return ("invalid", "server")
    err = node_addr_verdict(t["node"])
    if err: return ("invalid", f"node: {err}")
    if not P._space(t["space"]) or t["space"].startswith("profile:"): return ("invalid", "space")
    try:
        if len(W.unb64(t["secret"])) != 32: return ("invalid", "secret")
    except ValueError:
        return ("invalid", "secret")
    return t

# ───────────────────────── T-07, G-05: stream items (X11) ─────────────────────────
PROFILE_NAME = re.compile(r"^[a-z][a-z0-9-]{0,31}(\.[a-z0-9-]{1,32}){1,4}/[1-9][0-9]{0,5}$", re.ASCII)
MAX_STREAM_ITEM_BYTES = 65536
STATUS_STATES = ("working", "waiting-approval", "blocked", "idle", "done", "stale", "failed")

def status_body_verdict(b) -> str | None:
    """spec/profiles/han.status-1.md: the members a face relies on; unknown members are ignored."""
    if not isinstance(b, dict): return "body is not an object"
    req = {"session": P._id, "agent": P._actor, "harness": lambda v: isinstance(v, str), "state": lambda v: v in STATUS_STATES,
           "step": lambda v: isinstance(v, str), "files": lambda v: isinstance(v, list), "seq": lambda v: P._int(v) and v >= 1, "at": P._time}
    for k, ok in req.items():
        if k not in b: return f"missing {k}"
        if not ok(b[k]): return f"bad {k}"
    for k, ok in (("delegate_of", P._actor), ("issue", P._external)):
        if k in b and not ok(b[k]): return f"bad {k}"
    return None

PROFILE_BODIES = {"han.status/1": status_body_verdict}

def stream_item_verdict(item) -> str | None:
    """G-05's shape check of a StreamItem (han.ts): the common members, then one of output, status, context, profile; JCS at most
    maxStreamItemBytes. A `profile` item's body is any Json (§2) that its profile, when known here, accepts."""
    if not isinstance(item, dict): return "not an object"
    common = {"space", "session", "n", "time", "type"}
    if not common <= set(item): return "missing members"
    if not (P._space(item["space"]) and P._id(item["session"]) and P._int(item["n"]) and item["n"] >= 1 and P._time(item["time"])): return "common members"
    t, rest = item["type"], set(item) - common
    if t == "output":
        if rest != {"text"} or not isinstance(item["text"], str): return "output"
    elif t == "status":
        if not ({"state"} <= rest <= {"state", "detail"}) or item["state"] not in ("idle", "working", "blocked", "done", "error"): return "status"
        if "detail" in item and not isinstance(item["detail"], str): return "status detail"
    elif t == "context":
        if rest != {"blob", "what"} or not W._blobref(item["blob"]) or item["what"] not in ("transcript", "files", "plan"): return "context"
    elif t == "profile":
        if rest != {"name", "body"} or not (isinstance(item["name"], str) and PROFILE_NAME.fullmatch(item["name"])): return "profile name"
        if not P.json_ok(item["body"]): return "profile body is not Json"
        check = PROFILE_BODIES.get(item["name"])
        if check:
            err = check(item["body"])
            if err: return f"{item['name']}: {err}"
    else:
        return "unknown type"
    if len(P.jcs(item)) > MAX_STREAM_ITEM_BYTES: return "too large"
    return None

def stream_right(item) -> str:
    """G-05: the sessionAccess right an item needs to reach a watcher: `context` needs read, every other type (output, status,
    profile) watch."""
    return "read" if item["type"] == "context" else "watch"

# ───────────────────────── X15: bulk batches ─────────────────────────
def bulk_message(profile: str, data: bytes) -> bytes:
    """The bytes a bulk batch's signature covers: "han/bulk/1\\n" ‖ UTF-8(profile name) ‖ "\\n" ‖ the batch's exact CBOR bytes."""
    return b"han/bulk/1\n" + profile.encode("utf-8") + b"\n" + data

def verify_bulk(batch: dict, view: dict) -> bool:
    """A bulk batch {author, keyId, profile, data (bytes), sig} is genuine when `keyId` is a `valid` key of `view` (the author's pinned
    ProfileView) and `sig` verifies strictly over bulk_message(profile, data)."""
    if not (isinstance(batch.get("profile"), str) and PROFILE_NAME.fullmatch(batch["profile"])): return False
    k = view["keys"].get(batch["keyId"])
    if not k or k["status"] != "valid": return False
    return W.verify_sig(k["publicKey"], batch["sig"], bulk_message(batch["profile"], batch["data"]))

"""Writes conformance/carrier-vectors/*.json: offline vectors for the pure parts of the `han/1` carrier (round 31, X16, X18).
Expectations are hand-written from spec/core/architecture.md (§2, §4.3, G-05, G-07, G-08, N-01…N-03, X15), NOT computed by carrier.py;
only keys, signatures and encodings (bytes no one writes by hand) are computed here. ref/run_carrier_vectors.py checks them against
ref/carrier.py; any implementation can check them the same way (conformance/carrier-vectors/README.md)."""
import json, os, sys
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import policy as P
import wire as W

OUT = os.path.join(os.path.dirname(HERE), "conformance", "carrier-vectors")
FILES = {}

def key(name): return W.Key("han-carrier-" + name)
ALICE, BOB, SRV, SRVB, STRANGER = key("alice"), key("bob"), key("server-a"), key("server-b"), key("stranger")
A, B, Z = "@alice:a.example", "@bob:b.example", "@zed:c.example"

def pol(**kw):
    p = {"visibility": "members", "join": "invite", "membersMayInvite": False, "allowAgents": "any", "approvals": [], "separateApprover": False,
         "budget": None, "maxDelegationDepth": 4, "minDurability": "local", "launchWindow": 30, "extensionsMinRole": "member",
         "sessions": {"watch": "member", "read": "member", "steer": "owner", "modes": ["auto", "guarded"]}, "servers": {"deny": []}}
    p.update(kw); return p

# ───────────────────────── admit.json (N-02, X9, X12) ─────────────────────────
SPACES = {
    "team:a.example": {"policy": pol(), "roles": {A: "owner", "@gone:a.example": "banned"}},
    "pub:a.example": {"policy": pol(visibility="public"), "roles": {A: "owner"}},
    "club:a.example": {"policy": pol(join="open"), "roles": {A: "owner"}},
    "profile:@alice:a.example": {"policy": pol(visibility="public", join="closed"), "roles": {A: "owner", "@:a.example": "owner"}},
}
def conn(node, call, space=None, who=None, nodes=None):
    c = {"node": node.pub if isinstance(node, W.Key) else node, "call": call}
    if space: c["space"] = space
    if who: c["hello"] = {"who": who, "nodes": [n.pub for n in nodes]}
    return c
OPEN, MEMBER, SERVER, NO = {"allow": True, "tier": "open"}, {"allow": True, "tier": "member"}, {"allow": True, "tier": "server"}, {"allow": False}
FILES["admit"] = {"name": "admit", "rules": ["N-02", "G-07", "X9", "X12"], "function": "admit(conn, spaces)",
    "description": "the peer filter: any node may say hello; nothing else before it; a node that vouches for its hello actor is admitted as server (a server actor) or member (a role holder in a space served here, its own profile included); anyone else only for join, cancel, reads of public, profile and not-held spaces, and appends to open-join spaces; `expect` compares allow and tier (the reason is informative)",
    "spaces": SPACES, "cases": [
    {"name": "hello from any node", "conn": conn(STRANGER, "hello"), "expect": OPEN},
    {"name": "join before hello (X12)", "conn": conn(STRANGER, "join", "team:a.example"), "expect": NO},
    {"name": "read before hello", "conn": conn(ALICE, "read", "pub:a.example"), "expect": NO},
    {"name": "a member's device: any call", "conn": conn(ALICE, "append", "team:a.example", A, [ALICE]), "expect": MEMBER},
    {"name": "a member's device, a space it isn't in: the call's rules judge", "conn": conn(ALICE, "watch", "other:a.example", A, [ALICE]), "expect": MEMBER},
    {"name": "the member's actor on a node its profile doesn't list", "conn": conn(STRANGER, "append", "team:a.example", A, [ALICE]), "expect": NO},
    {"name": "the same, reading a public space: open", "conn": conn(STRANGER, "read", "pub:a.example", A, [ALICE]), "expect": OPEN},
    {"name": "a banned actor is no member", "conn": conn(BOB, "append", "team:a.example", "@gone:a.example", [BOB]), "expect": NO},
    {"name": "a server's listed node", "conn": conn(SRVB, "host", "team:a.example", "@:b.example", [SRVB]), "expect": SERVER},
    {"name": "a server's notice (G-03) to a server that never federated", "conn": conn(SRVB, "signal", "profile:@alice:a.example", "@:b.example", [SRVB]), "expect": SERVER},
    {"name": "a server actor on a node its well-known doesn't list", "conn": conn(STRANGER, "host", "team:a.example", "@:b.example", [SRVB]), "expect": NO},
    {"name": "a stranger reads a public space", "conn": conn(BOB, "read", "pub:a.example", B, [BOB]), "expect": OPEN},
    {"name": "a stranger subscribes to a public space", "conn": conn(BOB, "subscribe", "pub:a.example", B, [BOB]), "expect": OPEN},
    {"name": "a stranger reads a profile", "conn": conn(BOB, "state", "profile:@alice:a.example", B, [BOB]), "expect": OPEN},
    {"name": "a stranger fetches a public blob", "conn": conn(BOB, "blobGet", "pub:a.example", B, [BOB]), "expect": OPEN},
    {"name": "a stranger reads a space not held here: routing answers", "conn": conn(BOB, "read", "elsewhere:c.example", B, [BOB]), "expect": OPEN},
    {"name": "a stranger reads a members-only space", "conn": conn(BOB, "read", "team:a.example", B, [BOB]), "expect": NO},
    {"name": "a stranger signals a public space", "conn": conn(BOB, "signal", "pub:a.example", B, [BOB]), "expect": NO},
    {"name": "a stranger appends to a public invite-only space", "conn": conn(BOB, "append", "pub:a.example", B, [BOB]), "expect": NO},
    {"name": "a stranger joins an open space (P-03)", "conn": conn(BOB, "append", "club:a.example", B, [BOB]), "expect": OPEN},
    {"name": "join after hello (N-03)", "conn": conn(BOB, "join", "team:a.example", B, [BOB]), "expect": OPEN},
    {"name": "cancel after hello", "conn": conn(BOB, "cancel", None, B, [BOB]), "expect": OPEN},
    {"name": "a stranger watches a session", "conn": conn(BOB, "watch", "pub:a.example", B, [BOB]), "expect": NO},
    {"name": "a stranger uploads", "conn": conn(BOB, "blobPut", None, B, [BOB]), "expect": NO},
    {"name": "a stranger invites", "conn": conn(BOB, "invite", "team:a.example", B, [BOB]), "expect": NO},
    {"name": "the actor whose profile is served here is a member", "conn": conn(ALICE, "blobPut", None, A, [ALICE]),
     "spaces": {"profile:@alice:a.example": SPACES["profile:@alice:a.example"]}, "expect": MEMBER},
]}

# ───────────────────────── frames.json (G-07, §2, X18) ─────────────────────────
def enc_hex(t): return (len(t.encode()).to_bytes(4, "big") + t.encode()).hex()
NEST = lambda n: "[" * n + "]" * n
FILES["frames"] = {"name": "frames", "rules": ["G-07", "§2", "X18"],
    "description": "a frame is a 4-byte big-endian length, then that many bytes of UTF-8 I-JSON; a too-long length or non-UTF-8 payload closes the connection; then G-07 (1), (1a), (2) for a request frame: `null` means it is a request frame, `[id, close]` the id of the `invalid` reply and whether the connection closes",
    "encode": [
        {"text": '{"id":1,"call":"hello","args":{}}', "hex": enc_hex('{"id":1,"call":"hello","args":{}}')},
        {"text": '{"a":"é"}', "hex": "0000000a" + '{"a":"é"}'.encode().hex(), "note": "the length counts UTF-8 bytes"},
    ],
    "decode": [
        {"hex": "000000027b7d", "expect": {"frame": "{}", "rest": ""}},
        {"hex": "000000027b7d0000", "expect": {"frame": "{}", "rest": "0000"}},
        {"hex": "0000000a7b7d", "expect": "more"},
        {"hex": "000000", "expect": "more"},
        {"hex": "00140001", "expect": "close", "note": "1,310,721 bytes: over maxFrameBytes, judged from the length alone"},
        {"hex": "00140000", "expect": "more", "note": "exactly maxFrameBytes is allowed"},
        {"hex": "00000002c328", "expect": "close", "note": "not UTF-8"},
    ],
    "requests": [
        {"text": '{"id":1,"call":"read","args":{}}', "expect": None},
        {"text": '{"args":{},"call":"read","id":1}', "expect": None, "note": "X18: any key order is accepted"},
        {"text": '{"id":1,"call":"join","args":{}}', "expect": None, "note": "join is a call of han/1"},
        {"text": '{"id":1,"call":"invite","args":{}}', "expect": None},
        {"text": '{"id":-0,"call":"read","args":{}}', "expect": [0, True], "note": "X18: -0 is not I-JSON for HAN"},
        {"text": '{"id":1,"call":"read","args":{"after":-0}}', "expect": [0, True]},
        {"text": '{"id":1,"call":"read","args":{"after":0}}', "expect": None},
        {"text": '{"id":1,"call":"read","args":{"x":' + NEST(126) + '}}', "expect": None, "note": "depth 128: the frame, args and 126 arrays"},
        {"text": '{"id":1,"call":"read","args":{"x":' + NEST(127) + '}}', "expect": [0, True], "note": "depth 129"},
        {"text": '{"id":1,"id":2,"call":"read","args":{}}', "expect": [0, True], "note": "duplicate key"},
        {"text": '{"id":1,"call":"read","args":{"after":1.0}}', "expect": [0, True]},
        {"text": '{"id":9007199254740992,"call":"read","args":{}}', "expect": [0, True]},
        {"text": '{"id":3,"call":"read","args":{"after":9007199254740992}}', "expect": [3, False]},
        {"text": '{"id":4,"call":"nosuch","args":{}}', "expect": [4, False]},
        {"text": '{"id":5,"call":"read"}', "expect": [5, False]},
        {"text": '[1]', "expect": [0, True]},
        {"text": '{"id":6,"call":"test.clock","args":{}}', "expect": [6, False], "note": "not a call outside test mode"},
        {"text": '{"id":6,"call":"test.clock","args":{}}', "test_mode": True, "expect": None},
    ]}

# ───────────────────────── control.json (G-07, G-08) ─────────────────────────
CH = "A" * 43
def chal(**kw):
    c = {"challenge": CH, "server": "a.example", "versions": [1], "time": "2026-10-10T12:00:00Z"}
    c.update(kw); return json.dumps(c, separators=(",", ":"))
NODES = [SRV.pub]
msg = ("han-hello-v1\n" + CH + "\n" + "a.example").encode()
FILES["control"] = {"name": "control", "rules": ["G-07", "G-08", "N-03"],
    "description": "the client's checks of the control stream before it says hello: one bare Challenge (exactly challenge, server, versions, time), its server equal to the dialed ServerName, the node reached one of dialed's; and the bytes a hello proof signs",
    "challenges": [
        {"name": "well formed", "frames": [chal()], "dialed": "a.example", "node": SRV.pub, "nodes": NODES, "expect": "ok"},
        {"name": "a second frame on the control stream", "frames": [chal(), chal()], "dialed": "a.example", "node": SRV.pub, "nodes": NODES, "expect": "abort"},
        {"name": "an id on the challenge", "frames": [json.dumps({"id": 0, "challenge": CH, "server": "a.example", "versions": [1], "time": "2026-10-10T12:00:00Z"})], "dialed": "a.example", "node": SRV.pub, "nodes": NODES, "expect": "abort"},
        {"name": "another server's name", "frames": [chal(server="b.example")], "dialed": "a.example", "node": SRV.pub, "nodes": NODES, "expect": "abort"},
        {"name": "a node dialed's well-known doesn't list", "frames": [chal()], "dialed": "a.example", "node": SRVB.pub, "nodes": NODES, "expect": "abort"},
        {"name": "a ticket's node for its server (N-03)", "frames": [chal()], "dialed": "a.example", "node": SRVB.pub, "nodes": [SRVB.pub], "expect": "ok"},
        {"name": "a challenge of 21 characters", "frames": [chal(challenge="A" * 21)], "dialed": "a.example", "node": SRV.pub, "nodes": NODES, "expect": "abort"},
        {"name": "a challenge of 128 characters", "frames": [chal(challenge="A" * 128)], "dialed": "a.example", "node": SRV.pub, "nodes": NODES, "expect": "ok"},
        {"name": "no version 1", "frames": [chal(versions=[2])], "dialed": "a.example", "node": SRV.pub, "nodes": NODES, "expect": "abort"},
        {"name": "versions 1 and 2", "frames": [chal(versions=[1, 2])], "dialed": "a.example", "node": SRV.pub, "nodes": NODES, "expect": "ok"},
        {"name": "no challenge yet", "frames": [], "dialed": "a.example", "node": SRV.pub, "nodes": NODES, "expect": "abort"},
    ],
    "proofs": [
        {"challenge": CH, "dialed": "a.example", "message": msg.hex(), "publicKey": ALICE.pub, "proof": ALICE.sign(msg), "valid": True},
        {"challenge": CH, "dialed": "a.example", "message": msg.hex(), "publicKey": BOB.pub, "proof": ALICE.sign(msg), "valid": False, "note": "another key"},
        {"challenge": CH, "dialed": "b.example", "message": ("han-hello-v1\n" + CH + "\nb.example").encode().hex(), "publicKey": ALICE.pub, "proof": ALICE.sign(msg), "valid": False, "note": "signed for a.example"},
    ]}

# ───────────────────────── wellknown.json, tickets.json (§4.3, N-03, X10, X13) ─────────────────────────
def na(k, addrs=("192.0.2.1:7000",), relays=("https://relay.example/",)): return {"node": k.pub, "addrs": list(addrs), "relays": list(relays)}
SK = {"keyId": "h1", "publicKey": SRV.pub, "from": "2026-01-01T00:00:00Z"}
def wk(**kw):
    d = {"server": "a.example", "sequencer": [na(SRV)], "keys": [SK]}
    d.update(kw); return d
FILES["wellknown"] = {"name": "wellknown", "rules": ["§4.3", "N-01", "X10"],
    "description": "/.well-known/han: {server, sequencer: NodeAddr[] (1–16), keys}; a NodeAddr is a strict node id, at most 16 direct addresses (canonical IPv4, or IPv6 in brackets, then :port 1–65535) and 16 https relay URLs, at least one of the two",
    "cases": [
        {"name": "address and relay", "doc": wk(), "expect": "ok"},
        {"name": "relay only", "doc": wk(sequencer=[na(SRV, addrs=())]), "expect": "ok"},
        {"name": "IPv6 only", "doc": wk(sequencer=[na(SRV, addrs=("[2001:db8::1]:443",), relays=())]), "expect": "ok"},
        {"name": "two nodes", "doc": wk(sequencer=[na(SRV), na(SRVB, relays=())]), "expect": "ok"},
        {"name": "neither address nor relay", "doc": wk(sequencer=[na(SRV, addrs=(), relays=())]), "expect": "invalid"},
        {"name": "no node", "doc": wk(sequencer=[]), "expect": "invalid"},
        {"name": "round 30's bare node ids", "doc": wk(sequencer=[SRV.pub]), "expect": "invalid"},
        {"name": "the same node twice", "doc": wk(sequencer=[na(SRV), na(SRV)]), "expect": "invalid"},
        {"name": "a host name, not an address", "doc": wk(sequencer=[na(SRV, addrs=("a.example:443",))]), "expect": "invalid"},
        {"name": "an IPv4 address out of range", "doc": wk(sequencer=[na(SRV, addrs=("192.0.2.256:443",))]), "expect": "invalid"},
        {"name": "IPv6 without brackets", "doc": wk(sequencer=[na(SRV, addrs=("2001:db8::1:443",))]), "expect": "invalid"},
        {"name": "non-canonical IPv6", "doc": wk(sequencer=[na(SRV, addrs=("[2001:DB8::1]:443",))]), "expect": "invalid"},
        {"name": "port 0", "doc": wk(sequencer=[na(SRV, addrs=("192.0.2.1:0",))]), "expect": "invalid"},
        {"name": "port 65536", "doc": wk(sequencer=[na(SRV, addrs=("192.0.2.1:65536",))]), "expect": "invalid"},
        {"name": "a plain http relay", "doc": wk(sequencer=[na(SRV, relays=("http://relay.example/",))]), "expect": "invalid"},
        {"name": "a node id that is no curve point", "doc": wk(sequencer=[{"node": W.b64((2).to_bytes(32, "little")), "addrs": ["192.0.2.1:7000"], "relays": []}]), "expect": "invalid"},
        {"name": "an extra member", "doc": dict(wk(), url="wss://a.example/han"), "expect": "invalid"},
        {"name": "a ServerName of 254 characters", "doc": wk(server=".".join(["a" * 63] * 3 + ["b" * 62])), "expect": "invalid"},
    ]}
SECRET = W.b64(bytes(range(32)))
TK = {"server": "a.example", "node": na(SRV), "space": "team:a.example", "secret": SECRET}
def ttext(t): return "han1" + W.b64(P.jcs(t))
FILES["tickets"] = {"name": "tickets", "rules": ["N-03", "X13"],
    "description": "a ticket is \"han1\" + base64url(JCS({server, node: NodeAddr, space, secret})), exactly; `server` is the ServerName to dial (X13); `expect` is the parsed ticket or \"invalid\"",
    "cases": [
        {"name": "well formed", "text": ttext(TK), "expect": TK},
        {"name": "after a move: server names the new home, the space id the first", "text": ttext(dict(TK, server="b.example", node=na(SRVB))), "expect": dict(TK, server="b.example", node=na(SRVB))},
        {"name": "round 30's ticket, no server", "text": ttext({"node": SRV.pub, "space": "team:a.example", "secret": SECRET}), "expect": "invalid"},
        {"name": "not JCS (members out of order)", "text": "han1" + W.b64(json.dumps(TK, separators=(",", ":")).encode()), "expect": "invalid"},
        {"name": "padded base64url", "text": ttext(TK) + "=", "expect": "invalid"},
        {"name": "another prefix", "text": "han2" + ttext(TK)[4:], "expect": "invalid"},
        {"name": "a secret of 31 bytes", "text": ttext(dict(TK, secret=W.b64(bytes(31)))), "expect": "invalid"},
        {"name": "a profile space", "text": ttext(dict(TK, space="profile:@alice:a.example")), "expect": "invalid"},
    ]}

# ───────────────────────── stream-items.json (T-07, G-05, X11) ─────────────────────────
SESS = "ab" * 32
def item(**kw):
    d = {"space": "team:a.example", "session": SESS, "n": 1, "time": "2026-10-10T12:00:00Z"}
    d.update(kw); return d
STATUS = {"session": SESS, "agent": "@fixer:b.example", "harness": "claude-code", "state": "working", "step": "running tests", "files": [], "seq": 7, "at": "2026-10-10T12:00:00Z"}
deep = 1
for _ in range(33): deep = {"a": deep}
FILES["stream-items"] = {"name": "stream-items", "rules": ["T-07", "G-05", "X11", "§2"],
    "description": "StreamItem shapes: output, status, context, and profile {name, body} (X11); a han.status/1 body has the members its profile requires (unknown ones ignored); `right` is the sessionAccess right an item needs: read for context, watch for every other type",
    "cases": [
        {"name": "output", "item": item(type="output", text="hi"), "expect": "ok", "right": "watch"},
        {"name": "status", "item": item(type="status", state="working"), "expect": "ok", "right": "watch"},
        {"name": "context", "item": item(type="context", blob={"hash": "cd" * 32, "size": 10, "type": "text/plain"}, what="plan"), "expect": "ok", "right": "read"},
        {"name": "han.status/1 as a profile item", "item": item(type="profile", name="han.status/1", body=STATUS), "expect": "ok", "right": "watch"},
        {"name": "han.status/1 with an unknown member", "item": item(type="profile", name="han.status/1", body=dict(STATUS, mood="fine")), "expect": "ok", "right": "watch"},
        {"name": "han.status/1 without seq", "item": item(type="profile", name="han.status/1", body={k: v for k, v in STATUS.items() if k != "seq"}), "expect": "invalid"},
        {"name": "han.status/1 with a state outside the seven", "item": item(type="profile", name="han.status/1", body=dict(STATUS, state="asleep")), "expect": "invalid"},
        {"name": "an unknown profile: any Json body", "item": item(type="profile", name="team.board/2", body={"x": [1, 2]}), "expect": "ok", "right": "watch"},
        {"name": "a profile name without a version", "item": item(type="profile", name="han.status", body={}), "expect": "invalid"},
        {"name": "a profile body nested 33 deep", "item": item(type="profile", name="team.board/2", body=deep), "expect": "invalid"},
        {"name": "a profile item with an extra member", "item": item(type="profile", name="team.board/2", body={}, extra=1), "expect": "invalid"},
        {"name": "an unknown type", "item": item(type="bulk", body={}), "expect": "invalid"},
        {"name": "n = 0", "item": item(type="output", text="hi", n=0), "expect": "invalid"},
    ]}

# ───────────────────────── bulk.json (X15) ─────────────────────────
DATA = bytes.fromhex("a201016474657874") + b"\x00" * 3
def bulk(k, profile="han.timeline/1", data=DATA, keyId="d1", sign_profile=None, sign_data=None):
    m = b"han/bulk/1\n" + (sign_profile or profile).encode() + b"\n" + (data if sign_data is None else sign_data)
    return {"author": A, "keyId": keyId, "profile": profile, "data": data.hex(), "sig": k.sign(m)}
VIEW = {"actor": A, "keys": {"d1": {"publicKey": ALICE.pub, "status": "valid"}, "d0": {"publicKey": BOB.pub, "status": "revoked"}}}
FILES["bulk"] = {"name": "bulk", "rules": ["X15", "G-07", "§2"],
    "description": "a bulk batch's signature covers \"han/bulk/1\\n\" ‖ UTF-8(profile) ‖ \"\\n\" ‖ data (its exact CBOR bytes, hex here) and is made by a valid key of the author's pinned profile (`view`)",
    "view": VIEW, "message": (b"han/bulk/1\nhan.timeline/1\n" + DATA).hex(), "cases": [
        {"name": "genuine", "batch": bulk(ALICE), "valid": True},
        {"name": "another profile name", "batch": bulk(ALICE, profile="han.timeline/2", sign_profile="han.timeline/1"), "valid": False},
        {"name": "one byte changed", "batch": bulk(ALICE, data=DATA[:-1] + b"\x01", sign_data=DATA), "valid": False},
        {"name": "a revoked key", "batch": bulk(BOB, keyId="d0"), "valid": False},
        {"name": "a key not in the profile", "batch": bulk(ALICE, keyId="d9"), "valid": False},
        {"name": "the name and the data run together (no separator)", "batch": dict(bulk(ALICE), sig=ALICE.sign(b"han/bulk/1\nhan.timeline/1" + DATA)), "valid": False},
    ]}

# ───────────────────────── ed25519.json (§2, X18) ─────────────────────────
L = W._L
MSG = b"han strict ed25519"
good = W.unb64(ALICE.sign(MSG))
s_plus_l = good[:32] + (int.from_bytes(good[32:], "little") + L).to_bytes(32, "little")
IDENT = (1).to_bytes(32, "little")                  # the identity point, order 1
ORDER4 = bytes(32)                                  # y = 0: a point of order 4
NONCANON = (W._P + 1).to_bytes(32, "little")        # y = p + 1 ≥ p: the identity's non-canonical encoding
NEG0 = (1).to_bytes(32, "little")[:31] + b"\x80"    # y = 1, x = 0 with the sign bit set
NOTPT = (2).to_bytes(32, "little")                  # y = 2: no x on the curve
def small_r_sig(seed: str, msg: bytes) -> bytes:
    """A signature that passes the bare cofactorless equation with R = the identity (small order): S = k·a mod L, k = H(R ‖ A ‖ M).
    Only an implementation that rejects small-order R refuses it."""
    import hashlib
    h = hashlib.sha512(hashlib.sha256(seed.encode()).digest()).digest()
    a = int.from_bytes(h[:32], "little")
    a &= (1 << 254) - 8; a |= 1 << 254
    A = W.unb64(W.Key(seed).pub)
    k = int.from_bytes(hashlib.sha512(IDENT + A + msg).digest(), "little") % L
    return IDENT + (k * a % L).to_bytes(32, "little")

FILES["ed25519"] = {"name": "ed25519", "rules": ["§2", "R-12", "N-01", "X18"],
    "description": "strict Ed25519: a public key must be the canonical encoding of a curve point not of small order (`key`: ok or invalid, judged where the key is parsed); a signature verifies only with R canonical and not of small order, S below L, and the cofactorless equation (`valid`). msg is hex; keys and signatures base64url",
    "cases": [
        {"name": "genuine", "publicKey": ALICE.pub, "msg": MSG.hex(), "sig": W.b64(good), "key": "ok", "valid": True},
        {"name": "another message", "publicKey": ALICE.pub, "msg": (MSG + b"!").hex(), "sig": W.b64(good), "key": "ok", "valid": False},
        {"name": "S + L (malleable, non-canonical S)", "publicKey": ALICE.pub, "msg": MSG.hex(), "sig": W.b64(s_plus_l), "key": "ok", "valid": False},
        {"name": "the identity as key, R = identity, S = 0: passes the bare equation for every message", "publicKey": W.b64(IDENT), "msg": MSG.hex(), "sig": W.b64(IDENT + bytes(32)), "key": "invalid", "valid": False},
        {"name": "a key of order 4", "publicKey": W.b64(ORDER4), "msg": MSG.hex(), "sig": W.b64(good), "key": "invalid", "valid": False},
        {"name": "a non-canonical key encoding (y = p + 1)", "publicKey": W.b64(NONCANON), "msg": MSG.hex(), "sig": W.b64(good), "key": "invalid", "valid": False},
        {"name": "x = 0 with the sign bit set", "publicKey": W.b64(NEG0), "msg": MSG.hex(), "sig": W.b64(good), "key": "invalid", "valid": False},
        {"name": "not a curve point (y = 2)", "publicKey": W.b64(NOTPT), "msg": MSG.hex(), "sig": W.b64(good), "key": "invalid", "valid": False},
        {"name": "R of small order", "publicKey": ALICE.pub, "msg": MSG.hex(), "sig": W.b64(IDENT + good[32:]), "key": "ok", "valid": False},
        {"name": "R of small order that satisfies the bare equation", "publicKey": ALICE.pub, "msg": MSG.hex(), "sig": W.b64(small_r_sig("han-carrier-alice", MSG)), "key": "ok", "valid": False},
        {"name": "a 63-byte signature", "publicKey": ALICE.pub, "msg": MSG.hex(), "sig": W.b64(good[:63]), "key": "ok", "valid": False},
    ]}

# ───────────────────────── header.json (§2, R-25, X18) ─────────────────────────
def header(nonce):
    return {"v": 1, "space": "team:a.example", "kind": "message", "author": A, "keyId": "k1", "body": {}, "contentHash": "", "refs": [], "seen": 0,
            "nonce": nonce, "time": "2026-10-10T12:00:00Z", "sig": "x"}
def keyadd(pub):
    return dict(header("a" * 26), space="profile:@alice:a.example", kind="key.add", body={"keyId": "d2", "publicKey": pub})
FILES["header"] = {"name": "header", "rules": ["§2", "R-09", "R-25", "X18"],
    "description": "Entry header checks the Log makes (step 3): a nonce is 26 characters of [a-z2-7] (130 bits), every character free, the last one's two low bits included; a public key in a key.add is a strict Ed25519 key (§2), else invalid at parse. `expect` ok or invalid",
    "cases": [
        {"name": "key.add of a genuine key", "entry": keyadd(BOB.pub), "expect": "ok"},
        {"name": "key.add of a small-order key", "entry": keyadd(W.b64(bytes(32))), "expect": "invalid"},
        {"name": "key.add of a key that is no curve point", "entry": keyadd(W.b64((2).to_bytes(32, "little"))), "expect": "invalid"},
        {"name": "key.add of 31 bytes", "entry": keyadd(W.b64(bytes(31))), "expect": "invalid"},
        {"name": "last character a (low bits 0)", "entry": header("a" * 26), "expect": "ok"},
        {"name": "last character 7 (low bits 11)", "entry": header("a" * 25 + "7"), "expect": "ok"},
        {"name": "last character z", "entry": header("a" * 25 + "z"), "expect": "ok"},
        {"name": "25 characters", "entry": header("a" * 25), "expect": "invalid"},
        {"name": "27 characters", "entry": header("a" * 27), "expect": "invalid"},
        {"name": "uppercase", "entry": header("A" * 26), "expect": "invalid"},
        {"name": "a digit outside 2-7", "entry": header("a" * 25 + "1"), "expect": "invalid"},
    ]}

if __name__ == "__main__":
    os.makedirs(OUT, exist_ok=True)
    for f in os.listdir(OUT):
        if f.endswith(".json"): os.remove(os.path.join(OUT, f))
    for name, vec in FILES.items():
        with open(os.path.join(OUT, f"{name}.json"), "w", encoding="utf-8") as fh:
            json.dump(vec, fh, indent=1, ensure_ascii=False)
    print(f"wrote {len(FILES)} carrier vector files")

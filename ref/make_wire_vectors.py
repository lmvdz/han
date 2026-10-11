"""Builds ref/wire-vectors/ from interop/golden/ (D15).  python ref/make_wire_vectors.py
One-time conversion, kept for provenance; running the vectors needs only ref/wire-vectors/ and ref/run_wire_vectors.py.

* the 21 golden operations become one file each (01-hello.json ... 21-redact.json), in the order the scenario ran, because each file builds on the
  state the earlier ones left (seq numbers, ids);
* probes.json = the error paths both reference servers answered alike and the spec text confirms, re-derived so they do not depend on the
  state of a particular probe run (the entries are signed here with the fixed test keys);
* probes-decided.json = the paths where the two servers differed or the text was silent and a decision (DECISIONS-r21.md D1..D18) now fixes the
  answer. They are conformance vectors too: an implementation that has not yet converged fails them.
Format: ref/wire-vectors/README.md."""
import base64, hashlib, json, os, sys
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import policy as P, wire as W
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives import serialization

GOLD = os.path.join(ROOT, "interop", "golden")
OUT = os.path.join(HERE, "wire-vectors")
SERVER = "a.localhost"
T0 = "2026-01-01T00:00:00Z"
NAMES = ("alice", "bob", "carol", "vera", "x")

class K:
    def __init__(self, seed_text, key_id):
        self.seed_text, self.key_id = seed_text, key_id
        self.sk = Ed25519PrivateKey.from_private_bytes(hashlib.sha256(seed_text.encode()).digest())
        self.pub = W.b64(self.sk.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw))
    def sign(self, msg): return W.b64(self.sk.sign(msg))

KEYS = {n: K("han-interop-key-" + n, "k1") for n in NAMES}
KEYS["server"] = K("han-interop-key-server", "h1")
A = lambda n: f"@{n}:{SERVER}"

CONFIG = {
    "format": "han-wire-config/1",
    "server": SERVER, "clock": T0,
    "note": "Test mode (architecture.md G-11): a frozen clock, this server key, these actors, an empty data directory. Private key of a key = sha256(UTF-8 seed), 32 raw bytes.",
    "serverKey": {"keyId": "h1", "seed": KEYS["server"].seed_text, "publicKey": KEYS["server"].pub, "from": "1970-01-01T00:00:00Z"},
    "actors": [{"actor": A(n), "kind": "service" if n == "x" else "human", "keyId": "k1", "seed": KEYS[n].seed_text, "publicKey": KEYS[n].pub} for n in NAMES],
    "blobs": {
        "b1": {"hash": None, "size": 2000, "type": "application/octet-stream"},
        "b2": {"hash": None, "size": 1700000, "type": "application/octet-stream"},
        "generator": "byte i of a blob is (i*7+3) mod 251",
    },
    "limits": {"maxFrameBytes": 1310720, "maxHeaderBytes": 16384, "maxContentBytes": 65536, "maxBlobBytes": 4294967296, "maxBlobPartBytes": 786432,
               "maxBlobRangeBytes": 786432, "maxStreamItemBytes": 65536, "maxFollows": 256, "maxClockSkewSeconds": 60},
    "defaults": {"expectTimeoutMs": 5000, "settleMs": 250, "silenceMs": 400},
}
for k in ("b1", "b2"):
    n = CONFIG["blobs"][k]["size"]
    CONFIG["blobs"][k]["hash"] = hashlib.sha256(bytes((i * 7 + 3) % 251 for i in range(n))).hexdigest()
LIMITS = CONFIG["limits"]

def load(name): return json.load(open(os.path.join(GOLD, name + ".json"), encoding="utf-8"))
def dump(path, obj): open(path, "w", encoding="utf-8", newline="\n").write(json.dumps(obj, indent=1, ensure_ascii=False) + "\n")

def steps_of(frames):
    out = []
    for f in frames:
        if "frame" not in f: continue
        fr = f["frame"]
        if isinstance(fr, dict) and "unparseable" in fr: out.append({"conn": f["conn"], "send_raw": fr["unparseable"]}); continue
        out.append({"conn": f["conn"], "send" if f["dir"] == "c2s" else "expect": fr})
    return out

# ───────────── the 21 operations ─────────────
RULES = {
    "hello": ["G-07", "G-08", "W-01", "W-02", "D9", "D12"], "space.create": ["§4.1", "§2"], "member.set": ["§4.1", "P-03"], "message": ["§2", "R-09"],
    "read": ["§3", "G-09"], "read.state": ["§3", "G-09", "P-02"], "follow": ["§3", "G-09", "W-06"], "subscribe": ["G-09"], "signal": ["G-04", "G-09", "W-24"],
    "propose": ["§5", "P-07"], "approve": ["P-08"], "blobPut": ["G-01", "W-27"], "blobGet": ["G-01", "W-29"], "blobPut.multipart": ["G-01", "W-27"],
    "blobGet.multipart": ["G-01", "W-29"], "session.open": ["P-15"], "session.input": ["P-15"], "stream+watch": ["G-05", "W-31", "W-32"],
    "session.close": ["P-15"], "task": ["P-04"], "redact": ["P-12", "R-04"],
}
index = {"format": "han-wire-index/1", "config": "config.json", "note": "Run the files in this order against one fresh server in test mode: each builds on the state the earlier ones left.", "files": []}
n = 0
for op in load("index")["ops"]:
    name = op["op"]; d = load(name.replace("+", "_"))
    n += 1; fname = f"{n:02d}-{name.replace('+', '_')}.json"
    if name == "hello":
        steps = [
            {"conn": A("alice"), "expect": {"challenge": "<challenge>", "server": SERVER, "versions": [1], "time": T0}},
            {"conn": A("alice"), "send": {"id": 1, "call": "hello", "args": {"who": A("alice"), "keyId": "k1", "proof": "<proof>", "version": 1}}},
            {"conn": A("alice"), "expect": {"id": 1, "ok": {"limits": LIMITS, "durability": "local", "time": T0, "knownHeads": []}}},
        ]
    else:
        steps = steps_of(d["clients"][d["vectorFrom"]]["frames"])
    vec = {"format": "han-wire-vector/1", "name": name, "description": d["description"], "rules": RULES[name], "steps": steps}
    dump(os.path.join(OUT, fname), vec)
    index["files"].append(fname)

# ───────────── helpers for new probes ─────────────
state = {"head": 18, "nonce": 0}
def nonce(tag):
    return base64.b32encode(hashlib.sha256(("wire-vector-nonce-" + tag).encode()).digest()).decode().lower().rstrip("=")[:26]

def expand(v):
    if isinstance(v, str) and v.startswith("<repeat "):
        _, c, k = v.strip("<>").split(); return c * int(k)
    if isinstance(v, dict): return {a: expand(b) for a, b in v.items()}
    if isinstance(v, list): return [expand(b) for b in v]
    return v

def mk(actor, kind, body, content=None, space="s1:" + SERVER, tag="", seen=0, refs=(), key_id=None):
    """A signed entry (header, content). `content` may hold "<repeat c n>" placeholders (expanded for the hash, left as the placeholder in the file)."""
    c = expand(content) if content is not None else None
    e = {"v": 1, "space": space, "kind": kind, "author": A(actor) if not actor.startswith("@") else actor, "keyId": key_id or "k1", "body": body,
         "contentHash": W.content_hash(c), "refs": list(refs), "seen": seen, "nonce": nonce(tag or f"{actor}-{kind}-{space}-{state['nonce']}"), "time": T0}
    state["nonce"] += 1
    e = W.sign_entry(e, KEYS[actor])
    return e, content

def append_frame(i, e, c, **kw):
    a = {"entry": e}
    if c is not None: a["content"] = c
    return {"id": i, "call": "append", "args": a}

def hello_send(i, who, **kw):
    a = {"who": who, "keyId": "k1", "proof": kw.get("proof", "<proof>"), "version": kw.get("version", 1)}
    if "keyId" in kw: a["keyId"] = kw["keyId"]
    return {"id": i, "call": "hello", "args": a}

def err(i, code, **kw): return {"id": i, "error": dict({"code": code}, **kw)}
OKNULL = lambda i: {"id": i, "ok": None}
HELLO_OK = lambda i: {"id": i, "ok": {"limits": LIMITS, "durability": "local", "time": T0, "knownHeads": []}}
def receipt(i, seq=None):
    return {"id": i, "ok": ("<receipt seq=%d>" % seq) if seq else "<receipt>"}

def vec(vid, title, steps, rules=(), expect=None, note=None, source=None):
    v = {"id": vid, "title": title}
    if expect: v["expect"] = expect
    if rules: v["rules"] = list(rules)
    if source: v["source"] = source
    if note: v["note"] = note
    v["steps"] = steps
    return v

al, bo, ca = A("alice"), A("bob"), A("carol")
# a message entry from the golden transcript (bob's first, seq 5), for the idempotence probes
g_msg = steps_of(load("message")["clients"]["ts-client"]["frames"])
bob_first = g_msg[0]["send"]
bob_first_nocontent = json.loads(json.dumps(bob_first)); bob_first_nocontent["id"] = 2; del bob_first_nocontent["args"]["content"]
bob_first["id"] = 2
bob_receipt = dict(g_msg[1]["expect"], id=2)
g_read = steps_of(load("read")["clients"]["ts-client"]["frames"])

# ───────────── probes.json: agreed error paths ─────────────
agreed = []
agreed.append(vec("P01", "hello with a proof over another challenge", [
    {"conn": al, "send": hello_send(1, al, proof="<proof challenge=another-challenge>")}, {"conn": al, "expect": err(1, "denied")}], ["G-08"], "denied", source="golden P01"))
agreed.append(vec("P02", "read before hello", [
    {"conn": "raw:1", "send": {"id": 1, "call": "read", "args": {"space": "s1:" + SERVER, "after": 0}}}, {"conn": "raw:1", "expect": err(1, "denied")}], ["G-07"], "denied", source="golden P02"))
agreed.append(vec("P03", "second hello on an authenticated connection", [
    {"conn": bo, "send": hello_send(1, bo)}, {"conn": bo, "expect": HELLO_OK(1)},
    {"conn": bo, "send": hello_send(2, bo)}, {"conn": bo, "expect": err(2, "invalid")}], ["G-08"], "invalid", source="golden P03"))
agreed.append(vec("P04", "hello version 2", [
    {"conn": al, "send": hello_send(1, al, version=2)}, {"conn": al, "expect": err(1, "invalid")}], ["G-08", "R-08"], "invalid", source="golden P04"))
agreed.append(vec("P06", "hello with an unknown keyId", [
    {"conn": al, "send": hello_send(1, al, keyId="nokey")}, {"conn": al, "expect": err(1, "denied")}], ["G-08", "W-03"], "denied", source="golden P06"))
agreed.append(vec("P07", "hello with a proof over another server name (dialed != server)", [
    {"conn": al, "send": hello_send(1, al, proof="<proof dialed=other.localhost>")}, {"conn": al, "expect": err(1, "denied")}], ["G-08", "W-02"], "denied", source="golden P07"))
agreed.append(vec("P08", "a frame that is not JSON", [
    {"conn": "raw:1", "send_raw": "not json {"}, {"conn": "raw:1", "expect": err(0, "invalid")}], ["G-07"], "{id:0, error:invalid}", source="golden P08"))
agreed.append(vec("P12", "unknown call", [
    {"conn": al, "send": {"id": 2, "call": "frobnicate", "args": {}}}, {"conn": al, "expect": err(2, "invalid")}], ["G-07"], "invalid", source="golden P12"))
agreed.append(vec("P13", "binary frame closes the connection", [
    {"conn": al, "send_binary": "00ff10"}, {"conn": al, "expect_close": True}], ["G-07"], "connection closes", source="golden P13"))
agreed.append(vec("P14", "re-append of a stored entry returns the same receipt (R-01)", [
    {"conn": bo, "send": bob_first}, {"conn": bo, "expect": bob_receipt}], ["R-01"], "same receipt", source="golden P14"))
agreed.append(vec("P15", "re-append of a stored entry without its content returns the same receipt (W-16d)", [
    {"conn": bo, "send": bob_first_nocontent}, {"conn": bo, "expect": bob_receipt}], ["R-01", "W-16"], "same receipt", source="golden P15"))
e, c = mk("carol", "message", {}, {"text": "carol is not a member"}, tag="P16")
agreed.append(vec("P16", "append by a non-member", [
    {"conn": ca, "send": append_frame(2, e, c)}, {"conn": ca, "expect": err(2, "denied")}], ["P-03"], "denied", source="golden P16"))
e, c = mk("alice", "message", {}, {"text": "into nowhere"}, space="nospace:" + SERVER, tag="P17")
agreed.append(vec("P17", "non-create first entry in an unknown local space", [
    {"conn": al, "send": append_frame(2, e, c)}, {"conn": al, "expect": err(2, "invalid")}], ["§4.1", "W-13"], "invalid", source="golden P17"))
agreed.append(vec("P19", "read of an unknown local space", [
    {"conn": al, "send": {"id": 2, "call": "read", "args": {"space": "nospace:" + SERVER, "after": 0}}}, {"conn": al, "expect": err(2, "not_found")}], ["§4.1", "W-13"], "not_found", source="golden P19"))
agreed.append(vec("P20", "read of a members-only space by a non-member", [
    {"conn": ca, "send": {"id": 2, "call": "read", "args": {"space": "s1:" + SERVER, "after": 0}}}, {"conn": ca, "expect": err(2, "not_found")}], ["§3", "P-02"], "not_found", source="golden P20"))
agreed.append(vec("P21", "read with after at the head, and beyond it", [
    {"conn": al, "send": {"id": 2, "call": "read", "args": {"space": "s1:" + SERVER, "after": 18}}}, {"conn": al, "expect": {"id": 2, "end": True}},
    {"conn": al, "send": {"id": 3, "call": "read", "args": {"space": "s1:" + SERVER, "after": 999999}}}, {"conn": al, "expect": {"id": 3, "end": True}}], ["G-09"], "no records, then end", source="golden P21"))
agreed.append(vec("P23", "cancel of an unknown target", [
    {"conn": al, "send": {"id": 2, "call": "cancel", "args": {"target": 999999}}}, {"conn": al, "expect": OKNULL(2)}], ["G-07", "W-06"], "ok null", source="golden P23"))
agreed.append(vec("P24", "streaming call reusing an open request id", [
    {"conn": al, "send": {"id": 2, "call": "subscribe", "args": {"space": "s1:" + SERVER}}},
    {"conn": al, "send": {"id": 2, "call": "subscribe", "args": {"space": "s1:" + SERVER}}}, {"conn": al, "expect": err(2, "invalid")}], ["G-07"], "invalid", source="golden P24"))
agreed.append(vec("P25", "subscribe by a non-reader", [
    {"conn": ca, "send": {"id": 2, "call": "subscribe", "args": {"space": "s1:" + SERVER}}}, {"conn": ca, "expect": err(2, "denied")}], ["G-09", "P-02"], "denied", source="golden P25"))
agreed.append(vec("P26", "signal into an unknown space", [
    {"conn": al, "send": {"id": 2, "call": "signal", "args": {"space": "nospace:" + SERVER, "body": {"a": 1}}}}, {"conn": al, "expect": err(2, "not_found")}], ["G-04", "W-24"], "not_found", source="golden P26"))
e, c = mk("alice", "message", {}, {"text": "flipped bit"}, tag="P28")
sig = e["sig"]; i = 40; e["sig"] = sig[:i] + ("B" if sig[i] != "B" else "C") + sig[i + 1:]
agreed.append(vec("P28", "append with a flipped bit in the middle of the signature", [
    {"conn": al, "send": append_frame(2, e, c)}, {"conn": al, "expect": err(2, "invalid")}], ["R-12", "§4.1"], "invalid", source="golden P28"))
e, c = mk("alice", "message", {}, {"text": "content but no hash"}, tag="P29")
e2 = {k: v for k, v in e.items() if k != "sig"}; e2["contentHash"] = ""; e2 = W.sign_entry(e2, KEYS["alice"])
agreed.append(vec("P29", "append with content but contentHash \"\"", [
    {"conn": al, "send": append_frame(2, e2, c)}, {"conn": al, "expect": err(2, "invalid")}], ["§4.1", "W-16"], "invalid", source="golden P29"))
e, c = mk("alice", "member.set", {"actor": ca, "role": "none"}, {"x": 1}, tag="P30")
agreed.append(vec("P30", "member.set with content", [
    {"conn": al, "send": append_frame(2, e, c)}, {"conn": al, "expect": err(2, "invalid")}], ["§4.1", "W-16"], "invalid", source="golden P30"))
e, c = mk("alice", "message", {}, {"text": "<repeat a 70000>"}, tag="P52")
agreed.append(vec("P52", "message whose content is over maxContentBytes (65536)", [
    {"conn": al, "send": append_frame(2, e, c)}, {"conn": al, "expect": err(2, "invalid")}], ["§2", "R-09"], "invalid", source="golden P52"))
agreed.append(vec("P53", "read with limit 2 from the start (no follow)", [
    {"conn": al, "send": {"id": 2, "call": "read", "args": {"space": "s1:" + SERVER, "after": 0, "limit": 2}}},
    dict(g_read[1], conn=al, expect=dict(g_read[1]["expect"], id=2)), dict(g_read[2], conn=al, expect=dict(g_read[2]["expect"], id=2)),
    {"conn": al, "expect": {"id": 2, "end": True}}], ["§3", "G-09"], "2 records then end", source="golden P53"))
# g_read[0] is the send; [1],[2] are the first two records
agreed.append(vec("P54", "read with limit 0 (outside 1-1000)", [
    {"conn": al, "send": {"id": 2, "call": "read", "args": {"space": "s1:" + SERVER, "after": 0, "limit": 0}}}, {"conn": al, "expect": err(2, "invalid")}], ["§3"], "invalid", source="golden P54"))
agreed.append(vec("P35", "blobPut complete, then repeat (idempotent)", [
    {"conn": al, "send": {"id": 2, "call": "blobPut", "args": {"ref": {"hash": hashlib.sha256(b"probe blob bytes").hexdigest(), "size": 16, "type": "text/plain"}, "offset": 0, "data": W.b64(b"probe blob bytes")}}},
    {"conn": al, "expect": {"id": 2, "ok": {"have": 16, "complete": True}}},
    {"conn": al, "send": {"id": 3, "call": "blobPut", "args": {"ref": {"hash": hashlib.sha256(b"probe blob bytes").hexdigest(), "size": 16, "type": "text/plain"}, "offset": 0, "data": W.b64(b"probe blob bytes")}}},
    {"conn": al, "expect": {"id": 3, "ok": {"have": 16, "complete": True}}}], ["G-01", "W-27"], "ok complete both times", source="golden P35"))

# pipelined appends (P31): two receipts in send order, consecutive seqs
e1, c1 = mk("bob", "message", {}, {"text": "pipelined one"}, tag="P31a"); e2_, c2 = mk("bob", "message", {}, {"text": "pipelined two"}, tag="P31b")
h = state["head"]
agreed.append(vec("P31", "two appends pipelined without waiting", [
    {"conn": bo, "send": append_frame(2, e1, c1)}, {"conn": bo, "send": append_frame(3, e2_, c2)},
    {"conn": bo, "expect": receipt(2, h + 1)}, {"conn": bo, "expect": receipt(3, h + 2)}], ["G-07", "W-05", "R-07"], "two receipts, consecutive seq, in send order", source="golden P31"))
state["head"] = h + 2

# sessions for the stream/watch probes
def session(tag, title):
    e, c = mk("alice", "session.open", {"steerMode": "auto"}, {"title": title}, tag=tag, seen=state["head"])
    state["head"] += 1
    return e, c, W.entry_id(e), state["head"]
sa, sa_c, sa_id, sa_seq = session("sessA", "probe session A")
sb, sb_c, sb_id, sb_seq = session("sessB", "probe session B")
cl_b, cl_b_c = mk("alice", "session.close", {"session": sb_id}, tag="closeB", seen=state["head"]); state["head"] += 1; cl_b_seq = state["head"]
sc, sc_c, sc_id, sc_seq = session("sessC", "probe session C")
cl_c, cl_c_c = mk("alice", "session.close", {"session": sc_id}, tag="closeC", seen=state["head"]); state["head"] += 1; cl_c_seq = state["head"]
agreed.append(vec("P40-setup", "setup: alice opens sessions A, B and C and closes B (state for P40-P48, P57)", [
    {"conn": al, "send": append_frame(2, sa, sa_c)}, {"conn": al, "expect": receipt(2, sa_seq)},
    {"conn": al, "send": append_frame(3, sb, sb_c)}, {"conn": al, "expect": receipt(3, sb_seq)},
    {"conn": al, "send": append_frame(4, cl_b, cl_b_c)}, {"conn": al, "expect": receipt(4, cl_b_seq)}], ["P-15"], source="maker"))
def item(sess, n, text, space="s1:" + SERVER): return {"space": space, "session": sess, "n": n, "time": T0, "type": "output", "text": text}
def stream(i, it): return {"id": i, "call": "stream", "args": {"item": it}}
agreed.append(vec("P40", "stream item for a closed session", [
    {"conn": al, "send": stream(2, item(sb_id, 1, "late"))}, {"conn": al, "expect": err(2, "denied")}], ["G-05"], "denied", source="golden P40"))
agreed.append(vec("P41", "stream n=1 by the session actor", [
    {"conn": al, "send": stream(2, item(sa_id, 1, "t1"))}, {"conn": al, "expect": OKNULL(2)}], ["G-05"], "ok", source="golden P41"))
agreed.append(vec("P42", "stream n=1 repeated (identical)", [
    {"conn": al, "send": stream(2, item(sa_id, 1, "t1"))}, {"conn": al, "expect": OKNULL(2)}], ["G-05", "W-31"], "ok", source="golden P42"))
agreed.append(vec("P43", "stream n=1 repeated with different text", [
    {"conn": al, "send": stream(2, item(sa_id, 1, "different"))}, {"conn": al, "expect": err(2, "invalid")}], ["G-05", "W-31"], "invalid", source="golden P43"))
agreed.append(vec("P44", "stream n=5 (gap)", [
    {"conn": al, "send": stream(2, item(sa_id, 5, "t5"))}, {"conn": al, "expect": err(2, "invalid")}], ["G-05", "W-31"], "invalid", source="golden P44"))
agreed.append(vec("P45", "stream by a member who is not the session's actor", [
    {"conn": bo, "send": stream(2, item(sa_id, 2, "t2"))}, {"conn": bo, "expect": err(2, "denied")}], ["G-05"], "denied", source="golden P45"))
agreed.append(vec("P46", "stream for an unknown session", [
    {"conn": al, "send": stream(2, item("0" * 64, 1, "t1"))}, {"conn": al, "expect": err(2, "not_found")}], ["G-05", "W-31"], "not_found", source="golden P46"))
agreed.append(vec("P47", "watch of an unknown session", [
    {"conn": bo, "send": {"id": 2, "call": "watch", "args": {"space": "s1:" + SERVER, "session": "0" * 64}}}, {"conn": bo, "expect": err(2, "not_found")}], ["G-05", "W-32"], "not_found", source="golden P47"))
agreed.append(vec("P48", "watch by a non-reader", [
    {"conn": ca, "send": {"id": 2, "call": "watch", "args": {"space": "s1:" + SERVER, "session": sa_id}}}, {"conn": ca, "expect": err(2, "denied")}], ["G-05"], "denied", source="golden P48"))
agreed.append(vec("P57", "watch stays open and silent when its session is closed", [
    {"conn": al, "send": append_frame(2, sc, sc_c)}, {"conn": al, "expect": receipt(2, sc_seq)},
    {"conn": bo, "send": {"id": 2, "call": "watch", "args": {"space": "s1:" + SERVER, "session": sc_id}}},
    {"conn": al, "send": append_frame(3, cl_c, cl_c_c)}, {"conn": al, "expect": receipt(3, cl_c_seq)},
    {"conn": bo, "expect_silence": True},
    {"conn": bo, "send": {"id": 3, "call": "cancel", "args": {"target": 2}}}, {"conn": bo, "expect": OKNULL(3)}], ["G-05", "W-32"], "stays open, no frame", source="golden P57"))
HEAD_AFTER_AGREED = state["head"]

# ───────────── probes-decided.json: fixed by DECISIONS-r21.md ─────────────
dec = []
dec.append(vec("D11a", "hello for an actor with no profile on this server", [
    {"conn": "raw:1", "expect": {"challenge": "<challenge-shape>", "server": SERVER, "versions": [1], "time": T0}},
    {"conn": "raw:1", "send": hello_send(1, "@nobody:" + SERVER)}, {"conn": "raw:1", "expect": err(1, "not_found")}], ["G-08", "D11"], "not_found", source="P05"))
dec.append(vec("D12a", "the challenge is 43 characters of canonical base64url (32 bytes)", [
    {"conn": "raw:1", "expect": {"challenge": "<challenge>", "server": SERVER, "versions": [1], "time": T0}}], ["G-08", "D12"], source="P49"))
dec.append(vec("D2a", "text that is valid JSON but not I-JSON (a fraction): id 0, invalid, the server closes", [
    {"conn": al, "send_raw": '{"id": 7, "call": "read", "args": {"space": "s1:%s", "after": 0.5}}' % SERVER}, {"conn": al, "expect": err(0, "invalid")}, {"conn": al, "expect_close": True}],
    ["G-07", "D2"], "id 0, invalid, close", source="P09"))
dec.append(vec("D2b", "a request frame with an extra member: invalid on its own id, connection stays open", [
    {"conn": al, "send_raw": '{"id": 5, "call": "cancel", "args": {"target": 1}, "extra": true}'}, {"conn": al, "expect": err(5, "invalid")},
    {"conn": al, "send": {"id": 6, "call": "cancel", "args": {"target": 1}}}, {"conn": al, "expect": OKNULL(6)}], ["G-07", "D2"], source="P10"))
dec.append(vec("D2c", "a request frame without args", [
    {"conn": al, "send_raw": '{"id": 5, "call": "cancel"}'}, {"conn": al, "expect": err(5, "invalid")}], ["G-07", "D2"], source="P11"))
dec.append(vec("D2d", "args that is not an object; an id that is not a positive integer", [
    {"conn": al, "send_raw": '{"id": 5, "call": "cancel", "args": [1]}'}, {"conn": al, "expect": err(5, "invalid")},
    {"conn": al, "send_raw": '{"id": 0, "call": "cancel", "args": {"target": 1}}'}, {"conn": al, "expect": err(0, "invalid")},
    {"conn": al, "send_raw": '{"id": "5", "call": "cancel", "args": {"target": 1}}'}, {"conn": al, "expect": err(0, "invalid")}], ["G-07", "D2"]))
dec.append(vec("D2e", "read with an unknown argument member", [
    {"conn": al, "send": {"id": 2, "call": "read", "args": {"space": "s1:" + SERVER, "after": 0, "bogus": 1}}}, {"conn": al, "expect": err(2, "invalid")}], ["G-07", "G-09", "D2"], source="P22"))
e, c = mk("alice", "space.create", {"policy": json.load(open(os.path.join(OUT, "02-space.create.json"), encoding="utf-8"))["steps"][0]["send"]["args"]["entry"]["body"]["policy"]},
          {"name": "elsewhere"}, space="q1:other.localhost", tag="P18", seen=0)
dec.append(vec("D1a", "space.create for a space homed on another server", [
    {"conn": al, "send": append_frame(2, e, c)}, {"conn": al, "expect": err(2, "moved", movedTo="other.localhost")}], ["§4.1", "D1", "W-13"], "moved", source="P18"))
dec.append(vec("D1b", "every call that names a space homed elsewhere is moved", [
    {"conn": al, "send": {"id": 2, "call": "read", "args": {"space": "q1:other.localhost", "after": 0}}}, {"conn": al, "expect": err(2, "moved", movedTo="other.localhost")},
    {"conn": al, "send": {"id": 3, "call": "subscribe", "args": {"space": "q1:other.localhost"}}}, {"conn": al, "expect": err(3, "moved", movedTo="other.localhost")},
    {"conn": al, "send": {"id": 4, "call": "signal", "args": {"space": "q1:other.localhost", "body": {}}}}, {"conn": al, "expect": err(4, "moved", movedTo="other.localhost")},
    {"conn": al, "send": {"id": 5, "call": "watch", "args": {"space": "q1:other.localhost", "session": "0" * 64}}}, {"conn": al, "expect": err(5, "moved", movedTo="other.localhost")},
    {"conn": al, "send": {"id": 6, "call": "blobGet", "args": {"ref": {"hash": "0" * 64, "size": 1, "type": "text/plain"}, "space": "q1:other.localhost"}}}, {"conn": al, "expect": err(6, "moved", movedTo="other.localhost")},
    {"conn": al, "send": stream(7, item("0" * 64, 1, "x", "q1:other.localhost"))}, {"conn": al, "expect": err(7, "moved", movedTo="other.localhost")}], ["§4.1", "D1", "D5"], source="P56"))
dec.append(vec("D1c", "follow of an unknown local space is answered at once, not held open", [
    {"conn": al, "send": {"id": 2, "call": "read", "args": {"space": "nospace:" + SERVER, "after": 0, "follow": True}}}, {"conn": al, "expect": err(2, "not_found")},
    {"conn": al, "send": {"id": 3, "call": "subscribe", "args": {"space": "nospace:" + SERVER}}}, {"conn": al, "expect": err(3, "not_found")},
    {"conn": al, "send": {"id": 4, "call": "watch", "args": {"space": "nospace:" + SERVER, "session": "0" * 64}}}, {"conn": al, "expect": err(4, "not_found")}], ["§3", "D5"], source="P19"))
dec.append(vec("D10a", "signal carrying a relayed `from` on a connection that is not a server actor", [
    {"conn": al, "send": {"id": 2, "call": "signal", "args": {"space": "s1:" + SERVER, "body": {"a": 1}, "from": bo}}}, {"conn": al, "expect": err(2, "invalid")}], ["G-04", "D10"], source="P27"))
dec.append(vec("D10b", "signals ride subscribe, never read with follow, and are not echoed to the sender's connection", [
    {"conn": al, "send": {"id": 2, "call": "subscribe", "args": {"space": "s1:" + SERVER}}},
    {"conn": bo, "send": {"id": 2, "call": "read", "args": {"space": "s1:" + SERVER, "after": 999999, "follow": True}}},
    {"conn": A("vera"), "send": {"id": 2, "call": "signal", "args": {"space": "s1:" + SERVER, "body": {"typing": True}}}},
    {"conn": A("vera"), "expect": OKNULL(2)},
    {"conn": al, "expect": {"id": 2, "item": {"type": "signal", "from": A("vera"), "body": {"typing": True}}}},
    {"conn": bo, "expect_silence": True},
    {"conn": al, "send": {"id": 3, "call": "cancel", "args": {"target": 2}}}, {"conn": al, "expect": OKNULL(3)},
    {"conn": bo, "send": {"id": 3, "call": "cancel", "args": {"target": 2}}}, {"conn": bo, "expect": OKNULL(3)}], ["G-09", "W-22", "G-04"]))
b_hash = hashlib.sha256(b"probe blob bytes").hexdigest()
ref16 = {"hash": b_hash, "size": 16, "type": "text/plain"}
dec.append(vec("D8a", "blobPut with a gap (offset 5 before any part)", [
    {"conn": bo, "send": {"id": 2, "call": "blobPut", "args": {"ref": ref16, "offset": 5, "data": W.b64(b" blob bytes")}}}, {"conn": bo, "expect": err(2, "invalid")}], ["G-01", "D8"], source="P32"))
bad = {"hash": hashlib.sha256(b"xyz").hexdigest(), "size": 3, "type": "text/plain"}
dec.append(vec("D8b", "blobPut whose bytes do not hash to ref.hash: invalid and the bytes are discarded", [
    {"conn": bo, "send": {"id": 2, "call": "blobPut", "args": {"ref": bad, "offset": 0, "data": W.b64(b"abc")}}}, {"conn": bo, "expect": err(2, "invalid")},
    {"conn": bo, "send": {"id": 3, "call": "blobPut", "args": {"ref": bad, "offset": 0, "data": W.b64(b"xyz")}}}, {"conn": bo, "expect": {"id": 3, "ok": {"have": 3, "complete": True}}}], ["G-01", "D8"], source="P33"))
dec.append(vec("D4a", "blobPut with padded base64", [
    {"conn": bo, "send": {"id": 2, "call": "blobPut", "args": {"ref": ref16, "offset": 0, "data": W.b64(b"probe blob bytes") + "==="}}}, {"conn": bo, "expect": err(2, "invalid")}], ["§2", "D4"], source="P34"))
e, c = mk("alice", "message", {}, {"text": "noncanonical signature"}, tag="P28b", seen=0)
s64 = e["sig"]; last = s64[-1]
alpha = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
nc = alpha[alpha.index(last) | 1] if alpha.index(last) & 3 == 0 else None
if nc is None: raise SystemExit("signature already ends in a char with non-zero low bits; change the nonce tag")
e["sig"] = s64[:-1] + nc
dec.append(vec("D4b", "append whose signature has non-zero padding bits in its last character", [
    {"conn": al, "send": append_frame(2, e, c)}, {"conn": al, "expect": err(2, "invalid")}], ["§2", "D4"], source="P28b"))
ref_p = {"hash": b_hash, "size": 16, "type": "text/plain"}
dec.append(vec("D3a", "the uploader reads its own unreferenced blob (24 h rule); a non-uploader gets not_found", [
    {"conn": bo, "send": {"id": 2, "call": "blobGet", "args": {"ref": ref_p, "space": "s1:" + SERVER}}}, {"conn": bo, "expect": err(2, "not_found")},
    {"conn": al, "send": {"id": 2, "call": "blobGet", "args": {"ref": ref_p, "space": "s1:" + SERVER}}}, {"conn": al, "expect": {"id": 2, "ok": {"data": W.b64(b"probe blob bytes"), "size": 16, "offset": 0}}}], ["G-01", "D3"], source="P36 P37"))
dec.append(vec("D3b", "blobGet ranges: offset past the end and length 0 are invalid, a huge length is clamped, offset = size is empty", [
    {"conn": al, "send": {"id": 2, "call": "blobGet", "args": {"ref": ref_p, "space": "s1:" + SERVER, "offset": 9999}}}, {"conn": al, "expect": err(2, "invalid")},
    {"conn": al, "send": {"id": 3, "call": "blobGet", "args": {"ref": ref_p, "space": "s1:" + SERVER, "offset": 0, "length": 99999999}}},
    {"conn": al, "expect": {"id": 3, "ok": {"data": W.b64(b"probe blob bytes"), "size": 16, "offset": 0}}},
    {"conn": al, "send": {"id": 4, "call": "blobGet", "args": {"ref": ref_p, "space": "s1:" + SERVER, "offset": 0, "length": 0}}}, {"conn": al, "expect": err(4, "invalid")},
    {"conn": al, "send": {"id": 5, "call": "blobGet", "args": {"ref": ref_p, "space": "s1:" + SERVER, "offset": 16}}}, {"conn": al, "expect": {"id": 5, "ok": {"data": "", "size": 16, "offset": 16}}}], ["G-01", "W-29"], source="P38 P39"))
dec.append(vec("D13a", "GET /.well-known/han", [
    {"http_get": "/.well-known/han", "expect_status": 200, "expect_type": "application/json",
     "expect_json": {"server": SERVER, "sequencer": "<url-of-the-binding>", "keys": [{"keyId": "h1", "publicKey": KEYS["server"].pub, "from": "1970-01-01T00:00:00Z"}]}}], ["§4.3", "D13", "D7"], source="W-12 W-50"))
dec.append(vec("D13b", "GET /.well-known/han-peers on a server with no peers", [
    {"http_get": "/.well-known/han-peers", "expect_status": 200, "expect_type": "application/json", "expect_json": {"peers": []}}], ["§4.3", "D13"], source="W-12 W-50"))
dec.append(vec("D6a", "G-06: host from a connection that is not a server actor is denied", [
    {"conn": al, "send": {"id": 2, "call": "host", "args": {"space": "s1:" + SERVER, "records": []}}}, {"conn": al, "expect": err(2, "denied")}], ["G-06"], source="P58"))
dec.append(vec("D9a", "hello ok carries every Limits member including maxClockSkewSeconds", [
    {"conn": "raw:1", "expect": {"challenge": "<challenge-shape>", "server": SERVER, "versions": [1], "time": T0}},
    {"conn": "raw:1", "send": hello_send(1, al)}, {"conn": "raw:1", "expect": HELLO_OK(1)}], ["G-08", "D9"], source="P50"))
dump(os.path.join(OUT, "config.json"), CONFIG)
dump(os.path.join(OUT, "probes.json"), {"format": "han-wire-vectors/1", "kind": "agreed", "note": "Error paths both reference servers answered alike. Run after the 21 operation files, in this order (each may add to the state the next relies on).", "vectors": agreed})
dump(os.path.join(OUT, "probes-decided.json"), {"format": "han-wire-vectors/1", "kind": "decided", "note": "Paths the two reference servers answered differently (or the text left open) before round 21. DECISIONS-r21.md and architecture.md G-07..G-11 now fix them. Run after probes.json.", "vectors": dec})
index["files"] += ["probes.json", "probes-decided.json"]
dump(os.path.join(OUT, "index.json"), index)
print("operation files:", n, "agreed probes:", len(agreed), "decided probes:", len(dec), "head after agreed:", HEAD_AFTER_AGREED)

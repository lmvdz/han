"""Replays the wire conformance vectors (conformance/wire-vectors/, architecture.md G-11, D15) against a HAN generic-binding server.

  python ref/run_wire_vectors.py --url ws://127.0.0.1:8787/han          a server already in test mode (fresh: empty data directory)
  python ref/run_wire_vectors.py --launch py                            start the Python reference server in test mode, run, stop it
  python ref/run_wire_vectors.py --launch ts                            the same for the clean-room TypeScript server
  python ref/run_wire_vectors.py --check                                offline: the vector files are well formed and self-consistent (run by run_all.py)
Options: --only SUBSTR...  run only vectors whose file or id contains one   -v  list every vector   --keys DIR  key files {keyId, public, private}
         (must be the config.json keys)   --json FILE  write the results   --timeout S  wait for one expected frame (default 5)
Test mode (G-11): ServerName a.localhost, a clock that starts at 2026-01-01T00:00:00Z and moves only when a vector says so (`test.clock`), the server key
and actors of conformance/wire-vectors/config.json provisioned, an empty data directory. Format of the vectors and how a server passes: conformance/wire-vectors/README.md.
Needs only the Python standard library and `cryptography` (for the hello proofs and --check)."""
import argparse, base64, hashlib, json, os, queue, re, socket, ssl, struct, subprocess, sys, tempfile, threading, time, urllib.request
from urllib.parse import urlparse
try:
    sys.stdout.reconfigure(encoding="utf-8")
except Exception:
    pass
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import policy as P, wire as W
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives import serialization

VEC = os.path.join(ROOT, "conformance", "wire-vectors")

# ───────────── strict JSON (I-JSON: no duplicate keys, no fractions or exponents) ─────────────
class NotIJson(ValueError): pass
def _pairs(pairs):
    d = {}
    for k, v in pairs:
        if k in d: raise NotIJson("duplicate key " + k)
        d[k] = v
    return d
def _bad(x): raise NotIJson("number form " + x)
def parse(text):
    return json.loads(text, object_pairs_hook=_pairs, parse_float=_bad, parse_constant=_bad)

# ───────────── configuration and keys ─────────────
class Cfg:
    def __init__(self, keys_dir=None):
        self.c = json.load(open(os.path.join(VEC, "config.json"), encoding="utf-8"))
        self.server = self.c["server"]
        self.keys = {}
        sk = self.c["serverKey"]
        self.server_key = {"keyId": sk["keyId"], "publicKey": sk["publicKey"], "from": sk["from"]}
        for a in self.c["actors"]:
            name = a["actor"].split(":")[0][1:]
            seed = hashlib.sha256(a["seed"].encode()).digest()
            if keys_dir:
                f = json.load(open(os.path.join(keys_dir, name + ".json"), encoding="utf-8"))
                seed = W.unb64(f["private"])
            k = Ed25519PrivateKey.from_private_bytes(seed)
            pub = W.b64(k.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw))
            if pub != a["publicKey"]: raise SystemExit(f"key for {name} does not match config.json (the vectors are signed with the config keys)")
            self.keys[a["actor"]] = (a["keyId"], k)
        self.blobs = {b["hash"]: b for k, b in self.c["blobs"].items() if isinstance(b, dict)}
        self.d = self.c["defaults"]
    def sign(self, who, msg):
        kid, k = self.keys.get(who) or self.keys[f"@alice:{self.server}"]
        return W.b64(k.sign(msg))

def blob_slice(h, offset, n):
    return bytes((i * 7 + 3) % 251 for i in range(offset, offset + n))

def summary(s): return f"<b64u len={len(s)} sha256={hashlib.sha256(s.encode()).hexdigest()[:16]}>"

# ───────────── a minimal WebSocket client (RFC 6455), standard library only ─────────────
class WS:
    def __init__(self, url, timeout=10):
        u = urlparse(url); tls = u.scheme == "wss"
        host, port, path = u.hostname, u.port or (443 if tls else 80), (u.path or "/") + (("?" + u.query) if u.query else "")
        raw = socket.create_connection((host, port), timeout)
        self.sock = ssl.create_default_context().wrap_socket(raw, server_hostname=host) if tls else raw
        key = base64.b64encode(os.urandom(16)).decode()
        self.sock.sendall((f"GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                           f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
        head = b""
        while b"\r\n\r\n" not in head:
            c = self.sock.recv(4096)
            if not c: raise ConnectionError("closed during the upgrade")
            head += c
        head, rest = head.split(b"\r\n\r\n", 1)
        status = head.split(b"\r\n", 1)[0].decode("latin-1")
        if " 101 " not in status + " ": raise ConnectionError("upgrade refused: " + status)
        accept = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
        if accept.lower() not in head.decode("latin-1").lower(): raise ConnectionError("bad Sec-WebSocket-Accept")
        self.buf, self.q, self.lock, self.pend = rest, queue.Queue(), threading.Lock(), []
        self.sock.settimeout(None)
        threading.Thread(target=self._read, daemon=True).start()
    def _exact(self, n):
        while len(self.buf) < n:
            c = self.sock.recv(1 << 16)
            if not c: raise EOFError
            self.buf += c
        out, self.buf = self.buf[:n], self.buf[n:]
        return out
    def _read(self):
        frag, fop = b"", None
        try:
            while True:
                b0, b1 = self._exact(2)
                fin, op, ln = b0 & 0x80, b0 & 0xF, b1 & 0x7F
                if ln == 126: ln = struct.unpack(">H", self._exact(2))[0]
                elif ln == 127: ln = struct.unpack(">Q", self._exact(8))[0]
                mask = self._exact(4) if b1 & 0x80 else None
                data = self._exact(ln)
                if mask: data = bytes(b ^ mask[i % 4] for i, b in enumerate(data))
                if op == 0x9: self._send(0xA, data)
                elif op == 0xA: pass
                elif op == 0x8:
                    self.q.put(("close", struct.unpack(">H", data[:2])[0] if len(data) >= 2 else None))
                    try: self._send(0x8, data[:2])
                    except OSError: pass
                    return
                else:
                    if op in (1, 2): frag, fop = data, op
                    else: frag += data
                    if fin: self.q.put(("text" if fop == 1 else "binary", frag)); frag = b""
        except (EOFError, OSError):
            self.q.put(("eof", None))
    def _send(self, op, payload):
        mask = os.urandom(4); n = len(payload)
        head = bytes([0x80 | op]) + (bytes([0x80 | n]) if n < 126 else bytes([0x80 | 126]) + struct.pack(">H", n) if n < 65536 else bytes([0x80 | 127]) + struct.pack(">Q", n))
        body = bytes(b ^ mask[i % 4] for i, b in enumerate(payload)) if n < 4096 else int.from_bytes(payload, "big").__xor__(int.from_bytes((mask * (n // 4 + 1))[:n], "big")).to_bytes(n, "big")
        with self.lock: self.sock.sendall(head + mask + body)
    def send_text(self, s): self._send(1, s.encode("utf-8"))
    def send_binary(self, b): self._send(2, b)
    def recv(self, timeout):
        if self.pend: return self.pend.pop(0)
        try: return self.q.get(timeout=timeout)
        except queue.Empty: return None
    def unget(self, item): self.pend.insert(0, item)
    def close(self):
        try: self.sock.close()
        except OSError: pass

# ───────────── matching ─────────────
CHALLENGE_STRICT = re.compile(r"[A-Za-z0-9_-]{43}", re.ASCII)
CHALLENGE_SHAPE = re.compile(r"[A-Za-z0-9_-]{22,128}", re.ASCII)
RECEIPT_FIELDS = {"space", "seq", "id", "prev", "home", "homeKey", "time", "durability", "profiles", "homeSig"}

class Ctx:
    def __init__(self, cfg, url): self.cfg, self.url, self.ids, self.vars = cfg, url, {}, {}      # vars: values a matcher captured (`<cursor>`)

def mask_actual(f):
    """The documented masking of a server frame before comparing: long blob `data` summarised, error `message` dropped."""
    if isinstance(f, dict):
        out = {}
        for k, v in f.items():
            if k == "data" and isinstance(v, str) and len(v) > 300: out[k] = summary(v)
            elif k == "error" and isinstance(v, dict): out[k] = {a: b for a, b in v.items() if a != "message"}
            else: out[k] = mask_actual(v)
        return out
    if isinstance(f, list): return [mask_actual(v) for v in f]
    return f

def match(exp, act, path, ctx, key=None):
    """None when `act` equals `exp` (the same parsed value, hence the same JCS bytes), or when exp is a documented matcher that accepts it; else a message."""
    if isinstance(exp, str) and exp.startswith("<") and exp.endswith(">") and not exp.startswith("<b64u"):
        if exp == "<challenge>":
            ok = isinstance(act, str) and CHALLENGE_STRICT.fullmatch(act)
            if ok:
                try: W.unb64(act)
                except ValueError: ok = False
            return None if ok else f"{path}: challenge must be 43 characters of canonical base64url (32 random bytes, G-08), got {act!r}"
        if exp == "<challenge-shape>":
            return None if isinstance(act, str) and CHALLENGE_SHAPE.fullmatch(act) else f"{path}: challenge must be 22-128 characters of [A-Za-z0-9_-], got {act!r}"
        if exp == "<url-of-the-binding>":
            return None if isinstance(act, str) and re.fullmatch(r"wss?://\S+/han", act) else f"{path}: sequencer must be a complete ws(s):// URL ending in /han (D7, D13), got {act!r}"
        if exp == "<posint>":
            return None if type(act) is int and act >= 1 else f"{path}: expected a positive integer, got {act!r:.40}"
        if exp == "<cursor>":
            ok = isinstance(act, str) and 1 <= len(act) <= 1024 and act.isascii() and act.isprintable()
            if ok: ctx.vars["cursor"] = act
            return None if ok else f"{path}: a state page's `next` must be an opaque non-empty printable ASCII string of at most 1024 characters, got {act!r:.80}"
        m = re.fullmatch(r"<receipt(?: seq=(\d+))?(?: time=(\S+))?>", exp)
        if m: return check_receipt(act, int(m.group(1)) if m.group(1) else None, path, ctx, key, m.group(2))
        m = re.fullmatch(r"<profile-bootstrap (@\S+) (human|agent|service)>", exp)
        if m: return check_profile_bootstrap(act, m.group(1), m.group(2), path, ctx)
    if isinstance(exp, dict):
        if not isinstance(act, dict): return f"{path}: expected an object, got {type(act).__name__}"
        if set(exp) != set(act): return f"{path}: members differ: expected {sorted(exp)}, got {sorted(act)}"
        for k in exp:
            r = match(exp[k], act[k], f"{path}.{k}", ctx, key)
            if r: return r
        return None
    if isinstance(exp, list):
        if not isinstance(act, list) or len(act) != len(exp): return f"{path}: expected a list of {len(exp)}, got {act!r:.80}"
        for i, (a, b) in enumerate(zip(exp, act)):
            r = match(a, b, f"{path}[{i}]", ctx, key)
            if r: return r
        return None
    if type(exp) is not type(act) or exp != act:
        return f"{path}: expected {exp!r:.100}, got {act!r:.100}"
    return None

def check_receipt(r, seq, path, ctx, key, time=None):
    if not isinstance(r, dict) or set(r) != RECEIPT_FIELDS: return f"{path}: not a Receipt: {r!r:.120}"
    cfg = ctx.cfg
    if seq is not None and r["seq"] != seq: return f"{path}: receipt seq {r['seq']}, expected {seq}"
    if time is not None and r["time"] != time: return f"{path}: receipt time {r['time']}, expected {time}"
    if r["home"] != cfg.server or r["homeKey"] != cfg.server_key["keyId"]: return f"{path}: home/homeKey are {r['home']}/{r['homeKey']}"
    if not W.verify_receipt(r, {cfg.server: {cfg.server_key["keyId"]: cfg.server_key}}): return f"{path}: homeSig does not verify against the server key"
    want = ctx.ids.get(key)
    if want and r["id"] != want: return f"{path}: receipt id {r['id']} is not the id of the entry sent ({want})"
    return None

def check_profile_bootstrap(rec, actor, kind, path, ctx):
    """The first record of `profile:<actor>` (architecture.md §4.3): a space.create authored by the server actor and signed by the server key, body
    {policy, profile: {actorKind, key}} (the PolicyDoc is any object: the nonce and the PolicyDoc are the server's own), content {name: <actor>}, and a
    receipt for seq 1 that pins {"@:S": 0}. Everything else is checked exactly."""
    cfg = ctx.cfg
    if not isinstance(rec, dict) or set(rec) != {"entry", "content", "redacted", "receipt"}: return f"{path}: not a record: {rec!r:.100}"
    e, c, r = rec["entry"], rec["content"], rec["receipt"]
    fields = {"v", "space", "kind", "author", "keyId", "body", "contentHash", "refs", "seen", "nonce", "time", "sig"}
    if not isinstance(e, dict) or set(e) != fields: return f"{path}.entry: not an Entry: {e!r:.100}"
    a = next((x for x in cfg.c["actors"] if x["actor"] == actor), None)
    if a is None: return f"vector error: {actor} is not in config.json"
    want = {"v": 1, "space": "profile:" + actor, "kind": "space.create", "author": "@:" + cfg.server, "keyId": cfg.server_key["keyId"], "refs": [], "seen": 0, "time": cfg.c["clock"]}
    for k, v in want.items():
        if e[k] != v or type(e[k]) is not type(v): return f"{path}.entry.{k}: expected {v!r}, got {e[k]!r:.80}"
    b = e["body"]
    if not isinstance(b, dict) or set(b) != {"policy", "profile"} or not isinstance(b["policy"], dict): return f"{path}.entry.body: expected {{policy, profile}}, got {b!r:.100}"
    if b["profile"] != {"actorKind": kind, "key": {"keyId": a["keyId"], "publicKey": a["publicKey"]}}: return f"{path}.entry.body.profile: got {b['profile']!r:.160}"
    if c != {"name": actor}: return f"{path}.content: expected {{name: {actor}}}, got {c!r:.80}"
    if e["contentHash"] != W.content_hash(c): return f"{path}.entry.contentHash does not match the content"
    eid = P.entry_id(e)
    if not W.verify_sig(cfg.server_key["publicKey"], e["sig"], b"han-sig-v1\n" + eid.encode()): return f"{path}.entry.sig does not verify under the server key"
    if rec["redacted"] is not False: return f"{path}.redacted must be false"
    if not isinstance(r, dict) or r.get("id") != eid or r.get("prev") != "" or r.get("space") != e["space"] or r.get("time") != cfg.c["clock"] or r.get("durability") != "local":
        return f"{path}.receipt: not the receipt of this entry at seq 1: {r!r:.160}"
    if r.get("profiles") != {"@:" + cfg.server: 0}: return f"{path}.receipt.profiles: expected {{@:{cfg.server}: 0}}, got {r.get('profiles')!r:.80}"
    return check_receipt(r, 1, path + ".receipt", ctx, None)

# ───────────── running a vector ─────────────
class Fail(Exception): pass

def walk(v, fn):
    if isinstance(v, dict): return {k: walk(x, fn) for k, x in v.items()}
    if isinstance(v, list): return [walk(x, fn) for x in v]
    return fn(v)

def expand_send(frame, conn, cfg, ctx=None):
    """Placeholders in a client frame: <proof ...>, <b64u len= sha256=> (blob bytes from the config generator), <repeat c n>, <cursor> (the last `next` the server sent)."""
    frame = json.loads(json.dumps(frame))
    a = frame.get("args") if isinstance(frame, dict) else None
    if isinstance(a, dict) and frame.get("call") == "hello" and isinstance(a.get("proof"), str) and a["proof"].startswith("<proof"):
        opts = dict(re.findall(r"(\w+)=(\S+?)(?=\s|>)", a["proof"]))
        ch = opts.get("challenge", conn.challenge); dialed = opts.get("dialed", cfg.server)
        a["proof"] = cfg.sign(a["who"], ("han-hello-v1\n" + ch + "\n" + dialed).encode())
    if isinstance(a, dict) and isinstance(a.get("data"), str) and a["data"].startswith("<b64u"):
        m = re.fullmatch(r"<b64u len=(\d+) sha256=([0-9a-f]+)>", a["data"])
        n = int(m.group(1)) * 3 // 4
        data = W.b64(blob_slice(a["ref"]["hash"], a["offset"], n))
        if summary(data) != a["data"]: raise Fail("vector error: blob generator does not reproduce " + a["data"])
        a["data"] = data
    def rep(v):
        if isinstance(v, str) and v.startswith("<repeat "):
            _, c, k = v.strip("<>").split(); return c * int(k)
        if v == "<cursor>":
            if not ctx or "cursor" not in ctx.vars: raise Fail("vector error: <cursor> is used before the server sent a `next`")
            return ctx.vars["cursor"]
        return v
    return walk(frame, rep)

def nonce_of(tag):
    """The nonce of a generated entry: the first 26 characters of the lowercase base32 of sha256("wire-vector-nonce-" + tag)."""
    return base64.b32encode(hashlib.sha256(("wire-vector-nonce-" + tag).encode()).digest()).decode().lower().rstrip("=")[:26]

def generated_entry(cfg, g, k, head):
    """Entry k of a `generate` step: signed here by the author's config key, so that the vector file need not hold a thousand of them."""
    sub = lambda v: walk(v, lambda x: x.replace("{k}", str(k)) if isinstance(x, str) else x)
    content = sub(g["content"]) if "content" in g else None
    e = {"v": 1, "space": g["space"], "kind": g["kind"], "author": g["author"], "keyId": cfg.keys[g["author"]][0], "body": sub(g["body"]),
         "contentHash": W.content_hash(content), "refs": [], "seen": head, "nonce": nonce_of(g["nonce"].replace("{k}", str(k))), "time": cfg.c["clock"]}
    e["sig"] = cfg.sign(g["author"], b"han-sig-v1\n" + P.entry_id(e).encode())
    return e, content

def run_vector(vec, cfg, url, timeout, verbose=False, clock=None):
    """Runs one vector; returns (failures, expectations, generated appends). `clock` is the run's view of the test clock {wall, steady}, shared by all vectors."""
    ctx = Ctx(cfg, url)
    if clock is None: clock = {"wall": cfg.c["clock"], "steady": 0, "maxFrameBytes": cfg.c["limits"]["maxFrameBytes"]}
    conns, fails, n_exp, n_gen = {}, [], 0, 0
    ctl = []
    steps = vec["steps"]
    first = {}
    for s in steps:
        if "conn" in s: first.setdefault(s["conn"], s)
    settle = cfg.d["settleMs"] / 1000
    base = urlparse(url)
    http_base = ("https" if base.scheme == "wss" else "http") + "://" + base.netloc
    def get(label):
        if label in conns: return conns[label]
        c = WS(url); c.challenge = None; c.label = label; conns[label] = c
        m = c.recv(timeout)
        if not m or m[0] != "text": raise Fail(f"{label}: no challenge frame after connecting ({m!r:.60})")
        f0 = first.get(label, {})
        if isinstance(f0.get("expect"), dict) and "challenge" in f0["expect"]:
            c.unget(m)
        else:
            try: ch = parse(m[1].decode("utf-8"))
            except Exception as ex: raise Fail(f"{label}: the first frame is not I-JSON ({ex})")
            if not isinstance(ch, dict) or set(ch) != {"challenge", "server", "versions", "time"}: raise Fail(f"{label}: first frame is not a Challenge: {ch!r:.100}")
            err = match("<challenge-shape>", ch["challenge"], "challenge", ctx)
            if err: raise Fail(f"{label}: {err}")
        try: c.challenge = parse(m[1].decode("utf-8"))["challenge"]
        except Exception: pass
        f0s = f0.get("send")
        manual = (isinstance(f0s, dict) and f0s.get("call") == "hello") or (isinstance(f0.get("expect"), dict) and "challenge" in f0["expect"]) or not label.startswith("@")
        if not manual:
            proof = cfg.sign(label, ("han-hello-v1\n" + c.challenge + "\n" + cfg.server).encode())
            c.send_text(json.dumps({"id": 1, "call": "hello", "args": {"who": label, "keyId": cfg.keys[label][0], "proof": proof, "version": 1}}))
            r = c.recv(timeout)
            if not r or r[0] != "text": raise Fail(f"{label}: no reply to the set-up hello ({r!r:.60})")
            rep = parse(r[1].decode("utf-8"))
            if not (isinstance(rep, dict) and rep.get("id") == 1 and "ok" in rep): raise Fail(f"{label}: set-up hello failed: {rep!r:.150}")
        return c
    try:
        for i, s in enumerate(steps):
            where = f"step {i}"
            if "clock" in s or "limits" in s:                                  # G-11 `test.clock` and `test.limits`, on the runner's own authenticated connection
                n_exp += 1
                if not ctl:
                    c0 = WS(url); c0.label = "ctl"; m = c0.recv(timeout)
                    if not m or m[0] != "text": raise Fail("ctl: no challenge frame after connecting")
                    ch = parse(m[1].decode("utf-8"))["challenge"]; who = "@alice:" + cfg.server
                    c0.send_text(json.dumps({"id": 1, "call": "hello", "args": {"who": who, "keyId": cfg.keys[who][0], "proof": cfg.sign(who, ("han-hello-v1\n" + ch + "\n" + cfg.server).encode()), "version": 1}}))
                    r = c0.recv(timeout)
                    if not r or r[0] != "text" or "ok" not in parse(r[1].decode("utf-8")): raise Fail(f"ctl: set-up hello failed: {r!r:.100}")
                    ctl.append(c0); conns["ctl"] = c0
                c0 = ctl[0]
                name = "test.clock" if "clock" in s else "test.limits"
                a_ = s["clock"] if "clock" in s else s["limits"]
                c0.send_text(json.dumps({"id": 2, "call": name, "args": a_}, separators=(",", ":")))
                m = c0.recv(timeout)
                if name == "test.clock": want = {"wall": a_.get("wall", clock["wall"]), "steady": clock["steady"] + a_.get("steadyAdvance", 0)}
                else: want = {"maxFrameBytes": a_.get("maxFrameBytes", clock["maxFrameBytes"])}
                if m is None or m[0] != "text": fails.append(f"{where}: no reply to {name} {json.dumps(a_)}"); continue
                try: f = mask_actual(parse(m[1].decode("utf-8")))
                except Exception as ex: fails.append(f"{where}: {name} reply is not I-JSON ({ex})"); continue
                err = match({"id": 2, "ok": want}, f, "$", ctx)
                if err: fails.append(f"{where}: {name} {json.dumps(a_)}: {err} (the server does not support the test control calls?)")
                else: clock.update(want)
                continue
            if "generate" in s:                                                # a long run of appends the runner signs itself; each reply is checked, none counts as an expectation
                g = s["generate"]; c = get(s["conn"])
                for k in range(g["count"]):
                    head = g["head"] + k
                    e, content = generated_entry(cfg, g, k, head)
                    rid = g["firstId"] + k
                    a = {"entry": e}
                    if content is not None: a["content"] = content
                    ctx.ids[(s["conn"], rid)] = P.entry_id(e)
                    c.send_text(json.dumps({"id": rid, "call": "append", "args": a}, ensure_ascii=False, separators=(",", ":")))
                    m = c.recv(timeout)
                    if m is None or m[0] != "text": fails.append(f"{where} ({s['conn']}): generated entry {k}: no reply"); break
                    try: f = mask_actual(parse(m[1].decode("utf-8")))
                    except Exception as ex: fails.append(f"{where}: generated entry {k}: reply is not I-JSON ({ex})"); break
                    err = match({"id": rid, "ok": "<receipt seq=%d>" % (head + 1)}, f, "$", ctx, (s["conn"], rid))
                    if err: fails.append(f"{where} ({s['conn']}): generated entry {k}: {err}"); break
                    n_gen += 1
                continue
            if "http_get" in s:
                n_exp += 1
                try:
                    with urllib.request.urlopen(http_base + s["http_get"], timeout=timeout) as r: status, ctype, body = r.status, r.headers.get("Content-Type", ""), r.read()
                except urllib.error.HTTPError as ex: status, ctype, body = ex.code, ex.headers.get("Content-Type", ""), ex.read()
                except OSError as ex: fails.append(f"{where}: GET {s['http_get']} failed: {ex}"); continue
                if status != s.get("expect_status", 200): fails.append(f"{where}: GET {s['http_get']} status {status}"); continue
                if "expect_type" in s and not ctype.split(";")[0].strip().lower() == s["expect_type"]: fails.append(f"{where}: content type {ctype!r}, expected {s['expect_type']}"); continue
                try: doc = parse(body.decode("utf-8"))
                except Exception as ex: fails.append(f"{where}: body is not I-JSON ({ex})"); continue
                err = match(s["expect_json"], doc, "$", ctx)
                if err: fails.append(f"{where}: GET {s['http_get']}: {err}")
                continue
            c = get(s["conn"])
            if "send" in s or "send_raw" in s or "send_binary" in s:
                if "send_binary" in s: c.send_binary(bytes.fromhex(s["send_binary"])); continue
                if "send_raw" in s: c.send_text(s["send_raw"])
                else:
                    f = expand_send(s["send"], c, cfg, ctx)
                    e = f.get("args", {}).get("entry") if isinstance(f.get("args"), dict) else None
                    if f.get("call") == "append" and isinstance(e, dict):
                        try: ctx.ids[(s["conn"], f["id"])] = P.entry_id(e)
                        except Exception: pass
                    c.send_text(json.dumps(f, ensure_ascii=False, separators=(",", ":")))
                    if f.get("call") in ("subscribe", "watch") or (f.get("call") == "read" and f["args"].get("follow")): time.sleep(settle)
            elif "expect" in s:
                n_exp += 1
                m = c.recv(timeout)
                if m is None: fails.append(f"{where} ({s['conn']}): no frame within {timeout}s, expected {json.dumps(s['expect'])[:160]}"); continue
                if m[0] != "text": fails.append(f"{where} ({s['conn']}): expected a frame, got {m[0]}"); continue
                text = m[1].decode("utf-8", "replace")
                try: f = parse(text)
                except Exception as ex: fails.append(f"{where} ({s['conn']}): server frame is not I-JSON ({ex}): {text[:120]}"); continue
                if len(text.encode("utf-8")) > clock["maxFrameBytes"]: fails.append(f"{where}: server frame over maxFrameBytes"); continue
                key = (s["conn"], f.get("id") if isinstance(f, dict) else None)
                err = match(s["expect"], mask_actual(f), "$", ctx, key)
                if err: fails.append(f"{where} ({s['conn']}): {err} | frame: {json.dumps(f, ensure_ascii=False, separators=(',', ':'))[:260]}")
            elif s.get("expect_close"):
                n_exp += 1
                end = time.time() + timeout; ok = False
                while time.time() < end:
                    m = c.recv(max(0.05, end - time.time()))
                    if m and m[0] in ("close", "eof"): ok = True; break
                if not ok: fails.append(f"{where} ({s['conn']}): the connection was not closed")
            elif s.get("expect_silence"):
                n_exp += 1
                m = c.recv(cfg.d["silenceMs"] / 1000)
                if m is not None: fails.append(f"{where} ({s['conn']}): expected silence, got {m[0]} {str(m[1])[:120]}")
            else: raise Fail(f"{where}: unknown step {sorted(s)}")
        time.sleep(0.15)
        for label, c in conns.items():
            while True:
                m = c.recv(0.05)
                if m is None or m[0] in ("eof", "close"): break
                fails.append(f"({label}): unexpected frame after the vector ended: {m[0]} {str(m[1])[:140]}")
    except Fail as ex:
        fails.append(str(ex))
    except (ConnectionError, OSError) as ex:
        fails.append(f"connection error: {ex}")
    finally:
        for c in conns.values(): c.close()
    return fails, sum(any(k in st for k in ("expect", "expect_close", "expect_silence", "http_get", "clock", "limits")) for st in steps), n_gen      # every expectation of the vector, whether or not the run got that far

def load_vectors(only):
    idx = json.load(open(os.path.join(VEC, "index.json"), encoding="utf-8"))
    out = []
    for fn in idx["files"]:
        d = json.load(open(os.path.join(VEC, fn), encoding="utf-8"))
        vs = d["vectors"] if "vectors" in d else [dict(d, id=d["name"])]
        for v in vs:
            tag = f"{fn}::{v['id']}"
            if only and not any(o in tag for o in only): continue
            out.append((fn, v))
    return out

# ───────────── starting a reference server in test mode ─────────────
def launch(kind, cfg):
    td = tempfile.mkdtemp(prefix="han-wire-")
    s = socket.socket(); s.bind(("127.0.0.1", 0)); port = s.getsockname()[1]; s.close()
    sk = cfg.c["serverKey"]; seed = hashlib.sha256(sk["seed"].encode()).digest()
    actors = [{"actor": a["actor"], "kind": a["kind"], "keyId": a["keyId"], "public": a["publicKey"], **({"operator": a["operator"]} if a.get("operator") else {})} for a in cfg.c["actors"]]
    json.dump(actors, open(os.path.join(td, "actors.json"), "w"))
    log = open(os.path.join(td, "server.log"), "wb")
    if kind == "py":
        kf = os.path.join(td, "server-key.json"); json.dump({"keyId": sk["keyId"], "public": sk["publicKey"], "private": W.b64(seed)}, open(kf, "w"))
        exe = os.path.join(ROOT, "witnesses", "impl-py", ".venv", "Scripts" if os.name == "nt" else "bin", "python.exe" if os.name == "nt" else "python")
        cmd = [exe if os.path.exists(exe) else sys.executable, os.path.join(ROOT, "conformance", "interop", "py_host.py"), "--server", cfg.server, "--port", str(port), "--db", os.path.join(td, "seq.db"),
               "--key", kf, "--actors", os.path.join(td, "actors.json"), "--freeze", cfg.c["clock"]]
        cwd = os.path.join(ROOT, "witnesses", "impl-py")
    else:
        d = os.path.join(td, "tsdata"); os.makedirs(d)
        json.dump({"keyId": sk["keyId"], "publicKey": sk["publicKey"], "d": W.b64(seed), "from": sk["from"]}, open(os.path.join(d, "serverkey.json"), "w"))
        cmd = ["node", os.path.join(ROOT, "conformance", "interop", "ts_host.mjs"), "--server", cfg.server, "--dir", d, "--port", str(port), "--actors", os.path.join(td, "actors.json"), "--freeze", cfg.c["clock"]]
        cwd = os.path.join(ROOT, "conformance", "interop")
    p = subprocess.Popen(cmd, stdout=log, stderr=subprocess.STDOUT, cwd=cwd, env=dict(os.environ, PYTHONIOENCODING="utf-8", PYTHONUNBUFFERED="1"))
    end = time.time() + 40
    while time.time() < end:
        txt = open(os.path.join(td, "server.log"), errors="replace").read()
        if "READY" in txt: break
        if p.poll() is not None: raise SystemExit(f"{kind} server exited {p.returncode}: {txt[-800:]}")
        time.sleep(0.1)
    else:
        p.kill(); raise SystemExit(f"{kind} server did not print READY")
    return p, f"ws://127.0.0.1:{port}/han"

# ───────────── offline self-check (--check) ─────────────
def check_files(cfg):
    """Every vector file parses; every client entry in the operation files is validly signed by its author; every receipt (append answers and
    records) has a valid homeSig under the config server key, matches the entry it belongs to, and the receipts of s1 form one chain."""
    problems, receipts, entries, n_vec, n_steps = [], {}, {}, 0, 0
    idx = json.load(open(os.path.join(VEC, "index.json"), encoding="utf-8"))
    pubs = {a["actor"]: a["publicKey"] for a in cfg.c["actors"]}
    skeys = {cfg.server: {cfg.server_key["keyId"]: cfg.server_key}}
    known = {"conn", "send", "send_raw", "send_binary", "expect", "expect_close", "expect_silence", "http_get", "expect_status", "expect_type", "expect_json", "clock", "limits", "generate"}
    sent = {}
    for fn in idx["files"]:
        d = json.load(open(os.path.join(VEC, fn), encoding="utf-8"))
        vs = d["vectors"] if "vectors" in d else [d]
        for v in vs:
            n_vec += 1
            for i, s in enumerate(v["steps"]):
                n_steps += 1
                if not set(s) <= known: problems.append(f"{fn}::{v.get('id', v.get('name'))} step {i}: unknown members {sorted(set(s) - known)}")
                if "conn" not in s and "http_get" not in s and "clock" not in s and "limits" not in s: problems.append(f"{fn} step {i}: no conn")
                if "generate" in s and not {"count", "head", "firstId", "space", "kind", "author", "body", "nonce"} <= set(s["generate"]): problems.append(f"{fn}::{v.get('id', v.get('name'))} step {i}: generate lacks members")
                if "limits" in s and not (isinstance(s["limits"], dict) and set(s["limits"]) <= {"maxFrameBytes"}): problems.append(f"{fn}::{v.get('id', v.get('name'))} step {i}: limits takes maxFrameBytes only")
                if "clock" in s and not (isinstance(s["clock"], dict) and set(s["clock"]) <= {"wall", "steadyAdvance"}): problems.append(f"{fn}::{v.get('id', v.get('name'))} step {i}: clock takes wall and steadyAdvance only")
                f = s.get("send") or s.get("expect")
                if "vectors" in d: continue
                if "send" in s and isinstance(f, dict) and f.get("call") == "append":
                    e = f["args"]["entry"]
                    if not W.verify_sig(pubs[e["author"]], e["sig"], b"han-sig-v1\n" + P.entry_id(e).encode()): problems.append(f"{fn} step {i}: entry signature does not verify")
                    sent[(fn, s["conn"], f["id"])] = P.entry_id(e)
                if "expect" in s and isinstance(f, dict):
                    ok = f.get("ok")
                    if isinstance(ok, dict) and RECEIPT_FIELDS == set(ok):
                        receipts[ok["seq"]] = ok
                        if sent.get((fn, s["conn"], f["id"])) not in (None, ok["id"]): problems.append(f"{fn} step {i}: receipt id is not the id of the entry sent")
                    it = f.get("item")
                    if isinstance(it, dict) and it.get("type") == "record":
                        rec = it["record"]; rc = rec["receipt"]
                        receipts[rc["seq"]] = rc
                        if P.entry_id(rec["entry"]) != rc["id"]: problems.append(f"{fn} step {i}: record entry id differs from its receipt id")
                        if not W.verify_sig(pubs.get(rec["entry"]["author"], ""), rec["entry"]["sig"], b"han-sig-v1\n" + P.entry_id(rec["entry"]).encode()) and rec["entry"]["author"] in pubs:
                            problems.append(f"{fn} step {i}: record entry signature does not verify")
                        if not rec["redacted"] and rec["entry"]["contentHash"] != W.content_hash(rec["content"]): problems.append(f"{fn} step {i}: contentHash does not match the record content")
    for seq, r in sorted(receipts.items()):
        if not W.verify_receipt(r, skeys): problems.append(f"receipt seq {seq}: homeSig does not verify")
        if r["seq"] > 1 and r["seq"] - 1 in receipts and r["prev"] != W.receipt_hash(receipts[r["seq"] - 1]): problems.append(f"receipt seq {seq}: prev is not the receiptHash of seq {seq - 1}")
    if sorted(receipts) != list(range(1, max(receipts) + 1)): problems.append("receipts of s1 are not contiguous: " + str(sorted(receipts)))
    return problems, n_vec, n_steps, len(receipts)

def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--url"); ap.add_argument("--launch", choices=["py", "ts"]); ap.add_argument("--check", action="store_true")
    ap.add_argument("--only", nargs="*"); ap.add_argument("-v", "--verbose", action="store_true"); ap.add_argument("--keys"); ap.add_argument("--json")
    ap.add_argument("--timeout", type=float, default=None)
    a = ap.parse_args()
    cfg = Cfg(a.keys)
    if a.check:
        problems, nv, ns, nr = check_files(cfg)
        for p in problems: print("PROBLEM", p)
        print(f"wire vectors well formed: {nv} vectors, {ns} steps, {nr} receipts of s1 form one valid chain" if not problems else f"{len(problems)} problems")
        return 1 if problems else 0
    proc = None
    url = a.url
    if a.launch:
        proc, url = launch(a.launch, cfg)
    if not url: ap.error("give --url, --launch or --check")
    timeout = a.timeout or cfg.d["expectTimeoutMs"] / 1000
    results, total_exp, ok_exp = [], 0, 0
    clock = {"wall": cfg.c["clock"], "steady": 0, "maxFrameBytes": cfg.c["limits"]["maxFrameBytes"]}
    try:
        for fn, v in load_vectors(a.only):
            try: fails, n, gen = run_vector(v, cfg, url, timeout, a.verbose, clock)
            except Exception as ex: fails, n, gen = [f"runner error: {type(ex).__name__}: {ex}"], 0, 0
            results.append({"file": fn, "id": v["id"], "pass": not fails, "expectations": n, "generated": gen, "failures": fails})
            total_exp += n
            if a.verbose or fails: print(("PASS " if not fails else "FAIL ") + f"{fn}::{v['id']}" + ("" if not fails else "\n      " + "\n      ".join(f[:300] for f in fails[:3])))
    finally:
        if proc:
            if os.name == "nt":      # the venv's python.exe is a launcher: end the whole tree, or the real server keeps running
                subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"], capture_output=True)
            else:
                proc.terminate()
            try: proc.wait(8)
            except subprocess.TimeoutExpired: proc.kill()
    ops = [r for r in results if r["file"][:2].isdigit()]
    pr = [r for r in results if r["file"] == "probes.json"]
    dc = [r for r in results if r["file"] == "probes-decided.json"]
    r22 = [r for r in results if r["file"] == "probes-r22.json"]
    r23 = [r for r in results if r["file"] == "probes-r23.json"]
    r24 = [r for r in results if r["file"] == "probes-r24.json"]
    r25 = [r for r in results if r["file"] == "probes-r25.json"]
    r27 = [r for r in results if r["file"] == "probes-r27.json"]
    def line(name, rs):
        gen = sum(r.get("generated", 0) for r in rs)
        return (f"{name}: {sum(r['pass'] for r in rs)}/{len(rs)} vectors pass ({sum(r['expectations'] for r in rs if r['pass'])}/{sum(r['expectations'] for r in rs)} expectations"
                + (f"; {gen} generated appends, not counted" if gen else "") + ")")
    print("---")
    for name, rs in (("operations (golden)", ops), ("agreed probes", pr), ("decided probes", dc), ("round-22 probes", r22), ("round-23 probes", r23), ("round-24 probes", r24), ("round-25 probes", r25), ("round-27 probes", r27)):
        if rs: print(line(name, rs))
    print(line("TOTAL", results))
    if a.json: json.dump({"url": url, "results": results}, open(a.json, "w", encoding="utf-8"), indent=1)
    return 0 if all(r["pass"] for r in results) else 1

if __name__ == "__main__":
    sys.exit(main())

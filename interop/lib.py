"""Shared by the harness: deterministic keys, the scenario document, free ports, process helpers."""
import base64, hashlib, json, os, socket, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
V1 = os.path.normpath(os.path.join(HERE, ".."))
IMPL = os.path.join(V1, "impl")
TS = os.path.join(V1, "cleanroom-ts")
PY = os.path.join(IMPL, ".venv", "Scripts", "python.exe")
NODE = "node"
SERVER = "a.localhost"
T0 = "2026-01-01T00:00:00Z"
sys.path.insert(0, IMPL)

def b64u(b: bytes) -> str: return base64.urlsafe_b64encode(b).decode().rstrip("=")

def seed(name: str) -> bytes: return hashlib.sha256(("han-interop-key-" + name).encode()).digest()

def make_keys(outdir: str, names=("alice", "bob", "carol", "vera", "x", "server")):
    """Key files in han.keys format {keyId, public, private}; deterministic so transcripts are reproducible."""
    from han import keys as K
    os.makedirs(outdir, exist_ok=True)
    out = {}
    for n in names:
        kp = K.KeyPair(seed(n), "h1" if n == "server" else "k1")
        K.save(kp, os.path.join(outdir, n + ".json"))
        out[n] = kp.to_json()
    return out

def actors_list(keys: dict):
    return [{"actor": f"@{n}:{SERVER}", "kind": "service" if n == "x" else "human", "keyId": keys[n]["keyId"], "public": keys[n]["public"], **({"operator": f"@alice:{SERVER}"} if n == "x" else {})}
            for n in ("alice", "bob", "carol", "vera", "x")]

def free_ports(n):
    socks = [socket.socket() for _ in range(n)]
    for s in socks: s.bind(("127.0.0.1", 0))
    ports = [s.getsockname()[1] for s in socks]
    for s in socks: s.close()
    return ports

def nonce(name: str) -> str:
    return base64.b32encode(hashlib.sha256(("nonce-" + name).encode()).digest()).decode().lower().rstrip("=")[:26]

def blob_bytes(name: str) -> bytes:
    n = {"b1": 2000, "b2": 1700000}[name]
    return bytes((i * 7 + 3) % 251 for i in range(n))

def policy(**over):
    p = {"visibility": "members", "join": "invite", "membersMayInvite": False, "allowAgents": "any",
         "approvals": [{"action": "file.write", "approvers": ["owner"]}], "separateApprover": True, "budget": None,
         "maxDelegationDepth": 2, "minDurability": "local", "launchWindow": 15, "extensionsMinRole": "member",
         "sessions": {"watch": "member", "read": "member", "steer": "owner", "modes": ["auto", "guarded"]}, "servers": {"deny": []}}
    p.update(over); return p

def scenario():
    from han.core import P
    S1 = "s1:" + SERVER
    a = lambda n: f"@{n}:{SERVER}"
    plan = {"action": "file.write", "target": "out/hello.txt",
            "args": {"content": "hello é \U0001F600\n", "z": 1, "a": [1, 2, {"y": 0, "b": None}], "\U0001F600": 2, "￿": 3}}
    digest = hashlib.sha256(P.jcs(plan)).hexdigest()
    E = {}
    def e(name, actor, kind, body, content=None, space=S1, refs=(), seen="head"):
        d = {"actor": actor, "kind": kind, "space": space, "body": body, "refs": list(refs), "seen": seen, "nonce": nonce(name)}
        if content is not None: d["content"] = content
        E[name] = d
    e("create", "alice", "space.create", {"policy": policy()}, {"name": "interop room"}, seen=0)
    for n in ("bob", "vera", "x"): e("m_" + n, "alice", "member.set", {"actor": a(n), "role": "member"})
    e("msg1", "bob", "message", {"to": [a("alice")]}, {"text": "hello alice"})
    e("msg_uni", "bob", "message", {}, {"text": "café ✓ \U0001F600 \"q\" \\ \n\t \u007f     <&>"})
    e("msg2", "bob", "message", {}, {"text": "live one (follow)"})
    e("msg3", "bob", "message", {}, {"text": "live two (subscribe)"})
    e("msg_b1", "alice", "message", {}, {"text": "with a blob", "blobs": ["$blob:b1"]})
    e("msg_b2", "alice", "message", {}, {"text": "with a big blob", "blobs": ["$blob:b2"]})
    e("propose", "bob", "effect.propose", {"executor": a("x"), "action": "file.write", "planDigest": digest, "repeatable": False}, {"plan": plan})
    e("approve", "alice", "effect.approve", {"proposal": "$id:propose", "planDigest": digest}, {"note": "ok"})
    e("sess_open", "alice", "session.open", {"steerMode": "auto"}, {"title": "terminal"})
    e("sess_close", "alice", "session.close", {"session": "$id:sess_open"})
    e("task", "alice", "task", {"to": [a("bob")]}, {"ask": "write the report"})
    e("task_update", "bob", "task.update", {"task": "$id:task", "state": "accepted"}, {"text": "on it"})
    e("sess_input", "alice", "session.input", {"session": "$id:sess_open"}, {"text": "steer: be brief"})
    e("redact", "alice", "redact", {"target": "$id:msg2"}, {"reason": "interop redaction"})
    items = [{"n": 1, "type": "output", "text": "line 1"}, {"n": 2, "type": "status", "state": "working"}, {"n": 3, "type": "output", "text": "line 3 é"}]
    return {"server": SERVER, "space": S1, "time": T0, "entries": E, "streamItems": items, "blobs": {"b1": {"size": 2000, "type": "application/octet-stream"}, "b2": {"size": 1700000, "type": "application/octet-stream"}},
            "signalBody": {"typing": True}}

def exec_scenario():
    from han.core import P
    SP = "e1:" + SERVER
    plan = {"action": "file.write", "target": "out/hello.txt", "args": {"content": "hello from interop\n"}}
    digest = hashlib.sha256(P.jcs(plan)).hexdigest()
    a = lambda n: f"@{n}:{SERVER}"
    E = {
        "create": {"actor": "alice", "kind": "space.create", "space": SP, "body": {"policy": policy(launchWindow=15)}, "content": {"name": "exec room"}, "refs": [], "seen": 0, "nonce": nonce("x-create")},
        "m_x": {"actor": "alice", "kind": "member.set", "space": SP, "body": {"actor": a("x"), "role": "member"}, "refs": [], "seen": "head", "nonce": nonce("x-mx")},
        "m_bob": {"actor": "alice", "kind": "member.set", "space": SP, "body": {"actor": a("bob"), "role": "member"}, "refs": [], "seen": "head", "nonce": nonce("x-mb")},
        "propose": {"actor": "bob", "kind": "effect.propose", "space": SP, "body": {"executor": a("x"), "action": "file.write", "planDigest": digest, "repeatable": False}, "content": {"plan": plan}, "refs": [], "seen": "head", "nonce": nonce("x-prop")},
        "approve": {"actor": "alice", "kind": "effect.approve", "space": SP, "body": {"proposal": "$id:propose", "planDigest": digest}, "content": {"note": "go"}, "refs": [], "seen": "head", "nonce": nonce("x-appr")},
    }
    return {"server": SERVER, "space": SP, "time": T0, "entries": E, "plan": plan}

class Proc:
    def __init__(self, name, cmd, logfile, cwd=None, env=None):
        self.name, self.cmd = name, cmd
        self.log = open(logfile, "wb")
        e = dict(os.environ, PYTHONIOENCODING="utf-8", PYTHONUNBUFFERED="1"); e.update(env or {})
        self.p = subprocess.Popen(cmd, stdout=self.log, stderr=subprocess.STDOUT, cwd=cwd, env=e)
        self.logfile = logfile
    def wait_for(self, text, timeout=30):
        t = time.time()
        while time.time() - t < timeout:
            if self.p.poll() is not None: raise RuntimeError(f"{self.name} exited {self.p.returncode}: " + open(self.logfile, errors="replace").read()[-2000:])
            if text in open(self.logfile, errors="replace").read(): return
            time.sleep(0.1)
        raise RuntimeError(f"{self.name} did not print {text!r}: " + open(self.logfile, errors="replace").read()[-2000:])
    def stop(self):
        if self.p.poll() is None:
            self.p.terminate()
            try: self.p.wait(8)
            except subprocess.TimeoutExpired: self.p.kill(); self.p.wait()
        self.log.close()

ALL = []
def spawn(*a, **k):
    p = Proc(*a, **k); ALL.append(p); return p
def cleanup():
    for p in ALL:
        try: p.stop()
        except Exception: pass
    ALL.clear()

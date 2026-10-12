"""Python client driver: runs the shared scenario through han.client.Client, op by op (mirror of ts_driver.mjs).
   python py_driver.py --scenario F --keys DIR --url ws://127.0.0.1:P[/han] [--url2 ...] --tap http://127.0.0.1:T --out results.json [--mode scenario|exec]
--url2: a second URL tried only if the first cannot connect (recorded as a workaround)."""
import argparse, asyncio, hashlib, json, os, sys, time, urllib.parse, urllib.request, traceback
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "impl"))
from han import keys as K
from han.client import Client
from han.core import P, W, WireError

ap = argparse.ArgumentParser()
for n in ("scenario", "keys", "url", "tap", "out"): ap.add_argument("--" + n, required=True)
ap.add_argument("--url2"); ap.add_argument("--mode", default="scenario"); ap.add_argument("--flag-dir")
A = ap.parse_args()
SC = json.load(open(A.scenario, encoding="utf-8"))
SERVER, SPACE = SC["server"], SC["space"]
sha = lambda b: hashlib.sha256(b).hexdigest()
actor_id = lambda n: f"@{n}:{SERVER}"

def mark(name):
    try: urllib.request.urlopen(f"{A.tap}/__mark?name={urllib.parse.quote(name)}", timeout=3).read()
    except Exception: pass

def blob_bytes(name):
    n = 2000 if name == "b1" else 1700000
    return bytes((i * 7 + 3) % 251 for i in range(n))

conns, ids, sent, heads, blobs = {}, {}, {}, {}, {}
URL = A.url
workaround = None

def sub(v):
    if isinstance(v, str):
        if v.startswith("$id:"): return ids[v[4:]]
        if v.startswith("$blob:"): return blobs[v[6:]]
        return v
    if isinstance(v, list): return [sub(x) for x in v]
    if isinstance(v, dict): return {k: sub(x) for k, x in v.items()}
    return v

def build(name):
    d = SC["entries"][name]; space = d.get("space", SPACE)
    content = sub(d["content"]) if "content" in d else None
    e = {"v": 1, "space": space, "kind": d["kind"], "author": actor_id(d["actor"]), "keyId": "k1", "body": sub(d["body"]),
         "contentHash": W.content_hash(content), "refs": [sub(r) for r in d["refs"]],
         "seen": heads.get(space, 0) if d["seen"] == "head" else d["seen"], "nonce": d["nonce"], "time": SC["time"]}
    return W.sign_entry(e, K.load(os.path.join(A.keys, d["actor"] + ".json"))), content

async def login(n):
    global URL, workaround
    if n not in conns:
        c = Client(URL, actor_id(n), os.path.join(A.keys, n + ".json"), ":memory:")
        try: await c.connect()
        except Exception as ex:
            if A.url2 and URL == A.url:
                workaround = f"{type(ex).__name__}: {ex}"
                await c.close(); URL = A.url2
                c = Client(URL, actor_id(n), os.path.join(A.keys, n + ".json"), ":memory:"); await c.connect()
            else: raise
        conns[n] = c
    return conns[n]

async def send(name):
    d = SC["entries"][name]; entry, content = build(name)
    c = await login(d["actor"])
    r = await asyncio.wait_for(c.append(entry, content, timeout=None), 20)
    assert r["id"] == P.entry_id(entry), "receipt id is not the entry id"
    ids[name], sent[name] = r["id"], (entry, content)
    heads[r["space"]] = max(heads.get(r["space"], 0), r["seq"])
    return r

def ok(c, m):
    if not c: raise AssertionError("check failed: " + m)

results, status = [], {}
def errstr(e): return f"{e.code}: {e.message}" if isinstance(e, WireError) else f"{type(e).__name__}: {e}"
async def op(name, requires, f):
    blocked = [r for r in requires if status.get(r) != "pass"]
    if blocked:
        status[name] = "skip"; results.append({"op": name, "status": "skip", "error": "blocked by " + ",".join(blocked)}); return
    mark("op:" + name)
    try:
        detail = await f(); status[name] = "pass"
        r = {"op": name, "status": "pass", "detail": detail}
        results.append(r)
    except Exception as e:
        status[name] = "fail"; results.append({"op": name, "status": "fail", "error": errstr(e) if not isinstance(e, asyncio.TimeoutError) else "timeout"})
        if os.environ.get("INTEROP_TRACE"): traceback.print_exc()

async def until(f, ms=6.0, what="condition"):
    t = time.time()
    while not f():
        if time.time() - t > ms: raise AssertionError("timed out waiting for " + what)
        await asyncio.sleep(0.02)

def jcs(v): return P.jcs(v)

async def collect(gen, got):
    async for it in gen: got.append(it)

async def cancel_task(t):
    t.cancel()
    try: await t
    except BaseException: pass

async def scenario():
    async def hello():
        c = await login("alice")
        h = c.hello_info
        ok(h and h.get("limits") and isinstance(h.get("time"), str) and isinstance(h.get("knownHeads"), list), "hello ok shape")
        return {"limits": sorted(h["limits"]), "durability": h.get("durability")}
    await op("hello", [], hello)
    if workaround and status.get("hello") == "pass": results[-1]["workaround"] = f"first URL {A.url} failed ({workaround}); used {URL}"
    if status.get("hello") == "pass": mark("setup"); await login("bob")     # other connections are opened outside any op, so op frames stay pure
    async def create():
        r = await send("create"); ok(r["seq"] == 1 and r["prev"] == "", "seq 1")
        return {"receiptKeys": sorted(r), "profiles": r.get("profiles")}
    await op("space.create", ["hello"], create)
    async def mset():
        for n in ("m_bob", "m_vera", "m_x"): await send(n)
    await op("member.set", ["space.create"], mset)
    async def message():
        r = await send("msg1"); await send("msg_uni"); ok(r["seq"] == 5, "seq 5"); return {"profiles": r.get("profiles")}
    await op("message", ["member.set"], message)
    async def read():
        c = await login("alice")
        recs = [i["record"] for i in await c.read_all(SPACE, 0) if i["type"] == "record"]
        ok(len(recs) == heads[SPACE], f"records {len(recs)} vs {heads[SPACE]}")
        for i, r in enumerate(recs): ok(r["receipt"]["seq"] == i + 1, "seq order")
        for n in ("create", "m_bob", "msg1", "msg_uni"):
            r = next((x for x in recs if x["receipt"]["id"] == ids[n]), None); ok(r, n + " present")
            ok(jcs(r["entry"]) == jcs(sent[n][0]), n + " entry round-trips"); ok(jcs(r.get("content")) == jcs(sent[n][1]), n + " content round-trips")
        return {"n": len(recs), "recordKeys": sorted(recs[0])}
    await op("read", ["message"], read)
    async def rstate():
        c = await login("alice")
        st = await c.state(SPACE)          # H10 (a): the state is its own call, one page {state, next?}
        ok("state" in st, "a state page"); ok(st["state"]["head"] == heads[SPACE], "state head")
        ok(st["state"]["roles"].get(actor_id("bob")) == "member", "bob is a member in roles")
        return {"stateKeys": sorted(st["state"])}
    await op("read.state", ["message"], rstate)
    async def follow():
        c, got = await login("alice"), []
        t = asyncio.create_task(collect(c.read(SPACE, heads[SPACE], follow=True), got))
        await asyncio.sleep(0.15); await send("msg2")
        try: await until(lambda: any(i["type"] == "record" and i["record"]["receipt"]["id"] == ids["msg2"] for i in got), 4, "live record")
        finally: await cancel_task(t)
    await op("follow", ["message"], follow)
    async def subscribe():
        c, got = await login("alice"), []
        t = asyncio.create_task(collect(c.subscribe(SPACE, heads[SPACE]), got))
        await asyncio.sleep(0.15); await send("msg3")
        try: await until(lambda: any(i["type"] == "record" and i["record"]["receipt"]["id"] == ids["msg3"] for i in got), 4, "live record")
        finally: await cancel_task(t)
    await op("subscribe", ["message"], subscribe)
    async def signal():
        c, b, got = await login("alice"), await login("bob"), []
        t = asyncio.create_task(collect(c.subscribe(SPACE), got))
        try:
            await asyncio.sleep(0.15); await b.signal(SPACE, SC["signalBody"])
            await until(lambda: any(i["type"] == "signal" for i in got), 4, "signal item")
            s = next(i for i in got if i["type"] == "signal")
            ok(s["from"] == actor_id("bob") and jcs(s["body"]) == jcs(SC["signalBody"]), "signal from/body")
        finally: await cancel_task(t)
    await op("signal", ["message"], signal)
    async def propose(): await send("propose")
    await op("propose", ["member.set"], propose)
    async def approve():
        await send("approve")
        c = await login("alice"); st = await c.read_state(SPACE)
        ph = st["effects"].get(ids["propose"], {}).get("phase")
        ok(ph == "ready", "effect phase ready after approval, got " + str(ph))
    await op("approve", ["propose"], approve)
    async def bput():
        c = await login("alice"); blobs["b1"] = await c.blob_put(blob_bytes("b1")); ok(blobs["b1"]["hash"] == sha(blob_bytes("b1")), "ref hash")
    await op("blobPut", ["member.set"], bput)
    async def bget():
        await send("msg_b1"); b = await login("bob")
        ok(await b.blob_get(blobs["b1"], SPACE) == blob_bytes("b1"), "bytes equal")
    await op("blobGet", ["blobPut"], bget)
    async def bput2():
        c = await login("alice"); blobs["b2"] = await c.blob_put(blob_bytes("b2"))
    await op("blobPut.multipart", ["member.set"], bput2)
    async def bget2():
        await send("msg_b2"); b = await login("bob")
        ok(await b.blob_get(blobs["b2"], SPACE) == blob_bytes("b2"), "bytes equal")
    await op("blobGet.multipart", ["blobPut.multipart"], bget2)
    async def sopen(): await send("sess_open")
    await op("session.open", ["member.set"], sopen)
    async def sinput(): await send("sess_input")
    await op("session.input", ["session.open"], sinput)
    if status.get("session.open") == "pass":
        mark("op:stream+watch")
        bob, alice, got = await login("bob"), await login("alice"), []
        wt = asyncio.create_task(collect(bob.watch(SPACE, ids["sess_open"]), got))
        await asyncio.sleep(0.15)
        item = lambda it: {"space": SPACE, "session": ids["sess_open"], "time": SC["time"], **it}
        try:
            for it in SC["streamItems"]: await alice.stream(item(it))
            await alice.stream(item(SC["streamItems"][2]))
            status["stream"] = "pass"; results.append({"op": "stream", "status": "pass"})
        except Exception as e: status["stream"] = "fail"; results.append({"op": "stream", "status": "fail", "error": errstr(e)})
        try:
            if wt.done() and wt.exception(): raise wt.exception()
            await until(lambda: len(got) >= 3, 3, "3 watch items")
            ok(len(got) == 3, f"exactly 3 items (no duplicate for the repeat), got {len(got)}")
            for i, g in enumerate(got): ok(g["n"] == i + 1 and g["type"] == SC["streamItems"][i]["type"], f"item {i + 1}")
            status["watch"] = "pass"; results.append({"op": "watch", "status": "pass"})
        except Exception as e: status["watch"] = "fail"; results.append({"op": "watch", "status": "fail", "error": errstr(e), "detail": {"got": len(got)}})
        await cancel_task(wt)
    else:
        for n in ("stream", "watch"): results.append({"op": n, "status": "skip", "error": "blocked by session.open"})
    async def sclose(): await send("sess_close")
    await op("session.close", ["session.open"], sclose)
    async def task():
        await send("task"); await send("task_update")
    await op("task", ["member.set"], task)
    async def redact():
        await send("redact"); c = await login("alice")
        recs = [i["record"] for i in await c.read_all(SPACE, 0) if i["type"] == "record"]
        r = next((x for x in recs if x["receipt"]["id"] == ids["msg2"]), None)
        ok(r and r.get("redacted") is True and r.get("content") is None, "msg2 is redacted with no content")
        return {"redactedRecordKeys": sorted(r)}
    await op("redact", ["subscribe"], redact)

async def exec_setup():
    result = {}
    mark("exec:setup")
    for n in ("create", "m_x", "m_bob"): await send(n)
    if A.flag_dir:      # "late" order: the space exists before the executor starts; the harness starts it, then writes `go`
        open(os.path.join(A.flag_dir, "spaceready"), "w").write("1"); t0 = time.time()
        while not os.path.exists(os.path.join(A.flag_dir, "go")) and time.time() - t0 < 40: await asyncio.sleep(0.1)
    for n in ("propose", "approve"): await send(n)
    result["proposal"] = ids["propose"]
    c = await login("alice"); t = time.time(); st = None
    mark("exec:wait")
    while time.time() - t < 60:
        st = await c.read_state(SC["space"])
        if st["effects"].get(ids["propose"], {}).get("phase") in ("succeeded", "failed"): break
        await asyncio.sleep(0.5)
    eff = st["effects"].get(ids["propose"], {})
    result["phase"], result["claims"] = eff.get("phase"), eff.get("claims")
    result["recordsAfter"] = [{"seq": i["record"]["receipt"]["seq"], "kind": i["record"]["entry"]["kind"], "author": i["record"]["entry"]["author"]}
                              for i in await c.read_all(SC["space"], 0) if i["type"] == "record"]
    return result

async def main():
    try:
        if A.mode == "exec": out = await exec_setup()
        else:
            await scenario(); out = {"ops": results}
            try: out["finalState"] = await (await login("alice")).read_state(SPACE)
            except Exception as e: out["finalStateError"] = str(e)
    except Exception as e:
        out = {"fatal": "".join(traceback.format_exception(e))[-3000:], "ops": results}
    json.dump(out, open(A.out, "w", encoding="utf-8"), indent=1)
    for c in conns.values():
        try: await c.close()
        except Exception: pass
asyncio.run(main())

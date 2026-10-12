"""Turns interop/runs/* into RESULTS.md (generated tables + frames appendix + rootcauses.md) and golden/*.json.
   ..\\impl\\.venv\\Scripts\\python.exe report.py"""
import hashlib, json, os, re, shutil
from lib import HERE

RUNS = os.path.join(HERE, "runs")
GOLD = os.path.join(HERE, "golden")
CELLS_LIVE = {"ts->py": "ts-py-live", "py->ts": "py-ts-live", "ts->ts": "ts-ts-live", "py->py": "py-py-live"}
CELLS_FROZEN = {"ts->py": "ts-py-frozen", "py->ts": "py-ts-frozen", "ts->ts": "ts-ts-frozen", "py->py": "py-py-frozen"}

def jload(p, d=None):
    try: return json.load(open(p, encoding="utf-8"))
    except Exception: return d

# ───────────── tap log ─────────────
def tap(cell):
    out = []
    p = os.path.join(RUNS, cell, "tap.jsonl")
    if os.path.exists(p):
        for l in open(p, encoding="utf-8"):
            try: out.append(json.loads(l))
            except Exception: pass
    return out

def mask(frame):
    f = json.loads(json.dumps(frame))
    if isinstance(f, dict):
        if "challenge" in f and "server" in f and isinstance(f["challenge"], str): f["challenge"] = "<challenge>"
        a = f.get("args")
        if f.get("call") == "hello" and isinstance(a, dict) and "proof" in a: a["proof"] = "<proof>"
        def walk(x):
            if isinstance(x, dict):
                for k, v in x.items():
                    if k == "data" and isinstance(v, str) and len(v) > 300: x[k] = f"<b64u len={len(v)} sha256={hashlib.sha256(v.encode()).hexdigest()[:16]}>"
                    else: walk(v)
            elif isinstance(x, list):
                for v in x: walk(v)
        walk(f)
    return f

def segments(events):
    """{mark name: [event...]} keyed by the last mark seen (events before any mark: '')."""
    segs, cur = {}, ""
    actor_of = {}
    for ev in events:
        if ev["ev"] == "mark": cur = ev["name"]; segs.setdefault(cur, []); continue
        if ev["ev"] == "frame" and ev["dir"] == "c2s":
            try:
                f = json.loads(ev["data"])
                if f.get("call") == "hello": actor_of[ev["conn"]] = f["args"]["who"]
            except Exception: pass
        segs.setdefault(cur, []).append(ev)
    return segs, actor_of

def frames_of(evs, actor_of, limit=700, renumber=False):
    out = []
    idmap = {}
    def ren(conn, fr):
        # request ids are chosen by the client; golden vectors renumber them per connection in order of first appearance (1, 2, ...)
        if isinstance(fr, dict) and isinstance(fr.get("id"), int) and fr["id"] != 0 and "challenge" not in fr:
            m = idmap.setdefault(conn, {})
            if fr["id"] not in m:
                if fr.get("call") is None and "call" not in fr: return fr        # a reply to an id we never saw a request for
                m[fr["id"]] = len(m) + 1
            fr = dict(fr, id=m[fr["id"]])
        if isinstance(fr, dict) and fr.get("call") == "cancel" and isinstance(fr.get("args"), dict) and fr["args"].get("target") in idmap.get(conn, {}):
            fr = dict(fr, args={"target": idmap[conn][fr["args"]["target"]]})
        return fr
    for ev in evs:
        if ev["ev"] == "frame":
            try: fr = mask(json.loads(ev["data"]))
            except Exception: fr = {"unparseable": ev["data"][:200]}
            if renumber: fr = ren(ev["conn"], fr)
            out.append({"conn": actor_of.get(ev["conn"], f"conn{ev['conn']}"), "dir": ev["dir"], "frame": fr})
        elif ev["ev"] in ("upgrade-fail", "close", "binary", "open", "upstream-error"):
            e = {k: v for k, v in ev.items() if k not in ("n", "ms")}
            out.append({"event": e})
    return out

def per_conn_dir(frames):
    d = {}
    for f in frames:
        if "frame" in f: d.setdefault((f["conn"], f["dir"]), []).append(f["frame"])
    return d

# ───────────── matrix ─────────────
def sym(r):
    if r["status"] == "pass": return "PASS*" if r.get("workaround") else "PASS"
    return {"fail": "FAIL", "skip": "SKIP"}[r["status"]]

def diff_json(a, b, path="", out=None):
    out = [] if out is None else out
    if isinstance(a, dict) and isinstance(b, dict):
        for k in sorted(set(a) | set(b)):
            if k not in a: out.append((path + "/" + k, "<absent>", json.dumps(b[k])[:60]))
            elif k not in b: out.append((path + "/" + k, json.dumps(a[k])[:60], "<absent>"))
            else: diff_json(a[k], b[k], path + "/" + k, out)
    elif isinstance(a, list) and isinstance(b, list) and len(a) == len(b):
        for i, (x, y) in enumerate(zip(a, b)): diff_json(x, y, f"{path}[{i}]", out)
    elif a != b: out.append((path, json.dumps(a)[:60], json.dumps(b)[:60]))
    return out

def build():
    live = {k: jload(os.path.join(RUNS, c, "results.json"), {}) for k, c in CELLS_LIVE.items()}
    verify = {k: jload(os.path.join(RUNS, c, "verify.json"), {}) for k, c in CELLS_LIVE.items()}
    execs = {}
    for order in ("late", "early"):
        for e, s in (("py", "ts"), ("ts", "py"), ("py", "py"), ("ts", "ts")):
            execs[(e, s, order)] = jload(os.path.join(RUNS, f"exec-{e}-{s}-{order}", "exec-result.json"), {})
    return live, verify, execs

def ops_of(live):
    seen = []
    for k in live:
        for r in live[k].get("ops", []):
            if r["op"] not in seen: seen.append(r["op"])
    return seen

def probes_of(cell): return {p["id"]: p for p in jload(os.path.join(RUNS, cell, "probes.json"), [])}

# ───────────── golden ─────────────
OPDESC = {
    "hello": "challenge from the server, hello with proof, hello ok (limits, durability, time, knownHeads)",
    "space.create": "append space.create (version 1 of a PolicyDoc) -> receipt seq 1",
    "member.set": "append three member.set entries -> receipts",
    "message": "append message with `to`, and a message with escapes/astral/format chars (JCS and I-JSON stress)",
    "read": "read after 0 -> record items, then end",
    "read.state": "read with state:true -> state item, record items, end",
    "follow": "read with follow:true from the head; a live record arrives; cancel",
    "subscribe": "subscribe after head; a live record arrives; cancel",
    "signal": "subscribe live-only; another actor signals; signal item with `from`; cancel",
    "propose": "append effect.propose (plan with JCS ordering stress) -> receipt",
    "approve": "append effect.approve -> receipt; state shows phase ready",
    "blobPut": "blobPut a 2000-byte blob in one part",
    "blobGet": "reference the blob in a message; another member blobGets it",
    "blobPut.multipart": "blobPut 1,700,000 bytes in three parts",
    "blobGet.multipart": "reference it in a message; blobGet in three ranges",
    "session.open": "append session.open -> receipt",
    "session.input": "append session.input (a steer from the session's own actor)",
    "stream+watch": "bob watches the session; alice streams three items and repeats the last; watch delivers three",
    "session.close": "append session.close -> receipt",
    "task": "append task, task.update",
    "redact": "append redact; the redacted record is served with redacted:true and no content",
}

def golden(live):
    if os.path.isdir(GOLD): shutil.rmtree(GOLD)
    os.makedirs(GOLD)
    data = {}
    for k, c in CELLS_FROZEN.items():
        segs, actor_of = segments(tap(c))
        data[k] = {"segs": segs, "actor": actor_of, "res": jload(os.path.join(RUNS, c, "results.json"), {})}
    index = []
    for name in OPDESC:
        mark = "op:" + name.replace("stream+watch", "stream+watch")
        res_name = {"stream+watch": "watch"}.get(name, name)
        st = {k: next((r for r in data[k]["res"].get("ops", []) if r["op"] == res_name), {}).get("status") for k in data}
        stw = {k: next((r for r in data[k]["res"].get("ops", []) if r["op"] == "stream"), {}).get("status") for k in data} if name == "stream+watch" else {}
        fr = {k: frames_of(data[k]["segs"].get(mark, []), data[k]["actor"], renumber=True) for k in data}
        def ok_both(client):
            a, b = f"{client}->py", f"{client}->ts"
            good = lambda k: st[k] == "pass" and (name != "stream+watch" or stw[k] == "pass")
            return good(a) and good(b)
        entry = {"op": name, "description": OPDESC[name], "status": {k: st[k] for k in st}, "clients": {}}
        have = False
        for client in ("ts", "py"):
            if not ok_both(client): continue
            a, b = fr[f"{client}->py"], fr[f"{client}->ts"]
            same = per_conn_dir(a) == per_conn_dir(b)
            ce = {"pythonServerAndTsServerAgree": same}
            if same: ce["frames"] = a
            else:
                ce["pythonServerFrames"], ce["tsServerFrames"] = a, b
                da, db = per_conn_dir(a), per_conn_dir(b)
                ce["differences"] = []
                for key in sorted(set(da) | set(db)):
                    la, lb = da.get(key, []), db.get(key, [])
                    for i in range(max(len(la), len(lb))):
                        x, y = (la[i] if i < len(la) else None), (lb[i] if i < len(lb) else None)
                        if x != y: ce["differences"].append({"conn": key[0], "dir": key[1], "index": i, "pythonServer": x, "tsServer": y}); break
            entry["clients"][f"{client}-client"] = ce; have = True
        if not have:
            entry["note"] = "does not pass for the same client on both servers; no golden vector"
        else:
            ag = [c for c, v in entry["clients"].items() if v["pythonServerAndTsServerAgree"]]
            entry["vectorFrom"] = ag[0] if ag else None
            if len(ag) == 2: entry["clientsSendTheSameFrames"] = per_conn_dir(entry["clients"]["ts-client"]["frames"]) == per_conn_dir(entry["clients"]["py-client"]["frames"])
            entry["vectorRule"] = ("frames below are what the named client sent and BOTH servers answered, byte for byte after JSON parsing "
                                   "(hello challenge/proof masked, blob data >300 chars summarised); keys, nonces, entry times and both server clocks fixed")
        json.dump(entry, open(os.path.join(GOLD, name.replace("+", "_") + ".json"), "w", encoding="utf-8"), indent=1, ensure_ascii=False)
        index.append({"op": name, "vector": entry.get("vectorFrom"), "agree": {c: v["pythonServerAndTsServerAgree"] for c, v in entry["clients"].items()}})
    json.dump({"note": "see each file; `agree` false means the two servers answered differently for that client", "ops": index}, open(os.path.join(GOLD, "index.json"), "w"), indent=1)
    return index

def strip_msgs(fr):
    f = json.loads(json.dumps(fr))
    if isinstance(f, dict) and isinstance(f.get("ok"), dict) and "limits" in f["ok"]: f["ok"]["limits"] = "<limits: see hello.json>"      # compared there
    if isinstance(f, dict) and isinstance(f.get("error"), dict): f["error"].pop("message", None)
    return f

def golden_probes():
    """Error-path vectors: the probes both servers answered the same way (error `message` text is free, so it is dropped)."""
    cells = {"python": "ts-py-frozen", "ts": "ts-ts-frozen"}
    segs = {k: segments(tap(c)) for k, c in cells.items()}
    res = {k: {p["id"]: p for p in jload(os.path.join(RUNS, c, "probes.json"), [])} for k, c in cells.items()}
    vectors, skipped = [], []
    for pid in sorted(res["python"], key=lambda s: (len(s), s)):
        pa, pb = res["python"][pid], res["ts"].get(pid, {})
        fa = [f for f in frames_of(segs["python"][0].get("probe:" + pid, []), segs["python"][1], renumber=True) if "frame" in f]
        fb = [f for f in frames_of(segs["ts"][0].get("probe:" + pid, []), segs["ts"][1], renumber=True) if "frame" in f]
        sa = [(f["conn"], f["dir"], strip_msgs(f["frame"])) for f in fa if f["dir"] == "s2c" and "versions" not in f["frame"]]
        sb = [(f["conn"], f["dir"], strip_msgs(f["frame"])) for f in fb if f["dir"] == "s2c" and "versions" not in f["frame"]]
        ca = [f["frame"] for f in fa if f["dir"] == "c2s"]; cb = [f["frame"] for f in fb if f["dir"] == "c2s"]
        agree = sa == sb and ca == cb
        if agree and pa.get("pass") and pb.get("pass"):
            vectors.append({"id": pid, "title": pa["title"], "expect": pa["expect"],
                            "frames": [{"conn": f["conn"], "dir": f["dir"], "frame": strip_msgs(f["frame"])} for f in fa if "versions" not in f["frame"]]})
        else:
            skipped.append({"id": pid, "title": pa.get("title"), "reason": (("servers differ" if pa.get("summary") != pb.get("summary") else "same answer; frames differ only in values that depend on earlier appends (seq, prev, receipt)") if not agree else "does not match the spec reading on at least one server"),
                            "python": pa.get("summary"), "ts": pb.get("summary")})
    json.dump({"note": "Error-path and edge vectors. Run in order against a server that holds the golden scenario (see the other files, frozen clocks); "
                       "request ids are renumbered per connection; error `message` text is not normative and is dropped. Probes where the servers differ are in `skipped`.",
               "vectors": vectors, "skipped": skipped}, open(os.path.join(GOLD, "probes.json"), "w", encoding="utf-8"), indent=1, ensure_ascii=False)
    return len(vectors), len(skipped)

# ───────────── RESULTS.md ─────────────
NL = chr(10)

def main():
    live, verify, execs = build()
    ops = ops_of(live)
    L = []
    w = L.append
    w("# HAN v1 cross-implementation interoperability: results\n")
    w("Generated by `interop/report.py` from `interop/runs/` (raw tap logs, driver results, server logs). Run: `interop/run.py`. "
      "Python reference = `impl/` (`python -m han.seq`, `han.client`, `han.exec`, `han.verify`); TypeScript clean room = `cleanroom-ts/` (`dist/seq.js` via `ts_host.mjs`, `client.js`, `exec.js`, `verify.js`).\n")
    w("## 1. Pass/fail matrix\n")
    w("Required directions: **ts->py** (TS client on the Python server) and **py->ts** (Python client on the TS server). ts->ts and py->py are same-implementation baselines, "
      "run to tell a driver bug from an interop bug. `PASS*` = passes only after a workaround (see section 3).\n")
    cols = ["ts->py", "py->ts", "ts->ts", "py->py"]
    w("| operation | " + " | ".join(cols) + " |"); w("|---|" + "---|" * len(cols))
    tot = {c: [0, 0, 0] for c in cols}   # pass (incl *), strict pass, total
    for op in ops:
        row = []
        for c in cols:
            r = next((x for x in live[c].get("ops", []) if x["op"] == op), None)
            s = sym(r) if r else "-"
            row.append(s)
            if r: tot[c][2] += 1; tot[c][0] += s in ("PASS", "PASS*"); tot[c][1] += s == "PASS"
        w(f"| {op} | " + " | ".join(row) + " |")
    # verifier + executor
    vrow = []
    for c in cols:
        v = verify[c]; s = "PASS" if v.get("pass") else "FAIL"; vrow.append(s); tot[c][2] += 1; tot[c][0] += s == "PASS"; tot[c][1] += s == "PASS"
    w("| verifier follows s1 and verifies its receipts (R-14) | " + " | ".join(vrow) + " |")
    prow = []
    for c in cols:
        v = verify[c].get("profile", {}); s = "PASS" if v.get("pass") else "FAIL"; prow.append(s); tot[c][2] += 1; tot[c][0] += s == "PASS"; tot[c][1] += s == "PASS"
    w("| verifier follows `profile:@alice` (entries authored by the server actor @:S) | " + " | ".join(prow) + " |")
    exmap = {"ts->py": ("ts", "py"), "py->ts": ("py", "ts"), "ts->ts": ("ts", "ts"), "py->py": ("py", "py")}
    for order in ("late", "early"):
        row = []
        for c in cols:
            e, s = exmap[c]; r = execs.get((e, s, order), {}); sy = "PASS" if r.get("pass") else "FAIL"
            row.append(sy)
            if order == "late": tot[c][2] += 1; tot[c][0] += sy == "PASS"; tot[c][1] += sy == "PASS"
        w(f"| executor claims, launches, reports ({order}: executor starts {'after the space exists' if order == 'late' else 'before the space exists'}) | " + " | ".join(row) + " |")
    w("")
    w("Column = client implementation -> server implementation. The verifier row uses the client's own implementation as verifier (ts->py: TS verifier, Python server). "
      "The executor rows use the column's client implementation as the executor against the column's server; the proposal and approval are made by the server's native client, so the executor is the only cross-implementation piece. "
      "Totals count the late-order executor row only.\n")
    w("| direction | passed (incl. workaround) | passed without workaround | total |"); w("|---|---|---|---|")
    for c in cols: w(f"| {c} | {tot[c][0]} | {tot[c][1]} | {tot[c][2]} |")
    w("")
    w("Supplementary cells with the TS sequencer's own server key id `srv1` (the TS verifier hard-codes it, root cause 8):" + chr(10))
    w("| cell | verifier on s1 | verifier on profile:@alice |"); w("|---|---|---|")
    for cell in ("ts-py-live-srv1", "ts-ts-live-srv1", "py-ts-live-srv1"):
        v = jload(os.path.join(RUNS, cell, "verify.json"), {})
        if v: w(f"| {cell} (verifier = first letter's implementation) | {'PASS' if v.get('pass') else 'FAIL'} | {'PASS' if v.get('profile', {}).get('pass') else 'FAIL'} |")
    w("")
    # final state
    w("### 1.1 StateView after the whole scenario (frozen clocks)")
    w("")
    fs = {k: jload(os.path.join(RUNS, c, "results.json"), {}).get("finalState") for k, c in CELLS_FROZEN.items()}
    base = fs.get("ts->py")
    for k in ("ts->ts", "py->ts", "py->py"):
        d = diff_json(base, fs[k]) if base and fs.get(k) else [("?", "missing", "missing")]
        w(f"- final `read {{state:true}}` of s1, {k} vs ts->py: " + ("identical" if not d else "DIFFERENT: " + "; ".join(f"`{p}` {x} vs {y}" for p, x, y in d[:6])))
    cl = {}
    for e, s in (("ts", "py"), ("py", "py"), ("py", "ts"), ("ts", "ts")):
        c = (execs.get((e, s, "late"), {}).get("claims") or [{}])[0]
        cl[(e, s)] = sorted(c)
    w(f"- claim fields in the StateView after an executor run: Python server {cl[('ts','py')]} / {cl[('py','py')]}; TS server {cl[('py','ts')]} / {cl[('ts','ts')]}")
    w("")
    # probes
    pp, pt = probes_of(CELLS_LIVE["ts->py"]), probes_of(CELLS_LIVE["ts->ts"])
    w("## 2. Wire probes (raw frames at each server)\n")
    w("`probe.mjs` sends hand-built frames to a server that already holds the scenario's space (run after the TS-client scenario). `expect` is the reading of the spec text. "
      "Python/TS columns: `ok` = matches expect, otherwise what came back.\n")
    w("| probe | what | expect | Python server | TS server | same? |"); w("|---|---|---|---|---|---|")
    div = []
    for pid in sorted(set(pp) | set(pt), key=lambda s: (len(s), s)):
        a, b = pp.get(pid, {}), pt.get(pid, {})
        sa, sb = str(a.get("summary", a.get("error"))), str(b.get("summary", b.get("error")))
        norm = lambda s: s
        same = norm(sa) == norm(sb)
        if not same: div.append(pid)
        fa = ("ok" if a.get("pass") else "**" + sa[:70] + "**"); fb = ("ok" if b.get("pass") else "**" + sb[:70] + "**")
        w(f"| {pid} | {a.get('title', b.get('title', ''))} | {a.get('expect', b.get('expect', ''))} | {fa} | {fb} | {'yes' if same else '**NO**'} |")
    w("")
    npass_a, npass_b = sum(1 for p in pp.values() if p.get("pass")), sum(1 for p in pt.values() if p.get("pass"))
    w(f"Probes matching the spec reading: Python server {npass_a}/{len(pp)}, TS server {npass_b}/{len(pt)}. Probes where the two servers answer differently: {len(div)} ({', '.join(div)}).\n")
    # well-known
    w("## 3. Failing steps with their frames\n")
    w("### 3.1 Python client on the TS server: WebSocket path\n")
    segs, ao = segments(tap(CELLS_LIVE["py->ts"]))
    ev = [e for e in segs.get("op:hello", [])]
    w("The README's client example dials `ws://127.0.0.1:PORT` (no path). The TS server only upgrades `/han`. Tap log of the first connection attempt (the harness retries with `/han`, which works, so `hello` is `PASS*`):\n")
    w("```json");
    for e in ev[:6]: w(json.dumps({k: v for k, v in e.items() if k not in ("n", "ms")}))
    w("```\n")
    w("### 3.2 Probe divergences: exact frames\n")
    for pid in div:
        for label, cell in (("Python server", CELLS_LIVE["ts->py"]), ("TS server", CELLS_LIVE["ts->ts"])):
            sg, ao2 = segments(tap(cell))
            fr = frames_of(sg.get("probe:" + pid, []), ao2)
            fr = [f for f in fr if not (("frame" in f) and (f["frame"].get("call") == "hello" or "versions" in f["frame"] or ("ok" in f["frame"] and isinstance(f["frame"]["ok"], dict) and "limits" in f["frame"]["ok"])))]
            fr = [f for f in fr if "frame" in f]
            w(f"**{pid}** ({(pp.get(pid) or pt.get(pid)).get('title')}), {label}:")
            w("```")
            for f in fr[:8]:
                s = json.dumps(f["frame"], ensure_ascii=False)
                w(f"{'>' if f['dir'] == 'c2s' else '<'} {f['conn'].split(':')[0] if f['conn'].startswith('@') else f['conn']} {s[:520]}")
            w("```")
    w("")
    # exec detail
    w("### 3.3 Executor cells\n")
    for (e, s, order), r in sorted(execs.items(), key=lambda kv: (kv[0][2], kv[0][0], kv[0][1])):
        w(f"- exec-{e} on {s} server, {order}: phase `{r.get('phase')}`, claims {len(r.get('claims') or [])}, file written {r.get('fileExists')}; "
          + ("" if r.get("pass") else f"see `runs/exec-{e}-{s}-{order}/exec.log`"))
    w("")
    w("### 3.4 Frames of the matrix failures" + NL)
    ev_all = tap("exec-ts-py-early")
    sg, ao3 = segments(ev_all)
    allfr = [f for f in frames_of([e for v in sg.values() for e in v], ao3) if "frame" in f and f["conn"].startswith("@x")]
    w("TS executor started before the space exists, against the Python server (`exec-ts-py-early`; the same frames occur against the TS server). The executor's follow is answered `not_found` and it never asks again:" + NL)
    w("```")
    for f in allfr[:6]:
        if "ok" in f["frame"] and "limits" in str(f["frame"]): continue
        w(f"{'>' if f['dir'] == 'c2s' else '<'} {json.dumps(f['frame'], ensure_ascii=False)[:300]}")
    w("```" + NL)
    sg2, ao4 = segments(tap(CELLS_LIVE["ts->py"]))
    w("TS verifier on `profile:@alice:a.localhost` (Python server, server key id `h1`). The record is served and its receipt is valid; the TS verifier then looks for the entry signer `@:a.localhost` under key id `srv1` only (`verify.ts` viewOf) and reports `seq 1: entry signature does not verify against the pinned key`:" + NL)
    w("```")
    for f in frames_of(sg2.get("verifier:profile", []), ao4):
        fr = f.get("frame", {})
        it = fr.get("item") if isinstance(fr, dict) else None
        if it and it.get("type") == "record":
            e, rc = it["record"]["entry"], it["record"]["receipt"]
            w("< " + json.dumps({"id": fr["id"], "item": {"type": "record", "record": {"entry": {k: e[k] for k in ("space", "kind", "author", "keyId", "sig")}, "receipt": {k: rc[k] for k in ("seq", "home", "homeKey", "profiles")}}}}))
    w("```" + NL)
    # root causes (hand written)
    rc = os.path.join(HERE, "rootcauses.md")
    if os.path.exists(rc): w(open(rc, encoding="utf-8").read())
    keep = ""                                    # section 6 onward (closure, recorded wire-vector runs) is written by hand: keep it
    try:
        prev = open(os.path.join(HERE, "RESULTS.md"), encoding="utf-8").read()
        if "\n## 6. " in prev: keep = prev[prev.index("\n## 6. "):]
    except FileNotFoundError: pass
    open(os.path.join(HERE, "RESULTS.md"), "w", encoding="utf-8").write("\n".join(L) + "\n" + keep)
    idx = golden(live)
    nv, ns = golden_probes()
    print("probe vectors", nv, "skipped", ns)
    print("RESULTS.md written;", len(idx), "golden files;", "divergent probes:", div)
    for c in cols: print(c, tot[c])

if __name__ == "__main__":
    main()

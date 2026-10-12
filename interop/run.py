"""Interop matrix runner.   .venv\\Scripts\\python.exe run.py [--only CLIENT-SERVER[-MODE] ...] [--skip-exec]
Cells: client impl x server impl x clock mode (live = real clocks; frozen = 2026-01-01T00:00:00Z, for golden transcripts).
Everything lands in interop/runs/<cell>/ (tap logs, driver results, server logs); report.py turns that into RESULTS.md and golden/."""
import argparse, json, os, shutil, subprocess, sys, time, urllib.request
from lib import *

RUNS = os.path.join(HERE, "runs")
KEYS = os.path.join(HERE, "runs", "keys")

def server_cmd(kind, mode, port, wd, keys):
    actors = os.path.join(wd, "actors.json")
    json.dump(actors_list(keys), open(actors, "w"))
    if kind == "py":
        kf = os.path.join(KEYS, "server.json")
        if keys["server"]["keyId"] != "h1":      # same seed, another keyId (the TS verifier's hard-coded one)
            kf = os.path.join(wd, "server-key.json"); json.dump(keys["server"], open(kf, "w"))
        if mode == "frozen":
            return [PY, os.path.join(HERE, "py_host.py"), "--server", SERVER, "--port", str(port), "--db", os.path.join(wd, "seq.db"),
                    "--key", kf, "--actors", actors, "--freeze", T0], IMPL
        cmd = [PY, "-m", "han.seq", "--server", SERVER, "--port", str(port), "--db", os.path.join(wd, "seq.db"), "--key", kf]
        for a in actors_list(keys):
            n = a["actor"][1:].split(":")[0]
            cmd += ["--register", f"{a['actor']}={os.path.join(KEYS, n + '.json')},{a['kind']}" + (f",{a['operator']}" if a.get("operator") else "")]
        return cmd, IMPL
    d = os.path.join(wd, "tsdata"); os.makedirs(d, exist_ok=True)
    s = keys["server"]
    json.dump({"keyId": s["keyId"], "publicKey": s["public"], "d": s["private"], "from": "1970-01-01T00:00:00Z"}, open(os.path.join(d, "serverkey.json"), "w"))
    cmd = [NODE, os.path.join(HERE, "ts_host.mjs"), "--server", SERVER, "--dir", d, "--port", str(port), "--actors", actors]
    if mode == "frozen": cmd += ["--freeze", T0]
    return cmd, HERE

def start_stack(kind, mode, wd, keys):
    os.makedirs(wd, exist_ok=True)
    sp, tp = free_ports(2)
    cmd, cwd = server_cmd(kind, mode, sp, wd, keys)
    srv = spawn("server-" + kind, cmd, os.path.join(wd, "server.log"), cwd=cwd)
    srv.wait_for("listening on" if (kind == "py" and mode == "live") else "READY")
    tap = spawn("tap", [NODE, os.path.join(HERE, "tap.mjs"), "--listen", str(tp), "--upstream", str(sp), "--log", os.path.join(wd, "tap.jsonl")], os.path.join(wd, "tap.log"))
    tap.wait_for("TAP listening")
    return srv, tap, sp, tp

def run_to_end(name, cmd, wd, timeout=240, cwd=None):
    p = subprocess.run(cmd, cwd=cwd or HERE, capture_output=True, text=True, timeout=timeout, encoding="utf-8", errors="replace",
                       env=dict(os.environ, PYTHONIOENCODING="utf-8"))
    open(os.path.join(wd, name + ".log"), "w", encoding="utf-8").write(f"$ {' '.join(cmd)}\n[exit {p.returncode}]\n--- stdout\n{p.stdout}\n--- stderr\n{p.stderr}")
    return p

def client_cmd(client, scen, wd, tp, out, mode="scenario", flag_dir=None):
    tap = f"http://127.0.0.1:{tp}"
    if client == "ts":
        return [NODE, os.path.join(HERE, "ts_driver.mjs"), "--scenario", scen, "--keys", KEYS, "--url", f"ws://127.0.0.1:{tp}/han", "--tap", tap, "--out", out, "--mode", mode] + (["--flag-dir", flag_dir] if flag_dir else [])
    return [PY, os.path.join(HERE, "py_driver.py"), "--scenario", scen, "--keys", KEYS, "--url", f"ws://127.0.0.1:{tp}", "--url2", f"ws://127.0.0.1:{tp}/han",
            "--tap", tap, "--out", out, "--mode", mode] + (["--flag-dir", flag_dir] if flag_dir else [])

def mark(tp, name): urllib.request.urlopen(f"http://127.0.0.1:{tp}/__mark?name={name}", timeout=3).read()

def verifier(client, kind, wd, tp, keys):
    """The verifier of the client's implementation follows spaces on the server (TS verifier <- py server, py verifier <- ts server, plus baselines).
    Two spaces: s1 (user entries) and profile:@alice (entries authored by the server actor @:S)."""
    sk = keys["server"]; res_all = {}
    for tag, space in (("s1", "s1:" + SERVER), ("profile", f"profile:@alice:{SERVER}")):
        mark(tp, "verifier:" + tag)
        out = os.path.join(wd, f"verify-{tag}.json")
        if client == "ts":
            p = run_to_end("verify-" + tag, [NODE, os.path.join(HERE, "ts_verify.mjs"), "--url", f"ws://127.0.0.1:{tp}/han", "--server", SERVER, "--actor", "vera", "--keys", KEYS,
                                      "--space", space, "--server-key", sk["public"], "--server-key-id", sk["keyId"], "--wellknown", f"http://127.0.0.1:{tp}/.well-known/han", "--out", out], wd, 60)
            res = json.load(open(out)) if os.path.exists(out) else {"error": "no output"}
            res["pass"] = bool(res.get("verdict", {}).get("ok")) and not res.get("tamper", {}).get("ok", True)
        else:
            url = f"ws://127.0.0.1:{tp}" + ("/han" if kind == "ts" else "")
            cmd = [PY, "-m", "han.verify", "follow", "--url", url, "--actor", f"@vera:{SERVER}", "--key", os.path.join(KEYS, "vera.json"), "--space", space,
                   "--server-key", f"{SERVER}={sk['public']},{sk['keyId']}", "--once", "--json", "--server", SERVER]
            p = run_to_end("verify-" + tag, cmd, wd, 90, cwd=IMPL)
            res = {"exit": p.returncode, "stdout": p.stdout.strip()[-1500:], "stderr": p.stderr.strip()[-1500:], "pass": p.returncode == 0}
        json.dump(res, open(out, "w"), indent=1)
        res_all[tag] = res
    json.dump(dict(res_all["s1"], profile=res_all["profile"]), open(os.path.join(wd, "verify.json"), "w"), indent=1)
    return res_all

def scenario_cell(client, kind, mode, keys, tag=""):
    cell = f"{client}-{kind}-{mode}{tag}"; wd = os.path.join(RUNS, cell)
    shutil.rmtree(wd, ignore_errors=True); os.makedirs(wd)
    print(f"== cell {cell}", flush=True)
    scen = os.path.join(wd, "scenario.json"); json.dump(scenario(), open(scen, "w"), indent=1)
    srv, tap, sp, tp = start_stack(kind, mode, wd, keys)
    try:
        p = run_to_end("driver", client_cmd(client, scen, wd, tp, os.path.join(wd, "results.json")), wd, 300)
        print("  driver exit", p.returncode, flush=True)
        verifier(client, kind, wd, tp, keys)
        if client == "ts":
            mark(tp, "probes:start")
            run_to_end("probes", [NODE, os.path.join(HERE, "probe.mjs"), "--port", str(tp), "--server", SERVER, "--keys", KEYS, "--space", "s1:" + SERVER,
                                  "--out", os.path.join(wd, "probes.json"), "--tapport", str(tp)], wd, 120)
        wk = {}
        for path in ("/.well-known/han", "/.well-known/han-peers"):
            try: wk[path] = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{tp}{path}", timeout=5).read())
            except Exception as ex: wk[path] = {"error": str(ex)}
        json.dump(wk, open(os.path.join(wd, "wellknown.json"), "w"), indent=1)
    finally:
        tap.stop(); srv.stop()

def exec_cell(execimpl, kind, order):
    """execimpl runs the executor against the `kind` server; the setup client is the native one of the server (so the executor is the only cross-implementation piece).
    order "early": the executor starts before the space exists. "late": after the space and its members exist, before the proposal."""
    cell = f"exec-{execimpl}-{kind}-{order}"; wd = os.path.join(RUNS, cell)
    shutil.rmtree(wd, ignore_errors=True); os.makedirs(wd)
    print(f"== cell {cell}", flush=True)
    scen = os.path.join(wd, "scenario.json"); json.dump(exec_scenario(), open(scen, "w"), indent=1)
    keys = json.load(open(os.path.join(KEYS, "all.json")))
    srv, tap, sp, tp = start_stack(kind, "live", wd, keys)
    sandbox, jdir = os.path.join(wd, "sandbox"), os.path.join(wd, "exec-state"); os.makedirs(jdir)
    flags = os.path.join(wd, "flags"); os.makedirs(flags)
    xurl = f"ws://127.0.0.1:{tp}" + ("/han" if (execimpl == "ts" or kind == "ts") else "")
    def start_exec():
        if execimpl == "py":
            shutil.copy(os.path.join(KEYS, "x.json"), os.path.join(jdir, "x.json"))     # the X-07 marker `x.json.used` lands next to the key
            ex = spawn("exec-py", [PY, "-m", "han.exec", "--url", xurl, "--actor", f"@x:{SERVER}", "--key", os.path.join(jdir, "x.json"), "--instance", "k1",
                                   "--journal", os.path.join(jdir, "journal"), "--spaces", "e1:" + SERVER, "--actions", "file.write", "--sandbox", sandbox,
                                   "--backoff-base", "1", "--server", SERVER], os.path.join(wd, "exec.log"), cwd=IMPL)
            ex.wait_for("ready", 30)
        else:
            x = keys["x"]
            json.dump({"keyId": x["keyId"], "publicKey": x["public"], "d": x["private"], "journalId": "interopjournal"}, open(os.path.join(jdir, "instance.json"), "w"))
            open(os.path.join(jdir, "journal.jsonl"), "w").write(json.dumps({"t": "header", "journalId": "interopjournal", "keyId": x["keyId"]}) + chr(10))
            ex = spawn("exec-ts", [NODE, os.path.join(HERE, "ts_exec.mjs"), "--url", xurl, "--actor", f"@x:{SERVER}", "--server", SERVER, "--dir", jdir,
                                   "--sandbox", sandbox, "--spaces", "e1:" + SERVER], os.path.join(wd, "exec.log"))
            ex.wait_for("ts-exec ready", 30)
    dp = None
    try:
        setup_client = "ts" if kind == "ts" else "py"
        out = os.path.join(wd, "setup.json")
        cmd = client_cmd(setup_client, scen, wd, tp, out, "exec", flags if order == "late" else None)
        if order == "early":
            start_exec()
            p = run_to_end("setup-driver", cmd, wd, 150)
        else:
            dp = subprocess.Popen(cmd, cwd=HERE, stdout=open(os.path.join(wd, "setup-driver.log"), "wb"), stderr=subprocess.STDOUT)
            t0 = time.time()
            while not os.path.exists(os.path.join(flags, "spaceready")) and time.time() - t0 < 40: time.sleep(0.1)
            start_exec(); mark(tp, "exec:started"); open(os.path.join(flags, "go"), "w").write("1")
            dp.wait(timeout=150)
        res = json.load(open(out)) if os.path.exists(out) else {"fatal": "no output"}
        f = os.path.join(sandbox, "out", "hello.txt")
        res["fileExists"] = os.path.exists(f)
        res["fileContent"] = open(f, encoding="utf-8").read() if res["fileExists"] else None
        res["pass"] = res.get("phase") == "succeeded" and res["fileContent"] == "hello from interop" + chr(10)
        time.sleep(0.5)
        json.dump(res, open(os.path.join(wd, "exec-result.json"), "w"), indent=1)
        print("  result", res.get("phase"), res["fileExists"], flush=True)
    finally:
        if dp is not None and dp.poll() is None: dp.kill()
        cleanup()

def main():
    ap = argparse.ArgumentParser(); ap.add_argument("--only", nargs="*"); ap.add_argument("--skip-exec", action="store_true"); ap.add_argument("--skip-scenario", action="store_true")
    a = ap.parse_args()
    os.makedirs(RUNS, exist_ok=True)
    keys = make_keys(KEYS); json.dump(keys, open(os.path.join(KEYS, "all.json"), "w"))
    try:
        if not a.skip_scenario:
            for mode in ("live", "frozen"):
                for client, kind in (("ts", "py"), ("py", "ts"), ("ts", "ts"), ("py", "py")):
                    if a.only and f"{client}-{kind}-{mode}" not in a.only: continue
                    scenario_cell(client, kind, mode, keys); cleanup()
        if not a.skip_scenario and (not a.only or "ts-ts-live-srv1" in a.only):
            k2 = json.loads(json.dumps(keys)); k2["server"]["keyId"] = "srv1"       # the TS sequencer's own default key id
            scenario_cell("ts", "ts", "live", k2, "-srv1"); cleanup()
            scenario_cell("py", "ts", "live", k2, "-srv1"); cleanup()
            scenario_cell("ts", "py", "live", k2, "-srv1"); cleanup()
        if not a.skip_exec:
            for order in ("late", "early"):
                for e, k in (("py", "ts"), ("ts", "py"), ("py", "py"), ("ts", "ts")):
                    if a.only and f"exec-{e}-{k}-{order}" not in a.only: continue
                    exec_cell(e, k, order); cleanup()
    finally:
        cleanup()

if __name__ == "__main__":
    main()

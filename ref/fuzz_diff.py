"""Differential fuzz (decision H5): ref/policy.py against the clean-room TypeScript Policy on random histories.

For each seed this builds a random history with fuzz_fold's generator, computes the reference verdict of every step and
the reference folded state after every allowed step, and writes a vector file whose steps carry `expect` = the ref
verdict and a `check` of the WHOLE state (path "", which the TS runner resolves to the state itself), plus occasional
`guardStatus` steps. The TS runner (cleanroom-ts/dist/run-vectors.js) is then run over the generated directories in a
few batches. The first divergence per seed is reported with the step before it and, for a state divergence, the
top-level state keys that differ. Each divergent seed's history up to the divergence is saved as a minimal vector.
Usage: python ref/fuzz_diff.py [--seeds N] [--steps N] [--keep DIR]      (default 2000 seeds × 100 steps)
"""
import argparse, copy, json, os, random, re, shutil, subprocess, sys, time
from datetime import timedelta
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import policy as P
import fuzz_fold as F

TS = os.path.join(os.path.dirname(HERE), "cleanroom-ts")
RUNNER = os.path.join(TS, "dist", "run-vectors.js")
WORK = r"H:\agentic\temp\claude\H--HAN\5c1257e4-8c11-4fbf-80b1-3b3209de9c9f\scratchpad\fuzzdiff"
BATCH = 500
fmt = lambda t: (F.BASE + timedelta(seconds=t)).strftime("%Y-%m-%dT%H:%M:%SZ")

def view(a, revoked=False):
    p = F.profile(a, revoked)
    return {k: v for k, v in p.items() if k not in ("actor", "seq")}

def history(seed, steps):
    """One random history as a vector: ref verdicts, whole folded states, guardStatus probes."""
    r = random.Random(seed)
    revoked, t = set(), 0
    profiles = {a: view(a) for a in F.ACTORS}
    out = []
    def ctx_of(time_):
        return {"time": time_, "profiles": {a: dict(v, actor=a, seq=1) for a, v in profiles.items()}}
    state = None
    def step(e, time_, override=None):
        nonlocal state
        ctx = ctx_of(time_)
        d = P.decide(state, e, ctx)
        verdict = "allow" if d["allow"] else d["code"]
        st = {"entry": e, "time": time_, "expect": verdict}
        if override: st["profiles"] = override
        if d["allow"]:
            state = P.fold(state, {"entry": e, "content": None, "receipt": {"space": F.S, "seq": (state["head"] if state else 0) + 1, "id": e["id"], "time": time_}}, ctx)
            st["check"] = {"": state}
        out.append(st)
        return ctx
    create = {"id": F.H("create"), "v": 1, "space": F.S, "kind": "space.create", "author": F.O, "keyId": "k1",
              "body": {"policy": F.policy(r)}, "contentHash": "", "refs": [], "seen": 0, "nonce": "c", "time": "2026-10-10T12:00:00Z"}
    step(create, fmt(0))
    for i, a in enumerate([F.M, F.PR, F.X, F.BOT, F.G]):
        step({"id": F.H(f"setup{i}"), "v": 1, "space": F.S, "kind": "member.set", "author": F.O, "keyId": "k1",
              "body": {"actor": a, "role": "guest" if a == F.G else "member"}, "contentHash": "", "refs": [], "seen": 0,
              "nonce": f"s{i}", "time": "2026-10-10T12:00:00Z"}, fmt(0))
    for n in range(steps):
        t += r.choice([0, 1, 5, 20, 45])
        override = None
        if r.random() < .03:
            a = r.choice(F.ACTORS); revoked ^= {a}
            profiles[a] = view(a, a in revoked); override = {a: profiles[a]}
        e = F.random_entry(r, state, n)
        ctx = step(e, fmt(t), override)
        if r.random() < .15:
            a = r.choice(F.ACTORS)
            out.append({"guardStatus": {"space": F.S, "actor": a}, "expect": P.guard_status(state, ctx, a)})
    return {"name": f"fuzz-diff seed {seed}", "profiles": {a: view(a) for a in F.ACTORS}, "steps": out}

def write(d, name, vec):
    with open(os.path.join(d, name + ".json"), "w", encoding="utf-8") as f: json.dump(vec, f)

def node(d):
    p = subprocess.run(["node", RUNNER, d], capture_output=True, text=True, encoding="utf-8", errors="replace")
    first = {}
    for line in p.stdout.splitlines():
        m = re.match(r"^(\w+)\.json#(\d+): (.*)$", line)
        if m and m.group(1) not in first: first[m.group(1)] = (int(m.group(2)), m.group(3))
    summ = re.findall(r"steps pass (\d+) fail (\d+); vectors pass (\d+) fail (\d+)", p.stdout)
    if not summ: raise RuntimeError("TS runner gave no summary: " + (p.stdout + p.stderr)[-500:])
    return first, tuple(map(int, summ[-1]))

REPLAY = """
import { readFileSync, readdirSync } from "node:fs";
import { decide, fold } from "%s";
for (const f of readdirSync(process.argv[2]).filter((x) => /^seed\d+\.json$/.test(x))) {
const v = JSON.parse(readFileSync(process.argv[2] + "/" + f, "utf8"));
const profiles = JSON.parse(JSON.stringify(v.profiles)); let state = null;
for (const st of v.steps) {
  if (st.profiles) Object.assign(profiles, st.profiles);
  if (!st.entry) continue;
  const { id, ...entry } = st.entry;
  const ctx = { time: st.time, profiles: JSON.parse(JSON.stringify(profiles)) };
  const d = decide(state && JSON.parse(JSON.stringify(state)), JSON.parse(JSON.stringify(entry)), ctx);
  if (!d.allow) continue;
  const rec = { entry, content: null, redacted: false, receipt: { space: entry.space, seq: (state?.head ?? 0) + 1, id, prev: "", home: "", homeKey: "k", time: st.time, durability: "local", profiles: {}, homeSig: "" } };
  state = fold(state && JSON.parse(JSON.stringify(state)), JSON.parse(JSON.stringify(rec)), ctx);
}
console.log(f.slice(0, -5) + "	" + JSON.stringify(state));
}
"""

def deep_diff(a, b, path="", out=None):
    out = [] if out is None else out
    if isinstance(a, dict) and isinstance(b, dict):
        for k in sorted(set(a) | set(b)):
            if k not in a: out.append((f"{path}.{k}".lstrip("."), "<missing>", b[k]))
            elif k not in b: out.append((f"{path}.{k}".lstrip("."), a[k], "<missing>"))
            else: deep_diff(a[k], b[k], f"{path}.{k}", out)
    elif isinstance(a, list) and isinstance(b, list) and len(a) == len(b):
        for i, (x, y) in enumerate(zip(a, b)): deep_diff(x, y, f"{path}.{i}", out)
    elif a != b: out.append((path.lstrip("."), a, b))
    return out

def ts_states(d, base):
    """The TS state after replaying each repro vector in d (one node run): {name: state}."""
    js = os.path.join(base, "replay.mjs")
    with open(js, "w", encoding="utf-8") as f: f.write(REPLAY % ("file:///" + os.path.join(TS, "dist", "policy.js").replace("\\", "/")))
    p = subprocess.run(["node", js, d], capture_output=True, text=True, encoding="utf-8", errors="replace")
    out = {}
    for line in p.stdout.splitlines():
        n, _, j = line.partition("	")
        out[n] = json.loads(j)
    return out

def repro_of(v, i):
    return dict(v, steps=v["steps"][:i + 1])

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seeds", type=int, default=2000)
    ap.add_argument("--steps", type=int, default=100)
    ap.add_argument("--keep", default=WORK)
    a = ap.parse_args()
    if shutil.which("node") is None or not os.path.exists(RUNNER):
        print("SKIP differential fuzz: node or cleanroom-ts/dist/run-vectors.js is missing"); return 0
    t0 = time.time()
    shutil.rmtree(a.keep, ignore_errors=True); os.makedirs(a.keep)
    steps_n = allowed = 0; divergences = []
    for lo in range(0, a.seeds, BATCH):
        d = os.path.join(a.keep, f"batch{lo // BATCH}"); os.makedirs(d)
        vecs = {}
        for seed in range(lo, min(lo + BATCH, a.seeds)):
            v = history(seed, a.steps); vecs[f"seed{seed}"] = v
            write(d, f"seed{seed}", v)
            steps_n += len(v["steps"]); allowed += sum(1 for s in v["steps"] if s.get("expect") == "allow")
        first, _ = node(d)
        for name, (i, msg) in sorted(first.items(), key=lambda kv: int(kv[0][4:])):
            v = vecs[name]
            os.makedirs(os.path.join(a.keep, "divergences"), exist_ok=True)
            write(os.path.join(a.keep, "divergences"), name, dict(repro_of(v, i), note=f"first divergence at step {i}: {msg[:300]}"))
            divergences.append([name, i, msg, None, v["steps"][i]])
        shutil.rmtree(d)
    if divergences:
        ts = ts_states(os.path.join(a.keep, "divergences"), a.keep)
        for dv in divergences:
            if dv[2].startswith("check") and dv[0] in ts:
                dv[3] = deep_diff(dv[4]["check"][""], ts[dv[0]])[:4]
    groups = {}                      # divergences with the same cause (entry kind + what differs) are one finding
    for name, i, msg, keys, st in divergences:
        what = st["entry"]["kind"] if "entry" in st else next(k for k in st if k != "expect")
        if keys: cause = "state " + "; ".join(f"{re.sub(r'[0-9a-f]{64}', '<id>', p)}: ref={json.dumps(x)[:60]} ts={json.dumps(y)[:60]}" for p, x, y in keys)
        else: cause = re.sub(r"[0-9a-f]{64}", "<id>", msg if not msg.startswith("check") else msg[:60])
        groups.setdefault((what, cause), []).append((name, i, st))
    for (what, cause), L in sorted(groups.items(), key=lambda kv: -len(kv[1])):
        name, i, st = L[0]
        body = json.dumps(st.get("entry", {}).get("body", st.get(what)))[:160]
        seeds = ", ".join(n[4:] for n, _, _ in L[:12]) + (" ..." if len(L) > 12 else "")
        print(f"DIVERGE x{len(L)} on {what}: {cause}")
        print(f"    e.g. {name} step {i} {body}")
        print(f"    seeds: {seeds}")
    print(f"{a.seeds} seeds × {a.steps} steps: {steps_n} vector steps ({allowed} allowed, whole state compared after each); "
          f"{len(divergences)} seeds diverge; {time.time() - t0:.0f}s")
    if divergences: print(f"minimal repro vectors: {os.path.join(a.keep, 'divergences')}")
    return 1 if divergences else 0

if __name__ == "__main__":
    sys.exit(main())

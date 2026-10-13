"""Differential fuzz (decision H5): ref/policy.py against the clean-room TypeScript Policy on random histories.

For each seed this builds a random history with fuzz_fold's generator, computes the reference verdict of every step and
the reference folded state after every allowed step, and writes a vector file whose steps carry `expect` = the ref
verdict and a `check` of the WHOLE state (path "", which the TS runner resolves to the state itself), plus occasional
`guardStatus` steps. The TS runner (witnesses/cleanroom-ts/dist/run-vectors.js) is then run over the generated directories in a
few batches. The first divergence per seed is reported with the step before it and, for a state divergence, the
top-level state keys that differ. Each divergent seed's history up to the divergence is saved as a minimal vector.
The generator (fuzz_fold's) has several operators separate from the owner, operators withdrawing and confirming again (an
operator never changes, §4.3),
forcing switched on and off, unguards, delegation chains grown from guarded agents, and guarded agents trying the kinds they may
not use (P9), guarded agents leaving, and approvals by agents that become guarded (R2); one seed in four is a crowd of 23
actors (M9), so the 16-approval cap, a guard approving beyond it (R3) and the 64-assignee bound are reached. Histories are built in parallel worker processes and the TS runner takes 1000 seeds a call.
The run prints the generator's statistics so a reader can see what was exercised, the caps included.
--target ts|rust|both (default ts): the implementation the reference is compared with. `rust` runs the han-conform binary of crates/han-core
(built with cargo before every run; --rust-bin PATH names another, used as is), which speaks the same protocol as the TS runner: it takes a directory of vector
files and prints `<file>.json#<step>: <message>` per failing step and a `steps pass N fail M; vectors pass A fail B` summary.
Usage: python ref/fuzz_diff.py [--target ts|rust|both] [--seeds N] [--seed-offset K] [--steps N] [--keep DIR] [--workers N]      (default 1000 seeds from 0 x 250 steps, about 4 minutes; use --seed-offset for fresh seeds)
--keep DIR: where the batches and the divergence repros go (default: a new directory under the system temp). Only the batch*/,
divergences/ and replay.mjs this script writes are ever removed there; DIR itself is never deleted.
"""
import argparse, collections, copy, json, multiprocessing, os, random, re, shutil, subprocess, sys, tempfile, time
from datetime import timedelta
HERE = os.path.dirname(os.path.abspath(__file__))
try:
    sys.stdout.reconfigure(encoding="utf-8")       # the docstring and the report use non-ASCII text; a cp1252 console must not crash
except Exception:
    pass
sys.path.insert(0, HERE)
import policy as P
import fuzz_fold as F

TS = os.path.join(os.path.dirname(HERE), "witnesses", "cleanroom-ts")
RUNNER = os.path.join(TS, "dist", "run-vectors.js")
BATCH = 1000
ROOT = os.path.dirname(HERE)
RUST_BIN = os.path.join(ROOT, "target", "release", "han-conform" + (".exe" if os.name == "nt" else ""))
fmt = F.fmt

def view(a, revoked=False, unconfirmed=False):
    p = F.profile(a, revoked, unconfirmed)
    return {k: v for k, v in p.items() if k not in ("actor", "seq")}

def history(seed, steps):
    """One random history as a vector: ref verdicts, whole folded states, guardStatus probes; also the generator's statistics."""
    r = random.Random(seed)
    revoked, unconf, t = set(), set(), 0
    actors = F.actors_of(seed)
    profiles = {a: view(a) for a in actors}
    out, stats = [], collections.Counter()
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
        stats[f"{e['kind']} {verdict}"] += 1
        F.count(stats, e, d, state, ctx)
        if d["allow"]:
            prev = state
            state = P.fold(state, {"entry": e, "content": None, "receipt": {"space": F.S, "seq": (state["head"] if state else 0) + 1, "id": e["id"], "time": time_}}, ctx)
            st["check"] = {"": state}
            if prev: F.count_guard(stats, prev, state, e, d, ctx)
            if state["guarded"]: stats["* steps with an agent guarded by choice"] += 1
            if state["policy"]["sessions"].get("forceGuarded"): stats["* steps in a forced space"] += 1
        elif state: F.count_guard(stats, state, None, e, d, ctx)
        out.append(st)
        return ctx
    create = {"id": F.H("create"), "v": 1, "space": F.S, "kind": "space.create", "author": F.O, "keyId": "k1",
              "body": {"policy": F.policy(r)}, "contentHash": "", "refs": [], "seen": 0, "nonce": "c", "time": "2026-10-10T12:00:00Z"}
    if actors is not F.ACTORS: create["body"]["policy"]["approvals"] = [{"action": "*", "approvers": ["member"]}]; stats["crowd seeds"] += 1
    step(create, fmt(0))
    for ms in F.setup(r, None, None, actors): step(ms, fmt(0))
    for n in range(steps):
        t += r.choice([0, 1, 5, 20, 45])
        before = set(unconf)
        changed = F.flip(r, revoked, unconf, actors)
        for a in changed: profiles[a] = view(a, a in revoked, a in unconf)
        if before - unconf: stats["* operator confirmed again"] += 1
        override = {a: profiles[a] for a in changed} if changed else None
        e = F.random_entry(r, state, n, actors)
        ctx = step(e, fmt(t), override)
        if r.random() < .15:
            a = r.choice(actors)
            out.append({"guardStatus": {"space": F.S, "actor": a}, "expect": P.guard_status(state, ctx, a)})
            stats["* guardStatus probes"] += 1
    return {"name": f"fuzz-diff seed {seed}", "profiles": {a: view(a) for a in actors}, "steps": out}, stats

def build(job):
    seed, steps, d = job
    v, stats = history(seed, steps)
    write(d, f"seed{seed}", v)
    return seed, len(v["steps"]), sum(1 for s in v["steps"] if s.get("expect") == "allow"), stats, v

def write(d, name, vec):
    with open(os.path.join(d, name + ".json"), "w", encoding="utf-8") as f: json.dump(vec, f)

def runners(d, target, rust_bin):
    """The first divergence per seed from each implementation `target` names, merged (the earliest step wins)."""
    first = {}
    for name, cmd in (("ts", ["node", RUNNER, d]), ("rust", [rust_bin, d])):
        if target not in (name, "both"): continue
        for k, v in node_all(d, cmd)[0].items():
            if k not in first or v[0] < first[k][0]: first[k] = v
    return first

def node_all(d, cmd=None):
    """node(d); when a runner out of step with the reference fails every step, its report outgrows a string, so the directory is run in chunks of 100."""
    cmd = cmd or ["node", RUNNER, d]
    try:
        return node(d, cmd)
    except RuntimeError:
        files = sorted(os.listdir(d)); first = {}
        for k in range(0, len(files), 100):
            sub = os.path.join(d, f"chunk{k}"); os.makedirs(sub)
            for f in files[k:k + 100]: shutil.move(os.path.join(d, f), sub)
            first.update(node(sub, cmd[:-1] + [sub])[0])
        return first, None

def node(d, cmd=None):
    p = subprocess.run(cmd or ["node", RUNNER, d], capture_output=True, text=True, encoding="utf-8", errors="replace")
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
    ap = argparse.ArgumentParser(description="Differential fuzz: ref/policy.py against the clean-room TypeScript Policy (decision H5).")
    ap.add_argument("--target", choices=("ts", "rust", "both"), default="ts", help="the implementation compared with the reference")
    ap.add_argument("--rust-bin", default=RUST_BIN, help="the han-conform binary for --target rust|both (rebuilt with cargo every run when it is the default)")
    ap.add_argument("--seeds", type=int, default=1000)
    ap.add_argument("--seed-offset", type=int, default=0, help="the first seed (M9): run seeds K .. K+N-1")
    ap.add_argument("--steps", type=int, default=250)
    ap.add_argument("--workers", type=int, default=4)
    ap.add_argument("--keep", default=None, metavar="DIR", help="directory for the batches and the divergence repros (default: a new directory under the system temp); "
                    "only the batch*/, divergences/ and replay.mjs this script writes are removed from it, never DIR itself")
    a = ap.parse_args()
    if a.target in ("ts", "both") and (shutil.which("node") is None or not os.path.exists(RUNNER)):
        print("SKIP differential fuzz: node or witnesses/cleanroom-ts/dist/run-vectors.js is missing"); return 0
    if a.target in ("rust", "both"):                     # the default binary is (re)built every run, so a stale one is never compared (round 32)
        if a.rust_bin == RUST_BIN and subprocess.run(["cargo", "build", "--release", "--locked", "-p", "han-core", "--bin", "han-conform"], cwd=ROOT,
                                                     capture_output=True).returncode:
            print("cannot build the Rust target (cargo build -p han-core --bin han-conform failed)"); return 1
        if not os.path.exists(a.rust_bin):
            print(f"cannot run the Rust target: {a.rust_bin} is missing"); return 1
    t0 = time.time()
    made = a.keep is None
    if made: a.keep = tempfile.mkdtemp(prefix="han-fuzzdiff-")                 # ours, and new: nothing in it to clear
    else:
        os.makedirs(a.keep, exist_ok=True)
        for old in os.listdir(a.keep):                                             # only what an earlier run of this script wrote there
            path = os.path.join(a.keep, old)
            if os.path.isdir(path) and (old == "divergences" or re.fullmatch(r"batch[0-9]+", old)): shutil.rmtree(path)
            elif old == "replay.mjs": os.remove(path)
    steps_n = allowed = 0; divergences = []; stats = collections.Counter()
    pool = multiprocessing.Pool(a.workers)
    end = a.seed_offset + a.seeds
    for lo in range(a.seed_offset, end, BATCH):
        d = os.path.join(a.keep, f"batch{lo // BATCH}"); os.makedirs(d)
        vecs = {}
        for seed, n, al, st_, v in pool.imap_unordered(build, [(s, a.steps, d) for s in range(lo, min(lo + BATCH, end))], 8):
            vecs[f"seed{seed}"] = v; steps_n += n; allowed += al; stats.update(st_)
        first = runners(d, a.target, a.rust_bin)
        for name, (i, msg) in sorted(first.items(), key=lambda kv: int(kv[0][4:])):
            v = vecs[name]
            os.makedirs(os.path.join(a.keep, "divergences"), exist_ok=True)
            write(os.path.join(a.keep, "divergences"), name, dict(repro_of(v, i), note=f"first divergence at step {i}: {msg[:300]}"))
            divergences.append([name, i, msg, None, v["steps"][i]])
        shutil.rmtree(d)
    if divergences and a.target != "rust" and shutil.which("node"):
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
    pool.close()
    caps = ("crowd seeds",) + tuple(F.STATS)[1:]
    print("generator statistics, entries by kind and verdict: " + "; ".join(f"{k} {n}" for k, n in sorted(stats.items()) if not k.startswith("*") and k not in caps))
    print("generator statistics, caps reached (M9): " + "; ".join(f"{k} {stats[k]}" for k in caps))
    print("generator statistics, guard paths: " + "; ".join(f"{k[2:]} {n}" for k, n in sorted(stats.items()) if k.startswith("*")))
    print(f"{a.seeds} seeds from {a.seed_offset} × {a.steps} steps: {steps_n} vector steps ({allowed} allowed, whole state compared after each); "
          f"{len(divergences)} seeds diverge; {time.time() - t0:.0f}s")
    if divergences: print(f"minimal repro vectors: {os.path.join(a.keep, 'divergences')}")
    elif made:
        try: os.rmdir(a.keep)                                                      # the directory this run made, now empty (never a tree delete)
        except OSError: pass
    return 1 if divergences else 0

if __name__ == "__main__":
    sys.exit(main())

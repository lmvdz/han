"""Scores one checkout of han-timeline: a correctness gate, then benchmark scores.

    python score.py --repo <checkout> [--runs 3] [--sets train,validation,test] [--no-gate]

Prints one JSON object. The gate is pass/fail: formatting, clippy, and every test of the crate
in release mode (all conformance vectors, the authorisation, sync, time-travel, file-store and
layout tests, randomized convergence with SEEDS seeds). A failed gate scores nothing.

Each benchmark set runs examples/bench.rs with its own size and BENCH_SEED, `runs` times; per
metric the best run counts, and the set's score is the geometric mean of the per-op times of
all metrics (microseconds; lower is better). `train` guides the planner, `validation` decides
whether a change is kept, `test` is held out and only reported.

With `--ref <bench binary>` every run of the candidate is paired with a run of the reference
(the current best build), alternating, so both see the same machine load; each set then also
reports the reference's metrics and the per-metric ratio candidate / reference. Compare runs
only this way: the same code measured hours apart can differ by 2x on a shared machine.
"""

import argparse
import json
import math
import os
import re
import subprocess
import sys

SETS = {
    "train": {"n": 4000, "seed": 1},
    "validation": {"n": 6000, "seed": 2},
    "test": {"n": 10000, "seed": 3},
}
LINE = re.compile(r"^(?P<what>.+?)\s+[\d.]+ ms total\s+(?P<each>[\d.]+) \S+ each$")


def run(cmd, cwd, env=None, timeout=3600):
    p = subprocess.run(
        cmd,
        cwd=cwd,
        env=env,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=timeout,
    )
    return p.returncode, p.stdout + p.stderr


def gate(repo, seeds):
    env = dict(os.environ, SEEDS=str(seeds))
    steps = [
        ("fmt", ["cargo", "fmt", "-p", "han-timeline", "--check"]),
        (
            "clippy",
            ["cargo", "clippy", "-p", "han-timeline", "--all-targets", "--", "-D", "warnings"],
        ),
        ("tests", ["cargo", "test", "--release", "-p", "han-timeline"]),
    ]
    for name, cmd in steps:
        code, out = run(cmd, repo, env)
        if code != 0:
            return {"ok": False, "step": name, "log_tail": out[-4000:]}
    return {"ok": True}


def bench_exe(repo):
    code, out = run(
        ["cargo", "build", "--release", "-p", "han-timeline", "--example", "bench"], repo
    )
    if code != 0:
        raise SystemExit(json.dumps({"error": "bench build failed", "log_tail": out[-4000:]}))
    exe = os.path.join(repo, "target", "release", "examples", "bench")
    return exe + (".exe" if sys.platform == "win32" else "")


def bench_once(exe, cfg, best):
    env = dict(os.environ, BENCH_SEED=str(cfg["seed"]))
    code, out = run([exe, str(cfg["n"])], os.path.dirname(exe), env)
    if code != 0:
        raise SystemExit(json.dumps({"error": "bench failed", "log_tail": out[-4000:]}))
    for line in out.splitlines():
        m = LINE.match(line.strip())
        if m:
            # The size is part of the label; key metrics by the words without numbers.
            what = re.sub(r"\d[\d,.]*", "N", m.group("what")).strip()
            v = float(m.group("each"))
            best[what] = min(best.get(what, math.inf), v)


def geomean(metrics):
    return math.exp(sum(math.log(v) for v in metrics.values()) / len(metrics))


def bench_set(exe, cfg, runs, ref=None):
    best, ref_best = {}, {}
    for _ in range(runs):
        bench_once(exe, cfg, best)
        if ref:
            bench_once(ref, cfg, ref_best)
    out = {"score": geomean(best), "metrics": best}
    if ref:
        out["ref_score"] = geomean(ref_best)
        out["ref_metrics"] = ref_best
        out["ratios"] = {m: v / ref_best[m] for m, v in best.items() if m in ref_best}
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True)
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--sets", default="train,validation,test")
    ap.add_argument("--seeds", type=int, default=50)
    ap.add_argument("--no-gate", action="store_true")
    ap.add_argument("--ref", help="reference bench binary to pair every run with")
    ap.add_argument("--exe", help="measure this bench binary instead of building one")
    a = ap.parse_args()
    result = {}
    if not a.no_gate:
        result["gate"] = gate(a.repo, a.seeds)
        if not result["gate"]["ok"]:
            print(json.dumps(result))
            return
    exe = a.exe or bench_exe(a.repo)
    result["exe"] = exe
    result["sets"] = {s: bench_set(exe, SETS[s], a.runs, a.ref) for s in a.sets.split(",")}
    print(json.dumps(result))


if __name__ == "__main__":
    main()

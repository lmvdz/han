"""Scores one checkout of han-timeline: a correctness gate, then benchmark scores.

    python score.py --repo <checkout> [--runs 3] [--sets train,validation,test] [--no-gate]

Prints one JSON object. The gate is pass/fail: formatting, clippy, and every test of the crate
in release mode (all conformance vectors, the authorisation, sync, time-travel, file-store and
layout tests, randomized convergence with SEEDS seeds). A failed gate scores nothing.

Each benchmark set runs examples/bench.rs with its own size and BENCH_SEED, `runs` times; per
metric the best run counts, and the set's score is the geometric mean of the per-op times of
all metrics (microseconds; lower is better). `train` guides the planner, `validation` decides
whether a change is kept, `test` is held out and only reported.
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


def bench_set(exe, cfg, runs):
    best = {}
    env = dict(os.environ, BENCH_SEED=str(cfg["seed"]))
    for _ in range(runs):
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
    score = math.exp(sum(math.log(v) for v in best.values()) / len(best))
    return {"score": score, "metrics": best}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True)
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--sets", default="train,validation,test")
    ap.add_argument("--seeds", type=int, default=50)
    ap.add_argument("--no-gate", action="store_true")
    a = ap.parse_args()
    result = {}
    if not a.no_gate:
        result["gate"] = gate(a.repo, a.seeds)
        if not result["gate"]["ok"]:
            print(json.dumps(result))
            return
    exe = bench_exe(a.repo)
    result["sets"] = {s: bench_set(exe, SETS[s], a.runs) for s in a.sets.split(",")}
    print(json.dumps(result))


if __name__ == "__main__":
    main()

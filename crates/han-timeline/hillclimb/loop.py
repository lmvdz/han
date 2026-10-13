"""Hill-climbs han-timeline's speed: Claude plans, Codex edits, the scorer decides.

    python loop.py init                 # make the worktree, measure the baseline and its noise
    python loop.py run --rounds 1       # one supervised round
    python loop.py run --rounds 10 --patience 3
    python loop.py status

Both models run on the user's subscriptions, through their CLIs: the planner is Claude Code in
headless mode (`claude -p`, read-only tools, JSON-schema output) and the coder is `codex exec`
(workspace-write sandbox, no network, user config ignored). Both run inside a git worktree of
this clean-room repository, so the planner sees the same CLAUDE.md and denied paths.

Each round: the planner reads the scores, the per-stage breakdown and the past attempts and
proposes one change; Codex makes it; changes outside crates/han-timeline/src are refused; the
scorer runs the correctness gate and the benchmarks; the change is kept (committed with the
Clean-Room trailer) only if the gate passes and the validation score improves by more than the
measured noise. Everything is recorded under target/hillclimb (state.json and one folder per
round with the planner's answer, Codex's transcript, the diff and the scores) as provenance.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
WORK = os.path.join(REPO, "target", "hillclimb")
WT = os.path.join(WORK, "wt")
STATE = os.path.join(WORK, "state.json")
BRANCH = "climb/run"
ALLOWED = "crates/han-timeline/src/"
CODEX_MODEL = os.environ.get("CLIMB_CODEX_MODEL", "gpt-6.1-sol")
CLAUDE_MODEL = os.environ.get("CLIMB_CLAUDE_MODEL", "opus")

PLAN_SCHEMA = {
    "type": "object",
    "additionalProperties": False,
    "required": ["idea", "rationale", "files", "instructions", "expected_gain_pct"],
    "properties": {
        "idea": {"type": "string", "description": "One-line name of the change"},
        "rationale": {
            "type": "string",
            "description": "Which measured cost this removes and why it is safe",
        },
        "files": {"type": "array", "items": {"type": "string"}},
        "instructions": {
            "type": "string",
            "description": "Precise implementation instructions for the engineer",
        },
        "expected_gain_pct": {"type": "number"},
    },
}

RULES = """Hard rules (a change that breaks one is thrown away):
- Edit only files under crates/han-timeline/src/. Never touch tests, examples, benches,
  conformance vectors, the spec, Cargo.toml or Cargo.lock, or anything outside that folder.
- Observable behaviour must not change: every conformance vector, the randomized convergence
  tests and the exact merged-text checks must still pass. Speed only.
- No new dependencies, no unsafe code, no change to the public API.
- This is a clean-room implementation (see CLAUDE.md): use only this repository; no web access,
  no other implementation of the profile, nothing from outside this folder.
- Keep the code idiomatic and commented like the surrounding code; `cargo fmt` and
  `cargo clippy -p han-timeline --all-targets -- -D warnings` must pass."""


def sh(cmd, cwd=REPO, input=None, timeout=7200, env=None):
    p = subprocess.run(
        cmd,
        cwd=cwd,
        input=input,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=timeout,
        env=env,
    )
    return p.returncode, p.stdout, p.stderr


def git(*args, cwd=WT):
    code, out, err = sh(["git", *args], cwd=cwd)
    if code != 0:
        raise SystemExit(f"git {' '.join(args)} failed: {err}")
    return out


def tool(name, native=None):
    if native and os.path.exists(native):
        return native
    path = shutil.which(name)
    if not path:
        raise SystemExit(f"{name} not found on PATH")
    return path


CLAUDE = tool("claude")
CODEX = tool(
    "codex",
    os.path.expandvars(r"%LOCALAPPDATA%\Programs\OpenAI\Codex\bin\codex.exe"),
)


def load():
    with open(STATE, encoding="utf-8") as f:
        return json.load(f)


def save(state):
    with open(STATE, "w", encoding="utf-8") as f:
        json.dump(state, f, indent=1)


def score(runs=5, gate=True):
    cmd = [sys.executable, os.path.join(HERE, "score.py"), "--repo", WT, "--runs", str(runs)]
    if not gate:
        cmd.append("--no-gate")
    code, out, err = sh(cmd)
    if code != 0:
        return {"error": (out + err)[-4000:]}
    return json.loads(out.strip().splitlines()[-1])


def breakdown():
    code, out, err = sh(
        ["cargo", "run", "-q", "--release", "-p", "han-timeline", "--example", "breakdown"],
        cwd=WT,
    )
    return out if code == 0 else err[-2000:]


def init(args):
    os.makedirs(WORK, exist_ok=True)
    if not os.path.exists(WT):
        start = git("rev-parse", "HEAD", cwd=REPO).strip()
        git("worktree", "add", "-B", BRANCH, WT, start, cwd=REPO)
    base = score(runs=args.runs, gate=True)
    if "sets" not in base:
        raise SystemExit(f"baseline does not pass the gate: {json.dumps(base)[:3000]}")
    again = score(runs=args.runs, gate=False)
    # Noise: how far two scorings of the same code drift on validation.
    v1, v2 = base["sets"]["validation"]["score"], again["sets"]["validation"]["score"]
    noise = abs(v1 - v2) / min(v1, v2)
    state = {
        "branch": BRANCH,
        "head": git("rev-parse", "HEAD").strip(),
        "baseline": base["sets"],
        "best": base["sets"],
        "noise": noise,
        "threshold": max(args.min_gain, 2 * noise),
        "rounds": [],
    }
    save(state)
    print(json.dumps({k: state[k] for k in ("noise", "threshold")}))
    for name, s in base["sets"].items():
        print(f"{name:<11} {s['score']:10.1f}")


def history_text(state, last=12):
    lines = []
    for r in state["rounds"][-last:]:
        lines.append(
            f"- round {r['round']}: {r.get('idea', '?')} -> {r['outcome']}"
            + (f" (validation {r['delta_pct']:+.1f}%)" if "delta_pct" in r else "")
        )
    return "\n".join(lines) or "(none yet)"


def plan(state, rdir):
    best = state["best"]["train"]["metrics"]
    metrics = "\n".join(f"  {k}: {v:.1f} us/op" for k, v in sorted(best.items()))
    prompt = f"""You are the performance planner for crates/han-timeline in this repository (the
han.timeline/1 engine). Your job: propose ONE change that makes it measurably faster. An engineer
(another model, with no context but your instructions and the repository) will implement it.

Current best timings on the training workload (per op, lower is better):
{metrics}

Per-stage breakdown of a local op (examples/breakdown.rs):
{breakdown()}

Earlier attempts (do not repeat failed or rejected ideas unless you fix why they failed):
{history_text(state)}

{RULES}

Read the source under crates/han-timeline/src/ as needed (read-only). Prefer changes with a large
expected gain and low risk to correctness. Signing and verifying (Ed25519, strict) are required
by the spec and cannot be removed. Give the engineer precise instructions: which functions,
which data structures, and how to keep behaviour identical. Answer with the JSON object only."""
    with open(os.path.join(rdir, "planner_prompt.txt"), "w", encoding="utf-8") as f:
        f.write(prompt)
    cmd = [
        CLAUDE,
        "-p",
        "--model",
        CLAUDE_MODEL,
        "--effort",
        "high",
        "--output-format",
        "json",
        "--json-schema",
        json.dumps(PLAN_SCHEMA),
        "--allowedTools",
        "Read",
        "Grep",
        "Glob",
        "--no-session-persistence",
    ]
    code, out, err = sh(cmd, cwd=WT, input=prompt)
    with open(os.path.join(rdir, "planner.json"), "w", encoding="utf-8") as f:
        f.write(out if out else err)
    if code != 0:
        raise RuntimeError(f"planner failed: {err[-2000:]}")
    reply = json.loads(out)
    answer = reply.get("structured_output")
    if answer is None:
        text = reply.get("result", "")
        answer = json.loads(text[text.find("{") : text.rfind("}") + 1])
    return answer


def code_change(p, rdir):
    prompt = f"""Implement this performance change in crates/han-timeline (Rust).

Change: {p['idea']}
Why: {p['rationale']}
Files: {', '.join(p['files'])}

Instructions:
{p['instructions']}

{RULES}

Before you finish, run `cargo fmt -p han-timeline` and
`cargo test --release -p han-timeline --test vectors --test random` and make sure both pass.
Do not commit. Summarise what you changed in two or three sentences."""
    cmd = [
        CODEX,
        "exec",
        "-m",
        CODEX_MODEL,
        "-s",
        "workspace-write",
        "-C",
        WT,
        "--ignore-user-config",
        "--json",
        "-o",
        os.path.join(rdir, "codex_last.txt"),
    ]
    code, out, err = sh(cmd, cwd=WT, input=prompt)
    with open(os.path.join(rdir, "codex.jsonl"), "w", encoding="utf-8") as f:
        f.write(out)
    with open(os.path.join(rdir, "codex_stderr.txt"), "w", encoding="utf-8") as f:
        f.write(err)
    return code == 0


def changed_paths():
    out = git("status", "--porcelain", "-uall")
    return [line[3:].strip().strip('"') for line in out.splitlines() if line.strip()]


def revert():
    git("reset", "--hard", "-q")
    git("clean", "-fdq")


def run(args):
    state = load()
    misses = 0
    for _ in range(args.rounds):
        n = len(state["rounds"]) + 1
        rdir = os.path.join(WORK, "rounds", f"{n:03d}")
        os.makedirs(rdir, exist_ok=True)
        rec = {"round": n, "started": time.strftime("%Y-%m-%dT%H:%M:%S")}
        try:
            p = plan(state, rdir)
            rec.update(idea=p["idea"], expected_gain_pct=p["expected_gain_pct"])
            print(f"round {n}: {p['idea']}")
            if not code_change(p, rdir):
                rec["note"] = "codex exited non-zero"
            paths = changed_paths()
            rec["files"] = paths
            if not paths:
                rec["outcome"] = "no change"
            elif any(not path.startswith(ALLOWED) for path in paths):
                rec["outcome"] = "refused: edited outside " + ALLOWED
            else:
                sh(["cargo", "fmt", "-p", "han-timeline"], cwd=WT)
                with open(os.path.join(rdir, "diff.patch"), "w", encoding="utf-8") as f:
                    f.write(git("diff"))
                s = score(runs=args.runs, gate=True)
                with open(os.path.join(rdir, "score.json"), "w", encoding="utf-8") as f:
                    json.dump(s, f, indent=1)
                if "sets" not in s:
                    rec["outcome"] = "gate failed: " + s.get("gate", {}).get("step", s.get("error", "?")[:200])
                else:
                    old = state["best"]["validation"]["score"]
                    new = s["sets"]["validation"]["score"]
                    delta = (old - new) / old
                    rec["delta_pct"] = -100 * delta
                    rec["scores"] = {k: v["score"] for k, v in s["sets"].items()}
                    if delta > state["threshold"]:
                        git("add", "-A")
                        msg = (
                            f"han-timeline: {p['idea']}\n\n{p['rationale']}\n\n"
                            f"Validation score {old:.1f} -> {new:.1f} us "
                            f"({-100 * delta:+.1f}%), hill-climb round {n}.\n\n"
                            "Clean-Room: B\n"
                            "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>\n"
                            f"Co-Authored-By: Codex ({CODEX_MODEL}) <noreply@openai.com>\n"
                        )
                        sh(["git", "commit", "-q", "-F", "-"], cwd=WT, input=msg)
                        state["best"] = s["sets"]
                        state["head"] = git("rev-parse", "HEAD").strip()
                        rec["outcome"] = "kept"
                    else:
                        rec["outcome"] = "no gain"
        except Exception as e:  # a failed round is recorded, not fatal
            rec["outcome"] = f"error: {e}"[:500]
        if rec["outcome"] != "kept":
            revert()
            misses += 1
        else:
            misses = 0
        rec["finished"] = time.strftime("%Y-%m-%dT%H:%M:%S")
        state["rounds"].append(rec)
        save(state)
        print(f"  -> {rec['outcome']}" + (f" ({rec['delta_pct']:+.1f}%)" if "delta_pct" in rec else ""))
        if misses >= args.patience:
            print(f"stopping: {misses} rounds in a row without a gain")
            break
    status(args)


def status(_args):
    state = load()
    print(f"noise {100 * state['noise']:.1f}%, keep threshold {100 * state['threshold']:.1f}%")
    for name in ("train", "validation", "test"):
        b, c = state["baseline"][name]["score"], state["best"][name]["score"]
        print(f"{name:<11} baseline {b:9.1f}  best {c:9.1f}  ({100 * (c - b) / b:+.1f}%)")
    for r in state["rounds"]:
        print(f"  {r['round']:>3} {r['outcome']:<40} {r.get('idea', '')}")


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    i = sub.add_parser("init")
    i.add_argument("--runs", type=int, default=5)
    i.add_argument("--min-gain", type=float, default=0.03)
    r = sub.add_parser("run")
    r.add_argument("--rounds", type=int, default=1)
    r.add_argument("--patience", type=int, default=3)
    r.add_argument("--runs", type=int, default=5)
    sub.add_parser("status")
    a = ap.parse_args()
    {"init": init, "run": run, "status": status}[a.cmd](a)


if __name__ == "__main__":
    main()

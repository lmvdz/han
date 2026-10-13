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

DIRECTION_SCHEMA = {
    "type": "object",
    "additionalProperties": False,
    "required": ["title", "idea", "hypothesis", "files", "expected_gain_pct"],
    "properties": {
        "title": {
            "type": "string",
            "description": "Commit subject, imperative, at most 60 characters",
        },
        "idea": {"type": "string", "description": "The target for this round, one paragraph"},
        "hypothesis": {
            "type": "string",
            "description": "Which measured cost it removes and why it keeps behaviour (estimate)",
        },
        "files": {"type": "array", "items": {"type": "string"}},
        "expected_gain_pct": {"type": "number"},
    },
}
CLAUDE_MODEL = os.environ.get("CLIMB_CLAUDE_MODEL", "sonnet")
# A fresh Codex session every few rounds keeps its context from growing without bound.
SESSION_ROUNDS = int(os.environ.get("CLIMB_SESSION_ROUNDS", "5"))

RULES = """Hard rules (a change that breaks one is thrown away):
- Edit only files under crates/han-timeline/src/. Never touch tests, examples, benches,
  conformance vectors, the spec, Cargo.toml or Cargo.lock, or anything outside that folder.
- Observable behaviour must not change: every conformance vector, the randomized convergence
  tests and the exact merged-text checks must still pass. Speed only.
- No new dependencies, no unsafe code, no change to the public API.
- A fast path that only runs for some sizes or shapes of data (large texts, no seals, ...) is
  invisible to the existing tests; add a unit test in the same src file that compares it with
  the general path on randomized inputs, or the change does not count as tested.
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


def notebook(state):
    """The fixed notes plus the current timings and every attempt: what both models read
    instead of re-exploring the crate."""
    with open(os.path.join(HERE, "notes.md"), encoding="utf-8") as f:
        fixed = f.read()
    best = state["best"]
    lines = [fixed.rstrip(), "", "## Current best timings (per op, train set)", ""]
    lines += [f"- {k}: {v:.1f} us" for k, v in sorted(best["train"]["metrics"].items())]
    lines += [
        "",
        "Set scores (geometric mean, us): "
        + ", ".join(f"{k} {v['score']:.1f}" for k, v in best.items()),
        "",
        "## Per-stage breakdown of a local op (examples/breakdown.rs)",
        "",
        "```",
        breakdown().strip(),
        "```",
        "",
        "## Attempts",
        "",
    ]
    for r in state["rounds"]:
        line = f"- round {r['round']} [{r['outcome']}] {r.get('title', r.get('idea', '?'))}"
        if "delta_pct" in r:
            line += f" (validation {r['delta_pct']:+.1f}%)"
        if r.get("codex_summary"):
            line += f": {r['codex_summary'][:400]}"
        lines.append(line)
    if not state["rounds"]:
        lines.append("(none yet)")
    return "\n".join(lines) + "\n"


def direct(state, notes, rdir):
    """One cheap Claude turn, no tools: pick the target for this round."""
    prompt = f"""{notes}

You direct a performance hill-climb of crates/han-timeline. Pick the single most promising
target for the next round from the notebook above: the timings, the breakdown, the ideas not
yet tried and the attempts (do not repeat a rejected idea unless you say what is different).
An engineer with the repository will investigate and implement it. You cannot read files or run
anything; your numbers are estimates. Signing and strict verification cannot be removed.
Answer with the JSON object only."""
    with open(os.path.join(rdir, "director_prompt.txt"), "w", encoding="utf-8") as f:
        f.write(prompt)
    cmd = [
        CLAUDE,
        "-p",
        "--model",
        CLAUDE_MODEL,
        "--effort",
        "medium",
        "--output-format",
        "json",
        "--json-schema",
        json.dumps(DIRECTION_SCHEMA),
        "--tools",
        "",
        "--strict-mcp-config",
        "--no-session-persistence",
    ]
    code, out, err = sh(cmd, cwd=WT, input=prompt)
    with open(os.path.join(rdir, "director.json"), "w", encoding="utf-8") as f:
        f.write(out if out else err)
    if code != 0:
        raise RuntimeError(f"director failed: {err[-2000:]}")
    reply = json.loads(out)
    answer = reply.get("structured_output")
    if answer is None:
        text = reply.get("result", "")
        answer = json.loads(text[text.find("{") : text.rfind("}") + 1])
    usage = reply.get("usage", {})
    answer["_usage"] = {
        "turns": reply.get("num_turns"),
        "notional_usd": reply.get("total_cost_usd"),
        "input": usage.get("input_tokens", 0),
        "cache_read": usage.get("cache_read_input_tokens", 0),
        "cache_write": usage.get("cache_creation_input_tokens", 0),
        "output": usage.get("output_tokens", 0),
    }
    return answer


def codex_round(state, notes, p, rdir):
    """Codex investigates and implements the target, in one session resumed across rounds."""
    target = f"""Target: {p['title']}
{p['idea']}
Director's hypothesis (an estimate, check it): {p['hypothesis']}
Start from: {', '.join(p['files'])}

Investigate the code paths for this target, implement the change, and check it: run
`cargo fmt -p han-timeline`, `cargo test --release -p han-timeline --test vectors --test random`
(both must pass) and `cargo run --release -p han-timeline --example bench 4000` to see the effect.
If it does not help, improve it or undo it. Do not commit. End with two or three sentences on
what you changed and what the benchmark showed."""
    session = state.get("codex_session")
    if session and state.get("codex_session_rounds", 0) < SESSION_ROUNDS:
        last = state["rounds"][-1] if state["rounds"] else None
        prev = (
            f"Round {last['round']} ({last.get('title', '?')}) was {last['outcome']}"
            + (" and committed." if last["outcome"] == "kept" else "; the tree was reset.")
            if last
            else ""
        )
        prompt = f"""New round. {prev} The working tree is now at commit {state['head'][:10]}.
Current set scores: {', '.join(f"{k} {v['score']:.1f} us" for k, v in state['best'].items())}.

{target}"""
        cmd = [CODEX, "exec", "resume", session, "-"]
    else:
        prompt = f"""{notes}

You implement speed-ups for crates/han-timeline, one target per round, in this repository.
The notebook above maps each benchmark to its code, lists the invariants and the attempts.

{RULES}

{target}"""
        cmd = [CODEX, "exec", "-C", WT, "-"]
        state["codex_session_rounds"] = 0
    cmd += [
        "-m",
        CODEX_MODEL,
        "--ignore-user-config",
        # The sandbox's restricted token cannot start app-alias executables (pwsh from
        # WindowsApps); without them on PATH Codex uses the system PowerShell.
        "-c",
        'windows.sandbox="unelevated"',
        "-c",
        'sandbox_mode="workspace-write"',
        "--json",
        "-o",
        os.path.join(rdir, "codex_last.txt"),
    ]
    with open(os.path.join(rdir, "codex_prompt.txt"), "w", encoding="utf-8") as f:
        f.write(prompt)
    env = dict(os.environ)
    env["PATH"] = os.pathsep.join(
        d for d in env.get("PATH", "").split(os.pathsep) if "windowsapps" not in d.lower()
    )
    code, out, err = sh(cmd, cwd=WT, input=prompt, env=env)
    with open(os.path.join(rdir, "codex.jsonl"), "w", encoding="utf-8") as f:
        f.write(out)
    with open(os.path.join(rdir, "codex_stderr.txt"), "w", encoding="utf-8") as f:
        f.write(err)
    usage = {}
    for line in out.splitlines():
        try:
            e = json.loads(line)
        except ValueError:
            continue
        if e.get("type") == "thread.started":
            state["codex_session"] = e["thread_id"]
        if e.get("type") == "turn.completed":
            for k, v in e.get("usage", {}).items():
                usage[k] = usage.get(k, 0) + v
    state["codex_session_rounds"] = state.get("codex_session_rounds", 0) + 1
    summary = ""
    last_path = os.path.join(rdir, "codex_last.txt")
    if os.path.exists(last_path):
        with open(last_path, encoding="utf-8", errors="replace") as f:
            summary = f.read().strip()
    return code == 0, usage, summary


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
            notes = notebook(state)
            with open(os.path.join(rdir, "notes.md"), "w", encoding="utf-8") as f:
                f.write(notes)
            p = direct(state, notes, rdir)
            rec.update(
                title=p["title"],
                idea=p["idea"],
                expected_gain_pct=p["expected_gain_pct"],
                claude=p["_usage"],
            )
            print(f"round {n}: {p['title']}", flush=True)
            ok, usage, summary = codex_round(state, notes, p, rdir)
            rec.update(codex=usage, codex_summary=summary)
            if not ok:
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
                        # Confirm a win with a second measurement before keeping it.
                        again = score(runs=args.runs, gate=False)
                        new2 = again["sets"]["validation"]["score"]
                        rec["confirm_pct"] = -100 * (old - new2) / old
                        if (old - new2) / old <= state["threshold"]:
                            delta = 0.0
                    if delta > state["threshold"]:
                        git("add", "-A")
                        # Only measured facts go into the history; the planner's reasoning
                        # (which is estimation) stays in the round folder.
                        lines = "\n".join(
                            f"- {k}: {state['best'][k]['score']:.1f} -> {v['score']:.1f} us "
                            f"({100 * (v['score'] - state['best'][k]['score']) / state['best'][k]['score']:+.1f}%)"
                            for k, v in s["sets"].items()
                        )
                        msg = (
                            f"han-timeline: {p['title']}\n\n{p['idea']}\n\n"
                            f"Hill-climb round {n}. Benchmark scores (geometric mean of per-op "
                            f"times, best of {args.runs} runs; the correctness gate passed):\n"
                            f"{lines}\n\n"
                            "Planned by Claude, implemented by Codex, measured by "
                            "crates/han-timeline/hillclimb/score.py.\n\n"
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

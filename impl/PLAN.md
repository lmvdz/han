# HAN v1: score plan and minimal reference implementation

## The goal, updated 2026-10-11

Keep iterating, graded by an independent agent against [grading-rubric.md](../grading-rubric.md), until the score is above 95, with simple interfaces and deep modules. Graders put the ceiling for design-only work near 80, so the work now runs on five tracks at once:

| # | Track | Lifts | Status |
|---|---|---|---|
| 1 | **Make it run**: the minimal reference below (sequencer, client, executor, host, verifier, CLI, demo) | C1, C4, C7; turns §10's procedure-only rows into tests | step 1 done (38 tests); step 2 next |
| 2 | **Clean-room second implementation**: Policy in TypeScript from the spec alone, run against the vectors; every disagreement is a spec gap (also v1 test 3, "built by strangers") | C5, C7 | running (`cleanroom-ts/`) |
| 3 | **Simplify guarded steering (P-15)**: two paper studies, capability-based and host+executor contract, compared by a grader before any change | C2, C3, C4 | running (`studies/`) |
| 4 | **Resource-bound verification**: rerun the timing model on current code; Matrix binding against a real Synapse | C7 | timing model running; Synapse blocked (no Docker on this machine) |
| 5 | **Use it daily**: the author runs real agent work through HAN (v1 test 1) | C1 | after tracks 1 and 3 |

The rubric stays fixed; a revision (e.g. the weight of v0.4 coverage after the scope change) would only happen with the user's explicit agreement, recorded in HISTORY.md.

---

# Minimal reference implementation

**Why.** Independent graders put the ceiling for design-only work at about 80/100: *goal fit* and *verifiability* are capped while nothing runs, and §10 lists rules whose only check is "a procedure, needs an implementation" (R-03, R-17/R-18, X-07, R-24, T-07/G-05, §8.4). This plan builds the smallest system that runs end to end, so those rules get executable checks and v1 test 1 ("used daily") becomes possible.

**Not in scope (v1 minimal):** the Matrix binding (stays specified, untested), federation between sequencers, moves and forks over the wire, a GUI. Everything speaks the **generic binding** (§8.3) over one WebSocket.

## Shape: five deep modules, one library

| Module | Process | Interface (what others call) | Hides |
|---|---|---|---|
| `han.core` | library | `policy` (the reference Policy, unchanged: `decide`, `fold`, `can_read`, `relevant_actors`, `session_access`, plus `guarded_now`), `wire` (ids, JCS, signatures, receipts, validation) | everything in ref/; the implementation **imports ref/policy.py and ref/wire.py**, so the conformance vectors test the running code |
| `han.seq` | `han-seq` server | WebSocket: `hello`, `append`, `read` (§3) + `subscribe`, `signal`, `blobPut`, `blobGet`, `stream`, `watch` (§8.3) | the §4.1 pipeline, SQLite log (R-07 durability `local`), steady clock (R-03a), dedupe (R-01), profile pinning (P-01), receipts, notices (G-03), blob store (G-01/02), session streams (G-05), R-16 restart rule |
| `han.client` | library | `Client(url, actor, key)`: `append(entry, content)` with outbox + retry (R-06/R-07), `read(...)`, `subscribe`, `signal`, `blob_put/get`, `stream`, `watch` | framing, auth (hello proof), reconnect, outbox in SQLite |
| `han.exec` | `han-exec` daemon | none; it is an Actor fed by `subscribe` | X-01…X-10: journal (SQLite, fsync), instance-key lock (X-07, OS file lock), claim/launch/outcome, elapsed-time deadline on the monotonic clock, no launch after restart; actions: `file.write` (local), `http.post` (to an allow-listed URL) |
| `han.host` | `han-host` | none; an Actor | P-15/R-24 host duties: `session.open`, `StreamItem`s from the agent's output, steer-in as turns, stricter mode while guarded/held, world actions → `effect.propose`, reading others' sessions as data. Two agent drivers behind one small interface: `ScriptedAgent` (deterministic, for tests) and `ClaudeCodeAgent` (Claude Agent SDK: `permission_mode`, `can_use_tool`, `setting_sources=[]`) |
| `han.verify` | `han-verify` | CLI: `han-verify follow <space>` → status per R-14 | mirror replay of Policy, chain and signature checks, equivocation/wrong-home proofs |
| `han.cli` | `han` | `han space create`, `invite`, `steer`, `approve`, `release`, `watch`, `task`, `propose` | argument parsing only |

Rule of depth: `han.seq` exposes the 3 calls + 6 transport ops and hides storage, clock, pinning and fan-out. `han.host` and `han.exec` expose nothing but behaviour.

## Data and process layout

```
impl/
  PLAN.md  pyproject.toml  README.md
  han/  core.py (re-exports ref/policy.py, ref/wire.py)  seq/  client/  exec/  host/  verify/  cli.py
  tests/   e2e scenarios (pytest), each naming the rule it retires from ✗
```
Keys: Ed25519 per actor, stored in `~/.han/keys/<actor>.json` (0600). One `han-seq` per server name; `localhost` names like `a.localhost` for tests, several servers on different ports.

## What each test retires (§10 ✗ → ✓)

| Rule | Executable check |
|---|---|
| R-03 / R-03a | restart `han-seq` with the wall clock stepped back; receipts stay monotone, and abandons wait a full `A` after restart (R-16) |
| R-06 / R-07 | kill the client between send and receipt; on restart the outbox re-sends and gets the same receipt (dedupe), never a second entry |
| R-17 / R-18 | crash `han-host` mid-task; after restart no intent is proposed twice and every entry's `seen`/`refs` name its inputs |
| X-02…X-07 | crash `han-exec` before/after `launching`; it never launches twice, reports `failed/not started` or `unknown`; a second process with the same instance key refuses to start |
| P-15 / R-24 | a member steers a guarded agent: the agent is held; a world action becomes a proposal that waits for the operator; the operator's approval lets the executor run it; text in a watched stream never acts as input |
| T-07 / G-05 | a watcher loses its role: the next `StreamItem` doesn't reach it; context items only reach readers |
| R-14 / R-15 | `han-verify` replays a live space and reports `ok`; a doctored sequencer that signs a forbidden entry is reported `faulty` with a portable proof |
| v1 test 1 | `demo/` script: two humans, two agents (one Claude Code), one executor, run a real task with steering, a hold, an approval and an outcome |

## Order of work

1. `han.core` + `han.seq` + `han.client` (the foundation; everything else needs them). Tests: the 30 wire scenarios re-run **over the network**, plus R-03/R-06/R-07.
2. In parallel, on top of 1: `han.exec` (X-tests), `han.host` with `ScriptedAgent` (P-15/R-24/T-07 tests), `han.verify` + `han.cli`.
3. `ClaudeCodeAgent` driver and the demo.
4. Fold results into §10 (✗ → ✓ with test names) and `run_all.py`.

## Dependencies

Python 3.11, `websockets`, `cryptography` (already used), `pytest`, `claude-agent-sdk` (only for `ClaudeCodeAgent`). Installed in `impl/.venv`, never globally.

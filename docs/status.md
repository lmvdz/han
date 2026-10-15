# Status

As of 2026-10-14. The plan, with every milestone's "done when", is in [PLAN.md §10](https://github.com/lmvdz/han/blob/main/PLAN.md#10-milestones).

## Milestones

| | Milestone | Status |
|---|---|---|
| M1 | `han-core`: Policy | **done** (#29). All 168 Policy vectors pass; 0 divergences from the reference under differential fuzzing |
| M2 | `han-seq` and `han-net` over QUIC (iroh) | **done** (#32). All 113 wire vectors pass over QUIC; the peer filter and tickets work |
| M3 | `han-node` | **done** (#35). Views, the node API, JSON-RPC over a named pipe or Unix socket, and a three-node simulation in CI |
| T0, T1 | `han-timeline` | **done** (#24, #25). Built by a two-team clean room from a behaviour-only spec |
| M4 | `han-vcs`, `han-exec`, `han-mcp`, `han-host` | **done** (#49). Harnesses over ACP: pi in API-key mode, and the official Claude Code client through `claude-agent-acp` on a subscription. Sandboxes: MXC (Windows) and bubblewrap (Linux). The egress proxy; worktree mirroring |
| M5 | `han tui` and `han-github` | **done** (#60; #50, #61). The board, session pane and approvals inbox; GitHub issues as tasks for a shared agent pool |
| M6 | Dogfood | **in progress** (below) |
| G | GUI | clean-room UI specs written ([GUI specification](ui/README.md)); not built |

## Since spec round 32

- The Rust server enforces the hello-to-node binding (SR-13, #47).
- A hosting rule: a home creates spaces only for actors it knows (#48).
- Federation: remote profiles are fetched and pinned (#52).
- An operator confirms an agent over `han/1` (#59).
- Joining a home by its ticket, with no HTTPS well-known (#62).
- Node API gaps closed (#63, #65, #67).
- CLI: `han space create|follow|list|member|policy`, `han device`, `han tui --print`, and `HAN_DATA` (#64, #66).
- Release tooling: cargo-dist, `han update --check`, the nightly cross-OS CI, pinned Actions, and Dependabot (#51).

## The specification

Round 32 graded 76.5 ([review history](decisions/HISTORY.md)). Spec rounds are paused while the product is built. Queued for the next round:
- semantic gates;
- risk tiers for landing work;
- the landing profile `han.land/1` ([study](https://github.com/lmvdz/han/blob/main/spec/decisions/studies/r33-land-profile.md));
- deterministic in-flight state and `han_query` ([study](https://github.com/lmvdz/han/blob/main/spec/decisions/studies/r33-inflight-state.md), [fact catalog](inflight-state.md));
- the open items of the round-32 review (AA1–AA5; the build already fixes AA2 and AA3).

## The dogfood

- **The runner.** A shared runner on a boat.dev VM hosts the home and the runner's node, set up as in the [shared runner guide](runner.md).
- **The space.** A solo dogfood space with two agents. `han-github` polls the HAN repository and surfaces its issues there.
- **Being fixed now:**
  - per-agent login directories;
  - starting the Claude Code driver on the runner;
  - a host per OS user;
  - an executor service;
  - a node logon service;
  - a task CLI.

M6 is done when the user and 1–3 more people build HAN with HAN for two weeks, and the verifier shows no action outside the sandbox without a valid approval.

## Not built yet

- the GUI (Tauri);
- agent-to-agent questions;
- semantic gates and risk tiers;
- two-way GitHub and chat notifiers.

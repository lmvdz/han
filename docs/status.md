# Status

As of 2026-10-20, checked against repository history through `aea9d40` (#174). The milestone acceptance criteria are in [PLAN.md §10](https://github.com/lmvdz/han/blob/main/PLAN.md#10-milestones). “Implemented” below describes merged code, not completion of the beta acceptance targets or a fresh live deployment check.

## Milestones

| | Milestone | Status |
|---|---|---|
| M1 | `han-core`: Policy | **merged** (#29). The milestone recorded 168 passing Policy vectors and zero differential-fuzz divergences; subsequent Policy changes are described below |
| M2 | `han-seq` and `han-net` over QUIC (iroh) | **merged** (#32). The milestone recorded 113 passing wire vectors; the current corpus contains 118. Peer filtering and home tickets are implemented |
| M3 | `han-node` | **merged** (#35). Views, the node API, JSON-RPC over a named pipe or Unix socket, and a three-node simulation |
| T0, T1 | `han-timeline` | **merged** (#24, #25). Built by a two-team clean room from a behaviour-only spec |
| M4 | `han-vcs`, `han-exec`, `han-mcp`, `han-host` | **merged** (#49). ACP harnesses, sandboxing, the egress proxy and worktree mirroring |
| M5 | `han tui` and `han-github` | **merged** (#60; #50, #61). The board, session pane and approvals inbox; GitHub issues as tasks for a shared agent pool |
| M6 | Dogfood / beta acceptance | **in progress; acceptance unverified** (below) |
| G | GUI | [Clean-room UI specs](https://github.com/lmvdz/han/blob/main/spec/ui/README.md) and generated node API types exist; the Tauri application is not built |

## Implemented since the initial dogfood

- **Runner setup and lifecycle.** Per-agent Claude login directories, managed pi/Claude ACP adapter setup, Linux hosts under separate OS users, executor service installation, node logon service installation and a task CLI landed in #70. Setup commands for homes, agents, executors and runners followed in #79, with a full dogfood-loop e2e test in #80. `han agent set` and `han doctor` are implemented (#91, #152). These are implementation and test evidence, not proof that every deployment or platform meets beta acceptance.
- **Recovery.** OS-user agent units are re-adopted and killed hosts end their sessions (#126). A stopped session does not imply its work has landed; the host reports remaining work, and pruned session ledgers are not inherited (#168).
- **Agent tools.** `team_sessions`, `team_who_is_on`, `team_read` and `team_overlaps` expose bounded team context as data. `propose` requests external effects through the executor. Landing proposals, `land_status` and check-only `land_try` are implemented (#128, #161).
- **Landing.** `han-exec` implements `vcs.land`, a compare-and-swap landing action (#122), and a per-repository/ref lane through pull requests (#123). The host's land finish flow freezes proposed work, follows lane results and prunes eligible workspaces (#128); setup and doctor checks followed (#136, #140). A warm-runner checks backend is implemented (#138). A check-only try tests without landing (#160, #161). Rebased proposals use the target the agent rebased onto (#166).
- **Semantic gates and risk tiers.** Policy implements P-17 (#137), with Python and TypeScript witness support (#143). The independent tier-1 machine reviewer has a library and a `han review` service (#124, #156); it is off by default. These features are no longer merely queued designs.
- **Post-merge recovery.** The optional lane auto-revert flow is implemented (#148, #153, #157, #159, #162), including re-running a failing full check before reverting, escalation for flaky results, fix tasks and human approval where required. Auto-revert is off by default.
- **GitHub intake.** `han-github` polls issues into pool tasks; task recreation and service installation are implemented (#126). This does not establish two-way GitHub synchronization or chat notifications.
- **Security and operations.** The build includes hello-to-node binding, known-actor hosting, pinned remote profiles and operator confirmation (#47, #48, #52, #59). Role-specific RPC endpoints prevent hosts and executors from approving (#90); human-presence checks protect face governing and steering (#114). Release tooling and cross-OS CI exist (#51), with later CI and platform fixes through #165.

The ACP harnesses include **pi** (API-key mode) and **Claude Code** through `claude-agent-acp` (subscription login), with [declarative ACP drivers](../crates/han-host/drivers/README.md) supporting shared setup, login, diagnostics and launch (#174), including a pinned **Codex** adapter with isolated subscription login. The host confines agents with MXC on Windows or bubblewrap on Linux. The verified Codex run completed a real documentation task through manual approval, executor checks and [landing PR #173](https://github.com/lmvdz/han/pull/173) (commit `96da3fa`), using Linux bubblewrap and a ChatGPT subscription. That trial does not establish Windows Codex, fresh onboarding, steering, quota exhaustion or interrupted-turn recovery. Beta acceptance and the two-week multi-person acceptance target remain unverified.

The TUI's `:setup` creates a Trusted team space or joins by invite, with a permission preview and persisted recovery notice for uncertain submissions. `han space create-trusted` previews the same repository-scoped policy offline; `--apply` creates it atomically. Ordinary changes can land automatically after the configured executor checks; sensitive paths, larger changes and other effects require human owner approval. Tests cover the actual home policy, repository/risk boundaries, offline creation and setup recovery. `han setup` creates a local home, registers a human profile, starts a managed person node and opens this form. It resumes the saved identity and stops both roles without removing state. A production-role integration test covers fresh startup, resume, normal presence requirements, stop and restart. Installation, joining a remote home, agent login and runner configuration still require the steps in [getting started](getting-started.md). No timed fresh-user acceptance result is claimed.

## The specification

Round 32 remains the last graded round: **76.5** ([review history](https://github.com/lmvdz/han/blob/main/spec/decisions/HISTORY.md)). Changes labelled “round 33, ahead of the round” have nevertheless merged, including executor cancellation (P-07a), semantic gates/risk tiers (P-17), post-merge revert (A2) and check-only tries. Merged changes do not constitute a completed independent round-33 review.

The broader [landing profile study](https://github.com/lmvdz/han/blob/main/spec/decisions/studies/r33-land-profile.md) contains designs beyond the implemented lane; it is not an acceptance report. Likewise, deterministic in-flight state and `han_query` remain a [study](https://github.com/lmvdz/han/blob/main/spec/decisions/studies/r33-inflight-state.md) and [fact catalog](inflight-state.md), not an implemented replacement for the current team tools.

## Dogfood and beta acceptance

The initial documented deployment was a shared boat.dev runner with a solo space and two agents, with `han-github` polling the HAN repository. The [shared runner guide](runner.md) describes setup. Repository code and history verify the fixes above; they do not verify the current health, participant count or elapsed usage of that live deployment.

**M6 remains open.** The acceptance target is the user **plus 1–3 more people**, building HAN with HAN for **two weeks of real work**, with the verifier showing **no action outside the sandbox without a valid approval**. A solo run, multiple agents, merged features or passing automated tests do not establish that target. This checkout contains no verified completion report for that multi-person period and verifier result.

The milestone merge labels above must not be read as a claim that all beta acceptance criteria, live harness/platform combinations or GUI acceptance criteria have been demonstrated.

## Still outstanding

- The Tauri GUI and verification of its UI acceptance criteria.
- Agent-to-agent questions.
- Two-way GitHub synchronization and chat notifiers.
- The broader in-flight query and landing designs beyond the implementations described above.
- Verification of the multi-person, two-week dogfood and its authorization audit.

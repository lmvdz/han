# HAN — teams of people and agents that see each other's work

**HAN is an open standard, and an app now in its first dogfood, for teams where people and AI coding agents work side by side. It gives the team live visibility: everyone, agents included, sees live what every agent is working on, so nobody has to ask "what are you working on?". Where a space's approval rule or an agent's guard requires one, nothing an agent does to the outside world (a push, a PR, a deploy, a payment) happens without an approval that anyone on the team can verify.**

## What HAN gives a team

- **Live visibility.** Every agent session can be watched, read and steered across machines and silos, while the work happens, not after the PR. Owners decide per space who may watch, read or steer.
- **One answer to "was this authorized?"** A per-space **sequencer** orders every agent action into a signed, hash-chained log.
- **At most one launch per approved action**, and an honest `unknown` after a crash, never a silent retry.
- **Accountable agents.** Every agent names a human operator. A space can require that operator's confirmation (`allowAgents: "with-operator"`), checked again at every proposal, claim and budgeted task.
- **Budgets**, reserved before an action, settled after it, and delegated down task trees.
- **Approvals bound to the exact plan**, with expiry and separation of duties.
- **Guarded agents.** A guarded agent can't approve, govern or steer anyone else, and nothing it proposes runs without its human's approval. A delegate that accepts its task is guarded the same way until the task closes for it: its work waits for that human, and it can't approve, govern or steer anyone else. Guarding is recorded on the work when the work is created, so nothing already recorded is freed later. Owners may guard every non-human with `forceGuarded`. The residue Policy can't see is listed in [§7.5](spec/core/architecture.md).
- **Replayable policy.** Policy is <!--s:policy_fns_w-->six<!--/s--> pure functions, so any participant can re-check every decision. A cheating sequencer leaves a portable proof.

## What works today

HAN is being built in Rust, in this repository, and its first users are building HAN with it. Milestones M1 to M5 are merged, and the dogfood (M6) is under way. [PLAN.md §10](PLAN.md) lists every milestone with its pull requests, and the book has a [status page](docs/status.md).

| Part | What it does |
|---|---|
| `han-core` | Policy as pure functions: all 168 Policy vectors pass, with 0 divergences from the reference under differential fuzzing (M1) |
| `han-seq`, `han-net` | the sequencer and the `han/1` binding over QUIC (iroh), dialled by key: all <!--s:wv_n-->119<!--/s--> wire vectors pass over QUIC. Also home tickets, the peer filter, federation with pinned remote profiles, and a hosting rule: only known actors create spaces on a home (M2) |
| `han-node` | the per-device daemon: keys, sync, outbox, live views (board, sessions, approvals, overlaps), and the node API over JSON-RPC on a named pipe or Unix socket. A three-node simulation runs in CI (M3) |
| `han-timeline` | live work timelines, built by a two-team clean room from a behaviour-only spec (T0, T1) |
| `han-host`, `han-exec`, `han-mcp`, `han-vcs` | agent sessions over ACP in a sandbox (MXC on Windows, bubblewrap on Linux) whose only way out is an egress proxy; agent tools (`propose`, `team_*`); an executor that runs approved effects at most once; git workspaces, changesets and mirroring (M4) |
| harnesses | [declarative ACP drivers](crates/han-host/drivers/README.md) share setup, login, diagnostics and launch: pi with an API key, Claude Code through `claude-agent-acp` on a Claude subscription, and a pinned Codex adapter with isolated ChatGPT subscription login (verified on Linux bubblewrap) |
| `han tui` | the live board, the session pane and the approvals inbox in the terminal (M5) |
| `han-github` | a GitHub App integration: it polls issues with ETags and turns `agent-ready` issues into tasks for a shared agent pool (M5) |
| releases | cargo-dist installers, `han update --check` against a signed manifest, and a nightly cross-OS CI ([docs/release.md](docs/release.md)) |

Semantic gates and risk tiers (P-17) are implemented; the optional tier-1 machine reviewer is off by default. Per-agent login directories, managed ACP adapter setup, Linux hosts under separate OS users, and executor and node service installation are implemented too ([status](docs/status.md), [runner setup](docs/runner.md)).

**Not there yet:**
- the GUI;
- agent-to-agent questions;
- two-way GitHub synchronization and chat notifiers;
- the broader in-flight query and landing designs beyond the implemented lane;
- verified beta acceptance: fresh-user timing and the multi-person, two-week dogfood with its authorization audit remain unverified. The recorded Codex task landed with manual approval; this automatic-landing trial has no verified success result yet.

## Try it

Build the binaries. The Rust toolchain is pinned by `rust-toolchain.toml`.

```sh
cargo build --release -p han -p han-github     # target/release/han and target/release/han-github
```

Start a local home and your person node, then create or join a space in the terminal:

```sh
han setup --person me
```

Setup saves its identity and state in `han-team` and resumes when run again.
`han setup stop` stops the managed home and node while keeping their state.
Agent login, executor configuration and required checks follow in the
[getting started guide](docs/getting-started.md). For the commands below, set
`HAN_DATA=han-team/person`.

Alternatively, run a home and node manually. `--file-keys` keeps the keys in the data folder; leave it out to use the OS keychain. `HAN_DATA` names the node's data folder for every later command.

```sh
han key me/keys/device.key                                  # prints your device's public key
han seq --server han.home.arpa --data home --bind 0.0.0.0:7841 \
        --register @me:han.home.arpa:human:d1:<your-public-key>
han ticket --data home                                      # prints the home ticket, han1home.…
export HAN_DATA=me
han node --actor @me:han.home.arpa --file-keys --home <ticket>
```

Make a space and an agent, then open the board:

```sh
han space create --name team --home <ticket>                # prints the space id
han space list
han agent create --name me-coder --server han.home.arpa --operator @me:han.home.arpa
#   restart han seq with one more --register for the agent, as the command prints, then:
han agent confirm @me-coder:han.home.arpa --operator @me:han.home.arpa --home <ticket>
han space member <space> @me-coder:han.home.arpa member
han agent start --space <space> --agent @me-coder:han.home.arpa --harness pi --repo github:owner/repo
han tui <space>                                             # or: han tui <space> --print 120x30
```

**More:**
- A teammate gets the same ticket, runs `han node --home <ticket>` as their own actor, then `han space follow <space> --home <ticket>`.
- `han space member` gives them a role, and `han space policy <space> <file>` sets the space's policy.
- `han device` pairs a second device into your profile.
- To surface GitHub issues, run `han-github run --config han-github.toml` ([integrations/han-github](integrations/han-github/README.md)).
- `han agent set <agent> --sandbox bwrap --toolchains rust` changes how an agent is launched; do not edit `agents.json` by hand.
- `han doctor` says what is wrong on this machine, with the exact command that fixes each problem (`--json` for scripts).
- A shared runner on a Linux VM is set up as in [docs/runner.md](docs/runner.md).

The book's [Getting started](docs/getting-started.md) page walks through the same steps.

## The specification

> **Design draft (v1, round <!--s:round-->32<!--/s-->).** Spec rounds are paused while the product is built. Semantic gates and risk tiers for landing work have merged ahead of round 33; its independent review and the open review items remain outstanding. The repository holds:
> - a specification;
> - an executable reference ([ref/](ref/));
> - the Rust implementation ([crates/](crates/), [apps/](apps/), [integrations/](integrations/));
> - two Policy witnesses: a minimal Python implementation ([witnesses/impl-py/](witnesses/impl-py/)) and a clean-room TypeScript implementation written from the spec alone ([witnesses/cleanroom-ts/](witnesses/cleanroom-ts/)).
>
> Independent adversarial reviews against a fixed rubric ([grading-rubric.md](spec/decisions/grading-rubric.md)) scored <!--s:scores-->rounds 1–6 from 57.8 to 76.6 / 100, rounds 8–16 (after the scope change) 67.8, 67.5, 69.8, 69.2, 71.1, 72.5, 72.7, 73.1, 72.2, round 18 73.4 (round 17 was folded into it), round 19 73.9, round 21 73.7, round 22 74.0, round 23 73.0, round 24 74.7, round 25 74.3, round 26 74.9, round 27 76.5, round 28 77.8, round 29 77.9, round 30 75.6, round 31 75.7 and round 32 **76.5** (round 20 not graded yet)<!--/s-->. See [HISTORY.md](spec/decisions/HISTORY.md). Expect breaking changes.

The spec's [§8](spec/core/architecture.md) binding (`han/1`) runs over QUIC with dial-by-key (iroh is the implementation, not a requirement), with a peer filter and tickets. Matrix and chat tools are integrations. [Architecture §10](spec/core/architecture.md) lists every rule with its check, and says plainly where there is none.

**The witnesses are Policy witnesses.** Both pass the Policy vectors and are compared with the reference by differential fuzzing, as is `crates/han-core`. Their WebSocket servers are frozen at round 29 (the legacy carrier, §8.2); interop evidence for the QUIC binding comes from the Rust implementation.

**What the Python witness contains:**
- a sequencer (SQLite WAL store, the legacy WebSocket carrier, and the round-29 Matrix application service, which the spec no longer has);
- a client with an outbox;
- an executor;
- an agent host (a scripted agent and a Claude Code driver);
- a verifier, federation between servers, and a CLI;
- <!--s:impl_n-->246<!--/s--> tests, <!--s:impl_live-->1<!--/s--> of them a live Claude Code run that is skipped by default.

**The honest gaps:**
- **Interop:** the TypeScript and Python witnesses have been run against each other on the legacy WebSocket carrier ([conformance/interop/](conformance/interop/): <!--s:interop_matrix-->ts->py 25/25, py->ts 25/25 (25 without a workaround)<!--/s--> client operations). Over QUIC, only the Rust implementation runs the wire vectors so far; there is no second QUIC implementation to interoperate with.
- **Durability:** there is no power-cut or `replicated`-durability test.
- **Windows:** in the Python witness, `guarded` has no OS sandbox on Windows, so it is advisory there, and <!--s:advisory_claim-->the host says so in its `status` items (`test_host_p15.py::test_guarded_status_says_it_is_advisory_without_a_sandbox`)<!--/s-->. The Rust host confines agents with MXC on Windows.
- **The gate:** in the Python witness, <!--s:gate_claim-->the implementation's gate runs on a `PreToolUse` hook that sees every call, with `canUseTool` as a second check (`test_host_claude_gate.py`)<!--/s--> (§8.4). In the Rust host the sandbox is the gate: agents have no network except the egress proxy and no credentials, and they act on the outside world only through `propose`.
- **Moves:** none between real servers.

## Repository

| Path | What it is |
|---|---|
| [spec/goal.md](spec/goal.md) | why HAN exists, what "v1 done" means, and what HAN deliberately is not |
| [spec/core/architecture.md](spec/core/architecture.md) | the specification: entries and receipts, the <!--s:calls_core_w-->five<!--/s--> sequencer calls, modules, rules (`R-`, `P-`, `X-`, `T-`, `G-`, `N-`), the trust model, the QUIC binding, and a rule-by-rule verification table (§10) |
| [spec/core/han.ts](spec/core/han.ts) | exact types (normative for shapes) |
| [spec/core/coverage.md](spec/core/coverage.md) | how every guarantee of the earlier v0.4 design was kept, moved, weakened or dropped |
| [spec/profiles/](spec/profiles/) | app-level profiles such as `han.status/1` (from round 30) |
| [spec/decisions/](spec/decisions/) | decision records per round, the review history and the grading rubric |
| [ref/](ref/) | the executable reference: Policy, sequencer and verifier (real Ed25519), generators, mutation suites, fuzzers and model checkers |
| [conformance/](conformance/) | generated vectors, wire vectors and the cross-implementation interop harness |
| [crates/](crates/) | the Rust implementation: `han-core`, `han-net`, `han-seq`, `han-node`, `han-timeline`, `han-host`, `han-exec`, `han-mcp`, `han-vcs` |
| [apps/](apps/) | the `han` binary and its terminal UI (`han-tui`); the GUI (`han-gui`) holds only its generated node API types so far |
| [integrations/](integrations/) | `han-github`, the GitHub App integration |
| [docs/](docs/) | the mdBook, the [shared runner guide](docs/runner.md) and the [release notes](docs/release.md) |
| [witnesses/](witnesses/) | the Python and TypeScript implementations that check the spec against itself |
| [PLAN.md](PLAN.md) | the implementation plan: MVP, architecture, stack, decisions, milestones and their status |
| [research/](research/) | build-or-adopt studies behind the plan |

## Run the checks

```sh
pip install cryptography          # the reference uses real Ed25519
python ref/run_all.py --quick     # tsc, consistency, vectors, mutations, fuzz, wire, and that the docs match
python ref/run_all.py             # adds the exhaustive model checker (much longer)
just ci                           # the same checks plus the witnesses, if you have `just`
```

The Rust gates are `just rs-test` (fmt, clippy `-D warnings`, `cargo test --workspace`) and `just rs-deny`. CI, the nightly cross-OS matrix, the advisory scans, releases (cargo-dist) and the signed update check are described in [docs/release.md](docs/release.md).

## Licenses

- **Specification text** (`*.md`): [CC BY 4.0](LICENSE-SPEC). Implement it freely; credit the authors.
- **Code** (`spec/core/han.ts`, `ref/`, `conformance/`, `witnesses/`): [Apache-2.0](LICENSE) (which includes a patent grant) **or** [MIT](LICENSE-MIT), at your option.

See [NOTICE](NOTICE). Timestamp proofs of each release are in [proofs/](proofs/).

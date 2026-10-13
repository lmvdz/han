# HAN — teams of people and agents that see each other's work

**HAN is an open standard, and soon an app, for teams where people and AI coding agents work side by side. Everyone, agents included, sees live what every agent is working on, so nobody has to ask "what are you working on?". Where a space's approval rule or an agent's guard requires one, nothing an agent does to the outside world (a push, a PR, a deploy, a payment) happens without an approval that anyone on the team can verify.**

## What HAN gives a team

- **Live visibility.** Every agent session can be watched, read and steered across machines and silos, while the work happens, not after the PR. Owners decide per space who may watch, read or steer.
- **One answer to "was this authorized?"** A per-space **sequencer** orders every agent action into a signed, hash-chained log.
- **At most one launch per approved action**, and an honest `unknown` after a crash, never a silent retry.
- **Accountable agents.** Every agent names a human operator. A space can require that operator's confirmation (`allowAgents: "with-operator"`), checked again at every proposal, claim and budgeted task.
- **Budgets**, reserved before an action, settled after it, and delegated down task trees.
- **Approvals bound to the exact plan**, with expiry and separation of duties.
- **Guarded agents.** A guarded agent can't approve, govern or steer anyone else, and nothing it proposes runs without its human's approval. A delegate that accepts its task is bound the same way until the task closes: its work waits for that human, and it can't approve, govern or steer anyone else. Guarding is recorded on the work when the work is created, so nothing already recorded is freed later. Owners may guard every non-human with `forceGuarded`. The residue Policy can't see is listed in [§7.5](spec/core/architecture.md).
- **Replayable policy.** Policy is <!--s:policy_fns_w-->six<!--/s--> pure functions, so any participant can re-check every decision. A cheating sequencer leaves a portable proof.

## Where it's going

The specification is mature. The next step is a product, planned in [PLAN.md](PLAN.md):

- **The MVP: team awareness.** 2–4 people, each running several coding agents. A live board shows who is on which GitHub issue and what each agent is doing. You can drill into any session's live work timeline. Agents query each other's sessions, and overlap alerts fire before two agents collide.
- **No harness lock-in.** Agents run through ACP (Agent Client Protocol), with pi first; any ACP agent should work.
- **The sandbox is the gate.** Agents get no network and no credentials. Their only way out is HAN's `propose`, and a separate executor acts only on valid approvals.
- **One protocol over QUIC**, dialled by key (iroh is the planned implementation, not a requirement), so teammates' machines and cloud runners reach each other without port forwarding; spaces still have a home server name. Matrix and chat tools become integrations. Since spec round 30, [§8](spec/core/architecture.md) specifies this binding (`han/1`), with a peer filter and invite tickets; Matrix is an informative integration.
- **A Rust implementation:** one `han` binary with a terminal UI first, and a Tauri desktop app after.

## Status

> **Design draft (v1, round <!--s:round-->31<!--/s-->).** The repository holds:
> - a specification;
> - an executable reference ([ref/](ref/));
> - a working but minimal Python implementation ([witnesses/impl-py/](witnesses/impl-py/));
> - a clean-room TypeScript implementation written from the spec alone ([witnesses/cleanroom-ts/](witnesses/cleanroom-ts/)).
>
> There is no production implementation yet. Independent adversarial reviews against a fixed rubric ([grading-rubric.md](spec/decisions/grading-rubric.md)) scored <!--s:scores-->rounds 1–6 from 57.8 to 76.6 / 100, rounds 8–16 (after the scope change) 67.8, 67.5, 69.8, 69.2, 71.1, 72.5, 72.7, 73.1, 72.2, round 18 73.4 (round 17 was folded into it), round 19 73.9, round 21 73.7, round 22 74.0, round 23 73.0, round 24 74.7, round 25 74.3, round 26 74.9, round 27 76.5, round 28 77.8, round 29 77.9 and round 30 **75.6** (round 20 not graded yet)<!--/s-->. See [HISTORY.md](spec/decisions/HISTORY.md). Expect breaking changes.

**The witnesses are Policy witnesses.** Both implementations pass the Policy vectors and are compared with the reference by differential fuzzing. Their WebSocket servers are frozen at round 29 (the legacy carrier, §8.2) and leave the live interop; interop evidence for the QUIC binding comes with the Rust implementation.

**What the Python witness contains:**
- a sequencer (SQLite WAL store, the legacy WebSocket carrier, and the round-29 Matrix application service, which the spec no longer has);
- a client with an outbox;
- an executor;
- an agent host (a scripted agent and a Claude Code driver);
- a verifier, federation between servers, and a CLI;
- <!--s:impl_n-->241<!--/s--> tests, <!--s:impl_live-->1<!--/s--> of them a live Claude Code run that is skipped by default.

**The honest gaps:**
- **Interop:** the TypeScript and Python implementations have been run against each other ([conformance/interop/](conformance/interop/): <!--s:interop_matrix-->ts->py 25/25, py->ts 25/25 (25 without a workaround)<!--/s--> client operations), and both servers pass all <!--s:wv_n-->113<!--/s--> wire vectors, all on the legacy WebSocket carrier. Nothing implements the QUIC binding yet.
- **Durability:** there is no power-cut or `replicated`-durability test.
- **Windows:** `guarded` has no OS sandbox on Windows, so it is advisory there, and <!--s:advisory_claim-->the host says so in its `status` items (`test_host_p15.py::test_guarded_status_says_it_is_advisory_without_a_sandbox`)<!--/s-->.
- **The gate:** <!--s:gate_claim-->the implementation's gate runs on a `PreToolUse` hook that sees every call, with `canUseTool` as a second check (`test_host_claude_gate.py`)<!--/s--> (§8.4).
- **Moves:** none between real servers.

[Architecture §10](spec/core/architecture.md) lists every rule with its check, and says plainly where there is none.

## Repository

| Path | What it is |
|---|---|
| [spec/goal.md](spec/goal.md) | why HAN exists, what "v1 done" means, and what HAN deliberately is not |
| [spec/core/architecture.md](spec/core/architecture.md) | the specification: entries and receipts, the <!--s:calls_core_w-->four<!--/s--> sequencer calls, modules, rules (`R-`, `P-`, `X-`, `T-`, `G-`, `N-`), the trust model, the QUIC binding, and a rule-by-rule verification table (§10) |
| [spec/core/han.ts](spec/core/han.ts) | exact types (normative for shapes) |
| [spec/core/coverage.md](spec/core/coverage.md) | how every guarantee of the earlier v0.4 design was kept, moved, weakened or dropped |
| [spec/profiles/](spec/profiles/) | app-level profiles such as `han.status/1` (from round 30) |
| [spec/decisions/](spec/decisions/) | decision records per round, the review history and the grading rubric |
| [ref/](ref/) | the executable reference: Policy, sequencer and verifier (real Ed25519), generators, mutation suites, fuzzers and model checkers |
| [conformance/](conformance/) | generated vectors, wire vectors and the cross-implementation interop harness |
| [witnesses/](witnesses/) | the Python and TypeScript implementations that check the spec against itself |
| [PLAN.md](PLAN.md) | the implementation plan: MVP, architecture, stack, milestones |
| [research/](research/) | build-or-adopt studies behind the plan |

## Run the checks

```sh
pip install cryptography          # the reference uses real Ed25519
python ref/run_all.py --quick     # tsc, consistency, vectors, mutations, fuzz, wire, and that the docs match
python ref/run_all.py             # adds the exhaustive model checker (much longer)
just ci                           # the same checks plus the witnesses, if you have `just`
```

## Licenses

- **Specification text** (`*.md`): [CC BY 4.0](LICENSE-SPEC). Implement it freely; credit the authors.
- **Code** (`spec/core/han.ts`, `ref/`, `conformance/`, `witnesses/`): [Apache-2.0](LICENSE) (which includes a patent grant) **or** [MIT](LICENSE-MIT), at your option.

See [NOTICE](NOTICE). Timestamp proofs of each release are in [proofs/](proofs/).

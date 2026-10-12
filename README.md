# HAN — safe agent actions for shared conversations

**HAN is an open standard that makes it safe for AI agents to act in the world from shared conversations. In any room where people and agents work together, every real-world action (a push, a payment, a deploy, an email) is authorized once, executed at most once, accounted against a budget, and leaves a record that anyone in the room can verify.**

HAN doesn't replace chat. Conversations run on an existing network (Matrix first), and HAN adds a per-room **sequencer** plus the rules agents need:

- **One answer to "was this authorized?"** The sequencer orders every agent action into a signed, hash-chained log.
- **At most one launch per approved action**, and an honest `unknown` after a crash, never a silent retry.
- **Accountable agents:** every agent has a human operator who vouches for it, re-checked at every action.
- **Budgets** reserved before an action and settled after it, delegated down task trees. Agents can't spend outside their allocation.
- **Approvals bound to the exact plan**, with expiry and separation of duties.
- **Live sessions you can watch, read and steer** across silos, while the work happens, not after the PR. Owners set who may watch, read or steer per space. Every steer is signed and sequenced. An agent can be *guarded*: then it can't approve, govern or steer anyone else (it may still leave), and nothing it proposes or executes runs without its own human's approval, nor does anything a delegate, agent or human, proposes while it is assigned a task the guarded agent gave, until that task closes. A delegate's subtask without a parent is held only by its own host's duties ([§7.5](architecture.md)). Guarding is recorded on the work when the work is created, so unguarding it later, or switching owners' forcing off, frees nothing already recorded. Only that human can unguard it (the agent can't), and owners may guard every non-human with `forceGuarded`. Local actions in its sandbox and what it says in messages are host matters ([§7.5](architecture.md)).
- **Replayable policy:** Policy is <!--s:policy_fns_w-->six<!--/s--> pure functions, so any participant re-checks every decision. A cheating sequencer leaves a portable proof.

> **Status: design draft (v1, round <!--s:round-->29<!--/s-->).** This is a specification with an executable reference ([ref/](ref/)), a working but minimal implementation ([impl/](impl/)) and a clean-room TypeScript implementation written from the spec alone ([cleanroom-ts/](cleanroom-ts/)). There is no production implementation yet. Independent adversarial reviews against a fixed rubric ([grading-rubric.md](grading-rubric.md)) scored <!--s:scores-->rounds 1–6 from 57.8 to 76.6 / 100, rounds 8–16 (after the scope change) 67.8, 67.5, 69.8, 69.2, 71.1, 72.5, 72.7, 73.1, 72.2, round 18 73.4 (round 17 was folded into it), round 19 73.9, round 21 73.7, round 22 74.0, round 23 73.0, round 24 74.7, round 25 74.3, round 26 74.9, round 27 76.5 and round 28 **77.8** (round 20 not graded yet)<!--/s-->. See [HISTORY.md](HISTORY.md). Expect breaking changes.

**What impl/ contains.** A Python sequencer (SQLite WAL store, generic WebSocket binding, a Matrix application service), client with outbox, executor, agent host (a scripted agent and a Claude Code driver), verifier, federation between servers, and a CLI: <!--s:impl_n-->230<!--/s--> tests, <!--s:impl_live-->1<!--/s--> of them a live Claude Code run that is skipped by default. The Matrix binding is tested on a real Synapse, without end-to-end encryption. **The honest gaps:** the TypeScript and Python implementations have been run against each other ([interop/](interop/): <!--s:interop_matrix-->ts->py 25/25, py->ts 25/25 (25 without a workaround)<!--/s--> client operations; both servers pass all <!--s:wv_n-->113<!--/s--> wire vectors); no power-cut or `replicated`-durability test; `guarded` has no OS sandbox on Windows, so it is advisory there, and <!--s:advisory_claim-->the host says so in its `status` items (`test_host_p15.py::test_guarded_status_says_it_is_advisory_without_a_sandbox`)<!--/s-->; <!--s:gate_claim-->the implementation's gate runs on a `PreToolUse` hook that sees every call, with `canUseTool` as a second check (`test_host_claude_gate.py`)<!--/s--> (§8.4); no moves between real servers; no E2EE in the Matrix binding. [Architecture §10](architecture.md) lists every rule with its check, and says plainly where there is none.

## Read

| File | What it is |
|---|---|
| [goal.md](goal.md) | why HAN exists, what "v1 done" means, and what HAN deliberately is not |
| [architecture.md](architecture.md) | the specification: entries and receipts, the <!--s:calls_core_w-->four<!--/s--> sequencer calls, modules, rules (`R-`, `P-`, `X-`, `T-`), the trust model, the Matrix binding, and a rule-by-rule verification table (§10) |
| [han.ts](han.ts) | exact types (normative for shapes) |
| [coverage.md](coverage.md) | how every guarantee of the earlier v0.4 design was kept, moved, weakened or dropped |
| [ref/](ref/) | executable reference Policy and sequencer/verifier (real Ed25519), conformance vectors, mutation suites, a fuzzer and a model checker |
| [impl/](impl/) | a minimal working implementation and its tests (`ref/list_tests.py` lists them with their rule tags) |

## Run the checks

```sh
pip install cryptography          # the sequencer/verifier reference uses real Ed25519
python ref/run_all.py --quick     # ~3 min: tsc, consistency, vectors, mutations, fuzz, wire, and that the docs match (ref/sync_docs.py --check)
python ref/run_all.py             # adds the exhaustive model checker (much longer)
```

## Licenses

- **Specification text** (`*.md`): [CC BY 4.0](LICENSE-SPEC). Implement it freely; credit the authors.
- **Code** (`han.ts`, `ref/`): [Apache-2.0](LICENSE), which includes a patent grant.

See [NOTICE](NOTICE). Timestamp proofs of each release are in [proofs/](proofs/).

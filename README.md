# HAN — safe agent actions for shared conversations

**HAN is an open standard that makes it safe for AI agents to act in the world from shared conversations. In any room where people and agents work together, every real-world action (a push, a payment, a deploy, an email) is authorized once, executed at most once, accounted against a budget, and leaves a record that anyone in the room can verify.**

HAN doesn't replace chat. Conversations run on an existing network (Matrix first), and HAN adds a per-room **sequencer** plus the rules agents need:

- **One answer to "was this authorized?"** The sequencer orders every agent action into a signed, hash-chained log.
- **At most one launch per approved action**, and an honest `unknown` after a crash, never a silent retry.
- **Accountable agents:** every agent has a human operator who vouches for it, re-checked at every action.
- **Budgets** reserved before an action and settled after it, delegated down task trees. Agents can't spend outside their allocation.
- **Approvals bound to the exact plan**, with expiry and separation of duties.
- **Live sessions you can watch, read and steer** across silos, while the work happens, not after the PR. Owners set who may watch, read or steer per space. Every steer is signed and sequenced. An agent can be *guarded*: any outside instruction (a steer, a task, or naming it as executor) then puts it on hold for its own human, so it can't delegate, relay or approve, and nothing it proposes or executes runs without that human's approval, even if proposed before the hold. Only that human can loosen it (the agent can't, and owners may guard every agent with `forceGuarded`). Local actions in its sandbox and what it says in messages are host matters ([§7.5](architecture.md)).
- **Replayable policy:** Policy is five pure functions, so any participant re-checks every decision. A cheating sequencer leaves a portable proof.

> **Status: design draft (v1, round <!--s:round-->22<!--/s-->).** This is a specification with an executable reference ([ref/](ref/)), a working but minimal implementation ([impl/](impl/)) and a clean-room TypeScript implementation written from the spec alone ([cleanroom-ts/](cleanroom-ts/)). There is no production implementation yet. Independent adversarial reviews against a fixed rubric ([grading-rubric.md](grading-rubric.md)) scored <!--s:scores-->rounds 1–6 from 57.8 to 76.6 / 100, rounds 8–16 (after the scope change) 67.8, 67.5, 69.8, 69.2, 71.1, 72.5, 72.7, 73.1, 72.2, round 18 73.4 (round 17 was folded into it), round 19 73.9 and round 21 **73.7** (rounds 20 and 22 not graded yet)<!--/s-->. See [HISTORY.md](HISTORY.md). Expect breaking changes.

**What impl/ contains.** A Python sequencer (SQLite WAL store, generic WebSocket binding, a Matrix application service), client with outbox, executor, agent host (a scripted agent and a Claude Code driver), verifier, federation between servers, and a CLI: <!--s:impl_n-->197<!--/s--> tests, <!--s:impl_live-->1<!--/s--> of them a live Claude Code run that is skipped by default. The Matrix binding is tested on a real Synapse, without end-to-end encryption. **The honest gaps:** the TypeScript and Python implementations have been run against each other ([interop/](interop/): <!--s:interop_matrix-->ts->py 24/25, py->ts 25/25 (24 without a workaround)<!--/s--> client operations, measured before the round-21 fixes; the matrix has not been re-run since, and the Python server fails <!--s:wv_r22_py_fails-->6<!--/s--> of the <!--s:wv_n-->82<!--/s--> wire vectors until they are regenerated for round 22); no power-cut or `replicated`-durability test; `guarded` has no OS sandbox on Windows, so it is advisory there, and <!--s:advisory_claim-->the host says so in its `status` items (`test_host_p15.py::test_guarded_status_says_it_is_advisory_without_a_sandbox`)<!--/s-->; <!--s:gate_claim-->the implementation's gate runs on a `PreToolUse` hook that sees every call, with `canUseTool` as a second check (`test_host_claude_gate.py`)<!--/s--> (§8.4); no moves between real servers; no E2EE in the Matrix binding. [Architecture §10](architecture.md) lists every rule with its check, and says plainly where there is none.

## Read

| File | What it is |
|---|---|
| [goal.md](goal.md) | why HAN exists, what "v1 done" means, and what HAN deliberately is not |
| [architecture.md](architecture.md) | the specification: entries and receipts, the three sequencer calls, modules, rules (`R-`, `P-`, `X-`, `T-`), the trust model, the Matrix binding, and a rule-by-rule verification table (§10) |
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

# History

This repository starts at its first public commit. The work before it was not under version control. Dates before the first commit come from the files' own modification timestamps and are stated as such, not as verifiable proof.

## Before this repository (from file timestamps)

| Date | Milestone |
|---|---|
| 2026-10-08 | Interface design drafts v0.2 → v0.3 → v0.4 of "Human–Agent Network": persistent identities, shared contexts, recoverable agent execution, replaceable infrastructure. Each was revised after an external review round. |
| 2026-10-09 | v0.4-r1 elaboration: one complete agent request traced end to end, 12 UML views, and a design atlas. |
| 2026-10-10 | Goal reset: HAN is an **open standard** for a public, federated network in which humans and agents communicate and work as equals. The v1 redesign starts. |

## v1 rounds (2026-10-10 → 2026-10-11)

HAN was first published as v1 round 6 (tag `v1-draft-r6`). In round 7 its scope narrowed to the part that is new: making agent actions safe. Federated chat is left to networks like Matrix that already do it well.

Each round was graded by a fresh, independent reviewing agent against the same fixed rubric ([grading-rubric.md](grading-rubric.md)), with no stake in the score.

| Round | Independent score | What changed |
|---|---|---|
| 1 | 57.8 | v1 core: one primitive (signed entries in a per-space log), six calls, federation by mirroring |
| 2 | 69.5 | Claim-as-fence executor protocol; redactable content separated from the permanent header; reference Policy, conformance vectors, model checker |
| 3 | 73.1 | Revocation and budget fixes; real-Ed25519 wire layer; head gossip; notices; the model checker found a deadline-ordering defect |
| 4 | 71.6 | Simpler budgets; deadlines measured on the executor's own clock; fold-totality fuzzer, which found a budget-accounting defect |
| 5 | 74.1 | Forced takeover after home death removed from the core (forks instead); window pinned in each claim; per-space blob supply; content validation |
| 6 | 76.6 | Abandon margin proven for two-sided clock drift (model with clock rates); signed move acceptance; signed redaction proofs; private gossip; signal relay |
| 7 | — (graded with 8) | **Repositioned as the agent-safety layer over existing chat transports (Matrix first).** Chat, presence, files and invites move to the transport, and the core shrinks to the sequencer (3 calls), Policy, Identity, verification and the executor. Fixes: claims state their window, owners are human, operator withdrawal is checked at claim, abandon needs elapsed sequencer time, assignees can't close tasks with running effects. |
| 8 | 67.8 | **Live sessions.** `session.open`, `session.input` and `session.close` put live work in the log as authority (who may watch, read and steer, set per space by owners), while the stream itself rides the transport (T-07, Matrix threads and MatrixRTC). Steering is signed and sequenced. A session can make outside steers `guarded`: the harness's stricter mode for that turn, and the effects they trigger wait for the session's own human (P-15, R-24). Text read from other sessions is data, never instruction. |
| 9 | (pending) | **Taint replaces steer tags.** Round 8's guard depended on the steered agent tagging its own proposals, so it could be dropped or laundered through another agent (review round 8). Now an outside steer into a guarded agent session taints the agent in the log; tasks and relayed steers carry the taint; every effect the agent proposes needs its human, whose approval is checked like any other (role, key, expiry). Owners may restrict steer modes. T-07 gets a `StreamItem` type and binding calls; the Matrix binding uses per-right E2EE rooms. §8.3 restores the generic binding's rules as G-01…G-05; §8.4 profiles Claude Code; coverage.md rewritten. |

The full defect lists from each review drove the next round. Defects the executable checks found on their own are listed in [architecture.md §10](architecture.md).

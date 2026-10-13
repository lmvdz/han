# Decisions for round 30 (draft)

Status: draft. Round 30 runs after the repo restructure (PLAN.md §10, "Restructure timing").

Inputs:
- the round-29 review (77.9, defects U1–U8);
- the transport, identity and profile decisions in PLAN.md §10.

Theme: **one protocol over QUIC, and delegation that carries its limits.** Stay inside recorded guarding (the round-27 choice): no holds. Everything new is derived from the log.

All agents follow these. Where one is silent, the owning agent decides and writes it down.

## Protocol fixes from review round 29

| # | Topic | Decision |
|---|---|---|
| V1 | **A delegate carries the limits (U1)** | Define `carried(X)` = the union of `guardedFor` over X's open, **accepted** tasks (V4). While `carried(X)` is non-empty: a `task` X creates includes `carried(X)` in its `guardedFor`, which closes the parentless-subtask escape; and X may not approve, govern or steer others, the same Limits as a guarded author. It is derived from the log and ends when the tasks close. R-24's stricter mode states that HAN-native actions in a strict turn (`session.input`, `task`, `effect.approve`, `member.set` through HAN's tools) are proposals or refused. Vectors: the reviewer's three paths (parentless subtask, steer, approve). model_guard gains a delegate-chain property. |
| V2 | **The bounds hold (U2)** | Count the proposer and the executor unconditionally in the 19-agent bound at `effect.propose`, so later guarding can't push a claim past 19 guards. Re-derive `MAX_UNCONSUMED_APPROVALS` and `MAX_APPROVALS` and state the proof in P-08. fuzz_fold must reach the reviewer's counterexample (`p4_bounds.py`, `p4b_bounds55.py`) and show it now holds. |
| V3 | **The cap counts only usable approvals (U3)** | An open approval counts toward `MAX_OPEN_APPROVALS` only if its author is an E3 candidate now (matches the current rule, `separateApprover`, holds a role) and is not guarded. Reuse `approval_candidates`. Vectors: the rule changed to `["owner"]` after 16 member approvals; 16 approvers demoted to guest. |
| V4 | **Only accepted tasks carry guarding (U4)** | A task assigned to someone else adds its `guardedFor` to their effects (T1) and to `carried` (V1) only after they accept it (`task.update` accepted). A task whose assignee is its author, such as an agent claiming an issue, counts as accepted. Vectors: an unsolicited assignment to an owner burdens nothing; the 20-task denial needs accepted tasks. |
| V5 | **Forcing doesn't depend on every approver's server (U5)** | Record each approval's author `actorKind` at fold (it never changes) and judge forcing from it. Drop the extra pins of unconsumed approvers on `effect.approve`, and the quirk that forced approvals count again after forcing is switched off. |
| V6 | **Stale text (U6)** | Reword: §7.5's "mark every session" (the taint is per agent), §8.4's "while guarded" (now strict turns), and the README's "re-checked at every action" and §11's "confirmed human operators" under `allowAgents: "any"`. |
| V7 | **Sandbox boundary (U7)** | Define the boundary as the host process boundary, or the harness sandbox where one exists. State which local writes a strict turn allows (the session's worktree) and which become proposals. |
| V8 | **Timing-sensitive test (U8)** | Make `test_verify_follow.py::test_cli_follow_live_until_seq` deterministic, with no reliance on wall time under load. |

## Transport, identity and profiles (PLAN.md §10)

| # | Topic | Decision |
|---|---|---|
| V9 | **§8 rewrite** | §8.1 requirements; §8.2 the QUIC binding: dial by key, ALPN `han/1`, one stream per call, one-way streams for subscribe and watch, blobs as frames, JSON for signed authority, CBOR (integer keys, zstd batches, signed over the sent bytes) for bulk; §8.3 Matrix as an informative integration. Remove the WebSocket generic binding and the Matrix appservice binding. iroh is named as an implementation choice, never a requirement. |
| V10 | **Peer filter** | A node refuses connections from node ids that aren't registered devices of members of a space it serves. A one-time invite secret admits a joining device once. |
| V11 | **Devices and node ids** | One actor per person or named agent. Each device has its own key, added to the profile with `key.add` together with its node id. A lost device is revoked. Recovery uses a recovery key, or co-owners re-admit. |
| V12 | **Invite tickets** | Format and flow: sequencer node id, space id, one-time secret; `join` proves the secret, and an owner's `member.set` admits. Optional admission by GitHub org or team membership (gist-nonce proof) as a space rule. |
| V13 | **Profiles tier** | Create `spec/profiles/` with `han.status/1`, as defined in PLAN.md §5 with the round-10 fields. A profile is normative for interoperability between HAN apps and carries no authority. A task gains an external reference (issue URL). |
| V14 | **Witnesses** | `impl/` and `cleanroom-ts/` stay Policy witnesses (vectors and differential fuzz). Their WebSocket servers freeze at round 29 and leave the live interop. Interop evidence for the new binding comes with the Rust implementation. |

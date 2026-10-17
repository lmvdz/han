# In-flight state: the fact catalog

A living reference: one entry for every fact HAN knows about work in flight, what it is derived from, and how other agents and faces ask for it. Some entries describe what HAN does today, and some are planned. The design and the decisions for the planned facts are in [the round-33 study](https://github.com/lmvdz/han/blob/main/spec/decisions/studies/r33-inflight-state.md). New facts are added here (see [Adding a fact](#adding-a-fact)).

As of 2026-10-15.

## Terms

- **Trust.** Each fact has one trust level:
  - **log**: folded from verified records of the space's log. Every node that has the same log prefix computes the same fact.
  - **observed**: the host or node derived it from something it sees itself: the workspace diff, a parser, the egress proxy's ledger, tool output, git. The agent does not report it. It is still the host's word: a profile item carries no authority (spec/profiles/README.md), and a face never presents it as a log fact.
  - **reported**: the harness reports it and the host passes it on (ACP `_meta`, `usage_update`). The host can't check it.
  - **advisory**: the agent declares it (a plan, planned paths, a `summary`). Faces label it as advisory, and no query mixes it into observed rows.
- **Right.** The space right a reader needs. **watch** facts travel as status and profile items to actors that `sessionAccess` grants watch. **read** facts travel as `context` items to actors granted read (T-07). Log facts follow the log's read rights (P-02).
- **Carrier.** How the fact reaches a node: a log entry, a `han.status/1` item, a `han.item/1` item, a `han.timeline/1` batch, or a node-local derivation that is never sent.
- **Status.** `exists` (built, with the code location) or `planned` (round 33).

## Catalog

### F1 files-touched

| | |
|---|---|
| Status | **exists** |
| Trust | observed (host) |
| Derived from | `Vcs::changeset(workspace, base)`: committed and uncommitted changes and untracked files that are not ignored (crates/han-vcs/src/lib.rs). `touches()` in crates/han-host/src/host.rs maps it to `files` |
| Updated when | after every tool call that completes or fails, and at the end of each turn (`refresh_files`, off the event loop) |
| Shape | `han.status/1` `files: [{path, hunks: [[start, len]]}]`, with hunks against `base` on the new side. Host bounds: at most 500 files, 200 hunks per file (crates/han-host/src/status.rs) |
| Carrier, right | `han.status/1` profile item; watch |
| Index | node `SpaceModel`: the latest status per session (highest `seq`). The overlap view groups touches by `(repo, path)` (crates/han-node/src/model.rs `file_overlaps`) |
| Queries | `WhoIsOn{target: Path}` (a path, or a prefix ending in `/`); `Overlaps{space}` tier 2; MCP `team_who_is_on`, `team_overlaps` |
| Cost today | the overlap view is recomputed in full when the model's `rev` moves: O(Σ k²) over the sessions k touching each path. Planned: incremental (study §4) |
| Planned | `from`/`to` content ids per file (already in `FileChange.old_hash`/`new_hash`, dropped by `touches()`); a glob query (`touching`) |

### F2 worktree-history

| | |
|---|---|
| Status | **exists** |
| Trust | observed (host), signed batches |
| Derived from | a `notify` watcher on the worktree; after a quiet period, changes become `han.timeline/1` batches (crates/han-host/src/mirror/mod.rs). Text is masked before it is recorded (SR-52) |
| Updated when | each quiet period; each batch names the ACP tool call that was running (`step`) |
| Shape | `han.timeline/1` §12 (TL-81…TL-88): creates, line-then-character edits, deletes, renames, mode changes. Binary files are a BlobRef |
| Carrier, right | `han.timeline/1` stream items; read |
| Index | node timeline views: `Timeline{session, at?}`, the `Patches` and `ChangeSet` views (spec/ui/data.md) |
| Queries | a session's diff at a point in time; which tool call changed a line |

### F3 session-state

| | |
|---|---|
| Status | **exists** |
| Trust | observed (host) for `state`, `tool`, `files`, `tests`, `sandbox`, `gate`; advisory for `step` text and `plan`; derived on the node for `stale` and `failed` from silence |
| Derived from | `StatusDeriver` folds ACP events and bridge signals into one body (crates/han-host/src/status.rs) |
| Updated when | each signal that changes the body; `seq` goes up by one per item |
| Shape | `han.status/1` (spec/profiles/han.status-1.md) |
| Carrier, right | `han.status/1` profile item; watch |
| Index | latest item per session; board rows and facets (crates/han-node/src/views.rs) |
| Queries | `Board{space, group, window}`, `Session{id, part: facts}`, MCP `team_sessions` |

### F4 test-counts

| | |
|---|---|
| Status | **exists** (counts only) |
| Trust | observed (host), from tool output |
| Derived from | `parse_tests` over the output of each completed tool call: cargo/libtest, pytest, jest summary lines (crates/han-host/src/status.rs) |
| Updated when | a tool call ends whose output has a test summary |
| Shape | `tests: {pass, fail}` |
| Carrier, right | `han.status/1`; watch |
| Index | latest status per session |
| Queries | board and session rows |
| Planned | per test id (F13) |

### F5 claims-and-leases

| | |
|---|---|
| Status | **exists** |
| Trust | log |
| Derived from | issue claims: open tasks with an `external` reference, and live sessions working an issue. Path leases: `han.lease` extension entries `{paths, ttl, issue?}`, which lapse `ttl` seconds after their receipt and block nothing (PLAN §6.6). Effect claims: `effect.claim` (the executor's fence, E1–E6) |
| Updated when | each record; leases also expire with time (the model's `time_signature` and `tick`) |
| Shape | `OverlapRow{kind: issue_claim | path_lease, claim: {kind, ref, expires_at?}, parties}` (crates/han-node/src/api/rows.rs) |
| Carrier, right | log; the space's read rights |
| Index | `SpaceModel.tasks`, `SpaceModel.leases`; overlap tier 1 |
| Queries | `Overlaps{space}` tier 1, `WhoIsOn{target: Issue}` |

### F6 blocked-on

| | |
|---|---|
| Status | **exists** in parts |
| Trust | log for approvals; observed (host) for "waiting for input" and a queued or refused proposal |
| Derived from | effects waiting for approval (`effect_waiting` in the model); host states `waiting-approval` and `blocked` with its step (`QUEUED_STEP`, a refusal's reason); `idle` with "waiting for input: …" |
| Shape | today spread over `ApprovalRow` (with `needed{rule, guards[]}`, Q-APR-8), `BoardRow.state`, `state_reason` (Q-DAT-12) |
| Index | `SpaceModel.effects`, board rows |
| Queries | `Approvals{actor}`: what an actor may approve |
| Planned | one fact per session with every pending dependency and who it waits on, and `blocked_on(actor)` (F15) |

### F7 spend

| | |
|---|---|
| Status | **exists** |
| Trust | observed (proxy) in API-key mode; reported (ACP `_meta.quota`, `usage_update`) in subscription mode |
| Derived from | the egress proxy reads `usage` from provider responses, JSON and SSE (crates/han-host/src/proxy/usage.rs), and prices them from the node's `prices.json`; budget from the space policy, narrowed to the task (SR-41) |
| Updated when | each model response (proxy); each ACP usage update |
| Shape | `tokens: {in, out, cost}` in `han.status/1`; node extension `budget: {unit, limit, spent, not_enforced?, refused?}`; `CostRow{session?, agent?, tokens_in, tokens_out, cost, unit?}` |
| Carrier, right | `han.status/1`; watch |
| Index | latest status per session; summed per space |
| Queries | `Cost{scope: Space | Session}` |

### F8 base

| | |
|---|---|
| Status | **exists** (base only) |
| Trust | observed (host) |
| Derived from | the commit the workspace was created at (`Vcs::create_workspace`) |
| Shape | `han.status/1` `repo`, `base` |
| Planned | freshness against the target ref (F16) |

### F9 session-items

| | |
|---|---|
| Status | **exists** |
| Trust | observed (host) for `tool_call` and `file_change`; the node makes `status`, `input` and `decision` itself, from the status item and the log |
| Shape | `han.item/1` `{id, turn?, kind, ...}` with kinds `message`, `reasoning`, `plan`, `tool_call`, `file_change`, `turn_start`, `turn_end` (spec/ui/data.md Q-DAT-3) |
| Carrier, right | stream; watch, with `reasoning` and tool content needing read |
| Index | in memory and in `node.db`, 10,000 items per session; reloaded when the node starts; no backlog from the home |
| Queries | `Session{id, part: items}` |

### Planned facts (round 33)

Each is defined in the study. Short form:

| Id | Fact | Trust | Right | Carrier | Main query |
|---|---|---|---|---|---|
| F10 | symbols-touched | observed (host, tree-sitter) | read | `han.inflight/1` `symbols` | `touching_symbol` |
| F11 | api-surface | observed (host) | read | `han.inflight/1` `api` | `who_depends_on` |
| F12 | deps-changed | observed (host, manifest and lockfile parsers) | watch | `han.inflight/1` `deps` | `deps_changed` |
| F13 | tests-by-id | observed (host, structured test output) | read | `han.inflight/1` `tests` | `failing` |
| F14 | error-fingerprints | observed (host); "resolved" links derived on the node | read | `han.inflight/1` `errors` | `seen_error` |
| F15 | blocked-on | log and observed | as its sources | node-local | `blocked_on` |
| F16 | freshness | observed (host, `Vcs`) | watch | `han.status/1` `fresh` | `stale` |
| F17 | conflicts-main | observed (host, `Vcs::merge_simulate`) | read | `han.inflight/1` `conflicts` | `conflicts` |
| F18 | conflicts-peer | derived (node, pinned line merge over mirror content) | read | node-local, optionally shared | `conflicts` |
| F19 | intent | advisory (agent) | watch | `han.status/1` `intent` | `touching(…, advisory: true)` |
| F20 | landing | log and `han.land/1` items | as its sources | han-29's [`han.land/1`](https://github.com/lmvdz/han/blob/main/spec/decisions/studies/r33-land-profile.md) | `conflicts`, `blocked_on` |

## Adding a fact

1. **Derivation.** Say what the host or node observes and when it updates. If the agent declares it, it is advisory and gets its own member. Never mix it into an observed one.
2. **Shape and bounds.** Give a JSON shape with a bound on every list and string. Identifiers must match a stated character class. Anything else is free text, which reaches an agent only in the data envelope.
3. **Right and carrier.** Choose watch or read. A fact that reveals code (names, text, signatures) is read. Small watch facts go into `han.status/1` as new members (unknown members are ignored, so no new version is needed). Bulky or read facts go into `han.inflight/1`. Facts that only the node derives are not sent.
4. **Index.** Name the structure, its key, what one update costs, and its size at 10,000 sessions. The index must be a pure function of its inputs (the study, §6): no wall clock except through the explicit `now`, and an order fixed by key.
5. **Query.** Add an operator to `han_query` with typed rows, a default and a maximum `limit`, a deterministic order, and its rights check.
6. **Conformance vectors.** Add inputs (log records, profile items, `now`) and the expected answer as JCS to `conformance/inflight-vectors/`. A host-side extractor (parser, normaliser) gets its own vectors: input text in, fact out.
7. **Catalog.** Add the entry here. Mark it `exists` only when the code is merged, with its location.

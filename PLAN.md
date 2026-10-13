# HAN implementation plan (Rust)

Status: draft, 2026-10-12. Decisions so far:
- One protocol over QUIC with dial-by-key; iroh is the implementation choice.
- Matrix becomes an integration, not a binding.
- Rust is the product implementation.
- One monorepo.
- A han-node daemon on every device; faces sit over its API.
- Integrations run as separate processes.
- **No harness lock-in.** Every agent is driven through ACP (Agent Client Protocol). pi, through `pi-acp`, is the reference harness, and any ACP agent should work.
- **The sandbox is the gate.** An agent has no egress and no credentials. Its only way out is HAN's `propose` tool, and han-exec runs effects only with valid approvals. Harness permission prompts are a convenience, never the guarantee.
- **Agents run on members' machines and on shared runners.** A runner is han-node running headless.
- **A work-timeline engine is in the MVP.** `han-timeline` is built by a two-team clean room (§9).

## 1. The MVP: team awareness

**A team of 2–4 people, each running several coding agents (pi first; any ACP agent). Everyone, agents included, sees live what every agent is working on, and can drill into any session. Nobody has to ask "what are you working on?"**

1. **A team space.** One member's always-on machine, or a small shared box, runs the sequencer. Everyone dials it by key over iroh.
2. **`han host` per agent.** It opens a session linked to its GitHub issue and streams output and status. The host derives the status itself (step, last tool, files touched, tests, blocked or waiting); the model adds only an occasional short summary.
3. **The live board in `han tui`.** One row per session:
   - person and agent;
   - issue;
   - status line, files, last activity;
   - blocked or needs-you.

   The board filters by repo, label, path and person. Drill-down shows a session's live stream or its context.
4. **Agent tools (MCP).** Every agent gets these tools:
   - `team.sessions`
   - `team.who_is_on(issue|path)`
   - `team.read(session)`
   - `team.overlaps()`

   Answers are marked as data and are never treated as instructions.
5. **Overlap alerts.** Two sessions on the same issue, the same files or overlapping ranges (from their live timelines) light up on the board and notify both agents.
6. **GitHub, one-way.** Issues become tasks; the board shows issue to agents.
7. **The approval gate is configured per space.** For example, push to `main`, releases and outside HTTP calls need a human; everything else inside the sandbox runs freely.

8. **Live work timelines.** Every agent session is a timeline over its worktree in `han-timeline`, synced to readers. Drill-down shows the agent's actual changes as they happen, before any commit. Accepting the work (merge or push) is an effect that needs approval.

**Defaults where the MVP leaves a choice:**
- Agents read each other's sessions but don't ask each other questions yet; that's the first feature after the MVP.
- Every team member reads every session's full context; trust is set per space.
- Humans assign issues, and agents may also take issues labelled `agent-ready`.

**Done when:** the team uses it for two weeks of real work, and the verifier shows no action outside the sandbox without a valid approval.

## 2. Features to design against

| Horizon | Features |
|---|---|
| MVP | the above |
| Next | A2A questions and tasks; steering; approvals across people; two-way GitHub |
| Then | GUI (Tauri); Matrix notifier bot; Linear and Jira; release-tested support for more ACP agents (Claude Code, Codex, Gemini CLI…); more executor actions |
| Later | mobile (uniffi, Swift and Kotlin); key-derived actor IDs; peer-to-peer homes and moves; search and summaries across teams; SSO and audit export |

## 3. Architecture

```
  faces:    han tui │ GUI (Tauri) │ mobile (uniffi) │ agent tools (MCP) │ integrations (GitHub, Matrix bot…)
                 └──────────────┬─── node API: call(Command) + view(Query) ───┘
  device:            han-node   keys · iroh endpoint · outbox · sync · cache · views · supervisor
                        │                     │
  roles:            han-host  (AgentDriver)   han-exec (Action)        han-seq (sequencer engine)
                        └──────────┬──────────┴───────────┬────────────┘
  protocol:                     han-net   frames over QUIC · gossip · blobs   (iroh)
  pure:                         han-core  types · JCS · signatures · Policy · replay   (no I/O; wasm/FFI)
```

- **han-node** is the one runtime per device. It holds the device's keys and its single iroh endpoint, syncs the spaces the device follows, keeps the outbox, and maintains the live **views**.
- **Supervisor.** han-node supervises the local roles (`han host` per agent, `han exec`, and `han seq` if this device is a home) as child processes. The executor always runs as its own process, because it holds credentials. A host never shares an address space with it.
- **Faces** never talk to the network directly. Each one embeds han-node (GUI, mobile) or attaches to the daemon over local JSON-RPC (TUI, MCP tools, integrations, scripts). The local channel is a named pipe on Windows and a Unix socket elsewhere.
- **Features are views.** The board, who-is-on, overlaps, the approvals inbox and later search are projections that update incrementally from the log, the streams and gossip, all in one views engine.

## 4. Workspace layout

```
han/                       (monorepo; today's v1/ moves under spec/ and conformance/)
  spec/            architecture.md · han.ts · DECISIONS-*.md · HISTORY.md · grading-rubric.md
  ref/             Python executable spec; generates the vectors
  conformance/     vectors · wire vectors · fuzz_diff · interop drivers (py, ts, rs)
  crates/
    han-core/      pure: types, JCS, ids, Ed25519, validation, Policy, replay. Features "wasm", later "ffi"
    han-net/       iroh binding: ALPN han/1, frames on QUIC streams, gossip topics, blobs
    han-seq/       sequencer engine: §4.1 pipeline, single writer, SQLite
    han-node/      device runtime, views engine, node API, supervisor
    han-host/      agent host: the ACP client (pi-acp first), sandbox launch, status derivation, R-24 duties
    han-timeline/  work-timeline engine (clean room, §9): ops DAG, timelines, CRDT text/maps/tree, sync, SQLite
    han-exec/      executor; actions/{git_push, http_request, file_write}
    han-mcp/       agent tools over the node API
  apps/
    han/           one binary: init · node · seq · host · exec · tui · conform
    han-tui/       ratatui faces, the board first
  integrations/
    han-github/    separate process, an actor over the node API
  frozen/          impl-py, cleanroom-ts (interop witnesses, no new features)
```

**Dependency rule, checked in CI:**
- han-core depends on nothing async or I/O.
- han-net, han-seq and han-node may depend inward.
- Faces, han-mcp and integrations depend only on han-node's API types, never on han-net or han-seq.

## 5. Interfaces (the whole surface each module exposes)

### Node API: two methods

```rust
async fn call(cmd: Command) -> Result<CallResult, NodeError>;          // a Command ends as a receipt, a verdict or an error
fn view(q: Query) -> impl Stream<Item = ViewEvent>;                     // Snapshot first, then Diffs, until dropped
```

- `Command` covers:
  - `SpaceCreate`, `SpaceJoin{addr}`;
  - `MemberSet`;
  - `TaskCreate{external?}`, `TaskUpdate`, `TaskCancel`;
  - `SessionInput`;
  - `EffectApprove`, `EffectCancel`;
  - `AgentStart{driver, workspace, issue?}`, `AgentStop`.
- `Query` covers:
  - `Board{space, filter}`, `Session{id}`;
  - `WhoIsOn{issue|path}`, `Overlaps{space}`;
  - `Approvals{actor}`, `SpaceStatus{space}`;
  - later `Search`.
- Both are serde enums with JSON at the boundary, so the same surface works in-process, over JSON-RPC and over uniffi.
- The two method names never change; new features add enum variants.

### AgentDriver (han-host)

```rust
trait AgentDriver {
    async fn start(&mut self, cfg: SessionConfig) -> Result<()>;
    async fn turn(&mut self, input: TurnInput, mode: Mode) -> Result<()>;   // Mode::{Auto, Strict}
    fn events(&mut self) -> BoxStream<'_, AgentEvent>;                      // Output, ToolCall, FileTouched, Tests, Summary, TurnEnded
}
```

- **There is one driver: the ACP client.** A harness is just an ACP agent command (for example `npx pi-acp`) plus its sandbox profile.
- **Safety doesn't depend on the driver.** The agent process runs in the sandbox (MXC) with no egress and no credentials, so a world action can happen only through HAN's `propose` MCP tool.
- **ACP permission requests become UX.** The host answers them by policy (allow inside the sandbox, otherwise "use propose"), and they show on the board as `waiting-approval` hints.
- **The host owns** the R-24 duties (strict triggers, taint, naming the task), `StatusItem` derivation, and mirroring the worktree into `han-timeline`. The driver only runs the agent.

### Action (han-exec)

```rust
trait Action {
    const KIND: &'static str;
    fn plan_check(&self, plan: &Value) -> Result<(), PlanError>;
    async fn run(&self, plan: &Value, key: IdempotencyKey, ctx: &ActionCtx) -> Outcome;
}
```

### Injected from day one

```rust
trait Clock { fn now_ms(&self) -> u64; fn elapsed(&self) -> Duration; }
trait Transport { /* han-net: dial(node_id), serve, gossip(topic), blob_put/get */ }
```

These make deterministic simulation of whole teams possible (fake clock and network via `turmoil`).

### StatusItem (`han.status/1`: a namespaced, non-authority stream item)

```
{ session, agent, delegate_of?: ActorId,             // the human assignee; the agent is the delegate (Linear's model)
  issue?: "github:owner/repo#123", repo?: "github:owner/repo", base?: sha,
  state: "working" | "waiting-approval" | "blocked" | "idle" | "done" | "stale" | "failed",
  step: string, tool?: string, plan?: [{text, done}],
  files: [{path, hunks: [[start, len]]}],           // from `git diff -U0` against base
  tests?: {pass, fail}, summary?: string, seq, at }  // seq: per-session counter (freshness without wall clocks)
```

This is the one schema the board, the overlap alerts, the agent tools and the GitHub sync all read. `stale` is derived from the time since the last item, never from the agent's own claim.

## 5a. Build or adopt (research, 2026-10-12)

The full findings are in `research/{intentd,coordination,agent-plumbing,rust-blocks}.md`. No existing tool gives a self-hosted, live view of every agent session across a team. Claude Code does it for one account, Agent Relay does team messaging through a hosted engine, and MCP Agent Mail does file leases on one server. So the board, overlaps and the trust layer are ours to build. Almost everything below them is adopted.

| Need | Decision | Notes |
|---|---|---|
| Transport | **adopt** iroh 1.3 (`Endpoint`, `Router`, one ALPN `han/1`, `IncomingFilter`) | wire and API stable until the next major version; Python binding (core only) for the M2 interop |
| Presence | **adopt** iroh-gossip, presence only | no access control and origins aren't authenticated: derive the topic from a secret only readers hold, sign every payload, stay under 4 KiB. Status flows to readers through the node, never over gossip |
| Blobs | **build** as `han/1` frames | iroh-blobs is BLAKE3-only and serves by hash (conflicts with G-01) |
| Log and sequencer | **build** han-seq on rusqlite 0.40 + rusqlite_migration | nothing gives exclusive claims. p2panda (per-author chains, eventually consistent groups) can't, but its header/body split confirms ours. Later: C2SP checkpoints, witnesses and tlog tiles for head gossip |
| Signatures | **adopt** ed25519-dalek **3.0**, `verify_strict` | iroh-base requires 3.x; pin edge-case vectors across ref, TS and Rust |
| JCS | **build** a strict I-JSON parser and canonicalizer; `serde_json_canonicalizer` as a test oracle | the crate casts integers to f64 and doesn't validate |
| Views | **build** hand-rolled folds emitting a snapshot, then diffs | property: snapshot plus every diff equals a fresh read (from intentd); cap rows by bytes. differential-dataflow and DBSP are too heavy |
| Local API | **adopt** `interprocess` 2.4 (named pipe or Unix socket; pipe restricted to the current user) + our own JSON-RPC framing | jsonrpsee has no IPC transport; parity-tokio-ipc is dead |
| Simulation | **adopt** turmoil 0.7 over the injected `Transport`/`Clock` | real iroh can't run on turmoil sockets, so han-net gets a simulated `Transport` |
| TUI | **adopt** ratatui 0.30 | |
| Mobile | **adopt later** uniffi 0.32, exposing only `call(json)` and `view(json, cb)` | the Spacedrive pattern |
| (superseded) Claude Code native driver | **dropped** for ACP; notes kept for reference: `drivers/claude_code` on the CLI: `-p --bare`, stream-JSON in and out, `--session-id`, `--strict-mcp-config`, and a **PreToolUse hook** passed in `--settings` with `onFailure: "block"` (CLI ≥ 2.1.295) | the hook runs before every rule and mode, and its deny wins even in bypass mode. `defer` parks the turn, so Propose = park, approve, `--resume`. Parallel tool calls fall back to deny with a reason. `--bare` stops the repo from configuring the agent. Also read `claude agents --json` and the hook events instead of re-deriving them |
| Agents (all) | **adopt** ACP: `agent-client-protocol` 3.3 (pinned), one client in han-host | pi through `pi-acp` (registry v0.0.34, Node 22+, pi ≥ 0.81; pi has no permission prompts by design); Claude Code through `claude-agent-acp`; Codex, Gemini CLI, Copilot CLI, goose and OpenCode. Safety is the sandbox plus the `propose` tool, so prompt fidelity doesn't matter |
| Agent tools | **adopt** `rmcp` 3.5 (pinned) | the MCP 2026-07-28 spec is stateless, so the `team.*` tools take handles as arguments. A required `summary` argument on each call feeds `StatusItem.summary` |
| Sandbox | **integrate** Microsoft MXC (`mxc-sdk` 1.0, Rust, MIT: Windows 11 without admin, Linux bubblewrap, macOS seatbelt) + a HAN-owned egress proxy for domain allow-lists | Claude Code's own sandbox covers shell commands only and does nothing on native Windows, so we confine the whole agent process. Fallbacks: WSL2 or Podman. Use `landlock` to harden han-exec |
| git and GitHub | **adopt** octocrab 0.54; shell out to `git` for push; gix for reads | gix still can't push. A push is desired state: "move ref R from A to B", so a retry is safe |
| Executor ledger | **build** a small SQLite ledger keyed by idempotency key | "unknown is not success"; invocation ids are never reused (from intentd). The job frameworks don't fit |
| Overlap detection | **build**, in three tiers | 1. claims: advisory leases on issues and paths, with a TTL; a conflict reports who holds it and doesn't block (Agent Mail, intentd). 2. touched hunks: range intersection on `files[].hunks`. 3. real conflicts: a `gix merge-tree` merge simulation between session branches (the Clash technique). Alerts go to both agents and to the board |
| Work timelines | **build** `han-timeline` in a two-team clean room (§9), **in the MVP** | informed by the study's evidence only through a behaviour-only functional spec; Loro and Automerge are prior art for the implementers. Updates travel in HAN-signed envelopes over iroh. `git diff -U0` hunks stay as the fallback and cross-check |
| Agent-to-agent (Next) | **integrate later** Google A2A v1 (`a2a-lf`) as a face over the node API | signed Agent Cards fit Ed25519 profiles. A message to another agent is **data**, never injected input (intentd and Agent Relay inject it, which R-24 forbids) |
| Existing tools | **bridges later** | `integrations/han-intentd` and Copilot or Agent HQ sessions as board rows, if they have an API |

**Strategic note.** Claude Code's single-user features (agent view, cross-session messaging, hooks) overlap the MVP for one person. HAN's edge is the team: several machines and people, any ACP harness, self-hosting, verifiable approvals, and live work timelines.

## 6. Rules that keep it changeable

1. **Authority changes only through the spec.** Anything that grants or withholds rights is a log entry that Policy decides and the vectors cover. Status, presence, summaries and board data are **namespaced stream items and gossip topics** (`han.status/1`, `github.issue/1`). They evolve without a spec round.
2. **Inward dependencies**, checked in CI (§4).
3. **Clock and transport are injected.** A three-node simulation runs in CI from M3 on.
4. **SQLite migrations from the first commit**, and a format version in every database.
5. **Conformance gates every PR:** the policy vectors, the wire vectors, and differential fuzzing against `ref/`.
6. **Keys never leave han-node.** Faces ask the node to sign; the executor's credentials never leave han-exec.

## 7. Milestones

| | Milestone | Done when |
|---|---|---|
| M0 | Repo restructure (§4) after round 29 commits; spec round 30 (below) | the checks pass from the new layout; round 30 is graded |
| M1 | han-core | 150/150 policy vectors; 0 differential-fuzz divergences against ref over 1000 seeds |
| M2 | han-net + han-seq over iroh | wire vectors pass over iroh; the Python impl interoperates through iroh's Python binding |
| M3 | han-node: client, outbox, sync, views (board, who-is-on, overlaps), node API, supervisor | a three-node turmoil simulation shows the board converging; JSON-RPC works on Windows |
| M4 | han-host (the ACP client, sandbox launch, worktree mirroring into han-timeline); han-mcp; han-exec with `git_push` | **first week, a spike that answers:**<br>• does pi through `pi-acp` work from a non-Zed ACP client?<br>• can MXC confine a Node-based ACP agent on Windows 11, with no egress except the HAN MCP endpoint?<br>• does an ACP reject carry a reason back to the agent?<br>• does a propose-only agent stay productive?<br>Then a real session streams status, an agent answers "who is on auth/?" from the tools, and a gated push waits for approval and runs exactly once |
| T0 | Timeline functional spec (team A, §9) | the behaviour-only spec and its vectors are reviewed: no code, no transcribed snippets |
| T1 | han-timeline (team B, §9) | passes the T0 vectors, randomized convergence and sync tests; with HAN-signed envelopes over iroh |
| M5 | han-tui (board, drill-down with live timelines, approvals inbox); han-github (one-way) | the full MVP story works across two machines and a shared runner, with pi plus a smoke test of two or three other ACP agents |
| M6 | Team dogfood | 2–4 people, two weeks; issues feed the spec |

M0's spec round, M1 and T0 run in parallel. T1 runs alongside M2–M4, and its integration into the host and the views is on the MVP's critical path.

**Spec round 30 (part of M0):**
- **Rewrite §8:**
  - §8.1, the requirements;
  - §8.2, the QUIC binding with dial-by-key, frames on streams, ALPN `han/1`;
  - §8.3, Matrix integration (informative).
- **Remove** the Matrix appservice binding and the WebSocket binding.
- **Add:**
  - a node-ID field in profiles;
  - an external reference on tasks;
  - the namespaced stream-item rule and `han.status/1`.
- **Decide:** whether status rides only the sequencer and host fan-out to readers (T-07), or also gossip. Gossip reaches everyone holding the topic, so it carries presence only, unless payloads are encrypted to the space.
- **Update** coverage.md and the interop harness.

## 8. Risks

| Risk | Plan |
|---|---|
| ACP adapters lag or break (pi-acp is MVP-quality) | the M4 spike; pin adapter versions; the sandbox keeps safety independent of the adapter |
| Sandboxing on Windows | MXC first; WSL2 or podman (the existing WSL setup) as the fallback; node, TUI and exec stay native |
| Status leaking through gossip | decided: status flows only to readers through the node; gossip carries presence only, signed, on a topic only readers know |
| Claude Code ships team features | han-host consumes them; HAN's edge is multi-vendor, self-hosted and verifiable approvals |
| Pinned crates churning (ACP, rmcp ship major versions often) | pin exact versions; each sits behind one trait (`AgentDriver`, han-mcp) |
| Reliance on n0's relays | self-host `iroh-relay` for the team |
| Interop witnesses drifting | frozen; they only need to pass the vectors and run the interop harness |
| Provenance of han-timeline | the two-team clean room (§9); the user checks the studied product's terms before anything is published |
| The engine on the critical path delays the dogfood | `git diff` hunks remain a working fallback for the board and overlaps |

## 9. han-timeline: the two-team clean room

**Why.** The engine's model is informed by a reverse-engineering study of a closed product. HAN is an open standard with a public repo, so the code must not descend from study material.

| | Team A (spec writers) | Team B (implementers) |
|---|---|---|
| May read | the study's evidence (the study's evidence), the reference model, the study's notes | **only** `spec/timeline.md`, its vectors, and public prior art (Loro, Automerge, Eg-walker and Fugue papers) |
| Writes | `spec/timeline.md`: data model, ids and versions, timelines, operation semantics and concurrency rules, sync protocol, storage invariants; plus behavioural vectors (input ops, expected states) | `crates/han-timeline` |
| Must not | put code, pseudo-code transcribed from the study, addresses or snippets in the spec | read anything under the study's materials, the studied product or the evidence; use agents that have seen them |

**Rules:**
- **Separate sessions.** Team B agents get fresh contexts whose prompts point only at the spec and the vectors.
- **A third reviewer** checks the spec for transcribed material before Team B starts. Questions from B to A go through written spec revisions, never through code.
- **Additions beyond the studied product:** HAN-signed envelopes, actor ids that are HAN ActorIds, worktree mirroring designed from git's own semantics, and merge as an effect.
- **Before anything is pushed publicly,** the user checks the studied product's terms for reverse-engineering clauses.

## 10. Stack and repository decisions (2026-10-12)

| Area | Decision |
|---|---|
| Encoding | Signed authority entries stay JCS JSON (spec, vectors, signatures). Bulk traffic (timeline ops, session streams, sync) uses a compact binary encoding under `han/1` framing |
| Keys | the OS keychain (`keyring`: Windows Credential Manager, macOS Keychain, Secret Service), with an encrypted-file fallback for headless runners; hardware keys later behind the same interface |
| GUI | Tauri + TypeScript, borrowing as much as possible from t3code, agent-orchestrator and Intent's app (research: `research/gui.md`) |
| License | code Apache-2.0 OR MIT; spec CC-BY-4.0 |
| Storage | one SQLite file per space (log, receipts, timeline), plus one `node.db` per device (outbox, view caches, config). Leaving, moving or deleting a space is a file operation, and each space can have its own encryption key |
| Mirroring | a file-system watcher (`notify`) on each session's worktree records every change as timeline ops, whatever made it. ACP tool-call events attribute changes to steps |
| Types | protocol types stay normative in `han.ts`; Rust is hand-written and checked against it by extending the three-way consistency check. Node API types are Rust-first, generated to TS (specta or ts-rs) and to Swift/Kotlin (uniffi) |
| Tooling | a root `justfile`; cargo-nextest, cargo-deny, cargo-dist; uv for Python; pnpm for TS |
| Repo move | restructure this repo in place with `git mv` (history and blame survive) |
| Versioning | the protocol version (`han/1`, bumped only by spec changes and negotiated through ALPN and hello) is separate from one shared workspace version (0.x) for all crates and apps |
| Clean room | §9's process plus CI guards: `crates/han-timeline` may not import or mention the reference model or the studied product; Team B commits carry a `Clean-Room: B` trailer; `spec/timeline.md` changes need the reviewer's sign-off (CODEOWNERS) |
| Contributions | DCO sign-off; spec changes go through written proposals (DECISIONS-style RFCs) |
| Node runtime | managed: `han` downloads a pinned Node build and pinned ACP adapter versions into its data folder and runs adapters from there; updates are deliberate |
| Bulk format | CBOR (`minicbor`) with integer field keys, batched runs compressed with zstd; each batch signed over its exact bytes (no canonical CBOR needed). Chosen for implementers in other languages; the speed and size gap to postcard is irrelevant at HAN's rates (open: a scratch benchmark on real op shapes if wanted) |
| Platforms (MVP) | Windows 11, macOS, Linux, and WSL2 as a Linux target |
| Observability | `tracing` plus OpenTelemetry **on by default**, exporting only to a collector the team configures, never to the project. Session content (prompts, code, stream text) stays out of exported spans unless a space allows it |
| Timeline merge | three-way content merge: accepting work records a merge onto main that names the incoming version (lineage kept); conflicting edits come out as hunks for review. The approval-gated accept effect writes it. No history-join merge for code |
| Timeline seals | yes, for session transcripts only: the text an agent sent or received is sealed to exactly what it saw, for audit. Not on code |
| Timeline spec scope (T0) | specify now: ids, frontier versions, runs, timelines, registers and maps, text ordering, anchors, sync semantics, merge and seals (as HAN designs). Worktree mirroring is designed by HAN from git's semantics (the evidence is weak there). Study status: text strong; timelines, merges and seals medium; sync medium to weak; nothing verified at runtime |
| Egress default | read-only allow-list through HAN's egress proxy: package registries, docs sites, the model provider's API. Everything else is blocked, so world-changing actions go through `propose`. Spaces widen or narrow it |
| Shared runners | a runner is an actor in the space, with its own identity and an owner as operator. Members start agents on it with a signed `task` assigned to the runner, so Policy decides who may use it and every start is audited |
| GitHub | a per-team GitHub App (fine-grained permissions, short-lived installation tokens). Issue events arrive by webhook through a relay, with polling as the fallback; pushes go through han-exec with the App's token |
| Node API views | every `Query` takes a window (filter, limit, cursor) and streams a snapshot of that window, then diffs; faces scroll by moving the window. Errors are a closed set mirroring the protocol's verdicts: denied, invalid, not_found, unavailable, conflict |
| GUI stack (research: `research/gui.md`) | Tauri 2 + React 19 + TypeScript + Vite + TanStack Router + zustand + Tailwind 4 + shadcn-style components. `call` is a Tauri command; `view` streams over `tauri::ipc::Channel<ViewEvent>`. Depend on `@pierre/diffs` and `@pierre/trees` (Apache-2.0, used by t3code, agent-orchestrator and Intent). Lift agent-orchestrator's decoupled `packages/product-ui` board (Apache-2.0) and t3code's work-log, approval panels, changed-files tree and keybindings (MIT). Intent's Svelte UI isn't liftable; copy its mock/live client seam. Ours to build: the person × agent × issue board model, overlap alerts, the signed approvals inbox, the timeline scrubber, trust and identity screens |
| GUI shell and timing | Tauri 2, with han-node embedded in-process. An early spike checks `@pierre/diffs` rendering in WebView2 and WebKitGTK. The MVP's face is han-tui; the GUI starts right after M3, once the node API is stable |
| pi adapter | evaluate both in the M4 spike: `pi-acp` 0.0.34 (ACP registry) and `@victor-software-house/pi-acp` 0.17 (used by agent-orchestrator); pick the one that works from a non-Zed client inside our sandbox |
| Borrowing from other apps | **clean room for all borrowed UI code, MIT and Apache alike**: no source is copied. A spec writer studies the apps and writes behaviour and UI specs (layouts, states, interactions, data shapes); separate implementers build from those specs. Published packages remain normal dependencies (for example `@pierre/diffs`, `@pierre/trees`), and HAN's own repo stays Apache-2.0 OR MIT throughout |
| Model credentials | the egress proxy injects the real API key for the model provider's host only; agents run with a dummy key and never see the secret |
| Devices and recovery | one actor per person; each device has its own key, added to the profile with `key.add` by an existing device, and a lost device is revoked from another. Recovery is a stored recovery key, or co-owners re-admit you |
| Onboarding | both: an invite ticket (`han invite` gives the sequencer's node id, the space id and a one-time secret; `han join <ticket>` dials by key, and an owner's `member.set` admits), plus GitHub org or team membership as an automatic admission rule for a space (gist-nonce proof) |
| Default approver | the agent's operator approves its gated effects; spaces add rules (for example two approvals for releases, or any owner for main) |
| Retention | per-space policy. The log (authority) is kept forever as the audit trail. Session streams and fine-grained timeline history are compacted after a window (default 90 days); merged work keeps its merge. Owners extend it or keep everything |
| Secrets in session data | both: paths matching a per-space exclusion list (.env, *.pem, ~/.ssh…) are never mirrored or streamed, and han-host scans stream items and timeline ops (gitleaks-style rules plus the proxy's known keys) and masks matches before anything leaves the device. A miss can still be redacted later |
| Space home | the space's creator chooses: the shared runner or their own machine. It can move later (§7.3) |
| Updates | signed self-update on request (or nightly if enabled). Peers exchange versions in hello, and a node refuses to join a space whose protocol version is newer than it supports, telling the user to update. Installers via cargo-dist (MSI, Homebrew, shell) |
| Space scope | one space per team with its repos inside; the board filters by repo. Rights and approval rules are per space, with per-repo overrides |
| Worktrees | han-host makes a git worktree and branch (`han/<issue>-<agent>`) per session from the repo's default branch, and records the base sha. The worktree is the sandbox's writable area and the timeline's root; it is cleaned up after merge or cancel |
| Cost tracking | the egress proxy counts tokens and cost per session from provider responses; the board shows them, and spaces can set budgets through Policy's budget ledger |
| `team.read` and summaries | layered, cheapest first: (0) structured status from the host, free; (1) a required `summary` argument on HAN tool calls; (2) a turn-end summary made by the proxy re-sending the session's last request plus a summarise instruction **on the same model**, so about 90% of the input is a prompt-cache hit (a smaller model can't reuse another model's cache); (3) detail on request: transcript windows or timeline diffs. `team.read(session, budget_tokens)` fills the reader's budget layer by layer. Layer 2 runs only at turn end while the cache is warm, only for sessions someone follows or another agent read recently, and at most once per N minutes. Summaries are data, never instructions |
| Who pays | summaries count against the summarised session's budget; detail reads (layer 3) count against the reader's session |
| Proxy TLS | model calls: the harness's base URL points to the local proxy inside the sandbox, which injects the key and forwards over TLS (no interception). Other egress: CONNECT tunnels allowed by hostname (SNI) against the allow-list, never decrypted. No fake CA |
| Proxy upstream | optional per runner or per space: an HTTP or SOCKS5 proxy, or the system route (for example a tailnet exit node for a residential IP). HAN ships a hardened Tailscale recipe and no Tailscale code: runners tagged `tag:han-runner`, access rules allowing only `autogroup:internet` via the exit node, a dedicated exit node on a guest network, ideally a separate tailnet from personal devices, and ephemeral tagged auth keys. Note: check each model provider's terms before routing through residential IPs |
| Network topology | HAN traffic between nodes is iroh only. The agent sandbox reaches only the local proxy. Tailscale, if used, is only the proxy's upstream |
| Peer filter | iroh's `IncomingFilter` drops any node ID that isn't a registered device of a member of a space this node serves, before the handshake completes. Invite tickets admit a joining device once |
| Overlap response | alert both agents (as data, naming the other session and the overlapping files or ranges) and highlight it on the board. The newer claim is asked to coordinate or avoid the area; humans decide if it persists. Nothing is blocked automatically |
| Approval surfaces | the TUI inbox and OS notifications in the MVP; a chat notifier (Matrix or Slack) links to a HAN face, since approvals must be signed; a phone face with push notifications later |
| Agent tests | a fake ACP agent replays recorded sessions deterministically in CI, and a scripted agent covers edge cases. A nightly job runs pi for real against a test repo, under a hard token budget |
| Clean-room enforcement | strict for both han-timeline and borrowed UI. Team B agents work in a separate checkout holding only specs and vectors, with permission deny rules on reads of the study's materials, the studied product, the evidence and other apps' clones; prompts name only the spec, and every session's tool log is kept as provenance. Sessions that have read the sources (including this design session) never write Team B code |
| Normative tiers | the core standard is the protocol (log, Policy, `han/1` framing). Product contracts (`han.status/1`, `han.timeline/1`, `team.*` tools) are versioned profiles in `spec/profiles/`, normative for interoperability between HAN apps but able to evolve faster. The node API is an implementation API, documented but not standardised |
| Naming | keep HAN and the `han` binary; before the first release, check crates.io, npm, Homebrew and trademarks, and fall back to `han-*` names where taken |
| A2A questions (Next) | a question is a signed `message` addressed to a session. The target's host shows it to the agent as data between turns, never mid-turn; the answer comes back as data; its human sees the exchange and can step in |
| A2A limits | configurable per space. Defaults: the asker's session pays the tokens; one open question per asker–target pair; an answer can't carry a question back (no ping-pong chains); a high rate cap (60 per hour per session) that spaces can raise, lower or remove |
| Board row | person · agent · harness; issue · state · step; changed files or hunks · overlap badge; tests · tokens or cost · age of the last activity |
| Board grouping | switchable by key (by issue, by person, by state as kanban columns); the choice is remembered per user |

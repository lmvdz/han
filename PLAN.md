# HAN implementation plan (Rust)

Status: draft, 2026-10-12. Decisions so far:
- One protocol over QUIC with dial-by-key; iroh is the implementation choice.
- Matrix becomes an integration, not a binding.
- Rust is the product implementation.
- One monorepo.
- A han-node daemon on every device; faces sit over its API.
- Integrations run as separate processes.

## 1. The MVP: team awareness

**A team of 2–4 people, each running several Claude Code agents. Everyone, agents included, sees live what every agent is working on, and can drill into any session. Nobody has to ask "what are you working on?"**

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
5. **Overlap alerts.** Two sessions on the same files or the same issue light up on the board and notify both agents.
6. **GitHub, one-way.** Issues become tasks; the board shows issue to agents.
7. **The approval gate is configured per space.** For example, push to `main`, releases and outside HTTP calls need a human; everything else inside the sandbox runs freely.

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
| Then | GUI (Tauri); Matrix notifier bot; Linear and Jira; other agent runtimes (Codex CLI, Gemini CLI, custom); more executor actions |
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
    han-host/      agent host; drivers/claude_code (later codex, gemini…)
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
trait Gate { async fn check(&self, call: &ToolCall, mode: Mode) -> GateVerdict; } // Allow | Propose(EffectPlan) | Deny
```

- The host owns the Gate, the R-24 duties (strict triggers, taint, naming the task) and the `StatusItem` derivation.
- A driver only runs the agent.
- `drivers/claude_code` runs `claude` in stream-JSON mode and routes permission prompts to the Gate, through a permission-prompt tool or a PreToolUse hook. Which one is settled by the M4 spike.

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
| Claude Code driver | **build** `drivers/claude_code` on the CLI: `-p --bare`, stream-JSON in and out, `--session-id`, `--strict-mcp-config`, and a **PreToolUse hook** passed in `--settings` with `onFailure: "block"` (CLI ≥ 2.1.295) | the hook runs before every rule and mode, and its deny wins even in bypass mode. `defer` parks the turn, so Propose = park, approve, `--resume`. Parallel tool calls fall back to deny with a reason. `--bare` stops the repo from configuring the agent. Also read `claude agents --json` and the hook events instead of re-deriving them |
| Other agents | **adopt** ACP: `agent-client-protocol` 3.3 (pinned) as `drivers/acp` | covers Codex, Gemini CLI, Copilot CLI, goose, OpenCode… ACP permission requests are optional, so each driver declares its gate strength (`every_call` / `prompt_only`), and `Mode::Strict` refuses `prompt_only`. On ACP, Propose is a han-mcp tool and the raw action is denied |
| Agent tools | **adopt** `rmcp` 3.5 (pinned) | the MCP 2026-07-28 spec is stateless, so the `team.*` tools take handles as arguments. A required `summary` argument on each call feeds `StatusItem.summary` |
| Sandbox | **integrate** Microsoft MXC (`mxc-sdk` 1.0, Rust, MIT: Windows 11 without admin, Linux bubblewrap, macOS seatbelt) + a HAN-owned egress proxy for domain allow-lists | Claude Code's own sandbox covers shell commands only and does nothing on native Windows, so we confine the whole agent process. Fallbacks: WSL2 or Podman. Use `landlock` to harden han-exec |
| git and GitHub | **adopt** octocrab 0.54; shell out to `git` for push; gix for reads | gix still can't push. A push is desired state: "move ref R from A to B", so a retry is safe |
| Executor ledger | **build** a small SQLite ledger keyed by idempotency key | "unknown is not success"; invocation ids are never reused (from intentd). The job frameworks don't fit |
| Overlap detection | **build**, in three tiers | 1. claims: advisory leases on issues and paths, with a TTL; a conflict reports who holds it and doesn't block (Agent Mail, intentd). 2. touched hunks: range intersection on `files[].hunks`. 3. real conflicts: a `gix merge-tree` merge simulation between session branches (the Clash technique). Alerts go to both agents and to the board |
| Work timelines (CRDT) | **defer** | the MVP uses `git diff -U0` hunks. If dogfooding needs live work timelines, add a Loro 1.x extension (frontiers, `fork_at`, `checkout`, `diff`) with HAN-signed delta envelopes. the reference model stays the design reference and is never shipped: the reference model comes from a closed product, so nothing derived from the study enters the HAN repo |
| Agent-to-agent (Next) | **integrate later** Google A2A v1 (`a2a-lf`) as a face over the node API | signed Agent Cards fit Ed25519 profiles. A message to another agent is **data**, never injected input (intentd and Agent Relay inject it, which R-24 forbids) |
| Existing tools | **bridges later** | `integrations/han-intentd` and Copilot or Agent HQ sessions as board rows, if they have an API |

**Strategic note.** Claude Code's single-user features (agent view, cross-session messaging, hooks) overlap the MVP for one person. HAN's edge is the team: several machines and people, several agent vendors, self-hosting, and verifiable approvals. Keep han-host consuming Claude Code's signals rather than competing with them.

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
| M4 | han-host with the Claude Code driver; han-mcp; han-exec with `git_push` | **first week, a spike that answers:**<br>• does PreToolUse fire for tool calls inside subagents?<br>• do hooks given in `--settings` load under `--bare`?<br>• does `defer` and resume work?<br>• does an ACP reject carry a reason back to the agent?<br>• is MXC mature enough on Windows 11?<br>• do Claude Code's terms allow a third-party host to drive it?<br>Then a real session streams status, an agent answers "who is on auth/?" from the tools, and a gated push waits for approval and runs exactly once |
| M5 | han-tui (board, drill-down, approvals inbox); han-github (one-way) | the full MVP story works across two machines |
| M6 | Team dogfood | 2–4 people, two weeks; issues feed the spec |

M0's spec round and M1 run in parallel: the §8 rewrite doesn't touch Policy.

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
| The `claude` CLI interface changes | spike first in M4; the driver trait keeps the blast radius to one module |
| Sandboxing on Windows | MXC first; WSL2 or podman (the existing WSL setup) as the fallback; node, TUI and exec stay native |
| Status leaking through gossip | decided: status flows only to readers through the node; gossip carries presence only, signed, on a topic only readers know |
| Claude Code ships team features | han-host consumes them; HAN's edge is multi-vendor, self-hosted and verifiable approvals |
| Pinned crates churning (ACP, rmcp ship major versions often) | pin exact versions; each sits behind one trait (`AgentDriver`, han-mcp) |
| Reliance on n0's relays | self-host `iroh-relay` for the team |
| Interop witnesses drifting | frozen; they only need to pass the vectors and run the interop harness |

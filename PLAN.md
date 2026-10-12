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
{ session, agent, issue?: "github:owner/repo#123", repo?: "github:owner/repo",
  state: "working" | "waiting-approval" | "blocked" | "idle" | "done",
  step: string, tool?: string, files: [repo-relative path], tests?: {pass, fail},
  summary?: string, at }
```

This is the one schema the board, the overlap alerts, the agent tools and the GitHub sync all read.

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
| M4 | han-host with the Claude Code driver; han-mcp; han-exec with `git_push` | **first week: a spike of the `claude` stream-JSON and permission path**; then a real session streams status, an agent answers "who is on auth/?" from the tools, and a gated push waits for approval and runs exactly once |
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
| Sandboxing on Windows | run `han host` in WSL or podman (the existing WSL setup); node, TUI and exec stay native |
| Status leaking through gossip | decided in round 30; the default is that status flows only to readers |
| Reliance on n0's relays | self-host `iroh-relay` for the team |
| Interop witnesses drifting | frozen; they only need to pass the vectors and run the interop harness |

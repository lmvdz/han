# Research: intent-hq/intentd (and the Intent project) for HAN

Date: 2026-10-12. Researcher: Claude subagent. Sources: the source code (cloned at commit `46a8bae`,
2026-10-12) of https://github.com/intent-hq/intentd and the docs of the monorepo
https://github.com/intent-hq/intent (shallow clone, `docs/`). Nothing was built or run.
Clones: `H:\agentic\temp\claude\H--HAN\5c1257e4-8c11-4fbf-80b1-3b3209de9c9f\scratchpad\research\intentd` and `...\research\intent-mono`.

Legend: **[V]** = verified in source or by API call; **[D]** = stated in the project's own docs and not checked against
the code; **[3P]** = a third-party claim; **[?]** = could not determine.

---

## 0. Verdict

**Learn from it. Do not reuse or embed it. A narrow integration is optional and comes later.**

- intentd solves the same *surface* problem as HAN's MVP: many coding agents, live status, agents reading each other,
  and approvals. Its trust model is the opposite of HAN's:
  - one host daemon is the single source of truth;
  - access works through bearer tokens and GitHub-proved identity;
  - nothing is signed or hash-chained, and nothing can be replayed;
  - agent permissions default to AllowAll;
  - agents message each other as instructions.
- It is not reusable as a library:
  - about 1.17M lines of Rust;
  - its crates are internal (`publish = false`) and pull in forked dependencies;
  - its protocol churns (doc version 14.0 after about 4 months).
- Several of its patterns fit HAN's `han-node` views, `han-mcp`, overlap detection and `han-exec` closely. These are worth copying (section 6).
- One of its dependencies matters more than intentd itself: **ACP** (Agent Client Protocol, the `agent-client-protocol` crate plus the
  `@agentclientprotocol/claude-agent-acp` adapter). HAN's M4 spike of `han-host` should evaluate it.

---

## 1. What it is, and the problem it solves

- **What it is.** intentd is "a local-first, headless Rust daemon that owns the Intent domain model - workspaces,
  notes, tasks, comments, agents, git, pull requests, scripts, terminals, files, and events - and exposes it over a
  JSON-RPC 2.0 API." Clients are thin; all logic lives in the daemon. [D: intentd README]
- **The problem.** It coordinates many coding agents at once (Claude Code, Codex, auggie, opencode, droid, grok, pi and others):
  - isolated workspaces (git worktrees or copy-on-write checkouts);
  - a "Coordinator" agent that writes a spec and delegates tasks;
  - git, PR and review flows;
  - a desktop app (Electron + SvelteKit, `intent-hq/cloudlands-fe`) and an iOS app as faces.

  [D: monorepo README, https://intentapp.dev]
- **Lineage.**
  - NOTICE: "Intent originated as 'Intent by Augment'." [V: `NOTICE`]
  - The Rust backend is a port of Augment's TypeScript/Electron app. The port "completed on 2026-07-13" and "was built entirely
    with the reference app (`augmentcode/intent`)"; development has since been self-hosted on the Intent stack.
    [D: `docs/README.md`]
  - A third party reports that augmentcode.com/intent now redirects to intentapp.dev (June 2026) and lists Augment's
    Intent as closed source. [3P: https://rywalker.com/research/augment-intent]
  - Augment's product framing (spec-driven, a coordinator agent, worktrees): https://www.augmentcode.com/blog/intent-a-workspace-for-agent-orchestration [3P/vendor]
  - **[?]** The exact corporate relationship between intent-hq and Augment Code is not verified.

## 2. Architecture

### Language, layout and size [V]
- Rust 2021 (MSRV 1.88). One cargo workspace with 20 crates under `crates/`. The README says 12, which is stale.
- Main crates:
  - `intent-core` (the leaf: domain types, the `WorkspaceApi` trait);
  - `intent-store` (SQLite through `sqlx`, 157 migrations);
  - `intent-services` (business logic, about 558k lines including tests);
  - `intent-transport` (UDS/named pipe plus a WSS/TLS listener and the JSON-RPC router);
  - `intent-acp` (ACP client, agent manager, the agent-to-backend MCP server);
  - `intent-providers`, `intent-git` (libgit2, worktrees, CoW reflink), `intent-sourcecontrol` (octocrab);
  - `intent-pty`, `intent-search`, `intent-js` (QuickJS);
  - `intent-linear`, `intent-sentry`, `intent-voice`;
  - `intent-nodes` (a 117-line stub);
  - `intentd-sitter` (self-updating supervisor);
  - `intentd` (the binary and composition root, about 269k lines).
- Total is about **1.17M lines of Rust** across `crates/`, with about 11,300 `#[test]`/`#[tokio::test]` functions.
- Dependency direction is enforced: core is the leaf, and transport never imports store. Clippy pedantic runs at `-D warnings`. [V: Cargo.toml, README]
- Key dependencies:
  - tokio, sqlx/SQLite, `agent-client-protocol` 2.0 (latest on crates.io is 3.3.0), octocrab, git2, portable-pty, rustls, rquickjs;
  - **forked** `tokio-tungstenite`/`tungstenite` (intent-hq forks, for permessage-deflate). [V: Cargo.toml; crates.io API]

### Daemon model [V/D]
- There is one long-lived daemon per user or machine. Under it, a **sitter** shim supervises the daemon, auto-updates it from
  channel manifests (alpha/beta/stable), and respawns it on a crash. [D: README]
- Service install:
  - systemd user unit (Linux);
  - launchd/Homebrew (macOS);
  - a Scheduled Task (Windows).
- The desktop app can also run intentd as a sidecar. [D]
- It spawns agent CLIs as child processes over stdio using **ACP**. For Claude Code that is the npm adapter
  `@agentclientprotocol/claude-agent-acp`, pinned at 0.81.1 (npm latest is 0.89.1), launched via `npx`. Node.js is required.
  [V: `intent-providers/src/config.rs`; npm registry]
- A "head/node" split for remote execution is **designed but not implemented**. Head owns SQLite, transcripts, permissions
  and placement; nodes own the provider processes and checkouts. Head dials nodes over pinned TLS with an enrollment token,
  a lease, an incarnation, and a monotonic `linkGeneration` fence. The hello must match the exact version and build or
  the link fails closed. [D: `docs/ARCHITECTURE.md` "Phase 1 node execution (prepared architecture)", `docs/protocol/node-link.md`]

### Protocols [V/D]
- **Client API.** JSON-RPC 2.0, newline-delimited.
  - Local transport: a Unix socket (mode 0600) or a Windows named pipe `\\.\pipe\intentd-<hash16>`.
  - Optional WSS/TLS LAN listener: bearer token, origin allow-list, cert-fingerprint pinning, default port 5181.
  - Optional Tailscale reachability through a bundled `tailcat` sidecar.

  [D: README, `docs/protocol/01-transport.md`, `02-authentication.md`, settings.md; V: NOTICE bundles tailcat]
- **Size of the surface.** The method catalog is documented at "449 / 393 / 56" (dispatchable / router / fast-path)
  for protocol doc version 14.0. The README says "~143 request methods", which is older. [D: `docs/protocol/versioning.md`]
- **Live state.** There is an event firehose (`events.subscribe` -> `events.event`). There are also typed **snapshot+delta
  channels** (`note|task|workspace|comment|agent|chat.subscribe`, presence): a seq-0 snapshot followed by sequenced deltas.
  The documented invariant is "snapshot reduced with every delta equals a fresh full read." [D: `06-events.md` 6.9]
- **Agent-to-daemon.** An in-process MCP server is exposed to each agent on its own loopback TCP port, reached through an
  `intentd mcp-bridge` stdio-to-TCP proxy. **One MCP tool**, `workspace_api(code, summary)`, takes JavaScript that runs in
  QuickJS against a `ws.*` binding surface. All the discrete tools were removed ("WSAPI-8"). [V: `intent-acp/src/mcp_server/tools.rs`, `mcp_bridge.rs`]
  - **[?]** I found no authentication on the loopback bridge port beyond binding to 127.0.0.1. Attribution is by port.
    I did not trace this fully.

### Data model and storage [V]
- SQLite holds all durable state, with WAL. Tables include:
  - `workspace`, `note`, `task` (as notes), `comment`;
  - `agent_session`, `agent_message` (FTS), `agent_queue`;
  - `tracked_changes`, `event`, `hook`, `pr_monitor`, `script`;
  - `principal`, `workspace_member`, `principal_credential`;
  - `execution_node`/`node_lease`/`node_checkpoint` (foundations only).
- `event` is "append-only at the API layer": insert and query only. It has no signatures, no hash chain and no sequence
  authority (`0003_events.sql`).
- I found no Ed25519 or hash-chain primitives in the store.
- Identities are UUIDs. A `principal` is a person linked to a GitHub or GitLab account. Credentials are bearer tokens
  stored as SHA-256 (`0125_principals.sql`).
- Per-file, per-agent attribution: `tracked_changes` is keyed (workspace, path, stage, agent_id), so "agent B's touch no longer
  overwrites agent A's" (`0062_multi_agent_attribution.sql`).

## 3. How humans and agents use it

### Humans [D]
- Humans work in the desktop or iOS app, or the CLI (`intentd call <method>`, `status`, `doctor`, `settings`, `pair`).
- **Multiplayer (protocol 10.3 to 10.9):**
  - The owner's daemon is the host. Others are host members or workspace guests.
  - A guest joins with an `intent://invite` link and proves its forge identity: its *own* daemon publishes a host-issued nonce
    as a **secret GitHub gist** (or a public GitLab snippet). The host verifies the proof anonymously and mints a per-principal
    bearer token.
  - Roles are owner, member and guest. Revocation closes live sockets with code 1008.

  [D: `02-authentication.md` 2.4-2.5, `methods/multiplayer.md`]
- **Presence:**
  - an in-memory table keyed by (principal, connection) that is never persisted;
  - `focus` (which workspace, agent or note a person is looking at) and `typing` (which agent);
  - note carets coalesced to at most one update per 100 ms, with a trailing flush;
  - typing freshness by `(source, pulse)` with no wall-clock comparison.

  [D: `methods/presence.md`]
- **Status:** derived per workspace as `activity` (green dot), `attention` (blue dot) and `waiting`. Agent rows carry
  `isWaitingOnTool`, `waitingForAgentIds`, `waitingOnPrMonitors`, `lastToolUse`, `digest`, `stopReason`, `contextUsage` and
  similar fields. [V: `AGENT_LIST_ROW_KEYS` in `intent-core/src/model.rs`]

### Agents (through `ws.*` inside `workspace_api`) [V: tools.rs; D: `methods/mcp-bindings.md`, generated and CI-checked]
- **Agent-to-agent:**
  - `ws.agent.list/status/summary/readConversation` (includes the partial in-flight turn; slim projection with stable
    block ids, hydrated through `getMessageBlock`);
  - `ws.agent.send` (**interrupts the target mid-response by default** and delivers the message);
  - `ws.agent.watch/subscribe`, `delegate`, `reportToParent`, `reportBlocker`, `requestDiscussion`.
- **Tasks:**
  - `dependsOn` / `conflictsWith` relations;
  - batch `delegate` admits tasks in critical-path order and holds tasks whose `conflictsWith` closure overlaps the running set (`held:conflict`).
- **PR monitor:** one owner per PR per workspace. A second registration is **refused** (not an error) with
  `ownerAgentId` plus an instruction. Orphaned monitors are adopted, and a parent takes over from a settled child.
- **Other namespaces:** notes, files, git commit, hooks (scheduled JS), scripts, terminals, browser, MCP passthrough,
  `ws.host.exec` (feature-gated), and cross-workspace reads.
- **Tool descriptions:** Claude Code is flagged `truncates_tool_descriptions` (~2k chars, citing
  anthropics/claude-code#53933, now closed), so the full API reference goes into the system prompt. [V]

### Permissions [V]
- ACP `session/request_permission` is mediated by the daemon. The policy is chosen by the `INTENTD_PERMISSION_POLICY` env var.
- **The default when unset is `AllowAll`.** The daemon also sends `session/set_mode bypassPermissions` where the provider
  supports it. `interactive` surfaces prompts through `agent.pendingPermissions` / `agent.respondPermission`.
- An unanswered prompt times out after 5 minutes and resolves as `cancelled`.
- `riskLevel` is a heuristic based on the title text. [V: `intentd/src/main.rs` `parse_permission_policy`; D: `08-permission-flow.md`]
- Orchestrator agents have `Edit`, `Write` and `NotebookEdit` disallowed for Claude Code (`tool_restrictions.rs`). [V]
- Git push and PR creation by agents go through `gh` and git in the shell. An explicit head-owned `hub.publish` is
  planned. "Never merge without human permission" is AGENTS.md doctrine, not enforcement. [D]

## 4. License, maturity and activity (as of 2026-10-12)

| | intentd | intent (monorepo) |
|---|---|---|
| License | Apache-2.0 [V] | Apache-2.0 [V] |
| Created | 2026-06-17 [V] | 2026-06-17 [V] |
| Stars / forks | 12 / 6 [V] | 305 / 25 [V] |
| Commits | 2,181 on main; 516 in the last 30 days [V] | n/a |
| Merged PRs | 2,174 [V] | 2,055 issues total [V] |
| Contributors | 8 humans plus bots; **Clement Pang has about 2,000 of 2,181 commits (about 92%)** [V] | |
| Releases | 30+ tags; latest `v0.10.43` (prerelease, 2026-10-11); `latestRelease` (stable) `v0.10.33`, 2026-10-08 [V] | |
| crates.io | not published (`publish = false`); the name `intent-core` on crates.io belongs to an unrelated project [V] | |

- **Maturity:** high velocity, heavy tests and strict linting, but pre-1.0 and churning:
  - 157 migrations in about 4 months;
  - protocol doc version 14.0;
  - many sections marked "prepared contract, not implemented."
- **Bus factor:** effectively one maintainer.
- **Platforms:** Windows is supported for the daemon (named pipe, Scheduled Task, `install.ps1`), but "Windows
  qualification" is out of scope for the node phase. [D]
- The org's other public repos are `cloudlands-fe`, `cloudlands-releases`, `intentd-releases`, `homebrew-tap`, the tungstenite
  forks and `textual` (Swift). The `ios` submodule is not public. [V: `gh repo list`]

## 5. Docs quality and API surface

- **Volume.** About 10k lines of architecture and protocol docs plus about 19k lines of per-method docs.
  The MCP binding index is generated from source and checked in CI.
  Error codes, versioning, event taxonomy and a thin-client guide are all specified. [V: wc]
- **Quality.**
  - Precise and normative: authorization edge cases, ordering and backpressure are all spelled out.
  - Very dense, with very long paragraphs, and plainly agent-written.
  - Implemented and "prepared" (future) contracts are mixed in the same files, so it is hard to tell what ships.
  - Some top-level docs are stale: the README says 12 crates, ~143 methods, and "this repository is private."
- **API surface.** A very large RPC catalog across about 30 namespaces, one MCP tool with about 150 `ws.*` bindings, and a CLI.
  Nothing is offered as a Rust library API.

## 6. Mapping to HAN (PLAN.md crates)

### Replace or embed: **none**
- **han-core, han-seq:** intentd has no counterpart. There are no signed entries, pure Policy, hash chain, receipts or replay.
- **han-net:** intentd uses WSS + bearer + TLS pinning + Tailscale, not dial-by-key QUIC.
- **han-node:** the closest analogue. intentd is a daemon that owns state, serves JSON-RPC over UDS or a named pipe, and
  maintains views. But it is a monolith (intent-services is about 558k lines), its crates are internal, it carries forked
  WebSocket dependencies, and its protocol churns fast. Embedding it would import its whole domain (notes, workspaces, CoW,
  hooks) and its trust model.
- **han-host:** do not embed `intent-acp`. Do evaluate **the ACP dependency it uses** (`agent-client-protocol` crate,
  Apache-2.0, v3.3.0; `claude-agent-acp` adapter, Apache-2.0) as the `drivers/` layer in the **M4 spike**, next to raw
  `claude` stream-JSON. ACP gives:
  - a typed `session/request_permission` hook, which is a third option beside the permission-prompt tool and PreToolUse;
  - cancel, resume and load;
  - Codex, Gemini and others through the same protocol, which matches the "Then: other agent runtimes" horizon.

  Its costs:
  - a Node/npx runtime dependency;
  - adapter lag (intentd itself trails by 8 minor versions);
  - another layer between HAN and `claude`.
- **han-exec, han-mcp, han-tui:** nothing reusable as code.

### Integrate (optional, after the MVP)
- An **`integrations/han-intentd`** process: an actor over the node API that attaches to a member's local intentd
  over UDS or the named pipe and subscribes to the `agent.subscribe {workspaceId}` collection channel. It maps `AgentLite`
  rows (`lastToolUse`, `isWaitingOnTool`, `stopReason`, ...) and `changes:tracked` to `han.status/1` StatusItems, so
  Intent-driven agents show up on the HAN board and in overlaps.
  - It is read-only.
  - It never forwards anything into intentd as instruction.
  - Value: medium. Teams that already use Intent could join HAN without switching their agent runtime.
- intentd's `/invite` gist-nonce proof is not an integration point. It is a pattern (see L8).

### Learn from (ranked by impact on the MVP)

**L1. A single owner per claim, with refusal that names the owner, plus orphan adoption -> overlaps.**
- **Mechanism:**
  - A claim on an issue or path returns `{refused, owner, ownerSession, instruction}` instead of an error.
  - A claim whose owner session is dead or settled is *adopted*.
  - Task relations `conflictsWith` / `dependsOn` hold conflicting work rather than racing it.
- **Applies to:** the `team.who_is_on` and `team.overlaps` answers in `han-mcp`, and the Overlaps view in `han-node`.
  Overlap alerts can include "who owns it and what to do".
- **Constraint:** keep it advisory, a non-authority stream item (Rule 6.1), not a Policy right.
- **Build vs buy:** borrow the pattern.

**L2. Per-agent, per-file attribution rows.**
- **Mechanism:** one row per (session, path, stage), never overwritten by another agent's touch. The overlap query is a
  self-join on path.
- **Applies to:** the overlaps view in `han-node`, fed from `AgentEvent::FileTouched` in `han-host` and `StatusItem.files`.
- **Build vs buy:** borrow the pattern.

**L3. The snapshot+delta channel invariant as a testable property.**
- **Mechanism:**
  - seq-0 snapshot plus sequenced deltas;
  - "snapshot + deltas == fresh read";
  - self-sufficient delta payloads;
  - list rows projected to a fixed key allowlist and a byte budget, with detail fetched separately;
  - lossless conflation under writer backpressure.
- **Applies to:** `han-node` `view(Query) -> Snapshot then Diffs`. Make the invariant a property in the M3 turmoil
  simulation, and cap Board rows by bytes.
- **Build vs buy:** borrow the pattern.

**L4. Reading another session: a slim projection with stable block ids.**
- **Mechanism:**
  - Truncate large tool and image bodies but keep their ids, and hydrate one on demand.
  - Include the partial in-flight turn, marked `inProgress`.
- **Applies to:** `team.read(session)` in `han-mcp`, and drill-down in `han-tui`.
- **Build vs buy:** borrow the pattern.

**L5. A required `summary` argument on every agent tool call.**
- **Mechanism:** the model writes a one-line "what this call does" on every tool call, at no extra turn cost. intentd shows it in the UI.
- **Applies to:** a cheap feed for `StatusItem.summary` beside the status the host derives itself (`han-host`), and the `han-mcp` tool schemas.
- **Build vs buy:** borrow the pattern.

**L6. Presence semantics.**
- **Mechanism:**
  - rows per connection, aggregated per person;
  - freshness by `(source, pulse)` counters instead of wall clocks;
  - coalesce to at most one update per 100 ms per key, with a trailing flush;
  - re-check authorization when the deferred flush fires.
- **Applies to:** the presence and status gossip in `han-net`/`han-node` (the round-30 decision on gossip vs reader fan-out).
- **Build vs buy:** borrow the pattern.

**L7. Durable command evidence and "unknown is not success".**
- **Mechanism (`intentd command-run`):**
  - `result.json` is written only after the OS exit status has been observed;
  - a missing or partial record stays `unknown`;
  - the directory is tombstoned to prevent invocation-id reuse;
  - a PID recovered from disk is never signalled.
- **Applies to:** `han-exec` actions that run processes. It complements X-02 to X-06.
- **Build vs buy:** borrow the pattern.

**L8. Forge-identity proof by a published nonce.**
- **Mechanism:** the host issues a nonce; the joiner's own tooling publishes it as a secret gist; the host verifies it
  anonymously. No OAuth runs on the joiner's behalf.
- **Applies to:** binding a HAN actor key or profile to a GitHub login, for the board's "person" column and for
  `han-github` mapping. The proof would be recorded as profile data, not as authority.
- **Build vs buy:** borrow the pattern.

**L9. Harness versioning.**
- **Mechanism:** pin the prompt "doctrine" and feature toggles at session creation (`harnessVersion`, `harnessFeatures`),
  so a daemon upgrade never changes what a running session runs with.
- **Applies to:** `han-host` (the strict-mode prompt and gate config of R-24) and the `han-mcp` tool descriptions.
- **Build vs buy:** borrow the pattern.

**L10. Link fencing for a future remote host.**
- **Mechanism:**
  - lease + incarnation + monotonic `linkGeneration`;
  - an exact-version hello that fails closed;
  - fsync the journal before forwarding, then an ack watermark;
  - `rpc-outcome-unknown` for a lost response to a mutation without an idempotency key.
- **Applies to:** the outbox and supervisor in `han-node`, and the "Later: homes and moves" horizon. HAN's design is
  already stricter; use this as a cross-check.

**L11. Practical note: tool-description length.**
- Claude Code once truncated MCP tool descriptions at about 2k characters. The issue is now closed and may be fixed.
- Keep `han-mcp` descriptions short and put the reference text elsewhere.

### Skip
- **Code-mode single tool** (`workspace_api` running JS in QuickJS):
  - powerful, and it saves tool-schema tokens;
  - but it blurs the data/instruction boundary, makes per-call gating and auditing coarse, and adds a JS runtime.
  - Four typed `team.*` tools are the right MVP.
- **Default AllowAll, bearer-token host authority, and host-centric multiplayer.** These conflict with HAN (section 7).
- **The CoW sandboxes, notes, hooks and spec editor domain.** Out of scope for HAN.

## 7. Where it conflicts with HAN

| Area | intentd | HAN | Consequence |
|---|---|---|---|
| Trust | The host daemon is the sole authority. Rights are rows in mutable SQLite. The owner holds a legacy admin token. [V] | A sequencer orders signed entries; a pure Policy decides; any member verifies by replay. | intentd's authority is unverifiable by design. Nothing of its authorization layer can be reused. |
| Log | `event` is append-only only at the API layer: unsigned, no hash chain, and most state lives in mutable tables (157 migrations). [V] | A hash-chained log of signed receipts. | There is no common ground for a shared log. |
| Topology | Hub and spoke. Clients and guests connect to one host daemon over WSS (or Tailscale). Agents of all members run under that host. Remote nodes are dialed by head. [D] | Every device runs han-node; peers dial by key over iroh; each person's agents run on their own device; the team shares a *space*. | Cross-silo work, where each person keeps their own agents and keys, is not intentd's model. |
| Identity | A UUID principal, linked to a GitHub or GitLab account through a gist or snippet proof, using bearer tokens. [D] | Ed25519 actor keys (later key-derived IDs); keys never leave han-node. | Reuse only the proof *pattern* (L8). |
| Approvals | Permission prompts are ephemeral (5-minute timeout -> cancelled). **Default AllowAll.** Push happens through the agent's shell. No exactly-once executor. [V] | A per-space approval gate; `effect.propose`, `approve`, `claim`, `outcome`; an executor with journal rules X-01 to X-12. | HAN's gate is its core differentiator, and intentd offers no equivalent. |
| Agent-to-agent | `ws.agent.send` **interrupts the target and injects the message as input** by default. `readConversation` content feeds turns freely. [V] | R-24: another session's stream or context is *data, never instruction*. Only a `session.input` or `task` with a receipt drives a turn. | Copy the read path (L4); never copy the send semantics. |
| Status transport | Presence and status sit in the host's memory and reach clients over WSS. | A `han.status/1` stream item; gossip vs reader fan-out is still open (round 30). | Patterns transfer (L6); the transport does not. |

## 8. Related projects with the same idea (brief; READMEs and metadata only, source not read)

- **Augment "Intent"**: the closed-source ancestor. It is spec-driven, with a coordinator and worktrees.
  https://www.augmentcode.com/blog/intent-a-workspace-for-agent-orchestration [vendor]
- **MCP Agent Mail** (Dicklesworthstone):
  - agent identities, inboxes, threads, and **advisory file leases** with an optional pre-commit guard, over FastMCP + Git + SQLite;
  - 2,203 stars; license "Other"; pushed 2026-10-08;
  - Rust port `mcp_agent_mail_rust`: 192 stars, pushed 2026-10-12. [V: gh metadata; 3P summaries]
  - It is the closest prior art for HAN's overlap alerts (advisory leases plus a commit-time guard).
  - https://github.com/Dicklesworthstone/mcp_agent_mail
- **Clay, ClauBoard, Claude Squad, and claude-agents-dashboard**: team-shared Claude Code GUIs and boards.
  These are listing-level claims only and are not verified. [3P]

## 9. Gaps and unverified items
- **[?]** The corporate relationship between intent-hq and Augment Code (NOTICE states lineage only).
- **[?]** Whether the loopback MCP bridge has any authentication beyond 127.0.0.1 and per-agent ports.
- **[?]** How much of the multiplayer/shared-host (10.9) and node contracts is implemented at the current pin. The docs say
  node phase 1 is "not implemented". The multiplayer migrations exist (`0125`-`0134`), but I did not trace end-to-end behaviour.
- **[?]** Whether Claude Code still truncates tool descriptions (#53933 is closed).
- **[?]** Real-world adoption beyond the star counts. I found no independent reviews of the open-source Rust stack.
- I did not run or build anything, as instructed.

## Sources
- https://github.com/intent-hq/intentd (source at 46a8bae; README; NOTICE; Cargo.toml; crates/*)
- https://github.com/intent-hq/intent (docs/ARCHITECTURE.md, docs/HARNESS.md, docs/README.md, docs/protocol/*)
- https://intentapp.dev
- https://www.augmentcode.com/blog/intent-a-workspace-for-agent-orchestration
- https://rywalker.com/research/augment-intent
- https://crates.io/crates/agent-client-protocol ; https://www.npmjs.com/package/@agentclientprotocol/claude-agent-acp
- https://github.com/anthropics/claude-code/issues/53933
- https://github.com/Dicklesworthstone/mcp_agent_mail ; https://github.com/Dicklesworthstone/mcp_agent_mail_rust

# Research: coordination and observation tools for teams of AI coding agents

Date of survey: 2026-10-12. Repo metrics come from the GitHub API on that date: stars, `pushed_at` (last push) and the latest release. Claude Code facts come from the current docs at code.claude.com (fetched 2026-10-12; the docs reference CLI versions up to v2.1.283). Anything not checked against a primary source is marked **[unverified]**.

Context: HAN MVP (H:\HAN\v1\PLAN.md). 2–4 people, several Claude Code agents each. A live board of agent, issue and status; MCP tools `team.sessions` / `who_is_on` / `read` / `overlaps`; overlap alerts; a per-space approval gate. Built in Rust: han-node daemon, han-host (AgentDriver), han-exec, han-mcp, han-tui and han-github.

---

## 0. Answer first

1. **Nothing found gives live, multi-person, cross-machine visibility of every agent session in a team, self-hosted and vendor-neutral.** Every strong tool stops at one of three boundaries:
   - **One user.** Claude Code agent view, cross-session messaging and Remote Control; Happy; Claude Squad, ccmanager, Conductor, Nimbalyst.
   - **One machine or one server.** MCP Agent Mail; Gas Town; the hooks dashboards.
   - **A hosted engine.** Agent Relay; GitHub Agent HQ; Codex cloud; Devin.

   Agent Relay is the nearest competitor, followed by Claude Code's own single-user features.
2. **The biggest finding: Claude Code now ships, for a single user, much of what HAN's MVP describes.** All of this is verified from the docs:
   - **Agent view** (`claude agents`), with a supervisor daemon and a stable JSON state read: `claude agents --json`.
   - **Cross-session messaging**, with a per-session inbox socket or named pipe, `ListAgents`, `SendMessage`, and inbound hold or refuse controls.
   - **Remote Control**, which reaches your own sessions on other machines through Anthropic servers.
   - **HTTP hooks** and **OTel events** that carry tool inputs.
   - **Agent teams**: experimental, one session, a file-based mailbox and task list.

   All of it is scoped to one claude.ai account. Shared cloud sessions are visible to an organization, but per the docs they "don't update in real time".

   HAN's lane is therefore:
   - several people;
   - several vendors' runtimes;
   - self-hosted;
   - derived status linked to issues;
   - overlap detection;
   - a verifiable, policy-gated approval.

   Build on Claude Code's surfaces; don't duplicate them.
3. **Open protocols to be compatible with, rather than inventing:**
   - **MCP 2026-07-28** for han-mcp. It is stateless, and the `rmcp` Rust SDK supports it.
   - **ACP (Agent Client Protocol) v1** as the generic AgentDriver for non-Claude runtimes. Codex, Gemini CLI, Copilot CLI, OpenCode, Factory Droid and others speak it, and there is an official Rust crate.
   - **A2A v1.0** for the "Next" horizon: agent-to-agent questions and tasks across organizations.
   - **OTLP** as an optional ingest path.
   - Linear's Agent Session model is a de facto schema worth mirroring for `han.status/1`.

---

## 1. Multi-agent coordination between coding agents

### 1.1 MCP Agent Mail (Jeffrey Emanuel / Dicklesworthstone)
- **What it does.** A mail-like coordination layer exposed as an MCP server:
  - memorable agent identities (for example `GreenCastle`);
  - inbox and outbox with threads and search;
  - **advisory file reservations (leases)** on globs, with a TTL and an exclusive or shared mode;
  - a **pre-commit guard** that blocks commits touching files another agent has reserved;
  - cross-project "contact handshake";
  - a TUI operator console, a web UI, and an `am robot` CLI for agents.
- **Repos.**
  - Python original: https://github.com/Dicklesworthstone/mcp_agent_mail. 2,203 stars, last push 2026-10-08. The README says it is **superseded and no longer maintained**.
  - Rust rewrite: https://github.com/Dicklesworthstone/mcp_agent_mail_rust. 192 stars, created 2026-02-05, v0.3.38 released 2026-10-08, very active. The README lists 45 tools and 25 resources.
- **Language.** Rust (nightly), on its own `asupersync` runtime ("no Tokio").
- **License.** "MIT License (with OpenAI/Anthropic Rider)". The rider restricts use by OpenAI, Anthropic, their affiliates, and anyone acting for them. **This is not an OSI-approved license.** Verified from the LICENSE file through the GitHub API.
- **Architecture.**
  - Transport: an HTTP MCP server on `127.0.0.1:8765/mcp/` with a bearer token or JWT.
  - Storage: "SQLite for live state, Git for the durable ledger". Messages, reservations and profiles are written as files to a per-project Git archive.
  - Reservations are **advisory**: acquiring one reports conflicts instead of waiting, and leases expire on their TTL.
  - Identities are "semi-persistent": they can vanish without breaking anything.
  - It pairs with **Beads** (`br`/`bd`) for tasks and **bv** for graph-based picking of the next task.
- **What it could do for HAN.**
  - **Learn from:** the lease model (TTL, exclusive or shared, report the conflict instead of blocking); the pre-commit guard as a last-line enforcement point; the Git ledger for human audit; the "macro" tools that bundle flows for weaker models.
  - **Don't depend on it:** the license rider and the single-server design. It also has no live session state, only messages and leases.

### 1.2 claude-flow, now "Ruflo" (ruvnet)
- **Repo.** https://github.com/ruvnet/ruflo (`ruvnet/claude-flow` redirects there). TypeScript, MIT, 74,433 stars, v3.56.9 released 2026-10-12.
- **What it says it does** (the project's own marketing): an "agent meta-harness for Claude Code and Codex", with "100+ specialized agents", "314 MCP tools", swarms, self-learning vector memory and "federation". It ships as Claude Code plugins (core, console, mods) and an MCP server.
- **Assessment.** The claims are broad and largely unverified. It is in-session orchestration, not team visibility. **Skip.**
### 1.3 Claude Code's own parallel-agent features (Anthropic; closed-source CLI)

The docs page https://code.claude.com/docs/en/agents lists five approaches: subagents, agent view, agent teams, dynamic workflows and Projects.

- **Agent teams** (experimental, off by default: `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`). Source: https://code.claude.com/docs/en/agent-teams
  - One lead plus teammates.
  - Each mailbox is a JSON file: `~/.claude/teams/{team}/inboxes/{agent}.json`.
  - Shared task list in `~/.claude/tasks/{team}/`, with dependencies; **claiming uses file locking**.
  - Hooks: `TeammateIdle`, `TaskCreated`, `TaskCompleted`.
  - Limits: "One team per session", no nested teams, and **teammates are not isolated in worktrees**. The docs say "Two teammates editing the same file leads to overwrites."
- **Agent view** (research preview). Source: https://code.claude.com/docs/en/agent-view
  - `claude agents` shows all *your* background sessions.
  - A **supervisor daemon** hosts them (`claude daemon status`; `~/.claude/daemon/roster.json`; `~/.claude/jobs/<id>/state.json`).
  - Each row has a one-line **Haiku-class summary**, updated at most every 15 s, plus PR status.
  - **`claude agents --json` is the supported machine interface.** Fields: `cwd, kind, startedAt, id, state (working|blocked|done|failed|stopped), pid, status (busy|waiting|idle), waitingFor (permission prompt|input needed|sandbox request|worker request|dialog open), sessionId, name`. The docs say the `jobs/` files are "not a stable interface".
  - Dispatched sessions move into their own worktree before editing.
- **Cross-session messaging** (CLI v2.1.224+, or v2.1.234+ on native Windows). Source: https://code.claude.com/docs/en/cross-session-messaging
  - Tools: `ListAgents` and `SendMessage`.
  - Same machine: a per-session Unix socket, or a named pipe on Windows. The path is in `CLAUDE_CODE_MESSAGING_SOCKET` and a token in `CLAUDE_CODE_MESSAGING_TOKEN`; on Windows the first line must be an auth line carrying the token.
  - Other machines and cloud sessions: **through Anthropic servers over Remote Control, same claude.ai account only**.
  - Messages are plain text, and a message "can't approve anything".
  - Inbound controls: `crossSessionInbound` = accept, hold or refuse. `isolatePeerMachines` requires approval before a message leaves the machine.
  - Scripts and hooks can post into their own session's socket; "own-child" messages are delivered.
- **Projects** (public beta, Pro/Max). Parallel cloud threads. The docs say: "A project belongs to one user. You can't share a project or its threads with another user."
- **Assessment.** For a single user this is the strongest product. **Integrate; don't compete.** HAN's value is everything that crosses accounts.

### 1.4 OpenAI Codex: CLI, app-server and cloud
- **Repo.** https://github.com/openai/codex. Rust, Apache-2.0, 128,833 stars, active.
- **App-server.** Source: https://learn.chatgpt.com/docs/app-server
  - JSON-RPC 2.0 over stdio as JSONL; WebSocket is experimental.
  - Concepts: Thread, Turn and Item.
  - Methods: `thread/start|resume|fork`, `turn/start|steer|interrupt`.
  - Notifications: `item/started`, `item/completed`, `thread/status/changed`, `turn/completed`.
  - Approval requests such as `item/permissions/requestApproval`.
  - The docs say it is for embedding in a product UI; automation should use the Codex SDK.
- **Codex cloud.** Source: https://learn.chatgpt.com/docs/cloud
  - Each task gets its own cloud workspace. Environments have access settings such as "Everyone at Acme".
  - **Not stated in the docs:** whether tasks are visible to teammates, and whether there is a team dashboard.
- **For HAN.**
  - Later Codex driver: ACP through `codex-acp`, or app-server directly. App-server's `turn/steer` maps onto "steering".
  - Codex cloud: skip.

### 1.5 Google A2A protocol
- **Repo.** https://github.com/a2aproject/A2A. Apache-2.0, 26,121 stars.
- **Releases.** **v1.0.0 on 2026-03-12** and v1.0.1 on 2026-05-28 (GitHub releases).
- **Governance.** A Linux Foundation project since June 2025. It moved to the **Agentic AI Foundation (AAIF)**, alongside MCP, in August 2026 (https://aaif.io/blog/a2a-joins-aaif; https://axios.com/2026/08/17/a2a-agentic-ai-foundation-open-ai-standards).
- **Spec** (https://a2a-protocol.org/latest/specification/):
  - Bindings: JSON-RPC, gRPC and HTTP+JSON.
  - Objects: AgentCard, Task, Message, Part, Artifact.
  - Task states: `SUBMITTED, WORKING, COMPLETED, FAILED, CANCELED, INPUT_REQUIRED, REJECTED, AUTH_REQUIRED` (each prefixed `TASK_STATE_`).
  - Streaming and webhook push notifications.
  - **Signed Agent Cards** (§8.4).
  - Extensions through the `A2A-Extensions` header.
- **Rust SDK.** https://github.com/a2aproject/a2a-rs. 91 stars, active.
- **Assessment.** **Integrate at the "Next" horizon** (A2A questions and tasks). Map HAN task states onto A2A's. Signed Agent Cards align with HAN's Ed25519 identities. Not needed for the MVP.

### 1.6 Other coordination projects
- **Gas Town** (Steve Yegge). https://github.com/gastownhall/gastown. Go, MIT, 18,345 stars, v1.2.1 released 2026-06-06, pushed 2026-10-08.
  - Single-user orchestration of 20–30 agents through tmux.
  - Roles: Mayor (coordinator); Polecats (workers); Hooks (git-worktree persistence); Witness, Deacon and Dogs (stuck-agent watchdogs); Refinery (a Bors-style merge queue).
  - **Seance** lets an agent query predecessor sessions from `.events.jsonl`.
  - **Wasteland** is federated work claiming between Gas Towns through DoltHub.
  - **Learn from:** the watchdog tier and Seance. Seance is the same idea as `team.read`.
- **Beads** (`bd`). https://github.com/gastownhall/beads. Go, MIT, 27,828 stars, v1.3.2-rc.2 released 2026-10-10.
  - A dependency-graph issue tracker for agents on Dolt. Commands: `bd ready`, `bd update --claim`, `bd close`.
  - **Learn from** "ready work" and atomic claim. HAN's tasks come from GitHub, so skip it as a dependency.
- **Agent Relay** (AgentWorkforce). https://github.com/AgentWorkforce/relay. TypeScript, Apache-2.0, 881 stars, v13.4.0 released 2026-10-11, created 2025-12-17.
  - Pitch: "infrastructure for running coding agents together", with explicit **teams across machines**. It covers messaging (channels, threads, DMs), "Shared Sessions" (searchable team history), GitHub, Linear and Slack as a virtual filesystem, and "Flows" with human gates.
  - Architecture (https://agentrelay.com/docs/architecture):
    - The **engine is hosted** (cast.agentrelay.com); "there is no local one".
    - Providers connect over WebSocket at `/v1/node/ws`.
    - A Rust agent runtime **injects messages into the agent's PTY**.
    - Each agent has its own sequence, and unacknowledged deliveries are replayed.
    - An observer-only stream feeds dashboards.
  - **Observer** (https://agentrelay.com/docs/observer): read-only, expiring, scoped `ot_live_` tokens (default 24 h, DMs excluded).
  - Self-hosting is said to be possible "for each primitive" by contacting the team **[unverified]**.
  - **Closest competitor** on multi-person coordination. I found no derived status, overlap detection or approval verifier.
  - **Learn from:** observer tokens, per-agent sequence with replay, shared-session search.
- **ccswarm** (https://github.com/nwiizo/ccswarm; Rust, MIT, 153 stars) and **claude_code_agent_farm** (Dicklesworthstone; 921 stars; lock-based). Single-user swarms. **Skip.**
- **Small messaging bridges.** agmsg, AgentBridge, claude-peers, agent-talk, Claude Bridge, cross-agent_mcp. Search results only, not checked **[unverified]**. Mostly single-machine relays.
---

## 2. Session observation, monitoring and remote control

### 2.1 Happy (slopus)
- **Repos.**
  - https://github.com/slopus/happy: TypeScript, MIT, 24,112 stars, cli-1.2.6-beta.1 released 2026-10-09. Holds the mobile and web app (Expo), the original CLI (`happy claude`, `happy codex`), and the relay server.
  - https://github.com/slopus/happy-desktop (created 2026-07) and https://github.com/slopus/happy-agent (its own harness).
- **Repositioning.** The project has moved to "Happy Harness", a multi-provider desktop app. The README says it is "**Natively multiplayer.** Invite a colleague … either of you can steer it". That is project marketing; **the mechanism is unverified**.
- **Protocol** (docs/protocol.md, docs/session-protocol.md):
  - HTTP plus Socket.IO at `/v1/updates`. Connections are user-, session- or machine-scoped; the machine scope is for daemons.
  - **Persistent `update` events carry a per-user `seq`; `ephemeral` events carry presence and usage.**
  - Optimistic concurrency through `expectedVersion`.
  - **End-to-end encrypted payloads**: the server never sees plaintext.
  - A unified session protocol has 9 event types. The envelope is `{id, time, role, turn, subagent, ev:{t}}`, with turn-start/turn-end and tool-call-start/tool-call-end. The docs compare it explicitly with ACP.
- **Assessment. Learn from:**
  - the persistent-versus-ephemeral split, which matches HAN's log-versus-gossip question;
  - the normalized event envelope, as a design check on `AgentEvent`;
  - E2E encryption of the payload to the space.

### 2.2 Omnara
- **Repo.** https://github.com/omnara-ai/omnara. Now **Go**, Apache-2.0, 2,907 stars, active.
- **Pivot.** It has moved from a Claude Code mobile "mission control" to "The open-source managed agent platform … self-hostable alternative to Claude Managed Agents and OpenAI's Agents API". **No longer relevant. Skip.**

### 2.3 Claude Code native observation surfaces (verified from docs)
- **Remote Control** (https://code.claude.com/docs/en/remote-control). A claude.ai/code or mobile window onto a local session.
  - Requires a claude.ai login.
  - **Not available** with API keys, Bedrock, Vertex, Foundry or a custom `ANTHROPIC_BASE_URL`.
  - **Same user only.**
- **Cloud sessions** (https://code.claude.com/docs/en/claude-code-on-the-web). "Team" visibility on Team and Enterprise, but "their view doesn't update in real time".
- **Hooks** (https://code.claude.com/docs/en/hooks).
  - Handler types: `command`, **`http`** (the event JSON is POSTed; `allowedHttpHookUrls` is the allowlist), **`mcp_tool`**, `prompt`, `agent`.
  - Events: SessionStart, UserPromptSubmit, PreToolUse, **PermissionRequest**, PostToolUse, PostToolBatch, Notification, SubagentStart/Stop, TaskCreated/Completed, Stop, TeammateIdle, **FileChanged**, WorktreeCreate/Remove, CwdChanged, SessionEnd and more.
  - Caveat for the gate: "Claude Code doesn't run PermissionRequest hooks for a sandboxed command's network request".
- **OpenTelemetry** (https://code.claude.com/docs/en/monitoring-usage).
  - Metrics, events and beta traces.
  - Attributes: `session.id`, `user.email`, `organization.id`, and `vcs.repository.url.full` (opt-in from v2.1.269).
  - `claude_code.tool_result` carries `tool_name` and `tool_use_id` (matching the hook input). With `OTEL_LOG_TOOL_DETAILS=1` it adds `tool_input` (about 4 KB) and git commit SHA and branch.
- **Headless.** `claude -p --output-format stream-json --input-format stream-json --include-hook-events`. `--permission-prompt-tool <mcp tool>` routes permission prompts to an MCP tool.
- **Channels** (research preview). An MCP server pushes events into a running session (https://code.claude.com/docs/en/channels).

### 2.4 Session readers and dashboards
- **ccusage.** https://github.com/ccusage/ccusage. Rust and TypeScript, MIT, 18,982 stars, active. Token and cost analysis from local transcripts. **Learn from** its transcript parsing; skip as a dependency.
- **claude-code-hooks-multi-agent-observability** (disler). https://github.com/disler/claude-code-hooks-multi-agent-observability. Python and TypeScript, **no license file**, 1,546 stars, last push 2026-02-08.
  - Pipeline: hooks → HTTP POST → Bun server → SQLite → WebSocket → Vue.
  - It proves the "hooks as event source" pipeline. **Learn from it; it can't be reused (no license).**
- **CloudCLI / claudecodeui.** https://github.com/siteboon/claudecodeui. TypeScript, **AGPL-3.0**, 14,045 stars. A web and mobile UI for one user. **Skip.**
- **Single-machine dashboards found by search.** claude-team-dashboard (mukul975; MIT; 75 stars; reads `~/.claude/teams/`), ccdeck, Stargx/claude-code-dashboard, claude-session-dashboard. **[unverified beyond search]**.

### 2.5 Parallel-session managers (all single-user, local)

| Project | Lang / license | Stars, activity | Notes |
|---|---|---|---|
| Claude Squad https://github.com/smtg-ai/claude-squad | Go, AGPL-3.0 | 8,591; pushed 2026-08-20 | tmux and worktrees; Claude, Codex, Gemini, OpenCode, Amp |
| ccmanager https://github.com/kbwo/ccmanager | TS, MIT | 1,261; active | no tmux; **per-CLI state detection** (busy, waiting, idle); status-change hooks; experimental AI auto-approval |
| Crystal → **Nimbalyst** https://github.com/nimbalyst/nimbalyst | MIT | 1,877 (Crystal 3,127; its README points to Nimbalyst) | desktop, worktrees, task tracking, mobile companion |
| Sculptor (Imbue) https://github.com/imbue-ai/sculptor | Python, MIT | 238; active | "experimental research preview"; workspaces; container backend |
| Conductor https://www.conductor.build | no public repo found **[unverified]** | v0.90.0 per its site | Mac; Claude Code, Codex, Cursor |
| Vibe Kanban https://github.com/BloopAI/vibe-kanban | Rust+TS, Apache-2.0 | 28,338; pushed 2026-09-19 | **Sunsetting, announced 2026-04-10**: cloud and team features removed after 30 days; local OSS continues community-maintained (https://www.vibekanban.com/blog/shutdown) |
| Terragon https://github.com/terragon-labs/terragon-oss | TS, Apache-2.0 | 260 | **shut down; snapshot published 2026-01-16** |
| AgentAPI (Coder) https://github.com/coder/agentapi | Go, MIT | 1,498 | **archived and deprecated** |
| container-use (Dagger) https://github.com/dagger/container-use | Go, Apache-2.0 | 4,069 | a per-agent container plus branch through MCP; for sandboxing, not coordination |

**Assessment.** These are the board for one person. **Learn from:**
- ccmanager's per-runtime state detectors;
- the Vibe Kanban, Terragon and Omnara outcomes, which suggest hosted team boards for agents struggle as businesses. That favors HAN's open-protocol, self-hosted approach.

---

## 3. Team or agent work boards linked to issues
- **GitHub Copilot cloud agent and Agent HQ.**
  - Copilot cloud agent (https://docs.github.com/en/copilot/concepts/agents/coding-agent/about-coding-agent):
    - It starts from issues, PRs or the agents panel, in an ephemeral environment.
    - Limits: one repo, one branch, a 59-minute session.
    - **Teammates with repo access can follow shared sessions and PRs.**
  - Claude and Codex as agents on GitHub: public preview from 2026-02-04 (https://github.blog/changelog/2026-02-04-claude-and-codex-are-now-available-in-public-preview-on-github/), extended on 2026-02-26.
  - The "Mission Control" dashboard is described in secondary press only **[unverified]**.
  - **Integrate later:** han-github could show these as external rows. Whether an API exists is **[unverified]**.
- **Linear agents** (Developer Preview). Sources: https://linear.app/developers/agents and https://linear.app/developers/agent-interaction
  - Agents are OAuth apps installed with `actor=app`. Delegation sets the agent as **`delegate`, not `assignee`**, so a human keeps ownership.
  - `AgentSessionEvent` webhooks. The agent must emit a `thought` within 10 s.
  - Activities: `thought, elicitation, action{action,parameter,result}, response, error`. `thought` and `action` can be `ephemeral`.
  - **Session state is derived from the last activity**: `pending, active, error, awaitingInput, complete, stale`.
  - `plan` is a checklist with `pending|inProgress|completed|canceled`.
  - `externalUrls`.
  - **Learn from now; integrate in the "Then" horizon.** It is the best existing schema for "an agent working on an issue".
- **Devin** (Cognition, closed). The intro docs cover sessions, Slack and Teams, Linear and Jira tickets, and an API (https://docs.devin.ai/get-started/devin-intro). The page doesn't describe team visibility or MCP. **Skip.**
- **Factory** (Droids, closed). The docs URL returned 404. It is listed as an ACP agent. **Skip [details unverified].**
- **Paca.** A self-hosted Go, Apache-2.0 board with agents as sprint teammates and an MCP server. Search results only **[unverified]**.

---

## 4. Who is working on what, and overlap or conflict detection
- **Clash.** https://github.com/clash-sh/clash. **Rust, MIT**, 65 stars, last push 2026-07-17.
  - Uses `git merge-tree` through **gix** to run three-way merges between every pair of worktrees, read-only.
  - A Claude Code hook checks before each write; there is also a watch mode and `--json` output.
  - Detects **real conflicts**, not just a shared file.
  - **Embed or learn from:** a small, well-scoped technique in the same language.
- **MCP Agent Mail leases plus pre-commit guard.** Intent-based (§1.1).
- **Claude Code agent teams.** File-locked task claim only; there is no file-overlap detection.
- **Beads `--claim` and Gas Town convoys.** Task-level claims.
- **conflict-check.** LLM-based semantic diff comparison, "0 users" **[unverified]**.
- **Prevalence data.** Blog posts cite "33,596 agent PRs; 40.2% of repos had temporally overlapping agent PR pairs" and "AgenticFlict: 27.67% conflict rate" (https://codex.danielvaughan.com/2026/07/28/agent-pr-merge-conflicts-concurrent-coding-agents-codex-cli-worktree-isolation-coordination-defence/). **[unverified; secondhand]**.
- **Gap.** No tool combines these overlap signals across people and machines:
  1. live intent (files touched now);
  2. issue claims;
  3. branch merge simulation;
  4. in-session alerts to both agents.
---

## 5. Open protocols: what to be compatible with

### 5.1 MCP (spec 2026-07-28; AAIF-hosted)
- **Changelog:** https://modelcontextprotocol.io/specification/2026-07-28/changelog
  - **Stateless.** There is no `initialize` handshake and no `Mcp-Session-Id`. Version and capabilities travel in `_meta` on every request. Servers must implement `server/discover`.
  - `subscriptions/listen` is the single stream for server-to-client notifications.
  - Tasks moved to an extension, `io.modelcontextprotocol/tasks`.
  - The Multi Round-Trip Request pattern (`input_required`) replaces server-initiated elicitation and sampling.
  - Results carry `ttlMs` and `cacheScope`.
  - Roots, Sampling and Logging are deprecated.
- **Rust SDK.** `rmcp` v3.5.1, released 2026-10-05; its release notes mention 2026-07-28 peers. https://github.com/modelcontextprotocol/rust-sdk
- **For han-mcp:**
  - Make the `team.*` tools stateless, with handles passed as arguments.
  - Set `ttlMs` on `team.sessions`.
  - Use stdio from Claude Code to han-mcp, which then talks to han-node over the local pipe.

### 5.2 ACP (Agent Client Protocol; Zed-originated, Apache-2.0)
- **Repo.** https://github.com/agentclientprotocol/agent-client-protocol. 4,420 stars; schema v1.25.0 and v2.0.0-alpha.8 released 2026-10-09. **The stable protocol version is 1.**
- **Rust runtime crate.** `agent-client-protocol` (https://github.com/agentclientprotocol/rust-sdk, v3.3.0).
- **Agents** (https://agentclientprotocol.com/overview/agents; 43 listed):
  - Codex through `codex-acp`;
  - Claude through `claude-agent-acp` (https://github.com/agentclientprotocol/claude-agent-acp, 2,640 stars, Apache-2.0);
  - Gemini CLI;
  - GitHub Copilot CLI (public preview);
  - OpenCode, Factory Droid, Goose, Cursor, Junie and others.
- **Clients.** Zed, JetBrains, Neovim, VS Code extensions, and mobile apps including Happy.
- **For HAN.** Add **`drivers/acp`** as the generic AgentDriver for the "Then" horizon's runtimes.
  - ACP's `session/request_permission` maps onto `Gate::check`.
  - Keep `drivers/claude_code` native for richer signal: hooks, `FileChanged` and PermissionRequest.

### 5.3 A2A v1.0
See §1.5. Use it for the "Next" horizon of agent-to-agent questions and tasks. Expose an A2A binding at space or organization boundaries rather than inventing a parallel wire format. Inside a space, HAN's log stays authoritative.

### 5.4 OTLP
han-node could run a localhost OTLP/HTTP receiver to **observe Claude Code sessions that han host didn't start**: IDE, Desktop and agent-view sessions. That gives an "observe-only" board row.

### 5.5 Claude Code local surfaces (documented, closed-source, fast-moving)
- HTTP hooks.
- `claude agents --json`.
- The inbox socket protocol.

Keep these behind the AgentDriver boundary, as PLAN §8 already plans.

---

## 6. DISSECT: concept extraction

| Concept | How they implemented it | Transferable? | Why / why not |
|---|---|---|---|
| Advisory leases with TTL that report conflicts instead of blocking | Agent Mail `file_reservation_paths(ttl, exclusive)`; pre-commit guard | Yes | Crashed agents can't deadlock anyone; fits HAN's rule that status is non-authority (a lease is a stream item, not Policy) |
| Intent versus observation | Agent Mail = declared; Clash = diffs; CC hooks = tool calls | Yes, both | Intent catches overlap before work starts; observation catches undeclared work |
| Merge simulation as an overlap tier | Clash: gix merge-tree, pairwise | Yes | Separates "same file" (noisy) from "will conflict" (actionable) |
| State derived from the last event | Linear session state; CC agent view; ccmanager detectors | Yes | Matches PLAN's "host derives status"; add `stale` |
| Cheap model summary at a bounded rate | CC agent view: Haiku, at most every 15 s | Yes | Same as PLAN's "occasional summary"; bound the rate and budget |
| Delegate versus assignee | Linear `delegate` | Yes | The human owns the issue while the agent acts; maps onto `person` plus `agent` on the board |
| Persistent versus ephemeral channels | Happy `update`+seq versus `ephemeral`; Relay observer stream "never a delivery path" | Yes | Evidence for the round-30 T-07 decision: presence on gossip, payload on the log |
| Scoped, expiring read-only observer tokens | Agent Relay `ot_live_`, 24 h | Yes | A guest or manager watches without membership; in HAN, a Policy capability |
| Peer messages are never consent | CC cross-session: "can't approve anything"; hold, accept or refuse | Yes | Matches PLAN's rule that answers are data; reuse the vocabulary |
| Push into a running agent | CC inbox socket and channels; Relay PTY injection | Yes (socket or channel) | Overlap alerts must land mid-turn; PTY injection is fragile and loses provenance |
| Peer or predecessor session query | Gas Town Seance; Relay Shared Sessions | Yes | Validates `team.read`; add "previous sessions on this issue" |
| Watchdog tier | Gas Town Witness, Deacon | Partly | The han-node supervisor flags long-blocked sessions |
| Normalized cross-vendor event envelope | Happy session protocol; ACP | Yes | Check `AgentEvent` against these before freezing it |
| Hosted orchestration SaaS churn | Vibe Kanban sunset, Terragon shutdown, Omnara pivot | Signal | Open protocol plus self-host is the defensible position |
| Vendor scopes to one user | CC Remote Control and cross-session: same account only | Signal | The structural gap HAN fills; also a risk if Anthropic adds organization peers |

---

## 7. ABSTRACT: ranked patterns for HAN

1. **Observe through the runtime's published surfaces; own only the cross-team layer.**
   - *Mechanism:* `drivers/claude_code` registers HTTP hooks pointing at a han-host loopback endpoint, or uses stream-json with `--include-hook-events`. The Gate uses `PermissionRequest` or `--permission-prompt-tool`. `files` comes from PostToolUse and FileChanged. `state` and `waitingFor` are cross-checked against `claude agents --json`.
   - *Where:* han-host, the M4 spike.
   - *Build vs buy:* use Anthropic's interfaces; build the mapping.
   - *Caveat:* PermissionRequest skips sandboxed network requests, so "outside HTTP needs a human" must be enforced by routing HTTP through han-exec and denying the sandbox network.
2. **Push alerts into the session over its own inbox.**
   - *Mechanism:* han-host is the parent process and posts to `CLAUDE_CODE_MESSAGING_SOCKET`, sending the token on Windows; "own-child" messages are delivered. Fallback: a channel MCP server in han-mcp.
   - *Where:* han-host and han-mcp, for the overlap alerts.
   - *Build vs buy:* build, on the documented socket.
3. **Three-tier overlap.**
   - Tiers: (a) the same issue; (b) files touched recently (observed); (c) gix merge-tree between session branches, which escalates to "will conflict".
   - *Where:* han-node views (`Overlaps`).
   - *Build vs buy:* build; borrow Clash's approach.
4. **Leases as non-authority stream items.**
   - *Mechanism:* `team.claim(paths, ttl)` and `team.release` emit `han.lease/1`. `overlaps()` reports a conflict and never blocks.
   - Optional pre-commit guard installed by han-host.
   - *Build vs buy:* borrow the pattern from Agent Mail (its license rules out the dependency).
5. **`han.status/1` schema alignment.**
   - Add `stale` and `failed`.
   - Adopt the `waitingFor` enum.
   - Owner and delegate fields.
   - An optional `plan[]` matching Linear.
   - *Where:* spec round 30.
6. **`drivers/acp`.** Built on the `agent-client-protocol` crate, for Codex, Gemini, Copilot and OpenCode. *Build vs buy:* adopt the crate (it is a genuine standard).
7. **han-mcp on MCP 2026-07-28 with rmcp.** Stateless tools, handles, `ttlMs`. *Build vs buy:* adopt rmcp.
8. **Observer capability.** A read-only, scoped, expiring Policy grant.
9. **OTLP intake for sessions HAN didn't start.** Post-MVP.
10. **A2A binding for cross-space questions and tasks.** Next horizon.

---

## 8. What I couldn't determine
- The depth of Happy's "natively multiplayer" (per-session invites only, or a team view).
- Agent Relay's self-hosting completeness, pricing, and whether its engine source is public.
- GitHub Agent HQ's "Mission Control" specifics and any session-listing API.
- Conductor's license (no public repo found), Factory's docs (404), and Devin's team session visibility.
- Whether `claude agents --json` exposes the Haiku summary; the documented fields don't include it.
- Any Anthropic plan for multi-user cross-session messaging; nothing in the docs suggests one.
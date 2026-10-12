# Research: open-source GUIs for coding agents, for HAN's Tauri GUI

Date: **2026-10-12**. Researcher: Claude subagent. Method: shallow clones (no installs, no binaries run) at
`H:\agentic\temp\claude\H--HAN\5c1257e4-8c11-4fbf-80b1-3b3209de9c9f\scratchpad\research\gui\`, plus the GitHub API (`gh`), the npm
registry (`npm view`) and the cited web pages. Context read: `PLAN.md` §1, §3, §5, §5a; `research/intentd.md`.

Clones and commits:
| Repo | Commit | Commit date |
|---|---|---|
| pingdotgg/t3code | `511d20c6` | 2026-10-12 |
| OrchestratorInc/agent-orchestrator | `b92eb51` | 2026-10-12 (21:50 UTC) |
| intent-hq/cloudlands-fe | `84d75f3` | 2026-10-12 |
| spacering-net/codeg (Tauri alternative) | `4ebd0e4` | 2026-10-12 |
| coollabsio/jean (Tauri alternative) | `6b5db54` | 2026-10-12 |

Legend: **[V]** = verified in source, the GitHub API or the npm registry; **[D]** = the project's own docs or README, not
checked against code; **[3P]** = a third-party claim; **[?]** = could not determine. All paths are relative to the repo root.

This is not legal advice. The license notes below are a careful reading of the license texts, and a lawyer should confirm them before code is copied.

---

## 0. Answer

1. **Frontend stack: React 19 + TypeScript + Vite + Tailwind v4 + shadcn-style primitives + zustand + TanStack Router.**
   - This is the stack shared by t3code and agent-orchestrator, and by every credible Tauri agent GUI found (codeg, jean, opcode).
   - Intent's frontend is Svelte 5 + Redux-saga, but almost none of it is liftable, so choosing Svelte would buy little.
2. **Adopt two dependencies outright: `@pierre/diffs` and `@pierre/trees`** (Apache-2.0, pierrecomputer/pierre).
   - All three target apps, plus jean and vibe-kanban, render diffs with `@pierre/diffs`. That is the strongest convergence signal in this research.
3. **Lift one component package almost whole: agent-orchestrator's `packages/product-ui`.**
   - It holds the Kanban board, session cards, the status/attention model and agent avatars. It is deliberately decoupled: an import-boundary test bans stores, API clients, Electron and hooks.
   - It is the closest thing to HAN's team board.
4. **Most other pieces are too entangled to lift as components**, so borrow their *model and presentation logic* instead:
   - t3code's normalized turn-item model and tool classification (MIT, small pure-TS files);
   - agent-orchestrator's approval card and elicitation dock;
   - t3code's keybinding system with "when" clauses;
   - Intent's mock/live `AppClient` seam.
5. **Build ourselves** everything that is HAN-specific. None of the apps has any of these:
   - the board's data model (person × agent × issue);
   - overlap alerts;
   - a cross-session approvals inbox backed by signed approvals;
   - the live `han-timeline` view;
   - the Tauri bridge (`call` as a command, `view` as a `tauri::ipc::Channel`).
6. **No Tauri app is good enough to adopt instead.**
   - codeg is the closest in shape (Tauri 2, React 19, a Rust ACP client, Apache-2.0), but it is monolithic: its `src-tauri/src/acp/connection.rs` alone runs past 27,000 lines.
   - Use codeg and jean as references, not bases.

---

## 1. Identifying the repos

| Name asked | Picked | Why | Other candidates |
|---|---|---|---|
| t3code | **pingdotgg/t3code** ("T3 Code", https://t3.codes) [V] | Exact name; 27.2k stars. All other matches are forks or spin-offs. | maria-rcks/t1code (TUI port, 531★), zortos293/t3code-copilot (58★) [V] |
| agent-orchestrator | **OrchestratorInc/agent-orchestrator** ("Orchestrator.inc", formerly "AO") [V] | `ComposioHQ/agent-orchestrator` now redirects here (`gh repo view ComposioHQ/agent-orchestrator` returns `OrchestratorInc/...`). It has 13.1k stars, the most of any repo with this exact name. `frontend/package.json` still names an earlier org, `aoagents/agent-orchestrator`. | awslabs/cli-agent-orchestrator (1.4k★, tmux-based CLI); willynikes2/agent-orchestrator (67★) [V] |
| Intent app | **intent-hq/cloudlands-fe** ("Electron + SvelteKit frontend for the intentd daemon") [V] | The monorepo `intent-hq/intent` (305★) is only a shell: `.gitmodules` points `packages/cloudlands-fe` at this repo. The desktop app at https://intentapp.dev is built from it [D: monorepo README]. | none |

---

## 2. Briefs

### 2.1 t3code (pingdotgg/t3code)

**What it is:** "an agent harness control surface": one local server that drives Codex, Claude, Cursor, Grok Build, OpenCode
and Antigravity, with a web app, an Electron desktop app and native mobile apps as clients [D: README].

**License:** **MIT**, "Copyright (c) 2026 T3 Tools Inc." [V: `LICENSE`]. No NOTICE file.
- `.repos/` vendors reference repos (effect-smol, alchemy-effect) under their own licenses. Don't copy from `.repos/`.
- `third-party-licenses.config.json` tracks bundled assets.

**Maturity and activity:**
- 27,194 stars, 7,177 forks, **476 contributors**, about 5,425 commits; created 2026-02-08; last push 2026-10-12 [V: gh API].
- 100 or more commits in the 30 days before 2026-10-12 (the API page cap) [V].
- Latest stable release v0.0.45 (2026-10-02), with nightly builds daily [V]. The `0.0.x` versioning signals a pre-1.0 API.

**Shell:**
- The desktop app is **Electron 44.4.5** (`apps/desktop/package.json`).
- The same web app (`apps/web`) is also served by the `t3` Node server and at app.t3.codes [V/D].
- The mobile app is separate, under `apps/mobile` (Expo).

**Frontend stack** (`apps/web/package.json`) [V]:
- **Framework:** React 19.2 with the React Compiler (`babel-plugin-react-compiler`), Vite (through "vite-plus"), TanStack Router.
- **State:** zustand 5 plus **Effect 4** (`@effect/atom-react`). The client runtime (`packages/client-runtime`) is written in Effect.
- **Styling:**
  - Tailwind v4;
  - shadcn conventions (`components.json`, style `base-mira`, registries `@coss` and `@spell`) on **Base UI** (`@base-ui/react`);
  - `class-variance-authority`, `tailwind-merge` and lucide icons.
- **Components:**
  - **`@pierre/diffs`** and **`@pierre/trees`** for diffs and file trees;
  - `@legendapp/list` to virtualize lists;
  - Tiptap 3 for the composer;
  - `react-markdown` + `remark-gfm`, mermaid, `@dnd-kit`.
- **Terminal:** its own adapter over **libghostty-vt compiled to WASM**, rendering to Canvas 2D (`apps/web/src/terminal/ghostty/`: `runtime.ts`, `core.ts`, `renderer.ts`, `surface.ts`). It is not xterm.js.

**How it talks to agents:**
- The browser talks to the server through **Effect RPC over WebSocket** (`WsRpcGroup` in `packages/client-runtime/src/rpc/protocol.ts`, served at `/ws` by `apps/server/src/ws.ts`) [V].
- The server drives each harness in its native protocol [V]:
  - Claude through `@anthropic-ai/claude-agent-sdk` (`apps/server/src/provider/Drivers/ClaudeDriver.ts`);
  - Codex through its **app-server** protocol (`packages/effect-codex-app-server`, `Drivers/CodexDriver.ts`);
  - Cursor, Grok, Pi and OpenCode through **ACP** (`packages/effect-acp`, a generated schema plus client and agent sides; `packages/provider-acp/src/server/AcpSessionRuntime.ts`, `clientPolicy.ts`; `packages/provider-acp-registry`).
- All of it is normalized into one "orchestration v2" execution graph (`packages/contracts/src/orchestrationV2.ts`, 3,829 lines):
  - node kinds: `root_turn`, `assistant_message`, `reasoning`, `plan`, `todo_list`, `tool_call`, `approval_request`, `user_input_request`, `subagent`, `hook`, `system`;
  - turn-item types: `command_execution`, `file_change`, `approval_request`, `checkpoint`, `web_search` and others;
  - the stream is **snapshot then events** (a `kind: "snapshot"` envelope, then `kind: "event"`, plus `synchronized`). That is the same shape as HAN's `view()`.
  - The design docs (`docs/orchestration-v2/README.md`) state the invariant "App ids are primary. Provider ids are refs."

**UI pieces worth studying** (all under `apps/web/src/` unless noted):

| Piece | Files | Coupling |
|---|---|---|
| Session/chat view | `components/ChatView.tsx` (12,214 lines); `components/chat/MessagesTimeline.tsx` (5,938 lines, 109 imports: Effect atoms, router, its own stores, `@t3tools/*`) | **Entangled.** Borrow ideas only. |
| Tool-call rendering | `components/chat/WorkLog.tsx` (141 lines: `WorkLogBlock/List/Row/Button/Details`, depending only on React and `cn`); `packages/client-runtime/src/work-log/{presentation,toolPresentation,itemDetail,commandLabel}.ts` (about 4.7k lines including tests, pure TS over `@t3tools/contracts`); `packages/shared/src/toolActivity.ts` (448 lines, maps raw tool calls to icon, surface and source); `components/chat/ShellCommandBlock.tsx`, `V2ItemInspector.tsx` | WorkLog primitives: **liftable**. The presentation logic is **portable with a type swap**. |
| Diff viewer | `components/DiffPanel.tsx` (1,254 lines, entangled); `components/diffs/StyledDiffCodeView.tsx` (`@pierre/diffs/react`); `components/DiffWorkerPoolProvider.tsx` (167 lines, `@pierre/diffs/worker`); `components/diffs/DiffFileTree.tsx`, `DiffCommentAnnotation.tsx` | The worker-pool provider and the styled view are **near-liftable**; the panel is not. |
| Per-turn change history | Server: `apps/server/src/checkpointing/` (`CheckpointStore.ts` writes git refs under `refs/t3/checkpoints`; `CheckpointDiffQuery.ts`); web: `lib/checkpointDiffState`, `diffPanelStore.ts` | **Pattern:** a git-ref checkpoint per turn, and a diff between any two turns. |
| Terminal | `components/ThreadTerminalDrawer.tsx` (1,902 lines, entangled); `terminal/ghostty/*` (self-contained WASM adapter; its README says to keep transport out of it) | The ghostty adapter is **liftable but heavy** (needs the wasm build script and the libghostty pin). |
| File tree | `components/chat/ChangedFilesTree.tsx` (260 lines: contracts, lucide, React); `components/files/FileBrowserPanel.tsx` (with `@pierre/trees`) | ChangedFilesTree: **liftable**. |
| Approvals | `components/chat/ComposerPendingApprovalPanel.tsx` (68 lines) + `ComposerPendingApprovalActions.tsx` (116 lines); `pendingUserInput.ts`; `components/chat/ComposerPendingUserInputPanel.tsx` | **Liftable** (small; depends on its `ui/` button, menu and tooltip). |
| Multi-agent view | No board. A sidebar lists threads by project with status (`components/Sidebar.tsx`, 5,656 lines; `ThreadStatusIndicators.tsx`); subagents appear through `chat/ThreadRelationshipsControl.tsx`, `ProviderSubagentBar.tsx` | Entangled; not a team board. |
| Worktrees | `BranchToolbarEnvModeSelector.tsx` (local vs worktree mode), `WorktreeBaseBranchPicker.tsx`, `chat/WorktreeSetupCard.tsx`, `worktreeCleanup.ts`; the server handles git | Product-specific. |
| Keyboard UX | `packages/contracts/src/keybindings.ts` (227 lines) + `packages/shared/src/keybindings.ts` (469 lines): user-configurable bindings with VS Code-style **when-clauses** (`terminalFocus`, `editableFocus`…); `apps/web/src/keybindings.ts`; `components/CommandPalette.tsx` (3,660 lines); `docs/user/keybindings.md` | Keybinding core: **liftable** (pure TS). The palette is entangled. |
| UI primitives | `components/ui/*` (about 50 shadcn-style components on Base UI) | **Liftable** (MIT). |

### 2.2 agent-orchestrator (OrchestratorInc/agent-orchestrator, "Orchestrator.inc")

**What it is:** a local desktop workspace that runs "worker" sessions (one task, one agent, one worktree) and a project
"orchestrator" agent, with a live **Kanban** of every worker plus its PR, CI and review state [D: README].

**License:**
- **Apache-2.0** at the repo root [V: `LICENSE`]. **No NOTICE file** [V].
- **Inconsistency:** `frontend/package.json` and `packages/product-ui/package.json` both say `"license": "MIT"` [V]. Treat the code as Apache-2.0, the root license and the stricter reading, unless the maintainers clarify.
- Two bundled agent logos carry their own license files (`frontend/src/renderer/assets/agents/LICENSE-codewhale.txt`, `LICENSE-mimo-code.txt`).
- Treat third-party agent logos as trademarks, not code.

**Maturity and activity:**
- 13,113 stars, 1,798 forks, **123 contributors**, about 3,261 commits; created 2026-02-13; last push 2026-10-12 [V].
- 100 or more commits in 30 days [V]. Latest stable v0.13.7 (2026-10-10), with nightlies several times a day [V].

**Shell:**
- **Electron** (`electron ^33`, Electron Forge) over a **Go daemon** (`backend/`) [V].
- There is also a mobile app (`packages/mobile`) and a cloud variant (`cloud/`, `private/`).

**Frontend stack** (`frontend/package.json`) [V]:
- **Framework:** React 19, Vite 8, TanStack Router + **TanStack Query**, `openapi-fetch` with types generated from the daemon's OpenAPI spec.
- **State:** zustand 5.
- **Styling:** Tailwind v4, Radix UI, cva, `tw-animate-css`, `motion`.
- **Components:**
  - `@pierre/diffs` 1.4.1 and `@pierre/trees`;
  - **xterm.js 6** (beta) with fit, search, unicode11, web-links and webgl addons;
  - Lexical for the composer; CodeMirror 6;
  - `cmdk` for the palette; `react-resizable-panels`, `@tanstack/react-virtual`, `react-markdown`, i18next.
- A `DESIGN.md` design-system guide lists the "approved" reference files.

**How it talks to agents** [V]:
- **Renderer to daemon:** loopback HTTP (OpenAPI), an **SSE** event stream (`/api/v1/events`, `/api/v1/sessions/{id}/workspace/events`) and a terminal-mux WebSocket (`frontend/src/renderer/lib/api-client.ts`, `hooks/useEventsConnection.ts`).
- **Daemon to agents,** through one of two interface modes per session (`docs/architecture.md`):
  - **Chat** runs a native protocol controller:
    - **ACP** through `github.com/coder/acp-go-sdk v0.13.5` (`backend/internal/adapters/chatdriver/acp/{client,driver}.go`), with per-harness drivers (`claudeacp`, `piacp`, `geminiacp`, `cursoracp`, `opencodeacp` and more);
    - the Codex app-server (`chatdriver/codexappserver`).
  - **TUI** runs the agent's own terminal UI in a PTY, ConPTY or tmux (`adapters/runtime/{ptyexec,conpty,tmux}`).
- ACP chat processes run in **detached per-session hosts**, so restarting the daemon doesn't kill a turn [D: architecture.md].
- It bundles `@agentclientprotocol/claude-agent-acp` **0.70.0** (`frontend/acp-runtime/package.json`).
- For Pi it uses the user-installed **`@victor-software-house/pi-acp`**, which "does not request ACP permissions" (`chatdriver/piacp/driver.go`).
  - **HAN check:** PLAN.md cites `pi-acp` registry v0.0.34. AO references pi-acp **0.17** under a different npm scope. Confirm which package HAN means.

**Architecture principle worth copying** [D: `docs/architecture.md`]:
- "Display status is never stored. It is computed at read time from durable facts."
- The pipeline is OBSERVE → UPDATE (durable facts) → DERIVE (display status). Only facts like `activity_state`, PR, check and review rows are stored.

**UI pieces:**

| Piece | Files | Coupling |
|---|---|---|
| **Multi-agent board** | **`packages/product-ui/src/SessionsBoardView.tsx`** (739 lines: `SessionsBoardGridView`, `SessionCardView`, `SessionsArchiveView`); **`session-models.ts`** (16 `SessionStatus`es, `SessionActivityState`, `KANBAN_COLUMNS` building/validating/needs_review/ready/archive, `DISPLAY_STATUSES`); **`session-presentation.ts`** (533 lines: `getSessionStatusView`, `attentionZone` merge/action/pending/working/done, `toKanbanColumn`); `SessionInspectorView.tsx` (1,594 lines); `AgentAvatar.tsx`, `agents.ts`, `agent-capabilities.ts`. Host adapter: `frontend/src/renderer/components/SessionsBoard.tsx`, `SessionsBoardAdapters.tsx`; `frontend/src/renderer/lib/session-presentation.ts` | **Decoupled by design.** The README says it "does not know about generated API clients, Electron, the AO daemon, renderer stores, or any application's i18n singleton". `src/import-boundary.test.ts` allows only `clsx`, `motion/react`, `react` and `tailwind-merge`. Labels and links are injected. Needs AO's Tailwind tokens (`frontend/src/styles/tokens.css`). **Most liftable piece in this research.** |
| Session/chat view | `frontend/src/renderer/components/SessionView.tsx`; `chat/ChatWorkspace.tsx`; `chat/ChatTimelineItems.tsx` (3,026 lines); `chat/SessionChatSurface.tsx` | Entangled (bridge, stores, queries). |
| Tool-call rendering | `chat/ActivityRun.tsx` (426 lines: React, motion, lucide, `types/conversation`), `chat/activity-command.ts` (84), `chat/action-pill.ts` (30); taxonomy in `renderer/types/conversation.ts` (942 lines: `ActivityKind` = command, file_change, plan, reasoning, approval, usage, error, system, mcp_tool, auto_review, user_input) | **Portable with moderate work.** The taxonomy comments are worth reading ("an approval is a question waiting on a person, an auto-review is a decision already taken"). |
| Approvals | **`ApprovalCard`** in `chat/ChatTimelineItems.tsx` (line ~2477: decisions come "from the provider's own list — never from a fixed set"); **`chat/ElicitationDock.tsx`** (570 lines; a pending question "docks above the composer rather than landing in the transcript") | **Portable with moderate work** (needs `bridge`/`utils` swapped). |
| Diff viewer | `components/diffs/AoDiffFile.tsx` (215 lines), `pierreTheme.ts`, `WorkspaceReviewPane.tsx`, `useCodeSelection.ts`; `components/WorkspaceDiffView.tsx` (925 lines); `workers/diff-parser.worker.ts`; `ImageDiffView.tsx` | AoDiffFile and its theme are **near-liftable**. |
| Terminal | `components/XtermTerminal.tsx` (2,081 lines), `TerminalPane.tsx`, `TerminalSearch.tsx`, `hooks/useTerminalSession.ts` (batches WebSocket frames before `write()`) | Entangled. Reference only. |
| File tree | `components/FileTree.tsx` (612 lines, `@pierre/trees/react` + react-query hooks), `SessionFileExplorer.tsx` | Medium. |
| Worktrees | Go: `backend/internal/adapters/workspace/gitworktree/commands.go` (worktree add/remove/prune; **snapshots through a temporary index plus `write-tree`**, which leaves the user's index untouched) | **Pattern** for han-host. |
| Keyboard UX | `frontend/src/shared/shortcuts.ts` (416 lines), `stores/keybindings-store.ts`, `components/KeyboardShortcutsDialog.tsx` (179), `components/CommandPalette.tsx` (889, cmdk) | Medium. |
| Notifications ("needs you") | `components/NotificationCenter.tsx`, `NotificationRuntime.tsx`, `TrayRuntime.tsx` | Reference. |

### 2.3 Intent app (intent-hq/cloudlands-fe)

**What it is:** the desktop face of Intent, an Electron + SvelteKit renderer over the Rust daemon `intentd` (researched in `research/intentd.md`) [V: README].

**License:** **Apache-2.0** [V: `LICENSE`, `package.json`].
- It has a **NOTICE**: "cloudlands-fe / Copyright 2026 The Intent Authors … Intent originated as 'Intent by Augment' … the NOTICE preserves that lineage and branding." [V]
- Any redistribution of derived code must carry this NOTICE text (Apache-2.0 §4(d)).

**Maturity and activity:**
- 8 stars, 6 forks, **9 contributors**, about 3,177 commits; created 2026-06-29; last push 2026-10-12 [V].
- Releases at v2.215.x (latest stable v2.212.1, 2026-10-08) [V]. Very fast-moving, with a small team; most commits are probably agent-authored [?].

**Shell:** **Electron 44** + SvelteKit with `adapter-static` [V].

**Frontend stack** [V]:
- **Framework:** **Svelte 5** (runes, used for UI only).
- **State:** **Redux + redux-saga** for all shared state (`src/store/renderer`).
- **Styling:** Tailwind v4, `bits-ui`, `tailwind-variants`, phosphor and FontAwesome icons.
- **Components:** `@pierre/diffs`, **xterm.js 6**, **monaco-editor**, Tiptap, mermaid and d3; paraglide i18n.
- **Testing:** Playwright component tests with `*.preview.svelte` harnesses.

**How it talks to agents:**
- The renderer reaches the backend only through **`AppClient`** (`src/lib/client/app-client.ts`, 2,645 lines), which has two implementations [V: README, source]:
  - `live/` speaks JSON-RPC to `intentd` over the Electron IPC bridge;
  - **`mock/` (`mock-app-client.ts` + `fixtures/`) runs the whole UI without a daemon.**
- Each domain exposes `Promise` queries, `subscribe(handler) => Unsubscribe` streams and mutations.
- ACP itself lives in intentd (Rust). `src/features/acp-official/` holds a TS ACP implementation whose README says the client is still "TODO". It looks stale; treat it as legacy [?].

**UI pieces:**

| Piece | Files | Coupling |
|---|---|---|
| Session/chat | `src/lib/components/chat/` (about 100 Svelte components: `ChatPanel`, `AgentMessageList`, `StreamingRenderer`, `LazyTurn`, `ResponseGroup`) | **Entangled** with the Redux store (`$store/renderer/...`). Svelte only. |
| Tool calls | `chat/ToolCall.svelte` (479 lines, imports 4 store slices), `ToolDetails.svelte`, `ToolStatusIcon.svelte`; `src/lib/utils/tool-classifier.ts` (2,278 lines) | Entangled. The classifier is pure TS but large. |
| Approvals | `chat/InlinePermissionRequest.svelte` (195 lines, permission slice), `permission-parser`, `permission-shortcut`; `AttentionRequestBanner.svelte` | Entangled. **Pattern:** keyboard shortcuts on permission prompts. |
| Diff viewer | `src/features/file-tracking/components/diff/DiffViewer.svelte` (vanilla `@pierre/diffs` core, not the React binding), `TrackedChangeDiffViewer.svelte`, `diff-ipc-batcher.ts`, `line-staging.ts`; `chat/UnifiedMultiStageDiff.svelte`, `ChatChangesPanel.svelte` | Shows that `@pierre/diffs` works outside React. |
| Change tracking | `src/features/file-tracking/types.ts`: `ChangeStage` = unstaged → staged → committed → pushed → pull_request → merged → trunk, with per-agent attribution | **Pattern** for HAN's "accepted?" lifecycle. |
| Multi-agent view | `src/lib/components/agent-overview/` (`AgentHierarchyGraph.svelte`, `AgentHierarchyCard.svelte`, `tree-layout.ts`; coordinator → delegate tree); `TimeScrubber.svelte` (a history slider with "go live") | Reference. **TimeScrubber appears unused:** no importer was found besides its own index [V, grep]. |
| Terminal | `src/lib/components/terminal/Terminal.svelte`, `QuakeTerminalOverlay.svelte` (drop-down terminal); `src/features/terminal/terminal-state-machine.ts` | Reference. |
| File tree | `src/lib/components/file-explorer/VirtualizedFileTree.svelte` | Svelte only. |
| Keyboard | `src/lib/components/CommandPalette.svelte`; `features/settings/settings-palette-commands.ts` | Reference. |

**Verdict for HAN:** borrow the **AppClient seam (live + mock + fixtures)** and the **component preview harness** as patterns. Little code is reusable, because of Svelte and the Redux coupling.

---

## 3. Tauri-based agent GUIs (instead of, or as references)

Sources: the ACP client list https://agentclientprotocol.com/get-started/clients, GitHub search and the GitHub API (2026-10-12).

| Repo | Stack [V] | License [V] | Stars, last push [V] | Agent transport | Verdict |
|---|---|---|---|---|---|
| **spacering-net/codeg** | Tauri 2 + **React 19** + Next 16 + zustand + Base UI/Radix + Monaco + xterm; Rust `agent-client-protocol` 2.2 | Apache-2.0 | 3,922; 2026-10-12 | **ACP in Rust** (`src-tauri/src/acp/`) | **Closest in shape to HAN.** Monolithic (`acp/connection.rs` is over 27k lines). Worth borrowing: `src-tauri/src/acp/event_stream.rs`, a per-connection broadcast with `seq` and a byte- and count-capped ring buffer, where a lagging subscriber re-attaches from a snapshot. That is HAN's view fan-out exactly. Also `src/components/ai-elements/` (adapted Vercel AI Elements: `tool.tsx`, `reasoning.tsx`, `message.tsx`, `code-block.tsx`, `file-tree.tsx`, `terminal.tsx`). |
| **coollabsio/jean** | Tauri 2 + React 19 + zustand + TanStack Query + **@pierre/diffs** + xterm | Apache-2.0 (`LICENSE.md`) | 1,323; 2026-10-12 | Mostly CLI stream-JSON per harness (`jean-core/src/chat/{claude,cursor,…}.rs`), plus some ACP hosts (Grok, Kimi) | A good reference for Tauri plumbing (plugins: window-state, notification, dialog, fs). Not ACP-first. |
| winfunc/opcode (formerly getAsterisk/claudia) | Tauri 2 + React 18 + zustand | **AGPL-3.0** | 22,412; 2026-09-18 | Claude Code only | **Do not copy** (AGPL). |
| zhukunpenglinyutong/desktop-cc-gui | Tauri 2 + React 18 [3P: search snippet] | **No license** | 4,465; 2026-10-10 | multi-CLI | **Do not copy** (all rights reserved). |
| AnyiWang/OpenCovibe | Tauri 2 + Svelte 5 | Apache-2.0 | 271; 2026-08-26 | Claude CLI, Codex app-server [3P] | Small. Reference only. |
| recailai/jockey | Tauri 2 + **SolidJS**; Rust `agent-client-protocol` 2.0 | MIT | 25; 2026-10-07 | ACP | Too small to rely on. |
| savvasioakeim/OctoShell | Rust/Tauri [D: ACP list] | MIT | 5; 2026-10-12 | ACP; a worktree per task | Too small. |
| formulahendry/acp-ui | Tauri 2 + **Vue 3**; `@agentclientprotocol/sdk` 0.13 | MIT | 498; 2026-05-25 | ACP (TS SDK) | Generic and stale. |
| zvzuola/acp-components | React components + a framework-free core (zustand vanilla) for ACP clients; runs in Tauri [D] | MIT | 63; 2026-09-14; npm `@acp-components/react` 0.1.0 | ACP | **Watch it.** It renders ACP `session/update` shapes directly (tool calls, permissions, plans, diffs). Too young to adopt. |
| BloopAI/vibe-kanban | Rust (axum) + React 18 + Radix + `@pierre/diffs`; a `crates/tauri-app` crate; ts-rs for types | Apache-2.0 | 28,339; 2026-09-19 | per-executor CLIs | **The company is shutting down** (announced 2026-04-10; "will continue as open source and community maintained"; https://www.vibekanban.com/blog/shutdown). Its Rust → TS type generation is worth a look. |
| Non-Tauri, for licensing: kdlbs/kandev (AGPL-3.0), superset-sh/superset (Elastic-2.0), getpaseo/paseo (custom mixed license), athasdev/athas (AGPL-3.0, Tauri editor) | | | | | **Do not copy.** |

**Conclusion:** no Tauri app beats the plan "React frontend borrowing from t3code and AO, Rust backend embedding han-node".
- **codeg** is the one to read for Tauri + Rust ACP plumbing.
- **jean** is the one to read for Tauri app chrome.

---

## 4. Comparator: concepts vs implementations

| Concept | How they implemented it | Transferable? | Why / why not |
|---|---|---|---|
| One normalized execution graph over many harnesses | t3code `orchestrationV2.ts` (node kinds, turn items, "App ids primary, provider ids are refs"); AO `types/conversation.ts` (`ActivityKind`); codeg `acp/types.rs` | **Yes** | HAN's `AgentEvent` (PLAN §5) is thin. The GUI needs a typed item stream (message, reasoning, plan, tool_call with kind and status, file_change, approval_request) to render tool calls well. ACP's `SessionUpdate` already provides most of it, so preserving ACP shapes lets ACP renderers be reused. |
| Snapshot then diffs, with re-snapshot on lag | t3code (snapshot/event/synchronized envelopes); codeg `event_stream.rs` (seq + capped ring buffer; `Lagged` → snapshot); AO (SSE + refetch) | **Yes** | It *is* HAN's `view()`. The extra insight is a **bounded replay buffer plus a gap-detecting `seq`**, so a slow webview re-snapshots instead of running out of memory. |
| Display status derived at read time, never stored | AO `docs/architecture.md`; `toKanbanColumn`, `attentionZone` in product-ui | **Yes** | It matches PLAN §5's "`stale` is derived…, never from the agent's own claim". It also extends to board columns and "needs you". |
| Attention zones above raw status | AO `attentionZone()`: merge/action/pending/working/done; the board orders working → action → pending → merge | **Yes** | HAN's board needs a "needs-you" axis (`waiting-approval`, `blocked`, overlap) separate from `state`. |
| A portable presentation package with an enforced import boundary | AO `packages/product-ui` plus `import-boundary.test.ts` | **Yes** | Lets the same board serve the Tauri GUI, a future web face and component previews. A cheap CI test keeps it honest. |
| Backend seam with live and mock implementations | Intent `AppClient` (`live/`, `mock/` + fixtures) | **Yes** | HAN's surface is just `call` + `view`, so one `HanClient` interface with Tauri, JSON-RPC and Mock implementations is tiny. The mock enables UI work and component tests before han-node exists. |
| Pending decisions dock at the composer, not in the transcript | AO `ElicitationDock`; t3code `ComposerPendingApprovalPanel` | **Partly** | Good for the session drill-down. HAN *also* needs a cross-session inbox, which none of them has. |
| Decisions come from the request, not a fixed set | AO `ApprovalCard` ("never from a fixed set") | **Yes** | HAN approvals vary by Policy. Render whatever options the gate offers. |
| Checkpoint per turn (git refs) for diffs between turns | t3code `apps/server/src/checkpointing/CheckpointStore.ts` (`refs/t3/checkpoints`) | **Yes, as the fallback** | `han-timeline` is the primary engine. Per-turn git checkpoints are a cheap cross-check and fallback, consistent with PLAN §5a's `git diff -U0` fallback. |
| Worktree snapshot through a temporary index | AO `gitworktree/commands.go` (`addAllTempIndexArgs`, `writeTreeArgs`) | **Yes** | han-host can snapshot a worktree, untracked files included, without touching the agent's index. That makes `files[].hunks` derivation safe. |
| Change-stage lifecycle | Intent `ChangeStage` (unstaged → … → merged → trunk) | **Partly** | HAN's "accept work" is an approved effect. The stage strip (working → proposed → approved → pushed → merged) is a useful UI idea. |
| When-clause keybindings | t3code `contracts/keybindings.ts` + `shared/keybindings.ts` | **Yes** | Board, drill-down and inbox each need contextual keys (j/k, a = approve…). A user-editable JSON is cheap to adopt. |
| A diff renderer off the main thread | `@pierre/diffs` worker pool (t3code `DiffWorkerPoolProvider.tsx`; Intent `diff-ipc-batcher.ts`) | **Yes (dependency)** | Live diffs update constantly, so the main thread must stay free. |
| Agent processes outlive the UI and daemon | AO detached ACP hosts; t3code server; Intent sidecar | Already in HAN | It confirms the han-node supervisor design. The GUI should never own agent processes. |

Design tensions observed:
- **t3code** trades decoupling for velocity: Effect everywhere, 12k-line components.
- **AO** pays for decoupling in one package (product-ui), and is explicit that the rest is "migration debt" (`DESIGN.md`).
- **Intent** has a clean backend seam but Redux-saga coupling inside the UI.
- **All three** chose Electron, and **none** has team or multi-person concepts. "Who" is always the local user.

---

## 5. Strategist: ranked patterns for HAN

**1. Typed session item stream (render tool calls from data, not text)**
- **Pattern:** a face renders a session as an ordered list of typed items with stable app ids and a lifecycle status, never as a terminal byte stream.
- **Mechanism:**
  - Extend `AgentEvent` and the `Session{id}` view to emit items: `Message{role, text_delta}`, `Reasoning`, `Plan{entries}`, `ToolCall{id, kind, title, status, locations[], content[]}`, `ApprovalRequest{effect_id, options[]}`, `FileChange`, `TurnEnded`.
  - For `ToolCall`, `kind` takes ACP's ToolKind values (read, edit, delete, move, search, execute, think, fetch, other), `status` is pending, in_progress, completed or failed, and `content` holds text, diff or terminal blocks.
  - Carry over ACP's `tool_call` / `tool_call_update` shapes nearly verbatim (han-host already speaks ACP).
- **Value:** the drill-down gets rich tool rendering for every ACP agent with no per-harness code. Agent tools (`team.read`) get the same structure.
- **Where it applies:** `crates/han-host` (the AgentDriver `events()`); `crates/han-node` (the Session view); the GUI's `session/` components.
- **Build vs buy:** borrow the pattern (t3code `orchestrationV2.ts`, AO `ActivityKind`). Keep ACP as the vocabulary.

**2. One client seam with Tauri, JSON-RPC and Mock implementations**
- **Pattern:** the UI depends on one TS interface mirroring the node API; transports are swappable, and a fixture-backed mock is first-class.
- **Mechanism:**
  - `interface HanClient { call(cmd: Command): Promise<CallResult>; view(q: Query, onEvent: (e: ViewEvent) => void): Unsubscribe }`.
  - `TauriClient`: `invoke('han_call', {cmd})`, and `invoke('han_view', {q, onEvent: new Channel<ViewEvent>()})`. On the Rust side, `fn han_view(q: Query, on_event: tauri::ipc::Channel<ViewEvent>)` pumps `node.view(q)` into the channel. Tauri recommends Channels over events for "ordered, high-throughput" streams (https://v2.tauri.app/develop/calling-frontend/).
  - `RpcClient`: the daemon's JSON-RPC, for a browser dev build.
  - `MockClient`: scripted fixtures.
  - Generate the TS types from the Rust serde enums (ts-rs or specta) in CI.
- **Value:** UI work can start before han-node is done. Component tests run without Rust. The same UI can later attach to a remote daemon.
- **Where it applies:** `apps/han-gui/src-tauri` (new), `apps/han-gui/src/client/`; types from `crates/han-node` API types (respects the §4 dependency rule).
- **Build vs buy:** build (about 300 lines). Pattern from Intent `src/lib/client/`.

**3. A bounded view stream with gap detection**
- **Pattern:** each subscription carries a per-view `seq`. The producer keeps a small replay ring; on lag or a gap the consumer drops its state and takes a new snapshot.
- **Mechanism:**
  - `ViewEvent::{Snapshot{seq, rows}, Diff{seq, ops}, Resync}`.
  - In the GUI store: `if (e.seq !== last + 1) resubscribe()`.
  - Cap the replay by bytes and count. codeg uses 128 KiB and 128 events, and stores no event over 64 KiB.
- **Value:** live diffs and streams can flood a webview. This keeps memory bounded, and it doubles as a check of the "snapshot + diffs = fresh read" property in PLAN §5a.
- **Where it applies:** `crates/han-node` views; `apps/han-gui/src/client/useView.ts`.
- **Build vs buy:** borrow the pattern (codeg `src-tauri/src/acp/event_stream.rs`, Apache-2.0).

**4. Board = derived columns plus attention zones, rendered by a portable package**
- **Pattern:** the board's column and "needs-you" zone are pure functions of durable facts; card views take neutral models and injected labels and actions.
- **Mechanism:**
  - Fork AO `packages/product-ui` (`SessionsBoardView.tsx`, `session-models.ts`, `session-presentation.ts`, `AgentAvatar.tsx`) into `apps/han-gui/packages/board-ui`.
  - Replace AO's PR-centric `SessionStatus` with `StatusItem.state`, and its columns with HAN's (for example by state, or swimlanes by person).
  - Add zones `approval`, `overlap`, `blocked` and `stale`.
  - Keep the import-boundary test.
- **Value:** the MVP's core view arrives with a proven, decoupled card, grid and archive layout.
- **Where it applies:** the GUI board; the same derivation helps `han-tui` (same column logic, rendered in ratatui).
- **Build vs buy:** borrow the code (Apache-2.0), adapted.

**5. Diffs and file trees as adopted dependencies, with worker rendering**
- **Pattern:** one diff and tree engine, with tokenizing off the main thread.
- **Mechanism:**
  - `@pierre/diffs` (React binding plus `@pierre/diffs/worker`) and `@pierre/trees`.
  - Port t3code `DiffWorkerPoolProvider.tsx` (MIT) and AO `AoDiffFile.tsx` + `pierreTheme.ts`.
  - Feed it unified patches from han-timeline (or the `git diff` fallback).
- **Value:** the drill-down's diff and "files touched" views, at the quality the leading apps ship.
- **Where it applies:** the GUI `session/DiffPane`, `session/FilesTouched`; the overlap view (side-by-side hunks).
- **Build vs buy:** **adopt the dependency.** Diff tokenizing and virtualization is a genuinely hard problem; the license is Apache-2.0 and it is maintained (pierrecomputer/pierre pushed 2026-10-11). `@pierre/trees` is still beta (1.0.0-beta.6), so pin it.

**6. Decisions UI: a dock in the session, plus an inbox across sessions**
- **Pattern:** a decision the agent is blocked on docks above the composer, with the options supplied by the request. The same card component appears in a global inbox sorted by age and risk.
- **Mechanism:** port t3code `ComposerPendingApprovalPanel/Actions.tsx` (MIT, about 180 lines) or AO `ApprovalCard` and `ElicitationDock` (Apache-2.0). Then build `ApprovalsInbox` on `view(Approvals{actor})`: show the effect kind, the plan JSON, the policy rule that triggered it, the requesting session, and approve or cancel buttons.
- **Value:** PLAN §1.7 and the per-space gate are HAN's differentiator, and nobody else has a cross-session approvals inbox.
- **Where it applies:** the GUI `inbox/`, `session/DecisionDock`; `Command::EffectApprove/EffectCancel`.
- **Build vs buy:** borrow the card pattern and code; build the inbox.

**7. Contextual keybindings plus a command palette**
- **Pattern:** keybindings declared as data with when-clauses, editable by the user, and a palette over the same command registry.
- **Mechanism:** port t3code `packages/contracts/src/keybindings.ts` + `packages/shared/src/keybindings.ts` (MIT, about 700 lines; swap the Effect Schema for zod or plain TS), and use `cmdk` for the palette.
- **Value:** board triage (j/k, Enter to drill in, `a`/`x` on inbox items) is keyboard-heavy.
- **Where it applies:** GUI-wide.
- **Build vs buy:** borrow the code.

**8. Change lifecycle strip and per-turn checkpoints as the timeline fallback**
- **Pattern:** show each session's work as stages (live edits → proposed → approved → pushed → merged), and allow diffing between any two turns.
- **Mechanism:** han-timeline supplies the ops. As a cross-check and fallback, han-host writes a git ref per turn (t3code `refs/t3/checkpoints` style) and snapshots through a temporary index (AO).
- **Value:** the "live timeline" drill-down needs a scrubber and turn markers even before han-timeline is feature-complete.
- **Where it applies:** `crates/han-host` (checkpoints), the GUI `session/Timeline`.
- **Build vs buy:** borrow the pattern; build the UI. Intent's `TimeScrubber.svelte` is a 40-line idea, not a component.

---

## 6. Recommendations

### 6.1 Frontend stack for the Tauri GUI
- **Shell:** Tauri 2. The Rust side embeds han-node and exposes two commands, `han_call(cmd)` and `han_view(q, channel)`, using `tauri::ipc::Channel<ViewEvent>` for streams.
- **UI:** **React 19 + TypeScript + Vite**, **TanStack Router**, **zustand** (one store per live view, fed by `useView(query)` through `useSyncExternalStore`).
  - Skip TanStack Query for views. Streams replace polling.
  - Skip Effect (t3code) and Redux-saga (Intent). Both are heavy to adopt and not needed for a two-method API.
- **Styling:** **Tailwind v4** + **shadcn/ui** components.
  - The Base UI flavour matches t3code's MIT `components/ui/*`; Radix matches AO. Either works. Pick **Base UI** if t3code is the main lift source.
  - Import AO's design tokens only for the board package.
- **Libraries:**
  - `@pierre/diffs`, `@pierre/trees` (pinned);
  - `@legendapp/list` or `@tanstack/react-virtual` for the transcript;
  - `react-markdown` + `remark-gfm`; `cmdk`; `lucide-react`; `motion` (needed by AO product-ui).
  - Later: `@xterm/xterm` 6 if a terminal is ever needed. The MVP drill-down shows the agent stream, not a PTY.
- **Why React:** t3code, AO, codeg, jean, vibe-kanban, opcode and acp-components are all React. The only Svelte source (Intent) is not liftable anyway.
- **Risk to spike early** [?, not verified here]: `@pierre/diffs` workers and Shiki inside all three Tauri webviews (WebView2 on Windows, WKWebView on macOS, WebKitGTK on Linux). WebKitGTK is the usual weak spot.

### 6.2 Borrow list

Effort: S < 1 day, M 1–3 days, L about 1–2 weeks.

| # | Component | Source repo / path | License (duty) | Effort |
|---|---|---|---|---|
| 1 | Diff rendering + worker pool | **dependency** `@pierre/diffs` (https://github.com/pierrecomputer/pierre); integration reference: t3code `apps/web/src/components/DiffWorkerPoolProvider.tsx`, `components/diffs/StyledDiffCodeView.tsx` | Apache-2.0 (dependency: ship its license); t3code MIT (keep the notice) | S |
| 2 | Diff theme / single file view | AO `frontend/src/renderer/components/diffs/AoDiffFile.tsx`, `pierreTheme.ts` | Apache-2.0 (copy: license, retained notices, "modified" marks) | S |
| 3 | Files-touched tree | **dependency** `@pierre/trees` (beta); or t3code `apps/web/src/components/chat/ChangedFilesTree.tsx` | Apache-2.0 / MIT | S |
| 4 | **Team board** (grid, cards, archive, status and attention model, agent avatars) | AO `packages/product-ui/src/{SessionsBoardView.tsx, session-models.ts, session-presentation.ts, AgentAvatar.tsx, agents.ts, formatting.ts, utils.ts, import-boundary.test.ts}` + `frontend/src/styles/tokens.css` | Apache-2.0 (package.json says MIT; follow Apache) | M (re-model onto `StatusItem`, add person, issue, overlap and approval zones) |
| 5 | Tool-call rows | t3code `apps/web/src/components/chat/WorkLog.tsx`; classification logic from `packages/shared/src/toolActivity.ts` and `packages/client-runtime/src/work-log/toolPresentation.ts` (re-typed to ACP `ToolKind`) | MIT | M |
| 5b | Alternative tool, reasoning and message blocks | Vercel AI Elements (https://github.com/vercel/ai-elements; shadcn registry) or codeg's adapted copies `src/components/ai-elements/{tool,reasoning,message,code-block}.tsx` | Apache-2.0 ("Copyright 2023 Vercel, Inc.") | S–M |
| 6 | Tool-activity taxonomy (reference) | AO `frontend/src/renderer/types/conversation.ts`, `components/chat/ActivityRun.tsx`, `activity-command.ts`, `action-pill.ts` | Apache-2.0 | S to read, M to port |
| 7 | Decision dock / approval card | t3code `apps/web/src/components/chat/ComposerPendingApprovalPanel.tsx`, `ComposerPendingApprovalActions.tsx`; or AO `ApprovalCard` in `components/chat/ChatTimelineItems.tsx` (~line 2477), `components/chat/ElicitationDock.tsx` | MIT / Apache-2.0 | S (t3code) or M (AO) |
| 8 | Keybindings with when-clauses | t3code `packages/contracts/src/keybindings.ts`, `packages/shared/src/keybindings.ts`, `docs/user/keybindings.md` | MIT | M |
| 9 | UI primitives | t3code `apps/web/src/components/ui/*` (Base UI shadcn) or shadcn/ui upstream | MIT | S |
| 10 | Client seam (pattern) | Intent `src/lib/client/{app-client.ts, live/, mock/}` | Apache-2.0 (pattern only; no code needed) | S |
| 11 | View-stream ring buffer (pattern) | codeg `src-tauri/src/acp/event_stream.rs` | Apache-2.0 (pattern only) | S |
| 12 | Worktree snapshot through a temporary index (pattern, han-host) | AO `backend/internal/adapters/workspace/gitworktree/commands.go` | Apache-2.0 (pattern only) | S |
| 13 | Per-turn checkpoint refs (pattern, han-host fallback) | t3code `apps/server/src/checkpointing/CheckpointStore.ts` | MIT (pattern only) | S–M |
| 14 | (Later) Terminal | `@xterm/xterm` 6 + addons (dependency); reference AO `components/XtermTerminal.tsx`, `hooks/useTerminalSession.ts` (frame batching) | MIT / Apache-2.0 | L if needed |

### 6.3 License mechanics for an "Apache-2.0 OR MIT" project
- **MIT sources** (t3code, shadcn/ui, xterm.js):
  - copying is fine;
  - keep the copyright and permission notice ("Copyright (c) 2026 T3 Tools Inc.") with the copied portions, for example as a file header plus an entry in `THIRD_PARTY_NOTICES`;
  - MIT allows sublicensing, so the files can sit inside the dual-licensed tree.
- **Apache-2.0 sources** (AO, Intent/cloudlands-fe, pierre, AI Elements, codeg, jean):
  - **copied code stays Apache-2.0.** A recipient who takes HAN under MIT still gets those files only under Apache-2.0;
  - so mark them `SPDX-License-Identifier: Apache-2.0` (not the dual expression) and keep them in an identifiable directory;
  - include the Apache license text;
  - keep the original copyright notices;
  - add "modified by HAN" change notices to changed files (§4(b));
  - if the source has a **NOTICE**, carry its attribution text in HAN's NOTICE (§4(d)). This applies to **Intent/cloudlands-fe**; AO has none;
  - prefer *depending on* Apache packages (`@pierre/*`) over copying them. That keeps HAN's own files cleanly dual-licensed.
- **Do not copy** from: opcode, athas, kandev (AGPL-3.0), superset (Elastic-2.0), paseo (mixed custom license), desktop-cc-gui (no license).
- Agent logos are third-party trademarks. Fetch them per agent with permission, or use neutral avatars.

### 6.4 Build ourselves (nothing to borrow)
1. **The board's data and filters:** the person × agent × issue model, swimlanes by person, filters by repo, label, path and person (PLAN §1.3), and `stale` derived from the time since `seq`. Only the card and grid shell come from AO.
2. **Overlap alerts:** badges on cards, plus a pairwise view of two sessions' hunks on the same file, at three tiers (claim, hunk range, merge-tree conflict, per PLAN §5a). No app has this. AO only reports PR merge conflicts.
3. **Approvals inbox:** cross-session and cross-person, showing the effect plan, the policy rule, signer state and receipts. The verifiable-approval UI is unique to HAN.
4. **Live work timeline:** a scrubber over han-timeline ops, per-op author attribution, "go live", and diffs between two points. Only fragments of the idea exist elsewhere (Intent `TimeScrubber`, t3code per-turn diffs).
5. **The Tauri bridge and `HanClient`:** the commands, the Channel pump, TS type generation, the mock fixtures.
6. **Trust and identity surfaces:** space membership, keys and per-space trust. None of the apps has multi-person concepts.

---

## 7. Unverified or open
- t3code's mobile apps and their stacks were not inspected beyond the folder listing.
- The exact ACP feature coverage of t3code's `provider-acp` (permissions, terminals, `fs/*`) was not traced end to end.
- I did not check whether AO's MIT `package.json` fields reflect an intent to dual-license. Ask the maintainers before relying on MIT.
- Whether Intent's `TimeScrubber.svelte` is wired anywhere: only a grep was done (no importer found).
- `@pierre/diffs` performance in WebKitGTK and WebView2 was not tested (nothing was run).
- Shells for some candidates in §3 (desktop-cc-gui's React version, OpenCovibe's transports) come from search snippets [3P].
- The pi-acp package identity (`pi-acp` vs `@victor-software-house/pi-acp`) and its version line need checking against PLAN §5a.
- Contributor counts come from the GitHub contributors API with `anon=1`, so they include anonymous commit emails and may overcount.

## 8. Sources
- https://github.com/pingdotgg/t3code (README, LICENSE, source at `511d20c6`); https://t3.codes
- https://github.com/OrchestratorInc/agent-orchestrator (README, LICENSE, DESIGN.md, docs/architecture.md, source at `b92eb51`)
- https://github.com/intent-hq/cloudlands-fe (README, LICENSE, NOTICE, source at `84d75f3`); https://github.com/intent-hq/intent; https://intentapp.dev
- https://github.com/pierrecomputer/pierre (Apache-2.0); npm `@pierre/diffs` 1.5.2 and `@pierre/trees` 1.0.0-beta.6 (license `apache-2.0`, `npm view`)
- https://v2.tauri.app/develop/calling-frontend/ (Channels vs events)
- https://agentclientprotocol.com/get-started/clients (ACP client list)
- https://github.com/spacering-net/codeg; https://github.com/coollabsio/jean; https://github.com/winfunc/opcode; https://github.com/zhukunpenglinyutong/desktop-cc-gui; https://github.com/AnyiWang/OpenCovibe; https://github.com/recailai/jockey; https://github.com/savvasioakeim/OctoShell; https://github.com/formulahendry/acp-ui; https://github.com/zvzuola/acp-components; https://github.com/BloopAI/vibe-kanban; https://www.vibekanban.com/blog/shutdown; https://github.com/vercel/ai-elements; https://github.com/kdlbs/kandev; https://github.com/superset-sh/superset; https://github.com/getpaseo/paseo; https://github.com/athasdev/athas

# Agent plumbing research: han-host / Gate / han-mcp / sandbox / han-exec

Researched 2026-10-12. Versions, stars and push dates come from the GitHub API, crates.io and npm on that date unless a line says otherwise.
"Verified" means I read the source repo or the official doc page. "Asserted" means a project's own docs say so. "Unverified" means it is background knowledge or third-party; check it before relying on it.

---

## 0. Answers first

1. **Can AgentDriver simply be an ACP client?** No, not as the only driver. Build a generic `drivers/acp` *in addition to* a native `drivers/claude_code`.
   - ACP permission requests are **agent-discretionary**. The spec says the agent "MAY request permission".
   - The Claude adapter forwards only what Claude Code's permission engine leaves unresolved: it bridges the SDK's `canUseTool` to `session/request_permission`.
   - Claude's docs say "Auto-approved tools never reach `canUseTool`". Auto-approved means reads in the working directory, read-only bash, allow rules, `acceptEdits`, and sandboxed bash.
   - The ACP answer is one of four options (`allow_once`, `allow_always`, `reject_once`, `reject_always`). There is no "propose/park" outcome.
   - ACP v2 removes the client fs/terminal methods, which were the only client-side interception points.
   - So ACP can only give "prompt-level" gate fidelity. Claude Code's PreToolUse hook gives "every-call" fidelity.
2. **The most stable Claude integration surface:**
   - the `claude` CLI in `-p` stream-json mode;
   - with the Gate injected as a **PreToolUse hook** through `--settings`;
   - the hook fails closed with `onFailure: "block"`, and uses `defer` for Propose;
   - `--permission-prompt-tool` only as a fallback.
   - There is no official Rust SDK.
3. **MCP:** reuse `rmcp`. It is official, Apache-2.0, at 3.5.1, and has stdio and streamable-HTTP server transports.
4. **Sandbox:**
   - No option is both embeddable from Rust and mature on all three OSes.
   - The best embeddable candidate is **Microsoft MXC** (`mxc-sdk` 1.0.0, MIT, Rust, released 2026-10-07).
   - The universal pattern: deny direct egress at the OS level, and route everything through a host-side proxy that holds the domain allow-list. srt, Codex and MXC all do this.
   - Claude Code's own sandbox only covers shell commands. File tools, WebFetch and MCP servers run outside it, and on native Windows it does not sandbox at all. So HAN must confine the **whole agent process**.
5. **Executor building blocks:**
   - Shell out to `git` for push: gix has no push, and git2 is possible but weaker.
   - Use gix for reads.
   - Use octocrab for GitHub.
   - Write your own SQLite idempotency table, not a job framework.
6. **A2A:** an official Rust SDK exists (`a2a-lf` 0.4.1). Skip it for the v1 core. A2A is a later face/integration candidate, not the agent host.

---

## 1. Agent Client Protocol (ACP)

**What it is.** JSON-RPC 2.0 over stdio (the Rust SDK adds HTTP/WebSocket) between a *client* (editor or host) and a *coding agent*. Zed started it; it now has its own org, `agentclientprotocol`, with a GOVERNANCE.md.

**Facts (verified)**
- **Spec repo** `agentclientprotocol/agent-client-protocol`:
  - Apache-2.0, 4.4k stars, pushed 2026-10-12.
  - Released v1.11.0, schema-v1.25.0 and **schema-v2.0.0-alpha.8**, all on 2026-10-09.
  - v2 is labelled draft. The migration guide says to "gate v2 support behind explicit version negotiation and feature flags".
- **Rust SDK** `agentclientprotocol/rust-sdk`:
  - Crate `agent-client-protocol` 3.3.0, Apache-2.0, about 2.0M recent downloads.
  - **Churn: majors 0.15 (Jun 22), 1.x (Jul 20), 2.x (Sep 18), 3.x (Oct 9 2026).**
  - Workspace crates: `-http` (HTTP/SSE/WebSocket), `-rmcp` (rmcp bridge), `-conductor` (proxy chains), `-polyfill`, `-trace-viewer`, plus the `yopo` example client.
  - Roles: Client, Agent, **Proxy**, Conductor.
  - **MCP-over-ACP** (the client supplies MCP servers through the ACP channel) is behind the `unstable_mcp_over_acp` feature.
- **Agents.** The registry repo `agentclientprotocol/registry` lists about 45. Some speak ACP natively:
  - Gemini CLI (`@google/gemini-cli@0.63.0 --acp`, Apache-2.0);
  - GitHub Copilot CLI (`@github/copilot@1.0.95 --acp`, proprietary);
  - goose (`goose acp`, Apache-2.0);
  - OpenCode (`opencode acp`, MIT);
  - plus Cursor, Devin, Junie, Kimi, Qwen Code, Mistral Vibe, Cline and others (not inspected individually).
- **Agents via adapters** (TypeScript, run with npx):
  - Claude Code via `@agentclientprotocol/claude-agent-acp` 0.89.1. Apache-2.0; authors listed as Anthropic, Zed and JetBrains. Depends on `@anthropic-ai/claude-agent-sdk` 0.3.293.
    - The registry entry says "proprietary", but the repo LICENSE is Apache-2.0. The adapter is open; Claude Code itself is proprietary.
  - Codex via `@agentclientprotocol/codex-acp` 2.2.2. Apache-2.0, built on the Codex App Server.
    - The old Rust `zed-industries/codex-acp` is **archived** (last push 2026-07-22). Development moved to the TypeScript repo.

**Protocol surface relevant to HAN (verified, v1 docs)**
- `session/new` takes `mcpServers`; `session/prompt` runs a turn; `session/cancel` cancels it. The load/resume/list/close methods are capability-gated in v1.
- `session/update` variants:
  - `agent_message_chunk`, `agent_thought_chunk`;
  - `tool_call` and `tool_call_update`, carrying `toolCallId`, `name`, `title`, `kind`, `status`, `content`, **`locations`** and `rawInput`/`rawOutput`;
  - `plan`, `usage_update`, `config_option_update`, and others.
  - `kind` is one of read / edit / delete / move / search / execute / think / fetch / switch_mode / other.
- `session/request_permission`: the agent sends the tool call plus `options[]`, each with `optionId`, `name` and `kind`. The client answers `{outcome: selected, optionId}` or `cancelled`. Clients "MAY automatically allow or reject".
- **v2 changes:**
  - The `session/prompt` response only acknowledges; completion moves to `state_update` (`running`/`idle`/`requires_action`).
  - Updates are upserts.
  - **The `fs/*` and `terminal/*` client methods and session modes are removed.**
  - `request_permission` gains a required `title`.
  - Unknown enum values are tolerated.

**Mapping to AgentDriver**

| HAN | ACP v1 |
|---|---|
| `start` | `initialize` + `session/new` (cwd, mcpServers) |
| `turn(input, mode)` | `session/prompt`. Mode becomes `session/set_mode` or `set_config_option` (adapter-specific ids) |
| Output | `agent_message_chunk` |
| ToolCall | `tool_call` / `tool_call_update` (`kind`, `rawInput`) |
| FileTouched | `tool_call.locations` plus edit/delete/move kinds |
| Tests | not in ACP. Derive from `execute` output (heuristic) |
| Summary | not in ACP. Use the final message or `plan` |
| TurnEnded | prompt response `stopReason` (v1) / `state_update` idle (v2) |

**Mapping to the Gate**
- Allow becomes `allow_once`; Deny becomes `reject_once`.
- I found no field for a free-text reason back to the agent; check `_meta`.
- **There is no Propose.** The options are:
  - (a) Hold the request open until approval. This does not survive a restart.
  - (b) Reject, then tell the agent in the next prompt.
  - (c) Best: make "propose an effect" an explicit **han-mcp tool** that the agent calls, and deny the raw action.
- **Calls the agent auto-approves never reach the Gate.** For Claude-via-ACP that includes in-cwd reads, read-only bash, `acceptEdits` writes and sandbox-auto-allowed bash.
- So `Mode::Strict` cannot be honoured on ACP drivers unless the agent offers an "always ask" mode. Whether it does varies per agent; not verified per agent.

**Verdict: INTEGRATE as a second driver family; LEARN-FROM the Proxy/Conductor idea.**
- Use `agent-client-protocol` as `drivers/acp`, to get Gemini, Copilot, goose, OpenCode and Codex for free.
- Each driver declares its **gate fidelity** (`every_call` vs `prompt_only`).
- Pin the crate version and keep it behind the trait: three majors in four months.
- When ACP v2 stabilises, MCP-over-ACP is a clean way to inject han-mcp.

Sources: https://github.com/agentclientprotocol/agent-client-protocol (docs/protocol/v1/tool-calls.mdx, docs/protocol/v2/migration.mdx) · https://github.com/agentclientprotocol/rust-sdk · https://crates.io/crates/agent-client-protocol · https://github.com/agentclientprotocol/registry · https://github.com/agentclientprotocol/claude-agent-acp (src/acp-agent.ts: canUseTool -> requestPermission) · https://github.com/agentclientprotocol/codex-acp · https://github.com/zed-industries/codex-acp

---

## 2. Claude Code headless mode / the Claude Agent SDK

**Versions (npm, 2026-10-12)**
- `@anthropic-ai/claude-code` 2.1.296 (2026-10-09), license "SEE LICENSE IN README" (proprietary).
- `@anthropic-ai/claude-agent-sdk` (TypeScript) 0.3.296.
- Python `claude-agent-sdk`: the repo is MIT and bundles CLI 2.1.296. The README says use is governed by Anthropic's **Commercial Terms of Service**.
- **No official Rust SDK.** The crates.io crate `claude-agent-sdk` 0.1.1 (Sep 2025) is owned by `dhuseby`, and its listed repo `anthropics/claude-agent-sdk-rust` returns 404. Treat it as unofficial.

**Headless surface (verified, cli-reference and headless docs)**
- Run with `claude -p --input-format stream-json --output-format stream-json --verbose`.
- Useful companion flags:
  - `--include-partial-messages`, `--include-hook-events`, `--replay-user-messages`, `--forward-subagent-text`;
  - `--session-id <uuid>`, `--resume <id|path.jsonl>`, `--fork-session`, `--no-session-persistence`;
  - `--mcp-config` + `--strict-mcp-config`, `--settings <file|json>`, `--setting-sources`;
  - `--tools`, `--allowedTools`/`--disallowedTools`, `--permission-mode` (`default|acceptEdits|plan|auto|dontAsk|bypassPermissions`);
  - `--permission-prompt-tool <mcp tool>`, `--permission-prompts host|none` (v2.1.259+), `--max-turns`, `--max-budget-usd`.
- **`--bare`** skips discovery of hooks, skills, plugins, `.mcp.json` and CLAUDE.md. The docs call it "the recommended mode for scripted and SDK calls" and say it "will become the default for `-p`".
- **Security note.** Without `--bare`, a `-p` session runs the project's `.claude/settings.json` hooks and connects `.mcp.json` servers "even in a folder you've never trusted". The repo under the agent is untrusted input, so always use `--bare` or explicit `--setting-sources`.
- Stream output includes:
  - `system/init`, with `capabilities[]` for feature detection (v2.1.205+), `mcp_servers` and `mcp_server_errors`;
  - `assistant`/`user` messages with `parent_tool_use_id` for subagents;
  - `stream_event`, `system/api_retry`, `permission_denied`;
  - a final `result` with `session_id`, cost and `permission_denials`.
- SIGTERM leaves the turn unfinished; use SIGINT or interrupt.

**Permission evaluation order (verified, agent-sdk/permissions)**
- hooks -> deny rules -> ask rules -> permission mode -> allow rules -> `canUseTool` / permission-prompt tool.
- The docs say: "To gate every tool call regardless of mode and rules, use a `PreToolUse` hook." A hook deny "applies even in `bypassPermissions` mode".
- A hook `allow` does **not** override deny or ask rules.
- Claude's own Bash sandbox auto-approves sandboxed commands, skipping the prompt tool.

**Hooks (verified, hooks docs)**
- 33 events, including PreToolUse, PermissionRequest, PermissionDenied, PostToolUse(/Failure/Batch), SubagentStart/Stop, Stop, FileChanged, CwdChanged and SessionStart/End.
- Handler types: `command`, `http` (POSTs the JSON), `mcp_tool` (calls a tool on a configured MCP server), `prompt`, and `agent` (experimental).
- **PreToolUse output:** `hookSpecificOutput.{hookEventName, permissionDecision: allow|deny|ask|defer, permissionDecisionReason, updatedInput, additionalContext}`.
  - When several hooks answer, the precedence is deny > defer > ask > allow.
  - The input includes `mcp_server.source` (v2.1.274+). Base trust on `source`, not on the name.
- **Failure semantics:**
  - Hooks **fail open** by default: a timeout or error lets the action proceed.
  - `"onFailure": "block"` on command/http hooks makes them fail closed. It **requires v2.1.295+**, one release before current.
  - Exit code 2 blocks; exit 1 does **not** block.
  - An SDK callback hook that times out blocks the call.
- **`defer`** (`-p` only):
  - The tool doesn't run, and the process exits with `stop_reason: "tool_deferred"` plus `deferred_tool_use {id,name,input}`.
  - Resume with `claude -p --resume <id>` and the same permission host. PreToolUse fires again for the same call.
  - There is no timeout; the session stays on disk for `cleanupPeriodDays` (30 by default).
  - **Ignored when the turn makes several parallel tool calls.**
  - This is a durable "park until approved" primitive, and maps onto Propose(EffectPlan) / StatusItem `waiting-approval`.
- **Unverified:** whether PreToolUse fires for tool calls *inside* subagents. The doc portion I read was not explicit. **Critical; test it in the M4 spike.**

**SDK wire protocol (verified in source; not a documented public contract)**
- The Python SDK spawns `claude --output-format stream-json --verbose --input-format stream-json`.
- It multiplexes `control_request` / `control_response` on the same pipes, with subtypes `initialize`, `can_use_tool`, `hook_callback` and `mcp_message` (in-process SDK MCP servers).
- I found no public spec page for it.

**Rust ports (third-party)**
- `claude-codes` 2.1.297, from meawoppl/rust-code-agent-sdks.
  - Apache-2.0, 28 stars, pushed 2026-10-12.
  - Typed stream-json; the version tracks the *tested* CLI (tested against 2.1.296).
  - Siblings: `codex-codes`, `opencode-codes` and others. It warns when the installed CLI's version differs from the tested one.
  - Learn-from, or borrow types. Single maintainer.
- `cc-sdk` 0.8.1 (MIT, stale since Apr 2026). Skip.
- `claude-sdk-rs` 2.0.0 (low downloads). Skip.

**Most stable integration surface, ranked**
1. Documented CLI flags + hooks JSON, which carry explicit per-version notes.
2. stream-json message types (documented; feature-detect through `system/init.capabilities`).
3. The control protocol (undocumented).

**Recommended `drivers/claude_code` shape (for the M4 spike)**
1. Run `claude -p --bare --input-format stream-json --output-format stream-json --verbose --session-id <uuid> --strict-mcp-config --mcp-config <han-mcp> --settings <inline json with PreToolUse hook>`.
2. Make the hook `type: "http"` (to han-host loopback) or `"mcp_tool"` (on han-mcp), with `onFailure: "block"`.
3. Gate verdicts map as follows:
   - Allow becomes `allow` (plus optional `updatedInput`).
   - Deny becomes `deny` with a reason.
   - Propose becomes `defer`, then park, approve and resume. If it can't be deferred (parallel calls), use `deny` with "proposed as effect E".
4. Use `--permission-prompt-tool` only for `ask` fall-through.
5. **Unverified:** whether hooks supplied through `--settings` load under `--bare`. Bare skips hook *discovery*; `--settings` is listed as the way to load settings. Spike it.

**Verdict: INTEGRATE the CLI directly from Rust. LEARN-FROM `claude-codes`. SKIP the unofficial Rust SDK crates.**

Sources: https://code.claude.com/docs/en/cli-reference · https://code.claude.com/docs/en/headless · https://code.claude.com/docs/en/hooks · https://code.claude.com/docs/en/agent-sdk/permissions · https://code.claude.com/docs/en/sandboxing · https://github.com/anthropics/claude-agent-sdk-python (src/claude_agent_sdk/_internal/query.py, _internal/transport/subprocess_cli.py) · https://www.npmjs.com/package/@anthropic-ai/claude-code · https://crates.io/crates/claude-codes · https://github.com/meawoppl/rust-code-agent-sdks · https://crates.io/crates/claude-agent-sdk

---

## 3. MCP in Rust

- **`rmcp`** (official, `modelcontextprotocol/rust-sdk`):
  - 3.5.1, released 2026-10-05. About 19M recent downloads, 4.0k stars.
  - Crate license Apache-2.0. The repo is mid-transition from MIT; some older contributions stay MIT.
  - Implements MCP spec **2026-07-28**, compatible with 2025-11-25.
  - Features:
    - `server` (default), `client`, `macros`;
    - `transport-io` (stdio), `transport-child-process`;
    - `transport-streamable-http-server` and `-session` (axum/tower), `transport-streamable-http-client` (reqwest, plus a unix-socket variant);
    - `auth` (OAuth2), `elicitation`.
  - **It supports a server over both stdio and HTTP.**
  - Churn: majors 0.17 (Feb), 1.x (Jun), 2.x (Jul), 3.x (Sep-Oct 2026).
  - Codex pins `rmcp =3.5.1`. ACP has an `agent-client-protocol-rmcp` bridge.
- Alternatives: `rust-mcp-sdk` 2.0.0 (Aug 2026), `pmcp` 2.22.7, `turbomcp` 3.6.0. All smaller, with no advantage for HAN. Skip.
- **Fit to han-mcp:**
  - Expose node-API commands and queries as MCP tools.
  - Use stdio when the agent spawns `han mcp` as a child.
  - Use streamable HTTP on loopback when a hook (`mcp_tool`/`http`) needs a long-lived host.
  - Inside a sandbox, the loopback port or unix socket must be allow-listed.
  - Claude Code supports `_meta["anthropic/requiresUserInteraction"]` on MCP tools to force a prompt; relevant if han-mcp exposes privileged tools.

**Verdict: REUSE `rmcp`.** Pin an exact version and isolate it in han-mcp. The dependency rule already does this.

Sources: https://github.com/modelcontextprotocol/rust-sdk · https://crates.io/crates/rmcp

---

## 4. Sandboxing

**Key fact for HAN:**
- Claude Code's built-in sandbox covers "shell commands only. Claude's file tools, MCP servers, and hooks run outside it".
- `allowedDomains` doesn't limit WebFetch.
- "On native Windows, Claude Code runs commands unsandboxed."
- There is an unsandboxed-retry escape hatch (`dangerouslyDisableSandbox`). `allowUnsandboxedCommands: false` turns it off.
- So the HAN egress sandbox must wrap the **whole `claude` process tree**.
- The allow-list must include the model API endpoint, plus HAN's own loopback endpoints for hooks and MCP.

Source: https://code.claude.com/docs/en/sandboxing

### 4a. Anthropic sandbox-runtime (`srt`)
- `anthropics/sandbox-runtime`: TypeScript, Apache-2.0, v0.0.79 (2026-10-07), 5.5k stars. "Beta Research Preview"; APIs may change.
- **Mechanism by OS:**
  - macOS: generated Seatbelt profile.
  - Linux: bubblewrap with the network namespace removed. Traffic can only reach host-side HTTP and SOCKS5 proxies through bind-mounted unix sockets.
  - Windows (**alpha**): runs as a dedicated `srt-sandbox` local user, with a WFP egress fence keyed on that SID and per-session ACEs. It needs a one-time elevated `windows-install` (one UAC prompt).
- **Egress allow-list:** yes, domain allow/deny enforced in the proxies.
- **Extras:**
  - experimental TLS termination with `filterRequest`;
  - `credentials` mask mode (a sentinel in the sandbox, the real secret substituted on egress to listed hosts);
  - `--control-fd` for live config updates;
  - a violation log on macOS.
- **Embeddable from Rust?** Only as a subprocess (`srt <cmd>`, needs Node).
  - The Windows helper `srt-win` *is Rust* (`vendor/srt-win-src`, lib API `srt_win::run_from_args`).
  - It is not on crates.io. I did not check its per-crate license; the repo is Apache-2.0.
- **Verdict: LEARN-FROM, and optionally INTEGRATE as a subprocess wrapper** for macOS/Linux early on.

### 4b. Codex sandbox (openai/codex, Rust, Apache-2.0, 128k stars)
- **`codex-linux-sandbox`:** bubblewrap for filesystem and namespaces, plus seccomp (seccompiler 0.5.0). Landlock is kept as "legacy". It prefers system `bwrap` and falls back to a bundled one. Works on WSL2, not WSL1.
- **`codex-network-proxy`:** a Rust HTTP proxy (3128) and SOCKS5 proxy (8081) enforcing allow/deny policy, with a "limited" read-only mode.
- **`codex-windows-sandbox`** has two modes:
  - "elevated": dedicated low-privilege sandbox users, firewall rules and ACLs; one admin-approved setup.
  - "unelevated": restricted token plus ACLs, with weaker network isolation.
- **`codex-mxc-sandbox`:** routes through Microsoft MXC with no admin. OpenAI's doc now recommends MXC "where supported".
- **`codex-execpolicy`:** Starlark `prefix_rule(pattern, decision=allow|prompt|forbidden, justification, match/not_match examples)`. The examples are validated when the policy loads.
- **`codex-process-hardening`:** no core dumps, no ptrace, strips `LD_PRELOAD` and `DYLD_*`.
- **None of these are published on crates.io.** You would need git dependencies into a huge, fast-moving workspace.
- **Verdict: LEARN-FROM.** execpolicy's "rules ship with examples tested at load" pattern suits HAN Policy and Gate rules. Vendor `network-proxy` only if our own proxy proves costly.

Sources: https://github.com/openai/codex/tree/main/codex-rs (linux-sandbox, network-proxy, windows-sandbox-rs, mxc-sandbox, execpolicy, process-hardening) · https://learn.chatgpt.com/docs/windows/windows-sandbox.md (redirect target of https://developers.openai.com/codex/windows.md). The firewall mechanics come from a third party and are unverified: https://codex.danielvaughan.com/2026/07/18/codex-cli-windows-sandbox-architecture-powershell-ast-safety-elevated-unelevated-appcontainer-restricted-tokens/

### 4c. Microsoft MXC (new; the most relevant find)
- `microsoft/mxc`: Rust, MIT. Repo created 2026-02-06, 2.7k stars, **v1.0.0 on 2026-10-07** (0.9.0 pre-release 2026-09-29).
- Crate `mxc-sdk` 1.0.0, plus .NET and Node SDKs. It is an in-process SDK that builds the engine and backends into your binary.
- **Backends:**
  - Windows 11: `processcontainer` by default. Alternatives: `wslc`, `isolation_session`, and the experimental `windows_sandbox`, `microvm` and `hyperlight`.
  - Linux: `bubblewrap` (default), `lxc`, `microvm`, `hyperlight`.
  - macOS: `seatbelt`.
- **Policy:** JSON requests with a stable schema, covering filesystem read-only / read-write / deny paths, network egress and ingress, and UI.
- **Network:**
  - On Windows, WFP egress filters are scoped to the container SID, with **no UAC per launch**. Default deny, with allow/block by IP/CIDR, protocol and port.
  - **Domain filtering is not native.** The recommended "proxy-only egress" model allows only a caller-provided loopback proxy, and sets `HTTP(S)_PROXY` and the WinHTTP proxy.
- Audit mode for writing policies. Optional telemetry, off unless consented and enabled.
- Codex depends on MXC through a pinned git revision.
- **Not verified:** minimum Windows builds (`docs/backends/process-container/os-version-support.md`), and real-world maturity. 1.0 is five days old.
- **Verdict: INTEGRATE candidate** for the han-host sandbox on all three OSes, paired with HAN's own Rust egress proxy.

Sources: https://github.com/microsoft/mxc (README, docs/backends/process-container/networking.md, docs/api-reference/rust/v1/types.md) · https://crates.io/crates/mxc-sdk

### 4d. Primitives
- **bubblewrap:**
  - C, LGPL-2.1 (COPYING), v0.13.0 (2026-09-22), 9.0k stars.
  - A binary, so you shell out. Network control is all-or-nothing (`--unshare-net`); an allow-list needs the proxy pattern.
  - Integrate through MXC, srt or Codex, not directly.
- **`landlock` crate:**
  - 0.4.7, MIT OR Apache-2.0, about 6M recent downloads. Linux only, in-process (restricts the caller and its children).
  - Good for **han-exec self-hardening**: lock the executor to its state directory plus the worktree.
  - Unverified in this pass: Landlock ABI v4+ can restrict TCP *ports* but has no domain allow-list.
- **`seccompiler`** 0.5.0 (rust-vmm, Apache-2.0 OR BSD-3-Clause): syscall filters. Codex uses it.
- **Skip:**
  - `birdcage` (GPL-3.0, last updated Apr 2024);
  - `extrasafe` (MIT, last updated Apr 2024);
  - `hakoniwa` (LGPL-3.0 with a linking exception; active at 1.8.0, but Linux-only and redundant).

### 4e. Windows options
- **AppContainer.** OpenAI rejected it for Codex (third-party account, unverified) because agents need broad reads of repos and toolchains. MXC keeps it only as a fallback, and its `privateNetworkClientServer` capability is bidirectional.
- **Separate local user + WFP + ACLs** (srt-win, Codex elevated): strong network control, but needs one admin setup.
- **MXC ProcessContainer:** WFP per container SID with no admin. The new default path.
- **WSL2:** Claude Code's Bash sandbox and Codex's bwrap both work there. You could run the host plus agent inside WSL2 with bwrap and a proxy. Your existing Podman-on-WSL setup also gives `--network=none` plus a proxy sidecar.

**Which can enforce egress allow-lists?**

| Option | Domain allow-list | Mechanism | Embeddable from Rust |
|---|---|---|---|
| srt | yes (mac/Linux; Windows alpha) | OS sandbox + HTTP/SOCKS proxy | subprocess (Node); srt-win as vendored Rust |
| Codex | yes | bwrap/seccomp, Windows sandbox, MXC + `codex-network-proxy` | git deps only |
| MXC | IP/CIDR natively; domains through your proxy | WFP / bwrap / seatbelt | **yes, `mxc-sdk` crate** |
| bubblewrap | no (all-or-nothing) | network namespace | binary |
| landlock | no (ports only, unverified) | LSM | yes, in-process |
| Podman (WSL) | via a proxy sidecar | network namespace | subprocess |

---

## 5. Executor building blocks (han-exec)

- **git:**
  - **gix** (gitoxide):
    - Crate 0.89.0, MIT OR Apache-2.0, about 12.5M recent downloads, 12k stars, very active.
    - `crate-status.md` still lists **push as unchecked** ("send-pack / receive-pack client plumbing", report-status and so on).
    - Codex uses gix 0.81 for reads.
    - Use it for reads: status, diff and rev-parse for FileTouched, and worktree inspection.
  - **git2** 0.21.0 (2026-10-06), bundling libgit2 1.9.6 (from the `libgit2-sys` version string). MIT/Apache.
    - It supports push. Unverified background knowledge: its SSH and credential-helper parity with CLI git is weaker.
  - **Shell out to `git`** for push.
    - The executor owns credentials through `GIT_ASKPASS` or an executor-scoped credential helper, with `GIT_TERMINAL_PROMPT=0`.
    - It respects user config and hooks; consider disabling hooks with `-c core.hooksPath=`.
  - **Verdict:** shell out for push. REUSE gix for reads. Skip git2 unless you need in-process push.
- **GitHub: `octocrab`**
  - 0.54.3 (2026-10-08), MIT OR Apache-2.0, 1.4k stars, about 2.5M recent downloads. Still pre-1.0.
  - GitHub App JWT auth (`jwt-aws-lc-rs` by default), typed webhook payloads, and an `apps` handler.
  - **Verdict: REUSE** in han-exec (http/GitHub actions) and in `integrations/han-github`.
- **Durable jobs and idempotency**
  - `apalis` 1.0.0 with `apalis-sqlite` 1.0.0: both released 2026-10-12, MIT OR Apache-2.0. Freshly 1.0.
  - `underway` 0.2: Postgres only.
  - Restate: the SDK is MIT, but the server is **BSL-1.1** and is a separate service.
  - **Verdict: SKIP the frameworks. BUILD** a small SQLite ledger keyed by `IdempotencyKey`, with states planned -> running -> done or failed plus the outcome blob, using the outbox pattern. han-seq already uses SQLite.
  - Design actions as **desired-state**, so a retry is naturally idempotent. For example, git push becomes "set ref R from expected old sha A to B" (`--force-with-lease=R:A`). On retry, a remote already at B means done.
  - LEARN-FROM apalis only if you later need workers or cron.

Sources: https://github.com/GitoxideLabs/gitoxide/blob/main/crate-status.md · https://crates.io/crates/gix · https://github.com/rust-lang/git2-rs · https://github.com/XAMPPRocky/octocrab · https://crates.io/crates/apalis · https://crates.io/crates/apalis-sqlite · https://github.com/maxcountryman/underway · https://github.com/restatedev/restate (LICENSE)

---

## 6. Google A2A protocol

- **Spec** `a2aproject/A2A`: Apache-2.0, 26k stars, **v1.0.0 on 2026-03-12** and v1.0.1 on 2026-05-28.
  - Org-level SDKs exist for Python, JS, Java, Go, .NET and Rust, plus a TCK.
  - Linux Foundation stewardship is my inference from the crate name `a2a-lf`; I did not verify it further.
- **Proto** (`specification/a2a.proto`, verified):
  - RPCs: SendMessage, SendStreamingMessage, GetTask, ListTasks, CancelTask, SubscribeToTask, push-notification config CRUD, and GetExtendedAgentCard.
  - TaskState: SUBMITTED, WORKING, COMPLETED, FAILED, CANCELED, **INPUT_REQUIRED**, REJECTED, **AUTH_REQUIRED**.
- **Official Rust SDK** `a2aproject/a2a-rs`:
  - Created 2026-04-03, Apache-2.0, 91 stars, active. Crate **`a2a-lf` 0.4.1**.
  - Workspace crates: `a2a`, `a2a-client`, `a2a-server` (axum), `a2a-pb`, `a2a-grpc` (tonic), `a2a-slimrpc`, `a2acli`.
  - Bindings: JSON-RPC, REST, gRPC and SSE.
  - Community alternative: `EmilLindfors/a2a-rs`, crate `a2a-rs` 0.10.0, MIT.
- **Fit.**
  - A2A is remote, opaque agent-to-agent interop over HTTP or gRPC.
  - ACP is host <-> local agent over stdio.
  - HAN's own transport is iroh/QUIC with a signed log.
  - So A2A does not belong in han-host or han-net. It is plausible as a **face/integration** (like `han-github`): an A2A server that exposes a space's agents and tasks through the node API.
  - Its task states map cleanly onto StatusItem: `waiting-approval` corresponds to INPUT_REQUIRED or AUTH_REQUIRED.
- **Verdict:**
  - SKIP for v1.
  - LEARN-FROM the TaskState set and AgentCard for StatusItem and discovery.
  - Revisit as `integrations/han-a2a` after v1; it fits "full A2A" in the product direction.

Sources: https://github.com/a2aproject/A2A · https://github.com/a2aproject/a2a-rs · https://crates.io/crates/a2a-lf · https://github.com/EmilLindfors/a2a-rs

---

## 7. Dissect: concept vs implementation

| Concept | How they implemented it | Transferable? | Why |
|---|---|---|---|
| Gate at the first universal interception point | Claude: PreToolUse runs before rules and modes; its deny wins even in bypass | Yes | A permission *prompt* only sees what the agent's policy leaves unresolved. A pre-exec hook sees everything |
| Fail-closed policy hooks | `onFailure: "block"` (v2.1.295+); fail-open is the default | Yes | The default would silently void the Gate |
| Durable park-and-resume of one tool call | `defer` -> `tool_deferred`, resume by session id | Yes | Gives Propose a crash-safe home; approvals can wait hours |
| Gate fidelity varies per agent | ACP `request_permission` is a MAY | Yes | Make fidelity a declared driver capability that Policy reasons about |
| OS denies direct egress; a proxy owns the allow-list | srt proxies, codex-network-proxy, MXC proxy-only model | Yes (three sources converge) | One controllable point for domain policy and audit, on every OS |
| Credentials never inside the sandbox | srt credential masking; env scrubbing in Codex/MXC | Yes | HAN's executor split is the stronger form; the proxy may still inject read-only tokens |
| Policy rules carry tested examples | Codex execpolicy `match`/`not_match` | Yes | Fits the conformance-vector culture |
| Feature-detect, don't version-compare | `system/init.capabilities`; ACP capability negotiation | Yes | Agents ship weekly |
| Pin the "tested against" agent version | claude-codes versioning | Yes | Driver drift detection |
| Client injects tools into the agent session | `--mcp-config`, ACP `mcpServers`, MCP-over-ACP | Yes | How han-mcp reaches every agent |
| Proxy chains between client and agent | ACP Conductor/Proxy | Partly | HAN's host *is* the client; chains are not needed in v1 |
| Desired-state, naturally idempotent actions | git lease semantics | Yes | Simpler and safer than a generic job framework |

## 8. Abstract: ranked patterns for HAN

1. **Pre-execution gate with declared fidelity.**
   - *Pattern:* every driver reports, as a capability, whether its gate sees every tool call or only agent-escalated ones. Policy and `Mode::Strict` require `every_call`.
   - *Mechanism:*
     - `drivers/claude_code` installs a PreToolUse hook (`http` to the han-host loopback, or `mcp_tool`) with `onFailure: "block"`.
     - `drivers/acp` maps `request_permission` and declares `prompt_only`.
   - *Where:* han-host (the Gate trait plus a `DriverCaps` struct) and `drivers/*`.
   - *Build vs buy:* build (small).
2. **Propose = park, approve, resume.**
   - *Pattern:* a gated call needing approval is parked durably, the effect runs elsewhere, and the session resumes with the receipt.
   - *Mechanism:*
     - The hook returns `defer`, which ends the run with `tool_deferred`.
     - Emit an EffectPlan and set StatusItem `waiting-approval`.
     - After han-exec finishes, run `claude -p --resume <sid>`. The hook answers `deny`, with the reason "executed by han-exec: <receipt>", so the agent never pushes itself.
     - For parallel calls, or for ACP agents, fall back to deny-with-receipt, or give the agent a han-mcp `propose_effect` tool.
   - *Where:* the han-host session lifecycle; the M4 spike.
   - *Build vs buy:* build. The primitive is free from Claude Code.
3. **Whole-process confinement plus a HAN-owned egress proxy.**
   - *Pattern:* the agent process tree has no direct network. Its only route out is a host-side HTTP/SOCKS proxy holding the space's allow-list. The proxy logs every destination as a status and audit signal, and allows loopback only to han-mcp and the hook endpoint.
   - *Mechanism:*
     - Use `mxc-sdk` with proxy-only egress (ProcessContainer on Windows, bwrap on Linux, seatbelt on macOS).
     - The proxy is Rust (hyper plus SOCKS5), or vendored from `codex-network-proxy`.
     - Fallbacks: `srt` as a subprocess on mac/Linux, or WSL2/Podman on Windows.
   - *Where:* `han-host/sandbox`.
   - *Build vs buy:* buy the OS confinement (MXC), build the proxy.
4. **Untrusted-workspace hygiene.**
   - *Pattern:* never let the repo under the agent configure the agent.
   - *Mechanism:* always pass `--bare` (or explicit `--setting-sources`), `--strict-mcp-config` and an inline `--settings`. Set `allowUnsandboxedCommands: false` if Claude's own sandbox is also on.
   - *Where:* `drivers/claude_code`.
   - *Build vs buy:* config only.
5. **Generic ACP driver family.**
   - *Mechanism:* the pinned `agent-client-protocol` crate, with the event mapping from section 1. Install agents from registry metadata (npx or binary plus `acp` args).
   - *Where:* `han-host/drivers/acp`.
   - *Build vs buy:* adopt the crate.
6. **Tested-example policy rules.**
   - *Pattern:* each Gate or Policy rule carries must-match and must-not-match examples, checked at load and emitted as conformance vectors.
   - *Where:* han-core Policy and the Gate rule set.
   - *Build vs buy:* borrow the pattern.
7. **Desired-state actions with a SQLite idempotency ledger.**
   - *Where:* han-exec (`Action::run(plan, key)` already fits).
   - *Build vs buy:* build, using git CLI plus gix plus octocrab.
8. **Capability-detected, version-pinned drivers.**
   - Read `system/init.capabilities` and ACP capabilities, and record a "tested against" CLI version per driver.
   - Build.
9. **A2A as a later face.**
   - Write `integrations/han-a2a` over the node API with `a2a-lf`.
   - Defer.

## 9. Open questions (not determined)

- Does PreToolUse fire for tool calls *inside* Claude subagents? If not, the Gate must disallow the `Agent` tool or handle SubagentStart.
- Do hooks passed through `--settings` load under `--bare`?
- What exact response does an http hook expect? Not in the doc portion I read.
- Can an ACP `reject_once` carry a reason string, through `_meta` or otherwise?
- Which ACP agents offer an "always ask" mode that would raise them towards every-call fidelity?
- MXC: minimum Windows 11 builds for ProcessContainer, real-world maturity of 1.0, and telemetry defaults in the Rust crate.
- The per-crate license of srt-win, and whether it is usable standalone.
- Landlock network (port) restrictions: not verified in this pass.
- git2's credential and SSH limitations: background knowledge only.
- The Codex Windows firewall mechanics: third-party description only.
- Whether Claude Code's license terms restrict automated orchestration by a third-party host. I did not review the license text. The Agent SDK's use is governed by Anthropic's Commercial ToS.

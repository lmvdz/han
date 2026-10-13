# M4 spike: ACP + pi, MXC confinement, egress proxy, reject reasons, propose, summary fork

Run 2026-10-13 on the user's Windows 11 Pro machine (10.0.26200.9457, non-admin user, Node 25.8.2, Rust 1.97.1). The spike is throwaway code: everything lives in the session scratchpad (`…/scratchpad/m4-spike/`), and nothing in the repo changed except this file.

**Versions tested:**
- `agent-client-protocol` =3.3.0 (schema 1.11.0), `mxc-sdk` =1.0.0, `rmcp` =3.5.1;
- pi `@earendil-works/pi-coding-agent` 1.2.0;
- `pi-acp` 0.0.34 (registry);
- `@victor-software-house/pi-acp` 0.17.1, which embeds pi 0.75.5;
- `@agentclientprotocol/claude-agent-acp` 0.89.1, used only for Q4.

**Spike pieces** (Rust, one crate `rs/`):

| Piece | What it is |
|---|---|
| `acpc` | a minimal ACP client: Lines transport over child or MXC pipes, streams updates, answers `request_permission`, sets the model config option |
| `mxcprobe` | runs one command under an MXC ProcessContainer policy |
| `proxy` | the egress proxy: model route with key injection, CONNECT allow-list, `/fork` control port |
| `pipebridge` | a host named pipe ↔ TCP bridge to the proxy |
| `propose` | the rmcp stdio MCP server with one `propose` tool |
| `pilaunch` | a `pi.exe` stand-in that avoids `cmd.exe` |

Two pieces are Node:
- `mock/mock-provider.js`: a scripted Anthropic Messages mock over TLS, driven by directives in the prompt. It logs which key class it received, never the value.
- `rt/shim.js`: an in-sandbox loopback-to-named-pipe shim.

**Live model calls.** 4 requests reached a provider, and none produced model output:
- **Anthropic** (pi's stored subscription login): 2 calls, both answered `400 invalid_request_error: "You're out of extra usage. Add more at claude.ai/settings/usage and keep going."`
- **OpenAI Codex** (pi's stored login): the token refresh failed with `401 refresh_token_invalidated`, so no model request was sent.

So Q5, and the live half of Q6, are **blocked on account state**, not on design. Everything else ran against the mock. Per instructions, no credential file was read, copied or proxied. pi's own process used its login.

---

## Q1. Can a minimal Rust ACP client drive pi? Which adapter works from a non-Zed client?

**Verdict: works.** Use `pi-acp` 0.0.34 (registry). `@victor-software-house/pi-acp` 0.17.1 works only with workarounds that conflict with HAN's sandbox model.

**Evidence: `pi-acp`.** About 120 lines of `agent-client-protocol` 3.3 client code (`Client.builder().on_receive_notification(...).on_receive_request(...).connect_with(Lines::new(..))`). It went through initialize, `session/new` and two `session/prompt` turns, with streamed chunks and tool-call events:

```
initialize -> {"protocolVersion":1,"agentCapabilities":{"loadSession":true,…,"mcpCapabilities":{"http":false,"sse":false}…},"agentInfo":{"name":"pi-acp","version":"0.0.34"}}
session/new -> {"sessionId":"01a135aa-…","configOptions":[{"id":"model",…"currentValue":"hanmock/mock-sonnet"…}]}
>>> prompt: hello there
Hello from mock provider. Offered tools: read, bash, edit, write          (agent_message_chunk stream)
update usage_update: {"used":1550,"size":200000}
<<< prompt done: {"stopReason":"end_turn"}
>>> prompt: TOOL:bash {"command":"echo hi-from-tool > made.txt && dir made.txt"}
update tool_call: {"toolCallId":"toolu_598c…","title":"echo hi-from-tool > made.txt && dir made.txt","kind":"execute","content":[{"type":"terminal",…}],"_meta":{"terminal_info":{…"cwd":"…\\wt1"}}}
update tool_call_update: {… "status":"in_progress","_meta":{"terminal_output":{"data":"made.txt\n"}}}
update tool_call_update: {… "status":"completed","_meta":{"terminal_exit":{"exit_code":0}}}
<<< prompt done: {"stopReason":"end_turn"}
```

`made.txt` appeared in the worktree. The `session/set_config_option` model switch also works; the live run switched to `anthropic/claude-sonnet-5-5`.

**Evidence: victor adapter.**
- Version 0.17 is a thin client to a long-lived, detached daemon (`pi-acp --daemon`) over a Unix socket.
- It **requires Bun**: `import { $ } from "bun"`. Under Node it fails with `ERR_MODULE_NOT_FOUND: 'bun'`.
- Under Bun 1.3.14 it failed again with `ENAMETOOLONG` on the socket path. Unix socket paths are limited to about 108 characters.
- With a short `PI_ACP_SOCKET_DIR` it works on the second attempt; the first times out after 3 s while the daemon starts. Its updates are richer: `tool_call.rawInput`, `_meta.piAcp.toolName` and structured diffs.

**Gaps in both adapters:**
- They **ignore ACP `mcpServers`**; both READMEs list this as a MUST-level gap.
- Neither ever sends `session/request_permission`. pi has no permission prompts.

**Found in the live run:** a provider error is invisible over ACP. pi-acp ended the turn with `{"stopReason":"end_turn"}` and no message at all. The 400 "out of extra usage" error appeared only in pi's session JSONL (`stopReason: "error"`, `errorMessage`).

**Implications for M4:**
- Pin `pi-acp` 0.0.34 as the reference adapter.
- Do not use the victor adapter. Its detached shared daemon outlives the session process tree, which breaks SR-23 (whole tree inside the sandbox) and per-session isolation. It also adds a Bun dependency.
- Wire han-mcp through pi's own MCP config, not ACP `mcpServers` (see Q5).
- han-host must detect failed turns some other way: read the pi session file, or patch or upstream a fix to the adapter so it emits an error. Otherwise the board shows "idle/OK" for a dead session.
- The `AgentDriver` mapping in PLAN §5 holds: `tool_call.kind`/`title`, terminal `_meta` for output, and `usage_update` for context occupancy.

## Q2. Can MXC confine a Node-based ACP agent on Windows 11 without admin?

**Verdict: partial.** Filesystem confinement, child inheritance and network deny work without admin. Egress "only to the proxy" does **not** work with MXC's documented proxy mode on this Windows build; a named-pipe bridge works instead. Several Windows tools break inside the container.

**Evidence: tier and admin.**
- `mxc_sdk::v1::probe` returned `tier: "base-container"`, `base_container_api_present: true`, `base_container_supports_deny_paths: true` and `base_container_supports_ingress_host_loopback_allow: false`.
- No UAC prompt, no admin, and no DACL changes (`needs_dacl_augmentation: false`).

**Evidence: probes from inside.** The policy was rw = worktree, ro = `C:\Windows` plus the Node directory, with egress default deny. The probe is a Node script that re-runs itself as a child and also runs `cmd /c type` as a grandchild. Root and child gave identical results:

```
read worktree/hello.txt: READ OK            read outside/secret.txt: DENIED EPERM
read ~/.gitconfig: DENIED EPERM             read ~/.pi/agent/settings.json: DENIED EPERM
write worktree: WRITE OK                    write outside/, C:/Users/Public, H:/agentic/temp, C:/Windows/Temp: DENIED EPERM
write %TEMP%: OK  (TEMP = …\AppData\Local\Packages\sandbox.{guid}\AC\Temp, a per-container dir)
tcp 127.0.0.1:18081 (other loopback): EACCES   tcp [::1]:18082: EACCES
tcp 1.1.1.1:443: EACCES                        tcp 192.168.1.1:80: EACCES
dns lookup example.com: OK 172.66.147.243      <- DNS still resolves (via the host DNS client service)
[child] … identical results …                  grandchild cmd type outside: "Access is denied."
```

pi's own `read` and `write` tools, running inside MXC under ACP, gave `EPERM: operation not permitted, access '…\outside\secret.txt'` and `EPERM … mkdir '…\outside'`.

**Evidence: reaching the proxy.**
1. **MXC proxy mode with `hostLoopback: "allow"`** (`runtime_config.network_proxy`) was refused: `BackendUnavailable: network.ingress.hostLoopback='allow' is not supported by this version of Windows`. MXC's docs say this needs PSEC 1.1 *with ingress support*; even build 26691 is incomplete.
2. **MXC proxy mode with `allowed_proxy_peer`** needs the proxy to run as a packaged app or a named AppContainer. I ran the listener in a second MXC container and named it as the peer: the connection timed out.
3. **A direct egress allow rule** (`127.0.0.1/32`, TCP, proxy port) to a host-side listener timed out. AppContainer loopback isolation still applies, and lifting it needs admin.
4. **What works:**
   - An egress allow rule for one loopback port; a process connecting to its *own* container on that port: `self-loopback: self-ok`. Unlisted ports stay `EACCES`.
   - A shim inside the sandbox on that port forwards to `\\.\pipe\hanproxy-<sess>`, which han-host creates with a DACL granting `ALL APPLICATION PACKAGES` (S-1-15-2-1) read and write.
   - The bridge relays to the host proxy. Result: `tcp 127.0.0.1:18090 (proxy): CONNECTED` from root and child, and full HTTP round trips (Q3).
   - Named pipes have no half-close, so the bridge must close the pipe when upstream TCP ends. Without that, close-delimited SSE bodies hang; I hit this.
   - Scoping the pipe DACL to the one container SID did not work. `DeriveAppContainerSidFromAppContainerName(container_name)` is not the SID MXC uses; the profile is `sandbox.{random-guid}`. So the pipe is reachable by any AppContainer on the machine, and the proxy's per-session token (SR-28) is what separates sessions.

**Evidence: the agent stack inside MXC.** All of these ran inside, using a per-session agent directory (`PI_CODING_AGENT_DIR`) holding a dummy key:
- the shim, `pi-acp`, `pi --mode rpc` and the `propose` MCP server;
- a full ACP session, streaming through the shim and proxy to the mock;
- `mcp__han__propose` calls.

**What broke inside the container.** Most of these trace to one cause: an AppContainer cannot list or stat the ancestors of the worktree, such as `H:\`.
- Node's main-module `realpath` fails with `EPERM lstat 'H:\'`. Fix: `--preserve-symlinks --preserve-symlinks-main`.
- `cmd.exe` running an unquoted `.cmd` path (as cross-spawn does) gives `Access is denied`. Fix: launch pi through a small `pi.exe` instead of `pi.cmd`.
- **Git Bash/MSYS** (pi's default `bash` tool on Windows) dies with `0xC0000142` (STATUS_DLL_INIT_FAILED).
- **PowerShell** fails with `Cannot find drive … 'Microsoft.PowerShell.Core\FileSystem'` when the cwd is on `H:`.
- `where.exe` (used by pi to find `powershell.exe`) finds nothing.
- `cmd.exe` and `node` work.

**Environment.** MXC's default environment is filtered: no `*TOKEN*`, `*KEY*`, `*SECRET*` or `*PASS*` variables came through from the host. The host `PATH` *is* inherited, so an explicit environment is still needed.

**Implications for M4:**
- MXC is viable as the Windows sandbox without admin, but the egress design needs changes:
  - Replace "egress only to `127.0.0.1:<proxy port>`" with **an in-sandbox shim on a container-local loopback port, then a session named pipe, then the han-host proxy**.
  - Keep the per-session proxy token (SR-28) mandatory, because the pipe DACL can't be scoped to one container yet.
  - Track MXC/Windows for native `hostLoopback` support (PSEC 1.1 with ingress) and switch when the probe reports it.
- **SR-27 conflict:** DNS is not blocked. Name lookups reach the host resolver, which is an exfiltration channel. Either accept and document it (resolution only, no connect), or add a backend that can block it (WSL2/Podman).
- **Worktree placement:** put worktrees and the managed Node runtime under a root whose ancestors AppContainers can traverse, or test a tree under the container's own profile.
- **Shell:** test PowerShell and Git Bash again from a C:-rooted worktree. If they still fail, the Windows sandbox profile needs a shell that works in an AppContainer, such as pwsh, cmd or busybox-w32.
- Until then: "pi inside MXC" runs file tools and Node, but **pi's bash tool does not work** on this setup. That is a real productivity gap for M4.
- **Process launch:** han-host launches pi through `pilaunch` (`PI_ACP_PI_COMMAND`), never through `.cmd` shims.
- **Environment:** set an explicit environment allow-list, with `PATH` limited to the readable directories.

## Q3. Does base-URL redirect plus key injection work, and the agent never sees the real key?

**Verdict: works** against a mock provider over verified TLS. There was no real API key; a canary "real key" (`sk-ant-REALCANARY-<random>`) stood in for it.

**Setup:**
- The sandboxed pi's `models.json` sets `baseUrl: http://127.0.0.1:18090` (the shim), `apiKey: "han-dummy-key"` and `headers: {"x-han-session": "<token>"}`.
- The proxy removes `x-api-key`, `authorization`, `proxy-authorization` and `x-han-session`. It injects the canary key and forwards with reqwest/rustls to `https://127.0.0.1:18443` (the mock), using a spike CA added as a root.
- Allowed paths are `/v1/messages*`; everything else gets 403. A wrong session token also gets 403.

**Evidence:**

```
[proxy] POST /v1/messages?beta=true agent-sent-key-header=x-api-key session-token=ok route=model
[proxy]   upstream 200 usage: input_tokens=1538 cache_read_input_tokens=0 …
[mock] {"url":"/v1/messages?beta=true","keyClass":"REAL", …}        (20 of 20 requests: keyClass REAL)
[proxy] POST /v1/messages agent-sent-key-header=none session-token=BAD route=model   -> 403
[proxy] GET /v1/organizations … route=refused                                        -> 403
canary scan (prefix sk-ant-REALCANARY) over worktree, session agent dir, session home, runtime dir, adapter: no hits
canary in the sandboxed process environment: false;  dummy key present only in sess1/agent/models.json
```

Two gotchas:
- MXC and the shim set `HTTP(S)_PROXY`, so pi's undici sent `CONNECT 127.0.0.1:18090` for the base URL itself, which was denied. Fix: `NO_PROXY=127.0.0.1,localhost` in the sandbox environment.
- The proxy must support both origin-form and absolute-form requests.

**Subscription logins.** Findings from pi's source and its network behaviour; no credential file was read.

**How pi's Anthropic subscription auth reaches the provider:**
- The stored OAuth access token (`sk-ant-oat…`) is sent as `Authorization: Bearer` to `api.anthropic.com`.
- Requests carry `anthropic-beta: claude-code-20250219,oauth-2025-04-20`, `user-agent: claude-cli/<ver>` and `x-app: cli`.
- The first system block is forced to "You are Claude Code, Anthropic's official CLI for Claude." (`pi-ai/dist/api/anthropic-messages.js`, `createClient` and `buildParams`).
- **Refresh happens inside pi's process:** `POST https://platform.claude.com/v1/oauth/token` (`auth/oauth/anthropic.js`, `auth/resolve.js`), written back to pi's `auth.json`.
- Observed through the proxy: `CONNECT platform.claude.com:443` at session start (a refresh), then `CONNECT api.anthropic.com:443`. pi also tried `pi.dev` (catalog and version check, denied) and `auth.openai.com`.

**OpenAI Codex works the same way:**
- The OAuth bearer goes to `chatgpt.com/backend-api`.
- Refresh goes to `auth.openai.com` in-process. Observed: `CONNECT auth.openai.com:443`, after which pi recorded `OpenAI Codex token refresh failed (401) … refresh_token_invalidated`.

**Can the login live outside the sandbox?** Yes, and the spike shows the mechanism:
- pi chooses the subscription request shape purely from the key string (`isOAuthToken = apiKey.includes("sk-ant-oat")`).
- With `apiKey: "sk-ant-oat01-HAN-DUMMY-not-a-token"` in the sandbox `models.json`, pi sent `authorization` (not `x-api-key`) with `user-agent: claude-cli/2.1.280` and the "You are Claude Code…" system block to the proxy's base URL:

```
[proxy] POST /v1/messages?beta=true agent-sent-key-header=authorization …
system[0]: [{"type":"text","text":"You are Claude Code, Anthropic's official CLI for Claude.", …
```

So a proxy holding the login only has to swap the Bearer token and run the OAuth refresh itself, against `platform.claude.com` for Anthropic and `auth.openai.com` for Codex. Two designs follow:
- **A. Login outside (recommended).** han-host holds the subscription login and refreshes it. The agent gets a dummy key in the provider's token format. Key injection, token counting, budgets (SR-41) and the layer-2 fork all keep working.
- **B. Login inside.** The agent holds `auth.json` and refreshes it itself, with CONNECT allowed to the provider and OAuth hosts. Two costs:
  - The real credential is inside the sandbox, against PLAN §0.4 and SR-33.
  - The proxy only sees TLS tunnels, so it can't count tokens, enforce budgets, inject or do the fork.

  Use B only as an opt-in fallback.

**Which credit pool:** the Anthropic error text ("You're out of **extra usage**…") shows that subscription calls from pi draw on the account's *extra usage* pool. The plan's included usage is not used for them. This is a cost and UX point: subscription users need extra usage enabled and funded.

**Implications for M4:** as PLAN §6.4 says, plus these changes:
- Inject per provider route, with an auth mode per provider: `x-api-key`, or Bearer for OAuth.
- Add a dummy-token format per provider.
- Add an OAuth refresher in han-host for subscription logins.
- Put `NO_PROXY` in the sandbox environment.
- Make the session token mandatory.
- A key file the agent can read (`models.json`) must hold only the dummy. Keep the per-session agent directory separate from the user's `~/.pi/agent`.

## Q4. Does an ACP reject carry a reason back to the agent?

**Verdict: no.** It is blocked by the protocol and by the adapters.

**The protocol.** The ACP v1 schema (`RequestPermissionResponse`) has only `outcome` and `_meta`, and `SelectedPermissionOutcome` has only `optionId`. v2 adds an untagged `Other` outcome, but no reason field either.

**The test.** pi never requests permission, so the reject path was tested with `claude-agent-acp` 0.89.1 pointed at the proxy and mock (`ANTHROPIC_BASE_URL`, dummy key). The client rejected with the reason in `_meta`:

```
request_permission: {"toolCall":{"title":"curl -X POST https://example.com/hook","kind":"execute",…},"options":[{"optionId":"allow-once",…},{"optionId":"allow-with-updates",…},{"optionId":"reject","kind":"reject_once"}]}
-> answering: {"outcome":{"outcome":"selected","optionId":"reject"},"_meta":{"han":{"reason":"HAN gate: direct network effects are not allowed; call han.propose …"},"reason":"…"}}
tool_call_update: {"status":"failed","rawOutput":"User refused permission to run tool", …}
what the model received next (from the mock's request log):
  {"type":"tool_result","content":"User refused permission to run tool","is_error":true,"tool_use_id":"toolu_c81d…"}
```

The adapter hard-codes that message (`permissions/effects.js`: `deny(context, message = "User refused permission to run tool")`), and the `_meta` reason is dropped. A side note: `claude-agent-acp` also loaded the user's global skills despite `CLAUDE_CONFIG_DIR`, which is relevant to SR-30.

**Implications for M4.** PLAN §5 is right that "ACP permission requests are UX", but "answer by policy … otherwise 'use propose'" cannot be delivered through the reject. To tell the agent why:
- (a) send a follow-up user message in the next `session/prompt`, or a steering message where the adapter supports it;
- (b) better, rely on the sandbox to fail the raw action, and teach `propose` in the briefing and the tool description. The model then sees the OS error, such as `EACCES` or `EPERM`, plus a known alternative.

Consider proposing a `reason` field upstream in ACP v2 (`request_permission` already gained `title`).

## Q5. Does a propose-only agent stay productive?

**Verdict: the plumbing works; the behaviour is blocked by the account state of the subscription login.**

**Plumbing.** These were checked against the mock, inside MXC:
- The `propose` rmcp 3.5.1 stdio server is wired through the session agent directory's `mcp.json` with `"exposure": "direct"`. With pi's default `codemode` exposure the tool is hidden and the call fails with `Tool propose not found`.
- The model sees `read, bash, edit, write, mcp__han__propose`.
- A call produced an ACP `tool_call` (`title: "mcp__han__propose"`, full `rawInput`). The server returned `Proposed as effect E-1 (status: waiting-approval)…`, and the proposal JSONL got `{"id":"E-1","args":{"summary":"POST build status to webhook","kind":"http.request","effect":{…}}}`.

**The live attempt.** It ran pi through ACP outside MXC, with pi's own login, the egress proxy as `HTTPS_PROXY` (allow-list: provider and OAuth hosts) and the propose server via project `.pi/mcp.json` plus `--approve`. The task needed a webhook POST. It failed before the model ran:
- **Anthropic:** 400 "out of extra usage".
- **Codex:** `refresh_token_invalidated`.

No behavioural evidence was collected. Rerun the same command (`tools/q5live*.out`) once a subscription has extra usage or an API key is available. It costs about 3 to 6 small calls.

**Implications for M4:**
- **Tool exposure:** han-host must write `exposure: "direct"` for han tools.
- **Config location:** put han-mcp config in the per-session agent directory, not the repo. `--approve` trusts *all* project-local pi files, which conflicts with SR-30.
- **SR-29 conflict:** a stdio MCP server launched by pi runs inside the sandbox. Either run a thin stdio stub inside that forwards over the session pipe to han-mcp outside, or use pi's HTTP MCP transport to a URL served through the shim and pipe. The second option is not tested.
- **Q5 itself** stays an open M4 gate.

## Q6. Can the layer-2 cache-hit summary fork be done?

**Verdict: the mechanics work against the mock. Real cache-read numbers are blocked** (no successful live call).

**Evidence.** The proxy keeps the last `/v1/messages` body. Its control endpoint (`GET 127.0.0.1:18079/fork`, unreachable from the sandbox) re-sends the body with one appended user message (`[han.summary/v1] Summarise … at most 150 tokens. Do not call tools.`), `max_tokens: 300` and `stream: false`. It returns only the text blocks (SR-62):

```
{"status":200,"usage":{"input_tokens":37,"cache_read_input_tokens":1963,"cache_creation_input_tokens":0,"output_tokens":20},"summary":"SUMMARY(mock): session has 6 messages; …"}
```

The mock's cache accounting is simulated (a longest common prefix), so this proves only the request shape and the usage plumbing, not provider behaviour.

**Which usage fields each surface exposes:**

| Surface | What it shows |
|---|---|
| **pi-acp** | only `usage_update {used, size}` (context occupancy); no cache fields. Its `/session` command prints `Tokens: in, out, cache read, cache write` from pi's stats (all 0 in the failed live run) |
| **victor adapter** | `PromptResponse` carried `_meta: {}` and no `usage` in this test |
| **claude-agent-acp** | returns `_meta.quota.token_count {inputTokens, cachedInputTokens, cachedWriteTokens, …}` per turn |
| **The proxy** | is the reliable source: it parsed `cache_read_input_tokens` from every streamed response (`[proxy] upstream 200 usage: input_tokens=76 cache_read_input_tokens=1528 …`) |

**Implications for M4:**
- Layer 2 requires design A from Q3 (the proxy is in the model path); it is impossible with a login inside the sandbox.
- The fork must copy the original request's headers (`anthropic-beta`, and the `?beta=true` query), not just the body. My spike fork omitted them. With subscription OAuth it also needs the Claude Code identity headers. Otherwise the cache prefix or the auth mode differs.
- Count tokens at the proxy, not from ACP.
- Real cache-hit measurement stays an open item: one call pair once a working key or subscription exists.

---

## Rerun with claude-oauth extension: not performed

A rerun of Q5 and Q6 was requested through `@pi-plugins/claude-oauth`, an extension described as making pi's Anthropic OAuth requests match the Claude Code client so that calls use the plan's included usage instead of extra usage. The spike did not install or run it. **No live calls were made.**

**Why.** Anthropic's billing already classifies pi traffic as third-party: it drew on extra usage even though pi sends a `claude-cli` user agent and the Claude Code system block. An extension whose purpose is to make that traffic pass as the first-party client circumvents the provider's usage controls. HAN's design shouldn't rest on that. For the same reason, the "what must the proxy reproduce" analysis was not done for this extension: the proxy should not be built to impersonate another client.

**Ways to close Q5 and Q6 that need no workaround:**
- Fund extra usage on the subscription. The harness and command are unchanged.
- Use an Anthropic API key. That gives the full design-A path, including the proxy fork with real `cache_read_input_tokens`.
- Run the official Claude Code client (`claude-agent-acp`) for Q5. It is sanctioned subscription use, but its token would pass through the proxy unless the proxy is limited to CONNECT. In that case Q6 cannot be measured at the proxy.

## Rerun with the official Claude Code client (`claude-agent-acp`) on the subscription

The user chose this path, which is sanctioned subscription use.

**Setup:**
- `@agentclientprotocol/claude-agent-acp` 0.89.1 ran with Claude Code's own existing login, untouched: no config dir override, no base URL, no key.
- The spike's Rust `acpc` client drove it over ACP.
- han `propose` (the rmcp stdio server) was supplied through **ACP `session/new.mcpServers`**. This adapter accepts them; pi's adapters do not.
- Egress was **CONNECT-only**: `HTTPS_PROXY` pointed at the spike proxy, allow-listing `api.anthropic.com`, `platform.claude.com` and `claude.ai`. The proxy added no credentials and saw only TLS tunnels.
- Permissions were answered allow.
- The worktree had `SHA.txt` and a `CLAUDE.md` saying that outside effects go through `propose`.

**Run outside MXC.** Claude Code's login lives in the user's `~/.claude`, so a sandboxed run would need read-write access to that whole directory. The Windows shell problems from Q2 also apply (Claude Code's Bash tool uses Git Bash). So this run shows agent behaviour, not confinement.

**Calls.** About 7 model requests over 2 prompts:
- prompt 1: about 5 Opus 5.5 requests;
- prompt 2: 1 Opus request and 1 Haiku request (session title).

Over CONNECT the requests can't be counted exactly. The estimate comes from the `usage_update` cadence and the per-model usage breakdown. That brings the spike total to 2 failed Anthropic calls in the pi run plus about 7 here.

### Q5 rerun: does a propose-only agent stay productive? Works

The task: *"Our CI webhook at https://hooks.example.com/build expects a POST with JSON {"status":"green","sha":"<the sha in SHA.txt>"}. Please send that notification now."* The agent:
1. ran `cat SHA.txt; ls`;
2. loaded the deferred tool (`ToolSearch select:mcp__han__propose`);
3. made **one** `propose` call, with no curl attempt and no retries;
4. reported honestly that the notification was queued, not sent.

```
tool_call mcp__han__propose rawInput: {"summary":"POST a green build status notification for sha 3f9c2e1 to the CI webhook, as requested by the user.",
  "kind":"http.request","effect":{"method":"POST","url":"https://hooks.example.com/build","headers":{"Content-Type":"application/json"},
  "body":"{\"status\":\"green\",\"sha\":\"3f9c2e1\"}"}}
-> "Proposed as effect E-1 (status: waiting-approval)…"
agent: "The notification hasn't been sent yet. It's queued as effect E-1 and is waiting for approval. This session has no network access, so I put the request through HAN's propose tool…"
follow-up turn: "It hasn't been sent yet: it's queued as effect E-1 and still waiting for approval, as far as I know."
```

The EffectPlan was complete and correct: method, URL, header and body with the SHA filled in.

**Caveats:**
- The agent was briefed through `CLAUDE.md`. An unbriefed run was not tested.
- pi's propose-only behaviour is still untested live; pi's provider calls failed earlier on account state.

**Side finding (SR-30):** Claude Code loaded the user's own global MCP servers and connectors. The proxy denied attempts to reach `mcp.motion.dev`, `mcp-proxy.anthropic.com` (13 attempts) and `pypi.org` (9 attempts). han-host must start Claude Code with an isolated config and strict MCP. CONNECT-only egress contained it here.

### Q6 rerun: what usage and cache information is exposed? Works at the ACP layer; the proxy-side fork is **untested for subscriptions**

The `session/prompt` response `_meta.quota` gives per-turn and per-model token counts, including cache:

```
turn 1: {"totalTokens":89574,"inputTokens":6,"cachedInputTokens":70787,"cachedWriteTokens":18199,"outputTokens":582}   (claude-opus-5-5)
turn 2: opus {"inputTokens":2,"cachedInputTokens":30065,"cachedWriteTokens":235,"outputTokens":39};  haiku {"inputTokens":1434,"cachedInputTokens":0,"outputTokens":99}
```

**Other usage signals:**
- `usage_update` carries context occupancy (`used`/`size`) and a running `cost` (`{"amount":0.1802873,"currency":"USD"}`). This is an API-equivalent estimate, not a charge.
- **Credit pool:** `_meta._claude/rateLimit` reports `{"status":"allowed","rateLimitType":"five_hour","isUsingOverage":false,"overageStatus":"rejected","unifiedWindows":{"five_hour":{"utilization":0.01},"seven_day":{"utilization":0.55}}}`. These calls drew on the plan's included usage, not overage.

The follow-up turn read about 30k cached tokens out of about 30.3k. That is the same prompt-cache behaviour layer 2 relies on, so a fork made *by the client* would hit the cache.

The proxy-side fork of design A can't run on this path: the proxy sees only TLS, and the token stays in Claude Code. **The cache-read measurement for the subscription fork remains untested.**

**Implications for M4:**
- For Claude subscriptions, the sanctioned driver is `claude-agent-acp` with Claude Code's own login. Because the login stays with Claude Code, it falls under design B (Q3), with these consequences:
  - The login is inside the agent's trust boundary.
  - The proxy can only allow-list hosts.
  - Token and cost accounting and the cache figures must come from ACP `_meta.quota` and `usage_update`, not the proxy.
  - Layer-2 summaries must be produced client-side: a turn sent through the agent, or the SDK's own fork or resume. A proxy re-send is not available.
- Design A (the proxy holds the key) stays the path for API keys.
- `claude-agent-acp` accepts ACP `mcpServers`, so han-mcp injection works natively for Claude Code; pi needs the `mcp.json` route.
- Rate-limit `_meta` gives HAN a direct budget signal for subscription sessions: the five-hour and seven-day utilisation and the overage status.

## Summary of plan changes this suggests

1. **§6.4 Sandbox/egress (Windows):**
   - Proxy reachability is shim, then session named pipe, then han-host proxy, until MXC reports native `hostLoopback` support.
   - The per-session proxy token is mandatory.
   - DNS is not blocked by MXC; record that against SR-27.
   - Run the agent with an explicit environment, `NO_PROXY` for loopback and `--preserve-symlinks*`.
   - Launch pi through an exe, not `.cmd`.
   - Move worktrees to a root AppContainers can traverse.
   - Find a working shell; Git Bash and PowerShell failed here.
2. **§6.4 ACP:** `pi-acp` 0.0.34 is the adapter; drop the victor adapter. han tools go through pi's `mcp.json` in the per-session agent directory with `exposure: "direct"`, not ACP `mcpServers`. han-host must detect provider errors that pi-acp hides.
3. **§6.4 Model credentials:** add subscription OAuth.
   - han-host holds and refreshes the login; the agent gets a dummy token in the provider's format, and the proxy swaps the Bearer.
   - Document that Anthropic subscription use from third-party harnesses draws on "extra usage".
4. **§5 AgentDriver:** reject reasons cannot travel over ACP. Deliver reasons by follow-up prompt, and rely on sandbox errors plus `propose`.
5. **Open M4 gates:** Q5 (propose-only productivity) and Q6 (real cache-read on the fork) need about 6 to 8 live calls with a funded key or subscription. The spike harness is ready to rerun.

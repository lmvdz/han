# HAN implementation plan

Status (2026-10-14): M1–M5, T0 and T1 are merged; the dogfood (M6) is in progress on a shared runner. Spec rounds are paused while the product is built (round 32 graded 76.5). §10 has every milestone with its pull requests and the decisions taken during the build. Design consolidated 2026-10-12. Research behind the choices: `research/` (intentd, coordination, agent-plumbing, rust-blocks, gui). Spec decisions for the next round: `spec/decisions/DECISIONS-r30.md`.

## 0. Principles

1. **One protocol.** HAN frames run over QUIC with dial-by-key. iroh is the implementation choice, never a spec requirement. Matrix and chat tools are integrations, not bindings.
2. **One product implementation, in Rust.** `ref/` (Python) is the executable spec that generates the vectors. `witnesses/impl-py/` (Python) and `witnesses/cleanroom-ts/` stay as Policy witnesses.
3. **No harness lock-in.** Every agent is driven through ACP. pi is the reference harness, and any ACP agent should work.
4. **The sandbox is the gate.** An agent has no egress and no credentials. Its only way out is HAN's `propose` tool; han-exec runs effects only with valid approvals. Harness permission prompts are a convenience, never the guarantee.
5. **Authority only through the log.** Anything that grants or withholds rights is a log entry decided by Policy and covered by vectors. Everything else (status, presence, summaries, board data) is namespaced, non-authority data that can evolve without a spec round.
6. **Simple interfaces, deep modules.** Each crate exposes the smallest surface that does its whole job (§5).

## 1. The MVP: team awareness

**A team of 2–4 people, each running several coding agents. Everyone, agents included, sees live what every agent is working on and can drill into any session. Nobody has to ask "what are you working on?"**

1. **A team space** covers the team's repos. Its home (the sequencer) runs on a shared runner or the creator's machine, chosen by the creator. Everyone dials it by key.
2. **Agents.** A named agent actor (for example `@lars-coder`, operated by `@lars`) runs sessions through `han host`. Each session gets its own worktree and branch, and claims its GitHub issue as a signed task.
3. **The live board** in `han tui`. Each row shows:
   - person · agent · harness;
   - issue · state · step;
   - changed files or hunks · overlap badge;
   - tests · tokens or cost · age of the last activity.

   Grouping switches between issue, person and state, and each user's choice is remembered.
4. **Live work timelines.** Every session is a timeline over its worktree (`han-timeline`), synced to readers. Drill-down shows the agent's actual changes as they happen, before any commit.
5. **Agent tools (MCP).** Agents get:
   - `team.sessions`
   - `team.who_is_on(issue|path)`
   - `team.read(session, budget_tokens)`
   - `team.overlaps()`

   Answers are data, never instructions.
6. **Overlap alerts.** Two sessions on the same issue, the same files or overlapping ranges are flagged on the board, and both agents are told as data.
7. **Approvals.** Gated actions go to the agent's operator: an inbox in the TUI, plus OS notifications. Gates and the finish flow are configurable per space and per repo.
8. **GitHub, one way.** Issues come in as tasks through a GitHub App. The default finish: push the `han/*` branch, then propose opening a PR (gated).

**Defaults:**
- Agents read each other's sessions; asking each other questions is the first feature after the MVP.
- Every member reads every session in the team space.
- Humans assign issues; agents may also claim issues labelled `agent-ready`.

**Dogfood (M6):** the user plus 1–3 people, building HAN with HAN, on HAN's own GitHub issues. In progress: a shared runner on a boat.dev VM hosts the home, a solo dogfood space runs two agents, and han-github polls `lmvdz/han` (§10).

**Done when:** the team uses it for two weeks of real work, and the verifier shows no action outside the sandbox without a valid approval.

## 2. Features to design against

| Horizon | Features |
|---|---|
| MVP | §1 |
| Next | agent-to-agent questions; approvals across people; two-way GitHub; the GUI |
| Then | Matrix and Slack notifiers; Linear and Jira; release-tested support for more ACP agents (Claude Code, Codex, Gemini CLI…); more executor actions; Google A2A as a face |
| Later | mobile (uniffi); key-derived actor IDs; peer-to-peer homes and moves; search and summaries across teams; SSO and audit export; bridges for Intent and Copilot sessions |

## 3. Architecture

```
  faces:    han tui │ GUI (Tauri) │ mobile (uniffi) │ agent tools (MCP) │ integrations (GitHub, notifiers…)
                 └──────────────┬─── node API: call(Command) + view(Query) ───┘
  device:            han-node   keys · iroh endpoint · outbox · sync · views · supervisor
                        │                     │                      │
  roles:            han-host (ACP client,     han-exec (Action)      han-seq (sequencer engine)
                    sandbox, proxy, mirror)
                        │
  engines:          han-timeline (work timelines, CRDT)
  protocol:         han-net   han/1 frames over QUIC · CBOR bulk · gossip (presence) · blobs   (iroh)
  pure:             han-core  types · JCS · signatures · Policy · replay   (no I/O; wasm/FFI)
```

**Processes:**
- **han-node** is one runtime per device. It holds the device's keys and its single iroh endpoint, syncs the spaces the device follows, keeps the outbox, and maintains the live views.
- han-node supervises the local roles as child processes: `han host` per agent session, `han exec`, and `han seq` if the device is a home.
- The executor always runs as its own process, because it holds the credentials.
- **Faces never touch the network.** The GUI and mobile apps embed han-node; the TUI, MCP tools, integrations and scripts attach to the daemon over local JSON-RPC (a named pipe on Windows, a Unix socket elsewhere).
- **Features are views.** The board, who-is-on, overlaps, the approvals inbox and later search are incremental projections in one views engine.

**Network topology:**
- **Node to node:** HAN traffic is iroh only. A node refuses node ids that aren't registered devices of members of a space it serves; a one-time invite secret admits a joining device once.
- **Agent egress:** the sandbox reaches only the local egress proxy in han-host. The proxy injects model keys, enforces the allow-list, and counts tokens.
- **Proxy upstream (optional):** an HTTP or SOCKS5 proxy, or the system route, for example a tailnet exit node for a residential IP. HAN ships a hardened Tailscale recipe and no Tailscale code (§6.4).

## 4. Repository

**Layout:** one monorepo (github.com/lmvdz/han), restructured in place with `git mv` before round 30.

```
han/
  spec/
    core/          architecture.md · han.ts                        (the normative protocol)
    profiles/      han.status/1 · han.timeline/1 · team tools       (normative app profiles)
    decisions/     DECISIONS-rNN · HISTORY · grading-rubric
  ref/             Python executable spec and generators
  conformance/     vectors · wire vectors · golden transcripts · drivers (rs, py, ts)
  crates/
    han-core/      pure: types, JCS, ids, Ed25519, validation, Policy, replay
    han-net/       iroh binding: ALPN han/1, streams, CBOR bulk, gossip, blobs
    han-seq/       sequencer engine: pipeline, single writer, SQLite
    han-node/      device runtime, views engine, node API, supervisor
    han-host/      ACP client, sandbox launch, egress proxy, worktree mirroring, status, R-24 duties
    han-timeline/  work-timeline engine (clean room, §8)
    han-exec/      executor; actions: git_push, open_pr, http_request, file_write
    han-vcs/       the Vcs seam and its git backend: workspaces, changesets, land, merge simulation
    han-mcp/       agent tools over the node API
  apps/
    han/           one binary: node · seq · ticket · space · agent · device · tui · board · host · exec · dial · conform · update · key
    han-tui/       ratatui faces
    han-gui/       Tauri 2 + React (after M3)
  integrations/
    han-github/    GitHub App actor (separate process)
  witnesses/       impl-py, cleanroom-ts (Policy vectors and differential fuzz only)
  research/        build-or-adopt studies
  justfile         every task: build, test, conformance, ci, release
```

**Dependency rules (CI-enforced):**
- han-core depends on nothing async or I/O.
- han-net, han-seq, han-timeline and han-node depend inward only.
- Faces, han-mcp and integrations depend only on han-node's API types.
- `crates/han-timeline` may not import or mention the reference model or the studied product.

**Repo policies:**

| Topic | Decision |
|---|---|
| Tooling | a root `justfile`; cargo-nextest, cargo-deny, cargo-dist; uv for Python; pnpm for TS |
| CI | GitHub Actions. Every PR: the spec checks on Linux (vectors, fuzz, model at depth 4); Rust and TS builds and tests on Windows, macOS and Linux; cargo-deny; secret scanning. Nightly: the deep model, the 1000-seed fuzz, and a live pi run under a hard token budget |
| Versioning | the protocol version (`han/1`, bumped only by spec changes, negotiated in ALPN and hello) is separate from one workspace version (0.x) shared by all crates and apps. Spec versions are git tags such as `spec-v1-r30` |
| Normative tiers | the core standard is the protocol (log, Policy, `han/1` framing). Profiles (`han.status/1`, `han.timeline/1`, `team.*` tools) are normative for interoperability between HAN apps but evolve faster. The node API is an implementation API |
| Types | protocol types stay normative in `han.ts`; Rust is hand-written and checked against it (extending the consistency check). Node API types are Rust-first, generated to TS (specta or ts-rs) and Swift/Kotlin (uniffi) |
| License | code Apache-2.0 OR MIT; spec CC-BY-4.0 |
| Contributions | DCO sign-off; spec changes go through written proposals (DECISIONS-style) |
| Push policy | stay local until the restructure is done and the checks pass; then ask the user before each push |
| Naming | keep HAN and `han`; check crates.io, npm, Homebrew and trademarks before the first release, falling back to `han-*` names |
| Grading | spec rounds stay graded by an independent agent against the rubric; implementation milestones are judged by their "done when" |

## 5. Interfaces (the whole surface of each module)

### Node API: two methods

```rust
async fn call(cmd: Command) -> Result<CallResult, NodeError>;   // ends as a receipt, a verdict or an error
fn view(q: Query) -> impl Stream<Item = ViewEvent>;              // a snapshot of the window, then diffs
```

- **`Command`:**
  - `SpaceCreate`, `SpaceJoin{ticket}`, `Invite`;
  - `MemberSet`;
  - `AgentCreate`, `AgentStart{agent, harness, repo, issue?}`, `AgentStop`;
  - `TaskCreate{external?}`, `TaskUpdate`, `TaskCancel`;
  - `SessionInput`, `Ask{session, question}` (next);
  - `EffectApprove`, `EffectCancel`.
- **`Query`:**
  - `Board{space, filter, group}`, `Session{id}`, `Timeline{session, at?}`;
  - `WhoIsOn{issue|path}`, `Overlaps{space}`;
  - `Approvals{actor}`, `SpaceStatus{space}`, `Cost{space|session}`;
  - later `Search`.
- **Every `Query` takes a window** (filter, limit, cursor); faces scroll by moving it.
- **Errors are a closed set** mirroring the protocol's verdicts: denied, invalid, not_found, unavailable, conflict.
- **Serde enums with JSON at the boundary**, so the same surface works in-process, over JSON-RPC and over uniffi. New features add variants; the two methods never change.
- **Property:** a snapshot plus every later diff equals a fresh read (checked in the simulation tests).

### AgentDriver (han-host)

```rust
trait AgentDriver {
    async fn start(&mut self, cfg: SessionConfig) -> Result<()>;
    async fn turn(&mut self, input: TurnInput, mode: Mode) -> Result<()>;   // Mode::{Auto, Strict}
    fn events(&mut self) -> BoxStream<'_, AgentEvent>;                      // Output, ToolCall, FileTouched, Tests, Summary, TurnEnded
}
```

- **There is one implementation: the ACP client.** A harness is an ACP agent command plus its sandbox profile.
- **ACP permission requests are UX.** The host answers them by policy (allowed inside the sandbox, otherwise "use propose"), and they show as hints on the board.
- **The host owns** the R-24 duties (strict turns, per-agent taint, naming the driving task), status derivation, the egress proxy, secret masking, and worktree mirroring.

### Action (han-exec)

```rust
trait Action {
    const KIND: &'static str;
    fn plan_check(&self, plan: &Value) -> Result<(), PlanError>;
    async fn run(&self, plan: &Value, key: IdempotencyKey, ctx: &ActionCtx) -> Outcome;
}
```

- **Actions are desired state.** For example, a push is "move ref R from sha A to B", so a retry is safe.
- **The ledger** is SQLite, keyed by idempotency key. "Unknown" is never treated as success, and invocation ids are never reused.

### Injected from day one

```rust
trait Clock { fn now_ms(&self) -> u64; fn elapsed(&self) -> Duration; }
trait Transport { /* dial(node_id), serve, gossip(topic), blob_put/get */ }
```

These make deterministic simulation of whole teams possible (turmoil, with a simulated Transport, since real iroh can't run on turmoil sockets).

### `han.status/1` (profile; non-authority)

```
{ session, agent, delegate_of?: ActorId, harness,
  issue?: "github:owner/repo#123", repo?: "github:owner/repo", base?: sha,
  state: "working" | "waiting-approval" | "blocked" | "idle" | "done" | "stale" | "failed",
  step: string, tool?: string, plan?: [{text, done}],
  files: [{path, hunks: [[start, len]]}],
  tests?: {pass, fail}, tokens?: {in, out, cost}, summary?: string, seq, at }
```

This one schema feeds the board, overlap alerts, agent tools and GitHub sync. `stale` is derived from the time since the last item, never claimed by the agent; `seq` is a per-session counter, so freshness doesn't depend on wall clocks.

## 6. Decisions by subsystem

### 6.1 Protocol and encoding

| Topic | Decision |
|---|---|
| Encoding | signed authority entries stay JCS JSON. Bulk traffic (timeline ops, session streams, sync) is CBOR (`minicbor`) with integer field keys, in batches compressed with zstd, each batch signed over its exact bytes. CBOR was chosen for implementers in other languages; its gap to postcard doesn't matter at HAN's rates |
| Presence | iroh-gossip, presence only: topics derived from a secret only readers hold, signed payloads, under 4 KiB. Status flows to readers through the node, never over gossip |
| Blobs | `han/1` frames, verified by SHA-256 (iroh-blobs is BLAKE3-only) |
| Round 30 | the spec gets the §8 QUIC binding, the peer filter, device keys and node ids, invite tickets, the profiles tier, and the protocol fixes V1–V8 (`spec/decisions/DECISIONS-r30.md`) |

### 6.2 Identity, onboarding and approvals

| Topic | Decision |
|---|---|
| Keys | the OS keychain (`keyring`), with an encrypted-file fallback for headless runners; hardware keys later behind the same interface. Keys never leave han-node |
| People and devices | one actor per person; each device has its own key and node id, added with `key.add` by an existing device. A lost device is revoked from another. Recovery: a stored recovery key, or co-owners re-admit you |
| Agents | one actor per named agent, reused across sessions (`han agent create @lars-coder --operator @lars`), with its key held by the host that runs it. Several sessions can run in parallel |
| Onboarding | invite tickets (`han invite` gives node id, space id and a one-time secret; `han join <ticket>`; an owner's `member.set` admits), plus GitHub org or team membership as an optional admission rule (gist-nonce proof) |
| Approver | by default the agent's operator approves its gated effects; spaces add rules (for example two approvals for releases) |
| Gates (defaults, all configurable per space and per repo) | push to protected branches; outside HTTP writes; accepting timeline work into main; opening or merging PRs |
| Approval surfaces | MVP: the TUI inbox and OS notifications. Later: chat notifiers that link to a HAN face (approvals must be signed), and a phone face |

#### Approvals: human presence

Terminal input is shared and injectable (other processes, terminal plugins, built-in agents, attached clients), so the TUI's two-step `a` then Enter (APR-5) only guards against accidents. The authority is the node: it signs `effect.approve` only after a person has confirmed on the device, out of band, for the plan digest being signed (SR-106). `Command::EffectApprove` keeps its shape; it now means an intent.

| Topic | Decision |
|---|---|
| Flow | on `EffectApprove` the node reads the proposal's plan digest, then asks the presence check (`han-presence`) with the action, target, short digest (12 hex), space and proposer. Only on success does it re-check that the proposal still waits with the same digest, and sign. A cancel, a timeout, a failed check or a device without a check returns `denied` with a message starting `approval not signed: `, which faces show as is. The prompt runs on the node, never in a face; prompts are one at a time |
| Backends | Windows: Windows Hello (`UserConsentVerifier`; the message is the action and the short digest). macOS: LocalAuthentication through objc2 (`DeviceOwnerAuthentication`: Touch ID, then the password); compiled, not yet run on a Mac. Linux: polkit through `pkcheck` under `dev.han.approve` (policy file in `crates/han-presence/polkit/`); the fixed polkit message cannot show the digest until the D-Bus `CheckAuthorization` call replaces `pkcheck`, and a FIDO2 security-key backend comes later. None: `Presence::deny_all()`. Doubles that allow exist only under `cfg(test)` or the `test-doubles` feature |
| Setting | device-local, in `node.db` `config`: `presence.require` is `all` (the default) or a comma-separated list of action kinds (`effect.approve`), the empty list requiring nothing; `presence.backend` is `auto` (default), `windows-hello`, `touch-id`, `polkit` or `none`. With `none` while presence is required, approving fails: "this device has no presence check; …". No node API command reads or writes it, so a face or an agent cannot turn it off; a malformed value refuses and never defaults |
| Connections | the local pipe's class comes from the endpoint: a face endpoint takes every command, a host endpoint only agent commands, an executor endpoint agent and executor commands (SR-107). A host or executor endpoint cannot send `EffectApprove` |
| Wire | no change. Whether an approval should carry an attestation that a presence check passed (so a home or other members can see it) is the UI spec's open question, pending round 33 |

### 6.3 Storage and data

| Topic | Decision |
|---|---|
| Layout | one SQLite file per space (log, receipts, timeline) plus `node.db` per device (outbox, view caches, config). Leaving, moving or deleting a space is a file operation, with optional per-space encryption |
| Migrations | from the first commit, with a format version in every database |
| Retention | per space. The log is kept forever. Session streams and fine-grained timeline history are compacted after a window (default 90 days); merged work keeps its merge |
| Secrets | paths on a per-space exclusion list (.env, *.pem, ~/.ssh…) are never mirrored or streamed. han-host also scans stream items and timeline ops (gitleaks-style rules plus the proxy's known keys) and masks matches before anything leaves the device |

### 6.4 Agents, sandbox and egress

| Topic | Decision |
|---|---|
| ACP | `agent-client-protocol` 3.3, pinned. pi first; both pi adapters (`pi-acp` 0.0.34 from the registry, and `@victor-software-house/pi-acp` 0.17) are evaluated in the M4 spike |
| Node runtime | managed: `han` downloads a pinned Node build and pinned adapter versions into its data folder |
| Sandbox | Microsoft MXC (`mxc-sdk` 1.0: Windows 11 without admin, Linux bubblewrap, macOS seatbelt); fallback WSL2 or Podman. The boundary is the whole agent process. The session's worktree is its writable area |
| Egress default | an allow-list through the proxy: package registries, docs sites, the model provider's API. For tunnelled hosts "read-only" means unauthenticated: the proxy adds no credentials there, so anything that needs one goes through `propose`. Everything else is blocked; spaces widen or narrow it. The crates.io hosts are not default: the Rust toolchain adds them (Toolchains, below) |
| Toolchains | opt-in per node (`agents.json` `toolchains`, `toolchainPaths`) or per agent; Rust first (rustup, cargo), built so Node and Python are one more entry. **Read-only** roots (`RUSTUP_HOME` and rustup's `cargo`/`rustc` proxies: Windows `%USERPROFILE%\.rustup` and `.cargoin`, Linux for example `/opt/rust` or the node user's `~/.rustup`); **per-agent writable** `CARGO_HOME` (`<agent state>/cargo-home`, the registry and git caches, shared by that agent's sessions only); `CARGO_TARGET_DIR` per session next to the worktree, not in it (no mirror noise). `PATH`, `RUSTUP_HOME`, `CARGO_HOME` and `RUSTUP_AUTO_INSTALL=0` are set for the agent. A `rust-toolchain.toml` pin that is not installed fails the session start with the toolchain's name and the installed ones; the sandbox never downloads a toolchain. **Egress:** enabling Rust adds `crates.io`, `static.crates.io` and `index.crates.io` (443) to that agent's proxy allow-list, nothing else (SR-105); they are not in the default list. **MXC:** an AppContainer cannot call `GetFinalPathNameByHandle`, so `fs::canonicalize` fails and cargo cannot unpack a crate it downloads; the host therefore runs `cargo fetch --locked` for the worktree's `Cargo.lock` before launch, outside the sandbox but through the session's proxy, from a scratch directory (the repository's `.cargo/config.toml` is not read, no build script runs), and the sandbox builds from the filled `CARGO_HOME`. A failed prefetch shows as a sandbox gap. Linking needs MSVC: the host finds the newest `VC\Tools\MSVC\<v>` and `Windows Kits\Lib\<v>`, grants them read-only, sets `PATH`, `LIB` and `VSCMD_ARG_TGT_ARCH`, and points the linker at a small wrapper that creates the per-launch `%TEMP%` that `link.exe` needs. Without MSVC the GNU toolchain is the fallback (`toolchainPaths.rust.msvcDir`/`sdkLibDir` override discovery). Bubblewrap needs none of this: `/opt` is already bound read-only and cargo downloads through the proxy itself |
| Proxy TLS | model calls: the harness's base URL points to the local proxy, which injects the key and forwards over TLS. Other egress: CONNECT tunnels allowed by hostname (SNI). No interception, no fake CA |
| Model credentials | two modes, both supported, and in both the secret stays **outside** the sandbox (M4 spike, `research/m4-spike.md`). **API key:** the proxy injects the real key for the provider's host; agents hold a dummy key. **Subscription:** the harness holds a dummy token of the same format, and the proxy swaps in the real subscription bearer token and refreshes it outside the sandbox, so the proxy still sees token counts, budgets and the summary fork. Third-party subscription use is approved by Anthropic (an Anthropic staff member confirmed it, per the user, 2026-10-13); through pi it draws on the account's extra-usage credit. **Claude Code as the harness (first dogfood):** its login lives in its config directory, so the agent gets a **per-agent login dir inside the agent's trust boundary**: `<agent data>/claude-config`, set as `CLAUDE_CONFIG_DIR` and bound read-write into that agent's sandbox only (bubblewrap `--bind`; an MXC read-write grant on Windows), and nothing else of the user's home. `han agent login <agent>` makes the login once, outside the sandbox; `--check` reports only whether one exists. The driver records the gap in `isolation.gaps` |
| Proxy upstream | optional: an HTTP or SOCKS5 proxy, or the system route. Tailscale recipe: runners tagged `tag:han-runner`; access rules allowing only `autogroup:internet` via the exit node; a dedicated exit node on a guest network; ideally a separate tailnet from personal devices; ephemeral tagged auth keys. Check each provider's terms before using residential IPs |
| Worktrees | han-host gives each session its own clone with its own git metadata (not a `git worktree` of a shared checkout), on branch `han/<issue>-<agent>` from the default branch; it records the base sha and cleans up after merge or cancel (SR, research/security-requirements.md) |
| Shared runners | a runner is an actor in the space (an owner as operator). Members start agents on it with a signed task, so Policy decides who may use it |
| Board freshness | a working session with no status item for 10 minutes shows as stale; if its host is unreachable for 30 minutes it shows as failed. Configurable per space |
| Cost | the proxy counts tokens and cost per session from provider responses; spaces can set budgets through Policy's budget ledger |
| Summaries (`team.read`) | layered, cheapest first: (0) structured status, free; (1) a required `summary` argument on HAN tool calls; (2) a turn-end summary made by the proxy re-sending the session's last request plus a summarise instruction **on the same model**, so about 90% of the input is a prompt-cache hit; (3) transcript windows or timeline diffs on request. `team.read(session, budget_tokens)` fills the reader's budget layer by layer. Layer 2 runs only while the cache is warm, only for sessions someone follows, at most once per 5 minutes, matching the default prompt-cache lifetime. Summaries count against the summarised session; layer-3 reads against the reader |
| Agent tests | a fake ACP agent replays recorded sessions in CI, plus scripted edge cases; nightly, a live pi run under a hard token budget |

### 6.5 Work timelines (`han-timeline`, profile `han.timeline/1`)

| Topic | Decision |
|---|---|
| MVP scope | named timelines and fork; three-way content merges recorded with the incoming version; a text CRDT with anchors; time travel and checkpoints; seals on session transcripts |
| Merge | a three-way content merge onto main, naming the incoming version; conflicts come out as hunks for review. Only the approval-gated accept effect writes it. No history-join merges for code |
| Seals | only on session transcripts: the text an agent sent or received is frozen to exactly what it saw |
| Mirroring | a file-system watcher (`notify`) on the worktree records every change as timeline ops, whatever made it. ACP tool-call events attribute changes to steps. Designed by HAN from git's semantics |
| Transport | HAN-signed envelopes (CBOR batches) over iroh; actor ids are HAN ActorIds |
| Fallback | `git diff -U0` hunks feed the board and overlaps if the engine lags |
| Provenance | built by a two-team clean room (§8) |

### 6.6 Coordination, board and faces

| Topic | Decision |
|---|---|
| Issue claims | a signed `task` (external ref = the issue) assigned to the agent; self-claimed counts as accepted. A second claim is allowed but shows as an overlap |
| Overlap detection | three tiers: claims (issues, and advisory path leases with a TTL); touched hunks (range intersection); real conflicts (a `gix merge-tree` merge simulation between session branches) |
| Overlap response | alert both agents (as data) and highlight on the board. The newer claim is asked to coordinate or avoid the area; humans decide if it persists. Nothing is blocked automatically |
| Agent questions (next) | a signed `message` to a session, shown to the agent as data between turns and answered as data; its human sees it. Limits are configurable per space. Defaults: the asker pays; one open question per pair; no question back inside an answer; 60 per hour per session |
| TUI | board-first: Enter opens a session pane (live stream, timeline diff, tool calls); a side pane holds the inbox and alerts; Vim/VS Code keys and a `:` palette. Steering from the session pane is a signed `session.input`: instruction to your own agents, data to others' (R-24) |
| GUI (after M3) | Tauri 2 with han-node embedded; React 19 with the React Compiler, TypeScript (strict), Vite, TanStack Router, Tailwind 4, shadcn-style components. `call` is a Tauri command; `view` streams over `tauri::ipc::Channel`. Dependencies: `@pierre/diffs`, `@pierre/trees`. An early spike checks diff rendering in WebView2 and WebKitGTK. Frontend rules: §6.6.1 |
| Borrowed UI | clean room only (§8): spec writers study t3code, agent-orchestrator and Intent and write behaviour and UI specs; fresh implementers build from them. Published packages stay normal dependencies |

#### 6.6.1 Frontend rules (React, 2026)

| Rule | Decision |
|---|---|
| Effects and state | **Effect v4** (effect.website) owns every side effect, I/O, stream, service and typed error in the frontend. The node API's `view()` stream becomes an Effect `Stream`, exposed to React as **atoms** (Effect's `atom-react`: `useAtomValue`, `useAtomSuspense`, `RegistryProvider`; confirm the exact package name at build time). `call()` commands run as Effect programs. This replaces zustand and supersedes `research/gui.md`'s "skip Effect" |
| Lint and format | **oxlint** and **oxfmt** (Oxc). Confirm at GUI start that oxlint's React-hooks and React Compiler rules cover what we need; add a minimal ESLint only for a rule oxlint lacks |
| No `useEffect` | `useEffect` and `useLayoutEffect` are banned in app code: oxlint `no-restricted-imports` forbids importing them from `react`, and forbids the default and namespace imports of `react`, so `React.useEffect(...)` can't be written. CI fails on any use. No escape hatch in app code |
| What replaces it | values derived during render; event handlers; atoms for anything external (subscriptions, live views, timers); Suspense and error boundaries for loading and failure; React 19 Actions (`useActionState`, `useOptimistic`) for commands; **ref callbacks with cleanup** (React 19) to mount imperative widgets such as the diff viewer or a terminal |
| Memoisation | the React Compiler handles it; no hand-written `useMemo`/`useCallback` unless a measured hot path needs it |
| Rendering performance | windowed lists for the board and streams (views are already windowed by the node API); one subscription per view, owned by its atom, never per component; no state copied from props |
| Types | node API types generated from Rust (specta or ts-rs), plus a generated Effect `Schema` that decodes every frame from Tauri, so a bad frame is a typed error, not a crash |
| Clean room | these rules are part of the UI specs Team B implements from (§8) |

### 6.7 GitHub and the executor

| Topic | Decision |
|---|---|
| GitHub | a per-team GitHub App with fine-grained permissions and short-lived installation tokens, minted only by han-exec (the single holder of the **write** App's private key). han-github reads with a different, read-only App (Issues and Metadata read) whose key it holds itself, so a compromised poller cannot mint write tokens. Future work: a read-token broker in the executor, so han-github holds no GitHub key at all. MVP: han-github polls issues every 60 s with conditional requests (ETags). Later: a webhook relay run by the team, or a small hosted forwarder |
| Finish flow | configurable per space and per repo: mark done; push the branch and propose a PR (default); or merge the timeline into main. Each step is gated or free as configured |
| git | shell out to `git` for push (gix can't push yet); gix for reads; octocrab for the GitHub API |

### 6.8 Operations

| Topic | Decision |
|---|---|
| Platforms (MVP) | Windows 11, macOS, Linux, and WSL2 as a Linux target |
| Observability | `tracing` plus OpenTelemetry on by default, exporting only to a collector the team configures, never to the project. Session content stays out of spans unless a space allows it |
| Updates | signed self-update on request (or nightly if enabled). Versions are exchanged in hello; a node refuses a space whose protocol version is newer than it supports. Installers via cargo-dist |
| Space home | chosen by the creator (a shared runner or their machine); it can move later (§7.3) |
| Relays | self-host `iroh-relay` for the team; n0's public relays as a fallback |

## 7. Build or adopt

Full findings: `research/`. No existing tool gives a self-hosted, live view of every agent session across a team. So the board, overlaps, timelines and the trust layer are HAN's to build; almost everything beneath them is adopted.

| Need | Decision |
|---|---|
| Transport | adopt iroh 1.3 (`Endpoint`, `Router`, `IncomingFilter`) |
| Log, sequencer, views, JCS, executor ledger, overlaps, timelines | build. Nothing else gives exclusive claims. Use `serde_json_canonicalizer` only as a JCS test oracle |
| Signatures | ed25519-dalek 3.0, `verify_strict`, with pinned edge-case vectors across ref, TS and Rust |
| Storage | rusqlite 0.40 + rusqlite_migration |
| Local IPC | `interprocess` 2.4 (pipe restricted to the current user) + our own JSON-RPC framing |
| Agents | ACP (`agent-client-protocol` 3.3); MCP via `rmcp` 3.5 (stateless spec; handles as arguments) |
| Sandbox | MXC; `landlock` to harden han-exec |
| GitHub | octocrab 0.54 |
| Simulation, TUI, mobile | turmoil 0.7; ratatui 0.30; uniffi 0.32 (later) |
| Later | Google A2A (`a2a-lf`) as a face; C2SP checkpoints and witnesses for head gossip; bridges for Intent and Copilot sessions |

**Strategic note.** Claude Code's single-user features overlap the MVP for one person. HAN's edge is the team: several machines and people, any ACP harness, self-hosting, verifiable approvals, and live work timelines.

## 8. Clean rooms

**Why.** `han-timeline` is informed by a reverse-engineering study of a closed product, and the GUI borrows from other apps. HAN is an open standard in a public repo, so its code must not descend from those sources.

| | Team A (spec writers) | Team B (implementers) |
|---|---|---|
| Reads | the study's evidence, the reference model, and the borrowed apps' source | **only** the specs, their vectors, and public prior art (Loro, Automerge, Eg-walker, Fugue) |
| Writes | behaviour-only specs (`spec/profiles/han.timeline/1`, UI specs) with vectors | `crates/han-timeline`, `apps/han-gui` |
| Must not | put code, transcribed pseudo-code, addresses or snippets in a spec | read the study's materials, the studied product, the evidence or the apps' clones |

**Enforcement:**
- Team B works in fresh agent sessions in a separate checkout that holds only specs and vectors.
- Permission rules deny reads of the source paths.
- Prompts name only the spec, and each session's tool log is kept as provenance.
- A reviewer checks every spec for transcribed material.
- Questions from Team B to Team A go through written spec revisions.
- In CI:
  - `Clean-Room: B` commit trailers;
  - a CODEOWNERS sign-off on the specs;
  - a guard against mentions of the reference model or the studied product in Team B's crate.
- **This design session and every agent it briefed have read the sources and must never write Team B code.**
- **Before anything is published,** the user checks the studied product's terms for reverse-engineering clauses.


## 9. Rules that keep it changeable

1. Authority changes only through the spec, the vectors and Policy; everything else is a namespaced profile or stream item.
2. Inward dependencies, enforced in CI.
3. Clock and transport are injected; a three-node simulation runs in CI from M3.
4. SQLite migrations from the first commit.
5. Conformance gates every PR.
6. Keys never leave han-node; credentials never leave han-exec or the proxy.
7. Pinned versions for fast-moving crates and adapters (ACP, rmcp, pi adapters), each behind one trait.

## 10. Milestones

| | Milestone | Done when | Status |
|---|---|---|---|
| R | Repo restructure (§4) | every check passes from the new layout; CI runs on three operating systems | done |
| S30 | Spec round 30 (`spec/decisions/DECISIONS-r30.md`) | checks pass; graded by an independent agent | done (round 30 graded 75.6); rounds 31 and 32 followed (76.5); rounds paused since |
| M1 | han-core | all policy vectors pass; 0 differential-fuzz divergences against ref over 1000 seeds | **done** (#29): 168/168 Policy vectors, 0 fuzz divergences |
| T0 | Timeline spec (Team A) | `han.timeline/1` and its vectors reviewed: behaviour only | **done** (#24; answers to Team B's questions #26, #28) |
| M2 | han-net + han-seq over iroh | wire vectors pass over iroh; the peer filter and invite tickets work | **done** (#32): 113/113 wire vectors over QUIC |
| SR | Security requirements (before M3) | `research/security-requirements.md`: 103 controls with tests (SR-01…SR-103); the MUSTs listed there are required before M6 | done; the M6 MUSTs are tracked in `research/security-requirements.md` |
| M3 | han-node: outbox, sync, views, node API, supervisor | a three-node turmoil simulation converges; JSON-RPC works on Windows; the snapshot-plus-diffs property holds | **done** (#35) |
| T1 | han-timeline (Team B) | passes the T0 vectors and the randomized convergence and sync tests, with signed envelopes | **done** (#25; performance #30, #34, #39, #41) |
| M4 | han-host (ACP client, MXC, proxy, mirroring, masking), han-mcp, han-exec | **first week, a spike answering:**<br>• which pi adapter works from a non-Zed client?<br>• can MXC confine a Node-based agent on Windows 11 with egress only to the proxy?<br>• does an ACP reject carry a reason?<br>• does a propose-only agent stay productive?<br>• does the cache-hit summary fork work?<br>Then: a real session streams status and a live timeline; an agent answers "who is on auth/?"; a gated push waits for approval and runs once | **done** (#49) |
| M5 | han-tui (board, session pane, inbox) + han-github | the full MVP story across two machines and a shared runner; pi plus a smoke test of 2–3 other ACP agents | **done**: han-tui (#60), han-github (#50, #61) |
| M6 | Dogfood | the user plus 1–3 people, two weeks, building HAN with HAN | **in progress** (below) |
| G | GUI (after M3, parallel to M4–M6) | the board, session pane and inbox in Tauri, built from clean-room UI specs | clean-room UI specs written (#42, #44); the build has not started |

**Parallel tracks:** R first; then S30, M1 and T0 in parallel; T1 alongside M2–M4. Timeline integration is on the MVP's critical path, with `git diff` hunks as the fallback.

**Merged since round 32, besides the milestones:**
- the Rust server enforces the hello-to-node binding (SR-13, #47);
- the hosting rule (#48);
- federation: fetching and pinning remote profiles (#52);
- the operator's confirmation over `han/1` (#59);
- joining a home by ticket or node address (#62);
- node API gaps: view clocks, approval plan facts, the spaces list, the needs-me facet, `guardedFor`, view seq, Resync and End, board totals, the filter grammar, row reasons and overlaps (#63, #65, #67);
- CLI: `han space create|follow|list|member|policy`, `han device`, `han tui --print`, and `HAN_DATA` (#64, #66);
- release tooling: cargo-dist, `han update --check`, the nightly cross-OS CI, Actions pinned by SHA, and Dependabot (#51).

### 10.1 M6: the dogfood, now

- **Where it runs.** A shared runner on a boat.dev VM hosts the home and the runner's node, set up as in `docs/runner.md`.
- **What runs.** A solo dogfood space with two agents. han-github polls `lmvdz/han` and surfaces its issues there.
- **Being fixed now** (the remaining blockers):
  - per-agent login directories;
  - starting the Claude Code driver on the runner;
  - a host per OS user;
  - an executor service;
  - a node logon service;
  - a task CLI.
- **Then:** teammates join by home ticket, and the two weeks of the "done when" start.

### 10.2 Decisions taken during the build

| Topic | Decision |
|---|---|
| Hosting rule | a home creates spaces only for known actors: its own users, agents and server actor, and holders of a role (not banned) in a space it hosts (`han seq --hosting known`, the default). `open` and an allow-list are options; test mode is `open` (#48) |
| Agent pool | han-github doesn't assign. An `agent-ready` issue becomes one task addressed to the team's shared agent pool (`tasks.pool`), and the pool agent that takes it makes its own signed task with the same external ref (§6.6 Issue claims; #61) |
| Home ticket | `han seq` writes a home ticket (`han1home.…`: ServerName, node id, addresses, receipt keys), and `han ticket` prints it. `han node`, `han space` and `han agent` take `--home <ticket>`, so a home needs no HTTPS well-known (#62) |
| Ticket-pinned trust | a node given a ticket pins the home's node id and receipt keys in its own data folder (`node.db`), and later restarts need no flag (#62, #66). Round 32 treats a ticket as a dial hint checked against the well-known; the spec doesn't describe trust pinned from a ticket yet, so it goes to the next round |
| Per-agent login directories | each agent actor gets its own harness login directory. The host binds only that directory into the agent's sandbox, never the operator's home or login, so agents don't share a login with their operator or with each other. Being built for M6 |
| Spec rounds paused | no spec round runs while the product is built; round 32 (76.5) is the current spec. Queued for the next round: semantic gates, risk tiers for landing work (§12), and the round-32 review items AA1–AA5 (the build already fixes AA2 with the hosting rule, #48, and AA3 with the hello binding, #47) |
| Dogfood runner | the M6 home and the first shared runner run on a boat.dev VM, set up as in `docs/runner.md`. Public files name no address, id, ticket or space of it |

## 11. Risks

| Risk | Plan |
|---|---|
| ACP adapters lag or break | the M4 spike; pinned versions; safety doesn't depend on the adapter |
| MXC maturity on Windows | the M4 spike; WSL2 or Podman as the fallback |
| Claude Code or others ship team features | HAN's edge is multi-vendor, self-hosted, verifiable approvals and timelines |
| Fast-moving crates | pinned, each behind one trait |
| Relays | self-host `iroh-relay` |
| The timeline engine delays the dogfood | `git diff` hunks as the fallback |
| Provenance | the clean rooms (§8); the user checks the studied product's terms |
| Agents chatting and spending tokens | layered summaries, budgets, A2A limits, cost on the board |
| Tailnet exposure | the hardened recipe; runners can't reach personal devices |
| Spec erosion ("simple interfaces") | graders watch C2; prefer removing special cases over adding them |

## 12. Open items

- A scratch benchmark of CBOR against postcard on real op shapes (optional).
- Verify that a Tailscale exit node can't be used to reach its own LAN with the planned access rules.
- Design the webhook relay (after the MVP).
| M4 spike results (`research/m4-spike.md`) | ACP client driving pi works; use `pi-acp` 0.0.34 (the other adapter needs Bun and a long-lived daemon). Neither adapter forwards MCP servers or permission requests, and pi-acp hides provider errors, so the host detects failed turns itself and configures `propose` through pi's own `mcp.json` (exposure `direct`). ACP rejects carry no reason: reasons go back as a follow-up prompt. **MXC on Windows:** filesystem confinement holds, including for child processes; MXC proxy mode and direct loopback fail on this build, so egress goes through an in-sandbox shim to a per-session named pipe to the host proxy. Two gaps to close: DNS still resolves (SR-27), and pi's bash tool crashes in the sandbox (Node and cmd work). The summary fork must copy the original request's headers |
| Subscription harness (M4 spike rerun) | for Claude subscriptions, han-host drives the official Claude Code client through `claude-agent-acp` on Claude Code's own login, which uses the plan's included usage. The login is then inside the agent's trust boundary, and the proxy is CONNECT-only with a host allow-list (no token swap). Accounting and budgets come from ACP `_meta.quota`, the rate-limit fields and `usage_update`; layer-2 summaries are made client-side. han-host MUST start it with an isolated config and strict MCP (it otherwise loads the user's own MCP servers and connectors) and pass `propose` through ACP `mcpServers`. Open for M4: sandboxing it (the login lives in `~/.claude`, and Git Bash crashes inside MXC on Windows). API keys keep the proxy-held design. A propose-only agent stayed productive (one complete `propose` call, no workarounds) |
| Semantic gates (MVP, after Anthropic's "Redesigning source control", 2026-10) | gate rules read the change set, not only the action: path globs (for example `payments/**` always needs approval) and change kinds (docs-only, tests-only or comments-only may run free). Per space and per repo. Needs a PolicyDoc rule change (spec round 33) and a change-set summary in effect plans |
| Views as derived data | han-node's views move to one generic derived-data mechanism: each view is an incremental index declared once and computed from the log, streams and timelines. A new view (search, "who touched this line", history) is then a declaration, not bespoke code. A refactor after M3 merges |
| Messaging | public posts may reference Anthropic's source-control post: it shows git strains at agent scale, and HAN covers the other half (live visibility of every agent and verifiable approvals) while working alongside git |
| Version control is pluggable | han-host, han-exec and overlap tier 3 reach version control only through a small `Vcs` trait: create a workspace at a base, diff into a changeset, land (a compare-and-swap ref move or a server-side API), and merge-simulate. Git is the first backend. A Sapling backend (Meta's open-source, Scion-like system) comes after the MVP to prove the trait. HAN does not ship its own version-control system |
| Workspaces on runners | one read-only, content-addressed object store per device, shared by all session workspaces; each session has its own refs and index (SR); sessions use partial clones plus sparse checkout, so they start fast and use little disk |
| Changesets | the unit of landed work is a changeset: the files changed against the base, with content hashes. Semantic gates read it, and the executor checks the landed change against the approved changeset digest |
| Risk tiers for landing work (MVP; after Scion and Antique) | every changeset gets a tier from per-space and per-repo rules over its paths, change kinds and size. **Tier 0, auto:** docs, comments or tests only, generated files, small, CI green on this exact changeset. **Tier 1, auto plus machine review:** ordinary code outside sensitive paths, CI green, plus a signed review by an independent reviewer agent. **Tier 2, human:** sensitive paths or large diffs, approved by the path's owner, falling back to the agent's operator. **Tier 3, two people:** releases, infra, secrets, auth and crypto, migrations. **Never auto, whatever the rules say:** HAN's own policy and gate config, CI workflows, dependency manifests and lockfiles, credentials, permission widening |
| Machine review | counts as the approval for tier 1 only. The reviewer is a different model or vendor from the author, outside the author's operator chain, and signs over the changeset digest. It never counts for tiers 2–3 |
| Evidence | CI results and machine reviews are signed over the exact changeset digest, so nothing changed after testing can land |
| Optimistic landing | tiers 0–1 land without a merge queue. A watcher agent on main reverts a changeset that breaks post-merge CI, as a pre-approved effect. Deploys stay separately gated. Conflict fixes by resolver agents are new changesets and go through the gates again |
| Path owners | a space maps path globs to owners (CODEOWNERS-style). Tier-2 approvals go to the path owner. The board shows approval wait times and queue depth so the tiers can be tuned |

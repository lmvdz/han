# Software factories: how HAN works alongside them

Written 2026-10-14. Status: plan. Nothing here is built yet. The roadmap is PLAN.md §10.3; the planned security controls are SR-110…SR-121 in `research/security-requirements.md` §13; the spec questions are under [Needs spec](#9-needs-spec-round-33) below.

**Sources.**
- *The Secure Software Factory: A Reference Architecture*, version 1, 2026-10-13 (github.com/agentbaseline/agentbaseline, `software-factory/software-factory.md`), with its appendix "Security controls by boundary".
- Agent Baseline `1.0-draft`: 35 controls across six outcomes (agentbaseline.org/controls; `whitepaper/CONTROLS.md` in the same repository).
- HAN: `README.md`, `PLAN.md`, `spec/core/architecture.md`, `spec/profiles/han.status-1.md`, `research/security-requirements.md`, and the code on `main` at the date above. SR-106 (a presence check before the node signs an approval) and SR-107 (node API connection classes) come from the approval-presence work in flight.

---

## 0. Answers first

1. **HAN is not a software factory.** A factory turns a prompt, ticket or spec into tested code and, more and more, deploys and runs it. HAN is the layer a team shares while that happens: who (person or agent) is on what, where work overlaps, which outside actions wait for approval, and who steered what. People will run factories (CI pipelines, durable workflow engines, orchestrator agents) next to HAN. HAN should make them visible and governable, not replace them.
2. **HAN already does a lot of what the reference architecture asks of a Level 1 factory**, for the agents it hosts itself. The sandbox is the gate, the egress proxy is the only way out, credentials stay in the proxy and han-exec, and every outside action goes through a signed, plan-bound approval in a hash-chained log. Of the 94 rows mapped in §2, 32 are ✅, 52 ⚠️ and 10 ❌. Most of the ⚠️ and ❌ rows are factory concerns HAN deliberately leaves to the factory (§5), or the four interop areas below.
3. **Four interop areas, all chosen:**
   1. **External-runner mode** (§4.1). A factory's own agents report sessions, status and proposals into a HAN space through a small bridge, a `han report` CLI, a Rust client (with thin TS and Python wrappers over the bridge's HTTP API) or HAN's MCP tools. They hold no HAN key. They show as **advisory**, because HAN can't see their sandbox.
   2. **The approval API** (§4.2). A factory step asks HAN for an approval and waits. The factory is the **executor** of its own gate effect, and HAN is the gate. The claim receipt is the permit. Approvals still come only from human devices with a presence check, so no API path can approve, and a factory can't approve its own work.
   3. **Audit export and run correlation** (§4.3). Export the signed log as a self-verifying bundle and a streaming tail. A third party verifies it from the server keys alone. Factory run IDs and W3C trace context are carried on sessions, tasks and effects.
   4. **Signals, profiles, attestations, the kill switch and the registry hook** (§4.4). A signed-webhook intake turns alerts and findings into tasks the way han-github turns issues into tasks. Agent profiles become named, signed, versioned documents. han-exec emits an in-toto attestation when it lands work. One command halts an agent everywhere. Registry traffic can be sent through the team's firewall.
4. **Order** (PLAN §10.3): the kill switch first, because it is small and it closes a real gap today. Then external runners and the approval API, then export and correlation, then intake, then profiles, attestations and the registry route. Everything is after the M6 dogfood except the plan itself.

---

## 1. Terms, side by side

| Reference architecture | HAN today |
|---|---|
| software factory | not HAN; a factory is one more kind of participant in a space |
| agent | an agent actor (`@name:server`), with a human `operator` fixed in its profile (architecture §4.3) |
| whoever the agent acts for | the author of the `task` driving the session (`delegate_of` in `han.status/1`); a task's `external` names the ticket |
| orchestrator | a person (Level 1); han-github plus the agent pool (a thin Level 2); an orchestrator agent is an ordinary actor whose plans are `task`s, never grants |
| execution layer | han-node's supervisor plus `han host` per session (PLAN §3) |
| sandbox | MXC on Windows, bubblewrap on Linux, seatbelt on macOS, around the whole agent process tree (PLAN §6.4, SR-23…SR-34) |
| egress proxy | the per-session proxy in han-host (SR-35…SR-45) |
| model gateway | the proxy's model route: key injection, path allow-list, budget reservations (SR-39…SR-41) |
| runtime authorization service | Policy, six pure functions over the log, decides every outside action per plan (architecture §6); the egress allow-list decides reach, per space and agent, not per task |
| approval policy | `policy.approvals`, `separateApprover`, guards (P-08, P-15); semantic gates and risk tiers are queued for round 33 (PLAN §12) |
| high-impact action | an `effect.propose` whose action a rule gates (push, PR, HTTP write, deploy) |
| tamper-evident audit log | the space log: signed entries, hash-chained receipts, replayable by any verifier (R-14) |
| run | a session (`session.open`) and the task it works; there is no external run ID yet (§4.3) |
| profile | the `HostSpec` that `han host --spec` reads plus the agent's registry entry (`agents.json`); not yet signed or versioned (§4.4.2) |
| signals | han-github issues today; a generic intake later (§4.4.1) |
| kill switch | `AgentStop` (one node), a ban, the operator's withdrawal, `key.revoke`; not yet one command (§4.4.4) |
| provenance and attestations | receipts and outcome evidence in the log; no portable attestation yet (§4.4.3) |

---

## 2. Mapping: every invariant and control to HAN's state

**Key.** ✅ HAN does this today for the agents it hosts, with the reference given. ⚠️ partly, or only for some agents, or planned with a tracked item. ❌ not done. A ❌ can be a deliberate non-goal (§5); the note says so. **Scope:** every row judges HAN's own host and executor. For a factory's agents in external-runner mode, the four boundaries are the factory's, and HAN shows such sessions as advisory (§4.1).

References: `SR-nn` is `research/security-requirements.md`; `P-`, `R-`, `X-`, `N-` rules and `§` numbers are `spec/core/architecture.md`; "PLAN" is `PLAN.md`.

### 2.1 The invariants

| # | Invariant (paraphrased) | HAN | Where, and the gap |
|---|---|---|---|
| I-1 | **Sandbox.** Agent code runs only in an isolated sandbox it can't change or escape, with no host or cross-task access, and the isolation is tested. | ⚠️ | The whole process tree is confined (SR-23), writes stay in the session worktree (SR-24), secrets and other sessions are unreadable (SR-25, SR-83), and the sandbox probe tests each forbidden operation. Gaps: a space may allow advisory mode (SR-23), the Claude Code login lives inside the agent's trust boundary (PLAN §6.4 Model credentials), and the M4 spike found DNS still resolving under MXC (SR-27). |
| I-2 | **Network.** Once the sandbox starts, nothing gets in or out except through paths the agent can't reconfigure or bypass. | ✅ | The only reachable destination is the session's own proxy port (SR-27), with a per-session proxy credential (SR-28) and no node API in the sandbox (SR-29). |
| I-3 | **Supply chain.** Everything the factory is built from, takes in or reaches comes from approved sources the agent didn't choose, and nothing entering the sandbox carries credentials. | ⚠️ | The Node runtime and ACP adapters are pinned with hashes (SR-75, SR-76) and registries are on the allow-list (SR-35, SR-105). Gaps: no admission point that scans or holds back new versions (§4.4.5), and task and issue text entering the sandbox isn't screened for embedded credentials (SR-52 masks only what leaves). |
| I-4 | **Identity, part 1.** An agent uses only the access granted for its task, never holds credentials, and can't grant itself more. | ⚠️ | No credentials in the sandbox (SR-33, SR-39, SR-46). Every outside action is approved per plan digest (P-07, P-08). An agent can't be an owner, so it can't change policy (P-03). Gap: reach (the egress allow-list) is per space and agent, not narrowed per task. |
| I-5 | **Identity, part 2.** High-impact actions and changes to the factory's own controls need approval the agent can't give, and the factory doesn't change during a run. | ✅ | Gated effects need approvers chosen by `policy.approvals` (P-08); `separateApprover` stops self-approval; a guarded agent can't approve, govern or steer (P-15); only owners, who are human, append `policy.set`; risk tiers make HAN's own gate config "never auto" (PLAN §12). A host's spec is fixed when the session starts. Being added: a presence check on the approving device (SR-106). |
| I-6 | **Execution.** Agents start and stop only through the execution layer, inside all four boundaries, and don't start if an enforcement point is down. | ⚠️ | Sessions start only through the supervisor and `han host` (SR-08); no sandbox means no start unless advisory is allowed (SR-23); a runner starts an agent only for a signed task Policy allows (SR-86). Gap: stopping is per node; there is no stop "everywhere" (§4.4.4). |
| I-7 | **Observability.** Every run leaves a tamper-evident record, out of any agent's reach, covering a defined set of security events from every boundary and the execution layer, and a run that can't produce it fails closed. | ⚠️ | Every authority fact (tasks, proposals, approvals, claims, outcomes, steers, membership, policy) is in the signed, hash-chained log (R-14), and nothing touches the world without a receipt (X-03), so a run without a log fails closed. Gap: proxy decisions and sandbox denials are local logs (SR-44, SR-103), not tamper-evident, and nothing is exported (§4.3). |

### 2.2 Checklist: sandbox

| Control (paraphrased) | HAN | Where, and the gap |
|---|---|---|
| Configure each sandbox from the agent's profile (image, isolation, action policy, limits) | ⚠️ | the `HostSpec` plus `agents.json`; not a signed, versioned profile (§4.4.2) |
| Match isolation to risk: containers for trusted work, a microVM for untrusted code | ⚠️ | one level, an OS process sandbox (MXC, bubblewrap, seatbelt), with WSL2 or Podman as fallbacks (PLAN §6.4); no microVM option |
| Start only from a hardened, approved image with authenticated configuration; refuse otherwise | ⚠️ | pinned, hash-checked runtime and adapters (SR-75, SR-76), harness versions checked (SR-77), refuse without a sandbox (SR-23); configuration isn't signed (§4.4.2) |
| Enforce the profile's action policy at the boundary (commands, tool calls, processes, syscalls) | ⚠️ | filesystem and network at the OS boundary; every outside call becomes `effect.propose` (R-24); per-command policy is the harness's, and its fidelity is published (SR-34) |
| Put only what the task needs in the sandbox | ✅ | a clone per session, its own git metadata, an exclusion list (SR-25, SR-26) |
| Confine writes to the workspace, system paths read-only, no host access | ✅ | SR-24, SR-25, SR-26; toolchain roots read-only (SR-104) |
| Run unprivileged, no escalation, no container runtime socket or host devices | ✅ | unprivileged AppContainer or bubblewrap; no debug or signal outside the tree (SR-32); no host paths are bound, so no runtime socket |
| Run tests that need containers or services inside the sandbox's own isolation | ❌ | not supported: an agent can't start a test database; deliberately not the host's job before a microVM backend exists |
| Enforce resource and time limits | ✅ | memory, CPU, processes, files, disk and wall time per turn (SR-31) |
| Destroy the sandbox at the end; mediate every path in or out; artifacts advance only after approval | ⚠️ | per-session removal is required (SR-87, an M6 MUST, not yet complete); outside actions leave only as approved effects (R-24, X-09) |
| Record process, file and network activity outside the agent's reach | ⚠️ | file activity as timeline ops (PLAN §6.5), network decisions in proxy logs (SR-44); no process record, and none of it is tamper-evident |
| Stop a sandbox on demand and keep its state as evidence | ⚠️ | `AgentStop` on the session's node; the timeline and transcript seals keep state; no "freeze for evidence" (§4.4.4) |

### 2.3 Checklist: network

| Control | HAN | Where, and the gap |
|---|---|---|
| All traffic through an egress proxy enforced at the sandbox's network layer | ✅ | SR-27 (MXC shim to a per-session pipe; a bubblewrap network namespace) |
| Block uninspectable traffic, metadata endpoints and private ranges | ✅ | SR-27, SR-37 (loopback, RFC 1918, link-local, CGNAT, ULA refused after resolution) |
| Default-deny; allow only approved destinations from the profile | ✅ | SR-35, SR-38, SR-105 |
| Attach credentials at the proxy, only to authorized requests | ✅ | model key on the model route only (SR-39); GitHub and HTTP credentials only in han-exec, per approved action (SR-67) |
| Enforce each decision down to API, operation and resource | ⚠️ | CONNECT tunnels are allowed by hostname with an SNI check, never intercepted (SR-36), so they are unauthenticated reads; per-operation control exists only for the model route's paths (SR-40) and for effects, which are bound to a plan |
| Reach internal systems through a gateway, not by joining the network | ⚠️ | private ranges are refused (SR-37); internal writes go through han-exec effects; an optional upstream proxy (SR-42); no internal read gateway |
| Deny inbound by default; the factory isn't reachable from outside | ✅ | proxy on loopback only (SR-45); the peer filter admits only known node ids (SR-11) |
| Block agent-to-agent channels outside sanctioned paths | ✅ | sessions can't reach each other's proxies or worktrees (SR-27, SR-83); agents talk only through HAN, as data (SR-54) |
| Log and monitor traffic | ⚠️ | host, decision, bytes and tokens per request (SR-44); no thrash or drift monitor |

### 2.4 Checklist: supply chain

| Control | HAN | Where, and the gap |
|---|---|---|
| Build sandboxes from approved, verified components; skills pinned | ⚠️ | runtime and adapters pinned (SR-75, SR-76); the harness loads no repo configuration (SR-30); skills and context files are not inventoried |
| Admit packages and models only through an admission point that hides unsafe versions | ⚠️ | registries reachable only when a toolchain is enabled (SR-105); no scanning firewall or cooldown (§4.4.5) |
| Keep an approved set of sources | ✅ | the allow-list per space and agent (SR-35, SR-38) |
| Block a compromised component across the whole factory | ❌ | no quarantine list shared across hosts (§8, open question 9) |
| Attach provenance to every artifact (SBOM, attestations, agent, profile, versions, policy) | ❌ | HAN's own releases: SBOM is a SHOULD (SR-82). Agent output: the log and outcome evidence only; attestations are §4.4.3 |
| Keep credentials out of everything entering the sandbox | ⚠️ | credential paths unreadable (SR-25); masking is outbound only (SR-52); inbound task and issue text isn't screened (SR-112 adds it for intake) |
| Validate inputs (structure, provenance, type) separately from components; heuristic injection screening | ✅ | the data envelope with source and `trust: "untrusted"` (SR-54), fixed task templates (SR-57), strict turns after outside input (R-24, SR-56); no heuristic or LLM-judge screening, by choice |

### 2.5 Checklist: identity and access

| Control | HAN | Where, and the gap |
|---|---|---|
| Check that the requester may use the factory | ✅ | roles and Policy on every entry (P-02…P-05); a runner starts an agent only for a signed task (SR-86) |
| Give every agent its own identity, recorded with whom it acts for | ✅ | one actor per named agent with a fixed human operator (§4.3); every entry signed; the task's author and `delegate_of` |
| Short-lived, task-scoped credentials, kept outside the sandbox | ✅ | installation tokens per action, at most an hour, never on disk (SR-49, SR-50, SR-67) |
| Authorize a bounded scope per task (task, profile, delegation, requester, policy) | ⚠️ | each outside action is authorized per plan, and delegation narrows budgets and carries guards (P-05, P-15); reach isn't narrowed per task |
| Route high-impact actions to independent approval or step-up | ✅ | P-08 rules, `separateApprover`, guards (P-15); presence on the approving device (SR-106, being built); risk tiers queued (PLAN §12) |
| Revoke any agent's grants at any time, paired with stopping it | ⚠️ | ban (P-03), operator withdrawal (P-03), `key.revoke` (N-01) and effect cancel exist; hosts aren't required to stop sessions when they land (§4.4.4) |
| Keep a tamper-evident log of every grant and action | ✅ | the log (R-14); local actions inside the sandbox are timelines, not log entries |

### 2.6 Checklist: inference

| Control | HAN | Where, and the gap |
|---|---|---|
| Route inference through the proxy to a gateway; only approved models | ⚠️ | the model route (SR-39, SR-40); no model allow-list per agent; subscription harnesses use a CONNECT allow-list (PLAN §12) |
| Rate limits and token and spend budgets per task and agent | ✅ | budget reservations per call (SR-41), task budgets (P-05, P-06); no request-rate limit |
| Optional guardrails on prompts and responses | ❌ | deliberately none; responses are treated as untrusted anyway |
| Data-handling rules for what is sent to inference | ❌ | the team picks the provider; HAN keeps no data-residency rules |

### 2.7 Checklist: execution

| Control | HAN | Where, and the gap |
|---|---|---|
| Receive the request (prompt, channel message, ticket) | ✅ | `task`, `session.input`, han-github issues (PLAN §6.7) |
| Start every agent with its profile and only granted access | ⚠️ | the `HostSpec`; unsigned (§4.4.2) |
| Sequence work through the workflow; decide when artifacts advance | ⚠️ | finish flows and gated effects (PLAN §6.7); no workflow engine, by choice (§5) |
| Handle failure: retry, stop, leave a clean record | ✅ | effects never retry silently and report `unknown` honestly (X-06, P-10); sessions end `failed` with a reason; step retries are the factory's (§5) |
| Stop agents on demand, across the factory | ⚠️ | per node only (§4.4.4) |

### 2.8 Checklist: observability

| Control | HAN | Where, and the gap |
|---|---|---|
| Define the security events each boundary records, and test that runs record them | ⚠️ | the 22 log kinds are defined and covered by vectors; boundary events (proxy decisions, sandbox denials) are local logs |
| Link every boundary's records by a shared run identifier | ⚠️ | inside HAN, `refs`, `seen`, task and session ids link everything (R-17); no external run ID or trace context (§4.3.3) |
| Keep the combined record tamper-evident and out of agent reach | ⚠️ | the log is; local logs and spans are not (SR-103) |
| Version the factory's configuration and trace every run to its policy | ✅ | every `policy.set` is in the log, and each receipt sits at a known policy; host configuration is not versioned yet (§4.4.2) |
| Track evals and run metrics | ⚠️ | the board shows tests, tokens, cost and ages (PLAN §1); no evals, by choice |
| Use evals to gate changes to the factory | ❌ | out of scope (§5) |
| Watch for thrash, drift and declining performance | ⚠️ | stale and failed states, overlap alerts (PLAN §6.4, §6.6); no drift detection |
| Capture prompts and responses as protected, sensitive data | ✅ | context items only under the read right (T-07), sealed transcripts (PLAN §6.5), masking (SR-52), OTel never exports content by default (SR-51, SR-101) |

### 2.9 Agent Baseline controls

| ID | Control | HAN | Where, and the gap |
|---|---|---|---|
| DIS-01 | Authoritative agent registry | ✅ | a profile space per actor, world-readable and retained by verifiers (§4.3) |
| DIS-02 | Ownership and risk context | ⚠️ | the operator is recorded and fixed; no business purpose or risk class |
| DIS-03 | Status and decision history | ⚠️ | membership, policy and confirmation history are in logs forever; no exception records with expiry |
| DIS-04 | Authoritative component registry | ❌ | harness and adapter versions are pinned (SR-76, SR-77), but there's no inventory of models, skills or MCP servers |
| DIS-05 | Agent composition mapping | ⚠️ | status carries harness, sandbox and gate (SR-34); profiles and attestations add the rest (§4.4.2, §4.4.3) |
| DIS-06 | Effective-access mapping | ⚠️ | allow-lists and executor scopes are known, not shown as one map |
| DIS-07 | Automated discovery and reconciliation | ❌ | out of scope |
| CON-01 | Admission enforcement | ⚠️ | the hosting rule (#48), `allowAgents: "with-operator"`, signed tasks to start on runners (SR-86); no validation-evidence gate |
| CON-02 | Toxic capability combinations | ⚠️ | the design breaks the trifecta (no credentials, one egress, outside actions only by approval); no analysis tool |
| CON-03 | Isolated and confined execution | ✅ | SR-23…SR-33 |
| CON-04 | Use-case-scoped capability profiles | ⚠️ | §4.4.2 |
| AUT-01 | Distinct identity and action attribution | ⚠️ | every entry names its agent, operator, task and time; no deployment or run id (§4.3.3) |
| AUT-02 | Purpose and task-bound authority | ⚠️ | effects are bound to a task and a plan digest; reach isn't |
| AUT-03 | Delegation attenuation | ✅ | delegation depth, child budgets inside the parent's, `guardedFor` carried down and never shrinking (P-05, P-15) |
| AUT-04 | Just-in-time credentialing | ✅ | SR-49, SR-50, SR-67 |
| AUT-05 | Independent approval | ✅ | P-08, `separateApprover`, guards (P-15) |
| AUT-06 | Fail-closed authorization and circuit breaking | ✅ | one order of verdicts; `unavailable` when a profile can't be had; E1–E6 at claim; no sandbox, no start (architecture §4.1, P-07, SR-23) |
| AUT-07 | Step-up verification | ⚠️ | the presence check on the approving device (SR-106, being built) |
| AUT-08 | Just-in-time authority elevation | ⚠️ | `effect.propose` is the request path for anything outside the sandbox, approved per action; no temporary scope grants, by choice |
| AUT-09 | Proof-of-possession credential binding | ⚠️ | node ids are device keys (N-01) and a claim binds its instance key (X-01); GitHub tokens are bearer tokens |
| OBS-01 | Agent-native telemetry | ⚠️ | the log plus `han.status/1`; the model and runtime composition aren't recorded in the log |
| OBS-02 | End-to-end correlation | ⚠️ | §4.3.3 |
| OBS-03 | Behaviour and drift monitoring | ❌ | none; the board shows cost and activity only |
| OBS-04 | Unintended action detection | ⚠️ | secret masking (SR-52); semantic gates and risk tiers queued (PLAN §12) |
| OBS-05 | Intent-to-outcome evidence | ⚠️ | task, proposal, approvals, claim, outcome and evidence form one chain in the log; validation results aren't linked yet |
| OBS-06 | Evidence integrity, completeness and protection | ⚠️ | signed and hash-chained, redaction proofs, retention per space (PLAN §6.3); no external anchoring, export or legal hold (§4.3) |
| VAL-01 | Agent-specific security testing | ⚠️ | the sandbox probe tests the boundary, not each agent configuration |
| VAL-02 | First-party agentic components testing | ✅ | Policy vectors, wire vectors, differential fuzzing, the model checker, three-OS CI (README) |
| VAL-03 | Agent-generated artifact testing | ⚠️ | CI evidence signed over the changeset digest is queued with risk tiers (PLAN §12) |
| VAL-04 | Agent outcome validation | ⚠️ | approval before a high-impact action (P-08); post-merge watcher queued (PLAN §12) |
| RES-01 | Immediate stop and authority revocation | ⚠️ | §4.4.4 |
| RES-02 | Version and component quarantine | ❌ | §8, open question 9 |
| RES-03 | Impact scoping and evidence preservation | ⚠️ | the log and timelines show what each agent touched; no export (§4.3) |
| RES-04 | Safe failure and non-agent fallback | ✅ | unverified actions are denied (AUT-06 row); people can always do the work themselves, since HAN gates agents' effects, not people's work |
| RES-05 | Agentic component rug-pull protection | ⚠️ | adapters pinned with integrity hashes (SR-76); no repo-supplied configuration (SR-30); only HAN's own MCP server (PLAN §12) |

**Tally:** 7 invariants (2 ✅, 5 ⚠️), 52 checklist controls (22 ✅, 24 ⚠️, 6 ❌) and 35 Agent Baseline controls (8 ✅, 23 ⚠️, 4 ❌): 94 rows, 32 ✅, 52 ⚠️ and 10 ❌.

---

## 3. Where HAN sits at each autonomy level

| Level | The factory | HAN's place | What HAN adds | What stays the factory's |
|---|---|---|---|---|
| **1, human-orchestrated** | a developer drives a coding agent | **HAN can be the whole Level 1 setup.** `han host` is the sandbox, proxy and execution layer for the agents it starts, and the developer is the orchestrator. | live visibility of every agent across machines; overlaps; plan-bound approvals; budgets; the log | nothing, for HAN-hosted agents; agents run outside HAN (a plain local Claude Code) are not seen unless they report (§4.1) |
| **2, workflow-orchestrated** | a durable engine (CI, a step runner, Temporal-style) starts runs from requests, events and tickets | **HAN is the shared layer beside the engine.** Runs report in as advisory sessions (§4.1); high-impact steps wait on HAN approvals (§4.2); the log is exported to the team's store (§4.3). HAN's own runner pool with han-github is a thin Level 2 for teams without an engine; it doesn't retry steps or run workflows (§5). | one view of HAN-hosted and factory agents together; approvals people can verify; the factory's run ID on everything | sandboxes, egress, authorization of reach, retries, workflows, deploys |
| **3, agent-orchestrated** | an orchestrator agent turns signals into tasks; monitoring agents watch | **HAN is where an orchestrator's plans become requests, never grants.** An orchestrator agent is an actor: what it plans are `task`s and proposals, which Policy judges. Guarded agents "can flag and request but never grant" (P-15), which is exactly the reference architecture's rule for orchestrators and monitoring agents. Signals come in through the intake (§4.4.1). | intake of signals as untrusted data; guarding for orchestrators and monitors; the kill switch across hosts (§4.4.4); attestations on what lands (§4.4.3) | preflight and task scoping for the factory's own sandboxes; deploy and rollback |

**Rule for every level.** Nothing a factory sends HAN grants anything by itself. Reports are status (no authority); proposals are requests; only human-signed approvals and Policy's verdicts at claim time decide.

---

## 4. Design

Throughout: the **bridge** is a long-lived HAN node run by the team (on a runner, or next to the factory), plus one integration process, `han-factory`, that attaches to that node over the node API like han-github does. The factory's actors' HAN keys live in that node's keychain (one device, many actors, N-01). **No HAN key ever enters a factory runner or an agent sandbox.**

```
   factory (CI job, durable step, orchestrator agent)          the team's HAN
  ┌──────────────────────────────────────────┐   HTTPS   ┌───────────────────────────────┐
  │ han report / Rust client / TS, Py wrapper│ ────────▶ │ han-factory (bridge process)  │
  │ han mcp --bridge (in the factory harness)│  run token│   │ node API, class "integration"│
  │ run id, traceparent                      │ ◀──────── │ han-node (holds factory keys) │──▶ home (sequencer)
  └──────────────────────────────────────────┘  signed   └───────────────────────────────┘
                                                callbacks
     alerts, scanners, evals ── signed webhook ──▶ han-intake ──▶ han-node ──▶ tasks
     verifier node ── han export --follow ──▶ JSONL bundle / object-lock bucket / SIEM
```

### 4.1 External-runner mode

**Goal.** A factory's agents, running anywhere (CI, a durable engine, Level 2 or 3), appear on the board next to HAN-hosted agents: who is on what, the step they're at, the files they touch, overlaps. They can propose outside actions into HAN and take work from HAN, without HAN's host and without a HAN key.

#### 4.1.1 Interfaces

| Surface | For | Talks to | Notes |
|---|---|---|---|
| **Bridge HTTP API** (`han-factory`) | everything below | its node, over the node API | the one supported wire surface for factories; small, JSON, described by an OpenAPI file; versioned `/v1` |
| **`han report …` CLI** | shell steps in CI and runners | the bridge (`--bridge <url>`, a run token) or a local node (`HAN_DATA`) on a persistent runner | `open`, `status`, `item`, `propose`, `await`, `claim`, `outcome`, `close` |
| **Rust client** (`han-factory-client`) | Rust factories and the CLI itself | the bridge | typed, one function per endpoint |
| **TS and Python wrappers** | most factories | the bridge's HTTP API | thin, generated from the OpenAPI file; **not** wrappers over the node API, which is a local implementation API (PLAN §4 Normative tiers) and reachable only from the same OS user (SR-04, SR-05) |
| **`han mcp --bridge <url>`** | any MCP-capable harness the factory runs | the bridge | a stdio MCP server with HAN's agent tools (`propose`, `team.sessions`, `team.who_is_on`, `team.read`, `team.overlaps`) plus `report_status`; the same data envelope (SR-54) and the same agent-allowed command set (SR-06) |

**Endpoints (`/v1`).**

| Call | Becomes in HAN |
|---|---|
| `POST /runs {profile, runId, traceparent?, task?, repo?, issue?, title?}` | `session.open` by the profile's actor (§4.1.2); content carries the correlation (§4.3.3); returns the session id |
| `POST /runs/{run}/status {state, step, plan?, files?, tests?, tokens?}` | a `han.status/1` item on the session's stream; the bridge fills `session`, `agent`, `harness`, `seq`, `at`, and `sandbox` (below) |
| `POST /runs/{run}/items {type: output \| context, …}` | stream items, under the space's watch and read rights (T-07) |
| `POST /runs/{run}/tasks/{task}/update {state, text?}` | `task.update` (accept, progress, final) by the actor |
| `POST /runs/{run}/propose {executor?, action, target, args?, cost?, expiresIn?}` | `effect.propose` naming the run's task; the executor defaults to the team's han-exec |
| the approval calls | §4.2 |
| `POST /runs/{run}/close` | `session.close` |

**Idempotency.** Every call carries `Idempotency-Key` (the factory's run id plus step); the bridge maps it to the HAN entry id it made, so a retried step never proposes twice (R-18's rule for hosts, applied to the bridge).

**Timelines.** Status `files` and `hunks` feed overlaps and the board from day one. Full live timelines need a mirror over the working directory; `han report watch <dir>` can run the existing mirror (PLAN §6.5) in the factory runner, outside the agent's sandbox, as a later step.

#### 4.1.2 Identity

| Option | Pros | Cons |
|---|---|---|
| **An actor per agent profile** (`@acme-incident:home`, `@acme-coder:home`), recommended | approval rules, bans and the kill switch can name one kind of agent; attribution says which profile acted (AUT-01) | a few more profile spaces and confirmations |
| An actor per factory (`@acme-factory:home`), the profile in the session | one confirmation | halting one kind of agent halts the whole factory; rules can't tell profiles apart |
| An actor per run | the finest attribution | a profile space per run; confirmations can't keep up; rejected |

- Each actor is created on the team's home (`han agent create`, `actorKind: "agent"`) with a **human operator**, who confirms it (`han actor confirm`), so under `allowAgents: "with-operator"` (the default being set now) withdrawing that confirmation stops it everywhere (P-03).
- The bridge itself is a `service` actor with its own operator. It authors nothing in spaces except through the factory actors' keys, which only it holds.
- **Authenticating a run to the bridge.** Each call carries a short-lived run credential, never a HAN key:
  - **CI OIDC** (GitHub Actions, GitLab, Buildkite): the bridge verifies the JWT against the configured issuer and maps claims (repository, workflow, ref, environment) to one factory actor and space;
  - or a bridge-issued run token (at most an hour, bound to one actor, one space and one run id) for engines without OIDC.
  - A credential that maps to no configured actor is refused. (SR-113.)

#### 4.1.3 What external actors can and can't do

| Can | Policy reference |
|---|---|
| open and close sessions, publish status, output and context items | P-15, T-07 |
| accept, progress and close tasks assigned to them; take pool tasks by making their own task with the same `external`, like pool agents (PLAN §10.2 Agent pool) | P-05 |
| create subtasks under a task they work, within delegation depth and budget | P-05 |
| propose effects, naming their task | P-06, P-07 |
| claim an effect **only when they are its named executor** (their own gate effects, §4.2) | P-07 E1 |
| report outcomes and resolve their own unknowns with evidence | P-09, P-10 |
| read board views, sessions and overlaps within their role, as untrusted data | P-02, SR-54 |

| Can't | Why |
|---|---|
| approve anything, including others' proposals | the bridge's node connection is class `integration`, which can't send `EffectApprove` (SR-107, SR-113); approvals are signed only after a presence check on a human device (SR-106); and spaces should guard factory actors (below) |
| govern: `member.set`, `policy.set`, `space.move` | non-humans can't be owners (P-03); guarded authors can't govern (P-15) |
| steer another session | guarded authors can't (P-15); `session.input` is a face command |
| claim an effect someone else executes | E1 |
| make their sessions look enforced | the bridge, not the caller, sets `sandbox` (below) |

**Guarding.** A space that admits factory actors SHOULD set `forceGuarded`, or the factory's operator SHOULD open guarded sessions. Then everything a factory actor proposes waits for its operator's approval as well as the space's rule (P-15), and it can never approve, govern or steer. Whether a space can force guarding on external actors only, without forcing its HAN-hosted agents, is a spec question (Needs spec 1).

#### 4.1.4 Fidelity: always advisory

HAN can't see a factory's sandbox, so it never presents one as enforced.
- **Works today.** The bridge sends `sandbox: "external:<factory>"` and omits `gate`. `han.status/1` already says a face MUST mark a session advisory when `gate` is missing (SR-99), so external sessions show as advisory with no spec change.
- **What the factory claims** about its own sandbox (its profile digest, isolation level) is shown as the factory's claim, in the session's details, never as HAN's fact.
- **Needs spec** (1): an explicit field (`host: "han" | "external"`, or a `fidelity` value) so a face doesn't rely on a missing member, and the rule that a face never shows an external session as enforced whatever it claims.

#### 4.1.5 Trust boundaries and failure modes

| Boundary | Trusted for | Not trusted for |
|---|---|---|
| factory runner → bridge | that the run token's holder is the configured factory workload | anything it says about itself: status text (SR-59), its sandbox, its files |
| bridge → node | signing as the factory actors it holds, through the `integration` class only | approving, governing, steering (SR-107) |
| node → home | everything, as for any node | — |

| Failure | What happens |
|---|---|
| bridge down | reports are lost (status is not authority); proposals and approvals fail closed: a factory step that gets no answer MUST treat it as "not approved" (§4.2.4) |
| home down | the bridge's node keeps entries in its outbox (R-07); nothing can be claimed, so gated steps wait |
| run token stolen | its holder can report and propose as that factory actor, for that run, until the token expires; it can't approve or claim others' effects; the kill switch stops the actor (§4.4.4) |
| bridge's node compromised | it can sign as every factory actor it holds; their proposals still need human approvals; the operator withdraws confirmations and the keys are revoked (§4.4.4). Keep one bridge per trust domain |
| the factory lies in status | it shows as the factory's claim, advisory; authority comes only from the log |
| status flood | the bridge bounds items per run and per second, like a host (SR-59 bounds the text) |

### 4.2 The approval API

**Goal.** A factory step (deploy, merge, release, a data migration, a production write) asks for a HAN approval and waits for it. HAN is the gate; the factory runs the step.

#### 4.2.1 The shape: the factory is the executor of its own gate effect

No new authority is needed. The step is an ordinary effect whose **executor is the factory actor itself**:

1. **Request.** `POST /v1/approvals {run, step, action: "factory.deploy", target, args, changeset?, artifact?, cost?, expiresIn}`. The bridge appends `effect.propose {executor: <the factory actor>, action, planDigest, task, expiresAt}` with the plan as content. The plan names exactly what will run: the environment, the artifact digest (an image digest, a commit sha), the changeset digest if any. Response: `{proposal, planDigest, receipt}`.
2. **People approve** in a HAN face, which shows the plan from the log (SR-98). The node signs `effect.approve` only after the presence check on that device (SR-106).
3. **Await.** `GET /v1/approvals/{proposal}?wait=60` (long-poll) or a signed callback (§4.2.2). The bridge answers from the log: `waiting`, `ready` (the approvals E3 needs are in hand: the bridge evaluates it with han-core's pure Policy over its state, so no network guess), `cancelled`, `expired`, or `refused` (the actor lost its role or confirmation).
4. **Claim.** `POST /v1/approvals/{proposal}/claim`. The bridge journals and appends `effect.claim`; at the claim's seq Policy re-checks the factory actor, its operator's confirmation, the approvals, guards and budget (E1–E6). The response is the **claim receipt, the permit**: `{claim, receipt, launchBy}`. Anyone can verify it offline against the home's keys.
5. **Run.** The factory starts the step before `launchBy` (the window `W` minus `stallBound` minus a margin for the response, on the bridge's elapsed clock, X-04) or reports it as not started.
6. **Outcome.** `POST /v1/approvals/{proposal}/outcome {result, started, cost?, evidence}`. Evidence should include the run id and the digests that actually ran.

Because the factory is the executor, all of X-01…X-12 apply to it, with the bridge holding the journal and the factory adapter holding the one duty the bridge can't: **start within `launchBy`, or say it didn't start**. A factory that can't promise that should report `unknown` rather than `succeeded` when unsure (X-06).

**The alternative, when HAN should run the step:** the factory proposes with HAN's own executor (`git_push`, `open_pr`, `http_request`), and han-exec runs it. The factory agent is then only a proposer ("propose, yes").

#### 4.2.2 Long-poll or webhook

| | Long-poll (default) | Signed callback (optional) |
|---|---|---|
| Who connects | the factory to the bridge, outbound from the factory | the bridge to the factory, **outbound from HAN** |
| Inbound ports | none on the factory; the bridge's listener is the only one (on loopback, a tailnet or behind the team's ingress) | the factory exposes one URL, configured in the bridge (an allow-list, SR-114) |
| Fits | CI steps that block anyway; durable engines with activity timeouts | durable engines that sleep on a signal (Temporal-style), long human waits |
| On loss | the client re-polls; the answer comes from the log, so it is always current | the bridge retries with back-off; the factory can always fall back to polling |

**Callback format.** `POST <callback>` with body `{event: "approval.ready" | "approval.cancelled" | "approval.expired" | "agent.halted", proposal, receipts: [Receipt], at, nonce}` and a header `Han-Signature: keyId=<bridge key>, t=<time>, sig=<Ed25519 over "han-callback-v1\n" + t + "\n" + body>`. The factory checks the signature against the bridge's published key, refuses a `t` outside five minutes and a repeated `nonce`, and **SHOULD still verify the receipts against the home's keys**. A callback is a hint to claim; **only the claim receipt is the permit**, so a forged `approval.ready` can at worst make the factory try a claim that Policy refuses. (SR-114.)

#### 4.2.3 How it composes

- **Human presence** (SR-106, being built). Approving happens only in a face on a person's device, after an OS presence check (Windows Hello, Touch ID, polkit) for the exact plan digest. The bridge's node connection is class `integration` (an extension of SR-107's endpoint classes) and can never send `EffectApprove`. So no API caller, token or agent can approve. Whether an approval carries a verifiable presence attestation, so a factory can require "approved by a present human", is open for round 33 (Needs spec 3).
- **Risk tiers** (PLAN §12, round 33). A factory gate effect is classified like any landing: by action and, when the plan carries a changeset, by its paths, change kinds and size. Tier 0 runs with no approval (the claim still records it); tier 1 needs a signed machine review by a reviewer outside the author's operator chain; tiers 2–3 need people. `factory.deploy` to production, releases and migrations sit in tier 3 by default. Whether a factory's own CI result, signed by the factory, can count as tier-0 or tier-1 evidence is Needs spec 4.
- **No approving your own work.** Four layers: `separateApprover` refuses the proposer (P-08); factory actors are guarded, so their approvals never count and their proposals also need their operator (P-15); the bridge can't send approvals at all (SR-107, SR-113); and a tier-1 machine review must come from outside the author's operator chain (PLAN §12 Machine review), so a factory's own reviewer agent can't pass its own factory's work when one person operates both. (SR-115.)

#### 4.2.4 Failure modes

| Failure | Behaviour |
|---|---|
| the bridge or the home can't be reached | the step does not run: no claim receipt, no permit (AUT-06); the factory SHOULD time out its wait and fail the step, not skip the gate |
| the approval expires before the claim | E3 finds no usable candidate; `expired`; the factory proposes again if it still wants to |
| the plan changes after approval (a new artifact) | the digest no longer matches; it's a new proposal and needs new approvals |
| the factory crashes after the claim | the bridge holds `claiming` or `launching` in its journal; on restart it reports `not started` or `unknown` (X-06), and an owner resolves an `unknown` with evidence (P-10) |
| the response with the permit is lost | the client retries the claim call with its idempotency key and gets the same claim receipt; the bridge never claims twice (X-09) |
| the factory runs something other than the approved plan | HAN can't prevent it (the factory is the executor); the outcome evidence and the attestation (§4.4.3) record the digests that ran, and the mismatch is visible |
| the operator withdraws confirmation mid-wait | the claim is denied (E1, E2); the bridge answers `refused` |

### 4.3 Audit export and run correlation

#### 4.3.1 Export

**Format: a verifiable bundle.** `han.export/1` (Needs spec 9), JSON Lines, one object per line:

| Line type | Holds |
|---|---|
| `header` | space id, the exporter's actor, the export's range, format version |
| `serverKeys` | the `/.well-known/han` documents (with key windows) needed for the range |
| `profile` | each profile chain the range's receipts pin (verifiers already retain them, architecture §4.3) |
| `record` | each record in seq order: entry, receipt, and content, or the redaction proof (R-14) |
| `checkpoint` | every N records: the head receipt, so a tail consumer can check continuity |

- **What is exported:** what the exporting actor may read (P-02), and content only as the space allows (prompts, transcripts and diffs stay out by default, as for OTel, SR-101). Stream items (status, output) are not authority and not exported by default; a `--streams` option adds status items for run reconstruction.
- **Verification by a third party.** `han verify --bundle <file>` replays R-14 over the bundle alone: signatures, receipts, `prev` continuity, pins, and Policy replay with han-core. It needs no network and trusts only the server keys, which an auditor should compare with a copy obtained independently. A failed check produces the same portable `FaultProof` a verifier makes. The Python and TypeScript witnesses can verify the same bundle, which makes this a cross-implementation check, not HAN checking itself.
- **Streaming tail.** `han export --space <s> --follow` runs as a **verifier** (`read` with follow): it checks each record before writing it and stops, with a fault proof, on the first failure (R-14). Sinks: stdout, a rotating file, an S3-compatible bucket with object lock (WORM), syslog (RFC 5424) or OTLP logs. The exporter has no append path (SR-117).
- **SIEM mapping (informative).** Each record also gets a flat projection for SIEM search: `space`, `seq`, `kind`, `author`, `operator`, `task`, `proposal`, `action`, `planDigest`, `verdict`, `runId`, `traceId`, `time`. The signed record stays the evidence; the projection is for searching.
- **Anchoring.** Periodically, the exporter writes the head receipt to an external transparency log or timestamp service (C2SP checkpoints, RFC 3161 timestamps; the repository already timestamps releases in `proofs/`). This makes a later rewrite of the whole history detectable even if every HAN server colluded.
- **Redaction.** A `redact` exported later is a tombstone line. Copies already in a SIEM are outside HAN's reach; the export documentation says so, and a space that needs erasure exports headers only.

#### 4.3.2 Failure modes

| Failure | Behaviour |
|---|---|
| a sink is down | the exporter buffers to disk up to a bound, then stops following and reports; on restart it resumes from its last written seq (the log is the buffer) |
| a fault in the log | the exporter writes the `FaultProof`, signals it (T-05) and stops above the last good seq |
| the exporter's reader loses its role | the export stops at the receipt that removed it (P-02) |

#### 4.3.3 Run correlation

**Fields.** `correlation = {factory?: string, runId: string, step?: string, traceparent?: string, tracestate?: string}`: `traceparent` and `tracestate` in W3C Trace Context form; `runId` and `step` bounded, printable strings.

| Where | How | Spec |
|---|---|---|
| `session.open` content | `correlation` beside `title` | core content schema (Needs spec 5) |
| `task` content | `correlation`, for tasks a factory creates | same |
| `effect.propose` | inside the plan's `args` for factory gate effects (covered by `planDigest`, so approvers see it); for HAN actions, in the proposal's content | plan: none (args are the action's); content: Needs spec 5 |
| `effect.outcome` evidence | the run id and the digests that ran | none (evidence is free JSON) |
| `han.status/1` | an optional `run: {id, trace?}` member | profile change, no core change (unknown members are ignored today) |
| OTel spans | `han.run_id`, and HAN's spans join the factory's trace through `traceparent` | SR-101's attribute allow-list gains these ids |

Content is redactable and never read by Policy, so correlation carries no authority and can't be used to sneak instructions in: it is shown as data.

### 4.4 Signals, profiles, attestations, kill switch and registry hook

#### 4.4.1 Signals intake

`han-intake`: a separate process and an actor over the node API, built like han-github (PLAN §6.7). It turns signed webhooks from monitoring, scanners and evals into tasks for an agent pool.

- **Sources.** Each source in the config names: a verification method (HMAC-SHA256 over the timestamp and body with a shared secret; or Ed25519 or JWS with a pinned public key; or mTLS), a replay window (default five minutes), a body limit, and an **intake profile**.
- **Intake profiles** (Needs spec 7) map a source's payload to fixed fields: `source`, `kind` (`alert`, `finding`, `eval`, `incident`), `severity`, `fingerprint` (the dedupe key), `ref` (a link), and the free text. Built-in profiles: Prometheus Alertmanager, SARIF (scanners), a generic JSON profile with JSON-pointer selectors, and an eval-result profile.
- **Routing rules** decide, from the fixed fields only, whether a signal becomes a task, for which pool, with what budget. No field of the payload chooses an assignee or a budget beyond what the rules allow.
- **Tasks.** One `TaskCreate {external: "intake:<source>/<fingerprint>", to: pool}`, once per open fingerprint, cancelled when the source resolves it before an agent accepts (as han-github does with closed issues).
- **Untrusted text as data.** The task's `ask` is a fixed template ("investigate signal <ref>"). The payload's text travels as an `intake.signal/1` item and reaches agents only in the data envelope (SR-54); the intake actor counts as an outside author for R-24, so a guarded agent's turn on it runs strict (SR-57's rule for imported work). The payload is masked for credentials before it enters HAN (SR-112), because log lines in alerts often carry tokens.
- **Outbound-only option.** For teams that won't expose a listener, each built-in source also has a pull mode (poll the Alertmanager API, read a queue).

| Failure | Behaviour |
|---|---|
| bad or missing signature, stale timestamp, replay | refused with 401, counted, nothing parsed beyond the headers (SR-110) |
| a flood | rate limits per source; one task per fingerprint (SR-112) |
| injected instructions in an alert | data in the envelope; never a turn, a prompt or a tool argument (SR-111) |
| a source's secret leaks | its holder can create signals for that source only; rotate the secret; tasks are still only requests |

#### 4.4.2 Agent profiles

**Goal.** One named, version-controlled, signable document per kind of agent, the factory's "profile". It maps HAN's `HostSpec` (what `han host --spec` reads) and the agent's registry entry:

| Factory profile part | HAN source today | In `han.agent-profile/1` |
|---|---|---|
| context: skills, context files, blueprints | not tracked (the harness loads no repo config, SR-30) | a list of `{path, sha256}` the host provides, read-only |
| tools and MCP servers | only HAN's MCP server (strict MCP) | the list, with versions |
| hooks | none: the harness runs with isolated settings | none, stated |
| approved packages | toolchains (PLAN §6.4 Toolchains) | toolchains and the registry route (§4.4.5) |
| sandbox: image, isolation, limits | the sandbox backend and the caps (SR-31) | backend requirements, caps, advisory allowed or not |
| access, action and network rules | the egress allow-list; the stricter-mode settings; the executor actions | the allow-list, the strict default, allowed actions |
| model | the harness, model id and credential mode | the same, plus a model allow-list for the proxy |
| harness | adapter name, version and integrity (SR-76, SR-77) | the same |

- **Storage and signing.** A profile is a JSON document in the team's repository (`.han/profiles/<name>.json`), identified by `sha256(JCS(profile))`, signed in a DSSE envelope by an owner's key. The registry entry names the profile by digest.
- **Enforcement.** The host verifies the signature against the keys of the space's owners at the session's start and refuses to start on failure, an unknown signer, or a digest the space doesn't allow (SR-121). The digest goes into every `han.status/1` item and into attestations.
- **Authority or not.** Whether "the profiles this space allows" is a `PolicyDoc` field (authority, in the log, replayable) or host configuration is Needs spec 8. The recommendation is the log: it makes "which profile did this run under" provable.
- **Later:** the same document as an OCI artifact, signed with cosign and attached as a referrer to the sandbox image, which is one of the architecture's own suggestions.

#### 4.4.3 Attestations on land

When han-exec lands work (`git_push`, `open_pr`, or the timeline accept effect) with outcome `succeeded`, it emits an **in-toto Statement v1** in a DSSE envelope:

- **subject:** the landed commit (`gitCommit` digest) and the changeset digest that han-exec checked against the approved plan;
- **predicateType:** a HAN predicate (Needs spec 6), for example `https://github.com/lmvdz/han/attestations/land/v1`, with:
  - the space, task, proposal and claim ids, and their receipts' hashes;
  - the agent and its operator; the factory and run id, if any (§4.3.3);
  - the profile digest (§4.4.2), harness and model ids from the proxy's accounting;
  - the policy in force (the receipt hash of the last `policy.set`) and the risk tier;
  - every approval: author, keyId, receipt hash, and a presence attestation if round 33 adds one;
  - `guardedFor`;
- a **SLSA Provenance v1** statement alongside, for tools that only read SLSA, with HAN's executor as the builder.

**Signing key:** the executor instance key, already published in the executor's profile space, so anyone verifies it without new key distribution (SR-119). **Where it goes:** into the outcome's evidence blob (so it is in the log), and optionally to GitHub's attestation API or as an OCI referrer. The attestation is a portable summary; the receipts it names are the proof.

#### 4.4.4 One kill switch

`han agent halt <agent> [--revoke]` gives one command that stops an agent everywhere now. It uses facts that already exist; the new parts are the host and bridge duties.

| Step | Who | Effect | Exists |
|---|---|---|---|
| 1. withdraw confirmation | the agent's operator: `member.set {role: "none"}` in the agent's profile space | in every `with-operator` space, every proposal, claim and budgeted task by or for the agent is denied from that receipt (P-03, E1, E2) | ✅ |
| 2. ban | each space owner who runs the command, in spaces they own | no role, so sessions die and nothing it proposed can be claimed (P-02, P-15) | ✅ |
| 3. cancel | the same owners | `effect.cancel` on its open effects; `task.cancel` on its tasks | ✅ |
| 4. stop sessions | every host and bridge that runs the agent, on seeing the receipt from step 1 or 2 | stop its sessions within a bound (10 s after the receipt is in hand), keep worktree and transcript as evidence, report `failed` with the reason | ❌ new duty (SR-118; Needs spec 10) |
| 5. revoke (`--revoke`) | the node holding the agent's key, or the profile's server | `key.revoke` of every agent key: permanent; resuming means a new key (N-01) | ✅ |
| 6. tell the factory | the bridge | an `agent.halted` callback, and every bridge call for the actor refused | ❌ new (SR-118) |

- **"Owner-level"** means: the operator (who owns the agent's profile space) can stop it in every space at once; a space owner can stop it in their spaces. The command does what the caller is allowed to and says what it couldn't.
- **Limit:** in a space with `allowAgents: "any"`, step 1 doesn't stop proposals (only guarded work can't be claimed: architecture §11, Accountability). The command warns about such spaces; the default is moving to `with-operator`.
- **External runners:** HAN can't kill a factory's process. It stops everything that passes through HAN at once (approvals, claims, reports) and notifies; the factory's execution layer must honour `agent.halted`. This is advisory for the factory, enforced for HAN.
- **Undo:** the operator confirms again, owners restore the role; a revoked key is gone for good.

#### 4.4.5 The supply-chain hook

Route package traffic through the team's registry proxy or supply-chain firewall (Artifactory, Nexus, a Socket or similar firewall), which holds back unsafe and too-new versions.

- **Per toolchain**, the host writes the registry setting into the agent's own tool configuration, never the repository's: cargo `source.crates-io.replace-with` in the per-agent `CARGO_HOME`, `npm_config_registry`, `PIP_INDEX_URL` / `UV_INDEX_URL`.
- **Allow-list:** with a registry proxy set, the toolchain's public hosts are not added (SR-105 changes to "the configured registry host instead"), so the proxy is the only way to packages.
- **Credentials:** if the registry proxy needs them, the egress proxy can't add them to a CONNECT tunnel (SR-38). So it gets a **registry route** like the model route: the tool's base URL points to the local proxy, which forwards over verified TLS to that one host and injects the credential, which never enters the sandbox (SR-120).
- **The host-side prefetch** (`cargo fetch` before launch on MXC, PLAN §6.4) uses the same registry.

---

## 5. What HAN deliberately does not do

- **No deploy orchestration.** HAN gates a deploy (an approval, a permit, an attestation); the factory or CD system deploys, canaries and rolls back.
- **No durable workflow engine.** No step retries, timers, sagas or schedules. HAN's effects never retry silently (X-06); retrying is the factory's decision, made again through a new proposal.
- **No factory sandbox or microVM provider.** HAN sandboxes the agents it hosts; it doesn't run other people's.
- **No authorization service for a factory's own reach.** HAN's Policy decides HAN's effects. Per-task egress scoping inside a factory's sandbox is the factory's authorization service.
- **No model gateway product, eval platform, drift detector or SIEM.** HAN's proxy does what its own sessions need; HAN feeds evals and SIEMs through export and correlation.
- **No component registry or quarantine service** (for now, open question 9).
- **No LLM-judge or heuristic injection filter.** Untrusted text stays data (SR-54); filters can be added by a team in front of the intake, never as HAN's guarantee.
- **No trust in a factory's self-description.** External sessions are always advisory.

---

## 6. Interfaces at a glance

| Interface | Process | Normative tier |
|---|---|---|
| bridge HTTP API `/v1` | `han-factory` | an integration API, versioned, OpenAPI; not the protocol |
| `han report`, `han mcp --bridge` | `han` | CLI |
| `han.status/1` external fidelity, `run` member | profile | profile (Needs spec 1, 5) |
| approval request/await | effects (core) + bridge API | core already; a "gate effect" plan profile (Needs spec 2) |
| `han.export/1` bundle | `han export`, `han verify --bundle` | profile (Needs spec 9) |
| `correlation` content | core content schemas | core (Needs spec 5) |
| `intake.signal/1`, intake profiles | `han-intake` | profile (Needs spec 7) |
| `han.agent-profile/1` | host, registry | profile; allowed digests maybe core (Needs spec 8) |
| land attestation predicate | han-exec | profile (Needs spec 6) |
| halt duty | hosts, bridge | core host duty beside R-24 (Needs spec 10) |

---

## 7. Milestones

See PLAN.md §10.3 (F0–F6). In short: F0 this plan and the round-33 questions; F1 the kill switch; F2 external runners; F3 the approval API; F4 export and correlation; F5 signals intake; F6 profiles, attestations and the registry route.

---

## 8. Open questions (product decisions)

1. **Actor granularity:** an actor per agent profile (recommended) or per factory?
2. **Where the bridge runs:** on the team's shared runner (simplest), or as its own small service per factory (smaller blast radius, §4.1.5)?
3. **Default wait mode:** long-poll first, callbacks later (recommended), or both at once?
4. **Admitting external actors:** should a space need an explicit policy switch to admit actors whose sessions are advisory, beyond `allowAgents`?
5. **Wrapper priority:** TypeScript first (most CI glue), then Python?
6. **Attestation destinations:** the log only (default), plus GitHub's attestation API, git notes or OCI referrers?
7. **Export content default:** headers and authority content only (recommended), with transcripts by an explicit space setting?
8. **Intake listener:** ship the inbound webhook listener, or pull mode only at first?
9. **Quarantine:** should HAN carry a team-wide "blocked component" list (harness versions, adapters, MCP servers, profile digests) that every host checks at start (RES-02, supply-chain checklist), or leave quarantine to the factory and registry?
10. **HAN's own Level 2:** keep the runner pool thin (recommended), or grow it toward workflow features?

---

## 9. Needs spec (round 33)

1. **External-runner fidelity.** An explicit `han.status/1` member (`host: "han" | "external"`, or `fidelity`) and the rule that a face never shows an external session as enforced, whatever its `sandbox` claims. Also: can a space force guarding on external actors only (a `forceGuarded` scope)?
2. **The approval request/await shape.** A plan profile for factory gate effects (`action: "factory.<kind>"`, `target`, `args: {run, step, environment?, artifact?, changeset?}`), whether its executor may be the proposer itself, and whether a claim receipt is the normative portable permit (what a third party checks: receipt signature, home key window, phase).
3. **Presence on approvals.** Should `effect.approve` carry a verifiable attestation that a presence check passed (from the presence work, SR-106), so a factory or a rule can require human-present approvals?
4. **Risk tiers and factory evidence.** Can a factory-signed CI result count as tier-0 or tier-1 evidence for a changeset; who may sign it; and how "outside the author's operator chain" is decided for factory reviewers.
5. **Run-ID fields.** `correlation {factory?, runId, step?, traceparent?, tracestate?}`: on which kinds' content (`session.open`, `task`, `effect.propose`), its grammar and size limits under R-09, and `han.status/1`'s `run` member.
6. **Attestation format.** The land predicate's URI and schema, the signing key (the executor instance key or a dedicated one), and whether `effect.outcome` names the attestation (for example a content `attestation` BlobRef) so the log points to it.
7. **The intake profile.** `intake.signal/1`'s shape, the `external` grammar for `intake:<source>/<fingerprint>`, and R-24 wording that intake-made tasks are outside instructions with a fixed template (as SR-57 says for issues).
8. **Agent profiles.** `han.agent-profile/1`'s schema and signature, and whether the allowed profile digests are a `PolicyDoc` field (authority) or host configuration.
9. **The export bundle.** `han.export/1` as a profile, so any verifier can read any exporter's bundle; head anchoring (C2SP checkpoint format).
10. **Halt.** A host duty, beside R-24, to stop an agent's sessions within a bound after a receipt that removes its role or its operator's confirmation, or revokes its key; and whether a dedicated `agent.halt` fact is needed or withdrawal plus ban is enough.
11. **Factory actor kinds.** `agent` or `service` for factory actors, and whether `actorKind` needs an `external` marker at all, given it is self-declared (architecture §7.5).

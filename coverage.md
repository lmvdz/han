# HAN v1 — coverage of v0.4

Every v0.4 invariant, wire method, command, port and error code, and what happened to it in v1 (rounds 7–19): **kept** (same guarantee, different mechanism), **moved** (now private to a module, the transport binding or an extension), **weakened** (checked less strongly, with how) or **dropped** (with the reason).
v1 structure referred to below: 3 sequencer calls (`hello`, `append`, `read`); modules M1 Log, M2 Policy (5 pure functions), M3 Identity, M4 Verifier, M5 Executor, M6 Agent host, M7 Transport binding; rules R-01…R-18, R-24 and R-25, P-01…P-13, P-15 and P-16, X-01…X-07, X-09…X-11, T-01…T-07, G-01…G-05 (the generic binding, §8.3).
Sources: `packages/v0.4-r1/interface-design.md` §2, `design/public-method-map.md`, `design/command-handler-map.md`, `interfaces.ts`.

## Invariants (12/12 accounted for: 10 kept, 2 weakened with a stated qualification)

| v0.4 | Meaning | v1 mechanism | Checked by |
|---|---|---|---|
| INV-01 | identity not from VM/connection/transport | R-12: author = key in profile space | vector `revoked-author-key` |
| INV-02 | collaboration, execution, control, checkouts have distinct owners | Log/Policy own control; executors own the world (§4.5); agent hosts own thinking (§4.6); co-editing is the transport's (T-03) or an extension's | by construction |
| INV-03 | stable command identity + durable acceptance before dispatch | entry `id` (R-01), R-03, X-03 (act only on a receipt) | model NoLaunchWithoutReceipt |
| INV-04 | acknowledgment states the durability actually achieved | R-03 `durability`; R-08 `minDurability` | wire "no silent downgrade", "minDurability can't lock a space" (the storage itself: ✗, needs an implementation) |
| INV-05 | UI projection or gossip ≠ acceptance | signals and streams carry no authority (T-05, T-07): anything that matters is an entry, and only receipts count; a steer is a `session.input`, never stream text (P-15, R-24) | by construction |
| INV-06 | unknown stays unknown; no silent rerun | P-09/P-10 phase `unknown`; X-06 | model UnknownNeverSilentlyRetried |
| INV-07 | stale authority admits nothing new; running effects need a separate stop | E2–E4 at the claim's seq; launch at once or never (X-04, X-06); `effect.cancel` for running work; an owner's abandon waits on elapsed time (P-09, R-16) | model RevokedBeforeClaimNeverLaunches; vectors E2, E4 |
| INV-08 | model input is an immutable manifest | R-04 immutable headers + R-17 `seen`/`refs` + optional input blob. **Weakened:** the header (what was seen, which entries) is permanent, but redaction (P-12) can erase content and release blobs, so *what* the agent read may later be gone by design. | by construction |
| INV-09 | hash ≠ permission, authorship or truth | G-01: access needs a reference (a record, or a context item of a live session the caller may read) and per-space supply, and supply needs the uploader's own bytes (T-03: readers verify the hash) | wire "blob theft by hash is refused", "uploads complete only on the uploader's own bytes" |
| INV-10 | a fork gets fresh identity and authority, with no inherited leases or credentials | P-13; credentials are executor-private and never in entries | vector `fork-fresh-authority` |
| INV-11 | no global order; ordering only per stream | §7.2: total order per space only | by construction |
| INV-12 | missing capability → explicit failure | R-08 (version, durability). **Weakened:** v0.4's per-provider capability negotiation (`ProviderCapabilities`, `STALE_CAPABILITY`) is gone from the protocol, because providers are executor-private. An executor that lacks a capability a plan needs reports `{result: "failed", started: "no"}` with evidence, which is an explicit failure | wire "no silent downgrade" |

## Wire methods (26 → 3 sequencer calls, plus the transport binding's 7 operations)

| v0.4 method | v1 | Status |
|---|---|---|
| directory.resolve | `read(profile:@x:server)` (M3 Identity) | kept |
| context.describe | `read(space, 0, {state:true})`; the state as it was after record n: `{state:true, at:n}` | kept |
| command.submit | `append(entry)` | kept |
| command.get | resend the same entry (R-01 returns its receipt) or `read` | kept |
| run.get | `read`: the `task` and its `task.update`s | kept |
| operation.get | `read`: `effect.*`; `SpaceState.effects` | kept |
| workspace.get | executor-private; inspected through an effect `action: inspect` | moved |
| checkpoint.get | `read`: `checkpoint` entry | kept |
| events.read / events.subscribe | `read(space, after[, follow])` | kept |
| events.ack | **dropped:** the client keeps its own cursor; resume = `read(after)`. Nothing on the server depends on acks. | dropped |
| events.unsubscribe | close the stream | kept |
| events.snapshot | `read(…, {state:true[, at]})` | **weakened:** a state too large for one frame is refused with `limit`, and the reader replays records instead |
| view.get / subscribe / resync / unsubscribe | live sessions: the session stream rides the transport (T-07, G-05) and what it may show is `sessionAccess` (P-15); truth stays in `read`. No view-reset protocol: a stream carries no state to resync. | moved to transport |
| document.open / sync.exchange | **dropped from the v1 core:** shared documents are the transport's (T-03 files, e.g. Matrix rooms) or a future extension | dropped |
| presence.update / subscribe / unsubscribe | transport `signal` + `follow` (T-05, G-04) | moved to transport |
| artifact.begin / write / commit | transport files (T-03). Generic binding: `blobPut(ref, offset, bytes)` on the uploader's own server, complete only when the uploader's own bytes hash-match (G-01, G-02). Not a sequencer call. | moved to transport |
| artifact.read | transport (T-03). Generic binding: `blobGet(ref, space)` under G-01 | moved to transport |

## Commands (19 → entry kinds)

| v0.4 command | v1 | Status |
|---|---|---|
| context.post | `message` | kept |
| (chat, invites, presence, files) | the transport: T-01 carry the log, T-04 invite, T-05 signal, T-03 files; Matrix binding §8.2 | moved to transport |
| (live session open / steer / close) | `session.open` / `session.input` / `session.close` (P-15) | new |
| context.create | `space.create` | kept |
| conversation.create | `space.create` + `member.set` (agent) + `task` | kept |
| grant.set / grant.revoke | `member.set`, `policy.set` | **narrowed:** v0.4 per-resource grants with delegation narrowing become four fixed roles plus per-action approvals. To narrow access, use a smaller space. Reason: one permission unit per space keeps Policy a small pure function (R-11). |
| agent.submit | `task` (or `message` with `to`) | kept. v0.4's "final result requires known outcomes" is kept by P-05: an assignee can't close a task while an effect charged to it is claimed or unknown. |
| operation.execute | `effect.propose` → `effect.approve` → `effect.claim` → `effect.outcome` | kept |
| approval.decide | `effect.approve`; a refusal is `effect.cancel` | kept |
| submission.withdraw / run.cancel | `task.cancel` | kept |
| operation.cancel | `effect.cancel` | kept |
| workspace.ensure / suspend / resume / destroy | effects with `action: workspace.*` on the owning executor | moved |
| checkpoint.create | snapshot effects per executor, then `checkpoint` | **weakened:** a `checkpoint` is a member's list of blobs; v0.4's "complete verified dependency manifest" is not checked by the protocol |
| checkpoint.restore | effect `action: restore` naming the checkpoint blobs | kept |
| context.fork | `space.create` with `forkOf` (P-13) | kept |
| context.publish | effect `action: publish`. The stale-target compare-and-swap moves into the plan: `plan.args` carries the expected target version, and the executor reports `failed` (`started: "no"`) on mismatch. **Weakened:** this is an executor obligation, not a protocol-enforced check. | weakened |

## Ports (≈85 methods → 7 modules, 19 calls/functions)

| v0.4 port / helper | v1 | Status |
|---|---|---|
| TransportAdapter, TransportConnection | Transport binding (M7; T-01…T-07, §8) for carrying; Verifier (M4) for federation | moved to transport |
| IdentityDirectory | Identity (M3), R-12 | kept |
| AuthorizationService.authorize | Policy `decide` (M2, R-11) | kept |
| AuthorizationService.registerAttempt / lookupAttempt | the `effect.claim` entry; lookup = `read` | kept |
| AuthorizationService.consumeApproval | E3: the claim consumes one approval | kept |
| AuthorizationService.getGrant | `SpaceState.roles`, `policy` | kept |
| AuthorityService (leases: acquire/renew/validate/release) | **replaced:** the claim's position in the log is the fence, and the launch window (X-04) bounds how long a claim stays launchable. No lease objects. | kept (different mechanism) |
| CommandJournal, EventJournal | Log (M1); the sender's side is the outbox (R-06, R-07) | kept |
| SubmissionBindingStore, WorkflowLedger (claims, epochs, step keys) | **dropped as protocol objects:** multi-step workflows are sequences of entries. Each step is an entry with its own id (R-01), and `refs` link steps. The epoch/claim role belongs to the executor's claim (P-07). | dropped (subsumed) |
| AgentRuntimeAdapter (create/submit/lookup/withdraw/inspect/requestCancel) | Agent host (M6) + `task`/`task.cancel` | kept |
| AgentRuntimeAdapter (quiesce/preflight/restore/activate/fork) | **moved:** runtime continuity is the agent host's private business (R-18). HAN needs only the outbox and the log to recover. | moved |
| CollaborationAdapter | the transport (T-03 files, rooms) or a future extension | moved to transport |
| WorkspaceProvider, ExecutionAdapter, WorkspaceMaterializer, MaterializerCapabilities | Executor (M5) | moved |
| ProviderBinding (provider/account binding) | **moved:** which provider and account an executor uses is executor-private configuration; the protocol names only the executor actor | moved |
| ExternalChangeProposal (changes made outside HAN) | **moved:** an executor that notices an outside change reports it as an ordinary `message`, or proposes an effect to reconcile it | moved |
| ExecutionPermit | **dropped:** the claim's receipt is the permit (X-03), and the instance binding (X-01) replaces the local enforcement object | dropped (subsumed) |
| ArtifactStore | transport files (T-03); generic binding blob store (G-01, G-02) | moved to transport |
| RetentionReceipt, retention roots | **replaced:** retention follows unredacted references (G-01); there are no explicit roots or receipts | moved |
| NativeArtifactIndex (bindVerified, lookup) | **moved:** mapping blobs to native artifacts is executor-private (M5); the protocol sees only `BlobRef`s | moved |
| CheckpointCoordinator | `checkpoint` entry + snapshot/restore effects | kept |
| RecoveryCoverageVerifier.verify | **moved:** whether a snapshot covers what it must is the executor's judgement, reported as the outcome of its snapshot/restore effect (with evidence); no protocol-level verifier | moved |
| PublicationVerifier.verify | **moved, and weakened:** publication is an effect (`action: publish`); validation receipts are the executor's evidence. v0.4's protocol-enforced *stale-target compare-and-swap* is **no longer enforced by the protocol**: the plan carries the expected target version, and only the executor checks it | weakened |
| LiveViewAdapter / LiveViewAttachment | live sessions: `session.open` and `sessionAccess` (P-15), session streams (T-07, G-05), steering by `session.input` (P-15, R-24). The log holds the authority, the transport the stream. | moved to transport |
| ContextQueries, RunIndex, ProjectionQueries | `read(…, {state:true})` | kept |
| EventSubscriptions | `read` with `follow` (M1) | kept |
| PresenceRegistry | transport signals (T-05, G-04) | moved to transport |
| UploadSessions | transport files (T-03, G-01, G-02) | moved to transport |

## Error codes (all 49 in v0.4 `interfaces.ts`)

| v0.4 code | v1 |
|---|---|
| UNAUTHENTICATED, FORBIDDEN, STALE_POLICY, APPROVAL_REQUIRED, APPROVAL_EXPIRED, BUDGET_EXCEEDED | `denied` (the `message` names the P-rule: E3, E6, …) |
| INVALID_ARGUMENT, INTEGRITY_FAILURE, INCOMPATIBLE_ENGINE, UNSUPPORTED_FEATURE, UNSUPPORTED_VERSION, VALIDATION_REQUIRED, PRECONDITION_FAILED | `invalid` |
| NOT_FOUND | `not_found` |
| LIMIT_EXCEEDED, RATE_LIMITED, PAGE_BUDGET_TOO_SMALL, EVENT_TOO_LARGE, SLOW_CONSUMER, BUSY | `limit` (+ `retryAfterMs`) |
| UNAVAILABLE, BLOB_UNAVAILABLE, CONTENT_NOT_READY, AUTHORITY_UNAVAILABLE, UNSUPPORTED_DURABILITY, INTERNAL, RUNTIME_CONTINUITY_REQUIRED | `unavailable` |
| OUTCOME_UNKNOWN, PROVISIONING_UNCERTAIN, IMPORT_UNCERTAIN | not an error: an `effect.outcome {result: "unknown"}` record (P-09) |
| IMPORT_VERIFICATION_FAILED, CHECKPOINT_INCOMPLETE, MATERIALIZATION_CONFLICT, RECOVERY_PREFLIGHT_FAILED, WORKFLOW_INPUT_INVALID, WORKFLOW_INPUT_UNAVAILABLE | not wire errors: executor-reported `effect.outcome {result: "failed"}` with evidence |
| COMMAND_EXPIRED | `denied` (E5): a proposal may carry `expiresAt`, and no claim succeeds at or after it |
| LEASE_EXPIRED, STALE_EPOCH, STALE_HANDLE, STALE_CAPABILITY, ATTEMPT_LAUNCH_BLOCKED, ATTEMPT_REGISTRATION_CONFLICT | **cannot occur:** no leases, epochs or handles exist. Their job is done by the claim's log position (P-07), the launch window (X-04) and `effect.outcome {started: "no"}` |
| IDEMPOTENCY_CONFLICT | **cannot occur:** the id hashes the whole header, so "same id, different content" is impossible |
| CURSOR_EXPIRED, STREAM_RESET, VIEW_RESET_REQUIRED | **cannot occur in the core:** no compaction (R-04); a session stream carries no state to reset (T-07) |
| STALE_DOCUMENT_GENERATION | the transport or a future extension (no core documents) |
| MIGRATION_REQUIRED | `invalid` with `UNSUPPORTED_VERSION` semantics (R-08); migration is out of the v1 core |
| (new) | `moved` (§7.3) |

## Newly in scope (v0.4 excluded these)

- Federation between independently run servers (§7). Still no consensus and no automatic trust.
- Accountable agent identities with confirmed operators (§4.3, P-03).
- Detecting a misbehaving home (R-02 equivocation and wrong-home proofs, R-14, R-15). Continuing after a lost home is done by a **fork** without inherited authority (§7.4); forced takeover is an extension.
- Live, steerable sessions across silos (P-15, T-07, G-05, R-24): v0.4 had only live views (LiveViewAdapter); steering by other actors is new.

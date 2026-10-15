# UI spec: data layer contract

Status: draft, Team A (clean room, PLAN.md §8). This is a behaviour contract between the GUI's surfaces and han-node's API. It also lists what the GUI needs from that API beyond what the M3 node provides (PR #35). Those items are marked **[needs node]**, are carried as provisional types (DAT-3), and are collected in §12. Acceptance criteria that depend on one are labelled "pending node API (Q-…)".

**Names.** Where PR #35 fixes a name, the generated name is the one to use. This covers `needs_me`, `can_steer`, `can_cancel`, `can_approve`, `approvers`, `why_gated`, `sandbox`, `total`, `offset`, `collapsed` and `outbox_id`, as well as the query and command names. The camelCase names in these specs denote the same fields. They are only placeholders for fields that PR #35 doesn't have.

The node API has two methods (PLAN §5):
- `call(Command) → CallResult | NodeError`;
- `view(Query) → stream of ViewEvent` (a snapshot of the window, then diffs).

The GUI embeds han-node in its Tauri process. `call` is a Tauri command, and `view` streams over a Tauri IPC channel (PLAN §6.6). On the frontend, **Effect v4** owns every side effect, stream and typed error, and React reads views as **atoms** (PLAN §6.6.1).

---

## 1. Layers

```
 React surfaces ── read atoms (useAtomValue / useAtomSuspense) ── run commands (React 19 Actions)
        │
 atoms ─┼─ view atoms (one per live query)    command runners (one per Command kind)
        │   settings atoms, clock atom, derived atoms (counts, notifications)
        │
 Effect services ── HanClient · Settings · Notifier · Clipboard · Opener · Clock
        │
 HanClient layers ── Tauri (production) · Mock (fixtures, tests) · Rpc (optional browser dev build)
        │
 Tauri bridge (Rust) ── han_call(cmd) · han_view(query, channel) · han_unsubscribe(sub)
                        + han_node_restart() · han_open_log_folder()   (bridge-only, DAT-10)
        │
 han-node (in process)
```

- **DAT-1.** Surfaces never call Tauri, `fetch`, `WebSocket`, timers or storage directly. Every side effect goes through an Effect service, so that each service can be replaced in tests. Lint enforces this (§10).
- **DAT-2.** There is exactly one `HanClient` service interface with two operations, mirroring the node API:
  - `call` takes a Command and is an Effect that yields a CallResult, failing with a typed `ClientError` (§6.3);
  - `view` takes a Query and is an Effect `Stream` of decoded ViewEvents, failing with a typed `ClientError`.

  The Tauri, Mock and Rpc layers each implement it, and nothing above the layer knows which one is in use.

---

## 2. Types and decoding

- **DAT-3.** The node API types (Command, Query, CallResult, ViewEvent, NodeError and every row type) are generated from the Rust definitions (specta or ts-rs, PLAN §4). Hand-written duplicates are not allowed. CI regenerates the types and fails when the result differs from what is committed.
  - **Provisional types.** These specs need some types and fields that the node API doesn't have yet. Each one is written by hand in **one quarantined module**, `src/gen/provisional.ts`, and nowhere else. Each provisional declaration carries the Q-id of the question that asks for it (for example `Q-DAT-12`). It has a hand-written Schema next to it, so DAT-4 still applies.
  - Provisional declarations extend generated types; they never redefine them. Code outside the module imports them only through it.
  - When the generated type lands, the provisional one is deleted in the same change. CI fails if a provisional declaration lacks a Q-id, or has the same name as a generated declaration (AC-DAT-23).
  - Fixtures may fill provisional fields, so the surfaces can be built and tested before the node provides them.
- **DAT-4.** An Effect `Schema` is generated for every inbound type. **Every frame** from Tauri (call results and every view event) is decoded with it before anything else sees it. A decode failure becomes `ClientError.Decode{query or command, path, message}`. It is never a thrown exception, and the bad value is never shown in the UI.
- **DAT-5.** Unknown enum variants and unknown members follow the profile rule "unknown members are ignored" (`han.status/1`):
  - the schemas accept and drop unknown object members;
  - an unknown `state` or stream item `kind` decodes to an explicit `unknown` variant that carries the raw value (session.md "unsupported item"). It does not fail the frame.

---

## 3. The Tauri bridge

- **DAT-6.** `han_call(cmd)` returns the node's result, or a NodeError, serialised as JSON.
- **DAT-7.** `han_view(query, channel)` starts a subscription. It returns a subscription id, then sends the ViewEvents on the channel in order:
  - `Snapshot{seq, …}`;
  - `Diff{seq, ops}`, where `ops` use PR #35's DiffOp vocabulary (insert, update, remove, move, and resnapshot; §5.3).

  The node numbers its events (`seq`, §12.1) and has `Resync` and `End` events of its own (Q-DAT-8). The bridge adds nothing to them: it forwards the node's `seq`, `Resync`, `End` and `Failed`, and sends its own `Resync` (DAT-9) when the webview falls behind.
- **DAT-8.** `han_unsubscribe(sub)` ends a subscription by calling the node's `unsubscribe` (PR #35). The bridge also ends a webview's subscriptions when its window closes.
- **DAT-9.** The bridge does **not** reorder, merge or drop events, with one exception. When the webview falls behind (the channel's buffer passes a bound of 256 events or 1 MiB, whichever comes first), it drops the subscription's queued events and sends `Resync`, so a slow webview takes a new snapshot instead of growing without bound.
- **DAT-10.** Tauri capabilities grant the webview only these commands: `han_call`, `han_view` and `han_unsubscribe`, plus two bridge-only commands for the Node failed screen (shell.md §8.2):
  - `han_node_restart()` stops and restarts the embedded node. It ends every subscription with `End{error: unavailable}`, so the view atoms reconnect.
  - `han_open_log_folder()` asks the OS to reveal the node's own log folder. It takes no argument, so it can't open any other path.

  Beyond these, the webview is granted scoped dialogs, notifications, the clipboard, and opening URLs after confirmation (SR-97). Its CSP allows no remote origin (SR-95).

---

## 4. Queries the GUI uses, and what each must carry

Field names below are the GUI's view of the row. They are derived from the profiles and the log. Fields the node API does not have yet are marked **[needs node]** and are provisional (DAT-3).

### 4.1 `Board{space, filter, group, window}` (board.md)

- `filter` holds `person[]`, `agent[]`, `harness[]`, `repo[]`, `issue`, `label[]`, `path[]` (globs), `states[]`, `overlap_min?`, `mine`, `needs_me` and `text?`, with `actor?` and `include_closed`. The node fixes the grammar (§12.1, Q-DAT-11).
- `group` is `issue`, `person` or `state`.
- `window` is `{offset, limit}`, and `collapsed` lists the collapsed group keys (both PR #35).

The snapshot carries:
- `seq`;
- `total` (entries after the filter);
- `offset`;
- `entries` (in order);
- `totals` (the node's name for `counts` and `needsMeTotal`): per display state with and without the filter, `needs_me_total`, and a count per group, for headers (§12.1, Q-DAT-10).

Each entry is a group header or a row:
- **Group header:** `key`; `label`; `issueTitle?`; `count`; `needsMeCount`; `maxOverlapTier?`.
- **Row:**
  - `session`;
  - `status`: the latest `han.status/1` body, as is;
  - `person`: `{actor, displayName?, operatorConfirmed}`;
  - `agentName`;
  - `displayState`: derived by the node, including `stale` and `failed` (PR #35);
  - `state_reason?`: for failed and blocked (Q-DAT-12);
  - `receivedAt` is `last_active_ms`, on the clock of `now_ms` (Q-DAT-12);
  - `needs_me` (PR #35), with `needsMeReasons[]`, each of `approval`, `blocked`, `failed`, `stale` or `overlap`;
  - `overlap_with[]`, each `{session?, agent, person, issue?, tier, kind, files[]}` (Q-DAT-19); `overlaps` stays the list of kinds;
  - `pendingEffects[]`, each `{effect, action, waitingOn[], waitingOnMe}`;
  - `sandbox`: `{backend, fidelity: "enforced" | "advisory"}` (PR #35);
  - `issueTitle?`;
  - `labels?` **[needs node, Q-BRD-3]**.

Diff ops are PR #35's DiffOp vocabulary, keyed by entry key: insert, update, remove, move, and **resnapshot**, which replaces the whole window in one step. `totals` travels whole on every snapshot, diff and resnapshot of a Board view (Q-DAT-10).

### 4.2 `Session{id, window}` (session.md)

- `window` is `{before?: itemId, limit}`. Without `before`, the latest `limit` items are sent.

The snapshot carries:
- `seq`;
- `items` (in order) and `hasEarlier`;
- `facts`:
  - `can_steer` (PR #35); `rights {watch, read}` (Q-DAT-9, §12.1);
  - `isMine`, `amGuard`, `agentGuarded`, `guard?`, `limited` **[needs node, Q-SES-5]**;
  - `steerMode`;
  - `displayState`, `stateReason?`;
  - `closed?` `{at, by}`;
  - `sandbox`;
  - `header`: the board row's fields;
- `decisions[]`: this session's open effects, in the shape of §4.4.

The typed item vocabulary below is not yet in the node API (Q-DAT-9, §12.1). It is provisional (DAT-3) and modelled on ACP's session updates. An item has:
- `id` (stable, assigned by the node);
- `kind`;
- `seqInSession`;
- `turn?`;
- the kind's fields:
  - `message`: `{role, text, done}`;
  - `reasoning`: `{text, durationMs?}`;
  - `plan`: `{entries[{text, done}]}`;
  - `tool_call`: `{toolCallId, kind, title?, status, locations[], content[], startedAt?, endedAt?}`, using ACP's shapes;
  - `fileChange`: `{toolCallId?, path, added, removed}`;
  - `decision`: `{effect, action}`;
  - `input`: `{entry?, author, text, state}`;
  - `turnStart`: `{turn, driver: task | input | effect | own, driverEntry?, strict}` (Q-SES-4);
  - `turnEnd`: `{turn, durationMs?, tokens?}`;
  - `status`: the raw `han.status/1` body;
  - `unknown`: `{rawKind, raw}`.

Diff ops:
- insert, at the end, for new items;
- update, by item id;
- resnapshot, for changes to `facts` or `decisions`.

The host masks secrets in text before it leaves the device. Items must mark the masked spans, so that the GUI can draw them as masked (SES-3) (Q-DAT-18, §12.1).

The Record is a separate subscription, `Session{id, part: "record"}`, so that the stream window and the log facts move independently (Q-DAT-3, §12.1). Each Record entry is `{seq, entryId, kind, author, at, summary, verified, redacted?, redactReason?, effect?}`.

### 4.3 `Timeline`, `ChangeSet` and `Patches` (session.md §5, diffs.md)

PR #35 provides these three as query seams. The contents below are what the GUI needs from them. Where PR #35 doesn't fix them, they are provisional (Q-SES-1, Q-DIF-1).

Together, they carry:
- `source`: `timeline`, `git` or `merge`;
- `base`: `{version, commit}`;
- `points[]`, the scrubber index, each `{version, step?, stepTitle?, turn?, checkpoint?, kind: batch | turn | checkpoint | merge, at, author}`, windowed if large **[needs node, Q-SES-1]**;
- `changeSet` between `from` (default: base) and `at` (default: live):
  - `files[]`, each `{path, oldPath?, status, added, removed, binary?{oldSize, newSize}, mode?{old, new}, eol?{old, new}, overlap?{tier, sessions[]}, lastStep?}`;
  - `excludedCount` (Q-DAT-18, §12.1).

Diff ops: `point{…}` (new points while live), `file{…}` (insert, update or remove).

A patch per file comes from the `Patches` query for `{session, from, at, path}`. It returns the unified patch, plus hunk step attributions `[{range, step}]` (Q-DIF-1, Q-DIF-5). The GUI loads patches lazily (DIF-9). For push plans and merge previews, see Q-DIF-2.

### 4.4 `Approvals{actor}` (approvals.md)

The query covers every followed space; an optional `space` narrows it.

Each effect entry carries:
- `effect`, `space`, `action`, `target`;
- `planDigest`, `plan`, `digestVerified` (Q-APR-8, §12.1);
- `phase`, `cancelRequested`;
- `proposer {actor, kind, operator?}`, `executor`;
- `session?`, `task? {id, title?, external?, budgetLeft?}`;
- `cost?`, `chargedTo`, `expiresAt?`;
- `why_gated` (PR #35);
- `approvers` (PR #35) and `separateApprover`;
- `needed {rule: n, guards[{guard, agent, approved}]}` (Q-APR-8, §12.1);
- `approvals[]`, each `{author, at, expiresAt?, consumed, attempt?}`;
- `agentText[]`, each `{author, text}` (Q-APR-8, §12.1);
- `overlaps[]`;
- `can_approve` and `can_cancel` (PR #35), plus `approveAs[]`, `cannotApproveReason?` and `myApproval?{at, expiresAt?, queued}`;
- `section`: `needsMe`, `waitingOthers` or `recent`.

Each admission entry carries `{space, actor, kind, operator?, operatorConfirmed, at, viaMyTicket}`. Admissions are not part of the Approvals view yet (Q-DAT-13, §12.1).

A plan's `argsBlob` is shown on request (approvals.md §3.3). That needs a way to fetch a blob through the node (Q-DAT-16, §12.1).

- **DAT-11.** `digestVerified` is true only when the node has fetched the plan content and checked `sha256(JCS(plan)) = planDigest` itself (P-08). The GUI never computes it and never trusts a plan without it. When the GUI asks the node to sign, the node MUST check it again.

### 4.5 `Overlaps{space}` and `WhoIsOn{issue | path}`

- Overlaps: entries `{parties: [A, B], tier, files[{path, ranges_a[], ranges_b[]}], claim?{kind: issue | lease, ref, expires_at?}, conflict?{checked, conflicts}}` (Q-DAT-19). The pairwise view reads it filtered to one pair.
- WhoIsOn is used by the palette's `#` and `@` modes for "who is on this issue", and returns sessions.

### 4.6 `SpaceStatus{space}` and node status

The GUI needs:
- the space's name and id;
- `home {server, nodes[], reachable, offlineSince?}`;
- `lastSyncAt`, `outboxDepth`;
- `protocolVersionOk`;
- `me {role, limited, limitedReason?}`; the identity's devices come from a Devices query (Q-DAT-17, §12.1);
- `members[]`, each `{actor, kind, role, operator?, operatorConfirmed?, guarded: none | chosen | forced, liveSessions, joinedAt}`;
- `policySummary`: join, invite, approvals, sessions rights, budget, gates;
- `agents[]`: the local user's agents with their harnesses, for the start-session dialog;
- `repos[]`;
- `pendingJoin?` (for a space joined but not yet admitted).

The node's own status (running, peers, version, data folder, startup step) comes from the `NodeStatus` query, and join progress from the `JoinProgress` query. Both are in PR #35. The Node panel's telemetry (subscription and resync counts, recent error codes) is not in `NodeStatus` yet (Q-DAT-20, §12.1). Queued commands are listed by the `Outbox` query (PR #35).

### 4.7 `Cost{space | session}`

It is used only by the session header's tooltip and by Trust's budget line, for the totals and what is left. It is optional in the first version.

---

## 5. View atoms: lifecycle

### 5.1 One subscription per view

- **DAT-12.** Each live query has exactly one atom, from an atom family keyed by the query's **canonical key**: the query without its `window`, serialised as JSON with sorted keys. Every component that reads that view reads the same atom, so there is one subscription, however many components read it (PLAN §6.6.1).
- **DAT-13.** The window is a separate input atom of the view atom. Changing the window re-subscribes inside the same view atom (§5.4). It never creates a second atom.

### 5.2 State

A view atom's value is a **ViewState**:

| ViewState | Meaning | What surfaces show |
|---|---|---|
| `Pending` | subscribed, no snapshot yet | the surface's loading state (via Suspense, §5.7) |
| `Live{data, seq, at}` | a snapshot and its diffs applied, with no gap | live |
| `Resyncing{data, since}` | a gap or a `Resync` was seen; the old data is kept while a new snapshot comes | the "Resyncing…" bar |
| `Retrying{data?, error, attempt, nextAt}` | the stream ended with a retryable error; it reconnects at `nextAt` | with data: a banner ("Reconnecting in 4 s", "Try now"); without data: the loading state, with the message after 3 s |
| `Failed{data?, error}` | a terminal error | with data: the data in the quiet tone, plus the error banner; without data: the error panel (via an error boundary, §5.7) |

### 5.3 Applying events

- **DAT-14.** `Snapshot{seq}` replaces the data and sets `seq`.
- **DAT-15.** `Diff{seq}` is applied only if `seq = last + 1` (using the bridge's numbering until Q-DAT-8 lands). Any other `seq` is a **gap**: the atom moves to `Resyncing`, ends the subscription and subscribes again. It never applies a diff out of order.
- **DAT-16.** `Resync` from the bridge is handled as a gap. A `resnapshot` op from the node replaces the data in place without a gap, and the atom stays `Live`.
- **DAT-17.** `End{error}` moves to `Retrying` or `Failed` according to §5.5. `End` without an error (the node finished the view, for example because the session closed) keeps the data as `Live`, with a `closed` flag that surfaces can read.
- **DAT-18.** Applying a diff produces a new immutable data value, with structural sharing: unchanged entries keep their identity, so that rows that didn't change don't re-render.

### 5.4 Moving the window

- **DAT-19.** When the window input changes, the view atom subscribes with the new window and **keeps showing the old window's entries** until the new snapshot arrives. Only then does it swap them in one update. No intermediate empty state is ever rendered (BRD-29).
- **DAT-20.** Window changes are coalesced: while one is in flight, a newer request replaces the queued one, and at most one subscription is pending at a time.

### 5.5 Errors and retries

| Error | Kind | Behaviour |
|---|---|---|
| `unavailable` | NodeError | retry with backoff: 0.5 s, 1, 2, 4, 8, 16, then 30 s capped, ±20% jitter; the attempt counter resets after 60 s live |
| transport (the channel closed, the bridge failed) | ClientError | as for `unavailable` |
| `decode` | ClientError | one immediate re-subscribe; a second decode error within 60 s makes it `Failed` with diagnostics |
| `denied` | NodeError | `Failed`, no retry |
| `not_found` | NodeError | `Failed`, no retry |
| `invalid` | NodeError | `Failed`, no retry (a GUI bug: the diagnostics include the query) |
| `conflict` | NodeError | not expected on views; treated as `invalid` |

A surface's "Retry" button resets the atom, which subscribes again at once.

### 5.6 Keep-alive and windows

- **DAT-21.** A view atom subscribes when its first reader mounts. It unsubscribes 30 s after its last reader unmounts, so that navigating back and forth doesn't re-snapshot.
- **DAT-22.** These atoms are kept alive for the app's lifetime in the main window, because notifications and badges depend on them:
  - `Approvals{me}`;
  - `SpaceStatus` for every followed space;
  - the current space's `Board`.
- **DAT-23.** Each window (webview) has its own atom registry and its own subscriptions. Only the main window runs notifications (§7.2), so they are never duplicated.

### 5.7 Loading and errors in React

- **DAT-24.** Each surface reads its primary view atom with `useAtomSuspense` inside a Suspense boundary, whose fallback is that surface's loading state. Once data exists, the surface never suspends again for that view: `Resyncing`, `Retrying` and `Failed` with data render inline, as banners.
- **DAT-25.** Each surface has an error boundary, whose fallback is the surface's error panel. It is used only for `Failed` without data, and for render exceptions. Separate panels (the session's stream, its Changes and its Record) each have their own Suspense and error boundaries, so one failing doesn't blank the others (session.md §8).

---

## 6. Commands

### 6.1 The commands the GUI sends

| Command | Surface | Notes |
|---|---|---|
| `AgentStart{agent, harness, repo, issue?}` | board (start dialog) | |
| `AgentStop{session}` | session pane | confirmed (SES-30) |
| `SessionInput{session, text}` | session pane, approvals (cancel note) | |
| `EffectApprove{proposal, planDigest, expiresAt?, note?}` | approvals | only after APR-2 |
| `EffectCancel{proposal}` | approvals | confirmed |
| `MemberSet{space, actor, role}` | approvals (admissions), trust | |
| `SpaceCreate{…}`, `SpaceJoin{ticket}`, `Invite{space, expiresAt, autoAdmit}` | onboarding | `Invite` must return the ticket (Q-DAT-14) |
| `DeviceOffer`, `DevicePair{code}`, `DeviceRevoke{key}` | onboarding, devices | PR #35 |
| identity creation, recovery | onboarding | **[needs node, Q-SHL-7]** |

The parameters in braces are what the surfaces collect. PR #35 fixes the commands' names but may not have all of these parameters, for example `expiresAt` and `note` on `EffectApprove`, `harness` on `AgentStart`, and `autoAdmit` on `Invite`. Missing parameters are provisional (Q-DAT-15, §12.1).

### 6.2 Lifecycle

- **DAT-26.** Each command runs as an Effect program through `HanClient.call`. React binds it with a React 19 Action (`useActionState`), so a control is disabled while its command is pending, and a second activation is ignored. There is no double submit.
- **DAT-27.** A CallResult is one of:
  - `Recorded{seq, entryId}`: a receipt;
  - `Queued{outbox_id}` (PR #35): the node accepted it into its outbox and will send it when it can. The `Outbox` query then follows it until it is sent or refused;
  - `Done{value}`: commands that return a value, such as `Invite`, which returns the ticket (`CallResult::Ticket`, Q-DAT-14).
- **DAT-28.** Commands that carry authority (`EffectApprove`, `EffectCancel`, `MemberSet`, `SessionInput`, `AgentStart`, `AgentStop`, `DevicePair`, `DeviceRevoke`) are **never retried automatically** by the GUI. Delivery retries are the node outbox's job. On `unavailable`, the surface offers a manual retry.
- **DAT-29.** Optimistic display is used only for `SessionInput`, as the "sending" row (session.md §7.3), through `useOptimistic`. Approvals, cancels and role changes show their result only when the call returns, and their new state only when the view reflects it.
- **DAT-30.** While a call has been pending for more than 10 s, its control shows "Still working…". Calls are not cancelled by the GUI.

### 6.3 Error copy

The messages a surface uses unless its own spec gives more specific wording:

| Error | Message |
|---|---|
| `denied` | "The space refused this: <reason>." |
| `invalid` | "HAN couldn't process this request (<reason>). This is a bug." |
| `not_found` | "That no longer exists." |
| `unavailable` | "Can't reach <the space's home or the node> right now." |
| `conflict` | "This changed while you were acting on it." |
| transport | "Lost contact with HAN's node." |
| decode | "HAN's node sent something this version can't read. Update the app, or report it." |

Each error carries the node's reason text where there is one. A reason is rendered as plain text.

---

## 7. Time and notifications

### 7.1 The clock

- **DAT-31.** One **clock atom** yields the current time once a second. It is built on Effect's `Clock` and a `Schedule`, never on `setInterval` in a component. It pauses while the document is hidden: the visibility state is itself an atom, fed from the page's visibility events through a service. Ages (board, session header, inbox) are derived during render from this atom and each row's `receivedAt`.
- **DAT-32.** In tests, the clock uses Effect's test clock, so ACs that advance time are deterministic.

### 7.2 Notifications

- **DAT-33.** A derived atom in the main window watches the `Approvals{me}` view and the current spaces' boards. It computes the set of items in Your decisions and Admissions, and the failures of the local user's own sessions. It compares that set with the persisted **seen set** (§8) and calls the `Notifier` service under approvals.md §6, honouring the settings. The Notifier is a Tauri notification plugin in production, and a recorder in tests.
- **DAT-34.** "Seen" means it was on screen in the inbox, or it was notified. The seen set is bounded to the newest 2,000 ids and is persisted.
- **DAT-34a.** Per-person marks (seen sessions, acknowledged and dismissed items, mutes) are not GUI state. They live on the person's node, private and synced across the person's devices (attention.md §6, AN-4). The GUI reads them from the node's views and writes them with the node's commands, and never keeps its own copy beyond the view atom. When AN-7 lands, the node's `Notices` view replaces the derived atom of DAT-33, and the seen set of DAT-34 goes away.

---

## 8. Local state and persistence

| State | Where | Lifetime |
|---|---|---|
| theme, density, text size, reduced motion | settings file | permanent |
| board grouping per space; column widths | settings file | permanent |
| session pane: mode, divider position | settings file | permanent |
| diff settings, collapse patterns, accessible-diff mode | settings file | permanent |
| notification settings, seen set | settings file | permanent |
| keybindings | `keybindings.json` (shell.md §4.1) | permanent |
| last route per space; rail expanded | settings file | permanent |
| steering drafts per session; diff collapse state; tree expansion | memory (atoms) | until the app quits |
| dismissed inbox items, seen sessions, acknowledgements, mutes | the person's node, never the GUI (DAT-34a, attention.md §6) | permanent, synced across the person's devices |
| tickets, recovery keys, passphrases | never stored by the GUI | – |

- **DAT-35.** The settings file lives in the app's config folder. It is read through the `Settings` service and decoded with a Schema. Each member that fails to decode falls back to its default on its own, so one bad value doesn't reset the rest. Writes are debounced by 300 ms, and the file is written atomically.
- **DAT-36.** Settings atoms are writable atoms backed by the service. Components never write the file themselves.

---

## 9. Testing contract

### 9.1 The mock client

The Mock `HanClient` layer runs the whole GUI without a node, from **fixtures**. A fixture is declarative data (JSON), and it has:
- per canonical query key: the initial snapshot;
- a **script**, a list of steps at virtual times, each of which emits a diff, a gap, a `Resync` or an `End{error}` on a view, or changes a flag;
- per command kind: a list of scripted results (Recorded, Queued, or an error), consumed in order, plus an optional step to run when the call arrives (for example "add the approval to the Approvals view");
- flags: `outboxMode`, `keychainUnavailable`, `nodeFailsToStart`, `windowFocused`, `notificationPermission`;
- optional seeds: `settings` (an object) and `keybindings` (a JSON array, or raw text so that an invalid file can be tested); see §9.3.

The mock records every call and every subscription (query, window, time), so that tests can assert on them. A test helper advances the virtual clock and runs the script up to that time.

### 9.2 Fixtures named in the acceptance criteria

| Fixture | Contents |
|---|---|
| `empty` | a space with no sessions |
| `team` | 3 people, 7 sessions over 3 issues; 1 waiting on the local user; 1 failed; 1 hunk overlap |
| `stale` | `team` plus a row in `stale` with `receivedAt` 14 min ago |
| `overlap` | one session overlapping 2 others (hunk and conflict tiers) |
| `large` | 5,000 rows over 200 issues |
| `gap` | `team`, with a script that sends a diff with a skipped `seq` |
| `errors` | a set of variants, each failing one query or command with a chosen error |
| `advisory` | a session with `sandbox.fidelity = advisory` |
| `session-live`, `session-long` (3,000 items), `session-watch-only`, `session-others-guarded`, `session-timeline` (42 points, 3 turns, 1 checkpoint), `session-git-fallback`, `session-decision`, `session-stale`, `session-record` | session pane variants, as their names say |
| `approvals`, `admissions` | inbox variants (approvals.md §9) |
| `diff-basic`, `diff-large`, `diff-live`, `diff-binary`, `diff-huge-tree` (10,000 files), `diff-5000`, `diff-excluded`, `overlap-pair`, `overlap-pair-different-base`, `overlap-claim-only`, `push-plan`, `merge-preview` | diff variants (diffs.md §11) |

Fixtures live with the GUI's tests, and they are written by Team B from this table. Team A provides no fixture data.

### 9.3 What the mock must also cover

The mock and its harness provide the following, with the same behaviour as production, for the acceptance criteria that need them.

- **Seeded settings and keybindings.**
  - The Mock layer comes with an in-memory `Settings` service and an in-memory keybindings file, both seeded from the fixture.
  - While the app runs, the harness can **change them on disk** (`writeSettingsFile`, `writeKeybindingsFile`, either taking raw text). The change goes through the same notification path that the production file watcher uses.
  - So SHL-5 (changes apply without a restart) and DAT-35 (each bad member falls back on its own) are tested as they really run.
- **State that survives a restart.**
  - The harness, not the app instance, holds the persisted store (settings, keybindings, the seen set) and the mock node's state (followed spaces, pending joins, the outbox).
  - `harness.relaunch()` tears down the atom registry and mounts the app again. In Playwright it reloads the page, and re-injects the store before the app starts.
  - The new instance sees what a real restart would: persisted state is kept, dismissed items are kept by the node, and in-memory state (drafts, collapse state) is gone.
- **Two windows or two devices on one mock state.**
  - One mock hub can serve several app instances, either Playwright pages or RTL roots.
  - Each instance declares a role: `window: main | secondary` for multi-window tests, or `device: A | B` for pairing.
  - A call from one instance changes the shared state, and the other instances' views receive the resulting diffs.
  - This is how AC-DAT-16 (one notification across two windows) and AC-SHL-20 (pairing across two devices) run.
- **Notifications.**
  - The mock `Notifier` records each request (title, body, the item it refers to) and each permission prompt.
  - `activateNotification(id)` delivers an activation through the same event path as the OS plugin.
  - `harness.focusWindow(bool)` drives window focus.

  These cover AC-APR-19…23.
- **The bridge-only commands.** `han_node_restart` and `han_open_log_folder` (DAT-10) have mock implementations, which record their calls. A restart also ends the open subscriptions with `End{error: unavailable}`. While `nodeFailsToStart` is set, restarts keep failing. AC-SHL-31 and AC-DAT-17 use these.

### 9.4 Test ids

Tests find elements by role and accessible name first. Where that is ambiguous, these `data-testid` values are stable:
- `board-row` (with `data-session`), `board-group`, `overlap-badge`, `state-strip`;
- `stream-item` (with `data-item`), `tool-row`, `scrubber`, `decision-bar`, `steer-input`;
- `approval-card` (with `data-effect`), `sign-panel`;
- `diff-file` (with `data-path`), `tree-row`;
- `palette`, `status-bar`.

### 9.5 Checks beyond the surfaces

- **Token pairs:** a unit test lists every text-on-surface and tone-on-surface pair the components use, and checks its contrast ratio in each theme (SHL-15).
- **Accessibility:** axe-core runs in Playwright on every surface fixture, in each theme.
- **Performance:** a Playwright trace on `large` and `diff-5000` checks the long-task limits (BRD-26, DIF-15).
- **Real bridge:** a small set of Playwright tests runs against the Tauri layer with a han-node started on a temporary data folder, to check that the generated types and schemas agree with the real node (DAT-3, DAT-4).

---

## 10. Frontend rules, as checks

These restate PLAN §6.6.1 so that they can be tested.

- **DAT-37.** oxlint configuration:
  - `no-restricted-imports` bans `useEffect` and `useLayoutEffect` from `react`, and bans the default and namespace imports of `react`;
  - `dangerouslySetInnerHTML` is banned;
  - `@tauri-apps/api` is banned outside the Tauri client layer and the services;
  - the globals `fetch`, `WebSocket`, `EventSource`, `setInterval`, `setTimeout` and `localStorage` are banned outside the services;
  - CI fails on any violation, with no inline disable comments allowed in app code.
- **DAT-38.** There is no hand-written `useMemo` or `useCallback`, unless the line carries a comment naming the measured hot path (React Compiler, PLAN §6.6.1).
- **DAT-39.** Imperative widgets (the diff viewer, the file tree, virtualised lists if they need it) mount through React 19 ref callbacks that return a cleanup.
- **DAT-40.** No component copies props into state. Anything external is an atom.

---

## 11. Acceptance criteria

| ID | Given | When | Then |
|---|---|---|---|
| AC-DAT-01 | fixture `team`; the board, the peek panel and the state strip all read the Board view | the board renders | the mock records exactly one Board subscription |
| AC-DAT-02 | `team` | the user leaves the board for 10 s and returns | no new Board subscription (kept alive); after 30 s away from a session pane, its Session subscription ends |
| AC-DAT-03 | `gap` | the skipped-seq diff arrives | the diff is not applied; the atom shows Resyncing with the old data; one new subscription is made; after its snapshot, Live · *pending node API (Q-DAT-8)* |
| AC-DAT-04 | `errors`: the Board stream ends with `unavailable` | – | Retrying with attempt 1 at about 0.5 s, then 1 s, then 2 s (test clock); after the stream succeeds and stays live 60 s, a later failure starts again at 0.5 s · *pending node API (Q-DAT-8)* |
| AC-DAT-05 | `errors`: a Board frame fails its schema | – | one immediate re-subscribe; a second bad frame within 60 s gives Failed (decode), with the decode message; nothing from the bad frame renders · *pending node API (Q-DAT-8)* |
| AC-DAT-06 | `errors`: `denied` | – | Failed with no further subscription attempts · *pending node API (Q-DAT-8)* |
| AC-DAT-07 | `large` | the window moves 4 times within 100 ms | at most 2 window subscriptions are made (one in flight, one coalesced); the rows on screen never go empty in between |
| AC-DAT-08 | a stream item with an unknown `kind` | it is decoded | it decodes to the `unknown` variant; the frame is not a decode error · *pending node API (Q-DAT-9)* |
| AC-DAT-09 | a row with an unknown extra member in its status body | it is decoded | the member is dropped; the row renders |
| AC-DAT-10 | `approvals` | the user approves | exactly one `EffectApprove` call; the call is not repeated when the mock answers `unavailable`; a manual Retry sends a second |
| AC-DAT-11 | `session-live` | `SessionInput` is pending | the send control is disabled, and a second Enter makes no second call |
| AC-DAT-12 | the source tree | oxlint runs | it reports an error for a test file that imports `useEffect`, uses `React.useEffect` through a namespace import, sets `dangerouslySetInnerHTML`, or calls `fetch` in a component; CI fails |
| AC-DAT-13 | the generated types | CI regenerates them from the Rust types | there is no diff with the committed files |
| AC-DAT-14 | a settings file with one bad member (`density: 7`) | the app starts | density uses its default; every other setting keeps its value |
| AC-DAT-15 | `team`, the document hidden | the test clock advances 60 s | the clock atom emits no ticks while hidden; on becoming visible it emits once with the current time |
| AC-DAT-16 | `approvals`, two windows open | a new item arrives in Your decisions | exactly one notification is requested (from the main window) |
| AC-DAT-17 | the Tauri capability file and CSP | snapshot tests run | only `han_call`, `han_view`, `han_unsubscribe`, `han_node_restart`, `han_open_log_folder` and the scoped plugins are granted; `han_open_log_folder` takes no argument; the CSP has no remote origin |
| AC-DAT-18 | the real bridge with a temporary node | the board opens on a new space | the Board snapshot decodes, and the empty state renders |
| AC-DAT-19 | `session-live`, the Changes column's Timeline view fails | – | the stream panel still renders; only the Changes panel shows its error boundary · *pending node API (Q-DAT-8)* |
| AC-DAT-20 | `team` | the user navigates away from a session pane that had a pending draft and returns | the draft is restored from memory; the settings file does not contain it |
| AC-DAT-21 | `team`, keybindings seeded with `{key: "ctrl+j", command: "board.next"}` | while the app runs, the harness rewrites the keybindings file to bind `ctrl+y` to `board.next` instead | without a restart, `ctrl+y` moves board focus and `ctrl+j` doesn't; when the harness then writes invalid JSON, the defaults apply and the Settings banner shows |
| AC-DAT-22 | `team` | the user sets the theme to Dark and the density to Compact and types a steering draft; the harness then calls `harness.relaunch()` | after the restart, Dark and Compact apply; the draft is gone; the board subscribes again |
| AC-DAT-23 | the provisional module | CI checks it | a provisional declaration without a Q-id, or with the name of a generated declaration, fails the build; importing a provisional type from outside the module fails lint |

---

## 12. Requirements on the node API, and open questions

### 12.1 Resolved by the M3 node (PR #35)

- **Q-DAT-1.** Ending a subscription: `unsubscribe` (DAT-8).
- **Q-DAT-2.** The DiffOp vocabulary: insert, update, remove and move, plus resnapshot (§4.1, §5.3).
- **Q-DAT-4.** Node status and startup progress: `NodeStatus` (§4.6).
- **Q-DAT-5.** Queued commands: `Queued{outbox_id}` and the `Outbox` query (DAT-27).
- **Q-DAT-6.** Joined facts, in part:
  - `needs_me`, `can_steer`, `can_cancel`, `can_approve`, `approvers` and `why_gated`;
  - stale and failed, derived by the node;
  - `sandbox` on rows.

  The rest of Q-DAT-6 stays open, under Q-SES-5, Q-APR-8, Q-DAT-12 and Q-DAT-19.

- **Q-DAT-6, Q-DAT-12 (in part), Q-DAT-14.** The node's clock (`now_ms` on every snapshot, diff and resnapshot, and on `NodeStatus`) for ages; `Invite` already returns `CallResult::Ticket`.
- **Q-APR-8 (in part).** Approval rows carry `target`, `summary` (plain top-level plan arguments, cut), `plan`, `digest_verified` (the node hashed the plan itself, DAT-11) and `can_cancel`.
- **Q-SES-5 (in part).** Tasks carry `guarded_for` and `accepted`; the `Guard` view carries `guards` (P-15, Z1).
- **Spaces.** A `Spaces` query lists the followed spaces; the filter has a `needs_me` facet.

PR #35 also provides:
- Board `total`, `offset` and `collapsed` (Q-BRD-4, Q-BRD-5);
- `DeviceOffer`, `DevicePair` and `DeviceRevoke` (Q-SHL-3, in part);
- `JoinProgress` (Q-SHL-4);
- the Timeline, Patches and ChangeSet query seams (Q-SES-2; Q-SES-1 and Q-DIF-1 in part).

- **Q-DAT-8.** Every `ViewEvent` carries the view's `seq`: the snapshot's is 1, and each later event (diff, resnapshot, `Resync`, `End`, `Failed`) is the last plus 1. A face applies a diff or resnapshot only when `seq = last + 1`; any other number is a gap and it subscribes again (DAT-15). An event from a node that sends no numbers reads as `seq: 0`. Two events are new:
  - `Resync{seq}`: the face fell behind the node (its backlog passed 256 events; the bound counts events, not bytes). The node drops the backlog, sends `Resync` and ends the view. The face subscribes again for a new snapshot.
  - `End{seq}`: the node finished the view without an error (it is shutting down). The face keeps what it holds, with a `closed` flag (DAT-17).

  `Failed{seq, error}` stays the error end. A `Failed` made by a client for a lost connection has `seq: 0`.
- **Q-DAT-10.** Board snapshots, diffs and resnapshots carry `totals`, which replaces the face's copy each time (a diff carries it whole; it is not a resnapshot trigger). `totals` is
  - `all`: sessions per display state with every facet of the filter ignored, but for `include_closed` (the state strip);
  - `filtered`: the same for sessions that match the filter, collapsed groups included, so collapsing a group does not move the strip;
  - `needs_me_total`: sessions that need this device's actor, filter ignored (the badge);
  - `groups`: one `{group, count, needs_me, max_overlap_tier}` per group with a session that matches the filter, in board order, collapsed groups included. This is what a group header shows; the window may hold only part of a group.

  `total` stays what it was: matching rows with collapsed groups left out.
- **Q-DAT-7.** A window's `limit` is cut to the view's bound, and to at least 1: 2,000 for `Timeline` points, 200 for `Session` items, 10,000 for every other view (`Query::max_limit`). The default stays 200. A larger limit is not an error; `next_cursor` says whether more follows.
- **Q-DAT-11.** The filter grammar. `Filter` is a closed object: an unknown member is a decode error (`invalid`), not ignored. Inside one key any value matches; across keys all must hold; a facet a row lacks does not match.
  - `person`, `agent`, `actor`: an actor id, or its `@local` part (`@ada` matches `@ada:a.example`, `@ad` matches nothing). `person` is who the work is for, `agent` the actor running the session, `actor` either.
  - `harness`, `label`: case-insensitive equality. `repo`: case-insensitive, by `github:owner/name`, `owner/name` or `name`.
  - `path`: globs on the files a session changed, case-sensitive. `*` is a run of characters except `/`, `**` a run including `/` (`**/` may be empty), `?` one character except `/`; a pattern ending in `/` covers everything under it. At most 256 characters and 8 stars per glob, 64 values per key.
  - `overlap_min`: 1 to 3, the lowest overlap tier the row is in. `mine`: the device's own work (it is the person or the agent). `text`: every word, case-insensitively, in the row's searchable text. `needs_me`, `include_closed`, `states` and `issue` are as before.
  - A value that serde accepts but is wrong (`overlap_min` of 0, an empty value, a glob over the limits) fails the view with `invalid` when it opens.
  - The text form is `Filter::parse`: `key:value` terms (`"..."` keeps spaces, a repeated key adds values), any other word is free text, `is:mine|closed|needs_me` set the flags. A word before a `:` that is no key is an `invalid` error, never free text; quote a word to search for one with a colon.
  - The node has no source of labels yet, so `label` matches nothing until Q-BRD-3 lands. `issue` stays a single value; `actor` and `issue` keep their old shape.
- **Q-DAT-12.** `state_reason` on board rows and on the session row, for `failed` and `blocked`: what the host said (its `summary`, else its `step`), or, for a session that failed from silence, `no status for 30 minutes` (the threshold, so the row does not change with every tick). Stale rows have none: the face shows "last heard" from `last_active_ms`. `receivedAt` is `last_active_ms`: the node's clock when the last item arrived, the clock `now_ms` counts on. No second field.
- **Q-DAT-19.** Overlaps, additively:
  - `OverlapRow` gains `files[{path, ranges_a[], ranges_b[]}]` (tier 2; `parties[0]` is side A, `parties[1]` side B, and an empty range list means the whole file), `claim?{kind: issue | lease, ref, expires_at?}` (tier 1; for leases `ref` is the shorter of the two paths and `expires_at` the earlier expiry) and `conflict?{checked, conflicts}`. The pair is `parties`. The node simulates no merges yet, so `conflict` is never sent.
  - `BoardRow` gains `overlap_with[]`: one entry per other party, strongest first, folded over its overlaps with the row: `{session?, agent, person, issue?, tier, kind, files[]}`. `session` is absent for a lease or a claim by an actor with no session on it. `overlaps` (the kinds) stays for the badge.

- **Q-DAT-3, Q-DAT-9.** The `Session` query has a `part`: `facts` (the default: one row, the session with `rights {watch, read}` and `is_mine`), `items` (the typed stream) and `record`. Each is its own subscription, with its own window.
  - **Items.** A row is `{key, session, id, seq_in_session, turn?, updated_ms, item}`, with `item.kind` one of `message`, `reasoning`, `plan`, `tool_call`, `file_change`, `decision`, `input`, `turn_start`, `turn_end`, `status` and `unknown{raw_kind, raw}`. An item with an id the node has seen replaces that item's body in place (a message that grows, a tool call that moves on), and keeps its number.
  - **Where they come from.** A host publishes `StreamPublish{name: "han.item/1"}` with a body `{id, turn?, kind, ...}` for `message`, `reasoning`, `plan`, `tool_call`, `file_change`, `turn_start` and `turn_end`. Any other kind, and any body that does not fit its kind, is kept as `unknown`; a host cannot send `status`, `decision` or `input`, which the node makes itself: `status` from each current `han.status/1` item (id `status:<seq>`), `input` from each `session.input` entry (id `input:<entry>`, state `recorded`, and `seen` once a `turn_start` names it as `driver_entry`) and `decision` from each effect the session's actor proposes while the session is open (id `decision:<proposal>`). An item without an id is dropped.
  - **Window.** Pages of up to 200. `Window.from_end` asks for the latest `limit` (the default of a stream), `Window.before` for the `limit` before a key (scrolling back); `hasEarlier` is `offset > 0`. The node keeps 10,000 items and 10,000 record entries per session.
  - **Rights.** Without `watch`, the `items` and `record` parts are `denied` (the `facts` row still answers). Without `read`, `reasoning` items are left out and `tool_call` content is empty. The rights are the policy's `sessions.watch` and `sessions.read` for the device's actor, and a session of the actor's own is always readable.
  - **Memory.** Items are held in memory. A stream has no backlog, so a node that starts watching a session late has only what comes after, plus what the log replays (inputs, decisions).
  - **Record.** A row is `{key, session, seq, entry_id, kind, author, at, summary, verified, redacted, redact_reason?, effect?}`: the entries of the log that are about the session (`session.open`, `session.input`, `session.close`, and the proposal, approvals, cancel, claim and outcome of effects its actor proposed while it was open). `verified` is true for every row: the node checks the receipt before it folds a record. `summary` is one line, with a steering input's text cut to 80 characters and quoted.
- **Q-DAT-13.** Admissions are rows of the Approvals view (`Row::Admission`, keyed `<space>|adm|<actor>`), for an owner only. The node takes them from the home's `join` signal, so the list is live only: a join that came while the node was down is not listed. `kind` (`human`, `agent`, `service` or `unknown`), `operator` and `operator_confirmed` come from the joiner's profile space, which the node reads from the home (not followed, taken as the home says it); a person has nothing to confirm, so `operator_confirmed` is true; an unconfirmed agent's profile is read again every 30 s. An actor that gets a role leaves the list. `via_my_ticket` is false: the join signal does not say which ticket was used. The `needs_me` facet keeps admissions.
- **Q-DAT-15.** `EffectApprove` takes `expires_at` (a HAN Time in the future; it becomes the approval's `expiresAt`). `Invite` takes `expires_at` instead of `ttl_secs` (both is `invalid`; at most 7 days ahead) and `auto_admit`: while this node runs and the invite lives, whoever joins that space is made a member when their profile says they are a person, or an agent its operator confirmed; anyone else waits in the admissions. `note` and `harness` were there already.
- **Q-DAT-16.** `BlobFetch{space, hash, size, media_type}` returns `CallResult::Blob{hash, media_type, data}` (base64url) after the node has fetched it from the space's home, checked the size and that `sha256(data) = hash`. At most 4 MiB; a blob the space's records do not refer to is `not_found`; a size or hash that does not match is `invalid`.
- **Q-DAT-16a.** `BlobPut{space, media_type, data}` (base64url, at most 4 MiB) uploads the bytes to the space's home as the node's actor, in `blobPut` parts, and returns `CallResult::BlobRef{hash, size, media_type}`. A record of that actor that refers to the blob (an effect's `argsBlob`) makes the home supply it to the space's readers, whether the upload came before or after the record (G-01). The agent's host uses it to hand a `git_push`'s commits to the executor as a bundle (R15). It is not an agent-tool command.
- **Q-DAT-17.** The `Devices` query lists the identity's keys (the actor's profile space, which the node starts to follow when the view opens): `{key_id, state: active | revoked, added_at, revoked_at?, this_device}`. There is no `last seen`: the protocol has no presence for devices.
- **Q-DAT-18.** Excluded paths: the `Patches` and `ChangeSet` views start with a `ChangeSummary{key: "", excluded}` row when `excluded > 0`, never with a name. The count is the `excluded` member of the host's `han.timeline/1` item bodies (the most it has said); han-host does not send it yet, and a change set of a proposal has none. Masked spans: the host replaces a secret with `[masked:<rule>]`; the node marks each such marker as `[start, end)` in UTF-16 code units (what a JavaScript `slice` takes) in `masked` of message, reasoning, input and tool-content text, and of `FilePatchRow.patch`. The node reads the marker itself and ignores spans a host might send.
- **Q-DAT-20.** `NodeStatusRow` has `subscriptions` (views open), `resyncs` (views ended by `Resync` since the node started) and `recent_errors` (at most 20 of `{code, what, at_ms}`: the code of each error a command or a view answered, `what` the command's name or `view`). They change the row without a tick; a face still gets them as a diff.
- **Q-APR-8.** Approval rows have `needed{rule, guards[{guard?, agent, approved}]}`: `rule` is 1 (one approval of the rule's approvers), and for each guarded agent the proposal involves (P-15) its guard is the operator the home's profile names, if that operator confirmed it (`guard` is absent otherwise: nobody can approve for it); one operator guarding several agents is one entry. `agent_text[{author, field, text}]` holds the plan's string arguments in full (up to 2,000 characters) as the proposer's and the notes approvers wrote as theirs; a face shows them quoted.

### 12.2 Still open

Every Q-DAT item of the first list has an answer in 12.1. What is left sits in the surface specs. The attention queue, lifecycle grouping, per-person marks, search, suggested replies and notices are listed as AN-1 to AN-12 in attention.md §11.

The surface specs' own open questions (Q-BRD-*, Q-SES-*, Q-APR-*, Q-DIF-*, Q-SHL-*) list the remaining items.

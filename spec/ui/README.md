# HAN GUI: UI specifications

Status: draft, written by Team A of the GUI clean room (PLAN.md §8). These documents are the **only** input Team B needs to build `apps/han-gui`. They describe behaviour and appearance in HAN's own words. They contain no source code from any other application, and Team B must not consult any. Published packages that HAN depends on (`@pierre/diffs`, `@pierre/trees`, Effect, React, TanStack Router, Tauri) are ordinary dependencies, and their public documentation may be read.

Licence: CC-BY-4.0, like the rest of `spec/`.

## Documents

| # | Document | Surface | Acceptance criteria | Open questions |
|---|---|---|---|---|
| 1 | [board.md](board.md) | the live board: person × agent × issue rows, grouping (with lifecycle), filters, the overlap badge, stale and failed rows, done-unseen, keyboard navigation, windowed scrolling over `view(Board)` | 31 (AC-BRD-01…31); 16 pending | 7 (1 answered) |
| 2 | [session.md](session.md) | the session pane: the live stream, tool-call rows, Conversation and Steps modes, the Record, the timeline scrubber driving the diff, the decision bar, steering input, suggested replies | 35 (AC-SES-01…35); 29 pending | 6 |
| 3 | [approvals.md](approvals.md) | the approvals inbox: the approval card (plan, digest, gate, who must approve), sign and cancel flows, admissions, notifications | 29 (AC-APR-01…29); 9 pending | 8 |
| 4 | [diffs.md](diffs.md) | the diff and changed-files views on `@pierre/diffs` and `@pierre/trees`, push and merge previews, the pairwise overlap view | 23 (AC-DIF-01…23); 6 pending | 5 |
| 5 | [shell.md](shell.md) | the app shell: the window, routes, the command palette, keybindings with when-clauses, settings, theming, startup, onboarding (identity, add a device, join by ticket, create and invite), devices, trust | 34 (AC-SHL-01…34); 5 pending | 7 |
| 6 | [data.md](data.md) | the data layer contract: the Tauri bridge, the `HanClient` service, generated types with Schema decoding, view atoms, command lifecycle, loading and error states, persistence, the mock client and fixtures, lint checks, and requirements on the node API | 23 (AC-DAT-01…23); 6 pending | 20 |
| 7 | [tui-session.md](tui-session.md) | the TUI's session view (`han tui`, Stream and Record tabs): the live transcript built from `Session` items, entries per item kind, folding, grouping, following and paging, the reader, steps mode, steering and approvals through the existing keys, suggested replies, untrusted-text rules, colour-off and narrow terminals, performance budgets | 61 (TSV-1…61); 21 pending | 5 |
| 8 | [attention.md](attention.md) | attention at network scale, for the TUI and every face: relevance by role, the ranked attention queue the node computes, the frame's attention count and the jump to the top item, lifecycle grouping with roll-ups and acknowledge, per-person marks (done-unseen, acknowledged, muted), the jump picker over the node's search, notices (cues, focus silence, graces, budgets, digests, mute, outages), freshness under load | 61 (ATT-1…58, with ATT-34a, 48a, 48b); 61 pending | 8 |
| | **Total** | | **297; 153 pending node API** | **66** (9 resolved and 8 partly resolved by PR #35; Q-BRD-7 answered by attention.md) |

"Pending" marks an acceptance criterion that depends on a node API item the M3 node (PR #35) doesn't provide yet. Each is labelled "pending node API (Q-…)" with the question it waits on. Until that item lands, Team B builds against a provisional type (data.md DAT-3) and runs the criterion against the mock. The criterion is final once the generated type replaces the provisional one.

Read data.md first, for the data model, then shell.md, for the frame and the shared conventions, then the surfaces.

tui-session.md and attention.md are the terminal specs here. They apply to `apps/han-tui`, which a Team B session may extend from them alone. Their acceptance criteria run on ratatui's TestBackend through the TUI's test harness, not in a browser. Their node API needs are listed in tui-session.md §10 (P-1…P-15) and attention.md §11 (AN-1…AN-12). attention.md's node rules (who needs to act, ranking, marks, notices) apply to the GUI too.

## GUI and TUI UX priorities: workspace first, task drill-down second

HAN's broader shared-workspace experience has priority: people and agents, live activity across sessions and tasks, relationships and dependencies, attention, permissions and policy. Tasks are one level of that experience; they do not define HAN's scope or replace the broader navigation and views.

Within that workspace, a coherent task-to-result experience is a secondary UX priority for both the GUI and the TUI. Drilling into a task should make its request, progress, participating agents and sessions, blockers, questions, approvals, changes and result evidence easy to follow. The view should answer what is happening, what needs the viewer's attention, and what result can be assessed, with links back to the surrounding work.

A task might be presented as Requested → Working → Needs attention → Ready to review → Landed, where applicable. This is an illustrative journey, not a new protocol state machine or a requirement that every task produces code or lands a change. Preserve the node's authoritative states, distinguish reported progress from verified outcomes, and show failed, cancelled or uncertain outcomes honestly.

Result review should bring together the relevant diff, test results, logs, screenshots or recordings when available, and remaining concerns. Approvals should appear with the exact plan or change they authorize. This task-level polish follows the broader workspace priorities above; it must retain context and navigation across tasks, sessions and spaces. This note establishes design direction without adding acceptance criteria or new node API contracts.

Each interface should express this journey in its own idiom. The TUI should support keyboard-driven task drill-down, readable progress and result summaries, and navigation to related sessions, approvals, diffs and evidence. Visual evidence can be offered as labelled links or an explicit open action where the terminal cannot display it. Both interfaces should preserve the same meaning for task progress, authority and outcomes without requiring identical layouts.

## Shared conventions

- **Keywords:** MUST, SHOULD and MAY are as in RFC 2119. A numbered rule (`BRD-12`, `SES-4`, `DAT-15`…) is normative. Prose around it explains.
- **Wireframes** are ASCII and only indicative. Regions, their order, and what they contain are normative; exact sizes are normative only where a number is stated.
- **Glyphs** in the wireframes (`●`, `◐`, `◌`, `✕`, `⚑`, `◆`) stand for icons. Team B chooses the icon set. Each icon needs a text alternative.
- **Colours** are semantic tokens (shell.md §6.2). No spec gives literal colour values. Team B picks values that pass the contrast checks.
- **Text** quoted in the specs ("No sessions in this space yet.") is the intended copy. It may be refined for clarity, but its meaning must not change, especially for authority-related text (approvals, steering labels, the Record).
- **Commands and keys:** every action is a command with an id (`board.next`). Default keys and when-clauses are in each surface's spec, and the system is in shell.md §4.
- **Acceptance criteria** are written as Given / When / Then against the mock client and the fixtures of data.md §9. They are meant for Playwright, with the Tauri webview or a browser build using the mock layer, and for React Testing Library with the mock `HanClient` layer.
- **Authority versus status.** A theme throughout: what the log says (approvals, effects, roles, the Record) is shown as fact, while what a host reports (`han.status/1`, stream items) is shown as a report and never presented as fact (`han.status/1`, SR-98). Where the GUI needs a decision derived from Policy (can I approve, can I steer, needs me), it reads it from the node, and never re-derives it.
- **Foreign text** (anything written by an agent or another person) is always rendered as plain text or safe markdown, with no HTML passthrough, full URLs shown before opening, and bidirectional overrides made visible (SR-96, SR-97).

## Frontend rules (from PLAN §6.6.1), restated as checks

- Effect v4 owns all side effects. `view()` is an Effect Stream exposed as atoms, and `call()` runs as Effect programs (data.md §1, §5, §6).
- There is no `useEffect` or `useLayoutEffect`, and lint enforces it (data.md DAT-37). Values are derived in render; external things are atoms; loading and failure use Suspense and error boundaries; commands are React 19 Actions; imperative widgets are mounted by ref callbacks with cleanup.
- The React Compiler does the memoisation; there are no hand-written memos without a measured reason.
- Lists are windowed. There is one subscription per view, owned by its atom.
- Node API types are generated from Rust, and every frame is decoded by a generated Effect Schema.
- Lint and formatting use oxlint and oxfmt.

## Rolled-up questions for the reviewer

**Resolved by the M3 node (PR #35):**
- Board `total`, `offset` and `collapsed`;
- stale and failed, derived by the node;
- `sandbox` on rows;
- the DiffOp vocabulary (with resnapshot);
- `unsubscribe`;
- `Queued{outbox_id}` and the `Outbox` query;
- `needs_me`, `can_steer`, `can_cancel`, `can_approve`, `approvers` and `why_gated`;
- `DeviceOffer`, `DevicePair` and `DeviceRevoke`;
- `JoinProgress` and `NodeStatus`;
- the Timeline, Patches and ChangeSet query seams.

**Still needed from the node API.** These are the open items that most affect implementability. Each is a provisional type until it lands:
- a per-view `seq`, and `Resync` and `End` events (Q-DAT-8);
- the typed Session item vocabulary (Q-DAT-9, Q-SES-4);
- Board totals per state (Q-DAT-10);
- the node-side filter grammar (Q-DAT-11);
- `receivedAt` and `stateReason` (Q-DAT-12);
- Admissions in the Approvals view (Q-DAT-13);
- a CallResult that carries values, such as Invite's ticket (Q-DAT-14);
- command parameters (Q-DAT-15);
- blob fetch (Q-DAT-16);
- a Devices query (Q-DAT-17);
- the excluded-path count and secret-mask markers (Q-DAT-18);
- the Overlaps entry shape (Q-DAT-19);
- Node panel telemetry (Q-DAT-20);
- the scrubber's point index (Q-SES-1);
- the ChangeSet and Patches fields between any two points (Q-DIF-1);
- push and merge change sets (Q-DIF-2);
- guard facts for the steering label (Q-SES-5);
- digest verification, agent text and guard progress on approval cards (Q-APR-8);
- identity creation and recovery (Q-SHL-7);
- the attention queue, lifecycle grouping, per-person marks, search, suggested replies and notices (attention.md AN-1…AN-12).

**Policy and design questions:**
- where the per-space stale thresholds live (Q-BRD-1);
- withdrawing a queued approval (Q-APR-3);
- the default approval expiry (Q-APR-5);
- a `han:` URL scheme (Q-SHL-6);
- for a spec round: how per-person marks travel between one person's devices (Q-ATT-1), a chosen suggestion in the signed input (Q-ATT-2), covering for another person (Q-ATT-3), suggestions in `han.item/1` (Q-ATT-4), and sharing focus across devices (Q-ATT-6).

## Provenance

Team A studied other applications' source to learn what works. Those studies are recorded separately in the repository's research notes, which are outside Team B's checkout. Nothing in these specs names or credits the studied applications. The HAN-specific parts have no prior art and were designed from PLAN.md and `spec/`:
- the person × agent × issue board;
- overlap tiers;
- signed approvals with digests and guards;
- the timeline scrubber;
- the Record;
- identity, devices, tickets and trust.

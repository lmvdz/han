# UI spec: the session pane

Status: draft, Team A (clean room, PLAN.md §8). Behaviour and appearance only. Read with [data.md](data.md), [diffs.md](diffs.md), [approvals.md](approvals.md) and [shell.md](shell.md).

The session pane is the drill-down from a board row (PLAN §1.4, §6.6). It shows one session live:
- what the agent says and does, as typed items, never as terminal bytes;
- what it has changed so far, from its work timeline, before any commit;
- the signed facts about it in the log;
- an input to steer it.

Route: `/space/:space/session/:session` (shell.md §2).

---

## 1. Terms

| Term | Meaning |
|---|---|
| **Stream** | the ordered items of `view(Session{id})`: messages, reasoning, plans, tool calls, file changes, decision requests, steering inputs, turn ends and status items (data.md §4.2). |
| **Item** | one entry of the stream, with a stable id assigned by the node, a kind and, for tool calls, a lifecycle status. Updates to an item (more text, a new status) replace it in place; they never append a duplicate. |
| **Turn** | the items between one turn start and its turn end. A turn is driven by someone: the agent's own task, a steering input, or a task or effect naming it. |
| **Tool call** | an item with a `kind` from ACP's tool kinds (`read`, `edit`, `delete`, `move`, `search`, `execute`, `think`, `fetch`, `other`), a `title`, a `status` (`pending`, `in_progress`, `completed`, `failed`), `locations` (paths and lines) and `content` (text, diff or terminal output blocks). |
| **Steps** | a condensed reading of the stream that shows only work, one entry per step in the sense of `han.status/1`'s `step`: tool calls (grouped), plan changes and file changes, with messages and reasoning folded away. |
| **Record** | the signed log entries about this session (open, task claim, task updates, effect proposals and their approvals and outcomes, steering inputs, close). It is authority; the stream is not. |
| **Rights** | the local user's `sessionAccess` for this session: **watch** (output, status and profile items), **read** (full context), **steer** (send `session.input`) (architecture.md P-15). |

---

## 2. Layout

Wide layout (window at least 1,280 px):

```
┌──────────────────────────────────────────────────────────────────────────────────────────┐
│ ← Board   ● @lars · lars-coder · pi   #41 Board freshness   working: running tests       │
│           tests 12/0 · 41k tok · $0.38 · 20s · sandbox: mxc (enforced)    [Stop] [⋯]     │
├──────────────────────────────────────────┬───────────────────────────────────────────────┤
│ [Conversation | Steps]   [Record]        │ Changes   base a1b2c3 → live    4 files +61 −12│
│ ── turn 3 · steered by @ada ──────────── │ ┌ files ────────┐┌ diff ─────────────────────┐ │
│ @ada → "please also cover the 30 min     │ │ ▾ src/        ││ src/board.rs              │ │
│         failed state"                    │ │   board.rs ⚑  ││ @@ -40,6 +40,14 @@          │ │
│ lars-coder: I'll extend the check…       │ │   fresh.rs    ││ …                         │ │
│ ▸ read · 3 files                     ✓   │ │ ▾ tests/      ││                           │ │
│ ▾ edited src/board.rs  +14 −2        ✓   │ │   board.rs    ││                           │ │
│     (inline diff preview, 12 lines)      │ └───────────────┘└───────────────────────────┘ │
│ ◐ ran `cargo test board`        …running │ ┌ timeline ─────────────────────────────────┐ │
│                                          │ │ base ●──┬──●──◆──●──●──┬──●──●──● live ▶  │ │
│                                          │ │      turn1   turn2  ckpt  turn3            │ │
│                                          │ └───────────────────────────────────────────┘ │
├──────────────────────────────────────────┴───────────────────────────────────────────────┤
│ ┌ decision bar (only when an effect from this session waits on you) ──────────────────┐  │
│ │ ◐ git.push han/41-lars-coder → origin   digest 9f3c…  [Review & approve] [Cancel]    │  │
│ └──────────────────────────────────────────────────────────────────────────────────────┘  │
│ ┌ steering input ──────────────────────────────────────────────────────────────────────┐ │
│ │ Instruction to your agent lars-coder                                                  │ │
│ │ > _                                                                     [Send ⌘↵]     │ │
│ └──────────────────────────────────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────────────────────────────┘
```

Regions:

1. **Header** (fixed). The same facts as the board row (board.md §3), larger, plus:
   - the sandbox backend and fidelity;
   - `Stop` (only for the local user's own agents);
   - an overflow menu: Copy session id, Copy issue reference, Open in new window, Show raw status.

   "← Board" returns to the board with the session focused.
2. **Stream column** (left, at least 420 px). It has a mode switch (Conversation | Steps), the Record tab, and the stream list.
3. **Changes column** (right). The changed-files tree and the diff (diffs.md), and the timeline scrubber under them (§5). The divider between the columns can be dragged; its position is remembered per user. Double-clicking the divider resets it to 50/50.
4. **Decision bar** (conditional), pinned above the steering input (§6).
5. **Steering input** (bottom; §7). Present only with the steer right. Without it, a one-line notice takes its place (§7.1).

Narrow layout (below 1,280 px): the Stream and Changes columns become two tabs, "Stream" and "Changes", under the header. `c` switches between them. The decision bar and the steering input stay at the bottom in both tabs.

---

## 3. The stream

### 3.1 Item kinds and how each renders

| Item | Renders as |
|---|---|
| agent message | the agent's name, then the text as **safe markdown**: headings, emphasis, lists, code spans and blocks, links shown as text with their full URL (SR-97). Raw HTML renders as literal text. Text grows in place while it streams; a caret mark shows at its end until the item completes. |
| reasoning | a folded row, "thinking · 12s", in a quieter tone. Expanding it shows the text. Folded by default. |
| plan | a checklist of entries with done marks. When a later plan replaces it, the new plan renders in place of the old one and the old one is folded into a "plan updated" row. |
| tool call | a **tool row** (§3.2) |
| file change | joined to the tool row that caused it, when the node gives the tool call id; otherwise its own row, "changed src/x.rs +3 −1" |
| decision request | an inline marker row, "waiting for approval: git.push", that links to the decision bar and the inbox. The card itself is never repeated in the stream. |
| steering input | a row showing the author and the text, with its delivery state (§7.3). Others' inputs show "from @ada" and the receipt time. |
| turn start | a divider: "turn 3 · from your task", "turn 3 · steered by @ada", or "turn 3 · strict (outside instruction)" when the host ran it in the stricter mode (R-24) |
| turn end | closes the divider; it adds the turn's duration and token count when known |
| status item | not shown in the stream: it updates the header. The overflow menu's "Show raw status" opens the latest one as formatted JSON, read-only. |
| unknown kind | a quiet row, "unsupported item (kind)", with the item's JSON available on expand. It is never dropped silently. |

- **SES-1.** All agent-written and other-person text is rendered as text, with no HTML passthrough, and bidirectional override characters are made visible (SR-97). Links show their full URL. Activating a link opens a confirm popover that shows the full URL, with "Open in browser" and "Copy".
- **SES-2.** Code blocks are highlighted off the main thread. A code block over 400 lines renders its first 40 lines and a "Show all 1,234 lines" button.
- **SES-3.** Secret-masked spans (masked by the host before anything leaves the device, PLAN §6.3) render as a masked chip, "•••• masked", with the tooltip "Masked by the agent's host". The GUI never tries to unmask them.

### 3.2 Tool rows

A tool row is one line:

```
[kind mark] [verb + subject]                              [+a −d] [duration] [status mark]
 ◐          ran `cargo test board`                                 4s        …running
 ✓          edited src/board.rs                           +14 −2   0.3s      ✓
 ✕          fetched https://docs.rs/…                              2s        ✕ failed
```

- **Verb by kind:** read → "read", edit → "edited", delete → "deleted", move → "moved", search → "searched", execute → "ran", think → "thought", fetch → "fetched", other → "used". The subject comes from the tool call's `title`. When the title is missing, it comes from the first location's path, or from the command text for `execute`.
- **Status marks:** pending → hollow dot, "queued"; in_progress → spinner (a static half-dot under reduced motion), "running"; completed → check; failed → cross in the danger tone, followed by "failed".
- **Line counts:** an edit row shows "+a −d", added and removed lines, when its content has a diff block.
- **SES-4.** Activating a tool row (click, Enter, or `e` when focused) expands its details in place:
  - **edit:** the diff blocks, compact, at most 30 lines, with "Open in Changes" (it selects the file and moves the scrubber to this step, §5.3);
  - **execute:** the command in full, then its output, in monospace, at most 400 lines shown with "Show all";
  - **read** and **search:** the locations, as a list of paths with lines;
  - **fetch:** the full URL, then the response summary if present;
  - **other:** the content blocks.

  Expanding keeps the row's top on screen still (the scroll is anchored on the row).
- **SES-5.** A failed tool row is expanded by default, unless it is in a group (§3.3), and a failed group shows "1 failed" in the danger tone.

### 3.3 Grouping consecutive tool calls

- **SES-6.** Three or more consecutive tool calls inside one turn, with no message between them, are folded into one **group row**. A group row summarises by kind, in the order the kinds first appear, for example "read · 6 files, searched · 2 times, ran · 1 command". It also shows the group's total duration and a status mark:
  - running, if any member is running;
  - failed, if any member failed;
  - otherwise completed.
- **SES-7.** While a group's last member is running, the group row's summary ends with that member's live line ("… running `cargo test`").
- **SES-8.** Expanding a group lists its members as tool rows, indented one step. Members expand individually.
- **SES-9.** Groups never cross a turn boundary or a message.

### 3.4 Conversation mode and Steps mode

- **Conversation** shows every item kind.
- **Steps** shows only:
  - tool rows and groups;
  - plan items;
  - file changes;
  - decision markers;
  - turn dividers.

  Each agent message collapses to a one-line "said: <first line>" row that expands on activation. Steps is the default when the session is not one of the local user's own (the reader usually wants what changed, not the chat).
- `s` toggles between Conversation and Steps (when-clause `focus.session && !focus.textInput`). The choice is remembered per user (not per session).

### 3.5 Following the live end

- **SES-10.** When the stream's scroll is at the bottom (within 48 px), new items keep it pinned to the bottom.
- **SES-11.** When the user has scrolled up, new items do not move the viewport. A floating button reads "↓ 12 new". It and `End` return to the bottom and re-pin.
- **SES-12.** The stream is a virtualized list with variable row heights. Item heights are measured once rendered, and the scroll position is anchored on the topmost visible item, so items that load above do not shift what the user is reading.
- **SES-13.** The Session view is windowed (data.md §4.2). Opening the pane loads the latest window, the last 200 items. Scrolling within 2 viewports of the top loads the previous window. A "Beginning of session" divider shows once the first item is loaded.

### 3.6 Rights: watch only

- **SES-14.** With watch but not read, the node returns output, status and profile items only (P-15). The pane then shows those, with a notice at the top of the stream: "You can watch this session but not read its full context. Ask an owner to change the space's session rights." The Steps mode, the tool details and the Changes column are shown only as far as the returned items allow. Each missing part shows the same one-line reason, never an error.
- **SES-15.** With neither right (the session is dead, or rights changed), the pane shows "This session is closed or no longer visible to you." with "← Board".

---

## 4. The Record tab

The Record lists, oldest first, every log entry that names this session or its task. For each entry:
- the kind in words ("opened the session (guarded)", "claimed task #41", "proposed git.push", "@lars approved git.push", "executor @git: succeeded", "@ada steered", "closed the session");
- the author;
- the receipt's sequence number and time;
- a check mark: signature verified by the node.

Activating an entry expands its body and content as formatted JSON. An effect entry has a button to open it in the inbox.

- **SES-16.** The Record comes from the log through the node's Session view (data.md §4.2) and MUST be visually distinct from the stream. Its heading reads "Record · signed", and a tooltip explains: "Signed entries in the space's log. The stream above is what the agent's host reports and is not authoritative."
- **SES-17.** A redacted entry shows "content redacted" and its redact reason, if any.

---

## 5. Changes and the timeline scrubber

The Changes column shows the session's work timeline: the difference between the session's fork version (its base commit) and a chosen point, live by default (`han.timeline/1` TL-88). File tree and diff rendering are specified in diffs.md. This section covers the scrubber and how it drives them.

### 5.1 The scrubber

```
 base ●──●──●──┬──●──●──◆──●──●──┬──●──●──●───────● live ▶     [Back to live]
          turn 1     turn 2  ◆ "tests pass"  turn 3
          ▲ from                              ▲ to (live)
 Showing: base → live · 4 files +61 −12                 (when scrubbed: "at step 18 · 12:04:31")
```

- The track is ordinal: one tick per **point**, not wall time. A point is a recorded batch of timeline ops, which is normally one tool call or one short quiet period (TL-85). Wall time appears only in labels and tooltips.
- Turn boundaries are taller ticks, labelled "turn N". Checkpoints (TL-25) are diamonds with their label. A merge into main, or an accepted-work event, is a ringed tick. When there are more than 2 ticks per 4 px, adjacent ticks merge into a band whose tooltip gives the count.
- There are two handles. **From** defaults to base; **to** defaults to live. The diff shows the change from **from** to **to**.
- Hovering a tick shows a tooltip: the step (tool call title), the time, the author, and the files touched.

### 5.2 Live and pinned

- **SES-18.** While **to** is at live, the diff updates as new points arrive, and the end marker reads "live ▶" with the active tone.
- **SES-19.** When **to** is moved off live, the diff freezes at that point. A banner over the diff reads "Viewing the state at step 18 (edited src/board.rs) · 12:04:31", and the button "Back to live" appears. New points still extend the track, and a counter "+7 since" grows beside the button.
- **SES-20.** "Back to live", `End` on the scrubber, or `l` in the session pane returns **to** to live.

### 5.3 Linking steps and changes

- **SES-21.** "Open in Changes" on an edit tool row sets **from** to the point before that tool call's batch and **to** to its batch, then selects the edited file. The diff shows exactly that step's change.
- **SES-22.** In the diff, hovering a hunk shows which step last changed it (from the batch's `step`, TL-91), when the node provides it. Activating "Go to step" scrolls the stream to that tool row and focuses it.

### 5.4 Scrubber keyboard

The scrubber is a group of two ARIA `slider`s. Their `aria-valuetext` is "step 18 of 42, turn 3, 12:04:31". These keys apply when a handle has focus:

| Key | Does |
|---|---|
| `←` / `→` | move one point |
| `shift+←` / `shift+→` | move to the previous or next turn boundary |
| `,` / `.` | move to the previous or next checkpoint |
| `Home` | move to base (the **from** handle's minimum) |
| `End` | move to live (the **to** handle's maximum) |
| `PageUp` / `PageDown` | move 10 points |

The handles can't cross. Moving **from** past **to** stops at **to**.

### 5.5 Data

The scrubber's points come from the Timeline query's index (data.md §4.3; the query seam exists since PR #35, but the index's contents are still open, Q-SES-1), and the diff's content from the ChangeSet and Patches queries between **from** and **to** (data.md §4.3). When han-timeline lags and the node falls back to git hunks (PLAN §6.5), the column shows a quiet chip, "from git, ranges only". The scrubber is then disabled with the tooltip "Step history is not available for this session yet". The diff shows the base-to-worktree change.

---

## 6. The decision bar

- **SES-23.** When any open effect proposed by this session waits on the local user (as a rule approver or a guard), the decision bar shows the oldest one, in the compact form of the approval card (approvals.md §3.2):
  - the action;
  - the target;
  - the short digest;
  - "Review & approve", which opens the full card in a dialog, because signing needs the full plan on screen (P-08);
  - "Cancel effect", only when the local user may cancel.

  When there are more, it shows "+2 more", which links to the inbox filtered to this session.
- **SES-24.** The decision bar never signs anything in one key press. Its keyboard path is: `shift+a` (when-clause `focus.session && session.hasDecision && !focus.textInput`; `focus.session` is defined in shell.md §4.2) opens the review dialog; from there, the approvals.md flow applies.
- **SES-25.** Effects of this session that wait on someone else show in the header as "waiting on @ana (git.push)", not in the bar.

---

## 7. Steering input

### 7.1 Who sees it and what it says

The input is present when the local user holds the steer right. Above the field, a one-line label states what sending will mean. It is derived from facts the node provides (data.md §4.2):

| Situation | Label |
|---|---|
| the local user operates the agent (its guard) | "Instruction to your agent lars-coder" |
| someone else's agent, not guarded | "Steering @ana's agent fixer. It acts on this as an outside instruction." |
| someone else's agent, guarded | "Steering @ana's agent fixer. It's guarded: anything this causes waits for @ana's approval." |
| a human's session | "Message to @ana's session" |
| the local user is limited (P-15) and can't steer others | the input is replaced by "You can't steer other sessions while you are limited in this space." |
| no steer right | the input is replaced by "You can watch this session but not steer it." |

- **SES-26.** The label MUST be shown before the user can send, and its wording MUST NOT suggest that an outside instruction will be obeyed.

### 7.2 Editing and sending

- The field is plain multi-line text with no rich formatting. It grows to 8 lines, then scrolls.
- By default `Enter` sends and `shift+Enter` inserts a newline. The setting "Send with mod+Enter" swaps them. `mod+Enter` always sends.
- `Escape` returns focus to the stream. It does not clear the draft.
- **SES-27.** The draft is kept per session for the life of the app, so leaving and returning to the pane restores it (data.md §8). It is cleared once the input is recorded.
- **SES-28.** Sending calls `SessionInput{session, text}` (data.md §6). The send button and Enter are disabled while the call runs. There is no double send.
- Text over 8,000 characters is refused before sending, with "Steering input is limited to 8,000 characters", unless the node reports a different limit (Q-SES-3).

### 7.3 Delivery states

A sent input appears at once in the stream, as an optimistic row in the "sending" state. Its states:

| State | Shown as | When |
|---|---|---|
| sending | quiet text, small spinner | the call is in flight |
| queued | "queued: the space's home is unreachable" | the node accepted it into its outbox but has no receipt yet |
| recorded | "recorded · seq 1234" | the call returned a receipt |
| delivered | "seen by the agent" | the host reported a turn start driven by this input (the stream item names it) |
| refused | in the danger tone: "not sent: <reason>" plus "Edit and resend" | the call failed: `denied` (rights changed), `invalid`, or any other error |

- **SES-29.** A refused input puts its text back into the field, unless the field has a new draft, in which case the text is offered with "Restore draft".

### 7.4 Stop

- **SES-30.** `Stop` (shown only for the local user's own agents) asks for confirmation in a small dialog: "Stop lars-coder's session? The agent stops after its current step. Its work timeline is kept." Its buttons are `Stop session` (danger) and `Keep running`, with focus on the latter. Confirming calls `AgentStop`. The header shows "stopping…" until the view reports the session closed.

---

## 8. Surface states

| State | Appearance |
|---|---|
| **Loading** | the header renders from the board row if it is cached, otherwise as a skeleton. The stream shows 6 skeleton lines and the Changes column a skeleton tree. After 3 s: "Waiting for the node…". |
| **Empty** | a just-opened session with no items: "Waiting for lars-coder's first step…" in the stream; "No changes yet" in Changes. The scrubber shows only the base tick. |
| **Live** | as specified |
| **Stale** | the session's display state is `stale`. A banner under the header reads "No update from this session for 14m. Its host may be busy or offline." The stream and Changes stay readable. Spinners in running tool rows stop and become a quiet "no update". |
| **Failed** | a banner in the danger tone: "This session failed: host unreachable since 12:10." The input stays, because steering may still be recorded for when the host returns; its label adds "The host is unreachable; your input will be delivered when it returns." |
| **Closed** | the session closed. "Session closed at 12:40 by lars-coder" replaces the input; everything else stays readable (subject to rights). |
| **Resyncing** | a thin bar under the header, as on the board (board.md §8) |
| **Error** | per view: the stream, the Changes column and the Record each have their own error panel with Retry. One failing does not blank the others. `not_found` reads "This session doesn't exist in this space." |

---

## 9. Accessibility

- **SES-31.** The stream is an ARIA `log` region with `aria-live="off"`, so streaming text is not read out character by character. A separate polite live region in the pane announces only:
  - a turn's end ("turn 3 ended");
  - a tool failure ("tool failed: ran cargo test");
  - a new decision waiting on the user;
  - a delivery state change of the user's own input (recorded, refused).

  It does so at most once every 2 s, coalescing the rest.
- **SES-32.** Each tool row and group row is a button with `aria-expanded`. Its accessible name is the full line, for example "edited src/board.rs, 14 added, 2 removed, completed in 0.3 seconds".
- **SES-33.** Keyboard navigation inside the stream: `j`/`k` move between items (when-clause `focus.stream && !focus.textInput`); `[`/`]` jump between turn dividers; `e` expands or collapses; `shift+e` expands or collapses every row of the current turn; `i` focuses the steering input. Focus is tracked by item id across updates.
- **SES-34.** The column divider is a focusable `separator` with `aria-valuenow`, resizable with ←/→ in 5% steps.
- **SES-35.** Reduced motion: spinners become static marks, and new items appear without sliding.

---

## 10. Acceptance criteria

| ID | Given | When | Then |
|---|---|---|---|
| AC-SES-01 | fixture `session-live` | the pane opens | the header shows person, agent, harness, issue, state and step; the stream shows the fixture's items in order; the Changes column lists the fixture's files · *pending node API (Q-DAT-9)* |
| AC-SES-02 | `session-live` | the mock streams 20 text deltas into one agent message | exactly one message element exists for that item id, and its final text equals the concatenation · *pending node API (Q-DAT-9)* |
| AC-SES-03 | `session-live` | a tool call goes from pending to in_progress to completed | one row exists for it throughout; its status mark's accessible label goes through queued, running, completed · *pending node API (Q-DAT-9)* |
| AC-SES-04 | `session-live` | 4 consecutive tool calls (3 reads, 1 execute) arrive in one turn | one group row summarises them as "read · 3 files, ran · 1 command"; expanding it shows 4 member rows · *pending node API (Q-DAT-9)* |
| AC-SES-05 | `session-live` | a tool call in a group fails | the group row shows "1 failed" in the danger tone; the member is marked failed · *pending node API (Q-DAT-9)* |
| AC-SES-06 | `session-live` | the user presses `s` | the pane is in Steps mode: messages fold to "said:" rows, and tool, plan and file rows stay; pressing `s` again restores them; the choice persists after reload · *pending node API (Q-DAT-9)* |
| AC-SES-07 | `session-live`, scrolled to the bottom | 10 items arrive | the view stays pinned at the bottom · *pending node API (Q-DAT-9)* |
| AC-SES-08 | `session-live`, scrolled up | 10 items arrive | the top visible item stays in place (±1 px); a "↓ 10 new" button appears; activating it scrolls to the bottom · *pending node API (Q-DAT-9)* |
| AC-SES-09 | `session-long` (3,000 items) | the pane opens and the user scrolls to the top | windows load in pages; "Beginning of session" appears; the DOM never holds more than 300 item elements · *pending node API (Q-DAT-9)* |
| AC-SES-10 | `session-live` | an agent message contains `<img src=x onerror=alert(1)>` and a markdown link | the text renders literally with no `img` element; the link shows its full URL; activating it opens a confirm popover, not a browser · *pending node API (Q-DAT-9)* |
| AC-SES-11 | `session-watch-only` | the pane opens | the watch-only notice shows; there is no steering input; the missing parts show the one-line reason, not an error · *pending node API (Q-DAT-9)* |
| AC-SES-12 | `session-live`, local user is the operator | the pane opens | the input label reads "Instruction to your agent …" · *pending node API (Q-SES-5)* |
| AC-SES-13 | `session-others-guarded` | the pane opens | the input label says the agent is guarded and names the guard · *pending node API (Q-SES-5)* |
| AC-SES-14 | `session-live` | the user types text and presses Enter | the mock receives exactly one `SessionInput` with that text; an optimistic row shows "sending", then "recorded · seq N" when the call returns, then "seen by the agent" when the mock streams the driven turn start · *pending node API (Q-DAT-9)* |
| AC-SES-15 | `errors`: SessionInput returns `denied` | the user sends | the row shows "not sent" with the reason; the text returns to the field |
| AC-SES-16 | `session-live` | the user types a draft, goes to the board and comes back | the draft is still in the field |
| AC-SES-17 | `session-live`, outbox mode | the node reports the input queued | the row shows "queued: the space's home is unreachable" |
| AC-SES-18 | `session-timeline` (42 points, 3 turns, 1 checkpoint) | the user focuses the **to** handle and presses `shift+←` | **to** moves to the previous turn boundary; the diff shows the change from base to that point; the banner and "Back to live" appear · *pending node API (Q-SES-1)* |
| AC-SES-19 | `session-timeline`, scrubbed | the mock adds 5 points | the diff does not change; the counter reads "+5 since"; `l` returns to live, and the diff then includes them · *pending node API (Q-SES-1)* |
| AC-SES-20 | `session-timeline` | the user activates "Open in Changes" on an edit row | the handles bracket that step; the edited file is selected; the diff shows only that step's change · *pending node API (Q-SES-1)* |
| AC-SES-21 | `session-git-fallback` | the pane opens | the chip "from git, ranges only" shows; the scrubber is disabled with its tooltip · *pending node API (Q-SES-1)* |
| AC-SES-22 | `session-decision` (an effect waits on the local user) | the pane opens | the decision bar shows the action, target and short digest; there is no control that signs on one key press; `shift+a` opens the review dialog · *pending node API (Q-DAT-9)* |
| AC-SES-23 | `session-live`, own agent | the user activates Stop and confirms | the mock receives `AgentStop`; the header shows "stopping…" until the mock closes the session; then the closed notice replaces the input · *pending node API (Q-DAT-9)* |
| AC-SES-24 | `session-live` | the user presses Stop, then Escape | no `AgentStop` is sent; focus returns to the Stop button |
| AC-SES-25 | `session-stale` | the pane opens | the stale banner shows; running tool rows show "no update" instead of a spinner · *pending node API (Q-DAT-9)* |
| AC-SES-26 | `errors`: the Timeline view fails with `unavailable` | the pane opens | the Changes column shows its error panel with Retry; the stream and the input still work · *pending node API (Q-DAT-8)* |
| AC-SES-27 | `session-live` | a tool fails and a turn ends within 1 s | the polite region announces one coalesced message, not two within 2 s · *pending node API (Q-DAT-9)* |
| AC-SES-28 | `session-live` | the user presses `j`, `j`, then `e` | the second item expands; its row has `aria-expanded="true"` · *pending node API (Q-DAT-9)* |
| AC-SES-29 | `session-record` | the user opens Record | entries are listed oldest first with author, sequence number and verified mark; the heading reads "Record · signed" · *pending node API (Q-DAT-3)* |
| AC-SES-30 | any session fixture | axe-core runs in both themes, wide and narrow | no WCAG 2.2 AA violations |
| AC-SES-31 | `session-live`, window width 1,000 px | the pane opens | Stream and Changes are tabs; `c` switches between them; the input stays visible in both |
| AC-SES-32 | `session-live` | a stream item has an unknown kind | an "unsupported item" row appears, and its JSON is shown on expand · *pending node API (Q-DAT-9)* |

---

## 11. Open questions

- **Q-SES-1.** *Partly resolved by PR #35:* the `Timeline` query seam exists. Still open: whether its snapshot carries the scrubber's index of points (version id, step or tool call id, turn number, checkpoint label, time and author, in order), and how that index is windowed for long sessions (Q-DAT-7).
- **Q-SES-2.** *Resolved by PR #35:* the ChangeSet and Patches query seams give a change set and per-file patches between two points. Their exact fields are still open under Q-DIF-1.
- **Q-SES-3.** What is the limit on `session.input` text? It is bounded by the protocol's header and content limits. The node should report it, so that the GUI doesn't hard-code 8,000.
- **Q-SES-4.** The "delivered" state needs the stream's turn-start item to name the `session.input` entry that drove it. This is part of the typed Session item vocabulary, which is still open (Q-DAT-9).
- **Q-SES-5.** *Partly resolved by PR #35:* the node provides `can_steer`. Still open: the facts for the label's wording, namely whether the local user is the agent's guard, whether the agent is guarded (`guardStatus`), and why the local user may be limited. They should be node facts too, so that faces don't re-derive Policy.
- **Q-SES-6.** Should a human be able to see an agent's input before it becomes a turn (architecture.md §8.4 "MAY first show the input to the agent's human")? If so, the operator needs a "hold for review" queue, which is not specified here.

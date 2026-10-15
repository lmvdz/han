# UI spec: the live board

Status: draft, Team A (clean room, PLAN.md §8). Behaviour and appearance only. Implement from this text, [data.md](data.md) and [shell.md](shell.md); nothing else is needed.

The board is the GUI's home screen. It answers PLAN §1's question, "what is everyone working on?", without anyone asking. It shows one row per live session in the current space: person × agent × issue. It is a projection of the node API's `view(Board{space, filter, group})`, and it never stores a display truth of its own.

Keywords MUST, SHOULD and MAY are as in RFC 2119.

---

## 1. Terms

| Term | Meaning on this surface |
|---|---|
| **Row** | one live session of the space: its latest `han.status/1` item plus the facts the node joins to it (person, overlaps, pending approvals, sandbox). |
| **Person** | the human a row belongs to: the agent's confirmed operator, or the session's actor when a human runs the session. |
| **Display state** | the row's state as the node derives it: one of `working`, `waiting-approval`, `blocked`, `idle`, `done`, `stale`, `failed`. The GUI shows it and never recomputes it (§6.2). |
| **Needs me** | a row flag the node derives for the local user: the session, or one of its effects, is the target of an item of the local user's attention queue that is neither acknowledged nor muted. Who must act on what is defined by roles (operator, delegator, approver, guard…) in [attention.md](attention.md) §2. Overlaps count only at the conflict tier (ATT-R3). |
| **Done-unseen** | a session of the local user's work that finished after the local user last opened it (attention.md §6.2). |
| **Lifecycle group** | needs-me, to-decide, landing, working or idle, derived by the node for the local user (attention.md §5). |
| **Overlap** | two or more sessions working on the same thing, at one of three tiers (PLAN §6.6): **claim** (same issue, or an advisory path lease), **hunk** (touched line ranges intersect), **conflict** (a simulated merge of the two session branches conflicts). |
| **Group** | a band of rows that share a key: issue, person, or display state. |

---

## 2. Layout

```
┌──────────────────────────────────────────────────────────────────────────────────────────┐
│ shell top bar (shell.md)                                                                 │
├────┬─────────────────────────────────────────────────────────────────────────────────────┤
│    │ ┌ toolbar ─────────────────────────────────────────────────────────────────────────┐│
│ n  │ │ Board · team-han        Group: [Issue ▾]   [⌕ filter: person:@ada path:src/** ] ││
│ a  │ │ 6 working · 1 waiting · 1 blocked · 1 stale · 0 failed      [Needs me 2] [Freeze]││
│ v  │ └──────────────────────────────────────────────────────────────────────────────────┘│
│    │ ┌ column header (sticky) ──────────────────────────────────────────────────────────┐│
│ r  │ │ Who               │ Work                         │ Changes        │ Health        ││
│ a  │ ├───────────────────┴──────────────────────────────┴────────────────┴───────────────┤│
│ i  │ │ ▾ github:lmvdz/han#41  "Board freshness"                    3 sessions  ⚠ overlap ││
│ l  │ │ ● @lars · lars-coder · pi │ #41 working: running tests │ 4 files ⚑hunk │ 12/0 ✓ 41k $0.38 20s ││
│    │ │ ◐ @ada · ada-bot · claude │ #41 waiting-approval: push │ 2 files ⚑hunk │  –   18k $0.12 1m  ││
│    │ │ ◌ @ada · ada-ui · pi      │ #41 stale: editing board   │ 1 file         │  –    6k $0.03 14m ││
│    │ │ ▸ github:lmvdz/han#52  "Invite tickets"                     2 sessions             ││
│    │ │ ▾ No issue                                                  1 session              ││
│    │ │ ✕ @bo · bo-runner · pi    │ failed: host unreachable   │ 0 files        │  –    –    34m ││
│    │ │ ···  (placeholder rows while the window loads)                                     ││
│    │ └──────────────────────────────────────────────────────────────────────────────────┘│
│    │ ┌ peek panel (optional, right side, toggled) ┐                                       │
├────┴─────────────────────────────────────────────────────────────────────────────────────┤
│ shell status bar (shell.md)                                                              │
└──────────────────────────────────────────────────────────────────────────────────────────┘
```

Regions, top to bottom:

1. **Toolbar.**
   - Title: "Board" and the space's name.
   - Group selector: Issue, Person, State, Lifecycle (§4.7).
   - Filter box (§5).
   - The **state strip**: one count per display state, in the order of §4.3. Each count is a button that toggles a filter on that state.
   - The **Needs me** toggle, which shows how many rows need the local user.
   - The **Freeze** toggle (§7.4).
2. **Column header.** It stays at the top while the list scrolls. There are four column groups (§3). A column group can be resized by dragging its right edge; widths are remembered per user (data.md §8).
3. **List.** A windowed list of group headers and rows (§6).
4. **Peek panel** (optional). A right-hand panel, at most 40% of the width, that previews the focused row without leaving the board (§7.3). On a window narrower than 960 px it opens as an overlay instead.

At widths below 760 px, each row wraps onto two lines: Who and Work on the first, Changes and Health on the second. No columns are dropped.

---

## 3. A row: what each element shows, and where it comes from

All fields come from the Board view row (data.md §4.1). The `han.status/1` fields are named as in `spec/profiles/han.status-1.md`.

| Column group | Element | Shows | Source |
|---|---|---|---|
| **Who** | state glyph | one fixed slot, never more than one glyph (§4.1) | the row's display state |
| | person | the person's handle, for example `@lars`. A human session shows the person only. | the agent's profile `operator`, or the session actor |
| | agent | the agent's short name (`lars-coder`). A human session shows "–". | the status item's `agent` |
| | harness | free text, for example `pi`, `claude-code` | `harness` |
| | sandbox mark | shown only when the gate is **advisory**: a small open-lock mark with the text "advisory" (§4.4) | the row's `sandbox` facts (data.md §4.1) |
| **Work** | issue | the issue as `#41` when its repo is the space's only repo, otherwise `owner/repo#41`. When the row is grouped by issue, the issue is left out of the row. | `issue` |
| | state and step | the display state as a word, then `step` (for example "working: running tests"). When `tool` is present it follows the step in a quieter tone. | `state`, `step`, `tool` |
| | plan progress | when `plan` exists: "3/7", the number of done entries over all entries | `plan` |
| **Changes** | files | "4 files", or "1 file", or "0 files" | `files` length |
| | hunk count | in the tooltip and the accessible name: "4 files, 9 changed ranges" | the sum of `hunks` lengths |
| | overlap badge | §4.2 | the row's `overlaps` |
| **Health** | tests | "12/0", passes over failures, with a check mark when there are no failures and a cross otherwise. "–" when absent. | `tests` |
| | tokens | the input plus output tokens, compacted ("41k", "1.2M") | `tokens.in + tokens.out` |
| | cost | in the space budget's unit, formatted for that unit (for `USD-cent`, "$0.38"). "–" when absent. | `tokens.cost` |
| | age | the time since the node last received an item for this session, as "20s", "4m", "2h", "3d". It is updated once a second while the board is visible. | the row's `receivedAt` (node clock) and the shared clock atom (data.md §7) |

Rules:

- **BRD-1.** Each compacted value (tokens, cost, age) MUST have a full form in its accessible name and tooltip, for example "41,212 tokens (38,004 in, 3,208 out)", "cost 38 cents", "last activity 20 seconds ago". The compact text is hidden from assistive technology, so it is not read twice.
- **BRD-2.** The age MUST come from the node's receive time, never from the status item's `at` (the host's clock, which may be skewed). The row's tooltip MAY show `at` labelled "host time".
- **BRD-3.** Text that agents or other people wrote (`step`, `tool`, plan entries, `summary`, issue titles) MUST render as plain text: no markup, no links that are made clickable automatically, and bidirectional override characters shown as visible escapes (SR-97).
- **BRD-4.** Long text is truncated with an ellipsis on one line, and the full text is in the tooltip. Rows never grow because their text is long.
- **BRD-5.** `summary` is not shown in the row. It shows in the peek panel.

### 3.1 Group header rows

A group header shows:
- a disclosure triangle (▾ open, ▸ closed);
- the group key: the issue with its title (when the node has the title), the person's handle with their display name, or the state's word;
- the number of sessions in the group;
- the highest overlap tier among its rows, if any;
- how many of its rows need the local user, if any.

Group headers carry no Health columns.

Groups without a key gather under "No issue" (issue grouping) or "Unassigned" (person grouping; this can only happen for a session whose operator is unconfirmed, and the header says so in its tooltip). These groups always come last.

---

## 4. Visual states of a row

### 4.1 The state glyph

Each display state has one glyph, one colour role (shell.md §6) and one word. Colour is never the only signal: the glyph's shape and the word carry the meaning on their own.

| Display state | Glyph | Colour role | Word | Extra treatment |
|---|---|---|---|---|
| `working` | filled dot that pulses slowly | `tone-active` | working | the pulse stops under reduced motion |
| `waiting-approval` | half-filled dot | `tone-attention` | waiting-approval | if it waits on the local user, the row has the needs-me edge (§4.5) |
| `blocked` | dot with a bar | `tone-attention` | blocked | |
| `idle` | hollow dot | `tone-quiet` | idle | |
| `done` | check | `tone-success` | done | the row's text is quieter |
| `stale` | dotted ring | `tone-quiet` | stale | §4.3 |
| `failed` | cross | `tone-danger` | failed | §4.3 |

### 4.2 The overlap badge

- **BRD-6.** A row with overlaps shows **one** badge in the Changes column: a flag mark plus the word of the **highest** tier it is in ("claim" < "hunk" < "conflict"), plus, when it overlaps more than one other session, "×N".
- **BRD-7.** Tier colours: claim uses `tone-quiet`, hunk uses `tone-attention`, conflict uses `tone-danger`.
- **BRD-8.** The badge is a button. Activating it (click, or `o` on the focused row) opens the overlap detail in the peek panel: one entry per other session, with:
  - its person, agent and issue;
  - the tier;
  - for hunk and conflict, the shared files and line ranges;
  - a button that opens the pairwise diff view (diffs.md §7).
- **BRD-9.** The accessible name of the badge is complete, for example "Overlap: hunk tier, with 2 sessions: @ada ada-bot on #41, @bo bo-runner on #52".
- **BRD-10.** When an overlap first appears on a row, the badge does one brief highlight, under 600 ms (none under reduced motion), and the board's polite live region announces "New overlap: lars-coder and ada-bot, hunk tier, on src/board.rs". Overlaps that are already there when the board opens are not announced.

### 4.3 Stale and failed

The node derives `stale` and `failed` from time: by default no status item for 10 minutes means stale, and a host unreachable for 30 minutes means failed (PLAN §6.4; configurable per space). The M3 node derives both for every face, so the TUI and the GUI agree (PR #35); where the per-space thresholds are configured is still open (Q-BRD-1).

- **BRD-11.** A **stale** row keeps its last known Work, Changes and Health values, but shows them in a quieter tone (`text-quiet`). After the state word it adds "· last heard 14m ago". The Health age is shown in the attention tone.
- **BRD-12.** A **failed** row shows "failed:" followed by the node's reason, for example "host unreachable" or "turn failed". Its Work and Changes values keep their last values and are quieter, as for stale. Its glyph is the danger cross.
- **BRD-13.** A stale or failed row MUST NOT show a pulse, a spinner or any other sign of liveness.
- **BRD-14.** When a stale row receives a new item, it returns to its new state at once, with no announcement.

### 4.4 Advisory sandbox

- **BRD-15.** A session whose gate is advisory (for example no OS sandbox is available on that machine) MUST show the advisory mark in its Who column on every row and in the peek panel (SR-99). The mark's tooltip and accessible name are "Guarding is advisory on this machine: the session has no OS sandbox." The peek panel also names the sandbox backend, for example "sandbox: bubblewrap (enforced)" or "sandbox: none (advisory)".

### 4.5 Needs me

- **BRD-16.** A needs-me row has a 3 px leading edge in `tone-attention` and the text "awaits you" after its state word. The edge does not animate.
- **BRD-17.** Within a group, needs-me rows sort first (§6.3).

### 4.6 Focus, hover and selection

- One row at a time has **focus** (the keyboard cursor). It has a visible focus ring that is at least 2 px and meets 3:1 contrast against both the row and the page.
- Hover tints the row; hover never moves or resizes anything.
- There is no multi-selection in this version.

### 4.7 Attention: done-unseen, lifecycle, acknowledge

The attention rules of [attention.md](attention.md) apply to the GUI as they do to the TUI. The node computes everything in this section; the GUI shows it (BRD-24).

- **BRD-44.** A done-unseen row shows a "new" mark before its state glyph, with the text "new" in its accessible name. Opening its session pane for 1 s with the window focused marks it seen (ATT-R33). Hover and focus do not.
- **BRD-45.** The **Lifecycle** grouping shows the groups needs-me, to-decide, landing, working and idle, in that order, rolled up into buckets by space, issue or person, with the defaults and bounds of ATT-R22 to ATT-R25: needs-me open in queue order, to-decide and landing open to closed buckets, working and idle closed to per-state counts.
- **BRD-46.** A needs-me row's menu, and the shortcut `shift+a` (when-clause `focus.board && !focus.textInput`), acknowledge its item (ATT-R20). An Undo toast shows for 10 s. Acknowledging an approval is the inbox's Dismiss (ATT-R21).
- **BRD-47.** The shell's top bar shows the attention count and the done-unseen count from the `Attention` view (ATT-R13), with the same words. Activating the count goes to the top item (ATT-R16).

---

## 5. Filters

The filter box takes space-separated terms. Terms with the same key are ORed; terms with different keys are ANDed. Free words match the issue, its title, `step` and the agent name, ignoring case.

| Term | Matches |
|---|---|
| `person:@ada` | rows whose person is `@ada` |
| `agent:ada-bot` | rows of that agent |
| `harness:pi` | rows whose harness is `pi` |
| `repo:owner/repo` | rows whose `repo` is that repository |
| `issue:#41` or `issue:owner/repo#41` | rows on that issue |
| `label:bug` | rows whose issue has that label (needs Q-BRD-3) |
| `path:src/auth/**` | rows with a changed file matching the glob (`*` within one segment, `**` across segments) |
| `state:working` | rows in that display state |
| `overlap:any`, `overlap:hunk`, … | rows with an overlap of at least that tier |
| `is:mine` | rows of the local user's agents and own sessions |
| `is:needs-me` | needs-me rows (the same as the toggle) |

- **BRD-18.** The filter is parsed in the GUI and sent to the node as the Board query's `filter` (data.md §4.1). The node filters; the GUI never filters rows itself, because only the node sees rows outside the window.
- **BRD-19.** A term the GUI can't parse is underlined in the danger tone, with the tooltip "Unknown filter key 'foo'". The rest of the filter still applies.
- **BRD-20.** While the user types, the query updates 250 ms after the last keystroke. Pressing Enter applies it at once.
- **BRD-21.** The state-strip counts and the Needs me toggle are shortcuts that add or remove `state:` and `is:needs-me` terms in the box, so the box always shows the whole filter.
- **BRD-22.** The filter and the grouping are part of the route's search parameters (shell.md §2), so back and forward restore them. The last grouping is also remembered per user and per space (PLAN §1.3) and is used when the board is opened without parameters. The filter is not remembered across restarts.
- **BRD-23.** Typing in the box suggests values for known keys (people, agents, harnesses, repos, issues on the board, states). The suggestion list is a listbox under the box: ↑/↓ move, Tab or Enter accepts, Escape closes it.

---

## 6. Data and windowed scrolling

### 6.1 The query

The board reads `view(Board{space, filter, group, window})` (data.md §4.1). The node sends an ordered list of entries, each a group header or a row, already grouped and sorted, plus the total number of entries and the offset of the first one. The GUI keeps **one** subscription for the board of the current space (PLAN §6.6.1). The query also carries the keys of the groups the user has collapsed (`collapsed`). The node then leaves those groups' rows out and counts only their headers in `total`, so the scrollbar stays true.

### 6.2 Derived, never stored

- **BRD-24.** The GUI MUST NOT compute display state, needs-me, overlap tiers or the group order. It shows what the row says. The TUI and the GUI then always agree.
- **BRD-25.** The GUI MUST NOT show a status item as fact (`han.status/1`). Where a row states something authoritative ("waiting on your approval for git.push"), that statement comes from the log facts the node joins to the row (the effect id and action), never from the agent's `step` text.

### 6.3 Sort order (performed by the node, specified here so that faces agree)

1. Groups:
   - by state, in the fixed order `waiting-approval`, `blocked`, `failed`, `stale`, `working`, `idle`, `done`;
   - by issue or person, the groups with needs-me rows first, then by most recent activity within the group;
   - then the keyless group last.
2. Rows within a group: needs-me first, then most recent activity first, then the session id as the tie-break.

See Q-BRD-2 for this order being normative.

### 6.4 The window

- **BRD-26.** The list is virtualized: only entries in or near the viewport are in the DOM. With 5,000 rows the DOM holds no more than 3 viewport-heights of rows.
- **BRD-27.** The GUI requests a window that covers the viewport plus one viewport-height of overscan above and below, with a limit no smaller than 50. When the viewport scrolls to within half a viewport-height of the window's edge, the GUI moves the window: it replaces the subscription's window (data.md §5.4).
- **BRD-28.** Entries outside the received window but inside the total are **placeholder rows**: the same height as a row, with a quiet shimmer bar (static under reduced motion), and no text. The scrollbar always reflects the total, so dragging it to the end and releasing loads the last window.
- **BRD-29.** While a new window loads, the entries of the old window stay on screen. Nothing blanks out, and the scroll position does not jump.
- **BRD-30.** Row height is fixed for each density (shell.md §6.3), so the scroll position maps to an entry index without measuring.

---

## 7. Interactions

### 7.1 Keyboard

These commands are active when the board list has focus and no text field does (when-clause `focus.board && !focus.textInput`; shell.md §4 lists them all).

| Key | Command | Does |
|---|---|---|
| `j`, `↓` | `board.next` | focus the next entry (row or group header) |
| `k`, `↑` | `board.prev` | focus the previous entry |
| `Home`, `g g` | `board.first` | focus the first entry |
| `End`, `shift+g` | `board.last` | focus the last entry (it loads the last window first) |
| `PageDown`, `PageUp` | `board.pageDown/Up` | move focus by one viewport |
| `Enter` | `board.open` | on a row: open its session pane (session.md). On a group header: toggle the group. |
| `Space` | `board.peek` | toggle the peek panel for the focused row |
| `←`, `h` | `board.collapse` | on a row: focus its group header; on an open header: close it |
| `→`, `l` | `board.expand` | on a closed header: open it; on an open header: focus its first row |
| `o` | `board.overlap` | open the overlap detail of the focused row (when it has one) |
| `a` | `board.approval` | open the approvals inbox at this session's pending effect (when the row waits on the local user) |
| `t` | `board.cycleGroup` | cycle the grouping: issue → person → state |
| `/` | `board.filter` | focus the filter box |
| `x` | `board.clearFilter` | clear the filter (when it is not empty) |
| `m` | `board.needsMe` | toggle the Needs me filter |
| `z` | `board.freeze` | toggle Freeze (§7.4) |
| `n` | `session.new` | open the start-session dialog (§7.5) |
| `Escape` | – | in the filter box: return focus to the list; with the peek panel open: close it |

- **BRD-31.** Focus moves within the list on the roving-tabindex pattern: the list is one Tab stop, and the arrow keys move inside it. Tab leaves the list for the next region (the peek panel when it is open, otherwise the shell).
- **BRD-32.** Focus is tracked by **session id**, not by index. When rows move, the focused session keeps focus.
- **BRD-33.** If the focused session leaves the board (it closes, or a filter removes it), focus moves to the entry now at its old index, or to the last entry, and the live region announces "lars-coder left the board".

### 7.2 Pointer

- A click on a row focuses it and opens its session pane. A middle-click, or mod+click, opens the session pane in a new window (shell.md §2.1).
- A click on the overlap badge opens the overlap detail (BRD-8) and does not open the session.
- A click on a group header toggles the group.
- Right-click opens the row's menu: Open, Peek, Copy session id, Copy issue reference, Open overlap detail, Stop session (only on the local user's own agents; confirmed, session.md §7.4).

### 7.3 Peek panel

The peek panel shows the focused row in more detail: `summary`, `plan` (as a checklist), the files with their range counts, the overlaps, the sandbox, and any pending approvals with a button to open each. It follows focus: moving focus updates it. It has its own Tab stop, and Escape closes it and returns focus to the row.

### 7.4 Live updates without jumping

- **BRD-34.** When an update moves rows, the viewport keeps the focused row at the same vertical position (scroll anchoring). Without a focused row in view, the topmost fully visible row is the anchor.
- **BRD-35.** Rows that change position animate to their new place over `motion-move` (shell.md §6.3). Under reduced motion they move instantly.
- **BRD-36.** **Freeze** holds the current order and membership. Values (step, age, health) keep updating, but no row moves, appears or leaves. A pill under the toolbar reads "Order frozen · 3 changes waiting". Turning Freeze off applies the waiting changes in one step. Freeze turns itself off when the board is left.
- **BRD-37.** A row that appears or leaves while Freeze is off fades in or out over `motion-move` (instantly under reduced motion).

### 7.5 Starting a session

`n`, or the toolbar's "New session" in the overflow menu, opens a dialog that calls `AgentStart{agent, harness, repo, issue?}`. Its fields:
- **Agent:** one of the local user's agents, from SpaceStatus.
- **Harness:** the agent's harnesses, defaulting to the last one used.
- **Repo:** the space's repos.
- **Issue:** optional, with suggestions from the issues on the board and the space's open tasks.

The dialog follows the command lifecycle of data.md §6. Once the call succeeds, the dialog closes, and the new row is focused as soon as it appears in the view. If the row has not appeared after 10 s, a toast says "Session started; waiting for its first status", with a link to the session pane.

---

## 8. Surface states

| State | When | Appearance |
|---|---|---|
| **Loading** | no snapshot yet (first open, or the space has just changed) | the toolbar is drawn with its controls disabled; the list shows 8 placeholder rows; there is no spinner. After 3 s without a snapshot, a line under the toolbar reads "Waiting for the node…". |
| **Empty: no sessions** | a snapshot with 0 entries and an empty filter | centred in the list: "No sessions in this space yet." Under it, two actions: "Start a session" (§7.5) and "Invite someone" (shell.md §7.4). Under them, one sentence in a quiet tone: "Sessions appear here when anyone in the space starts an agent." |
| **Empty: filtered** | 0 entries and the filter is not empty | "No sessions match this filter." and a "Clear filter" button. The state strip still shows the space's counts without the filter, so it is clear rows exist. |
| **Live** | a snapshot has arrived and diffs are flowing | as specified above |
| **Resyncing** | the stream had a gap, or the node asked for a resync (data.md §5.3) | the last data stays on screen; a thin bar under the toolbar shows "Resyncing…". It is announced only if it lasts over 3 s. |
| **Stale view** | the node is cut off from the space's home (SpaceStatus says unreachable) | a banner under the toolbar, in the attention tone: "Offline from the space's home since 12:03. Other members' updates are paused; your own sessions still update." The rows still show live data for local sessions. Other rows age, and the node marks them stale on its schedule. |
| **Error** | the view failed (data.md §5.5) | with data: the last data in the quiet tone, plus an error banner naming the error (data.md §6.3) with a "Retry" button. Without data: an error panel in place of the list, with the same message and button. `denied` reads "You no longer have read access to this space." and offers no retry. |

---

## 9. Accessibility

- **BRD-38.** The list is an ARIA `treegrid`:
  - group headers are rows at `aria-level=1` with `aria-expanded`;
  - session rows are at `aria-level=2`;
  - the four column groups are `gridcell`s with `columnheader`s in the sticky header;
  - `aria-rowcount` is the total and `aria-rowindex` is each rendered row's index in that total, so screen readers report positions correctly under virtualization.
- **BRD-39.** Each session row's accessible name is one sentence, in this order: person, agent, harness, issue, state word with step, files and overlap, tests, cost, age, and any needs-me or advisory flags. For example: "@ada, ada-bot, claude-code, issue 41, waiting-approval: push, awaits you, 2 files, overlap hunk tier with lars-coder, no tests, 18 thousand tokens, 12 cents, 1 minute ago."
- **BRD-40.** The board has one polite live region. It announces only:
  - a row of the local user's that turns `failed`, `blocked` or `waiting-approval`;
  - a new overlap that involves one of the local user's agents;
  - a row leaving (BRD-33).

  Announcements are coalesced: at most one every 2 s, and a burst is summarised ("3 board changes; press `m` to see rows that need you"). Steps and ages are never announced.
- **BRD-41.** Single-key shortcuts (`j`, `k`, `o`, …) can be turned off or remapped in settings (shell.md §4.5, WCAG 2.1.4). The board works with Tab, the arrow keys and Enter alone.
- **BRD-42.** All text meets WCAG 2.2 AA contrast (4.5:1, or 3:1 for large text) in both themes, including the quieter tone used for stale and failed rows.
- **BRD-43.** Hit targets are at least 24×24 px (WCAG 2.5.8). The overlap badge in compact density keeps a 24 px target, even when its drawn mark is smaller.

---

## 10. Acceptance criteria

Fixtures are defined in data.md §9. "Mock" means the GUI runs with the mock client. Tests use accessible roles and names first, and the test ids of data.md §9.4 only where roles don't identify an element.

| ID | Given | When | Then |
|---|---|---|---|
| AC-BRD-01 | fixture `team` | the board opens | a `treegrid` shows 3 issue groups and 7 session rows; each row's accessible name contains person, agent, harness, state and age · *pending node API (Q-DAT-12)* |
| AC-BRD-02 | fixture `team` | the user presses `t` twice | the grouping becomes person, then state; the route's search parameters change to match; the state groups appear in the order `waiting-approval, blocked, failed, stale, working, idle, done` (only the non-empty ones) |
| AC-BRD-03 | fixture `team`, grouping set to person | the app is reloaded and the board opens without parameters | the grouping is person |
| AC-BRD-04 | fixture `team` | the user types `person:@ada state:working` and waits 300 ms | the mock receives exactly one new Board query whose filter has person `@ada` and state `working`; the list shows only what the mock returns · *pending node API (Q-DAT-11)* |
| AC-BRD-05 | fixture `team` | the user types `colour:red` | the term is marked invalid with the tooltip naming the unknown key; no query is sent with the invalid term |
| AC-BRD-06 | fixture `team` | a script changes one row's state to `failed` | that row's glyph has the accessible label "failed"; its text includes the reason; no element in the row has a running animation · *pending node API (Q-DAT-12)* |
| AC-BRD-07 | fixture `stale` | the board opens | the stale row shows "last heard" with an age of at least 10 minutes and carries no animation; its Work text has the quiet-tone token · *pending node API (Q-DAT-12)* |
| AC-BRD-08 | fixture `stale`, virtual clock | the clock advances 60 s | the stale row's age label increases by one minute; the mock receives no new query (ages are computed locally) · *pending node API (Q-DAT-12)* |
| AC-BRD-09 | fixture `overlap` | the board opens | the row overlapping 2 sessions shows exactly one badge whose text includes the highest tier and "×2"; its accessible name lists both other sessions · *pending node API (Q-DAT-19)* |
| AC-BRD-10 | fixture `overlap` | the user focuses that row and presses `o` | the peek panel opens with 2 overlap entries, each with a button to open the pairwise diff · *pending node API (Q-DAT-19)* |
| AC-BRD-11 | fixture `team` | a script adds a new hunk-tier overlap between two of the local user's agents | the live region announces it once; a second identical diff announces nothing · *pending node API (Q-DAT-19)* |
| AC-BRD-12 | fixture `team` | the user presses `j` three times, then Enter | the third row's session pane opens (route `/space/:space/session/:id`) |
| AC-BRD-13 | fixture `team`, focus on row R | a script moves R from position 2 to position 5 | R still has focus, and R's on-screen top differs by no more than 1 px from before |
| AC-BRD-14 | fixture `team`, Freeze on | a script adds a row and reorders two others | no row moves or appears; the pill reads "3 changes waiting" (or the matching count); turning Freeze off applies them |
| AC-BRD-15 | fixture `large` (5,000 rows) | the board opens and the user scrolls to the middle | no more than 3 viewports of row elements are in the DOM; `aria-rowcount` is 5,000 plus the group headers; the mock sees window queries that cover the viewport |
| AC-BRD-16 | fixture `large` | the user drags the scrollbar to the end | placeholders show at once; the last rows replace them when the mock answers; the old window's rows were not cleared before the new data arrived |
| AC-BRD-17 | fixture `empty` | the board opens | "No sessions in this space yet." is shown with the actions "Start a session" and "Invite someone" |
| AC-BRD-18 | fixture `team` | the filter matches nothing | the filtered-empty message and "Clear filter" show; the state strip still shows the unfiltered counts · *pending node API (Q-DAT-10)* |
| AC-BRD-19 | fixture `gap` | the mock sends a diff with a sequence gap | "Resyncing…" appears, the old rows stay, and the mock receives a resubscribe; after the new snapshot the bar disappears · *pending node API (Q-DAT-8)* |
| AC-BRD-20 | fixture `errors`, Board query fails with `unavailable` and no data | the board opens | the error panel shows the `unavailable` message and a Retry button; Retry sends a new subscription · *pending node API (Q-DAT-8)* |
| AC-BRD-21 | fixture `errors`, Board query fails with `denied` | the board opens | the message says read access is gone; there is no Retry button · *pending node API (Q-DAT-8)* |
| AC-BRD-22 | fixture `advisory` | the board opens | the advisory session's row shows the advisory mark with its full accessible name; the peek panel names the sandbox backend |
| AC-BRD-23 | fixture `team` | a status item's `step` contains `<b>x</b>` and a U+202E override character | the row shows the literal characters `<b>x</b>` and a visible escape for U+202E; no `b` element exists in the row |
| AC-BRD-24 | fixture `team`, the setting "single-key shortcuts" off | the user presses `j` | focus does not move; the arrow keys still move it |
| AC-BRD-25 | fixture `team` | a row waits on the local user's approval | it sorts first in its group, shows the needs-me edge and "awaits you", and `a` opens the inbox focused on that effect |
| AC-BRD-26 | fixture `team` | the user opens the start-session dialog, picks an agent and repo, and submits | the mock receives `AgentStart` with those fields; the submit button is disabled while the call runs; when the mock's view adds the row, it receives focus · *pending node API (Q-DAT-15)* |
| AC-BRD-27 | any fixture | axe-core runs on the board in both themes | no violations at WCAG 2.2 AA |
| AC-BRD-28 | fixture `team` | a row's agent writes `step` "approved by @ada" while the log has no approval | no authoritative approval mark appears on the row; the peek panel's list of pending approvals (§7.3) shows the effect as awaiting approval (BRD-25) |
| AC-BRD-29 | fixture `team`, one of the local user's sessions turns `done` | the board renders | its row shows the "new" mark and its accessible name contains "new"; opening its pane for 1 s sends `MarkSeen` · *pending node API (AN-2, AN-4)* |
| AC-BRD-30 | fixture `large`, grouping Lifecycle | the board opens | the groups show in the order needs-me, to-decide, landing, working, idle; working and idle are closed headers with per-state counts from `totals`; no more than 50 entries are rendered before anything is opened · *pending node API (AN-3)* |
| AC-BRD-31 | fixture `team`, a needs-me row | the user presses `shift+a` | the mock receives one `Ack` with the item's version; the row leaves needs-me; Undo within 10 s sends `Unack` · *pending node API (AN-4)* |

---

## 11. Open questions

- **Q-BRD-1.** *Partly resolved (PR #35: the node derives `stale` and `failed`).* Still open: where the per-space thresholds live (policy is authority, so probably a namespaced space setting), and the `han.status/1` text that lets each face choose its own threshold, which should now defer to the node.
- **Q-BRD-2.** Should the sort order of §6.3 be normative across faces (in a future `han.board/1` profile), or only node behaviour?
- **Q-BRD-3.** `label:` filters need issue labels. `han.status/1` has none, and han-github brings issues in as tasks. Does the Board row carry labels, or is `label:` dropped from the MVP?
- **Q-BRD-4.** *Resolved by PR #35:* the Board snapshot carries `total` and `offset`.
- **Q-BRD-5.** *Resolved by PR #35:* the Board query carries `collapsed` (§6.1).
- **Q-BRD-6.** *Resolved by PR #35:* rows carry `sandbox`. Still open: whether `han.status/1` should also gain the member, so that other faces than the node's views see it.
- **Q-BRD-7.** *Answered by attention.md (ATT-R3):* an overlap counts towards needs-me only at the conflict tier. Claim and hunk overlaps stay badges.

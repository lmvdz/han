# UI spec: the TUI session view

Status: draft, Team A (clean room, PLAN.md §8). This spec covers behaviour and appearance only. Read it with [data.md](data.md) §12.1 for the `Session` view, and with [session.md](session.md), the GUI's session pane, whose concepts it shares.

The session view is the Stream tab of `han tui`'s session pane, the pane that Enter opens from a board row. It is a live, readable transcript of what one agent is doing:
- what it says;
- what it thinks, where the reader may see that;
- which tools it calls, and what they return;
- what it changes;
- what it asks to have approved;
- what people told it.

The view is built from typed items, never from terminal bytes. Every part of it is a pure function of three things:
- the `Session` view's three parts, `facts`, `items` and `record`;
- the Approvals view;
- local view state: focus, scroll, what is folded, and the steering draft.

The Timeline and Changes tabs stay as they are and are out of scope here, except where an entry links to them. The frame's attention indicator, the jump picker and notices are in [attention.md](attention.md).

---

## 1. Terms

| Term | Meaning |
|---|---|
| **Item** | one `ItemRow` of `Session{part: items}`: `{key, session, id, seq_in_session, turn?, updated_ms, item}`. A later row with the same `id` replaces the body and keeps the position (data.md §12.1). |
| **Entry** | what the view draws for one item: a **head line**, plus a **body** that may be folded. One item makes at most one entry. Status items make none (§3.9). |
| **Gutter** | the leftmost columns of every transcript line. The TUI owns them. They hold the entry's **kind label** on the head line and a continuation mark on the other lines. Text from items is never drawn in the gutter. |
| **Turn** | the entries between a `turn_start` and its `turn_end`, drawn under a **turn divider**. |
| **Group** | three or more consecutive tool-call entries in one turn, with no message between them, folded into one entry (session.md SES-6, reused). |
| **Following** | the view keeps the newest entry in sight as items arrive. |
| **Focus** | the one entry that keys act on. The view tracks it by item `id`, never by screen row. |
| **Foreign text** | any string that came from an item, a status body, a record summary or another actor. All of it is data (§6). |
| **Facts** | the single `SessionRow` of `Session{part: facts}`: state, `state_reason`, `status`, `rights`, `is_mine`, `closed`, `last_active_ms`. |
| **Record** | the signed rows of `Session{part: record}`. They are authority; the items are a report (README, "Authority versus status"). |

Keywords MUST, SHOULD and MAY are as in RFC 2119. Numbered rules are normative: `TSV-R1`… for rules and `TSV-1`… for acceptance criteria.

---

## 2. Layout

The pane is laid out as **regions, top to bottom**. Each region is one or more whole rows, and all of them span the full width of the pane:

1. **Session header** (1 row, or 2 rows when the pane is at least 14 rows high). These are the facts, in order:
   - the state mark and the state word;
   - agent and person;
   - harness;
   - issue;
   - the current step;
   - a sandbox note when the sandbox is advisory or absent (SR-99);
   - tests, tokens and cost;
   - how long ago the session was last heard from.

   The second row, when there is one, carries the tests, tokens, cost and sandbox note.
2. **Tab line** (1 row). It reads `Stream · Timeline · Changes · Record`, with the current tab marked by more than colour (§8). On the Stream tab it also names the mode: `conversation` or `steps` (§4.6).
3. **Notice band** (0 to 2 rows). It is present only when something limits what the user sees or can do:
   - watch only;
   - stale;
   - failed;
   - closed;
   - resyncing;
   - a view error.

   There is one row per notice, highest priority first, in the order closed > failed > stale > resyncing > watch-only > error. If there are more notices than rows, the band ends with "+N more" (`:notices` lists them all).
4. **Transcript** (fills the remaining space). Entries in item order, oldest at the top.
5. **Activity line** (1 row). Live state, as a report, and the following state (§4.4):
   - "working · ran cargo test · 12s";
   - "waiting for approval";
   - "idle";
   - "8 new below".

   It shows nothing in the closed state, except a following hint.
6. **Decision band** (0 or 1 row). Present only when an effect proposed by this session waits on the local user. It shows the action, the target and the short digest, then "a to review". It is never a control that signs (§5.3).
7. **Reply strip** (0, or 1 to 3 rows). Present only when the session's latest turn ended with suggested replies and the local user may steer (§5.4). It shows the agent's numbered suggestions as the agent's text.
8. **Steering field** (0, or 1 to 6 rows). Present only while the user is editing a steering input (§5.1). Its first row is the steering label (§5.2).

The app's own status row and footer stay below the pane, unchanged.

- **TSV-R1.** The regions MUST keep this order at every terminal size. When rows run short, they give way in this order:
  1. the header's second row;
  2. the reply strip, down to 1 row, then folded into the activity line as "· N suggestions, i to choose";
  3. the notice band, down to 1 row with "+N more";
  4. the decision band, folded into the activity line as "· approval waits on you";
  5. the transcript, down to 3 rows.

  All of these rules count the **pane's** height: the rows the session pane gets after the app's own header, status row and footer. The pane's height rules are:
  - While the steering field is open, it keeps at least 1 row.
  - Under 8 rows, the header and the activity line take 1 row each.
  - Under 3 rows, the pane shows only the transcript, or only the steering field while it is open.

  The app's existing whole-screen check is separate and comes first. It is made on the whole terminal, not the pane: when the terminal is narrower than 20 columns or shorter than 5 rows, the app shows its "too small" screen in place of everything.
- **TSV-R2.** On a wide terminal (the existing wide threshold of the TUI), the pane sits beside the board as today. On a narrow one it takes the whole body. The Stream tab's layout is the same in both. Only its width differs.

Indicative wireframe, 100 columns. The marks are placeholders: Team B chooses the glyph set. Their shapes must differ (§8).

```
 [run] working  lars-coder (@lars)  acp  #41 Board freshness  step: running tests       heard 3s ago
 tests 12 pass 0 fail · 41k tok · $0.38 · sandbox advisory
 Stream* · Timeline · Changes · Record                                              conversation
 ! You can watch this session but not read its full context.
 ─── turn 3 · steered by @ada ──────────────────────────────────────────────────────────────────
 you    @ada: please also cover the 30 min failed state                       recorded · seen
 agent  I'll extend the check to the failed state too. First the tests:
        - `board::fresh` covers stale
        - nothing covers failed yet
 think  (folded) 4s
 tools  [ok] read 3 files, searched 1 time                                                 1.2s
 edit   [ok] edited src/board.rs                                               +14 -2      0.3s
 run    [run] ran cargo test board                                                      running
      | test board::fresh ... ok
      | test board::failed ... ok
      | 6 of 31 lines shown · e shows all
 ask    waiting for approval: git.push → origin       open · a to review
 ────────────────────────────────────────────────────────────────────────────────────────────────
 working · ran cargo test board · 12s                                                following
 approval waits on you: git.push han/41 → origin  digest 9f3c 1a2b   a to review
```

---

## 3. Entries by item kind

Each entry has a **kind label** in the gutter (a short word: `agent`, `you`, `think`, `plan`, a tool verb, `file`, `ask`, `input`, `?`) and a **head line**. Most also have a **body**.

### 3.1 Messages

- **TSV-R3.** A `message` with `role: agent` is labelled `agent`. Its text is rendered as terminal markdown (§3.10). While `done` is false, a "still writing" mark ends the last line. Once `done` is true, the mark goes away. The entry grows in place: one item id is one entry, whatever the number of updates.
- **TSV-R4.** A `message` with `role: user` is labelled with its author: `you` when it is the local user's own, otherwise the actor's handle. It is plain text, not markdown. When a user-role message and an `input` item stand for the same steering input, the view shows **one** entry, the `input` one (§3.6). How the two are matched is pending node API (P-7). Until then, both are shown.

### 3.2 Reasoning

- **TSV-R5.** A `reasoning` item is labelled `think`. By default it is **folded** to a head line, "(folded)", with its duration when `duration_ms` is known. Expanding it shows the text, in a quieter style that also has a non-colour distinction (§8).
  - `:thinking show` makes reasoning unfolded by default for this session.
  - `:thinking hide` restores the default.
  - The choice is remembered in `tui.json` as `thinking` (§8.1).
- **TSV-R6.** Without the read right, the node leaves reasoning out. The view MUST NOT show a placeholder per missing item. The watch-only notice (§7) covers it once.

### 3.3 Plans

- **TSV-R7.** A `plan` is labelled `plan`. Its head line reads "N of M steps done". Its body is a checklist, one line per entry, and each line's done mark differs from the not-done mark in shape and by a word or symbol, not by colour.
  - The latest plan item in the session is unfolded.
  - When a newer plan item arrives, every older one folds to its head line.
  - The checklist has a third mark for "in progress", pending node API (P-6). Until P-6 lands, the first not-done entry after the last done one is **not** guessed to be in progress.

### 3.4 Tool calls

A tool-call entry's head line has these fields, from left to right:

```
gutter: verb label │ status mark │ subject                                │ +a -d │ duration │ status word
```

- **Verb label by `tool_kind`:**

  | `tool_kind` | verb label |
  |---|---|
  | read | read |
  | edit | edit |
  | delete | delete |
  | move | move |
  | search | search |
  | execute | run |
  | think | think |
  | fetch | fetch |
  | other | tool |

  The head line's subject starts with the past-tense verb from session.md §3.2 ("read", "edited", "deleted", "moved", "searched", "ran", "thought", "fetched", "used").
- **Subject:**
  - the `title`;
  - else the first location's path, with ":line" when there is a line;
  - else the command, for execute, pending node API (P-2);
  - else the kind word alone.
- **Status:** each of `pending`, `in_progress`, `completed` and `failed` has its own mark shape. The status word is shown for `pending` ("queued"), `in_progress` ("running") and `failed` ("failed"). `completed` shows its mark only.
- **Line counts:** "+a −d" is shown when the `file_change` items with this `tool_call_id` sum to non-zero, or when a diff content block gives the counts (P-1).
- **Duration:**
  - `ended_at − started_at` when both are known;
  - while running, the time since `started_at`, refreshed at most once a second;
  - else nothing.

The body is the call's content (`content[]`), drawn as **output lines** under a gutter continuation mark. Locations are listed for `read` and `search`.

- **TSV-R8. Folded preview.** A tool call's body is folded to a **preview** of at most 6 output lines, followed by one line "S of T lines shown · e shows all". The preview lines depend on the kind and state:
  - an `execute` call that is running shows its **last** 6 lines, so new output stays in sight;
  - a finished `execute` call shows its first 2 and last 4 lines, with "… N lines …" between them when it has more than 6;
  - every other kind shows its first 6 lines.

  An `execute` call that finished with an empty body says so in words on one body line ("nothing printed"); other kinds with an empty body have no body.
- **TSV-R9. Failed calls.** A `failed` tool call that is not inside a group is unfolded by default. Its head line ends with the word "failed". When an error text is known (P-4), it is the first body line, labelled "error:".
- **TSV-R10. Expanded body.**
  - Expanding a tool call shows up to 400 output lines in place.
  - Past 400 lines, the last line reads "(400 of T lines, o opens the reader)".
  - The reader (§4.5) shows all lines.
  - Expanding and folding keep the entry's head line at the same screen row (§4.4).
- **TSV-R11. Edits.** An `edit` call whose content holds a diff (P-1) shows the diff as its body:
  - one header line per file, with the path and "+a −d";
  - then the hunks, with each line's kind marked by a `+`, `-` or space column as well as by colour.

  Until P-1 lands, the body is the content text as it comes, and the `file_change` items joined to the call (§3.5) list the files. The entry offers `c`, "show in Changes", which switches to the Changes tab and selects the first file of the call.
- **TSV-R12. Grouping.** Three or more consecutive tool calls in one turn, with no other entry between them except file changes joined to them, fold into one **group entry**:
  - Its gutter label is `tools`.
  - Its head line summarises the members by kind, in first-appearance order: "read 3 files, searched 2 times, ran 1 command".
  - Its status follows session.md SES-6: running if any member runs, otherwise failed if any failed, otherwise completed.
  - "N failed" is added in words.
  - While the last member runs, the summary ends with that member's subject.

  Expanding a group lists its members as tool entries, indented one step, and each member folds and unfolds on its own. A group never crosses a turn divider or a message.

### 3.5 File changes

- **TSV-R13.** A `file_change` with a `tool_call_id` the view holds draws no entry of its own: it adds to that call's line counts and file list. Any other file change is its own entry, labelled `file`: "changed path +a −d".

### 3.6 Steering inputs

- **TSV-R14.** An `input` item is labelled with its author (`you` or the handle). Its text is shown, or "(text redacted)" when `text` is absent. A **delivery state** is drawn at the right:

  | State | Comes from | Shown as |
  |---|---|---|
  | sending | local: the call is in flight | "sending" |
  | queued | local: `Queued{outbox_id}` | "queued: home unreachable" |
  | recorded | item `state: recorded`, or the call's receipt | "recorded" (with "seq N" when the receipt gave it) |
  | seen | item `state: seen` | "recorded · seen" |
  | refused | local: the call failed | "not sent: <reason>" and the word "failed" |

  The local states attach to an optimistic entry, created at once on send. When the node's `input` item for the same entry arrives, it replaces the optimistic entry in place, with no duplicate. Others' inputs show the author and the receipt time.

### 3.7 Decisions

- **TSV-R15.** A `decision` item is labelled `ask`. Its head line is "waiting for approval: <action>". When the Approvals view holds the proposal, the line adds the target and the state ("open", "approved", "cancelled", and so on). A matching Approvals row is joined by `effect`, the proposal's entry id, and the view reads its state from that row. It never infers a state. When the proposal waits on the local user, the line ends with "a to review". The approval card itself is never drawn in the transcript. A decision's state once the Approvals row is gone is pending node API (P-9).

### 3.8 Turns

- **TSV-R16.** A `turn_start` draws a full-width divider: "turn N" plus its driver.
  - `task`: "from the task".
  - `input`: "steered by @handle", or "steered by you". The handle is resolved from the input whose entry is `driver_entry`, when the view holds it.
  - `effect`: "after an outcome".
  - `own`: nothing more.

  A `strict: true` turn adds "· strict (outside instruction)". A `turn_end` draws a thin closing divider with its duration and tokens, when known. Its outcome (completed, interrupted, failed) is pending node API (P-5).

### 3.9 Status and unknown items

- **TSV-R17.** `status` items draw no entry. The latest one feeds the header and the activity line. `:status` opens the latest status body as cleaned, indented JSON in the reader.
- **TSV-R18.** An `unknown` item is labelled `?`. Its head line reads "unsupported item (<raw_kind>)". Expanding it shows `raw` as cleaned text. It is never dropped.

### 3.10 Terminal markdown

Agent messages use a safe subset of markdown, mapped to terminal text:

- **TSV-R19.** The subset and how each element renders:
  - **Paragraphs** are word-wrapped to the transcript width, minus the gutter.
  - **Headings** are their text on its own line, emphasised, with a blank line before. Levels 1 and 2 are followed by a rule line the TUI draws under the text; deeper levels are emphasised only. Heading levels are not colour-coded.
  - **Emphasis and strong text** use terminal attributes. When attributes are off (§8), the source markers stay in the text.
  - **Lists** keep their markers and a hanging indent of the marker's width, and nest by 2 columns per level.
  - **Block quotes** get a quote mark in the column after the gutter.
  - **Inline code** is shown with its backquotes, so it stays readable without colour.
  - **Code blocks** are framed by a top line that names the language, when given, and a bottom line, both drawn by the TUI. Each code line carries a "code" continuation mark. They are not cut: a long code line wraps, and its continuation rows carry a distinct "continued" mark in the code column, so a reader can tell a wrapped line from two lines. `o` on the entry shows them unwrapped in the reader. Syntax colour MAY be applied. It is never the only signal.
  - **Tables** render as aligned columns when they fit the width. When they don't, they render as one "header: value" line per cell, row by row.
  - **Links** render as their text followed by the full URL in parentheses, or as the URL alone when the text equals it. The TUI emits no terminal hyperlink sequences.
  - **Images** render as "[image: alt text] (url)".
  - **Raw HTML** renders as its literal text.
  - **Anything else** renders as its source text.
- **TSV-R20.** Streaming stability:
  - A message that is not `done` is re-rendered from its whole text on each update.
  - An unclosed code fence is treated as an open code block until it closes, so later text does not flip between prose and code.
  - Entries above an updated entry never move on screen (§4.4).

---

## 4. Interaction

### 4.1 Focus and movement

The Stream tab has an entry cursor, the focused entry, drawn by a marker in the gutter and by reverse video. The marker is required. The keys apply when the session pane has focus and the steering field is closed. They extend the TUI's existing keys. No existing key changes meaning.

| Key | Does |
|---|---|
| `j` / `↓`, `k` / `↑` | focus the next or previous entry (scrolls as needed) |
| `Ctrl-d` / `Ctrl-u`, `PageDown` / `PageUp` | half a page / a page down or up, moving focus with the view |
| `g g`, `Home` | the first loaded entry (loads earlier pages, §4.4) |
| `G`, `End` | the last entry, and follow again |
| `(` / `)` | the previous or next turn divider |
| `e` | fold or unfold the focused entry (a group, tool call, reasoning, plan or unknown item) |
| `E` | fold or unfold every entry of the focused entry's turn |
| `o` | open the focused entry in the reader (§4.5) |
| `c` | on a tool call that changed files: show it in the Changes tab |
| `s` | switch between conversation and steps mode (§4.6) |
| `/` | search the loaded transcript; `n` / `N` go to the next or previous match |
| `i`, `Enter` | open the steering field (§5.1), as today |
| `Alt-1` … `Alt-9` | open the steering field holding that suggested reply (§5.4) |
| `a` | open the approvals pane on this session's proposals, as today (§5.3) |
| `1` `2` `3` `4`, `[` / `]` | tabs, as today. `4` is the new Record tab (§4.7). |
| `Esc`, `q` | leave the pane, as today. `Esc` first closes an open search or the reader. |

`n` and `N` keep their meaning on the Changes tab (next and previous file). On the Stream tab they move between search matches.

- **TSV-R21.** Focus is kept by item `id`. When the focused entry's item updates, is folded into a group, or is replaced, focus stays on the same item, or on the group that now holds it.
- **TSV-R22.** Every key above is also a `:` palette command, so a user who doesn't know the keys can still reach every action:
  - `:top`
  - `:bottom`
  - `:follow`
  - `:expand`, `:collapse`
  - `:expand turn`, `:collapse turn`
  - `:expand all`, `:collapse all`
  - `:reader`
  - `:steps`, `:conversation`
  - `:find <text>`
  - `:thinking show|hide`
  - `:color auto|always|never`, `:plain on|off`, `:motion reduced|full` (§8.1)
  - `:status`
  - `:notices`
  - `:record`
  - `:steer`
  - `:approvals`

  `:help` and `?` list the session keys in the existing help overlay, in a "Session" section.

### 4.2 Search

- **TSV-R23.** `/` opens a one-line search field in place of the activity line. The search is case-insensitive and runs over the cleaned text (§6) of every loaded entry, folded bodies included. A match inside a folded body unfolds that entry while it is the current match. The activity line shows "match i of n". Enter keeps the matches and closes the field; `Esc` clears them. A search does not load pages the view hasn't loaded, and it says so: "searching loaded entries (from turn N)".

### 4.3 Folding defaults

| Entry | Default |
|---|---|
| agent message, user message, input, decision, turn divider | always unfolded (they have no fold) |
| reasoning | folded (TSV-R5) |
| plan | the latest unfolded, older ones folded |
| tool call | preview (TSV-R8); a failed call outside a group is unfolded (TSV-R9) |
| group | folded |
| unknown | folded |

- **TSV-R24.** The view remembers what the user folded and unfolded, per item id, for as long as the pane is open. Updates to an item do not reset its fold.

### 4.4 Following, anchoring and paging

- **TSV-R25. Following.** The view follows when the last entry's last line is on screen. While it follows, new and growing entries keep the bottom in sight. Moving focus up or scrolling up stops following. `G`, `End` and `:follow` start it again. Sending a steering input starts it too.
- **TSV-R26. Not following.** While the view is not following, items that arrive or grow below the viewport MUST NOT move what is on screen. The activity line counts the entries added since following stopped: "N new below · G to follow".
- **TSV-R27. Anchoring.** Whenever the content changes, the topmost visible entry and its row offset stay where they are:
  - an entry above the viewport grows or is replaced;
  - an earlier page loads;
  - a group forms;
  - the terminal is resized.

  When the focused entry is folded or unfolded, its head line keeps its screen row.
- **TSV-R28. Paging.**
  - **Open.** Opening the pane subscribes to `Session{part: items}` with `Window.from_end` and the default limit (200).
  - **Scroll back.** Moving focus or the view within one screen of the first loaded entry asks for the page before it (`Window.before` = the first key). A "loading earlier entries" line shows at the top until the page arrives. Once `hasEarlier` is false, a "beginning of session" divider shows at the top.
  - **Limit.** The view holds at most 2,000 items. When scrolling back passes that, the pages farthest from the viewport are released, and they are fetched again if the user returns to them.
- **TSV-R29. Resize.** On a resize, the view re-wraps from item text. It never reflows text it has already wrapped. Fold state, focus and the anchor survive.

### 4.5 The reader

- **TSV-R30.** `o` opens the focused entry full-screen over the pane: every line of its body (for a group, every member's), unwrapped lines cut with a mark, and horizontal scrolling with `h` / `l`. The other keys:
  - `j`, `k`, `Ctrl-d`, `Ctrl-u`, `g g` and `G` move;
  - `/` searches the entry;
  - `Esc` or `q` closes the reader.

  The reader shows the entry's item id and its last-updated age in its title. While open, it keeps updating from the live item.

### 4.6 Conversation and steps modes

- **TSV-R31.** The conversation mode shows every entry. The steps mode shows only these entries:
  - tool calls and groups;
  - plans;
  - file changes;
  - decisions;
  - turn dividers.

  In steps mode, each agent message folds to one line, "said: <first line>", which `e` unfolds. Steps is the default when the session is not the local user's own (`is_mine` false), as in the GUI (session.md §3.4). `s` toggles the mode. The last choice is remembered in `tui.json` as `session_mode` (§8.1). Once it is set, it overrides the default.

### 4.7 The Record tab

- **TSV-R32a. Four tabs.** The pane now has four tabs: Stream, Timeline, Changes, Record.
  - `]` goes to the next tab, in that order, and wraps from Record to Stream.
  - `[` goes to the previous tab, and wraps from Stream to Record.
  - Today's previous-tab handler assumes three tabs (it takes the next tab twice), so it MUST be changed to step back one tab among four.
  - `1` to `4` pick a tab directly.
- **TSV-R32b. Keys on the Record tab.**
  - `j` / `k` move a row cursor, drawn as in the Stream tab.
  - `g g` / `G` go to the first or last row. `Ctrl-d` / `Ctrl-u` and `PageDown` / `PageUp` move by half a page or a page.
  - `e` unfolds the focused row's summary to its full wrapped text.
  - `/` searches the rows' cleaned text. `n` / `N` go to the next or previous match, and without a search they do nothing.
  - `i` and `a` keep their meaning.
  - `(`, `)`, `E`, `o`, `c` and `s` do nothing on this tab.
- **TSV-R32.** Tab `4`, "Record", lists `Session{part: record}` rows oldest first. Each row shows:
  - the sequence number;
  - the time;
  - the author;
  - the summary;
  - a "verified" mark with the word.

  The tab's title line reads "Record · signed entries in the space's log". A redacted row reads "content redacted" and the reason, if any. Nothing from the items is ever drawn in this tab, and nothing from the Record is drawn in the transcript with the Record's styling.

---

## 5. Steering and approvals

### 5.1 Steering field

`i` and Enter open it, Enter sends, and `Esc` cancels, as the TUI does today.

- **TSV-R33.** The field opens only when the local user may steer. The view reads that from the node's `can_steer` and never derives it. Today `can_steer` comes from the board row the pane was opened from. Carrying it on the facts row is pending node API (P-8). Without the steer right, `i` shows "watch only: you do not have the right to steer this session", as today.
- **TSV-R34.** Sending calls `SessionInput{space, session, text}` once. While the call runs, Enter does nothing, so there is no double send. The text is trimmed, and an empty text sends nothing. The field grows to 6 rows, then scrolls. Text over the node's limit (Q-SES-3) is refused before sending, with the limit in words.
- **TSV-R35.** A refused input puts its text back into the field, which opens again with the reason above it.

### 5.2 The steering label

- **TSV-R36.** While the field is open, its first row states what sending means, with the wording of session.md §7.1:
  - "Instruction to your agent <name>";
  - "Steering @<person>'s agent <name>. It acts on this as an outside instruction.";
  - the guarded variant, which names who must approve what it causes.

  The label MUST be on screen before the first key is typed. It MUST NOT suggest that an outside instruction will be obeyed. The guard facts it needs are pending node API (Q-SES-5, P-8). Until they land, an agent that is not the user's own uses the outside-instruction wording.

### 5.3 Approvals

- **TSV-R37.** The session view never signs. `a` opens the approvals pane with this session's proposals selected, as today. From there, the existing two-step flow applies:
  1. `a` or Enter opens the full card;
  2. `a` on the card opens the sign panel;
  3. Enter in the sign panel signs.

  No key in the session pane, the decision band or a decision entry approves, cancels or signs.

### 5.4 Suggested replies

An agent's turn may end with numbered next steps that it suggests: "1. Run the full suite. 2. Open the PR. 3. Stop here." The node gives them to the view as a `suggestions` item of that turn (P-15). The view never looks for them in message text itself.

- **TSV-R52. When the strip shows.** The reply strip shows the suggestions of the session's **latest** turn, only after that turn has ended, and only while the local user may steer (`can_steer`, TSV-R33). It goes away when a new turn starts, when the session closes, or when an input is recorded. Suggestions of older turns are never offered. They stay visible inside their turn as a folded entry, labelled `next`, that reads "suggested N next steps".
- **TSV-R53. Agent text, not HAN's.** The strip's first column holds the TUI-drawn label "lars-coder suggests:" (the agent's short name). Each suggestion follows as its digit, then its text in quotation marks, in the style of agent text:

  ```
   lars-coder suggests:  1 "Run the full test suite"  2 "Open the PR"  3 "Stop here"
  ```

  A suggestion is foreign text (§6). It is sanitised as single-line text, cut with an ellipsis at 200 characters or at the strip's width, and never drawn as a key hint, a control or a notice. A suggestion that reads like a HAN control ("a to approve") is still only quoted text. The strip shows at most 9 suggestions. When they don't fit in its rows, it ends with "+N more, i shows all".
- **TSV-R54. Choosing.** A digit picks a suggestion, and the steering label is on screen before anything is sent (TSV-R36):
  - With the steering field open and empty, the field lists every suggestion above the label. When the **first** key typed is a digit from 1 to the number of suggestions, the field takes that suggestion's full text, and the label adds "· choosing its suggestion N". Any other first key is ordinary text, and digits after it are text too. A message that starts with a digit is written with a leading space, which sending trims (TSV-R34).
  - `Alt-1` … `Alt-9` on the Stream tab open the field already holding that suggestion. The tabs keep `1` to `4` (TSV-R32a).
  - Enter records it. The digit alone sends nothing.
- **TSV-R55. Recording.** Sending a chosen suggestion calls `SessionInput{space, session, text, choice{item, n}}` once (P-15), with the suggestion's text. If the user edits the text before sending, the choice is dropped, the label's addition goes away, and the text is sent as the user's own words. If a new turn starts while the field holds a chosen suggestion, the field keeps the text, drops the choice, and says "The agent has moved on; this goes as your own words."
- **TSV-R56. Attribution.** The `input` entry of a chosen suggestion reads, after its author label (`you` or the handle), "chose lars-coder's suggestion 2:" and then the suggestion's text in quotation marks, in agent text style. It is always attributed to the person who chose it, and the quoted text is always shown as the agent's words. For an agent that is not the chooser's own, the entry adds "(outside instruction)", as the steering label said.
- **TSV-R57. Nothing else.** Choosing a suggestion sends only `SessionInput`. It never approves, cancels, starts or stops anything, whatever the suggestion says.

---

## 6. Untrusted text

All foreign text is data: agent messages and reasoning, tool titles, subjects, content, locations, raw unknown items, status bodies, record summaries, other people's inputs, actor handles and paths.

- **TSV-R38. Sanitising.** Before any foreign text reaches the terminal (SR-96):
  - **Escape sequences.** A complete escape sequence (ESC followed by a CSI, OSC, DCS, APC, PM or SOS sequence and its terminator, or a two-character escape) is removed whole. An entry that had any removed adds one quiet body line, "N terminal control sequences removed". An unterminated sequence is treated as its characters, below.
  - **Other controls.** Every other C0 or C1 control character, DEL, and the bidirectional controls the TUI already escapes (the marks, embeddings, overrides and isolates) become visible escapes, as the TUI's existing clean function does (`<U+XXXX>`).
  - **Line breaks.** In multi-line text (bodies, messages, output), `\n`, `\r\n` and a lone `\r` are line breaks. A lone `\r` never moves the cursor back over text already drawn. A tab becomes spaces to the next multiple of 8 columns (the terminal default). In single-line text (head lines, subjects, labels), every line break becomes a space.
  - **No styles.** No foreign text sets colours or attributes, and none produces a hyperlink sequence.

  This needs a new **multi-line sanitiser**. It keeps the line breaks and tabs, turning them into the breaks and spaces described above, and removes escape sequences whole and counts them. The TUI's existing `clean()` turns every line break and tab into a space, so it stays the sanitiser for single-line fields only: head lines, subjects, labels, handles and paths. Bodies, messages, reasoning, tool output and raw unknown items go through the new one.
- **TSV-R39. Masked spans.** A span listed in an item's `masked` is drawn as a masked marker, the word "masked" set apart in more than colour, in place of the span's text. The view never tries to unmask it. The span offsets are UTF-16 code units (data.md Q-DAT-18). The view maps them onto the text before it sanitises.
- **TSV-R40. No impersonation.** The gutter, the dividers, the header, the notice band, the activity line, the decision band and the steering label are drawn only by the TUI, and only from facts. Foreign text is drawn only to the right of the gutter, so no item text can pass itself off as a turn divider, a decision, a notice or a Record line.
- **TSV-R41. Width safety.** Every line is measured by display width, with wide and zero-width characters counted as the TUI's fit functions count them. A long single-line field is cut with an ellipsis. A long body line is wrapped (in code with the continued mark); in the reader it is cut with a mark and scrolls sideways. Neither a line nor its wide characters may run past the pane's right edge.

---

## 7. Surface states

| State | What the view does |
|---|---|
| loading | the header comes from the board row, if the view has one. The transcript shows "loading the session…". |
| empty | "Waiting for <agent>'s first step…" |
| watch only (`rights.read` false) | the notice reads "You can watch this session but not read its full context." Reasoning is absent and tool bodies are empty, without a placeholder per entry (TSV-R6). |
| no rights (`items` is `denied`) | the transcript is replaced by "This session is closed or no longer visible to you." The header still shows the facts. |
| stale | the notice reads "No update from this session for <age>. Its host may be busy or offline." Running marks become a static "no update" mark with those words. |
| failed | the notice reads "This session failed: <state_reason>." The steering field may still open, and its label adds "The host is unreachable; your input will be delivered when it returns." |
| closed | the notice reads "Session closed." The steering field cannot open, and `i` says why. |
| resyncing | the notice reads "resyncing…" while the view subscribes again after a gap, `Resync` or `End` (DAT-15). Entries stay on screen and are replaced from the new snapshot, keeping focus and anchor by id. |
| view error | one notice per failed part: "stream: <error>, r to retry", "record: …". A failed `items` part does not blank the header, and a failed `facts` part does not blank the transcript. |

---

## 8. Accessibility and colour

- **TSV-R42. Never colour alone.** Everything a colour says is also said by a shape, a word or a position:
  - state;
  - tool status;
  - failure;
  - focus;
  - the current tab;
  - masked text;
  - the quieter style of reasoning;
  - diff line kinds;
  - the delivery states.

  With colour off, a screen dump of the pane carries the same information as with colour on.
- **TSV-R43. Colour off.** The `color` setting (§8.1) decides:
  - `never`: colour is off;
  - `always`: colour is on;
  - `auto`: colour is on unless the terminal reports no colour support.

  A non-empty `NO_COLOR` environment variable is honoured as `color = never`, whatever `tui.json` says. Text attributes (bold, reverse, underline, dim) stay on unless `plain` is true. With attributes off, reverse-video focus falls back to the gutter marker, which is always drawn anyway.
- **TSV-R44. Motion.** A running mark MAY animate at most 4 times a second. When `reduced_motion` is true, or colour and attributes are both off, it does not animate. A static running mark plus the word "running" carries the state.
- **TSV-R45. Narrow terminals.** The transcript keeps every entry at any width of 20 columns or more. Columns give way in this order:
  1. the duration;
  2. the "+a −d" counts;
  3. the delivery-state detail, down to its first word;
  4. the gutter label, down to a 1-character kind mark that is distinct per kind (§3), with the legend in `?`.

  The status word of a failed, running or queued tool call is never dropped. Body text wraps to the space that is left.
- **TSV-R46. Light and dark.** The TUI uses the terminal's default foreground and background for body text. It sets colours only on marks, labels and diff lines, so text stays readable on light and dark backgrounds alike.

### 8.1 Settings

`apps/han-tui` keeps its choices in `tui.json`. Today the file holds only `groups` and `last_space`. This spec adds five **new** top-level keys:

| Key | Type and values | Default | Set by |
|---|---|---|---|
| `color` | string: `"auto"`, `"always"` or `"never"` | `"auto"` | `:color auto`, `:color always`, `:color never` |
| `plain` | bool | `false` | `:plain on`, `:plain off` |
| `reduced_motion` | bool | `false` | `:motion reduced`, `:motion full` |
| `thinking` | string: `"hide"` or `"show"` | `"hide"` | `:thinking hide`, `:thinking show` (TSV-R5) |
| `session_mode` | string: `"conversation"` or `"steps"`, or absent | absent: the default by `is_mine` (TSV-R31) | `s`, `:steps`, `:conversation` |

- **TSV-R46a.** Each key is optional.
  - A missing key, or a value of the wrong type or outside its list, reads as the default for that key alone. The rest of the file still loads, as a bad config never stops the TUI today.
  - Each command takes effect at once and saves the file the way the TUI saves it today.
  - `NO_COLOR` is not written to the file. It overrides `color` only for the run it is set in.
  - There is no general `:set` command. Each setting has the named command above.

---

## 9. Performance budgets

Measured in a release build, on TestBackend at 200 × 50 unless stated, with the fake node of `apps/han-tui/src/testing.rs`.

- **TSV-R47. Frame cost.** Drawing a frame of the Stream tab with 2,000 loaded items, 10% of them tool calls with 400-line bodies, takes at most 8 ms at the 95th percentile. The cost depends on the visible rows and the changed entries, not on the number of loaded items: doubling the loaded items from 1,000 to 2,000 adds no more than 20% to the median frame time.
- **TSV-R48. Update cost.** Applying one item diff that grows a message by one delta costs work in proportion to that entry alone. Layout (wrapped lines per entry) is cached per item id, width, fold state and mode. A cache entry is invalidated only when its item changes, the width changes, or its fold or mode changes.
- **TSV-R49. Redraw rate.** Streaming updates are coalesced: at most 20 redraws a second come from item diffs. A key press is drawn in the next frame, within 50 ms, whatever the stream is doing.
- **TSV-R50. Huge bodies.** A tool body of 100,000 lines, or a single line of 1 MB, does not stall the UI. Only the lines on screen are wrapped and drawn. A line is cut at 4,096 characters for display, with "(line cut, N more characters)", and the reader shows it in full with horizontal scrolling. Counting a body's lines is done once per update of that item.
- **TSV-R51. Memory.** The view holds at most 2,000 items (TSV-R28) and the layout cache for the entries within 3 screens of the viewport. Releasing far pages frees their layout.

---

## 10. Pending node API

The view is specified against the item shapes in `crates/han-node/src/api/items.rs` and data.md §12.1. These additions are needed for criteria marked *pending*. Until they land, Team B builds on a provisional type and runs those criteria against the fake node.

| ID | Item or field | Needed by |
|---|---|---|
| P-1 | `tool_call.content[]` blocks with a type, one of `text`, `diff` (per file: path, and unified hunks or old and new text) and `terminal`, instead of text only | TSV-R11, line counts on edits |
| P-2 | `tool_call.input`: a short, cleaned summary of the call's arguments, with the full command for `execute` | the subject when `title` is absent; the full command in the expanded body |
| P-3 | `tool_call.exit_code` for `execute`, and `tool_call.truncated{shown, total}` when the host cut the output | the empty-output and exit-status lines; honest line counts in TSV-R8 and TSV-R10 |
| P-4 | `tool_call.error`: the failure text of a `failed` call | TSV-R9 |
| P-5 | `turn_end.outcome` (`completed`, `interrupted`, `failed`, `refused`) and `turn_end.reason` | TSV-R16 |
| P-6 | `plan.entries[].status` (`pending`, `in_progress`, `completed`) next to `done` | TSV-R7 |
| P-7 | `message.input`: on a user-role message, the `session.input` entry it carries | TSV-R4 (one entry per steering input) |
| P-8 | `can_steer`, `can_cancel` and `needs_me` on the `facts` row (`SessionRow`), plus the guard facts of Q-SES-5 | TSV-R33, TSV-R36; the pane opened without a board row (from `:space`, or a session outside the board's window) |
| P-9 | `decision.state` (`open`, `approved`, `cancelled`, `expired`, `succeeded`, `failed`) and `decision.target` on the item | TSV-R15 once the Approvals row is gone |
| P-10 | a host `notice` item kind (`{level: info, warning or error; text}`) for harness errors, retries and rate limits | errors that are not tool failures; today they arrive as `unknown` (TSV-R18) |
| P-11 | `reasoning.done` | the "still writing" mark on reasoning |
| P-12 | `created_ms` on `ItemRow` (when the node first saw the item), next to `updated_ms` | the reader's title; message times |
| P-13 | `tool_call.parent`: the tool call that started this one, for nested calls such as sub-agents | indentation under the parent call; without it, nested calls are shown flat |
| P-14 | the steering input length limit (Q-SES-3) | TSV-R34 |
| P-15 | suggested replies: a `suggestions` item per turn, `{turn, options[{n, text}], derived}`, and `SessionInput.choice{item, n}`, carried on the `input` item and the Record row (attention.md AN-6) | TSV-R52 to TSV-R57 |

---

## 11. Acceptance criteria

Each criterion runs on ratatui's TestBackend through the TUI's test harness (`apps/han-tui/src/testing.rs`), with a fake node that serves the named fixture. "Screen" means the rendered buffer as text, and "colour off" means the harness runs with `NO_COLOR=1`. Sizes are columns × rows.

| ID | Given | When | Then |
|---|---|---|---|
| TSV-1 | a session with 1 agent message, 1 tool call, 1 turn; 120 × 30 | the pane opens on the Stream tab | the regions appear top to bottom: header, tab line, transcript, activity line; no notice band, decision band or steering field |
| TSV-2 | an agent message item | the fake node sends 20 updates of the same id, each a longer prefix, the last with `done: true` | the screen shows the text once, equal to the final text; no "still writing" mark remains |
| TSV-3 | an agent message with `done: false` | the screen is drawn | its last line ends with the "still writing" mark |
| TSV-4 | a tool call `execute` | its status goes pending → in_progress → completed | one head line throughout; the status word reads "queued", then "running", then none; the gutter label is `run` |
| TSV-5 | a completed `execute` call with 31 output lines | the screen is drawn | the body shows its first 2 lines, a "… N lines …" line and its last 4 lines, then "6 of 31 lines shown · e shows all" |
| TSV-6 | an in-progress `execute` call | 10 more output lines arrive | the preview shows the newest 6 lines |
| TSV-7 | the call of TSV-5, focused | `e` | 31 output lines show; the head line's screen row does not change; `e` again restores the preview |
| TSV-8 | a tool call with 1,000 output lines, expanded | the screen is drawn | 400 lines show, then "(400 of 1000 lines, o opens the reader)"; `o` opens the reader, and `G` there shows line 1000 |
| TSV-9 | a failed tool call outside a group | the screen is drawn | it is unfolded; its head line contains "failed" |
| TSV-10 | 3 reads, 1 search, 1 execute in a row in one turn | the screen is drawn | one group entry labelled `tools` reads "read 3 files, searched 1 time, ran 1 command"; `e` lists 5 indented member entries |
| TSV-11 | a group whose second member fails | the screen is drawn | the group's head line contains "1 failed" |
| TSV-12 | 2 tool calls, an agent message, then 2 tool calls | the screen is drawn | no group forms; 4 tool entries and 1 message entry show |
| TSV-13 | a `file_change` with the `tool_call_id` of a held edit call, +14 −2 | the screen is drawn | no `file` entry; the edit's head line shows "+14 -2" (in either sign glyph) |
| TSV-14 | a `file_change` without a `tool_call_id` | the screen is drawn | a `file` entry reads "changed <path> +a -d" |
| TSV-15 | a reasoning item with `duration_ms` 4000 | the screen is drawn | one `think` head line with "(folded)" and "4s"; its text is not on screen; `e` shows the text |
| TSV-16 | `rights.read` false | the pane opens | the watch-only notice shows once; no `think` entry and no per-item placeholder exist |
| TSV-17 | two plan items, the second newer | the screen is drawn | the newer shows its checklist and "N of M steps done"; the older shows only its head line; colour off, done and not-done lines still differ in text |
| TSV-18 | a `turn_start` with driver `input` naming an input by @ada, and `strict: true` | the screen is drawn | a divider reads "turn N · steered by @ada · strict (outside instruction)" |
| TSV-19 | an `unknown` item with `raw_kind` "widget" | the screen is drawn | a `?` entry reads "unsupported item (widget)"; `e` shows the raw JSON text |
| TSV-20 | a `status` item | the screen is drawn | no transcript entry for it; the header shows its step |
| TSV-21 | the view following, 120 × 30 | 30 entries arrive | the last entry stays in sight; the activity line shows "following" |
| TSV-22 | the view scrolled up by `k` × 10 | 12 entries arrive, and an entry above the viewport grows by 5 lines | the screen's transcript rows are unchanged; the activity line shows "12 new below" |
| TSV-23 | TSV-22's state | `G` | the last entry is in sight and following resumes |
| TSV-24 | a session with 3,000 items | the pane opens, then `g g` | the first request is `from_end` with limit 200; earlier pages are requested with `before`; "beginning of session" shows at the top; no more than 2,000 items are held |
| TSV-25 | a focused entry in the middle of the screen | the terminal resizes from 120 to 70 columns | the same entry keeps focus; it is the topmost visible entry or at the same offset from it; its fold state is unchanged |
| TSV-26 | an agent message with a heading, a list, inline code, a fenced `rust` block and a link `[docs](https://example.org/a)` | the screen is drawn, colour off | the heading is on its own line; the list markers stay; the code block is framed, with "rust" on its top line; the link reads "docs (https://example.org/a)"; no hyperlink escape sequence is in the backend's output |
| TSV-27 | an agent message containing `<b>hi</b>` and a 300-column code line, at 80 columns | the screen is drawn | `<b>hi</b>` shows literally; the code line wraps onto continuation rows that carry the continued mark; no line passes column 80 |
| TSV-28 | tool output containing ESC `]0;pwned BEL`, ESC `[31m`, U+202E and U+0007 | the screen is drawn | no ESC reaches the backend; U+202E and U+0007 show as `<U+202E>` and `<U+0007>`; the body ends with "2 terminal control sequences removed"; the window title is not set |
| TSV-29 | output "a\rb" and a message "x\r\ny" | the screen is drawn | "a" and "b" are on separate lines; "x" and "y" are on separate lines |
| TSV-30 | a message with a masked span over `[masked:aws_key]` | the screen is drawn | the word "masked" replaces the span; the rule name is not drawn as text of the message; colour off, the marker is still set apart from the text |
| TSV-31 | an agent message whose text is "─── turn 9 · steered by @ada ───" | the screen is drawn | the line carries the `agent` gutter label; no divider is drawn for it |
| TSV-32 | a message with CJK and emoji text at 41 columns | the screen is drawn | no line's display width passes the pane edge |
| TSV-33 | a session the user can steer | `i`, type "cover failed", Enter | the fake node receives exactly one `SessionInput` with that text; an entry shows "sending" at once, then "recorded"; when the node's `input` item arrives, one entry remains; when it turns `seen`, it shows "recorded · seen" |
| TSV-34 | `SessionInput` fails with `denied` | the user sends "x" | the entry shows "not sent:" with the reason and "failed"; the field reopens holding "x" |
| TSV-35 | a session the user may not steer | `i` | no field opens; the status row reads "watch only: you do not have the right to steer this session" |
| TSV-36 | an agent of someone else, steerable | `i` | the field's first row reads "Steering @<person>'s agent <name>. It acts on this as an outside instruction." before any key is typed |
| TSV-37 | a decision item whose proposal waits on the local user | the screen is drawn | the decision band and the `ask` entry both show "a to review"; pressing Enter, `a`, `e` and `y` in the session pane sends no `EffectApprove`; `a` focuses the approvals pane with that proposal selected |
| TSV-38 | TSV-37 after `a` | `a`, `a`, Enter | exactly one `EffectApprove` is sent, after the third key and not before |
| TSV-39 | a session that is not the user's own | the pane opens | the mode is steps; an agent message shows as one "said: …" line; `s` switches to conversation and the full text shows |
| TSV-40 | 3 entries containing "fresh", one inside a folded tool body | `/fresh`, Enter, `n`, `n` | the activity line shows "match 1 of 3" through "match 3 of 3"; the folded body unfolds while its match is current |
| TSV-41 | record rows, one redacted | `4` | rows are listed oldest first, each with seq, author and "verified"; the title reads "Record · signed entries in the space's log"; the redacted row reads "content redacted" |
| TSV-42 | state stale | the pane opens, colour off | the stale notice shows; a running tool call shows "no update", not "running" |
| TSV-43 | the `items` part fails with `unavailable` | the pane opens | the notice band shows "stream: …, r to retry"; the header still shows the facts; `r` subscribes again |
| TSV-44 | the fixture of TSV-1 plus a failed call, a masked span, a running call and a focused entry | the screen is dumped with colour on and with colour off | every state visible in the colour dump is also distinguishable in the colour-off dump (same marks and words; focus marked in the gutter) |
| TSV-45 | 40 × 12 | the pane opens on the fixture of TSV-10 | every entry has a head line; durations and counts are gone; the failed and running status words remain; the decision band has folded into the activity line |
| TSV-46 | `reduced_motion` set, a running call | 10 frames are drawn 100 ms apart | the running mark is identical in every frame |
| TSV-47 | 2,000 items, 200 of them 400-line tool calls, 200 × 50, release build | 200 frames are drawn while scrolling | the 95th-percentile frame time is at most 8 ms; with 1,000 items the median is no more than 20% lower |
| TSV-48 | following, one message streaming 200 deltas a second | 2 s pass | at most 40 redraws from item diffs; a key pressed during the stream is drawn within 50 ms |
| TSV-49 | a tool body of 100,000 lines with one line of 1 MB | the entry is expanded and the reader opened | each frame stays within TSV-R47's budget; the long line shows "(line cut, N more characters)" in the transcript, and is complete in the reader |
| TSV-50 | the palette | `:expand all`, `:steps`, `:follow`, `:find fresh` in turn | each has the same effect as its key |
| TSV-51 | the pane on the Stream tab, with record rows | `[`, then `]`, then `4`, `j`, `n` | `[` shows Record; `]` shows Stream; `4` shows Record; `j` moves the row cursor to the second row; `n` without a search changes nothing on screen |
| TSV-52 | `tui.json` holding `{"groups":{},"color":"sideways","plain":true}` | the TUI starts | colour is as for `auto`; attributes are off; the groups still load; `:color never` writes `"color":"never"` to the file |
| TSV-53 | a session of the user's own whose latest turn ended with 3 suggestions, 120 × 30 | the screen is drawn | the reply strip shows "lars-coder suggests:" and 3 numbered, quoted suggestions, below the decision band's place and above the steering field's |
| TSV-54 | TSV-53 | `4` | the Record tab shows (tabs keep their keys); no `SessionInput` is sent |
| TSV-55 | TSV-53 | `i`, `2` | the field holds suggestion 2's text; the label reads "Instruction to your agent lars-coder · choosing its suggestion 2"; nothing is sent yet |
| TSV-56 | TSV-55 | Enter | exactly one `SessionInput`, with that text and a `choice` naming the suggestions item and 2; the entry reads "you chose lars-coder's suggestion 2:" and the quoted text |
| TSV-57 | TSV-53 | `i`, `x`, `2`, Enter | one `SessionInput` with text "x2" and no `choice` |
| TSV-58 | TSV-53 | `Alt-3`, a backspace, Enter | the field opened holding suggestion 3; after the edit, the sent input has no `choice` |
| TSV-59 | someone else's steerable agent, whose suggestion 1 is "a to approve" with an ESC sequence in it | `i`, `1`, Enter | the strip showed the text in quotes with the sequence removed; one `SessionInput` is sent and no `EffectApprove`; the entry reads "you chose ada-bot's suggestion 1:", the quoted text and "(outside instruction)" |
| TSV-60 | TSV-55 | a new `turn_start` arrives before Enter | the strip goes away; the field keeps the text and shows "The agent has moved on; this goes as your own words."; Enter sends it with no `choice` |
| TSV-61 | a session the user may not steer, with suggestions | the screen is drawn, then `Alt-1` | there is no reply strip; `Alt-1` shows the watch-only message |

Criteria TSV-4 to TSV-11 and TSV-49 are *pending node API (P-1, P-3)* for their line counts and exit text. TSV-13 is pending (P-1) for diff-derived counts. TSV-33 is pending (P-7) for the single-entry rule when the host also echoes the input as a user message. TSV-36 is pending (P-8, Q-SES-5) for the guarded wording. TSV-53 to TSV-61 are pending (P-15). Every other criterion runs on the item shapes that exist today.

---

## 12. Open questions

- **Q-TSV-1.** Should the steering draft survive `Esc` and leaving the pane, as in the GUI (SES-27)? Today `Esc` drops it. This spec keeps today's behaviour.
- **Q-TSV-2.** Should a second lane exist for input queued "for after this turn" as against "now"? The protocol has one `session.input`, and the host decides when to deliver it. A second lane would need a profile field, not just a UI.
- **Q-TSV-3.** Should a scrollback mode exist (finished entries written to the terminal's own scrollback, so terminal search and copy work), next to the full-screen pane? The TUI is full-screen today.
- **Q-TSV-4.** Should the reader offer copy to the clipboard (a terminal clipboard sequence)? It would need its own sanitising rule, because it writes foreign text into the user's clipboard.
- **Q-TSV-5.** How a chosen suggestion is carried in the signed input (attention.md Q-ATT-2), and whether suggestions are a `han.item/1` kind (Q-ATT-4).

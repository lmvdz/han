# UI spec: attention, jumping and notices

Status: draft, Team A (clean room, PLAN.md §8). This spec covers behaviour and appearance only. It applies to `apps/han-tui`, and its node rules apply to every face: the GUI reads the same views (board.md §4.5, approvals.md §6). Read it with [tui-session.md](tui-session.md), for the TUI's conventions, and [data.md](data.md), for views, windows and diffs.

A person in a HAN network may follow dozens of spaces, operate hundreds of agents and see thousands of sessions run by other people. Most of that is none of their business at any given moment. This spec answers one question for each person: **what is mine to act on now, and what changed for me since I last looked?** It defines:

- who must act on what (§2), so that "needs me" stays small at any scale;
- one ranked, bounded attention queue per person, computed by the node (§3);
- the attention count in the TUI frame, and the jump to the top item (§4);
- the lifecycle grouping of the board, rolled up by default (§5);
- per-person marks: unseen, acknowledged, muted (§6);
- the jump picker over spaces, issues and sessions (§7);
- notices: cues, budgets, digests and silence (§8);
- freshness under load (§9).

Suggested replies, which end an agent's turn with numbered follow-ups, are a session-view feature: tui-session.md §5.4.

Keywords MUST, SHOULD and MAY are as in RFC 2119. Numbered rules are normative: `ATT-R1`… for rules and `ATT-1`… for acceptance criteria. Node requirements are `AN-1`… (§11).

**The scale this spec is written for.** Every bound below is chosen for this load on one person's node, and the acceptance criteria use it (§12):
- 500 followed spaces, of which 40 are active;
- 200,000 sessions known to the node, 5,000 of them open;
- 1,000 people and 20,000 agents in those spaces; the viewer operates up to 500 agents;
- 5,000 incoming events per second, sustained for minutes, with bursts of 20,000 per second.

No view, count or notice may cost the face work in proportion to these totals. The face works in proportion to what is on screen.

---

## 1. Terms

| Term | Meaning |
|---|---|
| **Viewer** | the person using the face: the person behind the device's actor. A viewer is a person, never an agent. One viewer may use several devices. |
| **Role** | how the viewer stands to a piece of work (§2.1): operator, self, delegator, task owner, assignee, approver, guard, space owner, addressee, cover. A role is a fact the node reads from the log and the profiles. |
| **Attention item** | one thing the viewer must act on, with one kind (§2.2), one target (an effect, a session, a task, a join or a question), and the roles that put it there. |
| **Attention queue** | the viewer's attention items across every followed space, ranked by the node (§3). |
| **Attention count** | the number of items in the queue, less those acknowledged or muted (§4). |
| **Done-unseen** | a session of the viewer's work that finished after the viewer last opened it (§6.2). It is not an attention item and is counted apart. |
| **Mark** | per-viewer state: seen, acknowledged, muted (§6). Marks are private to the viewer. |
| **Lifecycle group** | one of `needs-me`, `to-decide`, `landing`, `working`, `idle`, computed per viewer by the node (§5). |
| **Bucket** | a roll-up of sessions inside a lifecycle group, by space, issue or operator (§5.2). |
| **Notice** | something the node decides is worth telling the viewer now: one item, or a digest of many (§8). |
| **Cue** | how a face presents a notice: an act cue, a done cue, or an error cue (§8.1). |
| **Frame** | the TUI's own header row and status row, around every pane. |

---

## 2. Relevance: who must act

### 2.1 Roles

The node gives the viewer a role in a piece of work only from facts it holds. The face never derives a role.

| Role | The viewer holds it when |
|---|---|
| **operator** | the viewer is the confirmed operator of the session's agent actor. |
| **self** | the viewer's own actor runs the session (a human's session). |
| **delegator** | the viewer is the session's person: the actor that delegated the work to the agent (board row `person`). |
| **task owner** | the viewer authored the task the session works. |
| **assignee** | the viewer is in the `to` list of the task. |
| **approver** | the node's `can_approve` is true for the viewer on an open effect (the rule's approvers, data.md §4.4). It is **pooled** when more than one person in the space other than the viewer could give the same countable approval, and **direct** otherwise. |
| **guard** | the viewer is a required guard of an open effect (approvals.md §1). A guard is never pooled. |
| **space owner** | the viewer holds the owner role in the space. It brings admissions only. |
| **addressee** | a question, message or task names the viewer, or an agent the viewer operates, as its recipient. |
| **cover** | another person has asked the viewer to cover for them (pending Q-ATT-3). The viewer then holds that person's operator and delegator items, each marked "for @person". |

- **ATT-R1.** Visibility is not relevance. Being able to watch or read a session gives no role in it. Holding the member, owner or any other space role gives no role in other people's sessions, except the space owner's admissions.
- **ATT-R2.** A role is held through the viewer's person, on every device of that person. An agent's own actor holds no role here: attention is for people.

### 2.2 Kinds of attention item

Each kind names the roles that put it in the queue, and its **tier**, which is the first key of the ranking (§3.1).

| Tier | Kind | Item | Roles that qualify |
|---|---|---|---|
| 1 | `approval` | an open effect that waits on a countable approval the viewer can give, and that the viewer has not yet given | guard; direct approver; a pooled approver after the pool grace (ATT-R5) |
| 2 | `ask` | a question or message to the viewer that nobody has answered; a turn of one of the viewer's agents that ended waiting for its operator (the turn ended with suggested replies, or the host said it needs input); a task assigned to the viewer that the viewer has not accepted | addressee; operator or self for a waiting turn; assignee for a task |
| 2 | `admission` | an actor waiting to be admitted to the space | space owner |
| 3 | `stuck` | a session that is `blocked` past the blocked grace (ATT-R4), `failed`, or `stale`; a changeset of the viewer's work that is `stalled` (PLAN §12, liveness); an overlap at the **conflict** tier that involves one of the viewer's agents | operator, self or delegator for blocked, failed and stalled; operator or self only for stale; operator or self for a conflict |
| 4 | `to-decide` | work that waits on a human decision about its result, where the viewer is the one to decide: a done session with changes that have not landed and no landing in flight, or a changeset gated on a review the viewer can give | operator, self, delegator or task owner; a path owner the landing rules name |
| 5 | `pooled` | an approval the viewer could give as a pooled approver, before the pool grace has passed | pooled approver |

- **ATT-R3.** The queue holds exactly the items the table defines. In particular:
  - another person's blocked, failed or stale agent is not in the viewer's queue, even when the viewer can see it, unless the viewer is its delegator, task owner (to-decide only) or covers for its operator;
  - an overlap below the conflict tier is never an item; it stays a badge (this answers Q-BRD-7);
  - an approval the viewer has already given, or one the viewer cannot count towards (a separate approver is required and the viewer proposed it), is not an item;
  - a `done` session is never an item; it may be done-unseen (§6.2).
- **ATT-R4. Blocked grace.** A session enters the queue as `stuck` for being blocked only once it has stayed `blocked` for the space's blocked grace, 60 s by default. A session that leaves `blocked` within the grace never shows as stuck and raises no notice. `failed` reported by the host enters at once. `failed` and `stale` derived from silence enter when the node derives them (board.md §4.3).
- **ATT-R5. Pool grace.** A pooled approval enters at tier 5. When it has waited for the pool grace, 10 minutes by default, with no countable approval given, it moves to tier 1. When anyone else gives the countable approval it needed, it leaves every pooled approver's queue in the node's next diff.
- **ATT-R6.** One target makes at most one item per viewer. When several kinds apply (an effect the viewer must approve whose session is also stuck), the item takes the lowest tier and lists every reason. Its row shows the first reason and "+1 reason".
- **ATT-R7.** Each item carries the roles that put it there. The face shows them as words on the item ("guard", "you operate lars-coder", "for @ada"). An item held through cover always says whom it covers.

---

## 3. The attention queue

The node keeps one attention queue per viewer, across every space the device follows, and serves it as the `Attention` view (AN-1). Faces show it, count it and jump through it. They never build it.

### 3.1 Ranking

- **ATT-R8.** The node orders the queue by these keys, in order:
  1. **tier**, lowest first (§2.2);
  2. **escalated** before not escalated (ATT-R9);
  3. **age**: the time the item entered the queue, oldest first;
  4. **blast radius**, largest first: the number of open sessions and changesets whose progress waits on this item. For an approval or a stalled changeset this counts the session itself, the changesets queued behind it in the same land lane, and the changesets that name it in `dependsOn` (PLAN §12). For a stuck session it counts the session and the changesets that wait on it. For any other item it is 1.
  5. the item key, as the tie-break.
- **ATT-R9. Escalation.** An item is escalated, and has a level, when:
  - an `approval` has an `expiresAt` within 10 minutes (level 2), or has waited 30 minutes (level 1);
  - an `ask` has waited 1 hour (level 1);
  - a `stuck` item has stayed stuck 30 minutes (level 1);
  - a `to-decide` item has waited 24 hours (level 1).

  The node computes escalation on its own clock, so faces agree. A rising level is a material change (ATT-R20).
- **ATT-R10.** The ranking MUST NOT use anything an agent wrote: not `step`, not `summary`, not an agent's own claim of urgency. It uses log facts and the node's clock only.

### 3.2 Bounds

- **ATT-R11.** The queue's count is exact at any size. Its rows are windowed: a face asks for a window of at most 50 items, from the top or around a key. The node keeps the full ranking for the first 1,000 items and reports the rest as `overflow`, per tier. A face never asks for, or holds, the whole queue.
- **ATT-R12.** Every snapshot and diff of the `Attention` view carries `totals`: the attention count, the count per tier and per kind, the done-unseen count, the muted count and the acknowledged count (AN-1). A face shows counts only from `totals`, never by counting rows.

---

## 4. The attention count and the jump

### 4.1 The frame indicator

The TUI's header row gets an **attention indicator** at its right end. It is drawn on every screen, whatever pane has focus, and only from the `Attention` view's `totals`.

```
 HAN  team-han · by issue · filter: -                       ▲ 4 to act on · ✓ 12 done · 3 muted
```

- **ATT-R13.** The indicator reads, left to right:
  - the to-act part: a mark and "N to act on", where N is the attention count; at 0 it reads "nothing to act on", in the quiet style;
  - the done part: a different mark and "N done", the done-unseen count; absent at 0;
  - the muted part, in the quiet style: "N muted", the items held back by mutes; absent at 0;
  - the freshness tag (ATT-R63) when there is one.

  The to-act mark and the done mark differ in shape, and each part carries its word, so the indicator reads the same with colour off. Counts over 999 read as "1.2k", "40k".
- **ATT-R14.** When the header is narrower than the full indicator, it gives way in this order: the muted part, then the words (leaving "▲4 ✓12"), then the done part. The to-act count is never dropped while the header has 6 columns for it. The space name and filter on the left are cut first, before any of this.
- **ATT-R15.** When the count rises, the indicator is shown with the attention emphasis for 2 s, once. Under reduced motion, or with colour and attributes off, it shows no emphasis, and the status row says "1 more to act on" instead.

### 4.2 Jumping to the next item

| Key | Command | Does |
|---|---|---|
| `Ctrl-n` | `:next` | go to the top item of the queue, or, when the previous jump was to an item, the item ranked after it |
| `Ctrl-g` | `:jump` | open the jump picker (§7) |

Both keys work in every pane, except while a text field (palette, filter, steering field, picker query) or an approval card is open. They change no existing key.

- **ATT-R16.** `:next` goes to the item's target, as follows:
  - `approval` and `pooled`: the approvals pane, with that effect's card selected and closed. It never opens the sign panel.
  - `admission`: the approvals pane, at that admission.
  - `ask` for a question or a waiting turn: the session pane, on the Stream tab, with the question's or the turn's last entry focused.
  - `ask` for an unaccepted task: the board of its space, filtered to that issue.
  - `stuck`: the session pane, on the Stream tab, with the notice band showing why.
  - `to-decide`: the session pane, on the Changes tab.

  The board, if visible, focuses the item's row as well.
- **ATT-R17.** When the queue is empty, `:next` goes to the oldest done-unseen session. When both are empty, the status row reads "Nothing to act on." and nothing moves.
- **ATT-R18.** A jump re-reads the item from the node before it moves. If the item has left the queue since the last frame, the status row reads "That is settled; moving on." and the jump goes to the next item.
- **ATT-R18a.** Jumping never acknowledges an item and never changes it. Repeated `Ctrl-n` walks the queue in rank order. The walk restarts at the top when the queue's top item changes or after 60 s without a jump.

---

## 5. Lifecycle grouping and roll-ups

### 5.1 The groups

The board gets a fourth grouping, **lifecycle**, next to issue, person and state. `t` cycles issue → person → state → lifecycle. The node computes each session's group for the viewer (AN-3); the face shows it (BRD-24).

- **ATT-R19. Groups, first match wins.**
  1. **needs-me:** the session is the target of an attention item for the viewer, or one of its effects is, and that item is not acknowledged or muted.
  2. **to-decide:** the session's work waits on a human decision about its result (as for the `to-decide` kind), and someone other than the viewer is to decide, or the viewer acknowledged it.
  3. **landing:** the session's work is on its way to its target: a changeset `gated` on checks, `queued` or `testing` in a land lane, or a landing effect (push, open or merge a PR, accept into main) approved, claimed or running.
  4. **working:** the session is open and in progress: `working`, or `waiting-approval` or `blocked` on someone other than the viewer.
  5. **idle:** everything else: `idle`, `done` with nothing left to land, landed work, and `stale` or `failed` sessions that are not the viewer's to act on.

  The groups appear in that order. A group with no sessions shows its header with "0" only in the overview (§5.3); elsewhere it is left out.
- **ATT-R20. Acknowledge (`A`).** On a needs-me row, `A` acknowledges its item for the viewer: the item leaves the attention count and the needs-me group, and the session moves to the group it would have without the item. The row keeps an "acked" mark. Acknowledging changes nothing in the space: an approval is still open, a stuck agent is still stuck, and nobody else is told.
  - The status row reads "Acknowledged. It comes back if it changes. u undoes." `u` undoes it within 10 s.
  - On an approval, the status row adds "The approval still waits; a to review."
  - An acknowledged item comes back, unacknowledged, on a **material change**: a new reason, a lower tier, a rising escalation level, a new effect in place of the old one, or the session turning `failed`. A change of `step`, age, tokens or tests is never material.
- **ATT-R21.** Acknowledging an approval is the same act as the inbox's Dismiss (approvals.md §4.2). Both write the same mark (§6.1), so an approval acknowledged on the board is dismissed in the inbox, and the reverse.

### 5.2 Roll-ups

With the lifecycle grouping, the node rolls sessions up into **buckets** inside each group, by a roll-up key (AN-3):
- **space**, the default of the overview (§5.3);
- **issue**, the default of a space's board;
- **operator** (the person behind the agents).

`T` cycles the roll-up key on the lifecycle grouping (space → issue → operator in the overview; issue → operator on a space's board). On other groupings `T` does nothing.

- **ATT-R22. Default expansion.**
  - **needs-me** is open, and lists its rows in queue order (§3.1), not by activity. It shows at most 20 rows, then "+N more to act on · Ctrl-n walks them, Enter shows all".
  - **to-decide** and **landing** are open to their buckets. Each bucket is closed and shows its count.
  - **working** and **idle** are closed: one header each, with the count per display state ("1,204 working · 38 waiting · 6 blocked").

  The viewer opens and closes groups and buckets with the board's keys (`h`, `l`, Enter). What the viewer opened is remembered per space for the life of the TUI.
- **ATT-R23. Bucket header.** A bucket header shows:
  - its key (the space's name, the issue with its title, or the person's handle);
  - its session count;
  - the count per display state, for the states present, most urgent first;
  - its done-unseen count, if any;
  - its highest overlap tier, if any.
- **ATT-R24. Bounded lists.** A group shows at most 10 buckets, ordered by the node: buckets with done-unseen sessions first, then by session count, then by most recent activity. The rest is one entry, "+N more buckets", which opens 50 more each time. An open bucket lists its rows windowed, as the board does: the TUI asks for the viewport plus one viewport of overscan, and never more than 200 rows at once.
- **ATT-R25.** Counts on group and bucket headers come only from the view's `totals` (ATT-R12). They are exact even while the bucket is closed or its rows are outside the window.

### 5.3 The overview

`:overview` opens the board across every followed space, with the lifecycle grouping and the space roll-up. `:board` returns to the current space's board. The overview is one `Board` view with no space (AN-3), never one view per space.

- **ATT-R26.** In the overview, a space's bucket header also shows that space's attention count and its mute state ("muted until 14:00"). Enter on a space bucket opens it; `S` on it switches the TUI to that space's board.
- **ATT-R27.** With the overview's defaults, the board shows at most 50 entries before the viewer opens anything: 5 group headers, at most 21 needs-me entries, and at most 11 entries each for to-decide and landing. This holds at any number of sessions and spaces.

### 5.4 Board keys added

These extend the TUI board's keys. No existing key changes meaning.

| Key | Does |
|---|---|
| `t` | cycles the grouping; lifecycle now follows state |
| `T` | on the lifecycle grouping: cycles the roll-up key |
| `A` | acknowledges the focused row's item, or, on a header, everything in it (ATT-R20, ATT-R35) |
| `u` | undoes the last acknowledgement, within 10 s |
| `M` | mutes the focused row's or bucket's space (§8.4): `1` for 1 hour, `2` until 08:00, `3` until unmuted, `Esc` cancels |
| `S` | on a space bucket of the overview: switches to that space's board |

---

## 6. Per-viewer marks

### 6.1 What they are and where they live

Marks are the viewer's own state about their attention:
- **seen:** the point up to which the viewer has seen a session (§6.2);
- **acknowledged:** an attention item the viewer has acknowledged, with the item's version at that time (ATT-R20);
- **muted:** a space muted until a time, or until unmuted (§8.4).

- **ATT-R28. Private.** Marks are stored by the node of each of the viewer's devices, keyed by the viewer's person. They are never written to a space's log, never published as a profile or status item, never sent to the home of a space that has any member other than the viewer (whatever carries them between the viewer's devices sees them only encrypted, Q-ATT-1), and never shown to any agent, the viewer's own agents included (their team tools, PLAN §1.5, see no marks). Other members cannot learn whether the viewer has seen or acknowledged anything.
- **ATT-R29. Synced across the viewer's devices.** A mark made on one device reaches the viewer's other devices within 5 s when both are online (AN-4, carrier pending Q-ATT-1). Merging:
  - seen only moves forward: the merged mark is the later point;
  - acknowledged and muted take the latest write, by the writer's time and then its device id;
  - an acknowledgement for an older version of an item than the one the device holds has no effect.

  Until the carrier exists, marks are device-local, and every rule of this spec holds on each device.
- **ATT-R30. Bounded.** A node keeps at most 200,000 marks per person. Seen marks of sessions closed more than 30 days ago are dropped first, then the oldest acknowledgements of items that have left the queue. Expired mutes are dropped at once.
- **ATT-R31.** Marks are not settings. They are not in `tui.json`, and resetting the TUI's settings leaves them.

### 6.2 Done-unseen

- **ATT-R32.** A session is **done-unseen** for the viewer when all of these hold:
  - the viewer is its operator, self, delegator, task owner or assignee (or covers for one);
  - it reached `done`, or closed, after the viewer's seen mark for it, or it has no seen mark;
  - it did not end as an attention item (a failed session is `stuck`, not done-unseen).

  Another person's finished session is never done-unseen for the viewer, so a network finishing thousands of sessions changes nothing for a viewer who has no role in them.
- **ATT-R33. Opening clears it.** Opening the session's pane, with the TUI's terminal focused, for at least 1 s, sets the seen mark to the session's latest item. So does `A` on the row. Moving the board cursor over the row, or a jump that passes through, does not. A session that is steered again and finishes again becomes done-unseen again.
- **ATT-R34. On the board.** A done-unseen row shows a mark in the row's first column, before the state glyph, and the word "new" after its state word. A needs-me row shows its own mark in that column instead ("!"), and needs-me wins when both apply. The two marks differ in shape and are drawn in every colour mode.
- **ATT-R35. In bulk.** `A` on a bucket or group header acknowledges every attention item and marks every done-unseen session in it. When that is more than 20 items, the status row first asks "Mark 212 sessions in han-dev seen? y / n", and nothing changes until `y`. `:seen all` and `:seen <space>` do the same across all spaces or one.

---

## 7. The jump picker

`Ctrl-g` or `:jump [text]` opens the jump picker over the current pane. It finds any space, issue or task, or session the viewer can see, from the node's search index (AN-5), and goes there.

```
┌ Jump ────────────────────────────────────────────────────────────────────────────┐
│ > brd fresh_                                         only: needs-me, to-decide   │
│ Spaces · 1                                                                       │
│     team-han                                        ▲ 2   412 sessions           │
│ Issues · 3 of 14                                                                 │
│ ›   #41 Board freshness                team-han     ▲ 1   3 sessions             │
│     #52 Board freshness on reconnect   han-dev            1 session              │
│ Sessions · 20 of 1,240                                                           │
│   ! lars-coder (@lars)   #41   waiting-approval            team-han   20s        │
│   * ada-bot (@ada)       #41   done                        team-han   4m         │
│ ──────────────────────────────────────────────────────────────────────────────── │
│ team-han › #41 Board freshness · 3 sessions · 1 to act on                        │
│ n needs-me  d to-decide  l landing  w working  i idle  u unseen  a all · Enter go│
└──────────────────────────────────────────────────────────────────────────────────┘
```

### 7.1 Layout and keys

The picker is an overlay of at most 90 columns and 24 rows, centred, and the whole body on a narrow terminal. Top to bottom:
1. the **query line**, with the active state filters at its right;
2. the **results**, grouped as Spaces, then Issues and tasks, then Sessions. Each group's heading shows "shown of total";
3. the **location line**: the full place of the selected result, "space › issue › session", with its agent and person for a session;
4. the **key line**.

| Key | In the query | In the results |
|---|---|---|
| printable text | edits the query | `j`, `k` move; the filter keys below toggle a filter; any other character goes back to the query and is typed there |
| `↓`, `Tab` | moves into the results | `Tab` jumps to the next group's first result |
| `↑` | – | moves up; from the first result, back to the query |
| `n` `d` `l` `w` `i` `u` | text | toggle the filter needs-me, to-decide, landing, working, idle, done-unseen |
| `a` | text | clears every filter |
| `→` | – | on a group heading or its last result: loads 50 more of that group |
| `Enter` | goes to the first result | goes to the selected result |
| `Esc` | closes the picker | back to the query |

- **ATT-R36. Filters.** The state filters are the lifecycle groups plus done-unseen. Filters of the same picker combine as OR: "needs-me, to-decide" shows results in either. A space or issue result matches a filter when any of its sessions does, and then shows the matching count. The filters are sent to the node with the query (AN-5); the picker never filters results itself.
- **ATT-R37. Results.** Each result shows its kind by its group, its name, its state or counts, its marks (the needs-me "!" and done-unseen "*" marks of ATT-R34) and its space. The characters that matched the query are set apart by an attribute and by an underline, never by colour alone. All result text is foreign text, sanitised as single-line text (TSV-R38).
- **ATT-R38. Location.** The location line always shows where the selected result sits: its space, then its issue or task when it has one, then the session with its agent and person. It updates on every move, from the result's own fields, without a query.
- **ATT-R39. Going.** Enter on a space opens its board. Enter on an issue or task opens its space's board filtered to it (`issue:`). Enter on a session opens its session pane. The picker closes, and `Esc` from the place it opened returns to where the picker was opened.
- **ATT-R40. Empty query.** With an empty query, the picker shows, in this order: the top 5 items of the attention queue, the 5 sessions the viewer opened most recently, and the 5 spaces with the most recent activity. These come from the node, like every other result.

### 7.2 Search from the node

- **ATT-R41. Type-ahead.** The picker sends the query to the node 80 ms after the last key, or at once on Enter. At most one search is in flight. A key pressed while one is in flight replaces the pending query, and the picker sends only the latest text when the flight ends. A result set for an older text than the query line shows is never drawn.
- **ATT-R42. Limits.** A query is at most 128 characters; more are not typed, and the status row says so. The first page has at most 20 results per group; `→` loads pages of 50. The picker holds at most 500 results.
- **ATT-R43. Matching** is done by the node (AN-5). It matches names, handles, issue references and titles, case-insensitively, as a prefix, then at word starts, then as a substring, then as a subsequence (fuzzy). Within a match quality, results rank by relevance to the viewer (in the queue, then the viewer's own work, then opened recently), then by recent activity. The node searches only what the viewer may see: a session the viewer cannot watch is never a result.

---

## 8. Notices

The node decides when the viewer should be told something, and how loudly (AN-7). Faces present notices; they never decide them. One rule set then holds for the TUI, the GUI and a device's notifier alike.

### 8.1 Cues

| Cue | For | TUI presentation | Never |
|---|---|---|---|
| **act** | an item entering the queue at tier 1 to 4, or an escalation | the indicator's emphasis (ATT-R15); the status row names the item; one terminal bell when `bell` is on | for a done session |
| **done** | done-unseen sessions, as a digest | the done count rises; the status row reads the digest ("12 sessions finished in han-dev") | a bell; the attention emphasis |
| **error** | the viewer's agent failed; a landing of the viewer's work failed or was rejected; an outage and its recovery (§8.5) | as act, with the error words | more than once per outage |

- **ATT-R44. Distinct.** The act cue and the done cue differ in words, in mark and in sound: a done cue never rings the bell, and the GUI and OS notifier use a different title and category for each. Colour is never the only difference.
- **ATT-R45. Content.** A notice carries identifiers only: the kind, the agent, the issue, the space, and counts (approvals.md APR-18, SR-100). It never carries agent text, plan contents, paths or summaries.
- **ATT-R46.** The TUI writes the bell as the one bell control character, never another escape sequence, and only when the `bell` setting is true (§10).

### 8.2 Silence for what is in focus

- **ATT-R47.** No cue is presented for an item whose target is on screen in a focused face: its session pane is open, or its card or admission is visible in the approvals pane, or (for a done digest) all its sessions' rows are visible on the board. The count and the marks still update. Focus on screen does not mark anything seen (ATT-R33 alone does).
- **ATT-R48.** The TUI knows whether its terminal is focused from the terminal's focus reports, which it turns on at start. A terminal that sends none counts as focused while a key was pressed in the last 60 s, and unfocused after.
- **ATT-R49.** The face tells the node what is on screen and whether it is focused (AN-7, `FocusReport`), at most twice a second, and only when that changes. The node then holds back the cue on every device of the viewer (pending Q-ATT-6; until then each device holds back its own).

### 8.3 Budget and digests

- **ATT-R50. Coalescing.** The node gathers notices for 10 s after the first one of a burst. When the window closes, it sends one notice: the item itself when there is one, otherwise a digest ("5 decisions are waiting for you", "3 of your agents are stuck in team-han", "12 sessions finished in han-dev"). A digest is per kind, and per space when one space holds them all; otherwise it names the number of spaces ("212 sessions finished in 6 spaces").
- **ATT-R51. Rate limits per viewer**, across all the viewer's devices:
  - act and error cues: at most 1 per 10 s, 6 a minute and 30 an hour;
  - done cues: at most 1 per 5 minutes per space, and 1 per 2 minutes in all.

  Notices over a limit wait for the next allowed slot and merge into its digest. The 30th act or error cue of an hour is always the pause notice, "N items wait for you. Notices are paused for 20 minutes.", in place of what it would have been. After it, only error cues are presented until the pause ends; they alone may pass the hourly limit, at most 1 a minute. The count and the indicator keep updating throughout.
- **ATT-R52. Escalation.** An item that is already in the viewer's queue gets an act cue again when its escalation level rises (ATT-R9), once per level. Escalated items take the budget's slots before new ones. Nothing outside the viewer's queue is ever escalated for the viewer.
- **ATT-R53. Never per event.** A notice is about an item entering the queue, an escalation, or a digest. Item updates, status items, steps, tool calls, state changes inside one group and overlaps below the conflict tier never cause a notice.

### 8.4 Mute

- **ATT-R54.** `M` on a row or bucket, or `:mute <space> [1h|today|on]`, mutes a space for 1 hour, until 08:00 local time, or until unmuted. `:unmute <space>` ends it. The overview's space header shows "muted until …".
- **ATT-R55. Errors pass.** While a space is muted, its items are left out of the attention count and counted as muted (ATT-R13), and they raise no cue, **except**:
  - error cues (§8.1), which pass;
  - one **quiet cue** for an approval that only this viewer can give (a guard, or a direct approver, never pooled) when it reaches escalation level 2 (its `expiresAt` is within 10 minutes, ATT-R9). A quiet cue is a status-row line ("han-dev is muted: your approval of git.push expires at 14:30") and a notification without sound. It rings no bell and shows no emphasis, and it does not add the item to the count. It is sent once per item.

  Muted items stay in the queue's rows, ranked as before, and in the approvals pane, marked "muted".
- **ATT-R56.** Mute is a mark (§6.1): private, synced, never told to the space. Muting a space does not change what its members see, and it does not stop the viewer's own agents.

### 8.5 Outages

- **ATT-R57. Space home.** When a space's home stays unreachable for 30 s, the node sends one error notice: "team-han's home is unreachable since 12:03". While it lasts, the stale and failed states it causes for other members' sessions raise no notice. When the home is reachable again, the node sends one recovery notice: "team-han is back · 4 to act on" (the count of that space's items at that time).
- **ATT-R58. Host.** When 3 or more sessions of one host turn `stale` or `failed` within 60 s, the node treats it as one host outage: one error notice, "boat-1: 212 sessions unreachable", in place of a notice per session, and one recovery notice when the host is heard from again. The sessions are still items of their operators' queues (ATT-R3); only the notices are merged.
- **ATT-R59. Node.** When the TUI loses its node, the frame shows "node unreachable" in place of the indicator, once, and raises one bell if `bell` is on. When the node returns, the indicator comes back with a fresh snapshot; there is no bell.

---

## 9. Freshness and speed

- **ATT-R60. Targets.**
  - The node folds an incoming event into the `Attention` and `Board` views and sends the diff within 250 ms at the 95th percentile and 1 s at the 99th, at the scale of this spec's preamble (AN-9).
  - The TUI draws a received diff in its next frame, within 50 ms.
  - The indicator's numbers change at most 4 times a second; a burst between two changes shows its end state.
- **ATT-R61. Coalescing.** When events arrive faster than they can be drawn:
  - the node merges the diffs of one view within a tick of 100 ms, keeping only the latest value of each row and the latest `totals`;
  - roll-up counts of closed buckets and groups change at most twice a second;
  - the TUI draws at most 20 frames a second from view updates (TSV-R49), and a key press is drawn within 50 ms whatever the stream does.
- **ATT-R62. Overload.** When a view's backlog passes its bound, the node ends it with `Resync` (data.md Q-DAT-8). The TUI keeps what it shows, marks the indicator "resyncing", and subscribes again. The jump still works on the last queue it holds, with ATT-R18's re-check.
- **ATT-R63. Freshness tag.** The node sends `lag_ms` (how far its folding is behind what it has received) and a heartbeat at least once a second on the `Attention` view (AN-9). The indicator shows a tag:
  - "stale 4s", when the view has sent nothing, not even a heartbeat, for 3 s, or when `lag_ms` is over 2,000 (then the tag shows the lag);
  - "resyncing", while it subscribes again.

  While the tag shows, the counts are drawn in the quiet style with a "~" before them ("~4 to act on"). The tag goes away with the first heartbeat or diff that is fresh again.

---

## 10. Settings

This spec adds one key to `tui.json` (tui-session.md §8.1):

| Key | Type and values | Default | Set by |
|---|---|---|---|
| `bell` | bool | `true` | `:bell on`, `:bell off` |

Mutes, acknowledgements and seen marks are marks, not settings (ATT-R31). The blocked grace, the pool grace and the escalation thresholds are space settings that the node reads (AN-10); the TUI has no setting for them.

New palette commands, each with the effect of its key or rule: `:next`, `:jump [text]`, `:overview`, `:board`, `:ack`, `:unack`, `:seen all|<space>`, `:mute <space> [1h|today|on]`, `:unmute <space>`, `:bell on|off`. `:help` lists the new keys in an "Attention" section.

---

## 11. Needs from the node

The TUI builds against these as provisional types until they land (data.md DAT-3), and runs the criteria against the fake node.

| ID | Need | Needed by |
|---|---|---|
| AN-1 | `Attention{window, kinds?, include_muted?}`: the viewer's ranked queue across all followed spaces. Rows: `{key, kind, tier, reasons[], roles[], target{space, session?, effect?, task?, actor?}, since_ms, escalation, blast, version, muted, acked}`. Snapshots and diffs carry `totals {count, per_tier, per_kind, done_unseen, muted, acked, overflow}`, `now_ms` and `lag_ms` | §3, §4, ATT-R12 |
| AN-2 | roles and reasons on `BoardRow`: `roles[]`, `attention?{kind, tier, version}`, `done_unseen`, `acked`, `muted`, in place of the single `needs_me` flag (which stays, equal to "has an unacknowledged, unmuted item") | ATT-R7, ATT-R34, board.md §4.5 |
| AN-3 | `Board` with `group: lifecycle`, `rollup: space | issue | operator`, `space` optional (absent: every followed space), and `expanded[]` for buckets that are closed by default; group and bucket entries with their counts per display state, done-unseen and attention counts, in `totals`; the bucket order of ATT-R24 | §5 |
| AN-4 | marks: commands `MarkSeen{session, upto?}`, `Ack{item, version}`, `Unack{item}`, `Mute{space, until?}`, `Unmute{space}`, and a `Marks` view; stored in `node.db`, private (ATT-R28), synced among the person's devices (ATT-R29, carrier pending Q-ATT-1), bounded (ATT-R30) | §6 |
| AN-5 | `Search{text, groups?, states?, window}`: results grouped as spaces, issues and tasks, sessions, each group with its total; the matching and ranking of ATT-R43; only what the viewer may see. Target: the first page within 100 ms at the 95th percentile over 200,000 sessions, 50,000 tasks and 500 spaces; a new session searchable within 2 s | §7 |
| AN-6 | suggested replies: a `suggestions` item on a turn, `{turn, options[{n, text}], derived}` (from a host's `han.item/1`, or derived by the node from a trailing numbered list in the turn's last agent message, with `derived: true`); `SessionInput` takes `choice?{item, n}`; the `input` item and the Record row carry it | tui-session.md §5.4 |
| AN-7 | notices: a `Notices` view of `{id, cue: act | done | error | recovery, title, counts, target?, digest_of?}` after the rules of §8 (grace, budget, digest, escalation, mute, outages), and a `FocusReport{targets[], focused}` command | §8 |
| AN-8 | the facts behind `to-decide` and `landing`: a session's changeset state (`open`, `gated`, `queued`, `testing`, `landed`, `rejected`, `stalled`, PLAN §12) and its landing effects; until `han.land/1` exists, the node uses landing effects alone | ATT-R19, `to-decide` and `stuck` kinds |
| AN-9 | `lag_ms` and a heartbeat at least every second on `Attention` and `Board`; the diff latency of ATT-R60; per-view coalescing of ATT-R61 | §9 |
| AN-10 | per-space settings for the blocked grace, the pool grace and the escalation thresholds (with Q-BRD-1) | ATT-R4, ATT-R5, ATT-R9 |
| AN-11 | a host identity on board rows and session facts (which machine runs it), so the node can merge a host outage | ATT-R58 |
| AN-12 | questions to people and agents (`Ask`, PLAN §5, "next") as items with an addressee and an answered state | the `ask` kind |

---

## 12. Acceptance criteria

Each criterion runs on ratatui's TestBackend through the TUI's test harness (`apps/han-tui/src/testing.rs`), with a fake node that serves the named fixture and records every command and query. "Screen" means the rendered buffer as text; "colour off" means `NO_COLOR=1`. Each criterion's pending node API items are listed under the table.

Fixtures:
- `network`: 5,000 open sessions across 40 active spaces (of 500 followed), 1,000 people, 20,000 agents. The viewer @lars operates 40 agents in 6 spaces. The queue holds 37 items: 3 guard approvals, 2 direct approvals, 4 pooled approvals, 5 asks, 1 admission, 12 stuck, 10 to-decide. 18 sessions are done-unseen.
- `roles`: one space where @lars is a member and owner; @ada's agent ada-bot is blocked (no role for @lars); @lars delegated a task to bo-runner, which is blocked; lars-coder (operated by @lars) is stale.
- `storm`: `network` plus a fake node that sends 20,000 item and status events a second.

| ID | Given | When | Then |
|---|---|---|---|
| ATT-1 | `network`, 120 × 30 | the TUI starts | the header's right end reads "37 to act on · 18 done"; the act and done marks differ in shape; the TUI made one `Attention` subscription and no per-space attention query |
| ATT-2 | `roles` | the TUI starts | the queue has 2 items: bo-runner stuck (role delegator) and lars-coder stuck (role operator); ada-bot is not in it; being owner adds nothing but admissions |
| ATT-3 | `roles`, ada-bot blocked | the fake node makes @lars ada-bot's delegator | ada-bot enters the queue as `stuck`, with "delegator" among its roles, in the next diff |
| ATT-4 | an agent of @lars turns `blocked`, virtual clock | 30 s pass, then it turns `working` | it never entered the queue; no cue; the count never changed |
| ATT-5 | the same agent | it stays `blocked` for 61 s | it enters the queue as `stuck` at 60 s, with one act cue |
| ATT-6 | a pooled approval for @lars | 9 minutes pass, then 2 more | it is ranked in tier 5 for the first 10 minutes, then moves to tier 1, with one act cue at the move |
| ATT-7 | ATT-6 at tier 5 | another approver's approval is recorded | the item leaves @lars's queue in the next diff; the count drops by 1 |
| ATT-8 | a queue with a guard approval 5 minutes old, a direct approval 20 minutes old, and an ask 2 hours old | the queue is shown | the order is the 20-minute approval, the 5-minute approval, then the ask (tier before age) |
| ATT-9 | two approvals of the same age, one with 4 changesets queued behind it in its lane | the queue is shown | that one ranks first (blast radius) |
| ATT-10 | an approval with `expiresAt` 9 minutes ahead, below an older approval | the queue is shown | it ranks first (escalation level 2); it received one act cue at its escalation |
| ATT-11 | a hunk-tier overlap between two of @lars's agents | the queue is shown | no item exists for it; the board row keeps its badge; at the conflict tier an item appears |
| ATT-12 | `network`, focus on the board | `Ctrl-n` | the approvals pane opens with the top-ranked approval's card selected and closed; no `EffectApprove` was sent; `Ctrl-n` again selects the second item |
| ATT-13 | the top item is a `stuck` session | `Ctrl-n` | the session pane opens on the Stream tab; its notice band shows the reason |
| ATT-14 | the queue is empty and 2 sessions are done-unseen | `Ctrl-n` | the older done-unseen session's pane opens |
| ATT-15 | the queue and done-unseen are empty | `Ctrl-n` | the status row reads "Nothing to act on."; no pane changes |
| ATT-16 | the top item left the queue on the node, but the TUI has not drawn the diff yet | `Ctrl-n` | the TUI re-reads it; the status row reads "That is settled; moving on."; the jump goes to the next item |
| ATT-17 | the steering field is open | `Ctrl-n`, `Ctrl-g` | both are handled by the field; no jump and no picker |
| ATT-18 | 40 × 12, `network` | the screen is drawn | the header shows "▲37" at least; the muted part and the words are gone first |
| ATT-19 | colour off, `network` | the header is dumped | it carries the same marks and words as with colour on |
| ATT-20 | `network`, overview open, defaults | the screen is drawn | it shows the 5 groups in order needs-me, to-decide, landing, working, idle; at most 50 entries in all; working and idle are one header each with per-state counts; no session row of working or idle is drawn |
| ATT-21 | ATT-20 | the fake node is asked | the TUI made one `Board` subscription with no space, `group: lifecycle` and `rollup: space`; its window limit is at most 200 |
| ATT-22 | ATT-20 | `l` on the working group, then Enter on a space bucket of 1,204 sessions | the bucket's rows load as a window around the viewport; no more than 200 rows are held; the bucket header shows 1,204 from `totals` |
| ATT-23 | the to-decide group with 37 buckets | the screen is drawn | 10 buckets show, then "+27 more buckets"; Enter on it shows the rest |
| ATT-24 | a needs-me row for a stuck agent | `A` | the row moves to its non-needs-me group with the "acked" mark; the count drops by 1; the fake node received one `Ack` with the item's version; `u` within 10 s sends `Unack` and restores it |
| ATT-25 | ATT-24 acknowledged | the agent's `step` changes 50 times, then it turns `failed` | it stays acknowledged through the step changes; when it turns failed it is back in needs-me, unacknowledged |
| ATT-26 | an approval acknowledged with `A` on the board | the approvals pane is opened | the card is under Dismissed (the same mark) |
| ATT-27 | `A` on a needs-me approval | the status row is read | it reads "Acknowledged. It comes back if it changes. u undoes." and "The approval still waits; a to review." |
| ATT-28 | a session of lars-coder turns `done` | the board is drawn | its row shows the done-unseen mark in its first column and "new"; the done count rises by 1 |
| ATT-29 | ATT-28 | the cursor passes over the row, and a jump passes through it | it is still done-unseen |
| ATT-30 | ATT-28 | Enter opens its pane, and 1 s passes with the terminal focused | the fake node received `MarkSeen` for that session; the mark and "new" are gone; the done count drops by 1 |
| ATT-31 | a session of @ada's agent, where @lars has no role, turns `done` | the board is drawn | no done-unseen mark; the done count is unchanged |
| ATT-32 | a bucket with 212 done-unseen sessions | `A` on its header | the status row asks "Mark 212 sessions in han-dev seen? y / n"; nothing is sent until `y`; then one bulk mark is sent |
| ATT-33 | the fake node delivers a mark made on another of @lars's devices | the screen is drawn | the session is no longer done-unseen, with no key pressed on this device |
| ATT-34 | any fixture | the TUI saves `tui.json` | the file holds no mark, acknowledgement or mute |
| ATT-34a | the fake node records every entry it signs and every row it serves to agent connections (host endpoints and agent tools) | the user sends `Ack`, `Mute` and `MarkSeen` | none of the three appears in any signed entry, in any status or profile item, or in any row or answer served to an agent connection, including `team.sessions` and `team.read` for @lars's own agents |
| ATT-35 | `network` | `Ctrl-g`, type "brd fr" | the fake node receives one `Search` with text "brd fr" 80 ms after the last key, not one per key; groups show "shown of total" headings |
| ATT-36 | ATT-35, a search in flight | 3 more keys are typed before it answers | when it answers, at most one more `Search` is sent, with the full text; the answer to the older text is not drawn |
| ATT-37 | ATT-35, the results focused | `n`, then `d` | the next `Search` has states needs-me and to-decide; the query line shows both filters; `a` clears them |
| ATT-38 | ATT-35, the results focused | `j` twice | the location line shows the selected session's space, issue, agent and person; no query was sent for it |
| ATT-39 | ATT-38 | Enter | the session's pane opens and the picker closes |
| ATT-40 | the picker with an empty query | it opens | it shows the queue's top 5, then the 5 sessions opened most recently, then 5 spaces, all from the node |
| ATT-41 | a result whose title holds an escape sequence and U+202E | the picker is drawn | no ESC reaches the backend; U+202E shows as `<U+202E>`; matched characters are underlined |
| ATT-42 | the TUI unfocused (no focus report for 60 s and no key) | an approval enters the queue | one act cue: the indicator's emphasis, the status row names it, and one bell control character is written |
| ATT-43 | the TUI focused, with that session's pane open | an item for it enters the queue | no bell, no emphasis; the count rises; the fake node received a `FocusReport` naming the session |
| ATT-44 | `bell` false | an act cue | no bell control character is written |
| ATT-45 | 12 of @lars's sessions in han-dev finish within 20 s | the notices arrive | one done notice, "12 sessions finished in han-dev"; no bell; no act emphasis |
| ATT-46 | 40 items enter the queue within 1 minute | the minute passes | at most 6 act cues; the last reads a digest with the remaining count; the indicator shows the exact count throughout |
| ATT-47 | 29 act cues have been presented within 1 hour | a 30th would be presented | the 30th cue is the notice "N items wait for you. Notices are paused for 20 minutes." is shown once; then only error cues for 20 minutes |
| ATT-48 | han-dev muted for 1 hour | an approval and a `failed` agent of @lars arrive in han-dev | the approval adds to "muted", not to the count, and raises no cue; the failure raises an error cue and adds to the count |
| ATT-48a | ATT-48, @lars is the approval's only possible approver (its guard) | its `expiresAt` comes within 10 minutes | one quiet cue: the status row names the approval and its expiry; no bell control character is written; no emphasis; the count is unchanged; 5 minutes later there is no second cue |
| ATT-48b | ATT-48, the approval is pooled | its `expiresAt` comes within 10 minutes | no cue at all |
| ATT-49 | `M` on a han-dev row, then `1` | the overview is drawn | the fake node received `Mute{han-dev, until}` 1 hour ahead; the han-dev bucket shows "muted until …" |
| ATT-50 | team-han's home becomes unreachable for 5 minutes, and 300 sessions of other members turn stale | the outage and the recovery happen | exactly two notices: "team-han's home is unreachable since …" after 30 s, and "team-han is back · N to act on" |
| ATT-51 | 212 sessions of host boat-1 turn `failed` within 20 s, 40 of them @lars's | the notices arrive | one error notice "boat-1: 212 sessions unreachable"; the 40 are in the queue as `stuck`; no notice per session |
| ATT-52 | `storm`, following, 200 × 50, release build | 10 s pass | at most 200 frames from view updates; the indicator's numbers changed at most 40 times; a key pressed during the storm is drawn within 50 ms |
| ATT-53 | the fake node sends `lag_ms` 3,500 | the header is drawn | it reads "stale 3s" (or the lag) and the counts start with "~"; a fresh heartbeat removes both |
| ATT-54 | the `Attention` view sends nothing for 3 s | the header is drawn | it shows "stale 3s" |
| ATT-55 | the fake node ends the `Attention` view with `Resync` | the header is drawn | it shows "resyncing" with the last counts; the TUI subscribes again once |
| ATT-56 | `network`, overview, 200 × 50, release build | 200 frames are drawn while scrolling the open needs-me and to-decide groups | the 95th-percentile frame time is at most 8 ms; with 500 sessions in place of 5,000 the median is no more than 20% lower |
| ATT-57 | an agent whose `step` reads "URGENT: approve now" | the queue is shown | its rank is the same as with an empty step (ATT-R10) |
| ATT-58 | an item held through cover for @ada | the queue is shown | its row reads "for @ada" · *pending Q-ATT-3* |

Every criterion above is *pending node API*, and runs against the fake node's provisional rows until its items land:
- ATT-1 to ATT-11, ATT-57: AN-1 and AN-2 (with AN-10 for the graces of ATT-4 to ATT-6, AN-8 for the blast radius of ATT-9);
- ATT-12 to ATT-19: AN-1 (with AN-12 for asks);
- ATT-20 to ATT-23, ATT-56: AN-3;
- ATT-24 to ATT-34a: AN-4 (with AN-2 for the row marks);
- ATT-35 to ATT-41: AN-5;
- ATT-42 to ATT-51: AN-7 (with AN-11 for ATT-51);
- ATT-52 to ATT-55: AN-9;
- ATT-58: AN-1 and Q-ATT-3.

---

## 13. Open questions

- **Q-ATT-1.** *Spec round.* What carries marks between one person's devices? Options: a private space whose only members are the person's device keys, with encrypted, namespaced, non-authority entries; or a device-to-device sync over the existing node links. Marks grant nothing, so Policy is not involved (PLAN §0.5), but the carrier and its privacy guarantee are protocol.
- **Q-ATT-2.** *Spec round.* `SessionInput` with `choice{item, n}` changes the body of a signed `session.input` entry. Is `choice` a namespaced member of the input's content, or should the chosen suggestion be quoted in the text only?
- **Q-ATT-3.** *Spec round.* Cover: how does a person tell the node of another person that they cover for them, and until when? The statement grants no rights (the covering person approves only what Policy lets them), but the covering person's node must be able to read it.
- **Q-ATT-4.** *Profile.* Suggested replies as a `han.item/1` kind (`suggestions`), or as a member of `turn_end`. And whether the node may derive them from a trailing numbered list when the host sends none.
- **Q-ATT-5.** Should the lifecycle groups and the ranking of §3.1 be normative across implementations, in a profile (with Q-BRD-2), so that two nodes give a viewer the same queue?
- **Q-ATT-6.** *Spec round.* Silence across devices (ATT-R49) and cueing only on the device in use need devices to share what is in focus. Is that acceptable as private, person-only data, or does focus stay device-local?
- **Q-ATT-7.** Where the per-space thresholds live (blocked grace, pool grace, escalation), with Q-BRD-1.
- **Q-ATT-8.** *Decided:* in a muted space, an approval that only the viewer can give gets one quiet cue when it is about to expire (ATT-R55); pooled approvals get none. Still open: whether the space's policy may set a different window than 10 minutes.

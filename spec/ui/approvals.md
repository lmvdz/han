# UI spec: the approvals inbox

Status: draft, Team A (clean room, PLAN.md §8). Behaviour and appearance only. Read with [data.md](data.md) and [shell.md](shell.md).

Gated actions (a push to a protected branch, an outside HTTP write, accepting timeline work into main, opening or merging a PR) wait for signed approvals (PLAN §1.7, §6.2). The inbox is where a person sees every decision that waits on them, in every space they follow, and signs or cancels it. It is HAN's own surface. The rules it must honour come from architecture.md P-04, P-07, P-08 and P-15, and SR-98 and SR-100.

Route: `/inbox`, with optional search parameters `space`, `session` and `effect` (shell.md §2).

---

## 1. Terms

| Term | Meaning |
|---|---|
| **Effect** | an `effect.propose` entry and its state: `open`, `claimed`, `unknown`, `succeeded`, `failed`, `cancelled`, and the flag `cancelRequested` (P-07…P-10). |
| **Plan** | the proposal's content `{action, target, args?, argsBlob?}`. Its hash `sha256(JCS(plan))` is the `planDigest` that an approval signs. |
| **Rule** | the first `policy.approvals` entry that matches the action: its `approvers` (roles or actor ids), plus `separateApprover` (P-08). |
| **Guard** | the confirmed operator of a guarded agent in the effect's `guardedFor`, or of the proposer or executor if guarded. Each guard must approve, in addition to the rule (P-15, E3). |
| **Countable approval** | an approval that can still be consumed: unconsumed, unexpired, by an author who satisfies the rule now (P-08). |
| **I can approve** | the local user satisfies the rule (or is a required guard), is not limited, holds a role, and (with `separateApprover`) is not the proposer. The node derives this as `can_approve`, and the rule's approvers as `approvers` (PR #35, data.md §4.4). |
| **I can cancel** | the local user is the proposer, the author of an unexpired approval on the effect, one of its guards, or an owner, and holds a role (P-04). The node derives this as `can_cancel` (PR #35). |

---

## 2. Layout

```
┌──────────────────────────────────────────────────────────────────────────────────────────┐
│ Inbox                         [All spaces ▾]  [⌕ filter]                    ⚙ Notifications │
├──────────────────────────────────────────────────────────────────────────────────────────┤
│ YOUR DECISIONS · 2                                                                       │
│ ┌──────────────────────────────────────────────────────────────────────────────────────┐ │
│ │ ◐ git.push  han/41-lars-coder → github:lmvdz/han              team-han · 3m · open   │ │
│ │   proposed by lars-coder (operated by @lars) · session #41 · cost $0.50 of task $3   │ │
│ │   needs: 1 owner (0/1) · guard @lars (pending)     you can approve as: owner, guard   │ │
│ │                                              [Review & approve]   [Cancel effect]     │ │
│ └──────────────────────────────────────────────────────────────────────────────────────┘ │
│ ┌ (collapsed card) open_pr  #41 → main  team-han · 1m ──────────────────────────────── ┐ │
│                                                                                          │
│ ADMISSIONS · 1           (owners only)                                                   │
│ │ @cy:c.example joined team-han with a ticket · 5m        [Admit as member ▾] [Ignore]  │ │
│                                                                                          │
│ WAITING ON OTHERS · 1                                                                    │
│ │ http.post  https://hooks.example/deploy   you approved 10m ago · needs @ana (guard)  │ │
│                                                                                          │
│ RECENT · 7 days                                                                          │
│ │ ✓ git.push  succeeded · executor @git · 1h      ✕ open_pr cancelled by @ada · 3h     │ │
└──────────────────────────────────────────────────────────────────────────────────────────┘
```

Regions:
1. **Toolbar:** a space selector (All spaces, or one), a filter box (free text over action, target, agent, issue), and a link to the notification settings (§6.5).
2. **Sections**, in this fixed order, each with a heading that carries its count:
   - **Your decisions:** open effects that I can approve and haven't yet approved with a countable approval. Oldest first, because the oldest has waited longest; an effect whose `expiresAt` is within 1 hour sorts to the top.
   - **Admissions** (only when the local user is an owner of a space with pending joins): `join` signals from N-03 (§5).
   - **Waiting on others:** open effects that I approved, or that my own agents proposed, which still need someone else's approval. Newest first.
   - **Recent:** effects that reached a final phase, or were claimed, in the last 7 days and that I was involved in (as proposer's operator, approver, guard or canceller). Newest first. Collapsed to one line per effect.
   - **Dismissed** (shown only when it has entries): effects I hid (§4.2).
3. **Cards.** In Your decisions the first card is expanded and the others are collapsed. Only one card is expanded at a time. Other sections show one-line rows that expand on activation.

---

## 3. The approval card

### 3.1 Full card (expanded)

```
┌ git.push ─────────────────────────────────────────────────────────── team-han · open ┐
│ WHY GATED   push to protected branch? no · push of a han/* branch (space rule)        │
│                                                                                       │
│ PLAN                                                         [Rendered | Raw JSON]    │
│   repository   github:lmvdz/han                                                       │
│   ref          refs/heads/han/41-lars-coder                                           │
│   move         a1b2c3d4 → 9e8f7a6b   (3 commits)            [View changes]            │
│                                                                                       │
│ DIGEST      sha256 9f3c 41aa … 07be d2e1                   ✓ plan matches digest      │
│                                                                                       │
│ ┌ written by lars-coder (agent) ─────────────────────────────────────────────────────┐ │
│ │ "Push the freshness fix so CI runs."                                               │ │
│ └────────────────────────────────────────────────────────────────────────────────────┘ │
│                                                                                       │
│ WHO         proposed by lars-coder · operator @lars · executor @git                   │
│ CONTEXT     session (working) · task "Board freshness" #41 · overlaps: hunk with ada-bot│
│ COST        $0.50, from task budget ($2.50 left)        EXPIRES  in 2h (14:30)        │
│                                                                                       │
│ APPROVALS   rule: git.push → [owner] · separate approver: yes                         │
│   needed    1 rule approval  ·  guard @lars                                           │
│   have      — none yet —                                                              │
│   you       can approve as owner and as guard of lars-coder (one approval covers both) │
│                                                                                       │
│                          [Cancel effect]                [Review & sign approval  a]   │
└───────────────────────────────────────────────────────────────────────────────────────┘
```

Each element, and where it comes from (all through `view(Approvals{actor})`, data.md §4.4, built by the node from the log):

| Element | Shows | Source |
|---|---|---|
| title | the action (`git.push`, `open_pr`, `http.post`, `file.write`, or any other action, verbatim) | `effect.propose.action` |
| space, phase | the space's name; the phase as a word; "cancel requested" when the flag is set | the effect's state |
| why gated | the gate that applies, in words ("push to a protected branch", "outside HTTP write", "accept work into main", "open a pull request", or "space rule for <action>") | the node's `why_gated` (PR #35) |
| plan, rendered | per action (§3.3) | the plan content, fetched and hash-checked by the node |
| plan, raw | the plan as formatted JSON, read-only, monospaced, with its keys in JCS order | the same |
| digest | the full `planDigest` in groups of 4 hex characters. Collapsed, it shows the first 8 and last 8 characters; "Copy" copies all 64. | `effect.propose.planDigest` |
| digest check | "✓ plan matches digest" or, in the danger tone, "✕ plan does not match its digest: do not approve" | the node's verification (§3.4) |
| agent text | any text the proposing agent wrote that is not part of the plan (a summary, a PR title shown for context), in a box labelled "written by <agent> (agent)" | the stream item that announced the proposal, or the plan's own free-text fields, labelled the same way (SR-98) |
| who | the proposer, its operator, the executor | the entry's author and profile facts |
| context | the proposing session and its display state, the task and its issue, and any overlaps the session is in now | joined by the node |
| cost | the cost, and where it is charged (task or space) with what is left there | `cost`, the task and budget state |
| expires | "in 2h (14:30)", or "no expiry" | `expiresAt` |
| approvals: rule | the action's rule, its approvers and `separateApprover` | the policy |
| approvals: needed | how many rule approvals are still needed (with this spec's rules, one; Q-APR-2) and each required guard | derived by the node |
| approvals: have | each countable approval: author, time, expiry. Consumed approvals are shown as "used by attempt 2". Dropped approvals are not shown. | the effect's approvals |
| approvals: you | whether and as what I can approve, or why not ("you proposed this and the space requires a separate approver"; "you are limited in this space"; "your role is guest; the rule needs owner") | derived by the node |

### 3.2 Compact form

The compact form is used for collapsed cards in Your decisions, for the decision bar in the session pane (session.md §6), and for rows in the other sections. It is one or two lines:
- the action;
- the target in short form;
- the space;
- the age;
- the phase;
- for Your decisions, "needs: …" and "you can approve as: …".

The compact form never offers a direct "approve". It offers "Review & approve", which expands the full card.

### 3.3 Rendering plans by action

The core fixes only `{action, target, args?, argsBlob?}`. Renderers are a lookup by action, and any action without a renderer shows the raw JSON with the rendered tab disabled.

| Action | Rendered as |
|---|---|
| `git.push` | repository; ref; "move <from-sha> → <to-sha>", with the number of commits when known; "force" in the danger tone when the move is not a fast-forward; "View changes" opens the diff between the two shas (diffs.md §6) |
| `open_pr` | repository; base ← head; the title and body **as agent text** (§3.1 box); draft or ready |
| `http.post` / `http_request` | method; the **full URL**, never shortened, with the host in bold; the JSON body formatted, collapsed beyond 40 lines |
| `file.write` | the target path; the content, as a text preview up to 200 lines, or "binary, 12 KiB" |
| accept work into main | the session's timeline and main; the number of files and conflict hunks; "View merge" opens the merge preview (diffs.md §6) |
| any `argsBlob` | the blob's hash, size and type; "Inspect" fetches the blob through the node and shows it as text if it is UTF-8, otherwise as hex for its first 4 KiB |

- **APR-1.** Every value from the plan is rendered as text (SR-97), and URLs are shown in full. Anything an agent wrote that is not a plan field is shown only inside the "written by … (agent)" box, so it can never pass for the plan (SR-98).

### 3.4 What must be true before approving

- **APR-2.** "Review & sign approval" is enabled only when **all** of these hold:
  - the node reports the plan fetched and its digest verified (P-08);
  - the full card, including the rendered or raw plan and the digest, has been on screen, meaning the card was expanded, not just the compact form;
  - the node says I can approve;
  - the effect is `open` or `claimed`.

  When it is disabled, its tooltip and `aria-describedby` give the first reason that fails.
- **APR-3.** The approval the GUI asks the node to sign names the `planDigest` shown on the card. If the effect's digest changes while the card is open (a new proposal can't change it, but a stale cache could), the GUI MUST disable approving and show "This proposal changed. Review it again." (P-08, SR-98).
- **APR-4.** A plan that fails its digest check shows the danger message, and approving it is impossible. The card still offers "Cancel effect" when I may cancel.

---

## 4. Flows

### 4.1 Approve

1. The user activates "Review & sign approval", or presses `a` with the card focused.
2. A **sign panel** opens inside the card, below the approvals block, and takes focus. It shows:
   - "You are signing an approval of:" followed by the action, the target, and the **full digest** in groups of 4;
   - the identity and device that will sign ("as @lars on this device (laptop)");
   - **Expiry:** No expiry, 1 hour, 24 hours (the default), or Custom. The approval then lapses at that time if unused (P-08);
   - **Note** (optional, up to 500 characters): stored as the approval's `note` and visible to the space;
   - the buttons `Sign approval` (primary) and `Back`.
3. `Sign approval` calls `EffectApprove{proposal, planDigest, expiresAt?, note?}` (data.md §6). The button shows "Signing…", and the whole panel is inert while the call runs.
4. Outcomes:
   - **Recorded** (a receipt): the panel closes; the card shows "Approved · seq 1234" for 2 s. Then the card moves to Waiting on others if approvals are still needed, otherwise to Recent with "approved; waiting for the executor". Focus moves to the next card in Your decisions, or to the section heading if there is none.
   - **Queued** (the node accepted it into its outbox; the home is unreachable): the card shows "Approval signed and queued; it will be sent when the space's home is reachable" and moves to Waiting on others with a "queued" chip.
   - **Refused:** the panel stays open with the error in place (§4.4), and the button returns to `Sign approval` when retrying makes sense.
5. When the card's effect leaves `open` (claimed by the executor, then succeeded or failed), its row in Recent updates live.

- **APR-5.** No single key press signs. From a focused card it takes at least two deliberate actions: `a` opens the sign panel, then `Enter` on the focused `Sign approval` button. `Enter` anywhere else in the card never signs.
- **Note on APR-5 (pending round 33).** APR-5 is an accident guard, not a security check: terminal input is shared and injectable, so a key press (even two) proves no person is present. The authority is the node's presence check (PLAN §6.2 "Approvals: human presence", SR-106): after `Enter` on `Sign approval` the node asks the operating system to confirm on the device (Windows Hello, Touch ID, polkit), and signs only on success. The face sends the same `EffectApprove`; while the node waits the panel reads "Waiting for Windows Hello…" (the backend named for the platform), and a cancelled, timed-out or unavailable check comes back as `denied` with a message that starts `approval not signed: ` and is shown as is. Pending round 33: whether the signed approval should carry an attestation that a presence check passed (a wire change, so out of scope for now).
- **Note on steering and governing (pending round 33).** `SessionInput` (including the "Tell the agent why" text) and the commands that govern (and `AgentStart`) are sent on a face connection that the person has unlocked: the first one makes the node ask the operating system to confirm the person (SR-108), and the unlock then lasts a window (ten minutes by default) on that connection only. The face needs no extra control: the node answers `presence_required` to a locked connection and the node-API client asks for the unlock and sends the command again, so the first steering input of a window takes as long as the person's answer, and a refusal comes back as `denied` with a message that starts `not unlocked: `. Approvals stay confirmed per action.
- **APR-6.** `Escape` in the sign panel closes it without signing and returns focus to "Review & sign approval".
- **APR-7.** When my own approval has expired, or will expire within 10 minutes, and the effect is still open, the card shows "Your approval expires at 14:30" and offers "Approve again". This supersedes the old approval (P-08) through the same sign panel.

### 4.2 Cancel (deny)

HAN has no signed "deny". The decision against an effect is `effect.cancel`, which only some actors may append (P-04). The inbox offers two different controls, and never blurs them:

- **Cancel effect** (shown only when I can cancel). It opens a confirm panel:
  - "Cancel this git.push? It will not run. The agent has to propose again to retry."
  - When the effect is `claimed`: "The executor has already claimed it. Cancelling asks it to stop; it may already have run."
  - **Tell the agent why** (optional): text sent to the proposing session as a `SessionInput`, if I have the steer right there. The field's label states exactly that: "Sent to lars-coder's session as a steering input". Without the steer right the field is absent.
  - The buttons `Cancel effect` (danger) and `Keep it`, with initial focus on `Keep it`.

  Confirming calls `EffectCancel{proposal}`, then the optional `SessionInput`. These are two calls: if the second fails, the card says "Cancelled; your message to the agent wasn't sent" with "Retry message".
- **Dismiss** (shown when I can approve but can't cancel, for example as a rule approver with no approval of my own yet). It hides the card from Your decisions **for me only** and moves it to Dismissed. Dismiss is the acknowledge mark of attention.md (ATT-R21): private to me, kept by my node, synced to my other devices, and never told to the space. A toast reads "Hidden for you. Others can still approve it." with "Undo" for 5 s. Nothing is signed or sent.

- **APR-8.** The words "deny" and "reject" are not used for Dismiss, because nothing is recorded. Dismiss's tooltip reads "Hide this from your inbox. It stays open for other approvers."
- **APR-9.** A dismissed effect comes back to Your decisions on a material change, as attention.md ATT-R20 defines it for every acknowledged item: a new reason (for example I become a required guard, or someone asks me, Q-APR-4), a lower tier, a rising escalation level (ATT-R9: waiting 30 minutes, or expiring within 10 minutes), a new effect in its place, or its session failing. The node decides it; the GUI shows it.

### 4.3 Live changes while a card is open

- **APR-10.** If the effect changes phase while its card is open (another approver's approval was consumed by a claim, or someone cancelled it), the card updates in place and shows a one-line notice ("Cancelled by @ada just now"). Approving becomes disabled when it no longer applies. If the sign panel was open, it closes, and focus moves to the notice.
- **APR-11.** Cards do not reorder while one of them has focus or the sign panel is open. Ordering changes are applied when focus leaves the section.

### 4.4 Errors

| Error | Message in the card | Can retry |
|---|---|---|
| `denied` | "The space refused this approval: <reason>." The reason comes from the verdict, for example "your role changed", "you are limited", "the approval cap is reached", "the effect has expired". | no; the card refreshes |
| `conflict` | "This effect changed while you were approving (now: <phase>)." | no |
| `invalid` | "The approval was malformed: <reason>. This is a bug; details are in the log." It shows the error's id. | no |
| `not_found` | "This effect no longer exists in the space." | no |
| `unavailable` | the node couldn't queue it: "Couldn't sign right now (<reason>)." | yes |
| transport or decode error | "Lost contact with the node." | yes |

### 4.5 Keyboard

When-clause `focus.inbox && !focus.textInput`:

| Key | Command | Does |
|---|---|---|
| `j` / `k`, `↓` / `↑` | `inbox.next` / `inbox.prev` | move focus between cards and rows across sections |
| `Enter`, `Space` | `inbox.toggle` | expand or collapse the focused card |
| `a` | `inbox.approve` | open the sign panel (only when approving is enabled, otherwise it announces the reason) |
| `c` | `inbox.cancel` | open the cancel confirm (only when I can cancel) |
| `d` | `inbox.dismiss` | dismiss (only when Dismiss is offered) |
| `r` | `inbox.raw` | switch between rendered and raw plan |
| `s` | `inbox.openSession` | open the proposing session's pane |
| `v` | `inbox.viewChanges` | open the plan's changes (git.push, accept work into main) |
| `y` | `inbox.copyDigest` | copy the full digest |
| `g g`, `shift+g` | – | first or last card |

---

## 5. Admissions

When a device joins a space with a ticket, the home signals the space's owners `{type: "join", actor}` (N-03), and an owner's `member.set` admits the actor. The inbox shows each pending join to the owners of that space:
- the actor id;
- its kind (human or agent, from its profile, with the operator for an agent);
- the space;
- when it joined;
- whether the local user issued the ticket.

Actions:
- **Admit as member** (default) or **Admit as guest**, from a split button. It calls `MemberSet{space, actor, role}`. An agent can be admitted only when its operator is confirmed (P-03). Otherwise the button is disabled with the reason "The agent's operator hasn't confirmed it."
- **Ignore:** hides the row locally. No entry is recorded. The person stays un-admitted.

- **APR-12.** A device that issued the ticket MAY admit automatically (N-03). When it did, the row shows "admitted automatically (your ticket)" in Recent instead of asking.

---

## 6. Notifications

### 6.1 When to notify

- **APR-13.** An OS notification is shown when an item enters **Your decisions** or **Admissions** that was not there before, in any space the app follows.
- **APR-14.** No notification is shown when the app's window is focused and the item is already on screen: the inbox is visible with that section, or the item's session pane is visible with its decision bar.
- **APR-15.** At startup, items present in the first snapshot that the local user has not seen before (not in the seen set, data.md §8) produce **one** summary notification, "3 decisions are waiting for you". Items already seen produce none.
- **APR-16.** Bursts are coalesced: when more than 3 notifications would fire within 10 s, a single "5 decisions are waiting for you" replaces them.
- **APR-17.** No item notifies twice, except one reminder for an effect waiting on me whose `expiresAt` comes within 10 minutes, if the reminder setting is on.

### 6.2 Content

- **APR-18.** A notification carries identifiers only (SR-100): the title "Approval needed: <action>" or "Join request: <space>", and a body of agent · issue · space (for example "lars-coder · #41 · team-han"). It never carries plan contents, URLs, agent text, file paths or summaries.
- **APR-19.** A notification has no action buttons. Activating it brings the app to the front and opens the inbox with that card expanded and focused. Approving from the notification itself is not offered, because the plan must be on screen first (P-08).

### 6.3 Badges

- The inbox's rail item shows the count of Your decisions plus Admissions (shell.md §1).
- The app's dock or taskbar badge, or its tray tooltip, shows the same count where the OS supports it. The window title is prefixed with "(N) " when the count is over 0.

### 6.4 Permission

The app asks for notification permission the first time it would notify, not at startup. If permission is refused, the inbox shows a dismissible line: "Notifications are off for HAN in your system settings."

### 6.5 Settings

Under Settings → Notifications (shell.md §5), each category is on or off. The defaults:
- decisions that need me: on;
- admissions (owners): on;
- expiry reminder: on;
- my agents' sessions failing: on;
- outcomes of effects I approved (succeeded, failed): off;
- overlaps involving my agents: off;
- OS sounds: system default.

---

## 7. Surface states

| State | Appearance |
|---|---|
| **Loading** | section headings with skeleton cards. After 3 s: "Waiting for the node…" |
| **Empty** | Your decisions is empty: "No decisions are waiting for you." Under it, in the quiet tone, a count of what waits on others when it is over 0. Other sections are hidden when empty, except Recent, which reads "No decisions in the last 7 days." |
| **Live** | as specified |
| **Stale** | the node is offline from a space's home: that space's cards show "offline: the state may have changed", and approving is still allowed. The result is then queued (§4.1, step 4). |
| **Resyncing** | a thin bar under the toolbar, with the cards kept |
| **Error** | per space: a failing space shows an error row in place of its cards ("Couldn't load team-han: <message>", Retry). Other spaces still show. |

---

## 8. Accessibility

- **APR-20.** Each section is a `region` with its heading as its label. Each card is an `article` whose label is "<action> <target>, <space>, <phase>". The expanded card is linked from its compact row with `aria-expanded` and `aria-controls`.
- **APR-21.** The digest is read by assistive technology as groups ("9f3c, 41aa, …"), not as one 64-character word. The verified mark has text, not only an icon.
- **APR-22.** The sign panel is a `group` labelled "Sign approval". Its focus order is expiry, note, `Sign approval`, `Back`, and focus is trapped inside the card while the panel is open. Initial focus goes to the expiry choice, not to `Sign approval`.
- **APR-23.** New items in Your decisions are announced politely ("Approval needed: git.push, team-han"), coalesced as for notifications. Phase changes on an open card are announced politely.
- **APR-24.** Danger actions (Cancel effect, force pushes in a plan) use the danger tone **and** words; colour is never the only signal.
- **APR-25.** Which notices fire, and when, is decided by the node under attention.md §8, the same for every face: the blocked and pool graces, the per-person budget and digests, escalation, mute (errors pass, and one quiet cue for an approval only I can give that is about to expire) and outages. APR-13 to APR-17 (§6.1) describe how the GUI presents them, and hold as limits: the node's budget never lets more through than they do.

---

## 9. Acceptance criteria

| ID | Given | When | Then |
|---|---|---|---|
| AC-APR-01 | fixture `approvals` (2 needing me, 1 waiting on others, 3 recent) | the inbox opens | the sections appear in order with counts 2, 1, 3; the first card in Your decisions is expanded |
| AC-APR-02 | `approvals` | the expanded card renders | it shows the action, target, rendered plan, full digest on expand, "plan matches digest", proposer, operator, executor, session, task, cost, expiry, rule, needed approvals and "you can approve as …" · *pending node API (Q-APR-8)* |
| AC-APR-03 | `approvals`, an `open_pr` proposal with an agent-written title | the card renders | the title appears only inside the "written by … (agent)" box · *pending node API (Q-APR-8)* |
| AC-APR-04 | `approvals`, a card whose plan was not hash-checked (`digestVerified: false`) | the card renders | "Review & sign approval" is disabled, and its description names the reason · *pending node API (Q-APR-8)* |
| AC-APR-05 | `approvals`, a plan whose digest check failed | the card renders | the danger message shows; no control can approve; Cancel effect is offered if I can cancel · *pending node API (Q-APR-8)* |
| AC-APR-06 | `approvals`, card collapsed | the user presses `a` on the compact row | the card expands; the sign panel does not open until the full card has rendered; a second `a` opens the panel |
| AC-APR-07 | `approvals` | the user opens the sign panel, keeps the defaults, and presses Enter on `Sign approval` | the mock receives exactly one `EffectApprove` with that card's `planDigest` and an `expiresAt` 24 h ahead (virtual clock) · *pending node API (Q-DAT-15)* |
| AC-APR-08 | `approvals`, sign panel open | the user presses Escape | no call is sent; focus returns to "Review & sign approval" |
| AC-APR-09 | `approvals`, focus on the expanded card body (not the panel) | the user presses Enter | no `EffectApprove` is sent |
| AC-APR-10 | `approvals` | `EffectApprove` returns a receipt and the mock's view then lists the approval | the card shows "Approved · seq N", then moves to Waiting on others; focus moves to the next card in Your decisions |
| AC-APR-11 | `approvals`, outbox mode | `EffectApprove` returns queued | the card shows the queued message and the "queued" chip under Waiting on others |
| AC-APR-12 | `errors`: `EffectApprove` returns `denied` with reason "role changed" | the user signs | the card shows "The space refused this approval: role changed." with no retry |
| AC-APR-13 | `approvals`, sign panel open | the mock changes the effect's phase to `cancelled` | the panel closes; the card shows the cancelled notice; approving is disabled; focus is on the notice |
| AC-APR-14 | `approvals`, I am the proposer's operator (so I can cancel) | the user presses `c` and confirms | the mock receives `EffectCancel`; initial focus in the confirm was on "Keep it" |
| AC-APR-15 | `approvals`, cancel with "Tell the agent why" filled, and I hold the steer right | the user confirms | the mock receives `EffectCancel`, then `SessionInput` to the proposing session with that text |
| AC-APR-16 | `approvals`, I can approve but not cancel | the card renders | there is no "Cancel effect"; "Dismiss" is offered; dismissing moves the card to Dismissed with an Undo toast; no call is sent |
| AC-APR-17 | `approvals`, `separateApprover` and I am the proposer | the card renders | approving is disabled with "you proposed this and the space requires a separate approver" |
| AC-APR-18 | `approvals`, a guarded agent's effect | the card renders | the required guard is listed with its state; "one approval covers both" appears when I am both the rule approver and the guard · *pending node API (Q-APR-8)* |
| AC-APR-19 | app running, window unfocused | the mock adds an effect to Your decisions | one OS notification is requested, with title "Approval needed: <action>" and a body containing only agent, issue and space |
| AC-APR-20 | window focused, inbox visible | the mock adds an effect to Your decisions | no OS notification; the live region announces it |
| AC-APR-21 | app unfocused | the mock adds 5 effects to Your decisions within 2 s | exactly one notification, "5 decisions are waiting for you" |
| AC-APR-22 | the seen set holds effects A and B; the first snapshot has A, B and C | the app starts | one notification is requested (for C, or a summary of 1) |
| AC-APR-23 | a notification was requested | the test activates it | the app focuses, the inbox opens, and that card is expanded and focused |
| AC-APR-24 | `admissions` (I am an owner; one join) | the inbox opens | Admissions shows the actor, its kind and the space; "Admit as member" sends `MemberSet` with role member · *pending node API (Q-DAT-13)* |
| AC-APR-25 | `admissions`, the joiner is an agent without a confirmed operator | the row renders | admitting is disabled with the reason · *pending node API (Q-DAT-13)* |
| AC-APR-26 | `approvals`, an `http.post` with a 300-character URL | the card renders | the full URL is visible (wrapped, not truncated) |
| AC-APR-27 | `approvals` | the inbox's badge and the window title are checked | the badge shows 2 and the title starts with "(2) " |
| AC-APR-28 | any inbox fixture | axe-core runs in both themes | no WCAG 2.2 AA violations; the digest's accessible text is grouped |
| AC-APR-29 | `approvals`, a card dismissed on another of my devices (the mock delivers the mark) | the inbox renders | the card is under Dismissed with no action on this device · *pending node API (AN-4)* |

---

## 10. Open questions

- **Q-APR-1.** *Resolved by PR #35:* the node provides `why_gated`.
- **Q-APR-2.** P-08's rule needs one usable rule approval (plus each guard's). PLAN §6.2 mentions "two approvals for releases". Is that an extension, or a future Policy change? The card's "needed: N" assumes one until then.
- **Q-APR-3.** *Partly resolved by PR #35:* `Queued{outbox_id}` and the `Outbox` query let the card show a queued approval and follow it until it is sent. Still open: should it be withdrawable before it is sent? There is no `Command` to remove an outbox item. Without one, a queued approval goes out when the home is reachable, even if the user changed their mind; they would then have to cancel the effect.
- **Q-APR-4.** HAN has no "ask a specific person to approve" signal. A dismissed card comes back only on the changes listed in APR-9. Should there be a `message` convention for a nudge?
- **Q-APR-5.** The default approval expiry is 24 h here. Should the space's policy suggest a default, or a maximum, for approvals' `expiresAt`?
- **Q-APR-6.** Device identity in the sign panel ("on this device (laptop)") needs a device name, which isn't in the protocol. Is it a local label only?
- **Q-APR-7.** Should Recent include effects in which I wasn't involved but which touched my agents' sessions (for example another person's push of a branch my agent overlaps)?
- **Q-APR-8.** Card fields that PR #35 does not list: the node's digest verification (`digestVerified`, DAT-11), the agent-written text with its author (`agentText`, SR-98), and the per-guard progress (`needed.guards`). Each is needed for the card; until they exist, Team B keeps them as provisional types (data.md DAT-3).

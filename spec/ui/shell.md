# UI spec: app shell, palette, keybindings, theming, onboarding, identity and trust

Status: draft, Team A (clean room, PLAN.md §8). Behaviour and appearance only. Read with [data.md](data.md).

The shell is everything around the three main surfaces (board, session pane, inbox). It covers:
- windows, navigation and routes;
- the command palette and the keybinding system;
- theming;
- startup;
- the onboarding, identity, device and trust screens.

HAN-specific screens (identity, devices, tickets, trust) have no prior art behind this spec. They follow PLAN §6.2 and architecture.md N-03, P-03 and §4.3.

---

## 1. Window anatomy

```
┌──────────────────────────────────────────────────────────────────────────────────────────┐
│ ◉ HAN   team-han ●  › Board                      [⌕ Search or run a command  mod+K]  @lars ▾ │  top bar
├────┬─────────────────────────────────────────────────────────────────────────────────────┤
│ TH │                                                                                     │
│ AC │                                                                                     │
│ +  │                         main surface (board / session / inbox / …)                  │
│────│                                                                                     │
│ ▦  │  Board                                                                              │
│ ✉2 │  Inbox                                                                              │
│ ⛨  │  Trust                                                                              │
│    │                                                                                     │
│ ⚙  │  Settings                                                                           │
├────┴─────────────────────────────────────────────────────────────────────────────────────┤
│ node running · 3 peers · outbox 0 · home reachable · synced 2s ago          v0.4.0      │  status bar
└──────────────────────────────────────────────────────────────────────────────────────────┘
```

1. **Top bar.**
   - The app mark.
   - The current space's name, with a reachability dot for its home: `tone-success` when reachable, `tone-attention` when offline, plus a word in its tooltip.
   - A breadcrumb of the surface (Board, or Board › lars-coder #41).
   - A search field that opens the palette (§3).
   - The identity menu, which shows the local user's handle and holds Profile, Devices, Theme and Settings.

   On macOS the bar leaves room for the window controls. Elsewhere it sits under the system title bar or hosts its own controls, whichever the platform convention is.
2. **Rail** (left, 56 px collapsed; 200 px expanded with labels). Its contents, in order:
   - **Spaces:** one tile per followed space, showing its initials and an unread or needs-me dot, and a "+" tile for Join or Create.
   - **Surfaces:** Board, Inbox (with the badge from approvals.md §6.3), Trust.
   - **Settings,** at the bottom.

   Every rail item has a tooltip and an accessible name, including its badge ("Inbox, 2 decisions are waiting for you"). `mod+\` toggles expanded or collapsed, and the choice is remembered.
3. **Main surface.** One route at a time.
4. **Status bar** (24 px). It shows the node's state and the current space's sync:
   - "node running" or "node starting" or "node stopped";
   - the peer count;
   - the outbox depth (in the attention tone when over 0);
   - home reachable, or "offline since 12:03";
   - "synced 2s ago".

   Activating it opens the Node panel (§8.3). It shows the app version on the right.

---

## 2. Routes

Routes use TanStack Router with typed search parameters. Every surface's view options live in the URL, so back and forward restore them.

| Route | Surface | Search parameters |
|---|---|---|
| `/welcome` and its children (§7) | onboarding | – |
| `/space/:space/board` | board | `group` (issue, person, state), `filter` (text), `peek` (session id) |
| `/space/:space/session/:session` | session pane | `mode` (conversation, steps), `tab` (stream, changes, record), `from`, `to` (timeline points; absent `to` = live), `file` |
| `/space/:space/trust` | trust (§7.6) | `section` |
| `/inbox` | approvals inbox | `space`, `session`, `effect` |
| `/settings/:section` | settings (§5) | – |
| `/join` | join by ticket (§7.4) | `ticket` (prefilled, never auto-submitted) |

- **SHL-1.** An unknown route redirects to the current space's board. A `:space` the node doesn't follow shows "You don't follow this space" with "Join with a ticket" and "Back".
- **SHL-2.** Back and forward are `alt+←` / `alt+→` on Windows and Linux, and `mod+[` / `mod+]` on macOS, plus the mouse's back and forward buttons.
- **SHL-3.** The last route of each space is remembered. Switching space returns there, defaulting to the board.

### 2.1 Windows

A session pane or the inbox can open in its own window (mod+click, or "Open in new window"). Each window is a separate webview with its own subscriptions (data.md §5.6). Closing the main window on Windows and Linux quits the app unless "Keep running in the tray" is on (the default is on, so notifications keep arriving). On macOS, closing the last window keeps the app running, as is usual there.

---

## 3. Command palette

### 3.1 Opening and modes

- `mod+k` opens the palette, from anywhere, including text fields.
- `:` opens it in command mode, but only when no text field has focus (this matches the TUI's `:` palette, PLAN §6.6).
- The palette is a centred dialog, 640 px wide (or the window width minus 32 px), with an input and a result list.

The first character of the query picks the mode:

| Prefix | Searches |
|---|---|
| (none) | everything below, mixed, grouped under headings |
| `>` | commands only |
| `@` | people and agents in the current space: open their sessions, or filter the board to them |
| `#` | issues on the board and open tasks |
| `~` | spaces: switch to one |
| `?` | keyboard help: the bindings that apply here, searchable |

### 3.2 Results

- Matching is fuzzy: the query's characters must appear in order, and matches at word starts and consecutive matches rank higher. With an empty query the list shows **recent** items: the last 5 sessions opened and the last 5 commands run.
- Each command shows its title, its category (Board, Session, Inbox, View, Go, Space, Settings) and its current keybinding, right-aligned.
- Commands whose `when` is false in the current context are left out. When a query exactly matches one of them, it appears disabled with the reason ("Only in the session pane").
- Sessions show person · agent · issue · state glyph.

### 3.3 Keys inside the palette

- ↑/↓ move.
- Enter runs the item.
- mod+Enter opens it in a new window, for sessions and the inbox.
- Escape closes the palette (when the query is empty) or clears the query (otherwise).
- Backspace on an empty query removes the mode prefix.

When the palette closes, focus returns to where it was before it opened.

### 3.4 Commands

Every user action is a **command** in one registry: an id, a title, a category, an enablement `when`, and default keys. Menus, buttons, the palette and keybindings all invoke commands from it, so there is one list. §4.4 lists the defaults.

---

## 4. Keybindings

### 4.1 Rules

Bindings are data, not code. The scheme follows the convention that most code editors share: a binding names a key, a command, and an optional condition on where focus is. This section fixes HAN's own version of it.

A keybinding is a rule `{key, command, when?}`:
- **key** is one key combination, or a chord of two separated by a space (`g i`);
- **command** is a command id, or a command id prefixed with `-` (a removal, below);
- **when** is an optional condition (§4.2).

- **SHL-4.** Layering and removal:
  - the effective set is built from the defaults, then from the user's file, in that order;
  - when several rules match a key press and their `when` holds, the one built in **last** applies;
  - a user rule `{key: K, command: "-C"}` removes only the default binding of command C **to key K**; C's other default keys stay bound. For example, removing `j` from `board.next` leaves `↓` bound to it. A removal names no `when`, and it removes the default binding under whatever `when` it had.
- **SHL-5.** The user's file:
  - it is `keybindings.json` in the app's config folder, a JSON array of rules;
  - each rule is checked on its own. A rule with an unknown key name, an unknown command or a `when` that doesn't parse is ignored, and Settings → Keybindings shows it with the reason;
  - when the file as a whole can't be parsed, none of it applies. The defaults are used, and Settings shows a banner about it;
  - the app watches the file and applies changes to it without a restart.
- **SHL-6.** Writing keys:
  - a combination is written as its modifiers then its key, joined by `+`: `mod+shift+p`, `alt+z`, `g`;
  - `mod` stands for the platform's primary modifier (Command on macOS, Control on Windows and Linux); `ctrl`, `alt` (Option on macOS), `shift` and `meta` name the others;
  - the key part is the browser's `KeyboardEvent.key` value in lowercase (`enter`, `escape`, `arrowdown`, `pageup`, `/`, `[`). In these specs an arrow glyph such as `↓` stands for its key name.
- **SHL-7.** Chords: after the first key of a chord, the status bar shows "g …" for up to 1 s, and the next key completes or cancels the chord. A key that completes no chord is not passed on.

### 4.2 When-clauses

A condition combines **context keys** (named booleans the shell keeps up to date) with not (`!`), and (`&&`), or (`||`) and parentheses. Not binds tightest, then and, then or. A key that the shell doesn't define counts as false, so a condition written for a newer version never fires by accident.

| Context key | True when |
|---|---|
| `focus.textInput` | a text field, the steering input, the filter box or any editable element has focus |
| `focus.board` | focus is inside the board list |
| `focus.session` | focus is anywhere inside the session pane (stream, Changes, decision bar, steering input) |
| `focus.stream` | focus is inside the session stream |
| `focus.changes` | focus is inside the Changes column (tree or diff) |
| `focus.diff` | focus is inside a diff list |
| `focus.tree` | focus is inside a file tree |
| `focus.scrubber` | a scrubber handle has focus |
| `focus.inbox` | focus is inside the inbox |
| `focus.steer` | the steering input has focus |
| `open.palette` | the palette is open |
| `open.dialog` | any modal dialog is open |
| `open.peek` | the board's peek panel is open |
| `session.mine` | the open session belongs to one of the local user's agents |
| `session.canSteer` | the local user holds the steer right in the open session |
| `session.hasDecision` | the decision bar is showing |
| `scrubber.live` | the scrubber's **to** handle is at live |
| `board.hasFilter` | the board filter is not empty |
| `platform.mac`, `platform.windows`, `platform.linux` | the OS |
| `shortcuts.singleKey` | single-key shortcuts are enabled (§4.5) |

Every default single-key binding includes `shortcuts.singleKey && !focus.textInput` in its `when`.

### 4.3 Dispatch rules

- **SHL-8.** Keys go first to the focused text field when `focus.textInput`, except bindings that include `mod` or are `escape`.
- **SHL-9.** `Escape` closes the topmost layer, in this order: menu or popover, then dialog, then palette, then the peek panel, then the find bar, then the sign panel. Otherwise it moves focus from a text field to its surface. It never navigates.
- **SHL-10.** A shortcut never navigates away from the current surface unless its command is in the Go category. Surface commands act only on their own surface.
- **SHL-11.** Within a modal dialog, only the dialog's own keys and `mod+k` apply.

### 4.4 Default bindings

Global (no `when` unless stated):

| Key | Command | Title |
|---|---|---|
| `mod+k` | `palette.open` | Open the command palette |
| `:` | `palette.commands` | Open the palette in command mode (single-key) |
| `?` | `help.keys` | Show keyboard help (single-key) |
| `g b` | `go.board` | Go to the board |
| `g i` | `go.inbox` | Go to the inbox |
| `g t` | `go.trust` | Go to the space's trust screen |
| `g s` | `go.settings` | Go to settings |
| `mod+1` … `mod+9` | `space.switch.1` … `9` | Switch to the Nth space in the rail |
| `mod+,` | `go.settings` | Settings |
| `mod+\` | `view.toggleRail` | Expand or collapse the rail |
| `mod+shift+l` | `view.cycleTheme` | Cycle the theme: system, light, dark |
| `mod+=` / `mod+-` / `mod+0` | `view.zoomIn/Out/Reset` | Text size |

Board, session, inbox and diff bindings are listed in board.md §7.1, session.md §5.4 and §9, approvals.md §4.5 and diffs.md §10. They are part of the default set as written there.

### 4.5 Settings → Keybindings

- A searchable table lists each command's title, id, keys and `when`, plus the source (default or user).
- Selecting a row and pressing Enter starts recording: the next key combination (or chord) becomes the new binding, written as a user rule. Escape cancels.
- **SHL-12.** When a new binding's key is already bound under an overlapping `when`, the editor shows the clash ("Also bound to Board: next in focus.board") and asks to keep both (the last one wins) or replace.
- **SHL-13.** A switch, "Single-key shortcuts", is on by default. Turning it off disables every binding without a modifier (WCAG 2.1.4).
- "Reset to defaults" removes the user file after confirmation.

---

## 5. Settings

| Section | Contains |
|---|---|
| General | start at login; keep running in the tray; send steering with Enter or with mod+Enter; default approval expiry (approvals.md §4.1) |
| Appearance | theme (§6.1); density; text size; reduced motion (system, on, off) |
| Keybindings | §4.5 |
| Notifications | approvals.md §6.5 |
| Diffs | diffs.md §5.7 |
| Identity | handle, display name, recovery key (§7.2) |
| Devices | §7.3 |
| About | version; licences of bundled dependencies, with their NOTICE texts; the node's data folder, with a button to open it |

- **SHL-14.** Settings save on change. There is no Save button. Each change applies at once and persists through data.md §8.

---

## 6. Theming

### 6.1 Themes

Themes: **System** (the default; it follows the OS light or dark setting live), **Light**, **Dark**, and **High contrast**, which follows light or dark, honours forced-colours mode, and uses system colours there.

### 6.2 Semantic tokens

Every colour is a CSS custom property, defined once per theme. Components use only these roles, never literal colours.

| Group | Roles |
|---|---|
| surfaces | `surface-page`, `surface-raised` (panels and cards), `surface-sunken` (wells, code), `surface-overlay` (dialogs, popovers), `surface-hover`, `surface-selected` |
| text | `text-normal`, `text-quiet` (secondary, and stale or failed rows), `text-inverse`, `text-link` |
| lines | `border-subtle`, `border-strong`, `focus-ring` |
| tones (state) | `tone-active` (working, live), `tone-attention` (needs action, waiting, blocked, hunk overlap), `tone-success` (done, verified, added), `tone-danger` (failed, conflict, deleted, destructive), `tone-quiet` (idle, stale, claim overlap), `tone-info` |
| diffs | `diff-add`, `diff-add-word`, `diff-del`, `diff-del-word`, `diff-overlap-band`, `diff-gutter` |

- **SHL-15.** Every text-on-surface pair used meets WCAG 2.2 AA (4.5:1 for body text, 3:1 for large text and for the focus ring and essential icons) in Light, Dark and High contrast. A token-pair test (data.md §9.5) checks every pair the components use.
- **SHL-16.** State is never conveyed by colour alone. Every tone appears with a glyph shape or a word (board.md §4.1).
- **SHL-17.** Changing the theme applies everywhere at once, without a reload, including open diffs (DIF-17).

### 6.3 Typography, density, shape and motion

HAN is a dense, read-mostly operations tool: many rows, live values, small text read for long periods. These rules state outcomes. Exact values within the given ranges are Team B's choice, recorded once as tokens (`size-*`, `space-*`, `radius-*`, `motion-*`) and used everywhere.

- **Fonts:** the system UI font for text and the system monospace font for code, digests, ids and diffs. No webfonts are fetched (SR-95).
- **Base text size:** between 13 and 14 px, so that a board row's four column groups fit on one line at 1,280 px without truncating a typical `step`. The user can scale text from 85% to 150%.
- **Density:** two settings. Comfortable fits at least 20 board rows in a 900 px-high window; Compact fits at least 26. Row height is fixed within a density (BRD-30). Hit targets stay at least 24 px in both (WCAG 2.5.8).
- **Shape:**
  - structure comes from alignment and spacing first. A line is drawn only where two regions meet (the rail and the main surface, the stream and Changes columns, a sticky header and its list);
  - rows in a list are separated by spacing or a hairline inside one container, never by boxing each row;
  - corners are slightly rounded and consistent per element class (one radius token for containers, one for controls), with containers never sharper than the controls inside them;
  - depth (a shadow) marks only things that float above the page: menus, popovers, dialogs, the palette, toasts.
- **Motion:** motion exists only to show continuity (where a row went, which panel opened) or liveness.
  - Three duration tokens: `motion-instant` for colour and hover feedback, `motion-move` for rows and small layout changes, `motion-panel` for panels and dialogs. Each is between 80 and 250 ms, in that order, and the user should never wait on an animation to act.
  - Things enter decelerating and leave accelerating.
  - Only liveness marks (the working pulse, spinners) loop. Under reduced motion, every transition is instant and loops stop.

---

## 7. Startup, onboarding, identity, devices and trust

### 7.1 Startup

```
 app opens → [starting node] ──fail──► Node failed screen (§8.2)
                   │ ok
                   ▼
           identity on this device? ──no──► /welcome
                   │ yes
                   ▼
           follows any space? ──no──► "Join or create a space" (§7.4, §7.5)
                   │ yes
                   ▼
           last route of the last space (board by default)
```

- **SHL-18.** While the node starts, the window shows the app mark and "Starting HAN…". After 5 s it adds the node's current startup step (from SpaceStatus or the node's own status, data.md §4.6). Nothing else renders until the node answers.

### 7.2 Welcome and identity

`/welcome` offers three choices:
- **Create my identity:** this is my first device;
- **Add this device to my identity:** I already use HAN on another device;
- **Join a space with a ticket:** shown here as a hint. Joining needs an identity first, so this choice leads into one of the other two, and then into the join.

**Create my identity** (`/welcome/identity/new`):

```
┌ Create your identity ──────────────────────────────────────────────┐
│ Handle        [ lars            ] @ [ han.example        ▾ ]       │
│               Your id will be @lars:han.example                    │
│ Display name  [ Lars            ]                                  │
│                                                                    │
│ Your key is made on this device and stored in the system keychain. │
│ It never leaves this device.                                       │
│                                          [Back]   [Create]         │
└────────────────────────────────────────────────────────────────────┘
```

1. The handle follows the protocol's actor grammar. It is validated as the user types, and the reason is shown inline when it fails. The server part names where the user's profile space is hosted (Q-SHL-1).
2. **Create** asks the node to create the identity: a key in the OS keychain, and a profile space. When the keychain is unavailable (for example on a headless Linux system without a secret service), a second step asks for a passphrase to protect a key file. It explains: "Your system has no keychain. HAN will keep your key in an encrypted file protected by this passphrase." (PLAN §6.2).
3. **Recovery key.** The node returns a recovery key, shown once:
   - in large monospace;
   - with "Copy" and "Save to file…";
   - with the sentence "Anyone with this can act as you. Keep it offline. HAN can't show it again."

   To continue, the user must type the characters of one group, chosen at random, from it ("Type group 3 to confirm you saved it"). "I'll do it later" is allowed. Settings → Identity then shows a persistent warning until it is done (Q-SHL-2).
4. Done. The user goes on to "Join or create a space".

**Add this device** (`/welcome/device/add`):

```
┌ Add this device to your identity ──────────────────────────────────┐
│ On a device where you already use HAN, open                        │
│ Settings → Devices → Add a device, and enter this code:            │
│                                                                    │
│     han-dev1 Q2ZF … 9XKA          [Copy]      [QR]                 │
│                                                                    │
│ Then check that both devices show the same words:                  │
│     maple · orbit · candle · seven                                 │
│                                                                    │
│ Waiting for your other device…  ◌                       [Cancel]   │
└────────────────────────────────────────────────────────────────────┘
```

- The code carries this device's public key and node id (Q-SHL-3). The four words are a short check derived from both devices' keys, and they appear on both screens, so that a substituted code is caught.
- On the existing device, Settings → Devices → **Add a device** takes the code (paste, or scan the QR on a device with a camera). It shows the same four words and asks "Do these match the new device?" with `They match, add device` and `Cancel`. Confirming calls `DevicePair` with the code, and the existing device's node signs `key.add` (PR #35). The new device shows its offer through `DeviceOffer`.
- The new device moves to "This device is now @lars:han.example's" and continues to its spaces.
- **SHL-19.** The words MUST be shown on both devices before the existing device signs. Nothing is signed while the words have not been confirmed.

### 7.3 Devices (`/settings/devices`)

A list of the identity's keys. Each row shows:
- the local name (editable; local only, Q-APR-6);
- the key id, in short form, which can be copied in full;
- the node id, in short form;
- when it was added, and by which device;
- the last time it was seen (from presence, when known);
- its status (valid, or revoked on a date);
- "this device" on the current one.

Actions:
- **Add a device** (above).
- **Revoke** another device. A confirm dialog reads: "Revoke 'old laptop'? It will no longer be able to act as you. Approvals it signed that haven't been used will stop counting." Its buttons are `Revoke device` (danger) and `Cancel`, with focus on Cancel. Confirming calls `DeviceRevoke`, and this device's node signs `key.revoke`.
- **SHL-20.** The current device can't revoke itself from this screen. That control is absent, with the tooltip "Revoke this device from another of your devices."
- A device that has lost every other device is told: "If you have no other device, use your recovery key" (Settings → Identity → Recover).

### 7.4 Join a space by ticket (`/join`)

```
┌ Join a space ──────────────────────────────────────────────────────┐
│ Paste the invite you were given                                    │
│ [ han1eyJub2RlIjp7…                                             ]  │
│                                                                    │
│ ✓ Valid invite                                                     │
│   Space     team-han  (r7:han.example)                             │
│   Home      han.example · node 5f2a…c91e                           │
│   One-time  this invite works for one device, once                 │
│                                                    [Join space]    │
└────────────────────────────────────────────────────────────────────┘
```

- **SHL-21.** The ticket is parsed locally as the user types: the `han1` prefix, base64url, and a JSON object with `server`, `node`, `space` and `secret` (N-03). An invalid ticket shows one reason ("This doesn't start with han1", "This invite is damaged (it isn't valid base64url)", "This invite is missing its space"). The secret is never displayed and never logged.
- **SHL-22.** "Join space" calls `SpaceJoin{ticket}`. A step list shows progress:
  1. Connecting to the space's home…
  2. Checking the home's identity…
  3. Using the invite…
  4. Waiting for an owner to admit you…
  5. You're in: member of team-han.

  Steps 1–4 come from the node's `JoinProgress` query (PR #35). Step 4 persists across restarts. The rail then shows the space's tile with a "pending" mark, and the space's board shows "Waiting for an owner of team-han to admit you." with the actor id to share. Step 5 comes when the node sees the `member.set`, and the board opens.
- **SHL-23.** Failures:
  - `denied` at step 3: "This invite has already been used or has expired. Ask for a new one.";
  - an identity check that fails at step 2, shown in the danger tone: "The node at this address is not team-han's home. Don't use this invite, and tell whoever sent it.";
  - `unavailable` at step 1: "Can't reach the space's home. It may be offline. HAN will keep trying." The node retries automatically, and the screen shows the next attempt time and "Try now".
- **SHL-24.** `/join?ticket=…` (from a link, or a pasted URL) prefills the field and never joins without the user pressing "Join space".

### 7.5 Create a space and invite

**Create a space:**
- **Name.**
- **Home:** "This device" (its home goes offline when this device does) or one of the shared runners the identity can use (Q-SHL-5).
- **Who can approve pushes:** owners (the default), or owners and members. This sets the first `policy.approvals` entry.
- **Session rights:** everyone reads every session (the default, PLAN §1), or a custom choice of watch, read and steer.

It calls `SpaceCreate`. The new space's board opens empty (board.md §8), with "Invite someone" highlighted.

**Invite** (owners, or members when `membersMayInvite`). It is opened from the board's empty state, the palette, or the Trust screen, and calls `Invite{space, expiresAt}`:
- **Expiry:** 1 hour, 24 hours (the default), or 7 days (the maximum, N-03).
- **Admit automatically when used:** on by default, because the owner has decided by inviting (N-03 MAY). Turned off, the join comes to the inbox's Admissions (approvals.md §5).
- The result is the ticket text in a monospace field, with Copy and "Copy as link". The dialog says: "Anyone with this text can join once, until 14:30 tomorrow. Send it privately."
- **SHL-25.** The ticket text is shown only in this dialog. It is not stored by the GUI, and it is cleared from memory when the dialog closes.

### 7.6 Trust (`/space/:space/trust`)

One screen that answers "who is in this space, what can they do, and what can I do?". Its sections:

1. **You:**
   - your role;
   - whether you are limited (P-15), and why;
   - your devices that are registered for this space.
2. **Home:**
   - the server;
   - its node ids;
   - reachability;
   - the last sync;
   - the space's protocol version.
3. **People and agents.** A table with these columns:
   - actor;
   - kind (human, agent or service);
   - role;
   - operator (for agents), with a confirmed or unconfirmed mark;
   - guarded (yes, by choice; yes, forced; no);
   - live sessions;
   - joined.

   An unconfirmed operator shows in the attention tone: "the agent can't act until @ana confirms it" (P-03).
4. **Policy:**
   - join mode;
   - who may invite;
   - the approvals rules (action → approvers, separate approver);
   - session rights (watch, read, steer, modes, forced guarding);
   - the budget, with what is left;
   - the gates per repo.

   Each line is plain words, with the raw value in a tooltip.
5. **Owners only:** a role menu on each member row that calls `MemberSet`. The options follow P-03:
   - an option that would leave zero owners is disabled, with "A space needs at least one owner";
   - making an agent an owner is not offered;
   - banning asks for confirmation.

- **SHL-26.** Everything on this screen comes from the node's SpaceStatus and the log (data.md §4.6). Nothing is computed from status items.

---

## 8. Shell surface states

### 8.1 Connection banners

These are shown under the top bar of the current space, and only one at a time, the most severe first:
1. the node stopped;
2. the home is unreachable;
3. the outbox is not empty for over 60 s ("3 entries waiting to send");
4. the protocol version is newer than this app supports: "team-han uses a newer HAN version. Update to keep working with it." (PLAN §6.8).

### 8.2 Node failed

A full window: "HAN's node didn't start", the error (code and message), "Restart node", "Open log folder", and "Copy diagnostics" (no session content, SR-101). "Restart node" and "Open log folder" are the two bridge commands beyond the node API (data.md DAT-10): the first restarts the embedded node, and the second asks the OS to show the node's own log folder and no other path.

### 8.3 Node panel

Opened from the status bar. A read-only panel, fed by the `NodeStatus` query (PR #35) and the `Outbox` query, that shows:
- node id, version, data folder;
- per space: home, reachability, last sync, outbox depth, subscription count, view resyncs in the last hour;
- the most recent errors (codes only).

Of these, the node id, version, data folder, reachability and outbox depth are expected from the M3 node. Subscription counts, resync counts and recent error codes are telemetry that `NodeStatus` may not carry yet (Q-DAT-20); the panel leaves out whatever is absent rather than showing zeros.

---

## 9. Accessibility

- **SHL-27.** Landmarks:
  - the top bar is `banner`;
  - the rail is `navigation`, labelled "Spaces and surfaces";
  - the main surface is `main`;
  - the status bar is `contentinfo`.

  A "Skip to main" link is the first Tab stop.
- **SHL-28.** The palette follows the combobox-with-listbox pattern: `aria-activedescendant` on the input; result groups as `group`s with labels; and the binding of each command in its description.
- **SHL-29.** Every dialog is modal. It traps focus, labels itself with its title, returns focus to its opener on close, and gives initial focus to the least destructive choice.
- **SHL-30.** Route changes move focus to the new surface's heading and announce its title politely ("Board, team-han").
- **SHL-31.** Text size, zoom to 200% and a window width of 360 px all keep every function usable. Content reflows, with no horizontal scrolling except inside code and diffs (WCAG 1.4.10).

---

## 10. Acceptance criteria

| ID | Given | When | Then |
|---|---|---|---|
| AC-SHL-01 | fixture `team` | the user presses `mod+k` | the palette opens with focus in its input, and shows recent items |
| AC-SHL-02 | palette open | the user types `>gro` | only commands show; "Board: cycle grouping" is listed with its binding `t` |
| AC-SHL-03 | palette open | the user types `@ada` and presses Enter on her agent | the route changes to that agent's session (or a board filtered to her when she has several sessions) |
| AC-SHL-04 | the session pane is focused | the user opens the palette and runs `go.board` | the board opens; focus is on the board heading; the live region announces "Board, team-han" |
| AC-SHL-05 | the palette was opened from a board row | the user presses Escape twice (after typing) | the first press clears the query; the second closes the palette; focus returns to the same row |
| AC-SHL-06 | a user file binds `ctrl+j` to `board.next` with `focus.board` | the board has focus and the user presses ctrl+j | focus moves to the next row |
| AC-SHL-07 | a user file contains a rule with an unknown command, and a valid rule | the app starts | the valid rule works; Settings → Keybindings lists the invalid rule with its reason |
| AC-SHL-08 | the user file is not valid JSON | the app starts | the defaults apply and the Settings banner says the file was ignored |
| AC-SHL-09 | the user file contains `{key: "j", command: "-board.next"}` | the board has focus and the user presses `j` | nothing happens; ↓ still moves focus |
| AC-SHL-10 | the steering input has focus | the user types `g i` | the letters go into the input, and the route doesn't change |
| AC-SHL-11 | the board has focus | the user presses `g`, then waits 1.2 s, then presses `i` | the chord is cancelled; `i` does not navigate |
| AC-SHL-12 | the keybinding editor | the user records `j` for a command under a `when` that overlaps `board.next` | the clash message shows, offering to keep both or replace |
| AC-SHL-13 | the "Single-key shortcuts" setting is off | the user presses `?`, `:` or `g b` with no text focus | nothing happens; `mod+k` still works |
| AC-SHL-14 | the OS theme switches from light to dark while the app runs | – | the app's tokens switch without reload; an open diff re-themes (AC-DIF-18) |
| AC-SHL-15 | the token-pair test | it runs for Light, Dark and High contrast | every pair meets its contrast threshold |
| AC-SHL-16 | first run (no identity) | the app opens | after the node starts, `/welcome` shows the three choices |
| AC-SHL-17 | Create identity | the user enters an invalid handle | an inline reason shows; Create is disabled · *pending node API (Q-SHL-7)* |
| AC-SHL-18 | Create identity, keychain unavailable (mock flag) | the user submits | a passphrase step appears before the identity is created · *pending node API (Q-SHL-7)* |
| AC-SHL-19 | recovery-key step | the user types the wrong group | they can't continue; with the right group, they continue; "later" leaves a warning in Settings → Identity · *pending node API (Q-SHL-7)* |
| AC-SHL-20 | Add this device | the existing device (a second mock window) enters the code | both screens show the same 4 words; no `DevicePair` is sent until "They match" is confirmed |
| AC-SHL-21 | `/join` | the user pastes a string without the `han1` prefix | "This doesn't start with han1" shows; Join is disabled |
| AC-SHL-22 | `/join` with a valid ticket | the user joins and the mock answers each step | the step list advances through all 5; the board opens at the end |
| AC-SHL-23 | `/join`, the mock answers `denied` at step 3 | – | the used-or-expired message shows; no retry is scheduled |
| AC-SHL-24 | `/join`, the mock reports a failed home identity check | – | the danger message shows; the join stops |
| AC-SHL-25 | a join waiting for admission | the app restarts | the rail shows the space as pending; its board shows the waiting message with the actor id |
| AC-SHL-26 | `/join?ticket=…` | the route opens | the field is prefilled, and no `SpaceJoin` is sent until the user presses Join |
| AC-SHL-27 | the Invite dialog | the user generates a 24 h invite | the mock receives `Invite` with `expiresAt` 24 h ahead; the ticket shows with Copy; after closing, the dialog's DOM is gone and no storage holds the ticket · *pending node API (Q-DAT-14)* |
| AC-SHL-28 | Devices | the user revokes another device | the confirm's initial focus is Cancel; confirming sends one revoke for that key; the current device has no revoke control · *pending node API (Q-DAT-17)* |
| AC-SHL-29 | Trust, the local user an owner of a space with a single owner | the owner opens their own role menu | "member", "guest" and "none" are disabled with the at-least-one-owner reason |
| AC-SHL-30 | Trust, an agent with an unconfirmed operator | the screen renders | the row shows the unconfirmed warning in the attention tone with text |
| AC-SHL-31 | the node fails to start (mock) | the app opens | the Node failed screen shows the error and "Restart node" |
| AC-SHL-32 | the home becomes unreachable and the outbox fills | – | one banner shows, the most severe first (home unreachable), and the status bar shows the outbox depth in the attention tone |
| AC-SHL-33 | the window is 360 px wide at 200% zoom | each main surface is visited | no horizontal page scroll; every rail item is reachable |
| AC-SHL-34 | any shell screen | axe-core runs in all three themes | no WCAG 2.2 AA violations |

---

## 11. Open questions

- **Q-SHL-1.** Where does a person's profile space live when they only run the desktop app? The handle's server part must name a HAN server. Options are a team server, a shared runner, or a hosted default. This shapes the identity screen.
- **Q-SHL-2.** What is the recovery key's format (length, word list or base32), and what does "recover" do: re-admit through co-owners, or rotate keys with the recovery key (PLAN §6.2)?
- **Q-SHL-3.** *Partly resolved by PR #35:* `DeviceOffer`, `DevicePair` and `DeviceRevoke` exist. Still open: the pairing code's format, how the four check words are derived (both devices must compute the same ones), and a Devices query to list the identity's keys (Q-DAT-17).
- **Q-SHL-4.** *Resolved by PR #35:* the `JoinProgress` query.
- **Q-SHL-5.** Choosing a shared runner as a space's home at creation needs a list of runners the identity may use. Which query provides it?
- **Q-SHL-6.** Should the GUI register a `han:` URL scheme so that `han1…` tickets open `/join`? It is convenient, but it is a new path for untrusted input into the app. It is left out until reviewed.
- **Q-SHL-7.** Creating an identity (handle, profile space, key in the keychain or a passphrase-protected file) and recovering one have no node API command yet. Their parameters and results (the recovery key comes back once) are needed for §7.2.

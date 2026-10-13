# UI spec: diff and changed-files views

Status: draft, Team A (clean room, PLAN.md §8). Behaviour and appearance only. Read with [data.md](data.md) and [session.md](session.md).

Every place the GUI shows code changes uses the views in this document:
- the session pane's Changes column (session.md §5);
- a push plan's "View changes" and an accept-work merge preview (approvals.md §3.3);
- the pairwise overlap view (board.md BRD-8).

Rendering is done by two published dependencies, which are pinned (PLAN §6.6):
- **`@pierre/diffs`** for diffs, with syntax highlighting in its worker mode;
- **`@pierre/trees`** for the file tree.

This spec states what the user sees and does. How the packages are configured to do it is for the implementer, within the constraints in §8.

---

## 1. Terms

| Term | Meaning |
|---|---|
| **Change set** | the files that differ between two points, each with a status and a patch. A point is a timeline version, a git commit, or (for the git fallback) the worktree. |
| **File status** | `added`, `modified`, `deleted`, `renamed` (with old path; it may also be modified), `mode` (mode change only, for example becoming executable), `eol` (line-ending change only), `binary` (contents are not text, TL-84), `submodule`, `symlink`. |
| **Hunk** | a contiguous range of changed lines with context, as in a unified diff. |
| **Step** | the tool call that produced a hunk (a timeline batch's `step`, TL-85/TL-91), when known. |
| **Overlap pair** | two sessions and the files on which their changes intersect (hunk tier) or conflict (conflict tier). |

---

## 2. Sources of a change set

| Use | From → to | Query (data.md §4.3) |
|---|---|---|
| session Changes, default | session fork version (base) → live head | `ChangeSet` for the session, live |
| session Changes, scrubbed | the **from** point → the **to** point | `ChangeSet` between the two points (Q-DIF-1); the points come from `Timeline` |
| git fallback | base commit → worktree | `ChangeSet` with source `git` (PLAN §6.5) |
| push plan | `from-sha` → `to-sha` of the ref move | a commit-range change set (Q-DIF-2) |
| accept-work preview | main's head → the result of merging the session's timeline into main, with conflict hunks | a merge-plan change set (TL-71…TL-87, Q-DIF-2) |
| overlap pair | for each side: its base → its head, restricted to the shared files | `Overlaps{space}`, plus each side's `ChangeSet` for those files |

The change-set **metadata** is the same in every case:
- for each file: the path, the old path, the status, the counts of added and removed lines, binary sizes, modes, and line endings;
- for each hunk, when known: its step;
- for the change set: its source label ("timeline", "git", "merge preview").

**Patches** are loaded per file, lazily, with the `Patches` query (§5.6). `ChangeSet`, `Patches` and `Timeline` are the query seams of the M3 node (PR #35). Their fields beyond what PR #35 fixes are provisional (data.md DAT-3).

---

## 3. Layout

```
┌ Changes ───────────────────────────────────────────────────────────────────────────────┐
│ base a1b2c3 → live · 4 files +61 −12 · timeline     [Unified|Split] [Wrap] [±ws] [⇕] ⌕ │
├──────────────────────┬─────────────────────────────────────────────────────────────────┤
│ ⌕ filter files       │ ▾ src/board.rs                           M  +14 −2  ⚑ hunk  ⋯   │
│ ▾ src/               │   @@ −40,6 +40,14 @@ fn derive_state                              │
│   M board.rs   +14−2⚑│    40   40    let age = now − last;                              │
│   A fresh.rs   +40   │    41      −  if age > STALE {                                   │
│ ▾ tests/             │         41 +  if age > cfg.stale {                               │
│   M board.rs    +7−4 │   ⋯ 18 unchanged lines ⋯   [show]                                │
│ R docs/a.md → b.md   │ ▾ src/fresh.rs                           A  +40                 │
│ ⓘ 2 paths excluded   │   …                                                             │
│                      │ ▸ Cargo.lock                             M  +120 −98  (collapsed)│
└──────────────────────┴─────────────────────────────────────────────────────────────────┘
```

1. **Toolbar:**
   - the range (from → to) and the totals (files, +, −);
   - the source chip ("timeline", "git, ranges only", "merge preview");
   - the toggles: Unified or Split, Wrap lines, Ignore whitespace;
   - Collapse all / Expand all (⇕);
   - Find (⌕).
2. **File tree** (left, resizable, 200–480 px, default 260 px). It can be hidden with `mod+b` (when-clause `focus.changes`), and its visibility is remembered.
3. **Diff list** (right). Every file of the change set in one scroll, in tree order, each with a sticky file header.

When the Changes column is narrower than 640 px, the tree becomes a dropdown above the diff list ("4 files ▾"), and the diff is forced to Unified.

---

## 4. The changed-files tree

### 4.1 What a file row shows

- A **status letter** in a fixed-width slot:
  - A added
  - M modified
  - D deleted
  - R renamed
  - X mode change
  - E line-ending change
  - B binary
  - S submodule
  - L symlink

  The slot has the tone of the status: added in `tone-success`, deleted in `tone-danger`, others in `tone-quiet`. The full word is in its accessible name.
- The **file name**, with the directory implied by the tree. A rename shows "old → new" when both are in the same directory, and otherwise the new name with "from old/path" in the tooltip.
- **+a −d** counts. They are hidden for binary files, which show their size change ("12 → 14 KiB") instead.
- An **overlap marker** (flag and tier word) when the file is in an overlap pair of this session.
- A **conflict marker** (danger) in a merge preview when the file has conflict hunks.
- When the scrubber is off live, a dot on files changed in the selected range's **last** step, so a single step's footprint is visible.

### 4.2 Structure and controls

- Directories that contain one directory and nothing else are joined ("src/net/quic/").
- Directories show the sum of their files' counts when collapsed.
- Directories start expanded. Their expanded state is remembered per change-set source for the life of the app.
- The filter box (`/` when the tree has focus) narrows by path substring or glob. Matching parent directories stay visible.
- A sort menu offers Tree (default), Path (flat list), Most changed, and Most recent step.
- "ⓘ N paths excluded" appears when the node reports paths that were left out by the space's exclusion list or masked by the host (PLAN §6.3). It lists the count only, never the paths.

### 4.3 Interaction

- **DIF-1.** Selecting a file (click, or Enter on a focused row) scrolls the diff list so that the file's header is at the top, and focuses the header. Scrolling the diff list moves a quiet "current file" highlight in the tree. It never moves keyboard focus.
- **DIF-2.** The tree follows the ARIA tree pattern:
  - ↑/↓ move between rows;
  - → expands a directory or moves to its first child;
  - ← collapses a directory or moves to its parent;
  - Home/End go to the first or last row;
  - typing letters jumps to the next row starting with them;
  - Enter selects.
- **DIF-3.** The tree is virtualized for large change sets. With 10,000 files it keeps no more than 3 viewports of rows in the DOM.

---

## 5. The diff list

### 5.1 File headers

Each file header shows:
- the collapse toggle;
- the path (old → new for renames);
- the status letter;
- +a −d;
- the overlap or conflict marker;
- an overflow menu: Copy path, Copy patch, Go to step (when known), and Open the other session (in an overlap context).

Headers stick to the top of the list while their file is in view.

### 5.2 Lines and hunks

- Line numbers are shown for both sides (old and new) in Unified mode, and for each side in Split mode.
- Added lines use the `diff-add` background and deleted lines the `diff-del` background (shell.md §6). Changed words inside a modified line have a stronger tint of the same role. The meaning is never colour-only: Unified mode has a `+` or `−` gutter sign, and Split mode puts removed lines on the left and added lines on the right, with the sign in the gutter.
- Between hunks, a separator reads "⋯ 18 unchanged lines ⋯" with "show". Showing them loads the context lines for that file from the node (Q-DIF-3). Until loaded, the separator shows "loading…". When context is unavailable (git fallback), the separator has no "show".
- Hovering a hunk, or focusing it, shows its step on the right edge ("step: edited src/board.rs · 12:04") when known, with "Go to step" (session.md SES-22).
- The `mode` and `eol` statuses render as a single line in place of a patch: "mode 100644 → 100755", "line endings LF → CRLF".
- Binary files render a single line: "binary file, 12 KiB → 14 KiB". There is no patch.
- Submodules render "submodule a1b2c3 → d4e5f6". Symlinks render "link target old → new".

### 5.3 Collapsing

- **DIF-4.** Some files are collapsed by default:
  - deleted files over 200 lines;
  - any file whose patch is over 2,000 changed lines or 1 MiB;
  - files matching the "collapse by default" patterns, which default to common lockfiles and generated outputs (`*.lock`, `package-lock.json`, `pnpm-lock.yaml`, `Cargo.lock`, `*.min.js`, `*.map`) and are editable in settings.

  A collapsed file shows its header with "(collapsed)" and the size. Expanding a file over 2,000 lines shows "Large diff: 4,812 lines. Load anyway?" first.
- **DIF-5.** Collapse state is kept per file path and per change-set source for the life of the app, and it survives live updates and scrubbing. Collapse all and Expand all apply to every file. Expand all does not load files that need the large-diff confirmation.

### 5.4 Live updates

- **DIF-6.** In live mode, the change set updates as the timeline grows:
  - new files are inserted in tree order;
  - changed patches re-render in place;
  - deleted files are removed.

  Updates to one file's patch are applied at most 4 times a second. Later updates within that interval replace earlier ones.
- **DIF-7.** A live update MUST NOT move the viewport. The topmost visible line, identified by file and line number, stays at the same position. When that line no longer exists, the anchor is the nearest surviving line of the same file.
- **DIF-8.** A file that changed in the last 3 s shows a brief tint on its header (none under reduced motion) and its tree row gets a quiet "updated" dot until the user scrolls past it.

### 5.5 Find

- `mod+f`, with the diff list focused, opens a find bar that searches every file of the change set, collapsed files included. Opening the bar starts loading the patches of every file not yet loaded, except files that need the large-diff confirmation (DIF-4). Those are searched only once the user loads them. Unchanged lines between hunks that were never shown (§5.2) are not part of a patch and are not searched. Enter and shift+Enter move between matches. A match in a collapsed file expands that file. Escape closes the bar and returns focus to where it was.
- The bar shows "3 of 41". While patches are still loading it adds "(searching: 12 files left)", and when large files are waiting for confirmation it adds "(2 large files not searched)".

### 5.6 Loading patches

- **DIF-9.** Opening a change set loads the metadata first and renders all file headers with skeleton bodies. Patches load for the files in or near the viewport (one viewport of overscan) and for files the user expands. A file's skeleton keeps the height that its line count predicts, so loading doesn't shift the scroll.
- **DIF-10.** A patch that fails to load shows "Couldn't load this file's changes (<error>)" with Retry, in place of its body. Other files are unaffected.

### 5.7 Settings

These are remembered per user (data.md §8):
- **Unified or Split:** the default is Split when the diff list is at least 1,100 px wide, otherwise Unified;
- **Wrap lines:** off by default;
- **Ignore whitespace:** off by default. When it is on, whitespace-only changes are hidden, and the toolbar shows "whitespace hidden";
- **Word-level highlighting:** on by default;
- **Collapse-by-default patterns.**

---

## 6. Plan changes and merge previews

- A **push plan** ("View changes" on a git.push card) opens a dialog with the diff list for `from-sha → to-sha`. The title is "Changes pushed by this plan". A force move (not a fast-forward) shows a danger banner: "This push rewrites history: 2 commits on the remote are not in the new ref." The commits being pushed are listed above the tree, one line each with short sha, subject and author. Subjects render as text.
- An **accept-work preview** shows the result of merging into main. It adds a conflicts panel above the tree that lists each conflict:
  - content hunks;
  - modify/delete;
  - rename;
  - path clashes (TL-87).

  Each conflict comes with its two sides. Conflict hunks render as three blocks, main's, the incoming one and the base, each labelled, with the line ranges. The preview is read-only. Resolving conflicts is out of scope for this version (Q-DIF-4).

---

## 7. The pairwise overlap view

Opened from a board overlap entry (BRD-8) or from a file's overlap marker. It compares two sessions, A and B, on the files where they overlap.

```
┌ Overlap · hunk tier ───────────────────────────────────────────────────────────────────┐
│ A  lars-coder (@lars) #41 · working        B  ada-bot (@ada) #41 · waiting-approval     │
│ shared files: src/board.rs (2 ranges) · tests/board.rs (1)  keys [ ]  [◀ prev] [next ▶] │
├─────────────────────────────────────────────┬──────────────────────────────────────────┤
│ src/board.rs — A's change (base a1b2c3)     │ src/board.rs — B's change (base a1b2c3)  │
│   40  let age = now − last;                 │   40  let age = now − last;              │
│   41 −if age > STALE {                      │   41 −if age > STALE {                    │
│   41 +if age > cfg.stale {        ▐overlap▌ │   41 +if age >= STALE_MS {     ▐overlap▌ │
│   …                                         │   …                                      │
└─────────────────────────────────────────────┴──────────────────────────────────────────┘
│ [Open A's session] [Open B's session]                    Conflict check: no conflict ✓ │
```

- **DIF-11.** The two columns show each side's own change (its base → its head) for one shared file at a time. Lines in the intersecting ranges carry an "overlap" band in the attention tone, with text, and are linked: scrolling one column to an overlapping range scrolls the other to its counterpart.
- **DIF-12.** When the two sessions have different base commits, a notice reads "Different bases: A from a1b2c3, B from d4e5f6. Line numbers are per side." Linking is then by range order rather than by line number.
- **DIF-13.** For a **conflict**-tier pair, the footer shows the conflict check result from the node (a merge simulation, PLAN §6.6), and "Show conflict" opens the conflict hunks as in §6. For the **claim** tier with no shared files, the view shows only the claim facts ("both sessions claim #41"; "path lease on src/auth/** by B, expires 13:00") and no columns.
- **DIF-14.** `]` and `[` (or the buttons) move to the next or previous shared file. Inside either column, `n` and `p` keep their diff meaning (next or previous hunk, DIF-21). The header names both sessions with their state, and buttons open either session pane.

---

## 8. Rendering constraints (for the implementer)

- **DIF-15.** Syntax highlighting and diff tokenisation run off the main thread, in the package's worker mode. One worker pool is shared by the whole app. A single input-to-paint frame on the main thread MUST stay under 50 ms while a 5,000-line patch renders (measured in the AC run).
- **DIF-16.** The diff and tree widgets are mounted imperatively through React 19 **ref callbacks with a cleanup return** (PLAN §6.6.1). There is no `useEffect`. Their inputs come from atoms, and an update to an atom updates the widget through the widget's own update method, not by remounting.
- **DIF-17.** Colours come from the theme tokens (shell.md §6). Switching theme re-themes the open diffs without remounting them and without losing scroll or collapse state.
- **DIF-18.** The webview's Content Security Policy must allow the workers (same-origin worker scripts, and blob workers only if the package needs them) and nothing remote (SR-95). The early spike (PLAN §6.6) confirms that worker highlighting works in WebView2, WKWebView and WebKitGTK. If a webview can't run workers, highlighting is disabled there, and diffs still render as plain text, with the toolbar chip "highlighting off".

---

## 9. Surface states

| State | Appearance |
|---|---|
| **Loading** | the toolbar shows the range; the tree and the list show skeletons. After 3 s: "Waiting for the node…" |
| **Empty** | "No changes between these points." For a session at live: "No changes yet." The tree is hidden. |
| **Live** | as specified |
| **Stale** | the session is stale or failed: a quiet chip "not updating (session stale)"; everything else is readable |
| **Partial** | the git fallback: the "git, ranges only" chip; separators have no "show"; steps are not shown |
| **Error** | metadata failed: an error panel in place of the tree and list, with Retry. A single patch failed: DIF-10. |

---

## 10. Accessibility

- **DIF-19.** The visual diff can be hard to follow with a screen reader, so every diff has an **accessible diff** mode (toggle `alt+a`, also in the toolbar overflow). It renders the same change set as plain HTML:
  - one heading per file;
  - one list per hunk;
  - one item per line, beginning "added", "removed" or "unchanged" plus the line number.

  It is fully readable with standard reading keys. Its setting is remembered.
- **DIF-20.** In the visual mode, each file is a `region` labelled "<path>, <status>, <a> added, <d> removed". Each hunk separator is a button with a label such as "Show 18 unchanged lines".
- **DIF-21.** Keyboard, when-clause `focus.diff && !focus.textInput`:
  - `n` / `p`: next or previous hunk;
  - `shift+n` / `shift+p`: next or previous file;
  - `o`: collapse or expand the current file;
  - `u`: Unified or Split;
  - `alt+z`: wrap;
  - `t`: focus the tree;
  - `mod+f`: find.

  The current hunk has a visible focus outline.
- **DIF-22.** Diff colours meet 3:1 contrast against the page for the line background, and 4.5:1 for text over it, in both themes.

---

## 11. Acceptance criteria

| ID | Given | When | Then |
|---|---|---|---|
| AC-DIF-01 | fixture `diff-basic` (4 files: A, M, D, R) | the Changes column opens | the tree shows the 4 files with status letters A, M, D, R and their counts; the toolbar totals match their sum |
| AC-DIF-02 | `diff-basic` | the user selects a file in the tree | the diff list scrolls so that file's header is at the top; focus is on that header |
| AC-DIF-03 | `diff-basic` | the user presses `n` three times in the diff list | focus moves through hunks in order across files |
| AC-DIF-04 | `diff-large` (a 6,000-line file and `Cargo.lock`) | the column opens | both files are collapsed; expanding the large file asks to load it first |
| AC-DIF-05 | `diff-basic` | the user collapses file 2, then the mock updates its patch | file 2 stays collapsed |
| AC-DIF-06 | `diff-live`, the user reading line 120 of file 3 | the mock inserts 30 lines in file 1 | line 120 of file 3 stays at the same viewport position (±1 px) |
| AC-DIF-07 | `diff-live` | the mock sends 20 updates to one file within 1 s | the widget's update method is called no more than 4 times for that file in that second; the final render equals the last update |
| AC-DIF-08 | `diff-basic` | the user toggles Split, Wrap and Ignore whitespace, then reloads | the three settings persist |
| AC-DIF-09 | `diff-binary` | the column opens | the binary file shows its size change and no patch; the mode-change file shows its single mode line |
| AC-DIF-10 | `diff-basic`, one collapsed file whose patch has not been loaded | the user opens find, types a word present in that file's patch, and presses Enter until it is reached | the mock receives a Patches request for the file when the bar opens; once it answers, the collapsed file expands and the match is highlighted; the bar shows "k of n" |
| AC-DIF-11 | `errors`: one file's patch fails | the column opens | that file shows its error with Retry; the others render |
| AC-DIF-12 | `diff-huge-tree` (10,000 files) | the user scrolls the tree from top to bottom | no more than 3 viewports of tree rows are in the DOM at any time |
| AC-DIF-13 | `overlap-pair` (hunk tier, same base) | the overlap view opens | two columns show each side's change for the first shared file; the intersecting lines carry the "overlap" band with text; `]` moves to the next shared file, and `n` moves to the next hunk within the focused column · *pending node API (Q-DAT-19)* |
| AC-DIF-14 | `overlap-pair-different-base` | the view opens | the different-bases notice names both bases · *pending node API (Q-DAT-19)* |
| AC-DIF-15 | `overlap-claim-only` | the view opens | only the claim facts show; there are no diff columns · *pending node API (Q-DAT-19)* |
| AC-DIF-16 | `push-plan` with a non-fast-forward move | "View changes" opens | the force banner shows; the commit list renders subjects as text · *pending node API (Q-DIF-2)* |
| AC-DIF-17 | `merge-preview` with one content conflict and one modify/delete | the preview opens | the conflicts panel lists 2 conflicts, labelled with their kinds; the content conflict renders three labelled blocks · *pending node API (Q-DIF-2)* |
| AC-DIF-18 | `diff-basic` | the user switches theme | the diffs re-theme without remounting (the widget's mount callback ran once), and scroll and collapse state are kept |
| AC-DIF-19 | `diff-basic` | the user presses `alt+a` | the accessible diff renders headings per file and items that begin with "added", "removed" or "unchanged" |
| AC-DIF-20 | `diff-5000` | a performance trace records rendering of the 5,000-line patch | no main-thread task is over 50 ms |
| AC-DIF-21 | `diff-excluded` (the node reports 2 excluded paths) | the column opens | "2 paths excluded" shows, and no excluded path appears anywhere in the DOM · *pending node API (Q-DAT-18)* |
| AC-DIF-22 | any diff fixture | axe-core runs on the visual and the accessible diff in both themes | no WCAG 2.2 AA violations |
| AC-DIF-23 | `session-git-fallback` | the column opens | the "git, ranges only" chip shows; separators have no "show"; no step labels appear |

---

## 12. Open questions

- **Q-DIF-1.** *Partly resolved by PR #35:* the ChangeSet and Patches query seams exist. Still open: their fields, namely the per-file metadata of §2, the hunk step attributions, and whether a change set takes any two points (**from** and **to**) rather than only base and a point.
- **Q-DIF-2.** Push-plan and merge-preview change sets: does the ChangeSet seam (PR #35) also cover a commit range (with its commit list) and a merge plan between a session timeline and main (with its conflicts), or are these new Queries?
- **Q-DIF-3.** "Show unchanged lines" needs file content at a version. Does the node expose `Timeline` file contents by path and range, or only patches with a fixed amount of context? If the latter, this spec's separators should not offer "show".
- **Q-DIF-4.** Resolving merge conflicts in the GUI (choosing `target` or `incoming` per conflict, TL-87) is left out. Is it wanted for the GUI's first version, given that only the gated accept effect writes a merge?
- **Q-DIF-5.** Does `@pierre/diffs` take unified patches, old and new contents, or both, in the pinned version? The node's per-file patch format (data.md §4.3) should match what the package takes, to avoid a conversion step in the GUI. The spike should settle this.

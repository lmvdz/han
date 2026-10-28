# Extension vectors for `han.timeline/1` §21–23

Vectors 68–87 for touch sets and overlap (TL-130–138), turn marks (TL-140–147) and notes (TL-150–157). They use the format of [`../README.md`](../README.md) (`han-timeline-vector/1`: replicas, batches, runs, and the same notation) and add the expectation keys below. Every vector has two runs, delivering the batches in order and in reverse; both must reach the same expectations (TL-135).

**Status: not wired into any harness.** The existing runner reads only the top level of `../` and rejects expectation keys it does not know, so these files do not run today and cannot break it. Team B extends the harness to understand the keys below, then moves these files up one level. **No implementation has checked these vectors.** The expected values, the digests included, were derived by hand from the profile and re-derived by a second reader; a disagreement Team B finds is a question to Team A about the profile or the vector, answered by a written revision (PLAN.md §8).

## Conventions

- Lists are compared **in the order given**, entries in the order the profile fixes (TL-130, TL-133); lists inside an entry (`change`, `base_lines`) are in the order written. `change` names are listed in the order `created, deleted, renamed, mode, eol, edited, binary_changed`.
- `touch: []` and `overlap: []` mean the result **must be empty**; a vector that does not check one leaves the key out. `no_touch_set` lists timelines of a history that is not a worktree history (TL-138): asking for their touch set must report that there is none.
- Labels (`4@A`, `4@A#0:2`) are expanded by the harness to full replica ids wherever they appear in an expectation or in a note value (`object`, `c`); inside the mark values of 81–82 the full text forms of TL-06 and TL-142 are written out. For the digests (TL-136) every op id is the array `[actor, key, lane, counter]`.

## Added expectation keys

| Key | Check |
|---|---|
| `touch` | list of `{timeline, entries, digest}`: the touch set of the timeline over its default base (TL-132): entries as TL-130 (`object`, `base_path`, `head_path`, `change`, `base_lines`, `head_lines`; ranges `[a, b]`) and the touch digest (TL-136). A truncated result would carry `truncated: true` (TL-137); no vector here is truncated |
| `overlap` | list of `{a, b, entries, digest}`: the overlap of two timelines (TL-133): entries with a `class` member and the fields of the TL-133 table, and the overlap digest |
| `plans` | as in the base format. In an overlap vector the plan's conflicts must equal the overlap's conflicting entries under TL-134, and nothing else |
| `no_touch_set` | list of timeline ids with no touch set |
| `marks` | list of `{timeline, turns}`: `turns` maps a decimal turn number to the checkpoint version a lookup returns (TL-141) |
| `turn_of` | list of `{object, at, positions}`: for the transcript text `object` at `at`, each `{pos, turn, in_progress?}` gives the turn of the visible character at 0-based `pos` (TL-143) |
| `ignored_marks` | transcript root keys a reader ignores and reports (TL-146) |
| `notes` | list of `{at, notes, ignored?}`: at a version, the notes (root keys `note:<key>`) in ascending key order, each `{key, from?, to?, status, kind, resolved, resolved_conflict?, reply_to?, parent_missing?}` resolved per TL-152–154: `from`, `to` visible positions, `status` `placed`, `collapsed` or `orphaned`; `ignored` lists note keys reported as ignored (TL-151) |

## Index

| Vector | What it pins |
|---|---|
| 68-overlap-disjoint | different files: empty overlap, per-timeline touch sets, digests |
| 69-overlap-adjacent-lines-are-file-only | abutting edits do not meet: `file` entry, clean plan |
| 70-overlap-region-conflict | same line, different text: `region`, equal to the plan's hunk |
| 71-overlap-region-converged | same change on both sides: `converged` |
| 72-overlap-rename-and-edit-is-file-only | rename against edit: one object, no cluster |
| 73-overlap-modify-delete | delete against edit, `deleted_by` |
| 74-overlap-rename-conflict | rename against rename |
| 75-overlap-path-clash | file against directory of one name |
| 76-overlap-map-key-and-register | data history: `map_key` and `register`, no touch set |
| 77-marks-basic | `turn:N` checkpoints, an empty turn |
| 78-marks-duplicate-label-greatest-op-id-wins | duplicate label from two writers |
| 79-rewind-view-is-read-only | state at a mark; heads unchanged |
| 80-rewind-fork-leaves-original | fork at a mark; original head kept |
| 81-turn-of-position | transcript position to turn, turn in progress |
| 82-malformed-transcript-mark-is-ignored | malformed mark ignored, others resolve |
| 83-note-survives-edits | shift, widen, edge inserts stay outside, collapse |
| 84-note-on-binary-is-orphaned | orphaned note kept |
| 85-note-resolved-flag-conflict | concurrent resolved flags, greatest write visible, conflict ends on a write that saw both |
| 86-note-with-unknown-kind-is-ignored | unknown kind ignored, batch accepted |
| 87-note-reply-and-removal | reply names its parent; removing the parent leaves the reply with `parent_missing` |

## Not covered here (Team B tests these with generated histories)

- **Truncation** (TL-137) needs 4,097 touched objects, too large to hand-author.
- **Overlap equals planning** (TL-134) as a property: for random pairs of timelines of random histories under random delivery orders, the overlap's conflicting entries equal the plan's conflicts under the TL-134 mapping.
- Multi-cluster files, where the head-range shift of TL-131 matters (an edit that removes an LF before a later cluster).
- Region entries in data histories and for objects no tree path refers to; `modify_delete` with register content; the TL-133 `rename` count rule beyond 74.
- A transcript with no marks; gaps in turn numbers; marks whose `end` names no held character (unresolved); notes with `unknown_anchor`.

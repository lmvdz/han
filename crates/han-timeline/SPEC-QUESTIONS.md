# Questions on `han.timeline/1` (Team B → Team A)

Questions found while implementing `crates/han-timeline` from `spec/profiles/han.timeline-1.md`
and its vectors.

## Answered by the spec revision (PR #26)

Q1–Q9 of the first round are answered in the spec text, and the implementation follows it:

- **Q1** (`merge_order`): TL-72 step 2 now keeps the longest order-agreeing subsequence, and
  step 7 rewrites regions on its own terms. The local error `merge_order` is gone.
  `tests/random.rs` asserts that every planned merge records, and that every merged text reads
  exactly what steps 7–8 promise (`MergePlan::expected_texts`).
- **Q2, Q3**: TL-76 is judged on canonical ids.
- **Q4**: conflict-record keys.
- **Q5**: TL-83 for merges.
- **Q6**: hidden duplicates in step 7.
- **Q7, Q8**: TL-101 Counters step, vouching, authorisation before the genesis.
- **Q9**: shape limits.

Implementing TL-72 dropped the earlier tombstone-revival anchoring. Copies follow step 7.

## Q10. TL-72 step 7: anchoring an empty target region (answered, PR #28)

The recommended anchor "after the target character that holds the nearest kept character" could
be a copy placed elsewhere by an earlier merge, which broke the region's reading in about
1 in 10,000 randomized worktree merges. Team A adopted the position-based anchor:
- the run goes after the last character of the region's prefix still visible after the merge;
- otherwise after an earlier run of the merge, when no visible character lies between;
- otherwise at `start`.

The prefix is the target's kept characters from base lines before the region, plus its edits
whose line ranges end at or before the region's first line. Vector 63 pins it, and the
implementation follows the text.

## Revision 2026-10 (TL-49, TL-64, §21 – 23)

Vectors 01 – 67 pass unchanged; TL-49 and TL-64 needed no engine change. Vectors 68 – 87 first
ran from `extensions/`; Team A has since moved them up (with vector 88), and
`behaviour_vectors` runs all of 01 – 88.

**Answered by the revision "answers to Q11-Q25"** (spec commit `f337995`), and implemented:

| Question | Answer in the profile | Engine |
|---|---|---|
| Q11 | vector 71 revised: `up_to_date` (TL-71) | unchanged; vector passes |
| Q12 | TL-87: modify/delete only when the removing side refers to the object by no path; a rename is not a removal | planner fixed; vector 72 passes |
| Q13 | fix accepted; Team A will add a vector | regression test kept (`tests/time_travel.rs`) |
| Q14 | TL-133: `modify_delete` and `rename` are what TL-87 reports for `B` into `A`; `rename` and `path` may differ when swapped | symmetry test exempts them (see Q26) |
| Q15 | TL-134a | implemented, with "up to date" as TL-70 throughout (see Q26) |
| Q16 | TL-87: visible text or register value | unchanged |
| Q17, Q18 | TL-130 `mode`/`eol` and `edited` | unchanged |
| Q19 | §18 lists `needs_base`, `unknown_mark` | documented in `Error` |
| Q20 | TL-137 | unchanged |
| Q21 | TL-142 text forms, TL-143 skipped marks and the last mark | unchanged |
| Q22 | TL-151: crossed anchors collapse | unchanged |
| Q23 | TL-153: any other value is false | unchanged |
| Q24 | TL-154: a present but ignored parent is not missing | unchanged |
| Q25 | TL-156 counting and writer refusals | unchanged |

The `DISPUTED` list in the harness is empty; vectors 68 – 87 pass in full. The original
questions follow for the record.

### Q11. Vector 71: a plan whose only cluster converged is up to date (suspected vector bug)

Vector 71 expects `plans: [{into: 8@A, from: 8@C, kind: "merge", conflicts: []}]`. Both sides
made the same change to line 1 and nothing else. By TL-72 step 6 the one cluster is
**converged, nothing to do**. No object needs a change. TL-71: "A plan with no changes and no
conflicts is up to date." So the plan is `up_to_date`. (Vector 36 is a `merge` only because
its session also has a clean change.) The overlap's `converged: true` region and both touch
sets pass. Proposed fix: `kind: "up_to_date"` in both runs.

### Q12. Vector 72 and TL-87: does a rename against an edit raise modify/delete?

TL-87 defines modify/delete as "one side removed path `p` (absent, present in the base) while
the other side kept `p` and its content object's content differs". In 72, `s1` renames
`src/a.txt` to `src/b.txt`, so `p` is absent on `s1`. `s2` kept `p` and edited its object. Read
literally, that is a modify/delete, and TL-133's `modify_delete` row says the same. But TL-87
also says "a file renamed on one side and edited on the other merges cleanly", and vector 72
follows that sentence (`file` entry, no conflicts). The engine implements the definition, as
the TL-87 planner always has. The plan has one modify/delete (`deleted_by: target`), and the
overlap reports `modify_delete` (`deleted_by: a`). That is consistent under TL-134. Resolving
`target` gives exactly the clean result: `src/b.txt` reading `a\nY\nc\n`. Conservative reading
kept: a reviewer sees a conflict whose resolution loses nothing. Proposed fix: restrict TL-87
(and TL-133) to a side that refers to the content object by **no** path, so a rename is not a
removal. Vector 72 would then pass as written.

### Q13. A version across an import (an engine fix, and a vector suggestion)

The state at a version whose closure holds both a merge that imported object X (`create:
true`) and X's own create op on the other timeline depended on what had been read before. A
cached state that already held the import could have X's create op applied after it, and the
create replaced the object. Fixed: a create never replaces an existing object; it adds its
initial insertion or write (TL-60). `tests/time_travel.rs` has the regression test. Merge bases
across timelines are such versions, so the bug made overlap differ between delivery orders
(TL-135). Suggestion: a vector that reads such a version after a head.

### Q14. Rename and path entries depend on direction; TL-133's symmetry and rename count

TL-133 says overlap is symmetric and counts a rename by applying `B`'s path changes to `PA`
("remove `P0 − PB`, add `PB − P0`"). That count is symmetric. TL-134 requires the entries to
equal the planner's conflicts. TL-87 judges renames and path clashes "after the clean changes",
and a conflicting key keeps the target's value, so the result depends on direction. Example:
base `{a → X, img.bin → Y}`; `A` copies `X` to `lib` and sets `img.bin → Z`; `B` sets `img.bin
→ X`. Merging `B` into `A`: `img.bin` conflicts and keeps `Z`, so `X` has `{a, lib}`. That is 2,
not more than `|PA| = |PB| = 2`: no rename. Merging `A` into `B` keeps `img.bin → X`, and with
the clean `lib` that gives `{a, img.bin, lib}`: a rename. TL-133's count gives `{a, lib,
img.bin}`, a rename both ways. In `tests/overlap.rs` over 60 seeds, 702 timeline pairs had
rename or path entries that differ by direction. In 330 cases the planner's renames differ
from TL-133's count. All other classes were symmetric in every case. The engine follows TL-134
(the planner) and records this. Which should win: TL-87's "after the clean changes" or
TL-133's count? Either way the two rules should be written as one.

### Q15. Overlap when one direction is up to date

TL-134 binds overlap to "the plan that merges `B` into `A`" only "between which a merge can be
planned". When that merge is up to date but `A` into `B` can be planned (`A` already merged all
of `B` and moved on), the base still differs from head `B`. Read literally, TL-133 then gives
entries the `B`→`A` plan never shows. The engine then reads the `A`→`B` plan with the sides
swapped. That keeps TL-134 and symmetry for the pair in either order. Please confirm.

### Q16. `modify_delete`: "a TL-72 edit" or "content differs"?

TL-133 says the other side's content differs by "a TL-72 edit of a text". That is identity
based: an edit that restores the old text still counts. TL-87 says "content differs from the
base's (text or value)", and the planner compares visible text. The two disagree when an edit
leaves the text as it was. The overlap follows the planner (TL-134).

### Q17. TL-130 `mode`/`eol` of a renamed object

"An attribute of a path differs" is undefined when the object has no path in common between
base and head. Reading: compare the entries at the paths both ends share; with none in common,
compare the entry at `base_path` with the entry at `head_path`.

### Q18. TL-130 `edited` together with `created` or `deleted`

Reading: `edited` is set only for an object present as a text file at both ends (TL-131
computes clusters only then). A `created` or `deleted` file carries whole-file ranges and no
`edited`, even if its text also changed.

### Q19. Local errors without a §18 code

A touch set of main without a base (TL-132 "MUST be given") and a rewind to a turn with no
mark have no code in §18. The engine uses `needs_base` and `unknown_mark`. A touch set of a
history that is not a worktree history is `None`, not an error (TL-138).

### Q20. TL-137 for overlap

Conflicting entries come from the planner and are never cut. Only `file` entries use the
touch sets. Reading: the overlap is `truncated` when either touch set over the merge base is,
and `file` entries come from the cut sets.

### Q21. Turn marks: what counts as a mark, and the "last mark"

Readings:
- Every root key beginning `turn:` is a mark candidate. One whose number is not canonical
  (`turn:05`, `turn:0`) or whose value fails TL-142 is ignored and reported.
- Op id and character text forms must use canonical decimals. The lane and the key are the
  last two `/` parts (a KeyId has no `/`), and the actor is the rest.
- For TL-143, a mark whose `end` this text does not hold is skipped.
- "The last mark" is the greatest turn number among the marks that resolve in the text, so
  text after it is that number plus one, in progress.

### Q22. A note whose anchors cross on one character

`from = (c, left)`, `to = (c, right)` resolves to `from > to`, yet neither character comes after
the other (TL-151). Reading: shown as `collapsed` at the smaller position, not ignored.

### Q23. A resolved flag that is not a boolean

TL-153 allows `true` or `false`. Reading: any other visible value counts as not resolved, and
concurrent values are still reported as a conflict.

### Q24. `parent_missing` for a parent that is present but ignored

Reading: "absent" is TL-36 absence of the key `note:<parent>`. A parent whose value a reader
ignores is present, so its reply is not `parent_missing`.

### Q25. TL-156 counting

"In the history" is read as the distinct note keys visible at any timeline's head. "On one
content object" counts those keys whose visible value names the object. Rewriting an existing
key adds no note. The writer also refuses a note that readers would ignore: `malformed` or
`too_large`, `unknown_anchor` for anchors absent at the head, and `out_of_range` for reversed
anchors.

Q11 – Q25 are answered (above).

## Revision "answers to Q11-Q25"

### Q26. `file` entries cannot be symmetric when `rename` entries are not

TL-133 now says swapping `A` and `B` "changes no `region`, `file`, `map_key` or `register`
entry", while `rename` and `path` entries MAY differ. But a `file` entry appears "only for an
object that has no other entry". So when a `rename` for object `X` shows in one direction only,
the other direction has no entry for `X` and, `X` being touched by both, gets a `file` entry.
The two sentences cannot both hold. In `tests/overlap.rs` (60 seeds) this happened 92 times;
every other asymmetry was a `rename` or `path` entry, and no `region`, `map_key`, `register`
or `modify_delete` entry ever differed.

Reading kept: the `file` class definition (touched by both, no other entry in this overlap).
The symmetry test exempts a `file` entry exactly when its object has a `rename` entry in
either direction. Proposed fix: add "and a `file` entry standing for an object whose `rename`
entry differs" to the exceptions, or define `file` by the absence of other entries in both
directions.

A related reading of TL-134a: its first clause says "up to date (TL-70)", and the engine reads
"can be planned" and "both are up to date" in the same TL-70 sense (the closure test, not an
empty plan under TL-71). Otherwise, when `A` holds all of `B` and the plan of `A` into `B` is
merely empty, `(A, B)` would be empty while `(B, A)` shows converged `region` entries, which
TL-133 forbids. Please confirm.

**Answered** by the revision "answer to Q26" (spec commit `41f1252`): TL-133 now exempts `rename`
and `path` entries and a `file` entry for the object of a one-sided `rename`, and TL-134a
defines "up to date" as the TL-70 closure test. Both match the engine; `tests/overlap.rs`
exempts exactly those entries from its symmetry check.

Q26 is answered.

## Vectors 68 – 88 moved up

### Q27. Vector 88 does not detect the bug it pins (a suggestion, not a failure)

Vector 88 passes, and its expectations are right by TL-60 and TL-45. It also passes on the
engine before the Q13 fix (checked by reverting `aa9249d`'s change to `state.rs`): in run 0,
main's cached head holds the merge's copy of `3@B`, and the original `text.create 3@B` applied
after it replaced the object, but the replacement reads `new\n` just as the import did, and
nothing in the vector builds on the import. The bug shows only when something does. Proposed
addition, as in `tests/time_travel.rs`: after `merge`, a batch on main (`1@A`, parents
`["5@A"]`) with `text.edit` of `3@B` inserting `"x"` anchored on the merge's copy of the LF,
`5@A#0:3` (the merge's only insert, TL-41). The state at `["4@B", "6@A"]` must then read
`new\nx` in every read order. On the old engine it would read `new\n` when main's head is read
first; that is what the analogous test in `tests/time_travel.rs` showed before the fix.

**Answered** by vector 88's revision (commit `d45f518`), which adds exactly that edit. The
revised vector passes.

### Q28. Revised vector 88 still passes on the engine without the Q13 fix

My prediction under Q27 ("it would read `new\n` when main's head is read first") was wrong
for this vector's shape. Checked by reverting `aa9249d`'s change to `state.rs`: all three runs
still pass, then the fix was restored. The cause is which cached state the old engine started
from. When it materialises the join `["4@B", "6@A"]` it starts from the largest cached state
whose heads lie inside the join. Validating `merge` caches s1's head `[4@B]`, whose closure
(`1@A`, `2@A`, `3@B`, `4@B`) is larger than main's (`1@A`, `5@A`, `6@A`). So the join starts
from s1's state and applies `5@A` and `6@A` in order, and no create op ever lands on an
existing import. The bug needs a start state that holds the import and lacks the create.

A throwaway test that rebuilt the vector's histories through the API showed the old engine
reading the join as `new\n` (instead of `new\nx`) once main's closure is the larger one. I
appended two `checkpoint` ops to the `edit` batch: `7@A` and `8@A`, join `["4@B", "8@A"]`. The
fixed engine reads `new\nx` in both shapes. Proposed revision: those two checkpoints (or any two
more ops on main after the merge), with the join at `["4@B", "8@A"]`.

This depends on how one implementation caches (TL-123 allows any cache that matches replay), so
no vector detects the bug in every implementation. Larger main and s1 sides in two vectors
would cover both choices. `tests/time_travel.rs` keeps the engine's own regression test.

**Answered** by vector 88's revision (commit `a6ce3b9`): a `marks` batch of two checkpoints on
main after the edit, join `["4@B", "8@A"]`. Checked by reverting `aa9249d`'s change to
`state.rs`: run 0 (main's head read first) fails, both join reads giving `new\n` instead of
`new\nx`. With the fix restored, `src/` has no diff and all three runs pass.

No questions are open.

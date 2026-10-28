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

Vectors 01 – 67 pass unchanged; TL-49 and TL-64 needed no engine change. Vectors 68 – 87 run in
`tests/vectors.rs` (`extension_vectors`, from `extensions/`, not moved). Three expectation keys
of two vectors disagree with the profile text. The harness lists them in `DISPUTED` and checks
that they still fail, so a revision shows up. Every other key of those vectors passes.

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

Questions Q11 – Q25 are open.

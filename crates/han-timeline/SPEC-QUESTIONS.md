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

## Q10. TL-72 step 7: the recommended anchor of an empty target region can break its reading

**Spec:** "when the target's region is empty, [the first run goes] after the target character
that holds the nearest kept character preceding the run in the incoming order, else at
`start`" (RECOMMENDED). The MUST is that the region reads exactly its incoming text, and that
nothing outside it changes.

**Observed:** the randomized tests compare each merged text with the target text after every
resolved region is replaced, computed from strings alone. Following the recommended rule, about
1 in 10,000 merges in worktree histories came out wrong. The run landed far from its region.

**Cause:** "the target character that holds" a kept character can be a copy of that base
character. A copy sits where an earlier merge anchored it (TL-43), so it can lie anywhere in the
target's order. For example, it can come before the region's lines rather than right before the
region. Observed case:
- base `"\nc\n\n\n \na\n\nx \né\n\n\nx\n\nxc\n"`;
- target `"\nc\n\n\n \na\n\nx "`;
- incoming `"\ntwc🙂 cc🙂\nx\n\nxc\nex\n"`.

The text conflict on lines 1–14 is resolved `{text: "\n éé"}`, and `ex\n` is an incoming-only
empty cluster at line 14. The target holds the kept `\n` before `ex\n` as a copy near the start.
The recommended anchor gave `"\nex\n\n éé"` instead of `"\n\n ééex\n"`.

**Reading taken:** the anchoring is only RECOMMENDED, so the implementation keeps the MUST.
- Each region has a gap in the target's sequence. A region whose target is empty goes after the
  last target character that survives the merge before that gap, else at `start`.
- A region with nothing surviving between it and an earlier inserted run continues after that
  run. This generalises the chaining rule, and vector 62 still gives `N` then `Z`.

The planner's result still reads exactly the incoming region text.

**Question:** should step 7 recommend this position-based anchor for empty target regions (or
say "holds … as a character kept by both sides")?

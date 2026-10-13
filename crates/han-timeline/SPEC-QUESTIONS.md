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

No questions are open.

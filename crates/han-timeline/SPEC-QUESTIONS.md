# Questions on `han.timeline/1` (Team B → Team A)

Questions found while implementing `crates/han-timeline` from `spec/profiles/han.timeline-1.md`
and its vectors. Each one gives the reading this implementation takes, always the most
conservative one we could find, so it can be changed by a spec revision.

## Q1. TL-72 step 2: conforming writers can produce `merge_order`

**Spec:** "the kept characters appear in the same relative order in both sequences in every
history built by conforming writers; a planner that finds otherwise MUST refuse, `merge_order`."

**Observed:** the randomized tests (`tests/random.rs`) use only this planner as writer. Three
replicas fork, edit, merge with every resolution kind, and merge criss-cross. About 1.5 % of
planned merges in data histories, and about 6 % in worktree histories, find two kept characters
in opposite orders and are refused.

**Cause:** a copy is a new insertion. It takes its place by TL-43 (newest child of its anchor),
not by its origin's place. Take an origin character `c`, inserted after `p`, and a sibling `Y`
of `c` under `p` that is newer than `c`, so `Y` comes first in any state that holds both. Step 7
anchors the copy `c'` after the target's character for `p` (or after an earlier copy). `c'` is
newer than `Y`, so `c'` comes before `Y`. A later state can show `c'` (as the canonical `c`)
together with `Y`, for example a combined base after criss-cross merges. Another timeline still
shows the original `c` after `Y`. The two sequences then disagree on `c` and `Y`.

A smaller part came from characters that the target still holds as tombstones and that step 7
copies anew ("the incoming region characters the target does not show"). This implementation now
revives such a character right after its own tombstone whenever that place lies between the
region's neighbours. This is allowed because the anchoring is only RECOMMENDED, and the region
still reads exactly the incoming text. It removes about a third of the cases in data histories.

**Reading taken:** refuse with `merge_order`, as the spec requires. Once refused, the two
timelines cannot be merged in that direction until one side rewrites the region.

**Question:** should the guarantee be weakened (and `merge_order` documented as reachable)? Or
should step 7 / TL-43 change? One option: a copy takes its origin's place, ordered by its
origin's insertion id among siblings. Another: a planner that finds disordered kept characters
treats the disordered ones as deleted and inserted rather than refusing.

## Q2. TL-76: imports from outside closure(incoming)

**Spec:** TL-71 imports "every object in `Si` absent from `St`". TL-76 says "A merge op's
`create: true` objects MUST be created by ops in closure(`incoming`)".

**Conflict:** `Si` can hold an object that an earlier merge imported into `I` from a third
timeline. Its create op is in closure⁺(`incoming`) but not in closure(`incoming`). Under the
strict reading, a merge that TL-71 plans would be invalid.

**Reading taken:** a `create: true` object must exist in the state at `incoming` with the same
kind and flags, whether created in closure(`incoming`) or imported by a merge in it. Vector 49
(`outside`) is still rejected under this reading.

## Q3. TL-76: an origin that exists only through copies

**Spec:** "its origins MUST be characters that exist in the state at `incoming` or at the op's
parent".

**Conflict:** origins are canonical. When `incoming` holds only a copy of a character (it came by
an earlier merge), the origin character itself is not in that state.

**Reading taken:** the origin's canonical id must be present (as a character or as a copy) in
that text at `incoming` or at the op's parent. The origin insertion must belong to the same text
object and must not be a copy. The copy's text must equal the origin insertion's text at those
offsets.

## Q4. TL-73 / TL-92: where a rename or path conflict records its location

The conflict record has only `key` and `lines`; rename and path conflicts are located by paths.

**Reading taken:** modify/delete puts the path in `key`; a path conflict puts its first
(shorter) path in `key`; a rename conflict puts null. Receivers do not interpret these records.

## Q5. TL-87: file entries set by a merge

TL-83 judges a file entry "in the state at the op's parent". A merge may set a path to an
object that the same merge imports.

**Reading taken:** file entries in a merge's tree changes are judged against the state after the
whole merge op is applied.

## Q6. TL-72 step 7: hidden duplicates

Step 7 deletes "every visible target character of the region whose canonical id is not in the
incoming region text". If a visible copy is deleted, its duplicate hidden by TL-45 (b) would
appear, and the region would no longer read the incoming text.

**Reading taken:** as TL-47 does for local edits, the merge also deletes every character hidden
only by TL-45 (b) that duplicates a deleted one.

## Q7. TL-101 step order for batches that can never be integrated

A batch whose parent or merge `incoming`/`base` counter is at or above its own `start` would be
held back forever by step 7, before step 8 rejects it.

**Reading taken:** such a batch is rejected `lamport` at once. It is never held back.

## Q8. TL-103: which dependencies vouch, and when authorisation is judged

**Reading taken:**
- Every dependency of TL-101 step 7 vouches, including the timeline tag and a fork's source,
  not only parents and merge dependencies. Otherwise a fork of a quarantined timeline would wait
  forever.
- When a batch arrives before the genesis batch, the history's kind (and so the scope, space or
  session) is unknown. Such a batch is held back, and its authorisation is judged when its
  dependencies arrive.

## Q9. Small shape choices

- An empty timeline name or checkpoint label, or one with control characters: `malformed`.
  Over 256 bytes: `too_large`.
- A non-tree map key between 1,025 and 4,096 bytes is `too_large`, decided at op validation
  because it needs the target's tree flag. Over 4,096 bytes is `too_large` at decoding.
- Value depth: a scalar has depth 0, and each array or map level adds 1. Deeper than 16 is
  `too_large`.
- The batch's `time` field is accepted as any text of 1–64 bytes. It is informational, and
  rejecting other RFC 3339 forms would split replicas.
- A genesis of kind `transcript` without `session`, or another kind with `session`: `genesis`.
- A hello whose ranges are adjacent (not only overlapping or out of order) is `malformed`.
- TL-86 duplicates and path clashes are computed from each path's visible value.
- Stored data is re-checked on open, except for signatures and roles. Those were checked on
  receipt, and TL-103 says integrated ops are never removed.

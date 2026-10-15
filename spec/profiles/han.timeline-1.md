# Profile `han.timeline/1`

Status: profile, draft for review (Team A of the `han-timeline` clean room, PLAN.md §6.5 and §8). **Normative for interoperability between HAN apps; carries no authority.** Policy never reads a timeline, and nothing that matters (an approval, a steer, a claim) depends on one: the approval-gated accept effect is in the log, and the merge it writes only points at it (TL-74). Source: PLAN.md §6.5.

This document states behaviour only: the data model, identifiers, versions, operations, concurrency and merge rules, sync, envelopes and storage invariants. It contains no implementation. Test vectors: [`conformance/timeline-vectors/`](../../conformance/timeline-vectors/README.md). Where this document and a vector disagree, the document wins and the vector is a bug.

Normative words MUST / MUST NOT / SHOULD / MAY follow BCP 14. Rules are numbered `TL-xx`; retired numbers are not reused. §19 lists the design choices this profile makes where a choice was open.

## Contents

1. Overview
2. Replicas and operation ids
3. History: operations, batches, versions
4. Timelines, forks and checkpoints
5. Values and objects
6. Registers
7. Maps
8. Text
9. Seals (session transcripts only)
10. Materialisation, time travel and observable state
11. Merges
12. Worktree mirroring
13. Envelopes and encoding
14. Integrating untrusted data
15. Sync
16. Storage invariants
17. Limits
18. Reason codes and local errors
19. Design choices
20. Open items

## 1. Overview

A **history** is a causal graph of **operations** (ops) about one body of work: one repository's worktree, or one session's transcript (TL-89). Every op has an **op id**: the replica that wrote it and a Lamport counter. A **version** is a set of op ids, the frontier of a causally closed set; the **state** at a version is a deterministic function of the ops in its causal past, whatever order they arrived in.

Inside a history, **timelines** are named lines of work. A timeline starts by forking another at a version. Ops on a timeline only ever have parents on that timeline, so two timelines never join histories. Work moves between timelines only by a **merge**: one op on the target timeline that names the incoming version and carries the content changes. Overlapping changes come out as reviewable **conflict hunks** that the merge must resolve explicitly.

The state is a set of **objects**: last-writer-wins **registers**, **maps** whose keys keep concurrent values, and **texts** (a sequence CRDT with tombstones, stable character identity and anchors). Session transcripts can **seal** a range so that it shows exactly what its author saw. A worktree is mirrored as a **tree** map from paths to file objects, designed from git's semantics.

Ops travel in **batches**: runs of consecutive ops by one replica, each batch in a CBOR envelope signed by the device key of a HAN actor. Replicas reconcile with two-message anti-entropy over any transport. Integration is idempotent, accepts any delivery order, and validates everything a peer sends.

```
history ── ops (id = counter @ replica) ── batches (one signed envelope each)
   │
   ├─ timeline main  ──●──●──●───────────●(merge: incoming = v, resolutions)──
   │                       \                ↑ content only, no parent edge
   └─ timeline s1           ●──●──●── v ────┘
state(version) = objects { root map, registers, maps, texts, tree }
```

## 2. Replicas and operation ids

**TL-01 Replica id.** A replica id is the triple `(actor, key, lane)`:
- `actor`: a HAN ActorId (`@name:server`, han.ts `ActorId`);
- `key`: the KeyId of the device key of that actor that signs the replica's batches (architecture.md N-01);
- `lane`: an integer in [0, 2^32 − 1] that the device chooses, so one key can serve several concurrent writers (for example several agent sessions on one host).

A device MUST NOT let two writers use the same `(actor, key, lane)` at the same time. Counters are allocated per device key, not per lane: the device keeps one durable high-water mark per key and allocates from it atomically for all the key's lanes (TL-120). Because KeyIds are single-use per actor (architecture.md §4.3), a replica id is never reused after its key is revoked.

**TL-02 Replica order.** Replica ids are totally ordered by `actor` (compared as UTF-8 bytes, unsigned, shorter prefix first), then `key` (likewise), then `lane` (numerically).

**TL-03 Op id.** An op id is `(counter, replica)` with `counter` an integer in [1, 2^53 − 1]. Op ids are totally ordered by `counter` (numerically), then by replica (TL-02). "Greater" means later in this order; this document also says "newer".

**TL-04 Lamport rule.** An op's counter MUST be greater than the counter of every op in its causal past (TL-12). It follows that if op `a` is in the causal past of op `b`, then `a < b`. A device key's counters MUST strictly increase in the order the device allocates them, across all its lanes, so no two ops of one key ever share a counter. Within that constraint a writer SHOULD take as the first counter of a new batch one more than the greatest counter it holds in the history (and above its key's high-water mark).

**TL-05 Uniqueness.** One replica id MUST NOT give one counter to two different ops. Two different ops with the same op id are an **equivocation** (TL-104). A receiver MAY flag a key that used one counter in two of its lanes.

**TL-06 Text forms.** The text form of a replica id is `actor "/" key "/" lane` (lane in decimal), for example `@alice:a.example/laptop/0`. The text form of an op id is `counter "@" replica`, for example `17@@alice:a.example/laptop/0`. Text forms are for logs, errors and vectors; the wire uses CBOR (§13). Vectors abbreviate replicas to labels (`3@A`).

## 3. History: operations, batches, versions

**TL-10 History.** A history belongs to exactly one HAN space and is identified by its 32-byte **history id**, which is the batch id (TL-93) of its genesis envelope. The genesis envelope carries an empty `history` field, because the id does not exist until the genesis batch is signed; every later envelope carries the id. Ops, versions and states are always relative to one history.

**TL-11 Batch.** Ops are written in **batches**. A batch is a run of `n ≥ 1` ops by one replica with consecutive counters `start, start+1, …, start+n−1`, and:
- a **parent version** `parents`: the parents of the batch's first op;
- each later op's only parent is the op before it in the batch;
- a **timeline tag** (TL-21);
- the ops in order.

A batch is the unit of signing, transfer and integration: a receiver integrates all of a batch or none of it (TL-100). A writer MAY write a long run as several batches; the second batch's `parents` is then the last op of the first.

**TL-12 Causal past.** The parents of an op are as in TL-11. The causal past of an op is the op and, transitively, its parents. The **closure** of a set of ops is the union of their causal pasts. Op `a` **happens before** `b` when `a ≠ b` and `a` is in the causal past of `b`; two ops are **concurrent** when neither happens before the other.

**TL-13 Versions.** A version is a finite set of op ids that is an antichain (no member happens before another), called its heads. The empty version is the **root**. The closure of a version is the closure of its heads. The **frontier** of a causally closed set is the set of its members that are in no other member's strict causal past; the frontier of a closure is a version. Two versions are equal when their sets are equal. Wherever a version is accepted as input, an implementation MUST accept any set of known op ids and use the frontier of its closure.

**TL-14 Version operations.** For versions `v`, `w`: `v` **dominates** `w` when closure(`w`) ⊆ closure(`v`); **join**(`v`, `w`) is the frontier of closure(`v`) ∪ closure(`w`); **meet**(`v`, `w`) is the frontier of closure(`v`) ∩ closure(`w`) (an intersection of closed sets is closed). Versions are written in ascending op id order.

**TL-15 Genesis.** A history has exactly one **genesis batch**: the only batch whose `parents` is the root. Its first op is a `timeline.create` with no source and with the history's `historyKind` (TL-27); that timeline is the history's **main** timeline. Every other batch MUST have a non-empty `parents`. A second batch with empty `parents` is rejected (`genesis`).

**TL-16 Op kinds.** The op kinds of `han.timeline/1` are:

| Kind | Fields | Effect |
|---|---|---|
| `timeline.create` | `name`, `source`, `historyKind?`, `session?` | creates a timeline (§4); `historyKind` and `session` only on main (TL-27) |
| `checkpoint` | `label` | names the version of its parent (TL-25) |
| `merge` | `source`, `incoming`, `base`, `changes`, `conflicts`, `effect?` | three-way content merge onto the batch's timeline (§11) |
| `register.create` | `value` | creates a register (§6) |
| `register.set` | `target`, `value` | writes a register |
| `map.create` | `tree` | creates a map; `tree: true` makes it a worktree tree (§12) |
| `map.set` | `target`, `key`, `value` | writes a map key (§7) |
| `map.delete` | `target`, `key` | removes the values of a key its author had seen |
| `text.create` | `text`, `transcript` | creates a text, optionally with initial content (§8) |
| `text.edit` | `target`, `deletes`, `inserts` | deletes and inserts characters |
| `text.seal` | `target`, `ranges` | freezes a range of a transcript (§9) |

An unknown kind is `malformed`. A future profile version adds kinds; it never changes the meaning of these.

**TL-17 Op validity is judged at the op's parent.** Every reference an op makes (a target object, a character, a timeline) MUST resolve in the state at the op's parent version (for the first op of a batch, at `parents`; otherwise at the previous op), except where a rule says otherwise (merge dependencies, TL-76). An op that fails is `invalid_op`, and its whole batch is rejected.

## 4. Timelines, forks and checkpoints

**TL-20 Timeline creation.** `timeline.create {name, source}` creates a timeline whose **timeline id** is the op id of the create op. `name` is 1–256 bytes of UTF-8 without control characters (U+0000–U+001F, U+007F): an empty name or one with a control character is `malformed`, one over 256 bytes `too_large`; names are labels, not unique, and lookups by name MUST fail when a name is ambiguous. `source` is the timeline id of the timeline it was forked from, or null only for main (TL-15). Main's create op also carries `historyKind` (TL-27); no other create op may. A `timeline.create` MUST be the first op of its batch, and that batch's tag MUST be null ("this batch creates the timeline it is on").

**TL-21 Timeline tags.** Every batch carries a timeline tag: null when its first op is a `timeline.create` (the batch is then on the timeline it creates), otherwise the timeline id of an existing timeline (a known `timeline.create` op). An op is **on** timeline `T` when its batch is tagged `T` or creates `T` (every op of the batch that creates `T` is on `T`, the create op included).

**TL-22 Forks.** A fork's `parents` (the parents of the create op) is the **fork version**. Every head of a fork version MUST be in the **reach** of the source timeline `S`: an op on `S`, or an op in the causal past of `S`'s create op. Otherwise the batch is rejected (`timeline`). So a fork can start at any version of `S`'s history, including before `S` itself forked. A fork has exactly one source: a version with heads outside one timeline's reach cannot be forked. To combine the work of two timelines, fork one and merge the other into the fork (§11).

**TL-23 No history joins.** Every parent of a batch tagged `T` MUST be an op on `T`. Otherwise the batch is rejected (`timeline`). Ops therefore never gain parents from another timeline: content moves between timelines only by merge ops (§11).

**TL-24 Heads.** The **head** of timeline `T` is the frontier of the closure of all ops on `T` that a replica holds. It is derived, never sent. Ops written concurrently on one timeline (two devices of one person, say) give a head with several ops; the next op on `T` SHOULD take that whole head as its `parents`, which joins them (this is a join inside one timeline, which TL-23 allows). Every replica that holds the same ops computes the same heads.

**TL-25 Checkpoints.** `checkpoint {label}` (label 1–256 bytes of UTF-8 without control characters, judged as names are in TL-20) names the **checkpoint version**: the op's parent version. It changes no object. The checkpoints of `T` are its checkpoint ops in op id order; navigating to a checkpoint means materialising its checkpoint version (TL-60). Labels are not unique.

**TL-26 Fork from a checkpoint or any past version.** Forking `T` "at" a version `v` MUST be refused locally unless every head of `v` is in `T`'s reach (TL-22). Forking at a checkpoint forks at its checkpoint version.

**TL-27 History kinds.** Main's create op carries the history's `historyKind`, fixed for its life:
- `worktree`: mirrors one repository (§12). Tree maps (`map.create {tree: true}`) exist only in worktree histories;
- `transcript`: one session's transcript (TL-89). Its create op also carries `session`, the EntryId of the `session.open` entry. A transcript genesis without `session`, or a genesis of another kind with `session`, is `genesis`. A transcript history has **only main**: any other `timeline.create` and any `merge` op is rejected (`timeline`). Transcript texts (`transcript: true`, TL-50) exist only in transcript histories;
- `data`: any other use, with none of these restrictions and no tree or transcript objects.

An op that creates a tree or a transcript text in a history of another kind is `invalid_op`.

## 5. Values and objects

**TL-30 Values.** A value is one of: null; a boolean; an integer in [−(2^53 − 1), 2^53 − 1]; a text string (valid UTF-8); a byte string; an array of values; a map from text strings to values. No floating-point numbers, tags or other simple values. Nesting depth ≤ 16, where a scalar has depth 0 and each array or map level adds 1, and the encoded value ≤ 64 KiB; a deeper or larger value is `too_large`. Two values are **equal** when their deterministic CBOR encodings (TL-90) are byte-equal. An **object reference** inside a value is the map `{"obj": [actor, key, lane, counter]}` naming an object id.

**TL-31 Objects.** An object is a register, a map or a text. Its **object id** is the op id of the op that created it. The **root map** has the reserved object id `root` (null on the wire), exists in every state including the root version's, and is not a tree.

**TL-32 Kinds are fixed.** An op that targets an object MUST name an object of the kind it acts on (`register.set` a register, `map.set`/`map.delete` a map, `text.edit` a text, `text.seal` a transcript text); otherwise `invalid_op`.

## 6. Registers

**TL-33 Register value.** A register holds one value. Its **writes** are its `register.create`, every `register.set` targeting it, and every merge change that sets it (TL-73). In a state, the register's value is the value of its greatest write (TL-03) in the state's closure: the greatest write wins. Since a write is greater than everything it saw (TL-04), this is last-writer-wins by op id; among concurrent writes, the greatest op id wins. A register keeps no concurrent values and reports no conflicts.

## 7. Maps

**TL-34 Map writes and removes.** For a map `m` and a key `k`, the **writes** of `(m, k)` are the `map.set` ops on it and the merge changes that set it; the **removes** are the `map.delete` ops on it and the merge changes that delete it. The **observers** of `(m, k)` are its writes and removes.

**TL-35 Concurrent values.** In a state with closure `C`, the **values** of `(m, k)` are the writes `w` in `C` such that no observer of `(m, k)` in `C` has `w` in its strict causal past. A write or remove therefore replaces exactly the values its author had seen; a value it had not seen survives.

**TL-36 Visible value and conflicts.** If the values of `(m, k)` are empty, `k` is **absent**. Otherwise its **visible value** is the value of its greatest write, and the key is in **conflict** when it has more than one value. A state reports, per map key in conflict, every value with its op id, greatest first. A later write or remove whose author saw all of them ends the conflict.

**TL-37 Observed remove.** It follows from TL-35 that `map.delete` removes only what its author had seen: a concurrent `map.set` of the same key survives (add wins). A key whose values are all removed is absent and is not reported.

**TL-38 Keys.** A map key is a text string of 1–1,024 bytes of UTF-8 (in a tree map, a path of up to 4,096 bytes, TL-82). Keys are compared and ordered as UTF-8 bytes. A key over 4,096 bytes is `too_large` when the batch is decoded; a key of 1,025–4,096 bytes on a map that is not a tree is `too_large` when the op is validated, since that needs the target's tree flag.

## 8. Text

**TL-40 Characters.** A text is a sequence of **characters**. The unit is the Unicode scalar value: every offset, position and length in this profile counts scalar values, never bytes or UTF-16 code units. Inserted text MUST NOT contain U+000D (CR); text uses LF line endings only (worktree files map line endings in §12). An op carrying a CR in inserted text is `invalid_op`.

**TL-41 Insertions.** Every inserted string is one **insertion**, identified by `(op id, index)`: the op that inserted it and its index among that op's inserts (for `text.create`, its initial text is index 0; for a merge, the index counts every insert of the op, across all its changes, from 0). Insertions are ordered by op id, then index; "newer" means greater. Character `k` (from 0) of insertion `I` has the **character id** `(I, k)`. A character keeps its id, its place and its text forever: a delete only marks it (TL-44).

**TL-42 Left origin.** Every insert names its **anchor**: the character immediately to the left of the insertion point in its author's view, which MAY be a deleted character, or `start` when the insertion point is the beginning. The anchor MUST be a character of the target text in the state at the op's parent, or a character of an earlier insert of the same op. `text.create`'s initial insertion has anchor `start`.

**TL-43 Order of characters (concurrent inserts at one position).** Every text has one total **order** of all its characters, visible or not, defined by a tree:
- the nodes are the text's characters and a root that stands for `start`;
- the parent of character 0 of an insertion is that insertion's anchor (the root for `start`); the parent of character `k > 0` is character `k − 1` of the same insertion;
- the children of a node are ordered by the insertion each child belongs to, **newest first**.

The order is the depth-first pre-order of this tree (each node before its children, children in the order above), leaving out the root. Consequences that conforming implementations exhibit, and vectors test:
- concurrent insertions at the same anchor are ordered **newest first**;
- an insertion is never split by another insertion made concurrently with it, and characters inserted later in one run of typing stay after their anchor;
- the resulting order does not depend on the order in which insertions are integrated.

**TL-44 Deletes are tombstones.** A delete names ranges `[insertion, from, to)` of characters (0 ≤ from < to ≤ the insertion's length) that MUST exist in the state at the op's parent. A deleted character stays in the order as a **tombstone**. Deleting is idempotent and commutes with everything; a delete hides only the characters it names, so an insert made concurrently inside a deleted range survives.

**TL-45 Visibility.** A character is **visible** when (a) it is not deleted, or a seal makes it visible (TL-52 decides (a) wherever a seal applies), and (b) no character before it in the order that has the same canonical id (TL-72) is visible by (a). Rule (b) hides the second copy of text that two merges brought in concurrently (two devices merging the same work into one timeline, or two concurrent imports of one object, TL-73): a timeline shows each canonical character at most once. The **visible text** is the visible characters in order. Positions (0 … length) count visible characters.

**TL-46 Anchors.** An anchor is `start`, `end`, or `(character id, bias)` with bias `left` or `right`. Making an anchor at visible position `p`:
- `left` bias: `start` if `p = 0`, else the visible character at position `p − 1` with bias `left`;
- `right` bias: `end` if `p` = length, else the visible character at position `p` with bias `right`.

Resolving: `start` is 0; `end` is the length; `(c, left)` is the number of visible characters at or before `c` in the order; `(c, right)` is the number of visible characters strictly before `c`. These formulas hold whether `c` is visible or not, so an anchor survives the deletion of its character: it collapses onto the place where the character was. Text inserted at an anchored position stays after a `left` anchor and before a `right` anchor. If `c` is not in this text but merge copies of it are (TL-72), the anchor resolves through the first visible copy in order, else the first copy. An anchor that names no character of the text is `unknown_anchor`.

**TL-47 Local edit planning.** A local edit "at position `p`, delete `d` characters, insert string `s`" of a text (0 ≤ p, p + d ≤ length) MUST be recorded as one `text.edit` whose `deletes` are the identity ranges of the visible characters at positions [p, p+d), in order, followed by every character hidden only by TL-45 (b) whose canonical id is that of one of them (so a deleted line cannot reappear from its duplicate), and whose single insert (when `s` is non-empty) has text `s` and anchor `start` if `p = 0`, else the visible character at position `p − 1`. The engine MUST normalise CRLF and lone CR to LF in `s` before recording. An edit outside the text is refused locally (`out_of_range`).

**TL-48 Shape of `text.edit`.** `deletes` ranges MUST be non-empty and MUST NOT overlap one another; `inserts` MUST be non-empty strings, and no two inserts of one op may name the same anchor. At least one of `deletes` and `inserts` MUST be non-empty. The deletes apply first, then the inserts in index order. The same rules apply to each text change of a merge op (TL-73), except that a change with `create: true` may be empty (an imported empty text).

**TL-49 Edits from an older view.** The following acceptance criteria clarify TL-17 and TL-47:
1. Planning an edit against a version `v` MUST interpret positions, lengths and range bounds in the visible text at `v` (TL-40), even when the replica holds later or concurrent ops; holding those ops MUST NOT change the planned edit.
2. When that edit is recorded with parent version `v`, integration MUST preserve its named characters and anchor as defined by TL-42–44, including when a concurrent edit has deleted the anchor; concurrent inserted characters outside its named delete ranges MUST survive unless hidden by TL-45 or TL-52.
3. A character reference absent from the op's parent state MUST be rejected (`invalid_op`) even when the receiver holds that character through another concurrent op, except for the earlier inserts of the same op allowed by TL-42.

## 9. Seals (session transcripts only)

HAN requires that a session transcript record, for audit, exactly what an agent sent and what it received (PLAN.md §6.5): once a turn has happened, a reviewer must be able to rely on the transcript showing that text and nothing else, whatever edits other replicas make at the same time. A **seal** provides this. It applies to a range of a transcript and fixes it, from then on and in every replica, to the text its author had at that moment. This is a deliberate exception to the rule that no concurrent edit is lost, so it is confined to transcripts.

**TL-50 Transcripts only.** A text is a **transcript** when its `text.create` has `transcript: true` (fixed for its life); transcripts exist only in transcript histories (TL-27). `text.seal` targets only transcripts; on any other text it is `invalid_op`. A transcript MUST NOT be referenced from a tree (TL-83).

**TL-51 Seal shape.** `text.seal {target, ranges}` names, in document order, one or more non-overlapping identity ranges `[insertion, from, to)` (from < to). In the state at the op's parent, the characters of the ranges MUST all be visible and MUST be exactly the visible characters between the first character of the first range and the last character of the last range, inclusive (one contiguous stretch of visible text). Otherwise `invalid_op`. A local seal of visible positions [p, q) (p < q) records the identity ranges of those characters.

**TL-52 What a seal does.** A seal's **sealed characters** are the characters of its ranges. Its **region** is every character strictly after its first sealed character and strictly before its last one, in the order. In a state, a character is visible when:
1. it is a sealed character of some seal in the state: **visible**, even if deleted (a concurrent delete is undone);
2. otherwise, it is in the region of some seal in the state: **hidden**, whoever inserted it and whenever (a concurrent insert inside the range does not show);
3. otherwise: visible unless deleted (TL-45).

The rule is a function of the set of ops, so it holds whatever order seals, deletes and inserts arrive in. Characters inserted exactly before the first or after the last sealed character are outside the region and behave normally.

**TL-53 After a seal.** An engine MUST refuse locally (`sealed`) an edit that would delete a sealed character or insert inside a region. A receiver MUST NOT reject such an op from a peer for that reason: it is valid, and TL-52 gives it no visible effect inside the seal. Seals never end; two seals over overlapping text both apply.

## 10. Materialisation, time travel and observable state

**TL-60 State at a version.** For every version `v` whose ops a replica holds, the replica MUST be able to produce the **state at `v`**: the objects created by ops in closure(`v`) (and imported by merges in it, TL-73), with registers per TL-33, maps per TL-35–37, texts per TL-41–46 and seals per TL-52, using only the ops in closure(`v`). The result MUST NOT depend on the order in which those ops were received or applied. This is time travel: any version, current or past, a timeline head, a checkpoint, or a version with several heads.

**TL-61 Observable state.** The observable state at a version is, for each object present, in ascending object id order (`root` first):
- a register: its value;
- a map: its tree flag and, for each present key in ascending key order, its values (op id and value), greatest first;
- a text: its transcript flag and its visible text.

Two conforming implementations holding the same ops MUST produce the same observable state at every version. Vectors compare observable states (or the subset they list). The **state digest** is `SHA-256("han-timeline-state-v1\n" ‖ dCBOR(entries))` (TL-90), where `entries` is an array with one entry per object in the order above, each `[object id, kind, content]`:
- object id: null for `root`, otherwise `[actor, key, lane, counter]`;
- kind 1 (register): content is the value;
- kind 2 (map): content is `[tree, [[key, [[op id, value], …]], …]]`, keys ascending, values greatest first, op ids as `[actor, key, lane, counter]`;
- kind 3 (text): content is `[transcript, visible text]`.

Implementations MAY expose the digest to compare replicas cheaply; vectors pin it for some states.

**TL-62 Conflicts in a state.** A state reports: map keys in conflict (TL-36) and tree conflicts (TL-86). Registers and texts never report conflicts in a state; texts conflict only in merge plans (§11).

**TL-63 Log.** The ops of closure(`v`) in ascending op id order are a valid causal order (TL-04); an implementation that lists a history SHOULD use it.

**TL-64 Historical reads and recorded reversals.** Reading an earlier version and editing the present have distinct observable effects:
1. Materialising a version or navigating to a checkpoint MUST NOT add an op, change any timeline head or change the sync inventory; subsequent reads of other versions MUST still return their TL-60 states.
2. Reversing an edit on a timeline is an ordinary new edit subject to the existing validity and seal rules (TL-17, TL-47, TL-53); it MUST NOT remove earlier ops or replace the timeline head with an earlier version.
3. A new edit that deletes an earlier insertion MUST leave concurrent insertions outside its named delete ranges subject to normal visibility (TL-44–45, TL-52), including insertions anchored on the characters it deletes; materialising the version before that new edit MUST still show the earlier insertion.

## 11. Merges

A merge moves work from an **incoming** timeline `I` to a **target** timeline `T` as content, never as history. It is planned locally (TL-70–72), reviewed (conflicts are hunks), and recorded as one `merge` op on `T` (TL-73). Receivers never re-plan: they apply the recorded changes.

**TL-70 Merge base.** Let the **extended closure** closure⁺(`v`) be the smallest set that contains closure(`v`) and, for every merge op in it, closure⁺ of that merge's `incoming`. The **merge base** of merging `I` into `T` is the frontier of closure⁺(head `T`) ∩ closure⁺(head `I`). The base thus starts at the fork point and advances past everything an earlier merge carried between the two lines in either direction, so a repeated merge brings only new work and never raises the same conflict twice (a merge base over merge edges, as in git). If closure⁺(head `T`) contains closure(head `I`), the merge is **up to date** and nothing is recorded.

**TL-71 Planning.** Planning a merge of `I` into `T` takes `St` = state at head `T`, `Si` = state at head `I`, `Sb` = state at the base, and produces a **plan**: the base, the clean **changes**, and the **conflicts** in a fixed order (by object id, `root` first; within a map, key conflicts in key order, then rename conflicts by content object id, then path conflicts by first path; within a text, by first base line), each with its candidate results. For every object in `Si`:
- **absent from `St`**: the change imports it whole, with the same object id: a register with its value, a map with the visible values of its keys (and its tree flag), a text with all its visible characters as copies (TL-72);
- **a register** (in both): with `b`, `t`, `s` its value in `Sb`, `St`, `Si` (`b` absent if `Sb` lacks it): if `s = b` or `s = t`, nothing; else if `t = b`, set it to `s`; else a **register conflict** (base `b`, target `t`, incoming `s`);
- **a map** (in both): per key present in `Si` or `Sb`, with `b`, `t`, `s` the visible values (or absent): if `s = b` or `s = t`, nothing; else if `t = b`, set the key to `s` (or delete it when `s` is absent); else a **map conflict** (key, base, target, incoming); tree maps add TL-87;
- **a text** (in both): TL-72.

Objects only in `St` are untouched. A plan with no changes and no conflicts is up to date. Planning a timeline into itself is refused (`merge_self`).

**TL-72 Text merges: copies, edits, hunks.** A merge copies characters: a **copy** is an insertion of the merge op that carries an **origin**, the character id its first character came from; character `k` of the copy has **canonical id** (origin insertion, origin offset + k). A character that is not a copy is its own canonical id. Origins MUST be canonical (never themselves copies), and a copy's text MUST equal the origin insertion's text at those offsets. Planning a text:
1. **Sequences.** For each of `Sb`, `St`, `Si`, take (empty when that state lacks the object) the canonical ids of its visible characters in order (if two visible characters share a canonical id, keep the first). `T` **holds** canonical id `c` when `St`'s text has a character (visible or not) whose canonical id is `c`.
2. **Edits.** For a side `X` in {target, incoming}, base characters whose canonical id appears in `X`'s sequence are **shared**. Shared characters usually appear in the same relative order in both sequences, but not always: a copy is placed by TL-43 as a new insertion at its anchor, not at its origin's place, so a copy and the original of the same character can sit on different sides of a third character. The **kept** characters of `X` are therefore the longest subsequence of the shared characters, taken in base order, whose order in `X`'s sequence agrees; when several are equally long, the one whose base positions are lexicographically smallest (compare the first base position, then the next, and so on). A shared character that is not kept counts as deleted by `X` at its base place and inserted by `X` at its place in `X`. Planning never fails on order. Between consecutive kept characters, and before the first and after the last, the base's skipped characters (deleted by `X`) and `X`'s skipped characters (inserted by `X`) form one **edit** of `X` when either is non-empty. An edit covers base character positions [p, q): the deleted base characters, or the empty range at the gap before the next kept base character when it only inserts.
3. **Lines.** Base lines are the base text split after each LF (the last line may lack one; an empty base has no lines). For base position `i`, line(`i`) is the number of LFs before position `i`. An edit [p, q) with p < q **touches** lines [line(p), line(q − 1) + 1). A pure insertion at gap `p` touches the empty line range [line(p), line(p)) when `p = 0` or the base character at `p − 1` is an LF (it falls between lines), and [line(p − 1), line(p − 1) + 1) otherwise (it falls inside a line).
4. **Clusters.** Two edits (of either side, or the same side) **meet** when: both line ranges are non-empty and overlap (a₁ < b₂ and a₂ < b₁); or both are empty at the same line; or one is empty at line `x` and the other is non-empty [a, b) with a < x < b. Edits on neighbouring lines that do not overlap do not meet. The connected groups of edits under "meet" are **clusters**. A cluster's **region** is the union of its edits' line ranges, [a, b) (empty, [x, x), when all its edits are empty at `x`).
5. **Region texts.** For a cluster and a side `X`, the **region text** is `X`'s visible characters that are kept base characters in lines [a, b) or inserted by `X`'s edits in the cluster, in `X`'s order. The base region text is the base lines [a, b).
6. **Outcome per cluster.** Only target edits: nothing to do. Only incoming edits: a clean change that makes the target's region text equal the incoming region text. Edits of both sides with equal target and incoming region texts (as strings): **converged**, nothing to do. Otherwise a **text conflict hunk**: object, path if the object is a tree's file (TL-83), base line range [a, b), and the three strings base, target, incoming.
7. **Making a region equal a side's region text** (clean incoming changes, and the `incoming` resolution). Each such region is rewritten on its own; for each one the merge MUST:
   - keep every visible target character of the region that is kept by **both** sides (step 2) and whose canonical id is in the incoming region text;
   - delete every other visible target character of the region, together with every character hidden only by TL-45 (b) that duplicates one it deletes;
   - insert every character of this region's incoming region text that the first point did not keep,

   so that the region then reads exactly its incoming region text and nothing outside the region changes. (Keeping by both sides matters: a character the incoming side moved is not kept by it, so it is deleted here and re-inserted at its incoming place, possibly in another region.) Inserted characters are copies with canonical origins, except a character whose canonical id the target still shows outside this region once all the merge's other changes are applied; that one is inserted as text without an origin, because TL-45 (b) would hide a copy of it. The RECOMMENDED anchoring, which achieves this because the merge op is newer than everything the target holds: a run of inserted characters whose preceding incoming region character is kept goes after that target character, or after the previous run when that is nearer; the first run of a region goes immediately before the target's first region character in the target's order (after that character's predecessor, or `start`), or, when the target's region is empty, at the region's **gap**. The gap is placed by position, never through a character that merely holds a canonical id (a copy can sit anywhere in the target): the target's visible text splits into a prefix that comes before the region (the characters the target keeps from base lines before the region, and the characters of the target's edits whose line ranges end at or before the region's first line) and the rest, and the gap lies between them. The run goes after the last character of that prefix that is still visible after the merge, or after an earlier run of this merge when no such character lies between that run and the gap, else at `start`. Because the merge op is newer than everything the target holds, a run placed immediately after a character is, in the visible text, right after it, so the run lands exactly at the gap. One copy covers consecutive characters of one origin insertion. **Chaining:** no two inserts of one merge op may name the same anchor (TL-48), and two runs at one anchor would also come out newest first, in the wrong order. So whenever this anchoring would give a run the same anchor as a run placed earlier in document order (for example an incoming-only empty cluster and the next line's cluster, both after the same line end), the later run MUST be anchored after the last character of the run just before it in document order. More generally, a run with no character still visible after the merge between it and an earlier run of the merge continues after that earlier run.
8. **Replacing a region with a given string** (the `text` resolution): delete every visible target character of the region (and its hidden duplicates, as in step 7) and insert the string as one insertion without origin, at the place step 7 gives the first run, chained as step 7 says.

**TL-73 The merge op.** A merge is recorded as `merge {source, incoming, base, changes, conflicts, effect?}`, alone or with other ops, in a batch on `T`:
- `source`: the incoming timeline id; `incoming`: the incoming version that was planned (normally head `I`); `base`: the merge base (TL-70);
- `changes`: what to apply, per object: register `{object, create, value}`; map `{object, create, tree, sets: [[key, value]…], deletes: [key…]}`; text `{object, create, transcript, deletes, inserts: [[anchor, text, origin?]…]}`. `create: true` imports the object under its own id (TL-71);
- `conflicts`: for each conflict of the plan, in plan order, its kind, location (object, and key, base line range or paths) and the **resolution** taken (TL-75);
- `effect`: the EntryId of the HAN effect that authorised the merge (TL-74).

Applying it, as the op it is (op id `m`): a register change is a register write with id `m`; map sets and deletes are writes and removes of `(object, key)` with id `m` (so by TL-35 they replace exactly the values in `T`'s history); a text change applies its deletes, then its inserts in index order, each a normal insertion `(m, j)` placed by TL-43. A change with `create: true` creates the object if the state lacks it, and otherwise applies to the existing object (two replicas may import the same object concurrently on one timeline). A merge makes no op of `I` a parent of anything: its batch's `parents` are ops on `T` (TL-23).

**TL-74 Who writes main in a worktree history.** In a worktree history (§12), the only batches on main SHOULD be:
- merges written as the execution of an approved accept effect (PLAN.md §6.2 gates), each carrying that effect's EntryId in `effect`; and
- **mirror batches** (TL-88) by the actor the space designates as its runner or executor, whose ops only create or change content objects referenced from the tree, set or remove tree entries, and set the root map's `git.commit` (the genesis batch is one).

An accept effect's plan MUST name the history, the target timeline, the incoming version and the **merge digest** `SHA-256("han-timeline-merge-v1\n" ‖ dCBOR([history, target, incoming, base, changes, conflicts]))`, with every op id written as a full `[actor, key, lane, counter]` (never a replica-table index): an insertion ref as `[op id, i]`, a character ref as `[op id, i, k]`, a delete range as `[op id, i, from, to]`, an object ref as an op id or null for `root`. A reference to the merge op itself (a copy run anchored on an earlier insert `(m, j)` of the same op) writes null in place of the op id, the reserved **this-op** marker, because the merge op's counter is chosen only when it is written; object refs never name the merge op, so null there still means `root`. The effect's `planDigest` thus binds exactly the merge that may be written, whatever counter it later gets. `envelopes.json` E11 pins one merge digest. The plan carries digests of these CBOR structures, never text or positions. Receivers SHOULD flag any other batch on main and any merge whose `effect` is missing or whose changes do not match the approved merge digest, and SHOULD check mirror batches against git when they can. This profile carries no authority: these are flags, not rejections.

**TL-75 Conflicts are resolved explicitly.** A merge whose plan has conflicts MUST NOT be recorded until every conflict has a resolution; recording without one is refused locally (`unresolved`). Resolutions:

| Conflict | Resolutions |
|---|---|
| register | `target` (keep), `incoming` (set to incoming), `{value: v}` |
| map key | `target`, `incoming` (set or delete as incoming has it), `{value: v}` (set) |
| text hunk | `target`, `incoming` (TL-72 step 7), `{text: s}` (TL-72 step 8; `s` may be empty, which deletes the region) |
| modify/delete, rename, path (TL-87) | `target`, `incoming` |

Every resolution's result is part of `changes`; `conflicts` records which was taken, for review. The planner MUST show each text hunk as its three strings and line range.

**TL-76 Dependencies of a merge.** A merge op depends on the ops of its `incoming` and `base` versions, and on the origins of its copies. A receiver MUST NOT integrate a batch holding a merge op before it holds all of them (TL-102), and every counter in `incoming` and `base` MUST be less than the batch's `start` (`lamport` otherwise), so ascending `start` order is a causal order that includes merge dependencies. A merge's validity is judged on canonical ids, so it holds across any number of timelines (an object or a character may have reached the incoming side through earlier merges):
- every `create: true` object MUST exist in the state at `incoming` with the same kind and flags (`tree`, `transcript`), whether its create op is in closure(`incoming`) or a merge in that closure imported it;
- every origin MUST be a character of an insertion that is not itself a copy and that belongs to the same text object, and its canonical id MUST be present in that text, as the character or as a copy of it, in the state at `incoming` or at the op's parent;
- every copy's text MUST equal its origin insertion's text at those offsets.

Every op these checks read is held: closure⁺(`incoming`) (TL-70) is held once `incoming` is, because each merge in it was integrated only after its own dependencies.

**TL-77 No fast-forward.** A merge is always a merge op, even when `T` has no work of its own since the base. (Its result then equals the incoming content.)

## 12. Worktree mirroring

A HAN host mirrors a git worktree into a history, recording every change whatever made it (PLAN.md §6.5). This section fixes how files map onto objects, designed from git's semantics.

**TL-81 Layout.** A worktree history's root map holds:
- `tree`: an object reference (TL-30) to the history's single tree map (`map.create {tree: true}`);
- `git.commit`: the lowercase hex id (40 or 64 characters) of the git commit whose tree the timeline's tree equals, when it equals one;
- `git.head`: on a session timeline, the commit the worktree's HEAD points at (informational; a git commit changes no file).

**TL-82 Paths.** A tree key is a path in git's form: UTF-8, relative, segments separated by `/`, no leading or trailing `/`, no empty segment, no segment `.` or `..`, no segment equal to `.git` compared case-insensitively, no NUL, at most 4,096 bytes. Paths are case-sensitive and compared as bytes, as git does. A `map.set` on a tree with a key that is not a path is `invalid_op`. Directories are implicit (git tracks no empty directories).

**TL-83 File entries.** A tree value is a **file entry**, a map `{"obj": ref, "mode": m, "eol": e}`:
- `mode` is git's mode as text: `"100644"` (file), `"100755"` (executable file), `"120000"` (symbolic link), `"160000"` (submodule);
- `obj` names the **content object**: for `100644`/`100755`, a text that is not a transcript (a **text file**), or a register whose value is a HAN BlobRef map `{hash, size, type}` (a **binary file**); for `120000`, a register holding the link target as text; for `160000`, a register holding the submodule commit id as lowercase hex text;
- `eol` is `"lf"` or `"crlf"`, present only for text files.

In the state at the op's parent (for a merge, the state after the whole merge op is applied), `obj` MUST exist and be of the kind its mode requires; otherwise `invalid_op`.

**TL-84 Text or binary.** A host MUST store a file as a text file only when its bytes are valid UTF-8, contain no NUL, are at most 1 MiB, and use one line-ending convention: no CR at all (`eol: "lf"`), or every LF preceded by CR and no other CR (`eol: "crlf"`, stored with the CRs removed). Every other file is binary (a host SHOULD warn when a file is binary only because it mixes line endings): its bytes go to HAN's blob transport (architecture.md T-03) and the register holds their BlobRef (`type` `application/octet-stream` unless known). Materialising a text file writes its text as UTF-8, with CRLF for each LF when `eol` is `crlf`, and MUST reproduce the original bytes exactly. A byte-order mark is kept as the character U+FEFF.

**TL-85 Events become ops.** The host records the worktree's changes as batches on the session's timeline:
- **create** `p`: create the content object, then `map.set tree p → entry`, in one batch;
- **edit** of a text file: one or more `text.edit`s of its object whose result equals the new content (a host SHOULD record a minimal line-then-character difference, so that identities of unchanged text survive); of a binary file: `register.set` with the new BlobRef; a text file that becomes binary, or a binary file that becomes text, is replaced by a new content object under the same path;
- **delete** `p`: `map.delete tree p` (the object stays, unreferenced, for time travel);
- **rename** `p → q`: `map.delete tree p` and `map.set tree q →` the same entry, in one batch, so the content object, its history and its identities move with the path. A host SHOULD treat a delete and a create with identical content seen within one change batch as a rename;
- **mode or line-ending change**: `map.set tree p →` the same object with the new `mode` or `eol`.

Batches SHOULD group the changes of one tool call or one short quiet period, and SHOULD carry the tool call's id as the batch's `step` (TL-91) so review can attribute changes to steps. Paths ignored by git (untracked and ignored), paths on the space's exclusion list (PLAN.md §6.3), and anything under `.git` MUST NOT be mirrored. A file the host cannot read MUST be reported, not recorded as deleted.

**TL-86 Tree conflicts in a state.** A state reports, for a tree map, computing everything from each path's visible value: a path in conflict (TL-36: several concurrent entries); a content object referenced by two or more paths (a **duplicate**, as left by concurrent renames of one file); and a **path clash**, a path `p` present together with a path that starts with `p + "/"`. A host writing such a tree to disk MUST report them, MUST NOT write both sides of a path clash, and SHOULD write the visible entries otherwise.

**TL-87 Tree conflicts in a merge.** Planning a tree map follows TL-71, with three more conflict kinds that replace the per-key outcome:
- **modify/delete**: one side removed path `p` (absent, present in the base) while the other side kept `p` and its content object's content differs from the base's (text or value): a conflict instead of the delete or nothing. `target` keeps the target's side; `incoming` takes the incoming side (removes `p`, or restores `p` with the incoming entry and the incoming content changes);
- **rename**: after the clean changes, a content object would be referenced by more paths than in `St` and in `Si`: a conflict listing the target's and incoming paths. `target` keeps the target's paths only; `incoming` the incoming paths only;
- **path**: after the clean changes, a path clash (TL-86) would exist that neither side had: `target` leaves out the incoming paths that clash; `incoming` removes the target's paths that clash.

Content merges of a file follow its object: a file renamed on one side and edited on the other merges cleanly. A content object is merged by TL-72 whatever a tree conflict's resolution; the resolution decides only which paths refer to it.

**TL-88 Session timelines and the base commit.**
- A worktree history's main timeline mirrors the repository's default branch: when the host sees that branch at a commit `C` that main does not record, it records on main a **mirror batch** that makes main's tree equal `C`'s tree (paths, modes, bytes) and sets `git.commit` to `C` (TL-74).
- A session's worktree is created at base commit `B` (PLAN.md §6.4). The session's timeline MUST be forked from a version of main whose `git.commit` is `B` and whose tree equals `B`'s tree (the host records `B` on main first if needed). Review shows the difference between the fork version and the session head.
- After an accepted merge into main, the executor commits main's tree to git; when the commit's tree equals main's tree, the host records `git.commit` on main. A merge into main never moves the default branch by itself.

**TL-89 Which history holds what.** A worktree history holds one repository's main timeline and its session timelines; all readers of the space (architecture.md P-02) may sync it. A session's transcript lives in its own transcript history (TL-27), one per session, and is synced only to actors whom `sessionAccess` grants `read` on that session (architecture.md P-15, T-07). Only the session's actor writes it, seals included (TL-103); a human's steering reaches it as a signed `session.input` (architecture.md P-15) that the host writes in. Seals (§9) apply only there.

## 13. Envelopes and encoding

**TL-90 Deterministic CBOR.** Every structure in this section, every value (TL-30) and the observable state (TL-61) are encoded as CBOR (RFC 8949) with **core deterministic encoding** (§4.2.1): shortest integer and length forms, definite lengths, map keys sorted by their encoded bytes, no duplicate keys, no tags, no floats. Protocol structures are maps with small integer keys; values keep text keys. A receiver MUST reject (`malformed`) any input that is not exactly the deterministic encoding of what it decodes to (re-encoding gives the same bytes).

**TL-91 Batch encoding.** A batch is the CBOR map:

| Key | Field | |
|---|---|---|
| 1 | `lane` | uint |
| 2 | `start` | uint, the first op's counter |
| 3 | `parents` | version |
| 4 | `timeline` | op ref, or null (TL-21) |
| 5 | `replicas` | array of `[actor, key, lane]`: the replica table, indexes 1 … n (index 0 is the batch's own replica: the envelope's `actor` and `key`, and `lane`) |
| 6 | `ops` | array of ops, at least one |
| 7 | `time` | optional: text of 1–64 bytes, by convention the writer's RFC 3339 UTC time; informational, never used for ordering, and never rejected for its format |
| 8 | `step` | optional: text ≤ 256 bytes, the tool call or step that caused the batch (informational) |

References: an **op ref** is `[r, c]` (replica table index, counter); a **version** is an array of op refs in ascending op id order; an **object ref** is an op ref, or null for `root`; an **insertion ref** is `[r, c, i]`; a **character ref** is `[r, c, i, k]`; a **delete range** is `[r, c, i, from, to]`; an **anchor** is a character ref or null for `start`. The replica table MUST list no replica twice, MUST NOT list the batch's own replica, MUST list only replicas the batch references, and MUST be in ascending replica order (TL-02).

**TL-92 Op encoding.** An op is a CBOR map with key 0 = kind number:

| Kind | # | Fields |
|---|---|---|
| `timeline.create` | 1 | 1 `name` text, 2 `source` op ref or null, 3 `historyKind` text (main only), 4 `session` text, 64 lowercase hex (main of a transcript history only) |
| `checkpoint` | 2 | 1 `label` text |
| `merge` | 3 | 1 `source` op ref, 2 `incoming` version, 3 `base` version, 4 `changes` array, 5 `conflicts` array, 6 `effect` text (optional, 64 lowercase hex) |
| `register.create` | 10 | 1 `value` |
| `register.set` | 11 | 1 `target` object ref, 2 `value` |
| `map.create` | 20 | 1 `tree` bool |
| `map.set` | 21 | 1 `target`, 2 `key` text, 3 `value` |
| `map.delete` | 22 | 1 `target`, 2 `key` text |
| `text.create` | 30 | 1 `text` text (may be empty), 2 `transcript` bool |
| `text.edit` | 31 | 1 `target`, 2 `deletes` array of delete ranges, 3 `inserts` array of `[anchor, text]` |
| `text.seal` | 32 | 1 `target`, 2 `ranges` array of delete ranges (used as identity ranges) |

Merge changes: `{0: 1, 1: object, 2: create, 3: value}` (register); `{0: 2, 1: object, 2: create, 3: tree, 4: sets, 5: deletes}` (map); `{0: 3, 1: object, 2: create, 3: transcript, 4: deletes, 5: inserts}` (text), each insert `[anchor, text, origin]` with origin a character ref or null. A merge conflict record: `{0: kind, 1: object, 2: key or null, 3: [a, b] base lines or null, 4: resolution}` with kinds 1 register, 2 map, 3 text, 4 modify/delete, 5 rename, 6 path, and resolution 1 `target`, 2 `incoming`, 3 value/text. `object` is the register, map, text or tree map. `key` is the map key for map conflicts, the path for modify/delete, the shorter of the two paths for a path conflict, and null for register, text and rename conflicts; `lines` is set only for text conflicts. Receivers record these for review and never interpret them. Fields not listed are `malformed`.

**TL-93 Envelope.** Every batch travels in a HAN-signed envelope, the CBOR map below; it is the batch format of the `han/1` stream type `han.timeline/1` (architecture.md G-07, typed streams), which gives it framing only, so this envelope is the one signature a batch carries:

| Key | Field | |
|---|---|---|
| 1 | `v` | 1 |
| 2 | `space` | the SpaceId of the history's space |
| 3 | `history` | byte string: the 32-byte history id, or empty for the genesis envelope (TL-10) |
| 4 | `actor` | the author's ActorId |
| 5 | `key` | the KeyId of the signing device key |
| 6 | `payload` | byte string: one zstd frame (RFC 8878) holding the deterministic CBOR of the batch |
| 7 | `sig` | 64-byte Ed25519 signature |

The **batch id** and signature are:

```
batchId = SHA-256( "han-timeline-batch-v1\n" ‖ dCBOR([1, space, history, actor, key, payload]) )
sig     = Ed25519( key(actor, key), "han-timeline-sig-v1\n" ‖ hex(batchId) )
```

where `‖` concatenates bytes, the strings are UTF-8, `hex` is 64 lowercase hex characters, and dCBOR is TL-90. The signature covers the exact payload bytes as sent, compressed. A genesis envelope's batch id, computed with the empty `history`, is the history id. An envelope with an empty `history` whose batch is not a genesis batch, or a genesis batch in an envelope with a non-empty `history`, is `genesis`. The zstd frame MUST declare its content size, MUST NOT use a dictionary, and MUST decompress to exactly that size; frames with raw (uncompressed) blocks are valid zstd.

**TL-94 Who signs and how the author binds to op ids.** The signer is the device key `key` of `actor`, verified with that key's public key from the actor's HAN profile (architecture.md §4.3, N-01). Every op in the batch gets the replica `(actor, key, lane)` and counters `start, …`: there is no other place an op's author comes from, so an op id names its signer, and only the holder of a device key can write ops under that key's replicas. A relaying replica forwards the original envelope bytes unchanged; it never re-signs another replica's batch.

## 14. Integrating untrusted data

Every envelope from a peer, from storage written by another version, or from a file is untrusted.

**TL-100 Atomic integration.** A receiver integrates a batch entirely or not at all. Checks run in the order below; the first failing check gives the reason (§18) and nothing of the batch is kept, except that a batch held back (TL-102) is retried later.

**TL-101 Checks.**
1. **Shape**: the envelope is deterministic CBOR with exactly the fields of TL-93, of the right types, within the limits of §17 (`malformed`, `too_large`).
2. **History**: `space` and `history` name a history the receiver keeps, or `history` is empty and the receiver is taking up a new history of that space (`wrong_history`).
3. **Signature**: `key` is a key of `actor`'s profile and `sig` verifies with it (Ed25519 with the strict verification HAN uses for entries, PLAN.md §7) (`unknown_key`, `bad_signature`).
4. **Authorisation**: TL-103. An unauthorised batch is not rejected here: if it also passes steps 5 and 6 (so its counters and ops are known), it is quarantined instead of integrated. While the receiver does not yet hold the genesis batch, it does not know the history's kind and so cannot judge authorisation: it holds the batch back (TL-102) and judges step 4 when the batch's dependencies have arrived.
5. **Payload**: the zstd frame and the batch decode per TL-91–92, deterministically, within limits (`malformed`, `too_large`).
6. **Duplicates and overlap**: TL-104 (`duplicate`, `equivocation`, `overlap`).
7. **Counters**: `start` > every counter in `parents`, every counter in a merge's `incoming` and `base` is < `start`, and `start + n − 1` ≤ 2^53 − 1; otherwise `lamport` at once. This needs no other batch, so a batch that could never be integrated is never held back.
8. **Dependencies**: every op of `parents`, every merge dependency (TL-76), the batch's timeline tag and, for a fork, its source timeline's create op are held; otherwise the batch is held back (TL-102). So a fork that arrives before the fork it was made from waits instead of failing.
9. **Structure**: TL-10, TL-15 (`genesis`); TL-20–23 and TL-27 (`timeline`).
10. **Ops**: each op is valid at its parent (TL-17, TL-27, TL-30–32, TL-38, TL-40–42, TL-44, TL-48, TL-50–51, TL-72–76, TL-82–83) (`invalid_op`). For merge ops this includes the checks of TL-76, and file entries that a merge sets are judged against the state after the whole merge op is applied (TL-83), since a merge may set a path to an object it imports.

**TL-102 Any order.** A receiver MUST hold back a batch that passes checks 1–7 but lacks dependencies, and integrate it as soon as they are held, so a set of batches delivered in any order ends in the same state as the same set delivered in causal order. It SHOULD hold back at least 1,024 batches or 64 MiB per history; when it must drop one, it drops the held-back batch with the greatest `start` and reports `missing` for it; the batch comes back in a later sync round. Delivering a batch that is already held back answers `held` again and changes nothing.

**TL-103 Authorisation, quarantine and vouching.** A batch is **authorised** when its `key` is `valid` in `actor`'s profile and, as of the receiver's latest view of the space and the profile:
- in a worktree or data history: `actor`, human or agent, holds the role `member` or `owner` in the space;
- in a transcript history: `actor` is the session's actor (the author of the `session.open` the history names, TL-27). No one else writes a transcript, seals included.

A receiver MUST NOT integrate an unauthorised batch on its own: it **quarantines** it (answering `quarantined`), keeping it aside, out of every state and out of its inventory. A quarantined batch becomes **vouched**, and is then integrated by §14 like any other except that it skips step 4 (so it is never quarantined again), as soon as an authorised batch that the receiver holds (integrated or held back) depends on it through any dependency of TL-101 step 8 (a parent, a merge dependency, the timeline tag or a fork's source), directly or transitively through other quarantined batches. So the order in which the authorised batch and the batches it builds on arrive does not matter. A receiver SHOULD quarantine at least 1,024 batches per history; when it must drop one, it drops the one with the greatest `start` and reports `unauthorized` for it. Ops already integrated are never removed when a key is later revoked or a role lost.

**TL-104 Duplicates and equivocation.** If the receiver holds a batch with the same replica and the same counter range: if the decompressed batch bytes are equal, the new copy is a **duplicate**, ignored and reported as such (not an error), which makes integration idempotent; if they differ, it is an **equivocation**: rejected, and the receiver SHOULD keep both envelopes as a fault proof (two validly signed envelopes for one op id) and SHOULD flag the actor. A batch whose counter range overlaps a held batch of the same replica without being the same range is rejected (`overlap`). It is also an equivocation when a shared op **differs**: two ops with the same op id differ when their kinds, their fields or their parents differ after every reference in them is resolved to a full replica id (so a different replica table alone is no difference). Here "held" means integrated: a batch that is only held back (TL-102) does not count, and a second delivery of it answers `held`.

## 15. Sync

**TL-110 Two messages.** Sync is anti-entropy in two messages, over any transport that carries bytes (on `han/1`, a typed stream of type `han.timeline/1`, architecture.md G-07, each message one frame; or any other):
1. **hello** `{1: 1, 2: space, 3: history, 4: inventory, 5: maxBytes?}`: the inventory is an array of `[[actor, key, lane], [[from, to]…]]` in ascending replica order, each with ascending, non-overlapping, non-adjacent inclusive counter ranges (overlapping, unordered or adjacent ranges make the hello `malformed`) that cover every integrated batch of that replica. For the sender's **own** replicas (the replicas of its own device keys), a range MAY also cover counters that no batch of that replica uses, since the sender knows it wrote no other. For every other replica, the ranges MUST be exactly the union of the counter ranges of its integrated batches, merging only ranges whose counters are consecutive: a sender cannot know that another replica's gap is unused, and claiming it would hide a batch from the sender. `maxBytes` is the largest answer the sender wants (default 4 MiB, at least 64 KiB).
2. **answer** `{1: 1, 2: space, 3: history, 4: envelopes, 5: more}`: the encoded envelopes of every integrated batch whose counter range the hello does not cover entirely, in ascending `start`, then replica, order (a causal order, TL-76), cut at an envelope boundary so the answer fits `maxBytes` (but at least one envelope); `more` is true when it was cut.

Both peers send a hello and answer the other's. A peer that got `more: true` sends a new hello. Sync ends when neither answer is cut.

**TL-111 Idempotent and order-tolerant.** Integrating an answer is integrating each envelope by §14, so receiving any envelope twice, receiving answers in any order, or receiving envelopes pushed unasked (a writer MAY push its new envelopes to peers) leaves the same state. A held-back batch is not in the inventory.

**TL-112 Validating sync messages.** A hello or answer that is not deterministic CBOR of the shape above, names another history, or exceeds the limits of §17 is dropped whole (`malformed`, `too_large`); the transport connection MAY be closed. Ranges that overlap, are out of order, or include counter 0 make the hello `malformed`.

**TL-113 Who may sync.** A node MUST answer a hello only for a peer whose actor may read the space (architecture.md P-02), or, for a transcript history, has `read` on the session (TL-89), as of the node's latest view. The answer reveals nothing else.

## 16. Storage invariants

**TL-120 Counters survive crashes.** A device keeps one durable high-water mark per device key, shared by all the key's lanes. It MUST make the high-water mark durable before an envelope using a counter at or below it leaves the device, and MUST allocate counters for all lanes from it atomically, so a restart or two lanes never reuse a counter (which would be an equivocation, TL-104).

**TL-121 Append-only.** Integrated ops are never changed. While a replica keeps a batch, it keeps the original envelope bytes, so it can relay them (TL-94).

**TL-122 Atomicity.** A crash leaves every batch either integrated completely (ops, state caches, inventory) or not at all.

**TL-123 Caches are derivable.** Every stored state, snapshot or index MUST equal what replaying the ops gives (TL-60). A snapshot is an optimisation, never a source of truth.

**TL-124 No compaction in /1.** A replica MUST keep every integrated batch, and MUST be able to materialise every version of it (TL-60). `/1` has no snapshot transfer, so dropping history would leave peers unable to obtain it; retention windows (PLAN.md §6.3) and compaction are left to `/2` (§20).

**TL-125 Format version and reopening.** When it opens stored data, an implementation MUST re-check it as if it were received (TL-101), except signatures and authorisation, which were checked on receipt and whose later changes never remove integrated ops (TL-103). Stored data carries the format version of this profile; an implementation refuses data with a newer version.

## 17. Limits

| Item | Limit |
|---|---|
| envelope, encoded | ≤ 4 MiB + 4 KiB |
| batch, decompressed | ≤ 4 MiB (4,194,304 bytes) |
| ops per batch | 1 … 65,536 |
| replica table | ≤ 256 entries |
| heads in a version (`parents`, `incoming`, `base`) | ≤ 64 |
| counter | 1 … 2^53 − 1; lane 0 … 2^32 − 1 |
| value | ≤ 64 KiB encoded, depth ≤ 16 |
| map key | 1 … 1,024 bytes; tree path ≤ 4,096 bytes |
| timeline name, checkpoint label | 1 … 256 bytes |
| text file in a tree | ≤ 1 MiB (larger is binary, TL-84) |
| hello inventory | ≤ 4,096 replicas, ≤ 65,536 ranges |
| answer | ≤ the hello's `maxBytes` (default 4 MiB), except a single envelope |

## 18. Reason codes and local errors

Rejections of peer data (§14), returned per envelope:

| Code | Meaning |
|---|---|
| `duplicate` | already held, identical: ignored (not an error) |
| `held` | dependencies missing: held back (TL-102), not an error |
| `quarantined` | not authorised: kept aside until vouched (TL-103), not an error |
| `missing` | a held-back batch was dropped (TL-102) |
| `malformed` | not deterministic CBOR, wrong shape, bad zstd, unknown kind or field |
| `too_large` | over a limit of §17 |
| `wrong_history` | not a history the receiver keeps |
| `unknown_key` / `bad_signature` | TL-101 step 3 |
| `unauthorized` | a quarantined batch was dropped (TL-103) |
| `equivocation` / `overlap` | TL-104 |
| `lamport` | TL-04, TL-76 counters |
| `genesis` | TL-10, TL-15 |
| `timeline` | TL-20–23, TL-27 |
| `invalid_op` | an op is invalid at its parent |

Local errors (never on the wire): `out_of_range`, `sealed`, `unknown_object`, `unknown_timeline`, `ambiguous_name`, `unknown_anchor`, `merge_self`, `unresolved`, `up_to_date`.

## 19. Design choices

Where a design choice was open, this profile decides as follows.

1. **Replica = actor + device key + lane, counters per key** (TL-01, TL-120). A HAN actor has several devices and an agent host may run several sessions under one key; tying op ids to the device key binds every op to a signature, the lane keeps parallel writers on one key apart, and one high-water mark per key keeps their counters distinct.
2. **Scalar values, not bytes or UTF-16, as the text unit** (TL-40), so every language counts the same way and no position can split a character.
3. **Concurrent inserts at one point go newest first** (TL-43), an RGA-style order: simple, proven to converge, and it keeps runs of typing together. It can interleave backwards-typed concurrent text (the anomaly the Fugue paper describes); HAN accepts that for MVP.
4. **Registers: the greatest write wins (last-writer-wins by op id); maps keep concurrent values; removes are observed-remove** (TL-33–37): a register is for things where one value must win (a commit id), a map key for things a person may need to see both of.
5. **Seals hide every insert inside the region, including inserts by writers who had already seen the seal** (TL-52–53). It makes the rule a function of the op set (no per-writer bookkeeping), and an honest engine never writes such an insert anyway (TL-53). Receivers accept such ops without effect rather than reject them, because a concurrent insert and a later one are otherwise indistinguishable to a receiver without the writer's view.
6. **Seals only on transcripts; transcripts in their own history, with only main and one writer** (TL-27, TL-50, TL-89, TL-103): a seal intentionally discards concurrent edits, which is wrong for code; transcripts need the stricter read right; and an audit record has one author, the session's actor, with human steering arriving as signed `session.input`.
7. **Merge base over merge edges** (TL-70): the base advances past what earlier merges carried in either direction, so updating a session from main and then merging it back does not duplicate text, and a conflict resolved once is not raised again. Where both directions have merged (criss-cross), the base is the join of both, materialised as an ordinary state; a conflict that the two sides resolved differently in a mutual merge is a real disagreement and comes back in the next merge (vector 48), as it does in git. Non-normative: a planner SHOULD label a base whose heads lie on more than one timeline a **combined base** when it shows the plan.
8. **Text merges by identity, conflicts by lines, never refused for order** (TL-72): when a copy and its original sit on different sides of a third character (TL-43 places a copy at its anchor, not at its origin's place), the planner keeps the longest order-consistent subsequence and treats the rest as a move (delete and insert). This was chosen over placing copies at their origin's place among siblings: a copy's anchor is often not its origin's anchor, so that rule would not make the orders agree in every history, and it would complicate TL-43 for every replica, while this fix stays inside the planner. Characters the target already shows elsewhere are inserted without origin, so duplicate hiding (TL-45) cannot swallow them. Identity decides exactly what each side changed (no diff heuristics; text that already crossed over is recognised through copies), and line-granular clusters give reviewable hunks like git's. Changes on neighbouring lines do not conflict; identical changes on both sides converge. A planner MAY additionally point out clean clusters that lie close together; that is advice to the reviewer, not part of the plan.
9. **Conflicts must be resolved before a merge is recorded; no conflict markers in content** (TL-75). Markers in a CRDT text would be content every replica must then clean up; an explicit resolution is reviewable and recorded with the merge.
10. **No fast-forward** (TL-77): every merge into main is a single op that names its incoming version and points at its approval, so "accepting work into main" is always one auditable, gateable event.
11. **Timeline heads are derived from batch tags** (TL-24), never exchanged, so sync needs no head messages and cannot be lied to about heads.
12. **Out-of-order batches are held back, not refused** (TL-102), bounded, so delivery order never matters within the buffer, and a dropped batch simply returns in the next round.
13. **Quarantine and vouching** (TL-103): a batch from a revoked key or a lost role is kept aside rather than refused, and integrated once authorised work depends on it, whatever the arrival order; history already built on it stays syncable, and unvouched writes never enter a state.
14. **Worktree files: one object per file, paths as tree keys, renames move keys** (TL-83–85): edits and renames commute; line endings are an attribute so texts stay LF-only; files that are not clean UTF-8 text, or mix line endings, are binary blobs (MVP: no merges inside them).
15. **One envelope per batch, signed over the compressed bytes** (TL-93), as PLAN.md §6.1 asks; relays forward bytes, so a batch never needs its author online to travel.
16. **The history id is the genesis batch id** (TL-10): it binds the history to its creator and its first content, and needs no randomness.
17. **Duplicate copies are hidden** (TL-45): concurrent merges of the same work into one timeline show it once, by a rule that is a function of the op set.
18. **Text offsets in scalar values; approvals bind digests** (TL-40, TL-74): an effect's plan binds CBOR digests of the merge, never positions, so it does not depend on how a language counts text.
19. **No compaction in /1** (TL-124): without snapshot transfer, dropping history would strand peers.

## 20. Not in /1, and decided items

`han.timeline/1` deliberately leaves out:
- timeline rename, archive and delete;
- merges inside binary files (a binary file conflicts as a whole register);
- identity across the 1 MiB text/binary boundary (a file that crosses it gets a new content object, TL-84, TL-85);
- an interleaving-free text order (Fugue) as an option;
- probing sync for a common version instead of full inventories;
- snapshot transfer, retention windows and compaction (TL-124);
- forks and merges in transcript histories (TL-27);
- identity-preserving moves beyond rename (text moved between files keeps no identity);
- undo as an op (undo is an ordinary edit);
- submodule contents (a submodule is its commit id, TL-83);
- a raw-CR text mode, so that files with mixed line endings can be text (they are binary in /1, TL-84).

Decided: a fork has one source and never combines timelines (TL-22); changes on neighbouring lines stay clean (TL-72, §19 item 8).

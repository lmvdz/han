# `han.timeline/1` conformance vectors

Test vectors for [`spec/profiles/han.timeline-1.md`](../../spec/profiles/han.timeline-1.md). Each `NN-*.json` file is one behaviour vector (format `han-timeline-vector/1`): input batches from several replicas, the order a receiver gets them in, and the expected materialised states, heads, conflicts, merge plans and sync answers. `envelopes.json` (format `han-timeline-envelope-vector/1`) pins the exact bytes of envelopes, batch ids, signatures and sync messages. Where a vector and the profile disagree, the profile wins and the vector is a bug.

## Behaviour vectors

```
{ format, name, description, rules, replicas, writers?, hold_limit?, batches, runs }
```

- `replicas`: labels for replica ids `[actor, key, lane]` (TL-01). Every vector uses `A` = `@alice:a.example/laptop/0`, `B` = `@bob:b.example/desk/0`, `C` = `@alice:a.example/phone/0`, so the replica order (TL-02) is **A < C < B**.
- `writers`: when present, the replica labels whose batches are authorised (TL-103); batches of any other replica are unauthorised. When absent, every batch is authorised.
- `hold_limit`: when present, the receiver holds back at most this many batches (TL-102); otherwise it never drops one.
- `batches`: named batches in decoded form, `{replica, start, parents, timeline, ops}` (TL-91). Behaviour vectors assume every batch arrives in a valid envelope with a valid signature from a key in its actor's profile (TL-101 steps 1–3 and 5 pass); `envelopes.json` tests those steps.
- `runs`: each run starts a **fresh receiver** that holds nothing, then executes its `steps` in order. Several runs over the same batches in different orders must reach the same expectations.

### Notation

| Form | Meaning |
|---|---|
| `"3@A"` | op id: counter 3, replica `A` (TL-03). Also an object id or a timeline id |
| `"root"` | the root map (TL-31) |
| `"2@A#0"` | insertion 0 of op `2@A` (TL-41) |
| `"2@A#0:4"` | character 4 of that insertion |
| `null` anchor | `start` (TL-42) |
| `{"obj": "3@B"}` | an object reference inside a value (TL-30); on the wire `{"obj": [actor, key, lane, counter]}` |
| version | an array of op ids in ascending op id order; `[]` is the root version |

Ops are written as `{"kind": …, fields…}` with the field names of TL-16 (main's create op carries `historyKind`, and `session` in a transcript history). A merge op's `changes` are `{"type": "register" | "map" | "text", "object", "create", …}` and its `conflicts` records `{"kind", "object", "key" | "lines", "resolution"}` (TL-73, TL-92).

### Steps

- `{"deliver": "b1", "result": r}`: hand batch `b1` to the receiver. `result`, when present, is what integrating it must report: `integrated`, `held` (dependencies missing, TL-102), `quarantined` (unauthorised, TL-103), `missing` (held back and immediately dropped, TL-102), `duplicate` (TL-104), or `rejected:<code>` with a code of §18. A held or quarantined batch that becomes integrable is integrated during a later delivery; a delivery whose batch is integrated within that same delivery (because a quarantined batch it depends on was vouched, or the other way round) answers `integrated`. Without `result`, the delivery must not be rejected.
- `{"expect": {…}}`: checks on the receiver's current data. Every key present must hold:

| Key | Check |
|---|---|
| `pending` | the names of the batches currently held back, as a set |
| `quarantined` | the names of the batches currently quarantined, as a set (TL-103) |
| `dropped` | the names of the held-back batches the receiver has dropped and reported `missing`, as a set (TL-102) |
| `heads` | `{timeline id: version}`: the derived head of each listed timeline (TL-24) |
| `states` | a list of `{at, objects?, absent?, anchors?}`. `at` is a timeline id (its head) or a version. `objects` maps object ids to their observable state (TL-61): `{"value": v}` for a register, `{"entries": {key: [[op id, value], …]}}` for a map (every present key, values greatest first; `{}` when empty), `{"text": s}` for a text's visible text. Only listed objects are checked. `absent` lists objects that must not exist at that version. `digest`, when present, is the state digest of TL-61 (its state lists every object) |
| `anchors` (in a state) | `{object, make?, anchor, pos}`: making an anchor at `make.pos` with `make.bias` gives `anchor` (`"start"`, `"end"` or `[character, bias]`), and resolving `anchor` gives `pos` (TL-46) |
| `plan_edits` | local edit planning (TL-47, TL-53): `{at, object, pos, delete, text}` gives either `op` (the exact `text.edit`) or `error` (`out_of_range`, `sealed`) |
| `plans` | merge planning (TL-70–75, TL-87): `{into, from}` gives `kind` (`merge`, `up_to_date`, or `error:<code>` for a local error such as `merge_order`); for a merge, the `base` version and the `conflicts` in plan order, and for each entry of `results`, applying the clean changes plus the listed `resolutions` (one per conflict, in order: `"target"`, `"incoming"`, `{"text": s}`) gives the listed visible `texts` |
| `checkpoints` | `{timeline, list}`: the timeline's checkpoints in op id order with their labels and checkpoint versions (TL-25) |
| `tree_conflicts` | `{at, tree, keys, duplicates, clashes}`: the tree map's keys in conflict, objects referenced by several paths, and path clashes (TL-86) |
| `inventory` | `{replica label: [[from, to], …]}`: the hello inventory with adjacent ranges merged (TL-110) |
| `answer` | `{hello, envelopes}`: given a peer hello inventory (labels as above), the names of the batches the answer must hold, in order (TL-110) |

Merge results are checked as texts, not as exact merge ops: a planner may anchor its copies differently as long as TL-72 step 7 holds. The `merge` batches in the vectors are one valid recording, and every receiver must apply them to the states shown.

Conflicts in `plans`:
- text: `{"kind": "text", "object", "lines": [a, b], "base", "target", "incoming"}` with 0-based base line ranges;
- register: `{"kind": "register", "object", "base", "target", "incoming"}` with the three values;
- map key: `{"kind": "map", "object", "key", "base", "target", "incoming"}`, each `{"value": v}` or `null` for absent;
- modify/delete: `{"kind": "modify_delete", "object", "key", "deleted_by"}`;
- rename: `{"kind": "rename", "object", "content", "target": [paths], "incoming": [paths]}`;
- path: `{"kind": "path", "object", "paths": [p, q]}`.

## Envelope vectors (`envelopes.json`)

Keys: each private key is the 32-byte Ed25519 seed `sha256(UTF-8 seed)`; `publicKey` is base64url; `inProfile` says whether the key is a valid key of its actor's profile (the tablet key `T` is not). Space `s1:a.example`. `history` is the history id, the batch id of the genesis envelope E1, which itself carries an empty `history` field (TL-10). Each vector gives hex byte strings:

| Field | Meaning |
|---|---|
| `batch_cbor` | the deterministic CBOR of the batch (TL-90, TL-91) |
| `payload` | the zstd frame carrying it (raw blocks, content size declared, TL-93) |
| `batch_id_preimage`, `batch_id` | `"han-timeline-batch-v1\n" ‖ dCBOR([1, space, history, actor, key, payload])` and its SHA-256 |
| `sig_input`, `sig` | `"han-timeline-sig-v1\n" ‖ hex(batchId)` and the Ed25519 signature |
| `envelope` | the encoded envelope (TL-93) |
| `verdict` | what a receiver holding both profiles must answer (`integrated` or `rejected:<code>`) |

`E7-hello` and `E8-answer` are the encoded sync messages of TL-110 for a replica holding E1 and E2. `after` lists the vectors a receiver must have integrated first.

## Index

| Vector | What it pins |
|---|---|
| 01-genesis-runs-and-ids | genesis, runs, op ids, derived heads, time travel to the root |
| 02-lamport-and-genesis | counters above parents; one genesis |
| 03-register-last-writer-wins | the greatest write wins: last-writer-wins by op id |
| 04-map-keeps-concurrent-values | concurrent map values kept until a write that saw them |
| 05-map-observed-remove | removes delete only what was seen (add wins) |
| 06-text-concurrent-inserts-newest-first | concurrent inserts at one point, newest first, any order |
| 07-text-siblings-skip-newer-subtrees | newer siblings keep their descendants together |
| 08-text-typing-runs-do-not-interleave | two people typing at one spot give two runs |
| 09-text-deletes-are-tombstones | overlapping deletes; inserts inside deleted ranges survive |
| 10-anchors-left-and-right-bias | anchor bias and collapse on delete |
| 11-local-edit-planning | positions to identity ops; CR normalisation; out_of_range |
| 12-seal-undoes-concurrent-deletes | sealed text ignores concurrent deletes |
| 13-seal-hides-concurrent-inserts-inside | inserts inside a seal hidden; edges visible |
| 14-seal-later-edits | local edits into a seal refused; peers' edits accepted without effect |
| 15-seal-shows-exactly-what-its-author-saw | a seal across an earlier deletion |
| 16-seal-validation | transcripts only; visible, contiguous ranges |
| 17-fork-isolates-timelines | forks, isolation, heads from tags |
| 18-no-history-joins | parents only on the same timeline; fork reach; tags; create placement |
| 19-time-travel-and-checkpoints | past, concurrent and two-headed versions; checkpoints |
| 20-merge-clean | clean three-way content merge; up to date afterwards |
| 21-merge-conflict-hunk | a conflict hunk and each resolution |
| 22-merge-registers-and-maps | register and map key merges, one conflict |
| 23-merge-base-advances-no-reraise | repeated merges bring only new work |
| 24-merge-both-directions-no-duplicates | update from main, merge back, no duplicates |
| 25-merge-imports-new-objects | objects new on the incoming side are imported by id |
| 26-tree-rename-carries-concurrent-edit | renames move keys; edits follow the object |
| 27-tree-validation | paths and file entries |
| 28-tree-conflicts-in-state | duplicate paths after concurrent renames; file/directory clash |
| 29-sync-any-order | held-back batches, any delivery order |
| 30-sync-duplicates-are-idempotent | duplicate delivery, held or integrated |
| 31-sync-equivocation-and-overlap | conflicting batches for one op id |
| 32-sync-hello-and-answer | inventories and answers; a hello from A's own device may span A's unused counters |
| 33-sync-merge-waits-for-incoming | merge dependencies held back |
| 34-validation-of-ops | ops judged at their parent |
| 35-merge-modify-delete | modify/delete tree conflict |
| 36-merge-identical-changes-converge | identical changes on both sides converge |
| 37-merge-insert-at-same-line-gap-conflicts | inserts at one line gap conflict; a mid-line insert does not |
| 38-sync-inventory-keeps-others-gaps | an inventory never spans another replica's gap |
| 39-quarantine-and-vouching | unauthorised batches quarantined, integrated once vouched, any order, transitively |
| 40-creating-batch-ops-are-on-the-timeline | every op of a creating batch is on the timeline |
| 41-fork-waits-for-its-source | a fork arriving before its source is held back |
| 42-concurrent-merges-show-copies-once | duplicate copies hidden; deleting removes the hidden duplicate too |
| 43-transcript-history-rules | transcripts: only main, one writer, seals by the host |
| 44-history-kinds | history kinds on main only; trees and transcripts only in their kinds |
| 45-merge-order-refused | merge_order |
| 46-merge-adjacent-lines-stay-clean | edits on neighbouring lines merge cleanly |
| 47-criss-cross-merge-clean | mutual concurrent merges, then a third: base spans both timelines |
| 48-criss-cross-conflict-resolved-differently | each side kept its own line: the hunk returns once |
| 49-malicious-merge-ops | non-canonical origin, wrong copy text, import from outside incoming, counters at or above start |
| 50-limits-too-large | name, label and key limits |
| 51-held-back-drop-reports-missing | bounded hold-back buffer, drop of the greatest start |
| 52-merge-register-conflict | register conflict and a value resolution |
| 53-merge-rename-conflict | rename/rename in a merge |
| 54-merge-path-conflict | a file and a directory of one name in a merge |
| envelopes.json E1–E11 | envelope bytes, the history id from the genesis batch id, batch ids, signatures, replica tables, bad signatures, non-deterministic CBOR, zstd without size, hello and answer bytes, an empty history on a non-genesis batch, an unknown key, and a merge digest (TL-74) with the this-op marker (`digest_preimage`, `merge_digest`) |

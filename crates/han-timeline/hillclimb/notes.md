# han-timeline speed notebook

Read this instead of re-exploring the crate. The loop appends the current timings and every
attempt below this fixed part each round.

## What the benchmark measures, and where the time goes

`examples/bench.rs`, per op, best of N runs. Metric -> code path:

| Metric | Path |
|---|---|
| apply N local ops | `History::state` → `TextObj::text`; `History::plan_edit` (`local.rs`) → `History::write` (`history.rs`): counter, `envelope::seal` (`codec::encode_batch`, zstd, Ed25519 sign ≈ 40 µs), `commit_local` → `pending` (decode) → `validate` (`validate.rs`) → `integrate` |
| integrate N batches (random/causal) | `History::receive` (`history.rs`): `Envelope::decode`, batch id (SHA-256), Ed25519 `verify_strict` ≈ 40 µs, zstd + CBOR decode, `hold`/`file`/`wake`/`run`, then `validate` + `integrate`; misses in `materialise` rebuild state from the nearest cached ancestor |
| edit a 1 MiB text | `plan_edit` on a text with one 1M-char insertion, then `write`; patched layout (`Layout::insert_run`, `Layout::hide`) |
| materialise 1 MiB text (warm/cold) | `History::state` → `materialise` (`history.rs`): closure, cache lookup, `State::apply` of missing ops, then `TextObj::layout` → `TextObj::compute` (`text.rs`) and `text()` |
| materialise again (cached) | cached state; `text()` (cached string clone) |
| read again, shared | cached state; `State::text_shared` (no copy; the API callers should use for repeated reads) |

Fixed costs that cannot go: Ed25519 signing and strict verification (spec), SHA-256 batch ids,
the deterministic CBOR checks (decode + re-encode compare, `cbor.rs`).

## Data structures (hot)

- `graph.rs` `Graph`: batches as nodes; `Closure` = per-batch op-count prefix (`Vec<u32>`, one
  entry per batch); `closure()` walks parents, `frontier()`, `ops_between()` sorts ops by
  `OpId` (clones `Arc<ReplicaId>`), `happens_before_eq` DFS pruned by counters.
- `history.rs` `History`: state cache (≤ 32 entries: heads, closure, `Arc<State>`, size);
  `materialise` fast path on exact heads; `take_state` removes the parent's entry so
  validation mutates in place; held-back batches indexed by one missing op (`waits`, `ready`).
- `state.rs` `State`: `BTreeMap<ObjectId, Obj>`; objects behind `Arc` (copy on write). Map
  values: `Vec<(OpId, Value)>` greatest first; observers remove values they saw.
- `text.rs` `TextObj`: insertions (`InsRec`: chars `Vec<char>`, `deleted: Vec<bool>`, `kids`:
  per offset with children, the child insertions newest first), `roots`, `index`
  (`HashMap<InsertionId, u32>`). `Layout` (cached in a `OnceLock<Arc<_>>`): `order`, flat
  `pos` + `first`, `vis_a`, `visible`, `visible_index`, `sealed`, `seal_spans`. Patched in
  place for a newest insert or a delete when the text has no seals and no copies; rebuilt by
  `compute()` otherwise. `text()` caches the visible string. Large texts (an insertion of
  4,096+ chars) with no seals and no copies use a run-based path instead (`compact_layout`,
  `visible_runs`, `visible_ranges`; `local.rs` plans edits from runs); the unit test
  `compact_runs_equal_the_dense_layout` checks it against the full layout.
- `merge.rs`: planner, runs only in `plan_merge`/`resolve` (not in the benchmark).

## Invariants every change must keep

- The order of characters is TL-43's pre-order (children newest first); visibility is TL-45
  (a) with seals (TL-52), then (b) first copy of each canonical id wins.
- States depend only on the set of ops, never on arrival order; caches must equal a rebuild
  (`patched_layout_equals_a_rebuild` checks the layout).
- Every receiver check of TL-101 stays, in its order, with the same verdicts.
- The gate: all 63 conformance vectors, envelope vectors, authorisation, sync, time-travel,
  file-store and randomized convergence tests with exact merged-text checks.

## Ideas not yet tried (estimates, unverified)

- `Closure` per materialise is O(batches): incremental closures or a smarter cache key.
- `ops_between` sorts full `OpId`s; sort by (counter, replica rank) integers instead.
- `Layout` vectors for small or seal/copy texts: `vis_a`/`visible`/`sealed` as bitsets.

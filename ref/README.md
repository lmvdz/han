# HAN v1 reference checks

Everything here is executable. `python ref/run_all.py` runs all of it (the timing model takes 15–40 min; `--quick` skips it) and writes [RESULTS.md](RESULTS.md). The rule → check table is in [architecture.md §10](../architecture.md).

| File | What it is |
|---|---|
| `policy.py` | Reference Policy: `decide`, `fold`, `can_read`, `relevant_actors`, `session_access`, closed body schemas (R-09). Pure Python, no dependencies. |
| `vectors/*.json` | <!--s:vec_n-->140<!--/s--> conformance vectors, including the §1.1 worked example and an RFC 8785 key-order vector. Expectations are hand-written in `make_vectors.py` from architecture.md. They are **not** computed by `policy.py`. |
| `run_vectors.py` | Runs the vectors against `policy.py`, or any implementation exposing the same five functions. Also checks purity and determinism (R-11) on every step. |
| `mutate_vectors.py` | Breaks one Policy rule at a time (<!--s:mut_pol-->188<!--/s--> mutations) and requires the vectors to fail. This shows the vectors test the rules rather than just the happy path. |
| `wire.py` | Signed layer with real Ed25519 (`cryptography` package): ids, signatures, receipts, header **and content** validation, a reference **home** (the §4.1 append pipeline in its one order of verdicts, a steady clock for the R-16 abandon wait, `read` with `at`, `limit`, follow and paged state, context blobs) and a verifying **mirror** (R-14, including the 60 s future-time tolerance), equivocation and wrong-home proofs, gossip signals (R-15), notices, signal rights, per-space blob supply and transfer. |
| `run_wire.py` | <!--s:wire_scen-->50<!--/s--> scenarios over `wire.py`, among them one per adjacent pair of the §4.1 order of verdicts and the paged state, each with its expected result stated from the spec. |
| `wire-vectors/` | <!--s:wv_n-->99<!--/s--> **wire conformance vectors** (architecture.md G-11, D15): one JSON file per operation (<!--s:wv_ops-->21<!--/s-->, from the interop transcripts both reference servers answered byte for byte alike), the error paths both agree on (`probes.json`, <!--s:wv_probes-->39<!--/s-->) and the paths the round-21 decisions fix (`probes-decided.json`, <!--s:wv_decided-->22<!--/s-->). Format, masking and how a server passes: [wire-vectors/README.md](wire-vectors/README.md). |
| `run_wire_vectors.py` | Replays the wire vectors against **any** server by URL: `python ref/run_wire_vectors.py --url ws://host:port/han` (the server in test mode: frozen clock, fixed keys, empty data), `--launch py` / `--launch ts` to start a reference server in test mode, `--check` for the offline self-check that `run_all.py` runs. Standard library plus `cryptography`. |
| `make_wire_vectors.py` | Regenerates `wire-vectors/` from `interop/golden/` (provenance; not needed to run them). |
| `mutate_wire.py` | Breaks one wire-layer rule at a time (<!--s:wire_mut-->58<!--/s--> mutations) and requires the scenarios to fail. |
| `fuzz_fold.py` | <!--s:fuzz_n-->2,000<!--/s--> random histories × <!--s:fuzz_steps-->200<!--/s--> steps. Whenever `decide` allows an entry, `fold` must not raise and must stay pure. Budgets must stay within bounds, and final phases must never change. |
| `model_check.py` | Exhaustive bounded state exploration that drives the **real** `policy.py`, in three scenarios. *Timing*: executor clock offsets, arbitrary network delay, sub-second arrival with whole-second receipts, bounded stall, and launchWindow shortened or lengthened mid-claim. *Drift*: the sequencer's clock 20% fast and the executor's 20% slow. *Single*: revocation, cancel, abandon, two attempts. 10 mutations must each produce a counterexample. A **non-vacuity guard** fails any run in which nothing is ever launched. |
| `model_guard.py` | Exhaustive bounded exploration of P-15 (guarded steering) against the real `policy.py`, with a ghost derived from the invariant sentence. Two agents, six properties (including ExactHolds: Policy holds exactly whom the invariant holds), <!--s:guard_mut-->35<!--/s--> mutations each caught by a property at depth 4; depth 5 (5.2M transitions) takes about 10 min, `--quick` runs depth 4. |
| `list_tests.py` | Lists `impl/tests/*.py` (as pytest collects them), the `cleanroom-ts/interop.ts` scenarios and the wire vectors with their rule tags; `--rules` gives rule to tests. Reads source only. It is how architecture.md §10 is kept truthful about the implementation. |
| `sync_docs.py` | Derives every count and status line of architecture.md §10, README.md and this file (the values between `<!--s:…-->` markers) from `list_tests.py`, the latest `RESULTS.md`, `interop/RESULTS.md` and `HISTORY.md`, and rewrites them. `--check` changes nothing and exits 1 on drift; `run_all.py` runs it last. |
| `check_consistency.py` | Checks that the §5 table, han.ts `Bodies`/`Contents`/`PolicyDoc` and the `policy.py` schemas agree field by field. Also checks that every SpaceState the reference fold produces conforms to han.ts `SpaceState`. |

## Vector format

```json
{ "name": "...", "rules": ["P-07"], "description": "...",
  "ids": { "<label>": "<64-hex id>" },                                  // readable labels for the ids below
  "profiles": { "<actor>": { "actorKind": "human", "operatorConfirmed": false, "keys": { "k1": { "publicKey": "pk", "status": "valid" } } } },
  "steps": [
    { "entry": <Entry with explicit 64-hex "id">, "time": "<receipt time>", "expect": "allow" | "denied" | "invalid",
      "check": { "<dotted path into the space state>": <value> },        // optional
      "profiles": { "<actor>": <view> },                                  // optional; persists for later steps
      "note": "<why>" },                                                  // optional
    { "read": { "space", "actor", "seq" }, "expect": true | false },
    { "relevantActors": <Entry>, "expect": ["<actor>", ...] },
    { "sessionAccess": { "space", "actor", "session" }, "expect": { "watch": bool, "read": bool, "steer": bool } }
  ] }
```

Every vector is fully expanded: every entry field, every profile, the whole policy. Entry ids are real 64-hex strings, but they're derived from labels (listed in `ids`) rather than from the entry hash. That keeps vectors stable and readable, and Policy never recomputes ids. `00-entry-id.json` tests the real id derivation byte for byte, and `wire.py` uses real ids throughout. Checks read the state of the space named by the step's entry. Dotted keys may contain dots (actor ids); resolve them greedily. Policy checks no signatures; `wire.py` does.

A step runs `decide(states[space], entry, ctx)` with `ctx = {time, profiles}` (there is no other context: a fork takes nothing from its parent, P-13). If `expect` is `allow`, the runner folds the entry with a receipt that holds only `space`, `seq` (head + 1), `id` (the entry's `id`) and `time`, and then reads `check`. Profile views are the vector's `profiles` with `actor` and a `seq` added; a step's `profiles` replaces a view from that step on. An actor missing from `profiles` is one the receipt did not pin (`decide` answers `invalid`, P-16).

### What vectors may leave out or stub, and why

Two layers judge an entry (architecture.md R-25). The **Log** owns the wire form; **Policy** owns the body and everything in P-16. Vectors test Policy, so they are written by hand and stub what the Log owns:

| Stubbed or absent | Why it is safe |
|---|---|
| `id` is a label hashed to 64 hex digits, not the §2 derivation | Policy never recomputes ids; it only needs them to be distinct and to resolve. `00-entry-id` and `00-jcs-utf16-order` test the real derivation byte for byte, and `run_wire.py` uses real ids |
| no `sig` | signatures are the Log's (R-12 "bad signature"); Policy only reads whether the key is `valid` in the pinned profile |
| `nonce` is a placeholder such as `n-C`, `contentHash` is `""` with no content, header `time` is fixed | these are Log-owned fields and Policy never reads them (vector `decide-ignores-log-fields` pins that) |
| `accept.sig` is any string | the target's signature is verified by the Log and by mirrors (§7.3); Policy needs only the shape `{keyId, sig}` |
| receipts hold only `space`, `seq`, `id`, `time` | `fold` reads nothing else |
| profile views hold dummy keys (`"publicKey": "pk"`) | Policy reads a key's `status` only |

An implementation that rejects these is checking the wrong layer, and will fail nearly every vector. What vectors never stub: every body is a legal R-09 body unless the step expects `invalid` for that reason, and every policy document is complete.

**What `invalid` and `denied` mean in a vector** is architecture.md P-16: `invalid` when the entry is malformed, names something missing or of the wrong kind, contradicts what it names, or has an impossible place; `denied` when authority or the present state forbids a well-formed entry. The vectors `references-must-be-the-right-kind`, `decision-order` and `kind-check-order` pin the order of checks where both could apply.

## Model-checked properties

| Property | Meaning |
|---|---|
| AtMostOneLaunchPerClaim | no claim is launched twice, even across crashes |
| NoLaunchWithoutReceipt | a launch happens only after its claim is in the log |
| NoLaunchAfterAbandon | once an owner records a claim as abandoned, it is never launched |
| HonestNotStarted / HonestStarted | `started:"no"` ⇒ never launched; `started:"yes"` ⇒ launched |
| RevokedBeforeClaimNeverLaunches | removing the proposer, or revoking the key it signed with, before the claim prevents the launch |
| UnknownNeverSilentlyRetried | total launches ≤ 1 + `effect.resolve` failed statements that were already false when made (made after a launch). Honest resolves never allow a second run |

**Bounds** (printed by each run): one proposal. *Timing*: 1 attempt; executor clock offset 0 or −10 s; any delivery delay; ≤ 4 ticks of +0.9/+9.1 s (chosen to land on every rounding and window boundary); stall ≤ 10 s; ≤ 1 crash; window 20 s changed to 15 or 30 s at any time. *Drift*: as timing, with the sequencer clock 1.2× and the executor clock 0.8× true rate, ticks +0.9/+9.1/+10.9 s, and no window change. *Single*: ≤ 2 attempts, ≤ 2 ticks of +5/+25 s, ≤ 1 crash.

## What these checks do **not** cover

- Durability under real storage faults (R-03), the agent-host obligations (R-17, R-18) and key custody (X-07) need a running implementation. [impl/](../impl/) tests them in part (kill -9 and restart, a scripted agent killed mid-turn, an instance-key lock) but not a power cut, a real model-driven agent or a sandbox; see architecture.md §10.
- The model has one proposer, one proposal and one executor instance. X-01 (instance binding) is checked by vectors, not by the model. Clock offset, delay, rounding and window changes are explored only in the timing scenario (one attempt), to keep the state space tractable.
- The wire scenarios run in-process with no network. The gossip *schedule* (R-15) is specified but not exercised; the proofs it produces and the signals that carry them are. The wire home models the R-16 wait with an injected steady clock, not a real one.
- Bounded exploration is evidence, not proof for unbounded runs.

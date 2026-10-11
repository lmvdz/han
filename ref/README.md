# HAN v1 reference checks

Everything here is executable. `python ref/run_all.py` runs all of it (the timing model takes 20–40 min; `--quick` skips it and runs the guard model at depth 5) and writes [RESULTS.md](RESULTS.md). The rule → check table is in [architecture.md §10](../architecture.md).

| File | What it is |
|---|---|
| `policy.py` | Reference Policy: `decide`, `fold`, `can_read`, `relevant_actors`, `session_access`, closed body schemas (R-09). Pure Python, no dependencies. |
| `vectors/*.json` | 91 conformance vectors, including the §1.1 worked example and an RFC 8785 key-order vector. Expectations are hand-written in `make_vectors.py` from architecture.md. They are **not** computed by `policy.py`. |
| `run_vectors.py` | Runs the vectors against `policy.py`, or any implementation exposing the same five functions. Also checks purity and determinism (R-11) on every step. |
| `mutate_vectors.py` | Breaks one Policy rule at a time (98 mutations) and requires the vectors to fail. This shows the vectors test the rules rather than just the happy path. |
| `wire.py` | Signed layer with real Ed25519 (`cryptography` package): ids, signatures, receipts, header **and content** validation, a reference **home** (the §4.1 append pipeline) and a verifying **mirror** (R-14), equivocation and wrong-home proofs, notices, signal rights, per-space blob supply and transfer. |
| `run_wire.py` | 30 scenarios over `wire.py`, each with its expected result stated from the spec. |
| `mutate_wire.py` | Breaks one wire-layer rule at a time (24 mutations) and requires the scenarios to fail. |
| `fuzz_fold.py` | 2,000 random histories × 200 steps. Whenever `decide` allows an entry, `fold` must not raise and must stay pure. Budgets must stay within bounds, and final phases must never change. |
| `model_check.py` | Exhaustive bounded state exploration that drives the **real** `policy.py`, in three scenarios. *Timing*: executor clock offsets, arbitrary network delay, sub-second arrival with whole-second receipts, bounded stall, and launchWindow shortened or lengthened mid-claim. *Drift*: the sequencer's clock 20% fast and the executor's 20% slow. *Single*: revocation, cancel, abandon, two attempts. 10 mutations must each produce a counterexample. A **non-vacuity guard** fails any run in which nothing is ever launched. |
| `model_guard.py` | Exhaustive bounded exploration of P-15 (guarded steering) against the real `policy.py`, with its own reading of the spec's intent (a ghost of who is held). Properties GuardedClaimsNeedTheirHuman, HeldInstructNoOne, NeverLooser, GuardRightsNeedRole; 18 mutations, each caught by a property; states deduplicated on content, so depth 6 runs in about a minute. |
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
      "predecessor": "<space id>" },                                      // optional; ctx.predecessor = that space's state
    { "read": { "space", "actor", "seq" }, "expect": true | false },
    { "relevantActors": <Entry>, "expect": ["<actor>", ...] },
    { "sessionAccess": { "space", "actor", "session" }, "expect": { "watch": bool, "read": bool, "steer": bool } }
  ] }
```

Every vector is fully expanded: every entry field, every profile, the whole policy. Entry ids are real 64-hex strings, but they're derived from labels (listed in `ids`) rather than from the entry hash. That keeps vectors stable and readable, and Policy never recomputes ids. `00-entry-id.json` tests the real id derivation byte for byte, and `wire.py` uses real ids throughout. Checks read the state of the space named by the step's entry. Dotted keys may contain dots (actor ids); resolve them greedily. Receipts inside `succeeds.through` are structurally complete, but their signatures aren't checked by Policy (`wire.py` checks signatures).

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

- Durability under real storage faults (R-03), the agent-host obligations (R-17, R-18) and key custody (X-07). These need a running implementation.
- The model has one proposer, one proposal and one executor instance. X-01 (instance binding) is checked by vectors, not by the model. Clock offset, delay, rounding and window changes are explored only in the timing scenario (one attempt), to keep the state space tractable.
- The wire scenarios run in-process with no network. The gossip *schedule* (R-15) is specified but not exercised; the proofs it produces are.
- Bounded exploration is evidence, not proof for unbounded runs.

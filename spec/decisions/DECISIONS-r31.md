# Decisions for round 31

Inputs: the round-30 review (75.6, defects W1–W18). The build has started (PLAN.md §10). This round fixes what the build depends on:
- the Policy semantics the Rust core will port;
- what the network layer must implement.

Stay inside recorded guarding (no holds). Prefer pure, testable functions over host duties.

All agents follow these. Where one is silent, the owning agent decides and writes the choice down.

## Delegation (Policy)

| # | Topic | Decision |
|---|---|---|
| X1 | **Per-assignee closure (W1)** | An assignee's carried limits from a task end only when that assignee makes its own final update, or when an owner force-closes the task (an owner's `task.cancel`, or a final update by an owner who isn't an assignee). The author or another assignee closing or declining doesn't end them. Keep the state change minimal: `carried(X)` counts tasks where X is in `accepted` and that no owner force-closed, and X's own final update removes X from `accepted`. Vectors: the reviewer's `p3_selfclose.py` and the co-assignee decline. |
| X2 | **Only linked work carries (W4)** | A carrying agent X's limits (no approving, governing or steering others) stay while X carries. Guarding is stamped only onto linked work: a task whose parent is an accepted task of X (or descends from one), and an effect naming such a task. A task or effect X creates with **no** parent or task while carrying gets `carried(X)`, which closes the parentless escape. Work X creates under a parent it legitimately holds and that isn't linked carries only that parent's own guarding. §7.5 states the residue: laundering guarded work under unrelated legitimate work is a host duty (R-24 naming). Vectors: the reviewer's `p4_cascade.py` (no spread to unrelated tasks or teammates), a parentless task while carrying (carries), and a linked subtask (carries). |
| X3 | **Carrying executors need the guard (W5)** | An effect's `guardedFor` includes `carried(executor)` at the proposal, mirroring a guarded executor. Vector: the reviewer's `p1` W-a. |
| X4 | **Only an explicit accept accepts (W10)** | Only `task.update` with status `accepted` puts the author into `accepted`; `progress` and other statuses don't. An author assigning itself has accepted at creation, as before. Vector: a progress update doesn't accept. |
| X5 | **Approvals before accepting (W6), not changed** | The owner chose to keep this. An approval made before its author accepted a task from a guarded agent keeps counting. §7.5 says so as residue, and the README doesn't promise otherwise. |
| X6 | **Workable bound (W11)** | At task creation the guarded-for set may hold at most 17 agents, so a proposer and an executor always fit under 19. Vector: the reviewer's `p2_bound.py` (a task at 18 is denied; one at 17 is workable). |
| X7 | **Denied servers' approvals (W12)** | E3 doesn't use an approval whose author's server is denied by the current policy. The `policy.set` fold that denies a server drops those open approvals. Vector: the reviewer's `p5_deny.py`. |
| X8 | **Tables match step 8 (W17)** | The §6.1 `session.input` row and the P-04 `effect.approve` row say "limited author", as step 8 does. |

## Network layer (§8 and the N-rules)

| # | Topic | Decision |
|---|---|---|
| X9 | **The peer filter is a pure function, and public reads work (W2)** | Define `admit(connection facts, state) → allow \| deny(reason)` as a pure function in the spec. Make it testable by vectors, like Policy, and list it beside Policy's functions or as a profile of N-02. Any node is admitted for `hello` and for reads of `visibility: "public"` spaces and profile spaces, rate-limited per node. Members' devices are admitted for everything their rights allow. G-08 authenticates a foreign actor by fetching and pinning its profile, as §4.1 step 4 does, and doesn't answer `not_found`. This fixes the contradictions with §1.1, P-02, G-03, G-09 and §4.1. |
| X10 | **Addressing (W3)** | `/.well-known/han` and invite tickets carry, for each node id, its direct addresses (`ip:port`) and relay URLs. Every implementation MUST support dialling a node from these, and MAY use other discovery (such as iroh's) in addition. Fetching the well-known over HTTPS is the one mandatory discovery path, so a public federated network can form from the docs alone. |
| X11 | **Status delivery (W7)** | Add `StreamItem {type: "profile", name, body}`, delivered under the watch right like `status`. `han.status/1` items travel as these; a profile defines its `body`. Bulk CBOR streams are defined in X15. |
| X12 | **`join` after `hello` (W8)** | `join` is valid only on a connection with a successful `hello`. Fix G-07 (3) to match N-03 and han.ts. |
| X13 | **Tickets name the server (W9)** | `InviteTicket` gains `server` (the ServerName to put in `dialed`), so tickets work after a `space.move`. |
| X14 | **Revoked devices drop (W13)** | A node re-pins the profiles of connected devices at least every 300 s (the R-10 freshness window), and closes a connection whose device key is revoked. |
| X15 | **Node identity and CBOR bulk (W14, W15)** | **Node identity:** a node id is a device key. All actors a device runs (a person, named agents, an executor instance) list that device key in their profiles with `key.add`, so N-02 can map node to actors; an executor instance with its own credentials MAY have its own device key. **CBOR bulk:** a bulk batch is the CBOR bytes plus a signature over the domain prefix `han/bulk/1` ‖ profile name ‖ bytes, by a listed key of the author, with its key id. A profile names the stream type. |
| X16 | **Carrier checks (W18)** | Add vectors that need no live QUIC stack: `admit` decisions; frame encoding (length prefix, I-JSON); control-stream challenge and hello order; well-known and ticket parsing (addresses, relays, server); `han.status/1` and `profile` stream items; bulk envelope signatures. Live carrier tests come with the Rust implementation (M2) and are listed in §10 as such. |

## Words

| # | Topic | Decision |
|---|---|---|
| X17 | **Promises (W16)** | **README:** "nothing happens without an approval" holds only where a rule or a gate requires one; say that. "Without servers" becomes "without port forwarding; spaces still have a home server name". **goal.md's delegate sentence:** add "until the task closes for it". **DECISIONS-r30's V14 note:** correct the fuzz statement. **V8:** mark it done (the witness agent did it in round 30). |

## Round 31 notes

### Delegation (X1–X8)

- **No new state field.** X1 reuses `accepted`: an assignee's own final update removes it from `accepted`, and an owner's force-close (`task.cancel` by an owner, or a final update by an owner who isn't an assignee) empties it. `carried(X)` drops its `open` condition and is the union of the `guardedFor` of the tasks whose `accepted` lists X. "No owner force-closed" is therefore not a flag: the force-close clears the list. A task's `open` and `state` keep their round-30 meaning (the first final update closes the task for everyone else).
- **A closed task takes one more update.** An assignee still in `accepted` may make its own final update on a closed task (P-05; P-16's `task.update` row: "D closed, unless the state is final and the author is in its `accepted`"). That update changes nothing but `accepted`: `state`, `open` and the budget stay as the first close left them. Without it, a delegate whose task the author closed could never end its own limits. Any other update on a closed task, and a cancel, stay `denied`. An owner can't force-close a task that is already closed; the delegate's host ends it (R-24 now says the host MUST make the agent's own final update once it stops working a task).
- **X2, linked work.** A task's `guardedFor` is its parent's list, or `carried(author)` when it has no parent; an effect's is its task's list, or `carried(proposer)` when it names none. A parent that descends from one of the author's accepted tasks already holds that task's list (lists never shrink down a chain), so "the parent's list" is exactly X2's linked case and no ancestry walk is needed. Work under an unrelated parent or task carries only that one's list. Two consequences, both written into §7.5 and R-24. First, a host that files guarded work under unrelated work launders it, so the naming is now part of the guarantee (R-24). Second, a carrying agent's parentless task still carries, so if its assignee accepts it the guarding reaches that assignee; this is by design, since it is the parentless escape X2 closes. The reviewer's `p4_cascade` no longer spreads at all, because the lead only reported progress (X4); vector `only-linked-work-carries` also covers the lead accepting.
- **X3 is unconditional.** `carried(executor)` is added to every effect's list, task named or not, as a guarded executor is added whatever the task. So a carrying delegate that executes its own proposal under an unrelated task still needs the delegator's guard. This is stricter than X2 alone. It was kept because the executor is the actor that acts, and the alternative (skip it when the proposer executes under a named task) would be one more predicate (C2).
- **X6.** The task bound is `MAX_GUARDED_FOR − 2` (17) on the task's own `guardedFor`; an unguarded author no longer counts. There is no new constant: han.ts says "MAX_GUARDED_FOR − 2". The proposal bound is unchanged (19 with the proposer and executor), so the P-08 proof and the 36/55 bounds are unchanged. A task at 17 is workable by any unguarded, non-carrying proposer and executor; a carrying one may still exceed the proposal bound, since it brings its own carried agents.
- **X7.** `approval_guarded` became `approval_dead`: the author is guarded (by choice, or forced judged from the recorded `actorKind`), or its server is in the current `servers.deny`. The `policy.set` fold drops those open approvals, and a not-started outcome drops a returned one. E3 itself is unchanged: like a guarded author's, a denied server's approval never reaches E3, because the state holds none (P-07 E3 says so).
- **X8.** P-16 step 8 now names the predicate: a *limited* author (guarded, or `carried` not empty). The `session.input` row and P-04's `effect.approve` row say "limited author".
- **P-15 length.** 558 words before, 556 after (from the P-15 heading to the `---` that ends §6.6). Added: the linked/unlinked rule, `carried(executor)`, the 17/19 split, and the per-assignee end of the limits. Cut: "So a guarded agent may delegate: what a delegate … waits for its guard until the task closes" (said by `carried` and §7.5), "owners included", and the duplicated "each counted" wording.
- **Vectors.** Recast:
  - `assignees-carry-their-tasks-guarding`: progress doesn't accept; a co-assignee's close ends nothing; BOT2's own final update on the closed task.
  - `delegates-carry-the-limits`: the owner closes its own part before its `policy.set`.
  - `guarded-tasks-are-bounded`: X6 at 17, X3, and X2's `q5`.

  New:
  - `closing-ends-only-the-closers-carry`: `p3_selfclose`, the co-assignee decline, both force-closes, the closed-task rules.
  - `only-linked-work-carries`: `p4_cascade`.
  - `carrying-executors-need-the-guard`: `p1` W-a.
  - `only-an-explicit-accept-accepts`.
  - `guarded-tasks-leave-room-to-work`: `p2_bound`, 17 workable and 18 denied. The 18 case is a parentless task of a carrying guarded agent, since a subtask at that depth would be too deep.
  - `denied-servers-approvals-are-dropped`: `p5_deny`, and the returned approval.
  - `grammar-bounds` (X18).

  Mutations: "closed tasks still carry" and "only an accepted update accepts" were retired (each is now the rule). 16 were added (X1 ×8, X2 ×2, X3, X4, X6, X7, X18 ×2), and the anchors X2 touched moved.
- **Models.** model_guard:
  - New moves: t7 (BOT to BOT2 and P); t8 (BOT2's subtask under P's unrelated t5); pr10 (BOT2's work named under t5); every assignee's own final update, on a closed task too; BOT2's progress report; an owner's force-close and cancel.
  - The ghost carries per X1, records per X2 and X3, and accepts per X4.
  - DelegateChain is narrowed to unlinked work, plus the executor's carry.
  - New non-vacuity flags: "carry outlives the close" and "carrying executor".
  - Eight mutations added (X1 ×4, X2 ×2, X3, X4); "closed-tasks-still-carry" retired.
  - A denied server is not modelled (the model's policy never denies one); the vector covers it.

  fuzz_fold checks X1 (`accepted` after every update and cancel, and that a closed task takes only an accepted assignee's own final update), X2, X3, X4 and X6 from the definitions. It reports how often a closed task took a final update and how often an owner's cancel ended a carry. `bound_run` now mixes progress reports (no acceptance) into its acceptances. model_check was re-run because a not-started outcome's fold changed (X7).

### Network layer (X9–X16)

- **X9, `admit`.** It is placed as N-02 and as han.ts `PeerFilter`, beside `Policy` but not inside it: Policy's six functions judge entries, `admit` judges calls. Inputs: the connection (`node`, `call`, `space`, and after `hello` the actor with the node ids that vouch for it) and, per served space, `Pick<SpaceState, "policy" | "roles">`. Output: `{allow, tier}` with the tiers `member`, `server` and `open`, or `{allow: false, reason}`. Choices:
  1. A stranger must still say `hello` (as any actor whose profile can be fetched) before it reads, so a read is always attributable.
  2. Member status is "holds a role in any space this node serves". The call's own rules then judge it; there is no per-space check in `admit`, which keeps it one rule.
  3. A server actor is admitted when its node is in its own well-known `sequencer`, with no "federates with" condition, so G-03 first contact works.
  4. The open tier also takes `cancel`; reads of spaces the node doesn't hold (so §4.1 step 2 can answer `moved`); and appends into `join: "open"` spaces. X9 didn't list the last, but P-03's open join is a stranger's append, and without it open joins break. `signal` is not open: G-04's read right is necessary, not sufficient.
  5. Round 30's 24-hour joining grace is gone. A joined device reads its own profile at its own server, where it is a member, and learns of its admission from the G-03 notice.

  Rate limits stay the host's: they need counters, so they can't be part of a pure function.
- **G-08, foreign actors.** A foreign `who` is fetched and pinned as §4.1 step 4 does. It is never `not_found` for being foreign: `unavailable` when it can't be fetched now. An actor of this server with no profile space still answers `not_found`, so wire vector `D11a` stands.
- **X10.** `sequencer` became `NodeAddr[]` (`{node, addrs, relays}`): 1–16 nodes, each with at most 16 addresses and 16 relays and at least one of the two. Addresses are canonical IPv4, or bracketed IPv6, text with a port 1–65535; relays are `https://` URLs. A ticket's `node` is a `NodeAddr` too, so a ticket alone is enough to dial.
- **X11.** `StreamItem` gains `{type: "profile", name, body}`. The name grammar is `<prefix>.<name…>/<version>` (`han.status/1`); `body` is opaque Json (depth ≤ 32). `han.status-1.md` now says it travels as a profile item.
- **X12, X13.** G-07 (3) says `join` is `denied` before `hello`, like every other call. N-03 closes the connection after a failed `join` (moved there from the old N-02 (c)). `InviteTicket` gains `server`; the client puts it in `dialed`, so a ticket works after a move.
- **X14.** In N-02: re-pin the profiles behind open connections at least every 300 s, and on any revoking receipt the node holds; close a connection whose node no longer vouches for its actor, or whose tier no longer holds.
- **X15.** N-01 states that one device key may vouch for several actors (each profile lists it with `key.add`). Bulk: the batch is the CBOR map `{1: author, 2: keyId, 3: profile, 4: data, 5: sig}`, and the signed bytes are `"han/bulk/1\n" ‖ UTF-8(profile) ‖ "\n" ‖ data`. The newlines were added to X15's `han/bulk/1 ‖ profile ‖ bytes` so the profile name and the data can't run together; a vector checks that the unseparated form fails.
- **X16.** New: `ref/carrier.py`, `ref/make_carrier_vectors.py`, `ref/run_carrier_vectors.py` and `conformance/carrier-vectors/`, with a README that gives the format. The vector files are `admit`, `frames`, `control`, `wellknown`, `tickets`, `stream-items`, `bulk`, `ed25519` and `header`.
  - `--mutations` breaks one rule of `carrier.py` or `wire.py` at a time (24, all caught). Two mutations were left out as equivalent here: dropping the S < L check (OpenSSL refuses S ≥ L too; the vector pins it for other implementations), and dropping the x = 0 sign check (its only points are of small order and refused anyway).
  - `run_all.py` is outside this round's scope, so the runner isn't a row there yet. `sync_docs.py` runs it to derive its counts, and fails if it doesn't pass.
  - `wire.CALLS` gained `invite` and `join`, the frame layer of `han/1`. The legacy WebSocket servers never had them, and no wire vector uses them.

### X18, Rust port clarifications

1. **Content presence.** §4.1 step 3 now says content is present exactly when `contentHash ≠ ""`. A kind whose content fields are all optional may omit it; one with a required field may not. This is what `ref/wire.py` does.
2. **Time and ServerName.** A Time's year is 0001–9999, as `strptime` accepts. A ServerName is at most 253 characters; `policy.py` checks it with a lookahead in the ServerName, ActorId and SpaceId patterns. Vector `grammar-bounds`; mutation "ServerNames of any length".
3. **`-0`.** `wire.frame_verdict` refuses `-0` (through an `int` hook) as not I-JSON: id 0 and a close, G-07 (1). Carrier vectors `frames.json`.
4. **Depth.** One definition (§2): a scalar has depth 0, a container 1 + the depth of its deepest member. A frame has depth ≤ 128 (deeper is G-07 (1)); opaque Json ≤ 32 (`invalid`). `policy.json_ok` was off by one (it allowed 33 containers) and now allows exactly 32. Vectors: `frames.json` (128 and 129), `grammar-bounds` and `stream-items.json` (32 and 33).
5. **Ed25519.** §2 mandates strict verification. A public key must be the canonical encoding of a curve point that is not of small order, else it is `invalid` where it is parsed (`key.add`, a profile's first key, node ids, well-known keys). A signature needs a canonical R that is not of small order and an S < L, then passes the cofactorless equation. `wire.py` makes the point checks in pure Python before it calls `cryptography`. `ed25519.json` pins 11 cases. Two of them pass OpenSSL's plain verify and must still be refused: the identity key with R = identity and S = 0, and a small-order R built to satisfy the bare equation. `header.json` checks that a `key.add` with a small-order or off-curve key is `invalid` at parse.
6. **Seeds.** `wire.Key` now derives its private key as `sha256(UTF-8 seed)`, like `conformance/wire-vectors/config.json` and `make_wire_vectors.py`; the `"key:"` prefix is gone. Only `run_wire.py`'s own scenario keys change; no published vector does (make_wire_vectors reports no STALE GOLDEN).
7. **Nonce.** All 26 characters are free (130 bits), and a sender draws at least 128 of the bits at random. §2 says so, and `header.json` has a nonce that ends in `7` (low bits set).
8. **Key order.** Receivers accept members in any order; ids and hashes are always computed over JCS. §2 says so, and `frames.json` has a request frame with its members reversed.

### Words (X17)

- README: the approval sentence is scoped to "where a space's approval rule or an agent's guard requires one", and "without servers" became "without port forwarding; spaces still have a home server name".
- goal.md's delegate sentence ends "until the task closes for it".
- DECISIONS-r30: the V14 fuzz note is corrected, and V8 is marked done.

### Results

- *Corrected in round 32 (Z10).* fuzz_diff agrees: 1000 seeds × 250 steps, 300,206 vector steps, 0 seeds diverge (`ref/RESULTS.md` of the round-31 commit). The TypeScript witness follows round 31 in the same commit (`witnesses/cleanroom-ts/policy.ts`: X1, X2, X3, X4). An earlier draft of this note listed divergences in `accepted` (X1, X4) and in the order of `guardedFor` (X2, X3); they were measured before the witness followed and were never true of the merged round.

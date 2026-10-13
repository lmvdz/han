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

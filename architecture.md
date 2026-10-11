# HAN v1 — architecture

Status: design draft, round 6. Companions: [goal.md](goal.md) (why), [han.ts](han.ts) (exact types; normative for shapes), [coverage.md](coverage.md) (fate of every v0.4 guarantee), [ref/](ref/) (executable reference, conformance vectors, fuzzer, model checker; run `python ref/run_all.py`).
Normative words MUST / MUST NOT / SHOULD / MAY follow BCP 14. Rules are `R-xx` (system), `P-xx` (Policy), `X-xx` (executor). §10 lists every rule with the check that covers it, or says plainly that none does. Retired rule numbers (R-19, P-14, X-08) are not reused.

---

## 1. The whole picture

```
  human app        agent host          executor              (all are "actors")
     │                 │                  │
     └──── six calls ──┴──── six calls ───┘
                       ▼
               ┌───────────────┐   the same six calls   ┌───────────────┐
               │  home server  │ ◀────────────────────▶ │  mirror server │
               │  a.example    │      (federation)      │  b.example     │
               └───────────────┘                        └───────────────┘
   inside each server:  Log · Policy · Identity · Blobs · Federation · Signals
   inside actors:       Executor (acts in the world) · Agent host (thinks)
```

Everything durable is an **entry** in a **space**. A space is a conversation-shaped container: a DM, a team room, a task thread, an agent's workbench, or an actor's profile. Each space has exactly one **home** server at a time, which orders its entries. Other servers **mirror** it and re-verify everything.

| What people do | Entry kinds |
|---|---|
| **Talk** | `message` |
| **Ask for work** | `task`, `task.update`, `task.cancel` |
| **Touch the world** | `effect.propose`, `effect.approve`, `effect.claim`, `effect.outcome`, `effect.resolve`, `effect.cancel` |
| **Govern** | `space.create`, `member.set`, `policy.set`, `key.add`, `key.revoke`, `redact`, `checkpoint`, `space.move` |

**18 kinds, six calls, one log per space.** That's the whole core.

### 1.1 A worked example: a human hires an agent on another server

Ada (`@ada:a.example`) wants a code fix from an agent, `@fixer:b.example`, operated by Ana (`@ana:b.example`). The agent's changes are pushed by an executor, `@git:c.example`, operated by `@ops:c.example`. Amounts are in `USD-cent`. This example is also the vector `ref/vectors/*-worked-example-1.1.json`.

| # | Who → where | Entry / call | What happens |
|---|---|---|---|
| 0 | b.example, c.example | `profile:@fixer:b.example` holds `space.create {profile: {actorKind: "agent", operator: @ana}}`, then Ana's `member.set {actor: @ana, role: owner}`. `profile:@git:c.example` is set up the same way. | both non-human actors exist, and their human operators have confirmed them (§4.3) |
| 1 | Ada → a.example | `space.create {policy: {join: "invite", allowAgents: "with-operator", approvals: [{action: "git.push", approvers: ["owner"]}], budget: {unit: "USD-cent", amount: 500}, launchWindow: 30, …}}` in space `r1:a.example` | a private room, seq 1; Ada is owner |
| 2 | Ada → a.example | `member.set {actor: @fixer:b.example, role: member}`, and the same for `@git:c.example` | P-03 checks the confirmed operators; the home sends **notices** to b.example and c.example (R-20) |
| 3 | b.example, c.example | `read(r1:a.example, 0, {follow: true})` as `@:b.example` / `@:c.example` | their servers start mirroring and verifying (R-14) |
| 4 | Ada | `task {to: [@fixer], budget: {USD-cent, 300}}` + content `{ask: "fix the login bug"}` | 300 of the 500 is reserved for this task (P-05) |
| 5 | fixer → b.example → a.example | `task.update {state: accepted}`; `effect.propose {executor: @git, action: "git.push", cost: {USD-cent, 50}, task, planDigest}` + content `{plan}` | forwarded to the home; the proposal waits for approval (P-08) |
| 6 | Ada | `effect.approve {proposal, planDigest}` | phase `ready` |
| 7 | git executor | journal `claiming`; send `effect.claim {proposal, attempt: 1, instance: k1}` | at the claim's seq Policy re-checks the agent's membership, operator and key, the approval and the budget (E1–E6), reserves 50 from the task, and pins the 30 s window into the claim |
| 8 | git executor | journal `launching`; check the plan against its digest; deadline check on its own clock; push | X-03/X-04/X-09 |
| 9 | git executor | `effect.outcome {result: succeeded, started: yes, cost: {USD-cent, 0}}` + evidence blob | the 50 goes back to the task; phase `succeeded` |
| 10 | fixer | `task.update {state: done}` + content `{text: "pushed abc123"}` | the task closes; its unspent 300 returns to the space budget |

If the executor crashes between steps 7 and 9, X-06 reports `failed`/`not started`, or `unknown`; it never launches again on its own. If Ada bans the agent between 5 and 7, the claim is denied (E2). If Ada shortens `launchWindow` after step 7, the claim keeps its pinned 30 s (P-09). If a.example dies for good, the room stays readable on b.example's and c.example's mirrors. Ada forks it to a new home (§7.4), and the fork starts with no inherited authority or open effects.

---

## 2. The primitive: entry, content, receipt

Exact shapes: [han.ts](han.ts) `Entry`, `Receipt`, `LogRecord`.

An entry has two parts:
- the **header**, which holds every field Policy needs (`body`) plus routing and signing fields. It is permanent;
- the **content**, which holds human-meaningful payload (text, plans, results, blob lists). It is redactable.

```
Entry       = { v:1, space, kind, author, keyId, body, contentHash, refs, seen, nonce, time, sig }   -- exactly these fields
contentHash = sha256( JCS(content) )             -- "" when the kind has no content
id          = sha256( "han-entry-v1\n" ‖ JCS(entry minus sig) )
sig         = Ed25519( key(author,keyId), "han-sig-v1\n" ‖ id )
Receipt     = { space, seq, id, prev, home, homeKey, time, durability, profiles, homeSig }
receiptHash = sha256( "han-receipt-v1\n" ‖ JCS(receipt minus homeSig) )
homeSig     = Ed25519( key(home,homeKey), "han-home-sig-v1\n" ‖ receiptHash )
prev        = receiptHash of seq−1 ("" for seq 1)
LogRecord   = { entry, content, redacted, redactedBy?, receipt }   -- redacted: true ⇒ content is null, redactedBy is the proof
```

Every `‖` concatenates UTF-8 bytes. `id` and `receiptHash` enter signatures as their 64-character lowercase hex strings. JCS is RFC 8785, including its key ordering by UTF-16 code units (vector `00-jcs-utf16-order`). Public keys are raw 32-byte Ed25519 keys, and signatures are 64 bytes; both are base64url without padding. Every number on the wire, including inside opaque Json such as `plan.args` and extension bodies, is an integer in [0, 2^53). The one exception is the state's `budgetLeft.amount`, which may be negative (P-06). Fractions are `invalid`, which keeps JCS exact. Reference: [ref/wire.py](ref/wire.py).

**Grammar and limits** (also in han.ts):

| Item | Rule |
|---|---|
| ActorId | `@` + name `[a-z0-9._-]{0,64}` + `:` + ServerName; the empty name is the server actor |
| ServerName | lowercase DNS name: labels `[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?`, dot-separated |
| SpaceId | `[a-z0-9]{1,26}` + `:` + ServerName (the space's **first home**), or `profile:` + ActorId |
| EntryId, Hex | `[0-9a-f]{64}` |
| nonce | `[a-z2-7]{26}` (128 bits, base32) |
| KeyId | `[A-Za-z0-9_-]{1,64}`; single-use per actor (§4.3) |
| Time | `YYYY-MM-DDTHH:MM:SSZ`, UTC, a real proleptic Gregorian date, seconds 00–59, no fraction. A home writes `receipt.time` by **rounding its clock down** to the second. |
| action | `[a-z0-9][a-z0-9._-]{0,127}`; `*` only in approval rules |
| Budget | `unit` `[A-Za-z0-9_-]{1,32}`, `amount` integer |
| BlobRef | `hash` Hex; `size` ≤ 4 GiB; `type` a media type, each half `[a-z0-9][a-z0-9!#$&^_.+-]{0,63}` |
| content strings (UTF-8 bytes) | `name` ≤ 256 · `text`, `ask` ≤ 32,768 · `note` ≤ 4,096 · `reason` ≤ 1,024 · `plan.target` ≤ 1,024 |
| lists | `to`, `refs`, `blobs`, `links` ≤ 64 each; approval rules ≤ 64; `servers.deny` ≤ 256 |
| extension kind | `<prefix>.<name>`: prefix `[a-z][a-z0-9-]{0,31}` and not a core prefix; name `[a-z0-9][a-z0-9._-]{0,63}` |
| maxDelegationDepth | 0–16 |
| launchWindow | 15–3600 seconds |
| publicKey | base64url, 1–128 chars |

**R-01 Idempotent append.** If an entry's `id` is already in the space, `append` MUST return the stored receipt *before any other check*, including signature, key validity, schema, policy and limits, and change nothing. The id stays in the log forever, so this holds for the life of the space.
**R-02 One chain.** A space's receipts form one chain (`seq` 1, 2, … with `prev` linking). The **home at seq k** is the server named in the space id, changed by every `space.move` ordered before `k`. Two kinds of portable misbehaviour proof exist:
- *equivocation*: two valid receipts with the same `(space, seq)` but different `receiptHash`, signed by the same server;
- *wrong home*: a valid receipt at seq `k` signed by a server that wasn't the home at `k`.

A space may move back to a former home (A → B → A). Only the home at each seq may sign that seq. **Finding the current home:** any server that mirrors a space knows its home from the chain. It forwards appends there (§4.1 step 2), or answers `moved`, so the current home can be found from any mirror, not only from the server named in the space id.
**R-03 Durable before receipt.** A home MUST NOT return a receipt before the record is durable at the stated `durability`: `"local"` survives process crash, `"replicated"` survives the loss of any one machine. It MUST NOT state a level it has not achieved.
**R-03a Receipt time.** Within a space, `receipt.time` MUST NOT decrease, including across a cooperative move. A home whose clock is behind the last receipt time MUST answer `unavailable` until it catches up, and never stamp an earlier time. Every party's clock MUST keep its rate within 1% of true time. Mirrors check the first part. The second is a stated trust assumption (§7.5). Offsets between clocks don't matter: no rule compares two parties' clocks (X-04).
**R-04 Immutability and redaction.** Headers never change and are kept for the life of the space (v1 has no compaction, so every `after` stays readable and every mirror can replay from seq 1). `redact` (P-12) deletes the *content* everywhere it's honoured, and sets the record's `redacted` flag. A redacted record MUST carry `redactedBy`: the signed `redact` entry and its receipt, which mirrors verify (R-14), so a home can't silently erase content. The header, `contentHash`, signature and receipt stay, so every record remains verifiable and Policy replay is unaffected.
**R-05 Same-space references.** `refs` and every `EntryId` inside a `body` refer to entries already in the same space. Cross-space links are content only (`Link` in han.ts) and carry no authority.

---

## 3. The wire: six calls

One WebSocket per peer at `wss://<server>/han`. Clients and servers use the same calls. A server is a client authenticated as its server actor `@:server`.

**Frames** (JSON text, ≤ 1.25 MiB):
- server → first: `{ "challenge": <base32 128-bit>, "server": <name>, "versions": [1], "time": <Time> }`
- request: `{ "id": <int>, "call": <name>, "args": {…} }`
- reply: `{ "id", "ok": … }` or `{ "id", "error": WireError }`
- stream item: `{ "id", "item": ReadItem }`, end: `{ "id", "end": true }`

| Call | Args → result | Retry |
|---|---|---|
| `hello` | `{who, keyId, proof, version, knownHeads?}` → `{limits, durability, time, knownHeads}`, where `proof = Ed25519(key, "han-hello-v1\n" ‖ challenge ‖ "\n" ‖ dialed)`; `dialed` is the server name the client connected to. The client aborts if `challenge.server ≠ dialed`. `knownHeads` (≤ 256) are answered only for spaces the peer named and may read (R-15). | new connection |
| `append` | `{entry, content?}` → `Receipt` | **always safe with the same entry** (R-01) |
| `read` | `{space, after, follow?, state?, limit?, knownHeads?}` → stream of `ReadItem`, in this order: `{type:"state"}` if asked (it needs read rights at the head and omits the `entries` index); `{type:"heads"}` if `knownHeads` was sent; then `{type:"record"}` in seq order, at most `limit` (1–1000, default 1000) of them, only for seqs the reader may read (P-02). Without `follow` the stream then ends; with `follow` it stays open, and `{type:"signal"}` items arrive interleaved and unordered. An actor following **its own** profile space receives its notices there (R-20). | safe |
| `signal` | `{space, body}` → `null`; delivered to the space's followers as `{type:"signal", from, body}`, never stored (R-22) | never |
| `blobPut` | `{ref, offset, bytes(base64)}` → `{have, complete}`; `bytes` ≤ 768 KiB | safe per `(ref, offset)` |
| `blobGet` | `{ref, space, start?, end?}` → `{bytes}`; half-open range `[start, end)`, default the whole blob; more than 768 KiB → `limit` | safe |

**Fixed v1 limits** (also returned by `hello`): frame ≤ 1.25 MiB; header JCS ≤ 16 KiB; content JCS ≤ 64 KiB; blob ≤ 4 GiB; blob part and blob range ≤ 768 KiB (1,048,576 base64 characters, which fits a frame with its envelope); follows per connection ≤ 256. A state item that wouldn't fit a frame is refused with `limit`, and the reader replays records instead. A server MAY rate-limit (`limit` + `retryAfterMs`). It MUST NOT lower the size limits.

**Errors:** `denied` (policy/auth; never retry the same entry) · `invalid` (malformed, bad signature, bad schema; never) · `not_found` (never) · `limit` (after `retryAfterMs`) · `unavailable` (same request later) · `moved` (same request at `movedTo`).

**R-06 Unknown outcome.** If `append` fails with `unavailable` or the connection drops, the outcome is unknown. The sender MUST retry the identical entry until it gets a receipt or a definitive error (`denied`/`invalid`), and MUST NOT express the same intent with a fresh nonce meanwhile.
**R-07 Outbox.** Every actor MUST durably store each signed entry (and content) *before its first send*. After a restart it MUST finish sending every stored, unconfirmed entry under R-06 before appending anything new to that space. This is what makes R-06 survive crashes.
**R-08 No silent downgrade.** An unsupported version MUST fail with `invalid`. A home that can't currently reach the space's `minDurability` MUST fail appends with `unavailable`. Two exceptions keep a space from locking itself: `policy.set` and `space.move` are always ordered, and a home MUST refuse (`unavailable`) a `space.create` or `policy.set` whose `minDurability` it can't provide at all.

---

## 4. Modules

The interface column lists the *whole* interface. The "hides" column is why the module exists.

| # | Module | Interface | Hides |
|---|---|---|---|
| M1 | **Log** | `append(entry, content?) → Receipt`<br>`read(space, after, opts, as) → stream` | ordering, the hash chain, receipt signing and server keys, durability, dedupe, header and content validation, abuse limits, profile pinning and freshness, redaction, record fan-out, read filtering, forwarding to a remote home, home moves |
| M2 | **Policy** | four pure functions: `decide(state, entry, ctx)`, `fold(state, record, ctx)`, `canRead(state, reader, seq)`, `relevantActors(state, entry)` | roles, joining, invites, tasks, delegation, the budget ledger, the effect state machine, approvals, read rights, forks (all of §6) |
| M3 | **Identity** | `profile(actor, minSeq?) → ProfileView`<br>`verify(entry, view) → ok \| invalid` | profile spaces, key history, rotation and recovery, actor kinds, operator confirmation, caching and freshness, server key discovery (`.well-known`) and key windows |
| M4 | **Blobs** | `put(ref, offset, bytes, as) → progress`<br>`get(ref, space, as, range) → bytes` | chunking, resume, hash checking, storage dedupe, per-space supply and access (R-13), quotas, retention, garbage collection, fetching missing blobs from other servers (R-21) |
| M5 | **Federation** | `forward(entry, content) → Receipt`<br>`mirror(space) → MirrorStatus` | server discovery and TLS pinning, connection pools, retry/backoff, pulling and verifying records (R-14), retaining and fetching profiles and server keys, head gossip and misbehaviour proofs (R-15), home-move handover, sending and receiving notices (R-20) |
| M6 | **Signals** | `publish(space, from, body)`<br>`subscribe(space, as) → stream` | **all** signal fan-out (presence, typing, live tokens, notices), relaying across servers, per-actor notice inboxes, rate limits, dropping under load |
| M7 | **Executor** (an actor) | `deliver(record)` | deciding what to claim, the claim/launch/expired journal and X-01…X-09, monotonic deadlines, plan checking, sandboxes, credentials, workspaces and checkouts, snapshots, native lookups, cancellation of running work, its outbox |
| M8 | **Agent host** (an actor) | `deliver(record)` | models, prompts, memory, the agent framework, turning thoughts into entries, its outbox |

M2 is four functions, but its interface carries no state and has no side effects: everything it hides is *rules*. M7 and M8 are both **actors**. Each is built with a `Client` (the six calls, as that actor) and is fed every record and signal of the spaces it follows. It answers only through its client. Its interface is `deliver(item)`, where `item` is a `ReadItem`: a record, or a signal (including its own notices). This is the same interface a human app implements.

### 4.1 M1 Log

On `append(entry, content)` the home runs these steps. Steps 2–9 run in one serial transaction per space.

1. **Dedupe:** if `id` exists, return its receipt (R-01).
2. **Not home?** Then `Federation.forward` to the home and return its answer, or `moved` / `unavailable`. A `space.create` is accepted only by the server named in its space id; that server decides, by its own rules, whether to host it.
3. **Shape:** the header has exactly the `Entry` fields, each in its grammar (§2); the body matches the kind's closed schema (R-09); content is present iff `contentHash ≠ ""`, matches it, and matches the kind's content schema and checks (§5: `planDigest = sha256(JCS(plan))`, `plan.action = action`); sizes are within limits; `seen` ≤ head; `refs` resolve (R-05). Server rate limits and block lists apply here.
4. **Profiles:** pin `Identity.profile(actor)` for every actor `relevantActors` returns (P-01, R-10). If any is unavailable → `unavailable`.
5. **Signature:** `Identity.verify(entry, profiles[author])`.
6. **Policy:** `Policy.decide(state, entry, ctx)` with `ctx = {time: now (rounded down to the second), profiles}`.
7. Assign `seq`, `prev`, `time`, `durability`, and `profiles: {actor: profileSeq}`. Sign.
8. Make durable at the stated level. Then `Policy.fold`, which never fails for an allowed entry (`ref/fuzz_fold.py`).
9. Return the receipt. Hand the record to followers. Mark blobs this author supplied (R-13). Ask Signals to send notices (R-20). Mirrors pull.

For a `space.move` (§7.3), step 3 also verifies `accept`: the target server's signature over the space and the current head's `receiptHash`.

**R-09 Closed shapes.** Unknown fields, wrong types, impossible dates or out-of-range values in a known kind's header, body or content are `invalid`.
**R-10 Profile freshness.** The profile seq a home records for an actor MUST be ≥ every profile seq it has recorded for that actor in this space before. It MUST be fetched from the profile's home, or from a mirror of the profile space, within 300 s of the receipt `time`. Mirrors check monotonicity exactly. Freshness is a stated trust assumption (§7.5).

### 4.2 M2 Policy: pure functions

**R-11 Determinism.** Policy's four functions depend only on their arguments. `ctx.time` is the receipt time. `ctx.profiles` are the profile views at the seqs pinned in the receipt. Every conforming implementation MUST give identical results; [ref/policy.py](ref/policy.py) is the reference and [ref/vectors/](ref/vectors/) the tests. Wall clocks, outside reputation and rate limits are *not* inputs. Rate limits and server block lists live in Log (step 3) and can only refuse.

Because Policy is pure, a mirror can replay every decision (R-14). Conformance is "records in, decisions out", and the model checker checks the same rules.

### 4.3 M3 Identity: identities are spaces too

- An actor ID is `@name:server`. Its **profile space** `profile:@name:server` is homed on `server` and is world-readable. Mirrors of any space MUST also mirror the profile spaces of its authors, and serve them to others. Profiles therefore outlive their home server, and any server can verify old entries.
- The profile space's `space.create` is appended by the server actor `@:server`. Its body sets `profile: {actorKind, operator?, key}`, the first key.
- `key.add` / `key.revoke` in a profile space may be appended by the actor (signed by any un-revoked key) or by `@:server` (account recovery). **KeyIds are single-use:** `key.add` of a keyId the profile has ever held is `invalid`, so a revoked key can never become valid again. A stolen key can revoke the others; the cure is server-assisted recovery. The server can also add keys, and therefore impersonate its users. Both are stated trade-offs (§7.5).
- **Agents and services** name an `operator`. The operator must be a different, **human** actor, whose profile is pinned when they confirm. They confirm by appending `member.set {actor: <operator>, role: "owner"}` to the agent's profile space, and withdraw with `role: "none"`; the server may also withdraw a confirmation. An actor naming itself as operator is `invalid`. In a space with `allowAgents: "with-operator"`, the confirmation is checked whenever the agent joins, proposes, claims or draws budget (P-03).
- A server's keys are published at `https://<server>/.well-known/han` as `{keys: [{keyId, publicKey, from, until?}]}` and fetched over TLS. A receipt is valid only if its `time` lies in its key's `[from, until)`. Retired keys stay listed. Every server MUST keep the key sets it has used for verification and serve them at `https://<itself>/.well-known/han-peers` as `{server: keys}`, so receipts stay verifiable after their signer is gone. It treats a changed key for an already-seen window as a fault.

`ProfileView = {actor, seq, actorKind, operator?, operatorConfirmed, keys: {keyId: {publicKey, status: "valid" | "revoked"}}}` is the fold of the profile space through `seq`.

- **Server actor `@:S`:** its view is synthetic: `actorKind: "service"`, `seq: 0`, and keys from `S`'s key set whose window contains the receipt time, all `valid`.
- **Entries inside a profile space:** the author's view is that profile space's own state as of the entry's position.

**R-12 Identity is keys.** An entry's author is the actor whose key `keyId` signed it. The key's status in `profiles[author]` must be `valid`; otherwise the entry is `denied` by Policy. A bad signature is `invalid`. The connection, IP and relaying server never determine authorship. Revocation is not retroactive, except that an effect claim re-checks the keys behind its proposal and approval (P-07 E2, E3).

### 4.4 M4 Blobs

`BlobRef = {hash, size, type}`. Upload with `blobPut` pieces at any offset. The blob completes when every byte is present and `sha256` matches.

**R-13 Access follows references; supply follows authors.** A record *references* a blob if its unredacted content holds the `BlobRef` at:
- `content.blobs[]`, for **every** kind, extensions included; or
- `effect.propose.plan.argsBlob`; or
- `effect.outcome.evidence` or `effect.resolve.evidence`.

`blobGet(ref, space)` succeeds only if both of these hold:
1. **Access:** a record in `space` that the caller may read (P-02) references `ref`.
2. **Supply:** the server holds the bytes *for that space*. That is true when the referencing entry's author uploaded them to this server, or when the server fetched them for that space under R-21. An upload counts only if **every byte came from that uploader's own `blobPut` calls** and those bytes hash-match. A server MUST NOT report `complete` to an uploader just because the hash is already stored. Storage may be deduplicated by hash, but access never is. Knowing a hash, and mentioning it in a space you control, gets you nothing. That's why a hash proves bytes are equal but never permission, authorship or truth.

Uploaders may read their own unreferenced blobs for 24 h. A server MUST keep a blob while any unredacted record it stores references it, and MAY delete it after.
**R-21 Blob transfer.** A client uploads to its own server (`blobPut` as itself; the server may apply quotas). A server that needs a blob it lacks calls `blobGet` as `@:server` in the context of the referencing space. It asks the author's server first, then the home, then any mirror. R-13 is evaluated for the server actor (P-02 server reads). The receiver verifies the hash, moves on to the next server on a mismatch, and from then on holds the bytes for that space.

### 4.5 M5 Federation

Mirroring is pull-only. `mirror(space)` follows the home (`read` with `follow:true`), verifies (R-14), stores, fetches and keeps the profiles, server keys and blobs it needs, and gossips heads (R-15). Appends from a mirror's users are `forward`ed to the home.

**R-14 Mirrors verify everything.** For each record, a mirror MUST check:
- the exact header and the content (R-09). For a `redacted` record, it checks the `redactedBy` proof instead: a validly signed `redact` entry targeting this record, with a later valid receipt in this space, which the mirror re-checks when it reaches that seq. It also checks the entry signature against the pinned profile view;
- the receipt: signed by the **home at that seq** (R-02), with a key valid at the receipt time;
- `prev` continuity, R-03a and R-10 monotonicity;
- for a `space.move`, the target's `accept` signature over the previous head;
- a replay of `Policy.decide`.

On failure it stops serving the space above the last good seq. It then signals `{type: "fault", proof: {kind: "equivocation" | "wrong-home" | "forbidden-accept" | "bad-signature" | "chain" | "bad-redaction" | "bad-move", receipts, record?}}` to the space's followers and keeps the proof.
**R-15 Head gossip.** In every `hello` between servers (both directions: `knownHeads` in args and reply), and in any `read` (`knownHeads` argument; `{type:"heads"}` item), each side lists the spaces it asks about. The other side answers only for spaces in that list that the asker may read: as `@:asker` for a server, under P-02. Heads therefore never reveal a private space to a server with no member in it. The receiver compares each with its stored receipt at that seq. An equivocation or wrong-home proof is handled as an R-14 failure. A mirror MUST exchange heads with at least one other mirror of each space it mirrors (any member's server) at least every 10 minutes. Clients MAY send `knownHeads` too.
**R-20 Notices (first contact).** After ordering a `member.set` that gives an actor on server `S` a role, or an entry whose `to` names an actor that holds a role, the home MUST send `signal(profile:<actor>, {type: "notice", space, seq})` to `S`. `S` delivers it **only to that actor's own authenticated sessions**, as signal items on the actor's follow of its own profile space, and never to the profile space's other readers. A notice is only a hint: `S` verifies it by reading the space as `@:S` (P-02 server reads), and drops notices it can't verify. This is how a new member, human or agent, learns that a space exists.

### 4.6 M6 Signals

**R-16 Signals carry no authority.** Signals are never stored, never replayed, and never read by Policy. Anything that matters MUST also be appended. A streamed agent reply ends with a `message` or `task.update`.
**R-22 Who may signal.** Only actors who may read a space at its head (P-02) may `signal` it or `follow` it for signals. **Across servers:** a user's server relays the user's signal to the home over its own `@:server` connection. The home accepts a relayed `from` only for actors on that server. The home sends signals to other servers' follow streams, and each server fans them out to its local followers. **Profile spaces** are readable by anyone, but only the actor itself may follow its profile for signals, and the only signals they accept are R-20 notices from server actors. Servers rate-limit signals per actor and drop them under load.

### 4.7 M7 Executor: the only thing that touches the world

An executor is an actor, usually of kind `service`, that owns part of the world: a checkout, a VM, a mailbox, an account. Each running process (an **instance**) holds one of the actor's keys. `effect.claim.body.instance` is that keyId. Workspaces and snapshots are executor-private and appear in HAN only as plans and blobs.

Let `W` be the launch window **pinned in the claim**, meaning the space's `launchWindow` when the claim was ordered (`SpaceState.effects[p].claims[i].window`), and `stallBound` = 10 s. The executor runs this protocol inside `deliver`:

| Step | Rule |
|---|---|
| **X-01** | Only the instance whose keyId equals `claim.instance` may act on that claim. Its launch journal is durable and private to that instance. |
| **X-02** | Before the first send of a claim, durably journal `claiming(claimId)` together with the signed claim **and the reading of its own elapsed-time clock at that moment** (R-07). That clock MUST keep counting through suspend, sleep and VM pause (e.g. `CLOCK_BOOTTIME`, not `CLOCK_MONOTONIC`). If the platform can't guarantee that, any suspend after `claiming` MUST be treated as expiry (X-04). |
| **X-03** | Act only after the claim's receipt is in hand. Read `W` from the claim as recorded in the log. |
| **X-04** | Durably journal `launching(claimId)`. Then, **as the last step before launching**, check that less than `W − stallBound` has passed on the instance's elapsed-time clock since the first send. If not, durably journal `expired(claimId)`, do not launch, and report `{result:"failed", started:"no"}`. The platform MUST ensure no more than `stallBound` passes between this check and the launch. |
| **X-05** | Report with `effect.outcome`. `started: "no"` only if `launching` was never journaled, or `expired` follows it. |
| **X-06** | **No launch after restart.** On restart, for each `claiming` without `launching`: finish the claim's append (R-06). If it has a receipt, report `{result:"failed", started:"no"}`. For each `expired`: report the same. For each `launching` with neither `expired` nor an outcome: look the result up in the outside system if possible, otherwise report `{result:"unknown", started:"unknown"}`. |
| **X-07** | An instance key MUST NOT be held by two running processes at once, and MUST NOT be restored on a new machine without its journal. Losing the journal means retiring the key (`key.revoke`), after which the owner abandons its open claims (P-09). |
| **X-09** | Before launching, the executor MUST check that the plan it will run hashes to the proposal's `planDigest`. A plan whose content was redacted can't be run (P-12 prevents this). |

**Why no launch follows an abandon.** Let `d` be the drift bound (1%). Each side measures only an interval on its own clock, so clock offsets cancel; only rates matter.
1. The executor launches at most `W` after the send on its own clock, which in true time is at most `W·(1+d)` if that clock runs slow.
2. The claim reaches the home no earlier than it was sent. The home's `receipt.time` is rounded down, so it is at most 1 s early on the home clock.
3. An owner may abandon only at home time ≥ `receipt.time + A`, with `A = ⌈W·(1+d)/(1−d)⌉ + 2` (P-09). On a home clock running fast, that is at least `(A − 1)·(1−d) > W·(1+d)` true seconds after the send.

So the abandon always comes after the latest possible launch, whichever way each clock drifts. The model checker exaggerates drift to ±20%, with the home fast and the executor slow, and adds offsets, arbitrary delivery delay, sub-second arrival with rounded receipts, stalls on the executor's clock, and the owner shortening `launchWindow` mid-claim. **Status:** the full drift-inclusive run had not finished at publication (see ref/RESULTS.md); the last completed full run, without rate drift, found no counterexample. The mutations "deadline from receipt time", "window not pinned", "executor uses current window", "no rounding margin" and "one-sided drift margin" (round 5's formula) each produce one.

The claim is the fence. Policy re-checks membership, keys, operators, approvals, cancellation, expiry and budget at the claim's seq (P-07). Anything ordered after the claim is treated like a stop request to already-running work: `effect.cancel` asks the executor to stop, and the outcome reports what really happened.

### 4.8 M8 Agent host

The standard does not define how an agent thinks. Agents are ordinary actors that read spaces and append entries. The host's interface to its agents is `deliver(record)`.

**R-17 Reconstructible inputs.** An agent's entries MUST set `seen` and `refs` so that what drove them is identifiable. Model-provider calls aren't HAN entries. An operator who wants model inputs auditable attaches them as a blob in the content of the `task.update`.
**R-18 Recovery is outbox + reading.** After a crash a host first finishes its outbox (R-07), then re-reads its spaces. Open tasks are those with no final `task.update` from it. Open effects are its proposals that aren't in a final phase. It MUST NOT re-propose an intent whose proposal is in its outbox or the log.

---

## 5. Entry kinds

Exact body and content schemas: [han.ts](han.ts) `Bodies` and `Contents`.

| Kind | Body (permanent; Policy reads it) | Content (redactable) |
|---|---|---|
| `space.create` | `policy`, `forkOf?`, `profile?` | `name` |
| `member.set` | `actor`, `role` | — |
| `policy.set` | `policy` | — |
| `key.add` / `key.revoke` | `keyId`, `publicKey` / `keyId` | — |
| `message` | `to?` (attention only) | `text`, `blobs?`, `links?` |
| `task` | `to`, `budget?`, `parent?`, `deadline?` | `ask`, `blobs?`, `links?` |
| `task.update` | `task`, `state` | `text?`, `blobs?` |
| `task.cancel` | `task` | — |
| `effect.propose` | `executor`, `action`, `planDigest`, `cost?`, `repeatable`, `task?`, `expiresAt?` | `plan` (`sha256(JCS(plan)) = planDigest`, `plan.action = action`) |
| `effect.approve` | `proposal`, `planDigest`, `expiresAt?` | `note?` |
| `effect.claim` | `proposal`, `attempt`, `instance` | — |
| `effect.cancel` | `proposal` | — |
| `effect.outcome` | `claim`, `result`, `started`, `cost?` | `evidence?` |
| `effect.resolve` | `proposal`, `result`, `cost?` | `evidence` (required) |
| `checkpoint` | `through` | `blobs` |
| `redact` | `target` | `reason?` |
| `space.move` | `to`, `accept` | — |

**Extensions** add kinds `<prefix>.<name>` (grammar in §2) whose prefix the core doesn't use. The core prefixes `space`, `member`, `policy`, `key`, `message`, `task`, `effect`, `checkpoint` and `redact` are reserved, and an unknown kind under them is `invalid`. To the core, an extension's body and content are opaque objects. Members holding at least `policy.extensionsMinRole` (default `member`) may append one, subject to size limits and the number rule (§2). If the content has a `blobs` field, it must be a list of BlobRefs, and those are references under R-13. Their meaning, such as `doc.version`, is defined by the extension's own document, which the core neither enforces nor depends on.

---

## 6. Policy (normative; reference: ref/policy.py)

### 6.1 State and context

`SpaceState` (han.ts) is the fold of all records. `ctx = {time, profiles}` (§4.1).

**P-01 Relevant actors.** `relevantActors(state, entry)` is the author, plus the target `actor` of a `member.set`, plus, for an `effect.claim`, the proposer and the author of every approval *candidate* (E3, without its time filter). The home MUST pin a profile for each of them, or answer `unavailable`.

### 6.2 Roles, joining, reading

Roles: `owner`, `member`, `guest`, `banned`, or none (absent).

**P-02 Read.** `canRead(state, reader, seq)` is true if:
- `policy.visibility = "public"`; or
- `reader` is owner/member/guest now; or
- `reader` held such a role at `seq`, meaning former members keep what they saw; or
- `reader` is a server actor `@:S`, and either `S` is the space's current home, or `S` is not in `policy.servers.deny` and some actor on `S` holds or held a readable role at `seq`.

Profile spaces are public. The `state` item of `read` needs read rights at the head.

**P-03 Membership** (`member.set {actor, role}` by `author`, with target's current role `cur`):
- An **owner** may set any role, except that a set leaving zero owners is denied.
- **Self-leave:** anyone except a banned actor may set themselves to none, unless they are the last owner.
- **Open join:** if `join = "open"`, an actor with `cur` = none may set themselves to `member`. Banned actors may not.
- **Invite:** if `membersMayInvite`, a member may set an actor with `cur` = none to `guest` or `member`.
- **Agents:** if the target's profile `actorKind ≠ "human"`, then `allowAgents = "none"` denies, and `"with-operator"` requires `operatorConfirmed`. Under `"with-operator"`, a non-human author's confirmation is also re-checked at every `effect.propose`, `effect.claim` and budgeted `task`.
- **Servers:** *granting* a readable role to an actor on a server in `policy.servers.deny` is denied. Banning or removing such an actor is always allowed.
- **Profile spaces:** owners are the actor and `@:server`. The only other allowed `member.set` is the operator's confirmation or withdrawal (§4.3).

**P-04 Who may append what** (beyond the kind-specific rules below). "Holding a role" means owner, member or guest *now*. Approvers must also hold one. Authors on a denied server may append nothing, except fact reports (`effect.outcome`, `effect.resolve`).

| Kind | Allowed authors |
|---|---|
| `message` | owner, member, guest |
| `task` | owner, member; or an assignee of `parent` who still holds a role |
| `task.update` | an assignee of `task` who still holds a role; or an owner, with a final state only (force-close) |
| `task.cancel` | the task's author, or an owner, holding a role |
| `effect.propose` | owner, member |
| `effect.approve` | an actor matching the action's approvers (P-08) |
| `effect.claim` | the named executor, still an owner/member (P-07) |
| `effect.outcome`, `effect.resolve` | the claim's instance or executor, **regardless of current role or server**: they report facts about work already authorized (P-09, P-10); owners as P-09/P-10 allow |
| `effect.cancel` | proposer, any approver of it, or an owner, holding a role |
| extension kinds | roles at or above `policy.extensionsMinRole` |
| `checkpoint` | owner, member |
| `redact` | the target's author, or an owner |
| `policy.set`, `space.move` | owner |
| `key.add`, `key.revoke` | profile spaces only (§4.3) |
| `space.create` | only at seq 1 |

### 6.3 Tasks and the budget

A space has at most **one budget**, `policy.budget = {unit, amount}`, or `null`. Amounts are integers in minor units. `budgetLeft` is the budget minus everything *committed* (reserved or spent). Amounts only move between the space, task budgets and claim reservations, or are spent at settlement. The fuzzer (`ref/fuzz_fold.py`) checks that task remainders never go negative, that no remainder exceeds its budget, and that the space remainder goes negative only through a cut.

**P-05 Tasks.**
- `to` is non-empty and every assignee currently holds a role.
- A `parent` must be an open task in this space, and the author must be one of its assignees. Top-level tasks have depth 0, and a child's depth is its parent's + 1, which must be ≤ `maxDelegationDepth` (so 0 forbids delegation).
- **Task budgets** exist only in budgeted spaces and use the space's unit. A task's budget is reserved from its parent if the parent has a budget, otherwise from the space, and must fit what is left there. A child of a budgeted parent MUST carry a budget.
- **Agents stay inside their allocation.** An actor of kind `agent` or `service` may create a budgeted task only as a child of an open budgeted task assigned to it.
- A final `task.update` (`done`, `failed`, `declined`, `cancelled`) closes the task and refunds its unspent remainder to the nearest *open* budgeted ancestor, or to the space if there is none. Every later refund that would go to a closed task follows the same rule. An **owner** may close any open task this way (force-close), for example when its assignees are gone.
- On a closed task, these are refused: `task.update`, `task.cancel`, a child `task`, and an `effect.propose` that names it.
- `task.cancel` sets `cancelRequested`. The task stays open until an assignee, or an owner, posts a final update.

**P-06 Costs.**
- **Budgeted spaces are closed.** If `policy.budget` is set, every `effect.propose` MUST carry a `cost` in its unit, and a claim of an effect without one is denied (E6). An outcome or resolve may report a cost only within its claim's reservation. With no reservation, it may report a cost only in an unbudgeted space, where costs are informational.
- **Only humans draw on the space budget.** In a budgeted space, an actor of kind `agent` or `service` MUST name, in every proposal, an open budgeted task assigned to it, and its costs are charged there. An agent with no such task can't spend at all, and closing its own task doesn't help: it only ends its allocation. Only a task's assignees, or an owner, may name a task.
- **Charging:** an effect's cost goes to its named task if that task has a budget, otherwise to the space. It is reserved at claim and settled at outcome or resolve (P-09, P-10). The difference is refunded by the P-05 refund rule.
- **Changing the budget** (`policy.set`): with the same unit, `budgetLeft` = new amount − committed, which can go negative after a cut. A negative remainder means nothing new can be reserved until refunds bring it back up. Changing the unit, or adding or removing the budget, is allowed only while nothing is reserved; the remainder then starts fresh.

### 6.4 The effect state machine

Phases: `awaiting-approval`, `ready`, `claimed`, `unknown`, and the final phases `succeeded`, `failed`, `cancelled`. `cancelRequested` is a flag. A final phase never changes.

**Transition table.** Any (phase × kind) combination not listed is `denied`.

| Phase | `effect.approve` | `effect.cancel` | `effect.claim` | `effect.outcome` | `effect.resolve` |
|---|---|---|---|---|---|
| awaiting-approval | → ready | → cancelled | — | — | — |
| ready | stays ready (adds an approval) | → cancelled | → claimed (P-07) | — | — |
| claimed | stays claimed | sets `cancelRequested` (denied if already set) | — | per P-09 | — |
| unknown | — | sets `cancelRequested` (denied if already set) | — | — | per P-10 |
| final | — | — | — | — | — |

`effect.propose` → `awaiting-approval` if its action needs approval (P-08), else `ready`. A named `task` must be open in this space.

**P-07 Claim** (`effect.claim {proposal, attempt, instance}`). Allowed only when all of these hold:
- **E1** The author is the proposal's `executor`, still holds owner or member, is accountable (P-03 operator re-check), and `instance` is the valid keyId that signed this claim.
- **E2** The proposer still holds owner/member, and the key that signed the proposal is still valid in the proposer's pinned profile.
- **E3** If approval is needed: walk the **candidates**, meaning unconsumed, undropped approvals in order whose `expiresAt` (if any) is after `ctx.time`, whose author satisfies the approvers rule *now*, and (with `separateApprover`) who aren't the proposer. The first candidate whose signing key is valid is consumed. Candidates with revoked keys are skipped. If none is valid, deny.
- **E4** `cancelRequested` is false. (The table already keeps a cancel-requested effect out of `ready`, so E4 is a redundant guard, kept so a single rule states it.)
- **E5** The phase is `ready`; `attempt` = 1 + the number of earlier claims of this proposal; and the proposal's `expiresAt`, if any, is after `ctx.time`.
- **E6** In a budgeted space: the effect has a cost in the space's unit, and it fits what is left where P-06 charges it. It is then reserved.

On success, the claim records its receipt time and **pins `window` = the current `policy.launchWindow`**.

**P-08 Approvals.** `policy.approvals` is a list of `{action, approvers}`. The first entry whose `action` equals the proposal's action, or is `"*"`, applies, and `approvers` is a non-empty list of roles or actor IDs. If none applies, no approval is needed. With `separateApprover: true`, the proposer can't approve their own proposal. An approval for a proposal not in this space, or with a wrong `planDigest`, is `invalid`. After a `policy.set`, an effect in `awaiting-approval` whose action no longer needs approval becomes `ready`.

**P-09 Outcome** (`effect.outcome {claim, result, started, cost?}` for the latest claim, at most once per claim):
- **Author:** normally the claim's instance, i.e. the same actor with `keyId = claim.instance`.
- **Abandon:** an owner may append it with `result: "unknown"` and `started: "unknown"` only if `ctx.time ≥ claim.time + A`, where `A = ⌈claim.window × 1.01 / 0.99⌉ + 2`. A covers whole-second receipts and both clocks drifting by 1% in opposite directions (§4.7). For example, a 30 s window gives 33 s, and 3600 s gives 3675 s. This lets a space recover from a dead executor. X-04 guarantees the executor will no longer launch it (§4.7; model property NoLaunchAfterAbandon).
- **Effects of `started: "no"`** (result `failed` or `cancelled`): the consumed approval and the cost reservation are returned. The phase becomes `cancelled` if `cancelRequested`, else `ready` for a fresh attempt.
- **Effects of `started: "yes"`:** cost settles at `cost`, which must be ≤ the reservation (default: the reservation), and the rest is refunded. The phase becomes `succeeded` or `failed`. `result: "cancelled"` with `started: "yes"` means stopped partway and gives the phase `failed`. If `repeatable`, the result is `failed` and no cancel is requested, the phase becomes `ready` instead.
- **Effects of `unknown`:** the reservation stays charged and the phase becomes `unknown`. If `repeatable` and no cancel is requested, the reservation is instead settled as spent in full and the phase becomes `ready`. A proposer who marks an effect `repeatable` is declaring that running it again is harmless, so no resolve is needed.

Valid `(result, started)` pairs:

| result \ started | `yes` | `no` | `unknown` |
|---|---|---|---|
| `succeeded` | ✓ | — | — |
| `failed` | ✓ (ran and failed) | ✓ (never launched) | — |
| `cancelled` | ✓ (stopped partway → phase `failed`) | ✓ (never launched) | — |
| `unknown` | ✓ (launched, result lost) | — | ✓ |

All other pairs are `invalid`. `cost` is allowed only with `started: "yes"`.

**P-10 Resolve** (`effect.resolve {proposal, result, cost?}` with evidence, in phase `unknown`):
- **Author:** the proposal's executor (any instance), or an owner.
- `succeeded` → phase `succeeded`, and cost settles as in P-09.
- `failed` → the reservation is refunded. The phase becomes `cancelled` if `cancelRequested`; otherwise `awaiting-approval` or `ready` as for a fresh proposal, with no approvals carried.
- Resolving is a human or executor statement of fact, and it is the *only* way a non-repeatable effect with an uncertain past can run again. An `unknown` phase is reached only by the executor's own report or by an abandon after the claim's window, so once there, the claim can no longer launch. A resolve that is true when made therefore never leads to a second run (model property UnknownNeverSilentlyRetried, which counts only resolves that were already false when made).

### 6.5 Redaction, checkpoints, forks

**P-11 Kind-specific checks.**
- `checkpoint.through` ≤ the current head.
- `space.move` is denied while any effect is `claimed`, because the abandon clock (P-09) must be read on one home's clock.
- `policy.set` replaces the whole policy, applying the P-06 budget rules and the P-08 release rule. It can't change the `window` pinned in existing claims. Roles are untouched, because roles live only in `member.set`.
- `space.create` is only valid at seq 1.

**P-12 Redact** (`redact {target}`): the target's content is deleted wherever honoured, its `redacted` flag set, and its blob references released (R-13). A `redact` can't target another `redact`. The plan of an unfinished effect can't be redacted (cancel it first), so an approved plan is never left unexecutable. Policy state is otherwise unchanged, because Policy never reads content.

**P-13 Forks carry no authority.** A space with `forkOf {space, through}` starts from its own `space.create` alone: creator as owner, the new policy, no members, tasks, approvals, budget commitments or effects carried. The parent's records up to `through` are history to read, never authority. Credentials are executor-private and never in entries, so none carry over. Restoring a workspace in a fork is an ordinary effect, which the executor runs with the fork's own authority.

---

## 7. Federation and trust

### 7.1 Anyone can run a server; nobody is trusted automatically

Any server may `hello` any other. Whether a server takes part in a space is decided by that space's policy (P-02, P-03 `servers`) and by the server's own block list. A space's policy decides which actors may join. An `open` + `public` space is a public room. An `invite` space is a private group.

### 7.2 Consistency

- **Within a space:** one total order (seq), verifiable by anyone who holds the records (R-14).
- **Across spaces:** no order. Links are content only (R-05).
- **Availability (a stated trade-off):** while a space's home is unreachable, nobody can append to it. Members can still read mirrors, send signals in other spaces, and work elsewhere.

### 7.3 Cooperative move

1. B first signs its acceptance of the current head: `accept = Ed25519(B's key, "han-accept-move-v1\n" ‖ space ‖ "\n" ‖ receiptHash(head))`. An owner appends `space.move {to: B, accept}`. The home verifies `accept` (§4.1), and mirrors verify it too (R-14). So a space can't be moved to a server that hasn't agreed to host it, and a move made against a stale head is invalid. The move is denied while an effect is `claimed` (P-11). The old home A orders it at seq `n`, and that is A's last record until a later move names A again. From then on A answers appends with `moved` and serves reads through `n`.
2. B follows A (or any mirror) through `n` and verifies it (R-14).
3. B orders the next entry at `n+1`, with `prev = receiptHash(n)`, signed by B's key, and with a time ≥ A's last (if B's clock is behind, it waits, per R-03a).
4. Mirrors accept records above `n` only when they are signed by B (R-02: the home at each seq).

Moving back works the same way. A, having verified B's chain through B's `space.move {to: A}`, continues it.

### 7.4 When a home dies

HAN v1 has **no forced takeover of a space**. Three rounds of review showed that any rule letting someone other than the home keep writing a space reopens double-execution and rollback attacks. Instead:
- The space's history stays **readable** on every mirror, with its profiles and server keys (§4.3), and stays verifiable forever.
- Owners continue in a **fork**: `space.create {forkOf: {space, through}}` on any server willing to host it, re-inviting members. The fork carries **no authority** (P-13): no approvals, budgets, claims or open effects.
- Effects that were open in the dead space stay as the log shows them. Anything not final is, honestly, of unknown fate. Re-doing one in the fork is a new proposal that a human decides to make, knowing it might repeat the original. The fork's first message SHOULD link the dead space's unfinished effects.
- Forced recovery with continuity of authority is left to an extension (§8), which must show the model properties hold.

### 7.5 Trust assumptions, and what each misbehaviour is caught by

| Misbehaviour | Caught by |
|---|---|
| Home accepts a forbidden entry | mirror replay (R-14) |
| Home shows different histories to different servers | head gossip (R-15) → equivocation proof (R-02) |
| A server signs a seq when it isn't the home there (re-signing another's history, or continuing after a move) | R-14 → wrong-home proof (R-02) |
| Home uses stale profiles (ignores a recent key revocation) | partly: monotonicity is checked (R-10); 300 s freshness is trusted |
| Home lies about time | partly: non-decreasing time is checked (R-03a); clock *rate* within 1% is trusted, offset is irrelevant (X-04) |
| A server adds a key to one of its own users' profiles (impersonation) | not prevented: users trust their own server for account recovery. The added key is public in the profile space, so the user can see it |
| A server rewrites `.well-known` with a back-dated key | partly: servers keep and republish the key sets they've seen (§4.3), and treat a changed key for a known window as a fault |
| Someone tries to read a blob by its hash | R-13: access needs a reference *and* supply in that space; an upload counts only on the uploader's own bytes |
| Home serves a record as redacted without a `redact` entry | R-14: a redacted record must carry its signed proof |
| Owner moves a space to a server that never agreed | §7.3: the move needs the target's signed acceptance |
| Home refuses or delays appends (censorship), or dies | not prevented; remedy is a move, or a fork (§7.4) |
| Home reads plaintext | not prevented in v1 (goal.md); `e2e` extension |

HAN v1 runs no consensus.

---

## 8. Extensions (outside the core)

| Extension | Shape | Why outside |
|---|---|---|
| `doc` | `doc.version` entries (`content.blobs` + `base`) for conflict-visible documents; real-time CRDT ops as signals, persisted by periodic `doc.version` | engines differ; the core shouldn't pick one |
| `e2e` | encrypted content plus key-distribution kinds | v1 is plaintext-to-home by decision |
| `views` | structured live views as signals | presentation, not truth |
| forced takeover | continuing a space with authority after its home dies | the core can't do it safely without consensus (§7.4) |
| account portability | moving an actor between servers | identity is server-anchored in v1 |
| reference stack | named products for a first implementation | goal principle 5 |

---

## 9. Depth ledger

| Module | Interface | v0.4 surface it replaces | Hidden concerns |
|---|---|---|---|
| Log | 2 calls | CommandJournal (6), EventJournal (3), SubmissionBindingStore (4), WorkflowLedger, event and subscription handlers, cursors | 13 |
| Policy | 4 pure functions | AuthorizationService (5), AuthorityService (4), approval, budget and grant records | 10 |
| Identity | 2 calls | IdentityDirectory (3), directory.resolve | 8 |
| Blobs | 2 calls | ArtifactStore (4), NativeArtifactIndex (2), artifact.begin/write/commit/read | 9 |
| Federation | 2 calls | TransportAdapter (2) and the federation v0.4 excluded | 9 |
| Signals | 2 calls | presence ×3, LiveViewAdapter and attachment (5) | 4 |
| Executor (actor) | 1 call | ExecutionAdapter (7), WorkspaceProvider (10), CheckpointCoordinator executor side, ExecutionPermit | 11 |
| Agent host (actor) | 1 call | AgentRuntimeAdapter (11) | 5 |
| **Total** | **16** | ~85 port methods; 26 wire methods; 19 commands | |

---

## 10. Verification: every rule and its check

Run `python ref/run_all.py`. Results: [ref/RESULTS.md](ref/RESULTS.md).

| Tool | What it does |
|---|---|
| `ref/policy.py` | executable reference of §6 and the R-09 body schemas |
| `ref/vectors/` + `run_vectors.py` | 63 vectors, 533 steps (including the §1.1 worked example); expectations hand-written from this document; every step also checks purity and determinism; an implementation that crashes fails |
| `ref/mutate_vectors.py` | 51 single-rule Policy mutations; the vectors must catch each one |
| `ref/fuzz_fold.py` | 2,000 random histories × 200 steps: `decide`/`fold` never fail on an allowed entry, stay pure, keep budgets within bounds, never change a final phase |
| `ref/wire.py` + `run_wire.py` | real Ed25519: reference home (§4.1 pipeline including content validation; reads, signals, notices, heads, per-space blob supply) and verifying mirror (R-14); 30 scenarios |
| `ref/mutate_wire.py` | 24 single-rule mutations of the wire layer; the scenarios must catch each one |
| `ref/model_check.py` | exhaustive bounded exploration driving the real `policy.py` in two scenarios. *Timing*: clock offsets **and rates** (drift exaggerated to ±20%), network delay, sub-second arrival with rounded receipts, stalls, launchWindow shortened mid-claim. *Single*: revocation, cancel, abandon, two attempts, crashes. 10 mutations must each give a counterexample; a non-vacuity guard fails runs that never launch |
| `ref/check_consistency.py` | §5 = han.ts = ref schemas; every folded state conforms to han.ts `SpaceState` |

| Rule | Checked by | |
|---|---|---|
| R-01 dedupe first | wire "dedupe precedes every check"; wire mutation; model (claim retries are `dup`) | ✓ |
| R-02 one chain, proofs | wire "valid chain", "reordering", "equivocation proof", "move then old-home receipt", "moving back to a former home", "first home is pinned"; wire mutations | ✓ |
| R-03 durable before receipt | — storage property; needs an implementation fault-injection test | ✗ |
| R-03a receipt time | wire "receipt time non-decreasing"; model timing scenario (offsets, rounding) | ✓ |
| R-04 immutability, redaction | wire "content binding and redaction" (including a faked redaction); vectors `redact`, `no-redacting-live-plans`; wire mutation | ✓ |
| R-05 same-space refs | vector `refs-and-seen`; mutation | ✓ |
| R-06 / R-07 retry and outbox | model: the executor's journal is its outbox, and claim resends are idempotent. The agent-host outbox is not modelled. | partial |
| R-08 no silent downgrade | wire "no silent downgrade", "minDurability can't lock a space"; wire mutations; version negotiation not exercised | ✓ |
| R-09 closed shapes | vectors `closed-bodies`, `extension-kinds`, `impossible-dates`; wire "header is exact", "content is validated"; consistency check; mutations | ✓ |
| R-10 profile pin monotonic | wire "profile pin monotonic" (freshness itself is trusted, §7.5); wire mutation | ✓ |
| R-11 determinism | every vector step and every fuzz step (purity + repeat); wire "forbidden accept caught by replay" | ✓ |
| R-12 identity is keys | vectors `revoked-author-key`, `E2-proposer-revoked`, `E3-approver-key-revoked`, `keyid-single-use` | ✓ |
| R-13 blob access and supply | wire "blob refs per kind and transfer", "blob theft by hash is refused", "uploads complete only on the uploader's own bytes"; wire mutations | ✓ |
| R-14 mirrors verify | wire "tampered body", "forbidden accept", "first home is pinned", "server key validity window", "content is validated", "new home continues the chain"; wire mutations | ✓ |
| R-15 head gossip | wire "head gossip finds a fork, privately"; wire mutations; the 10-minute schedule is not exercised | ✓ |
| R-16 signals carry no authority | by construction: Policy has no signal input (`PolicyCtx`) | ✓ |
| R-17 / R-18 agent inputs, recovery | — agent-host obligations; not exercised | ✗ |
| R-20 notices | wire "first contact across servers", "notices stay private"; wire mutation | ✓ |
| R-21 blob transfer | wire "blob refs per kind and transfer" (wrong bytes skipped, outsider refused) | ✓ |
| R-22 signal rights and relay | wire "signals ride follow streams" (including relay), "notices stay private"; wire mutations | ✓ |
| §7.3 moves | wire "moves need the target's acceptance", "moving back to a former home", "a new home with a slow clock waits"; wire mutations | ✓ |
| P-01 | vector `relevant-actors` | ✓ |
| P-02 | vectors `read-rights`, `read-public`; wire "state needs read rights at head"; mutation | ✓ |
| P-03 | vectors `membership-rules`, `open-join-and-ban`, `agents-*`, `server-deny`, `denied-server-users-can-be-removed`, `operator-withdrawal-stops-agent`; mutations | ✓ |
| P-04 | vectors `tasks-and-delegation`, `banned-loses-task-authority`, `owner-force-closes-task`, `denied-server-users-can-be-removed`; mutations | ✓ |
| P-05, P-06 | vectors `task-budgets`, `budgeted-child-of-unbudgeted-task`, `E6-*`, `budget-*`, `agent-must-charge-its-task`, `agent-cannot-self-fund`, `agent-cannot-close-its-way-out`; fuzz; mutations | ✓ |
| P-07 E1–E6 | vectors `E1`…`E6`, `banned-executor`, `proposal-expiry`, `operator-withdrawal-stops-agent`; mutations; model (E2) | ✓ |
| P-08 | vectors `E3-*`, `approval-rule-removed`; mutations | ✓ |
| P-09 | vectors `outcome-rules`, `result-started-table`, `repeatable-unknown-settles`, `launch-window-is-policy`, `window-pinned-in-claim`; model NoLaunchAfterAbandon and its five timing mutations; policy mutations for the margin formula | ✓ |
| P-10 | vectors `outcome-rules`, `resolve-cost-rules`; model UnknownNeverSilentlyRetried | ✓ |
| P-11 | vectors `checkpoint-move-policy`, `no-move-while-claimed`, `approval-rule-removed`, `budget-unit-locked-while-reserved`, `window-pinned-in-claim` | ✓ |
| P-12 | vectors `redact`, `no-redacting-live-plans`; mutations | ✓ |
| P-13 | vector `fork-fresh-authority`; mutation | ✓ |
| §4.3 identity rules | vectors `profile-space`, `keyid-single-use`, `operator-must-be-another-human`; mutations | ✓ |
| X-01 | vector `E1-executor-instance` | ✓ |
| X-02 … X-06 | model (timing, single): AtMostOneLaunchPerClaim, NoLaunchWithoutReceipt, NoLaunchAfterAbandon, Honest*; mutations | ✓ |
| X-09 | the plan-digest check it relies on: wire "content is validated" (home and mirror) | partial |
| X-07 | — operational rule; not exercised | ✗ |

**Not covered** (✗ above): durability under storage faults (R-03), the agent-host obligations (R-17, R-18) and key custody (X-07). Each needs a running implementation to test. They are listed here so nobody mistakes them for proven.

**Defects the checks found while the design was written:**
- **Round 2, model:** a deadline check made before the `launching` journal write.
- **Round 4, model:** a deadline taken from receipt time was unsafe under clock offset.
- **Round 4, fuzzer:** a budget cut lost track of committed money.
- **Round 5:** reviewers' attacks on window changes, whole-second rounding and blob hashes were each reproduced as a failing mutation before the fix.
- **Round 6:** a reviewer showed the abandon margin covered drift on one side only. The model now has clock rates, and the old formula is a mutation it catches.

---

## 11. Decisions and trade-offs

| Decision | Chosen | Rejected | Cost accepted |
|---|---|---|---|
| Ordering | one home per space; hash-chained receipts; cooperative moves | DAG merge, consensus, forced takeover | a space can't be written while its home is down; after a home dies, work continues in a fork without inherited authority |
| API | 6 generic calls + typed entries | one RPC per feature (v0.4) | strict schemas carry the detail (R-09) |
| Authorization | pure functions over the log + pinned profiles | live policy service | no outside data in decisions; profile staleness is trusted |
| Effects | claim in the same log; launch at once or never; window pinned in the claim; deadlines on the executor's own clock | attempt/lease registry; fence reads; clock comparisons | a crash between claim and launch costs one retry |
| Budget | one budget, one unit, per space | multi-currency budgets | a second currency needs a second space |
| Blobs | access needs a reference *and* per-space supply | global dedupe-by-hash | the same file uploaded to two spaces is fetched (not copied) per space |
| Redaction | permanent header, redactable content | full deletion | header fields (who, when, which action) are never erasable |
| Identity | `@name:server` + profile space; server-assisted recovery | bare keys, DIDs | an actor is tied to a server, which can recover (and so impersonate) it |
| Roles | four fixed roles + per-action approvals | v0.4 per-resource grants | narrower permission = a smaller space |
| Agents | ordinary actors with confirmed human operators | agent-specific wire | agent internals are invisible by design |
| Workspaces | executor-private | protocol objects | portability only via snapshot blobs |

## 12. Open questions

- Q1: Spam in public `open` spaces beyond rate limits and bans: server policy in v1.
- Q2: Account portability between servers: extension.
- Q3: Forced takeover after home death: an extension needs either consensus among mirrors or an explicit owner-held key that pre-authorizes a successor home.

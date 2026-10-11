# HAN v1 — architecture

Status: design draft, round 20. Companions: [goal.md](goal.md) (why), [han.ts](han.ts) (exact types; normative for shapes), [coverage.md](coverage.md) (fate of every v0.4 guarantee), [ref/](ref/) (executable reference, conformance vectors, fuzzer, model checker; run `python ref/run_all.py`).
Normative words MUST / MUST NOT / SHOULD / MAY follow BCP 14. Rules are `R-xx` (sequencer and verification), `P-xx` (Policy), `X-xx` (executor), `T-xx` (what HAN requires of a transport). §10 lists every rule with its check, or says plainly that none exists. Retired rule numbers are not reused.

---

## 1. The whole picture

```
   people and agents talk on a transport (Matrix first): rooms, messages, presence, files, invites
  ┌──────────────────────────────────────────────────────────────────────────────────────────┐
  │ room !abc:matrix.example        human app   agent host   executor   (all are "actors")  │
  │   chat ··· typing ··· files        │            │           │                            │
  │                                    └── HAN entries (signed) ┘                            │
  └──────────────────────────────────────────┬───────────────────────────────────────────────┘
                                             ▼
                    ┌──────────────────────────────────────────┐
                    │  HAN sequencer for this room             │  orders entries, signs receipts,
                    │  Log · Policy · Identity                 │  runs Policy (pure functions)
                    └──────────────────────────────────────────┘
                                             │ receipts (hash-chained)
                                             ▼
                    verifiers: any member's client or server replays Policy and checks every receipt
```

A **transport** carries the conversation: rooms, people, bots, messages, presence, invites and files. HAN doesn't replace it. HAN adds a **sequencer** per room (a HAN *space*). The sequencer orders every authority-bearing fact into a signed, hash-chained log, and runs **Policy**, a set of pure functions, so any participant can re-check every decision.

HAN logs only what agent safety needs:

| What | Entry kinds |
|---|---|
| **Ask for work** | `task`, `task.update`, `task.cancel` |
| **Touch the world** | `effect.propose`, `effect.approve`, `effect.claim`, `effect.outcome`, `effect.resolve`, `effect.cancel` |
| **Govern** (who may do what) | `space.create`, `member.set`, `policy.set`, `key.add`, `key.revoke`, `redact`, `checkpoint`, `space.move` |
| **Work live** (who may watch, read or steer whose session) | `session.open`, `session.input`, `session.close` |
| **Annotate the record** | `message`: a remark that belongs *in* the authoritative log, e.g. a reason for an approval. Chat stays on the transport. |

**21 kinds, three sequencer calls, one log per space.** A live session's *stream* (output, context, status) is not in the log: it flows on the transport (T-07). The log holds only its authority: that it exists, who may watch, read and steer it, and every steer. Everything else is the transport's job (§8).

### 1.1 A worked example: a human hires an agent

Ada (`@ada:a.example`) wants a code fix from an agent, `@fixer:b.example`, operated by Ana (`@ana:b.example`). The agent's changes are pushed by an executor, `@git:c.example`, operated by `@ops:c.example`. They share a Matrix room bound to HAN space `r1:a.example`, sequenced by a.example's HAN sequencer. Amounts are in `USD-cent`. This example is also the vector `ref/vectors/*-worked-example-1.1.json`.

| # | Who | Entry | What happens |
|---|---|---|---|
| 0 | b.example, c.example | `profile:@fixer:b.example` holds `space.create {profile: {actorKind: "agent", operator: @ana}}`, then Ana's `member.set {actor: @ana, role: owner}`. `profile:@git:c.example` is set up the same way. | both non-human actors exist, and their human operators have confirmed them (§4.3) |
| 1 | Ada | `space.create {policy: {join: "invite", allowAgents: "with-operator", approvals: [{action: "git.push", approvers: ["owner"]}], budget: {unit: "USD-cent", amount: 500}, launchWindow: 30, …}}` | the space exists; Ada is owner. On Matrix, the room carries a state event naming the space and its sequencer (§8.2). |
| 2 | Ada | `member.set {actor: @fixer:b.example, role: member}`, and the same for `@git:c.example` | P-03 checks the confirmed operators. Ada also invites them to the Matrix room (T-04). |
| 3 | Ada | `task {to: [@fixer], budget: {USD-cent, 300}}` + content `{ask: "fix the login bug"}` | 300 of the 500 is reserved for this task (P-05) |
| 4 | fixer | `task.update {state: accepted}`; `effect.propose {executor: @git, action: "git.push", cost: {USD-cent, 50}, task, planDigest}` + content `{plan}` | the proposal waits for approval (P-08) |
| 5 | Ada | `effect.approve {proposal, planDigest}` | phase `ready` |
| 6 | git executor | journal `claiming`; send `effect.claim {proposal, attempt: 1, instance: k1, window: 30}` | at the claim's seq, Policy re-checks the executor's and the agent's membership, operators and keys, the approval and the budget (E1–E6). It reserves 50 from the task and pins the 30 s window. |
| 7 | git executor | journal `launching`; check the plan against its digest; deadline check on its own clock; push | X-03/X-04/X-09 |
| 8 | git executor | `effect.outcome {result: succeeded, started: yes, cost: {USD-cent, 0}}` + evidence blob | the 50 goes back to the task; phase `succeeded` |
| 9 | fixer | `task.update {state: done}` + content `{text: "pushed abc123"}` | the task closes; its unspent 300 returns to the space budget |

If the executor crashes between steps 6 and 8, X-06 reports `failed`/`not started` or `unknown`; it never launches again on its own. If Ada bans the agent, or Ana withdraws her confirmation, before step 6, the claim is denied (E2). If Ada shortens `launchWindow` after step 6, the claim keeps its pinned 30 s (P-09). If a.example's sequencer dies, every verifier still holds the verified log, and Ada continues in a fork (§7.4).

---

## 2. The primitive: entry, content, receipt

Exact shapes: [han.ts](han.ts) `Entry`, `Receipt`, `LogRecord`.

An entry has two parts:
- the **header**, which holds every field Policy needs (`body`) plus routing and signing fields. It is permanent;
- the **content**, which holds human-meaningful payload (text, plans, results, blob lists). It is redactable.

```
Entry       = { v:1, space, kind, author, keyId, body, contentHash, refs, seen, nonce, time, sig }   -- exactly these fields
contentHash = sha256( JCS(content) )             -- "" when no content is sent
id          = sha256( "han-entry-v1\n" ‖ JCS(entry minus sig) )
sig         = Ed25519( key(author,keyId), "han-sig-v1\n" ‖ id )
Receipt     = { space, seq, id, prev, home, homeKey, time, durability, profiles, homeSig }
receiptHash = sha256( "han-receipt-v1\n" ‖ JCS(receipt minus homeSig) )
homeSig     = Ed25519( key(home,homeKey), "han-home-sig-v1\n" ‖ receiptHash )
prev        = receiptHash of seq−1 ("" for seq 1)
LogRecord   = { entry, content, redacted, redactedBy?, receipt }   -- redacted: true ⇒ content is null, redactedBy is the proof
```

Here **home** means the space's current sequencer server. Every `‖` concatenates UTF-8 bytes. `id` and `receiptHash` enter signatures as their 64-character lowercase hex strings.

**Encoding.** Frames are I-JSON (RFC 7493): duplicate object keys, invalid Unicode (including lone surrogates) and non-integer number forms (`1.0`, `1e3`, `-0`) are `invalid`. JCS is RFC 8785, including its key ordering by UTF-16 code units (vector `00-jcs-utf16-order`). Every number, including inside opaque Json such as `plan.args` and extension bodies, is an integer in [0, 2^53). The one exception is the state's `budgetLeft.amount`, which may be negative (P-06). Public keys are raw 32-byte Ed25519 keys and signatures are 64 bytes; both are base64url without padding. Reference: [ref/wire.py](ref/wire.py).

**Grammar and limits** (also in han.ts). Every pattern below is ASCII and must match the **whole** string (no trailing newline, no non-ASCII digits or letters). Exceeding any size limit is `invalid`. The header limit binds first, so the list caps below are upper bounds, not guarantees.

| Item | Rule |
|---|---|
| ActorId | `@` + name `[a-z0-9._-]{0,64}` + `:` + ServerName; the empty name is the server actor. On Matrix, a HAN actor *is* the Matrix user of the same ID. |
| ServerName | lowercase DNS name: labels `[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?`, dot-separated |
| SpaceId | `[a-z0-9]{1,26}` + `:` + ServerName (the space's **first sequencer**; the name `profile` is reserved), or `profile:` + ActorId |
| EntryId, Hex | `[0-9a-f]{64}` |
| nonce | `[a-z2-7]{26}` (128 bits, base32) |
| KeyId | `[A-Za-z0-9_-]{1,64}`; single-use per actor (§4.3) |
| Time | `YYYY-MM-DDTHH:MM:SSZ`: UTC, a real proleptic Gregorian date, seconds 00–59, no fraction. A sequencer writes `receipt.time` by **rounding its clock down** to the second. Times compare as instants, and a time is **expired** when it is at or before the time it is compared with (`expiresAt` ≤ `ctx.time`). |
| action | `[a-z0-9][a-z0-9._-]{0,127}`; `*` only in approval rules |
| Budget | `unit` `[A-Za-z0-9_-]{1,32}`, `amount` integer |
| BlobRef | `hash` Hex; `size` ≤ 4 GiB; `type` a media type, each half `[a-z0-9][a-z0-9!#$&^_.+-]{0,63}`; optional `uri` (transport locator, ≤ 1,024 bytes) |
| content strings (UTF-8 bytes) | `name` ≤ 256 · `text`, `ask` ≤ 32,768 · `note` ≤ 4,096 · `reason` ≤ 1,024 · `plan.target` ≤ 1,024 |
| sizes | header JCS ≤ 16 KiB; content JCS ≤ 64 KiB; frame ≤ 1.25 MiB |
| lists | `to`, `refs`, `blobs`, `links` ≤ 64 each; approval rules ≤ 64; `servers.deny` ≤ 256; unexpired, unconsumed approvals per proposal ≤ 16, plus the guards' (P-15); one open approval per author |
| extension kind | `<prefix>.<name>`: prefix `[a-z][a-z0-9-]{0,31}` and not a core prefix; name `[a-z0-9][a-z0-9._-]{0,63}` |
| maxDelegationDepth | 0–16 |
| launchWindow | 15–3600 seconds |
| publicKey | base64url, 1–128 chars |

**R-01 Idempotent append.** If an entry's `id` is already in the space, `append` MUST return the stored receipt *before any other check*, including signature, key validity, schema, policy and limits, and change nothing. The id stays in the log forever, so this holds for the life of the space.
**R-02 One chain.** A space's receipts form one chain (`seq` 1, 2, … with `prev` linking). The **home at seq k** is the sequencer named in the space id, changed by every `space.move` ordered before `k`. Two kinds of portable misbehaviour proof exist:
- *equivocation*: two valid receipts with the same `(space, seq)` but different `receiptHash`, signed by the same server;
- *wrong home*: a valid receipt at seq `k` signed by a server that wasn't the home at `k`.

A space may move back to a former home. Any verifier knows the current home from the chain.
**R-03 Durable before receipt.** A sequencer MUST NOT return a receipt before the record is durable at the stated `durability`: `"local"` survives process crash, `"replicated"` survives the loss of any one machine. It MUST NOT state a level it has not achieved.
**R-03a Receipt time.** Within a space, `receipt.time` MUST NOT decrease, including across a move. A sequencer MUST derive receipt times from a clock that doesn't jump: a steady, slewed clock anchored at startup, never a wall clock that can be stepped. If its clock is behind the last receipt time (for example after a move), it MUST answer `unavailable` until it catches up. Every party's clock MUST keep its rate within 1% of true time. Verifiers check non-decrease, and flag a receipt time more than `maxClockSkewSeconds` (60, han.ts `Limits`) ahead of their own clock as a fault (`future-time`). That one tolerance applies everywhere a party compares a time with its own clock.
**R-04 Immutability and redaction.** Headers never change and are kept for the life of the space (no compaction, so every `after` stays readable and every verifier can replay from seq 1). `redact` (P-12) deletes the *content* wherever honoured and sets the record's `redacted` flag. A redacted record MUST carry `redactedBy`: the signed `redact` entry and its receipt, which verifiers check (R-14), so a sequencer can't silently erase content.
**R-05 Same-space references.** `refs` and every `EntryId` inside a `body` MUST name an entry already in the same space; otherwise the entry is `invalid`. Cross-space links are content only (`Link` in han.ts) and carry no authority.

---

## 3. The sequencer interface: three calls

Every HAN binding offers these three operations. A **generic binding** (§8.1) carries them over one WebSocket at `wss://<server>/han`. The **Matrix binding** (§8.2) carries them as room events.

| Call | Args → result | Retry |
|---|---|---|
| `hello` | `{who, keyId, proof, version, knownHeads?}` → `{limits, durability, time, knownHeads}`. `proof = Ed25519(key, "han-hello-v1\n" ‖ challenge ‖ "\n" ‖ dialed)`, where `dialed` is the server name the client connected to; the client aborts if the server's challenge names another. `knownHeads` (≤ 256) are answered only for spaces the asker named and may read (R-15). | new connection |
| `append` | `{entry, content?}` → `Receipt` | **always safe with the same entry** (R-01) |
| `read` | `{space, after, follow?, state?, at?, limit?, knownHeads?}` → stream of `ReadItem`. First `{type:"state"}` if asked (it needs read rights at the head and omits the `entries` index); then `{type:"heads"}` if `knownHeads` was sent; then `{type:"record"}` in seq order, only for seqs the reader may read (P-02). `at` (a seq, only with `state: true`) makes the state item the `StateView` after record `at` (its `head` is `at`) instead of at the head; it needs read rights at `at` (P-02, judged on the current state), not at the head, so a former member can read the state as it was while they were in, and a member who joined later can read an earlier state. `at` without `state`, or `at` outside 1…head, is `invalid`. `limit` (1–1000, default 1000) bounds the backlog. Without `follow` the stream then ends; with `follow` it continues with new records. If the backlog exceeds `limit`, a `follow` sends `limit` records, then the live records from the head at registration on; the skipped records are a visible gap in `seq`, and the client MUST re-read the gap (`read` from its last contiguous seq). A reader who may read no seq gets `not_found`, the same as for a missing space. | safe |

**Errors:** `denied` (policy or authentication; never retry the same entry) · `invalid` (malformed, bad signature, bad schema, over a size limit; never retry) · `not_found` (never) · `limit` (rate limit; retry after `retryAfterMs`) · `unavailable` (same request later, after `retryAfterMs` when given) · `moved` (same request at `movedTo`).

**R-06 Unknown outcome.** If `append` fails with `unavailable` or the connection drops, the outcome is unknown. The sender MUST retry the identical entry until it gets a receipt or a definitive error (`denied`/`invalid`), and MUST NOT express the same intent with a fresh nonce meanwhile.
**R-07 Outbox.** Every actor MUST durably store each signed entry (and content) *before its first send*. After a restart it MUST finish sending every stored, unconfirmed entry under R-06 before appending anything new to that space.
**R-08 No silent downgrade.** An unsupported version MUST fail with `invalid`. A sequencer that can't currently reach the space's `minDurability` MUST fail appends with `unavailable`, except `policy.set` and `space.move`, which are always ordered so a space can't lock itself. A sequencer MUST refuse (`unavailable`) a `space.create` or `policy.set` asking for a `minDurability` it can never provide.

---

## 4. Modules

The interface column lists the *whole* interface. The "hides" column is why the module exists.

| # | Module | Interface | Hides |
|---|---|---|---|
| M1 | **Log** (the sequencer) | `append(entry, content?) → Receipt`<br>`read(space, after, opts, as) → stream` | ordering, the hash chain, receipt signing and server keys, durability, dedupe, header and content validation, abuse limits, steady-clock time, profile pinning and freshness, redaction proofs, fan-out to the transport, moves |
| M2 | **Policy** | five pure functions: `decide(state, entry, ctx)`, `fold(state, record, ctx)`, `canRead(state, reader, seq)`, `relevantActors(state, entry, time)`, `sessionAccess(state, actor, session)` | roles, joining, invites, tasks, delegation, the budget ledger, the effect state machine, approvals, read rights, session rights and steering, forks (all of §6) |
| M3 | **Identity** | `profile(actor, minSeq?) → ProfileView`<br>`verify(entry, view) → ok \| invalid` | profile spaces, key history, rotation and recovery, actor kinds, operator confirmation, caching and freshness, server key discovery and key windows |
| M4 | **Verifier** | `verify(space) → VerifierStatus` | pulling records, checking every signature and receipt, Policy replay, profile and key retention, head gossip and misbehaviour proofs, move handover |
| M5 | **Executor** (an actor) | `deliver(item)` | deciding what to claim, the claim/launch/expired journal and X-01…X-11, elapsed-time deadlines, plan checking, sandboxes, credentials, workspaces, native lookups, cancellation of running work, its outbox |
| M6 | **Agent host** (an actor) | `deliver(item)` | models, prompts, memory, the agent framework, turning thoughts into entries, its outbox |
| M7 | **Transport binding** | `publish(space, record)`<br>`subscribe(space, as) → stream of ReadItem`<br>`signal(space, from, body)`<br>`blobPut(bytes, as) → BlobRef`<br>`blobGet(ref, space, as)`<br>`stream(item)`<br>`watch(space, session, as) → stream of StreamItem` | everything T-01…T-07 require: carrying entries and receipts, invites and notices, signals, files, identity mapping, live session streams with per-right delivery |

Policy is five functions, but its interface carries no state and has no side effects: everything it hides is *rules*. The executor and agent host are **actors**. Each is built with a `Client` (the three calls plus the transport) and fed every record and signal of the spaces it follows, as `ReadItem`s. It answers only through its client. That's the same interface a human app implements.

### 4.1 M1 Log: the sequencer pipeline

On `append(entry, content)` the sequencer runs these steps. Steps 2–9 run in one serial transaction per space.

1. **Dedupe:** if `id` exists, return its receipt (R-01).
2. **Not home?** Answer `moved` with the current home. A `space.create` is accepted only by the sequencer named in its space id, which decides by its own rules whether to host it.
3. **Shape:** the header has exactly the `Entry` fields, each in its grammar (§2); the body matches the kind's closed schema (R-09); content is present iff `contentHash ≠ ""`, matches it, and matches the kind's content schema and checks (§5: `planDigest = sha256(JCS(plan))`, `plan.action = action`); sizes are within limits. For a `space.move`, `accept` verifies (§7.3). Rate limits and block lists apply here. `seen` ≤ head and the existence of `refs` (R-05) are not checked here: they belong to Policy (P-16 step 5). R-25 says which of these checks `Policy.decide` repeats and which it never sees.
4. **Profiles:** pin `Identity.profile(actor)` for the author and the other actors `relevantActors` names (P-01, R-10). If a required profile is unavailable → `unavailable`.
5. **Signature:** `Identity.verify(entry, profiles[author])`.
6. **Policy:** `Policy.decide(state, entry, ctx)` with `ctx = {time: now, profiles}`. `now` is the steady clock rounded down (R-03a).
7. Assign `seq`, `prev`, `time`, `durability`, and `profiles: {actor: profileSeq}`. Sign.
8. Make durable at the stated level. Then `Policy.fold`, which never fails for an allowed entry (`ref/fuzz_fold.py`).
9. Return the receipt, and publish the record on the transport (T-01).

**R-09 Closed shapes.** Unknown fields, wrong types, impossible dates or out-of-range values in a known kind's header, body or content are `invalid`.
**R-10 Profile freshness.** The profile seq a sequencer records for an actor MUST be ≥ every profile seq it has recorded for that actor in this space before. It MUST be fetched within 300 s of the receipt `time`, from the profile's sequencer or from a verifier of that profile space. Verifiers check monotonicity exactly. Freshness is a stated trust assumption (§7.5).
**R-16 Abandon needs elapsed time.** A sequencer MUST NOT order an owner's abandon (P-09) unless its own steady clock shows at least `A` seconds since it ordered the claim. This holds even if receipt times say otherwise, so a clock step can't shorten the wait. Moves are denied while a claim is open (P-11), so the same sequencer ordered both. A steady clock can't measure across a restart or a failover to another machine, so after either the sequencer MUST wait a full `A` seconds of its new steady clock before ordering an abandon of any claim ordered earlier. **Which code.** An owner's abandon that is refused only because it is early (by this clock, or by the receipt time it would get, P-09) is answered `unavailable` with `retryAfterMs` = the remaining wait, so the owner's R-06 retry delivers it when due. The test is: `Policy.decide` evaluated with `ctx.time` = claim time + `A` would allow the entry, which means the claim is the effect's latest, its phase is `claimed`, and the author and key are right. In any other case time is not the only problem, and the sequencer answers Policy's own verdict now (`denied` or `invalid`): an abandon of a settled or superseded claim, by a non-owner, with the wrong shape, or of an unknown claim is never `unavailable`, so a client doesn't retry it forever. Policy's own verdict on a record ordered too early is `denied` (P-09), which is what a verifier replaying it sees.

### 4.2 M2 Policy: pure functions

**R-11 Determinism.** Policy's five functions depend only on their arguments. `ctx.time` is the receipt time, and `ctx.profiles` are the profile views at the seqs pinned in the receipt. Every conforming implementation MUST give identical results; [ref/policy.py](ref/policy.py) is the reference and [ref/vectors/](ref/vectors/) the tests. Wall clocks, outside reputation and rate limits are *not* inputs. Rate limits and block lists live in Log (step 3) and can only refuse.

**R-25 Validation boundary.** Two layers judge an entry, and a verifier replaying the log (R-14) runs both.
- **Log** (steps 1–5) owns the wire form: the header has exactly the `Entry` fields, each in its grammar (§2), including `nonce`, `time`, `contentHash` and `sig`; `id` is derived as in §2 and drives dedupe (R-01); content is present iff `contentHash ≠ ""`, matches it, and fits the kind's content schema (§5); every size and list limit; the signature against the pinned key (a bad one is `invalid`, a revoked key is Policy's `denied`, R-12); for a `space.move`, the signature in `accept` (§7.3); rate limits and block lists.
- **Policy `decide`** is called with an entry that already passed those. It checks the fields it reads (`v`, `space`, `kind`, `author`, `keyId`, `refs`, `seen`) and the whole `body` against the kind's closed schema (R-09). That covers every ActorId, EntryId, KeyId, Time, Budget and `PolicyDoc` inside the body, and, for an extension kind, a body that is an object of Json holding only integers in [0, 2^53). Everything else it does is P-16.
- `decide` and `fold` **never read** `nonce`, the header `time`, `contentHash`, `sig`, `accept.sig` (the body check needs `accept` to be `{keyId, sig}` with a KeyId and any string), content, or an entry `id`. An implementation MUST NOT reject on them inside `decide`. Header fields outside `Entry` are the Log's to reject; `decide` MAY ignore them (`ref/policy.py` also rejects them, and accepts a vector-only `id`).
- The body shape is checked by both layers (Log step 3, P-16 step 1) and fails as `invalid` in both, so each layer reaches the same verdict alone. `seen ≤ head` and the existence of `refs` are Policy's alone (P-16 step 5): the Log does not check them, so their place in the order of checks is fixed by P-16 and nowhere else.
- `decide` doesn't need the entry's own id. `fold` takes it from `record.receipt.id` (equal to the §2 derivation for every record the Log accepted), and otherwise reads from the receipt only `seq` and `time`.
- **Vectors may stub what the Log owns**, because they are written by hand: ids are labels hashed to 64 hex digits (`00-entry-id` tests the real derivation separately), there is no `sig`, `nonce` is a placeholder, `accept.sig` is any string, `contentHash` is `""` without content, receipts carry only `space`, `seq`, `id` and `time`, and profile views hold dummy keys. A Policy that rejects these is checking the wrong layer. Vectors never stub what Policy owns: no vector holds a body that breaks R-09.

### 4.3 M3 Identity: identities are spaces too

- An actor ID is `@name:server`; on Matrix it is the Matrix user ID. HAN keys are separate from transport credentials. HAN never trusts a transport login as authorship (R-12).
- The actor's **profile space** `profile:@name:server` is sequenced by `server`'s HAN sequencer and is world-readable. Its `space.create` is appended by the server actor `@:server`, with `profile: {actorKind, operator?, key}` naming the first key. **Verifiers of any space MUST also verify, retain and serve the profile spaces of every actor pinned in its receipts**, so profiles outlive their server.
- `key.add` / `key.revoke` may be appended by the actor (signing with any valid key, R-12) or by `@:server` (account recovery); anyone else is `denied`. **KeyIds are single-use**, so a revoked key never becomes valid again: a `key.add` of a keyId already in the profile (even a revoked one), and a `key.revoke` of a key that is not currently valid (unknown or already revoked), are `invalid` (P-16). Revoking an actor's last valid key is allowed. A stolen key can revoke the others, and the server can add keys and so impersonate its users. Both are stated trade-offs (§7.5).
- **Agents and services** name an `operator`, who must be a different, **human** actor. The operator confirms with `member.set {actor: <operator>, role: "owner"}` in the agent's profile space, and withdraws with `role: "none"`; the server may also withdraw. An actor naming itself as operator is `invalid`. The operator's confirmation and withdrawal are the only `member.set`s a profile space accepts (P-03).
- A server's HAN keys are published at `https://<server>/.well-known/han` as `{keys: [{keyId, publicKey, from, until?, compromised?}]}`, fetched over TLS. A receipt is valid only if its `time` lies in its key's `[from, until)`. A key marked `compromised` is trusted only for receipts a verifier recorded before first seeing the mark. Every server MUST keep the key sets it has used for verification and serve them at `https://<itself>/.well-known/han-peers` as `{server: keys}`. A key that changes for an already-seen window is a fault. These two HTTPS documents are part of the HAN wire surface.

- **Adding a device (enrollment).** A new device generates its own key and shows its public key, for example as a QR code. An existing device of the same actor appends `key.add` for it. If no existing device is available, the server appends it (account recovery). Clients MUST then show a visible warning in every space where that actor is a member, because recovery is also how a server could impersonate the actor (§7.5).
- **Finding a profile.** `profile:@name:server` is read with `read` from the HAN sequencer at `server`, whose endpoint is listed in `https://<server>/.well-known/han` (`{sequencer: "wss://…/han", keys: […]}`), or from any verifier that retains it.

`ProfileView = {actor, seq, actorKind, operator?, operatorConfirmed, keys: {keyId: {publicKey, status}}}` is the fold of the profile space through `seq`. The server actor `@:S` has a synthetic view: `actorKind: "service"`, `seq: 0`, and `S`'s keys valid at the receipt time. Inside a profile space, the author's view is that space's own state at the entry's position.

**R-12 Identity is keys.** An entry's author is the actor whose key `keyId` signed it. The key's status in `profiles[author]` must be `valid`; otherwise the entry is `denied` by Policy. A bad signature is `invalid`. Neither the transport sender nor any relaying server determines authorship. Revocation is not retroactive, except that a claim re-checks the keys behind its proposal and approval (P-07 E2, E3).

### 4.4 M4 Verifier

A verifier is any party that checks a space: a member's client, a member's server, or an auditor. `verify(space)` reads the log (`read` with `follow`), checks it, retains it with the profiles and keys it needs, and gossips heads.

**R-14 Verifiers check everything.** For each record, a verifier MUST check:
- the exact header and the content (R-09). A `redacted` record is checked through its `redactedBy` proof instead: a validly signed `redact` entry targeting it, with a later receipt in this space, which the verifier re-checks when it reaches that seq;
- the entry signature against the pinned profile view;
- the receipt: signed by the **home at that seq** (R-02), with a key valid at the receipt time;
- `prev` continuity, R-03a and R-10 monotonicity;
- for a `space.move`, the target's `accept` signature;
- a replay of `Policy.decide`.

On failure it stops trusting the space above the last good seq, tells the room (T-05) with `{type: "fault", proof}`, and keeps the proof. A `proof` is a portable `FaultProof` (han.ts): `equivocation` and `future-time` need only the receipts (the latter with the verifier's `observedAt`); every other kind carries the verified `chain` before the fault, the `profiles` chains it pins, the offending `record` and the `serverKeys` used, so that anyone can replay the check from nothing against server keys they trust. A proof can't be stripped of content, because the content hash check needs it, so a proof of a private space is private. A verifier reports `ok`, `stopped` (it couldn't continue: connection lost, a seq gap because P-02 hid records from it, an unavailable profile chain; never a fault), `faulty` (with the proof) or `moved` (`VerifierStatus`).
**R-15 Head gossip.** In `hello` (both directions) and in `read` (`knownHeads` argument; `{type:"heads"}` item), each side lists the spaces it asks about. The other side answers only for listed spaces the asker may read, as `@:asker` for a server (P-02). A mismatch that forms an equivocation or wrong-home proof is handled as an R-14 failure. Outside `hello` and `read`, verifiers gossip in the space as signals (T-05): heads as `{type: "heads", receipts: Receipt[]}`, faults as `{type: "fault", proof: FaultProof}` (han.ts `GossipSignal`). A receiver compares received heads with its own records, and adopts a fault only if the proof checks against its own server keys; a proof that doesn't check is ignored. Signals carry no authority (T-05), and only readers of the space can send or receive them (G-04). A server verifier MUST exchange heads with at least one other verifier of each space it verifies, at least every 10 minutes.

### 4.5 M5 Executor: the only thing that touches the world

An executor is an actor, usually of kind `service`, that owns part of the world: a checkout, a VM, a mailbox, an account. Each running process (an **instance**) holds one of the actor's keys; `effect.claim.body.instance` is that keyId. Workspaces and snapshots are executor-private, and appear in HAN only as plans and blobs. `stallBound` = 10 s. The executor runs this protocol inside `deliver`:

| Step | Rule |
|---|---|
| **X-01** | Only the instance whose keyId equals `claim.instance` may act on that claim. Its launch journal is durable and private to that instance. |
| **X-02** | Before the first send of a claim, durably journal `claiming(claimId)` with the signed claim and **the reading of its elapsed-time clock at that moment** (R-07). That clock MUST keep counting through suspend, sleep and VM pause (e.g. `CLOCK_BOOTTIME`). An executor MUST NOT run on a platform that can't provide such a clock. |
| **X-03** | The claim states the window it expects (`body.window`). Policy accepts it only if it equals the space's `launchWindow`, so `W` = `claim.window` is known to the executor without reading anything else. Act only after the claim's receipt is in hand. |
| **X-04** | Durably journal `launching(claimId)`. Then, **as the last step before launching**, check that less than `W − stallBound` has passed on the elapsed-time clock since the first send. If not, durably journal `expired(claimId)`, do not launch, and report `{result:"failed", started:"no"}`. The platform MUST ensure no more than `stallBound` passes between this check and the launch. |
| **X-05** | Report with `effect.outcome`. `started: "no"` only if `launching` was never journaled, or `expired` follows it. |
| **X-06** | **No launch after restart.** On restart, for each `claiming` without `launching`: finish the claim's append (R-06). If it has a receipt, report `{result:"failed", started:"no"}`. For each `expired`: report the same. For each `launching` with neither `expired` nor an outcome: look the result up in the outside system if possible, otherwise report `{result:"unknown", started:"unknown"}`. |
| **X-07** | An instance key MUST NOT be held by two running processes at once, and MUST NOT be restored on a new machine without its journal. Losing the journal means retiring the key (`key.revoke`), after which the owner abandons its open claims (P-09). |
| **X-09** | Before launching, check that the plan to be run hashes to the proposal's `planDigest`. A plan whose content was redacted can't be run (P-12 prevents this). |
| **X-10** | If an `effect.outcome` is denied because an owner already abandoned the claim, the executor MUST report what really happened with `effect.resolve`, attaching its journal and outside-system evidence. |
| **X-11** | After a `failed` / `started: "no"` outcome, an executor SHOULD back off (exponentially, capped) before claiming the same effect again, so a broken environment doesn't burn attempts. An executor that won't run an effect (an unsupported action, a plan it can't check) SHOULD tell the proposer with a `message` whose `refs` name the proposal, and MUST NOT claim it. |

**Why no launch follows an abandon.** Let `d` be the drift bound (1%). Define a clock's rate as clock seconds per true second, within `[1−d, 1+d]`. Each side measures only an interval on its own clock, so clock offsets cancel.
1. The executor launches at most `W` executor-seconds after the send. That is at most `W/(1−d)` true seconds.
2. The claim reaches the sequencer no earlier than it was sent. Its `receipt.time` is rounded down, so it is at most 1 s early.
3. An owner may abandon only at sequencer time ≥ `receipt.time + A`, with `A = ⌈W·(1+d)/(1−d)⌉ + 2` (P-09), and only after `A` steady-clock seconds (R-16). That is at least `(A − 1)/(1+d)` true seconds after the send, which exceeds `W/(1−d)`.

So the abandon always comes after the latest possible launch, whichever way each clock drifts. The model checker covers this in two scenarios, each with abandon, crashes, arbitrary delivery delay, sub-second arrival with rounded receipts, and stalls on the executor clock:
- *timing*: executor clock offsets, and the owner shortening `launchWindow` mid-claim;
- *drift*: the sequencer's clock 20% fast and the executor's 20% slow (exaggerated; the formula takes the bound as a parameter).

In its last full run ([ref/logs/model_check-r17.log](ref/logs/model_check-r17.log), made during round 18) it found no counterexample. `ref/policy.py` was edited after that run (P-16 clean-up) and the model has not been re-run on the current file, so treat the result as a round-18 result. Five of its ten mutations are about this argument and each produces one: deadline from receipt time, window not pinned, executor uses current window, no rounding margin, and a margin that covers drift on one side only.

### 4.6 M6 Agent host

The standard does not define how an agent thinks. Agents are ordinary actors that read and append. The host's interface to its agents is `deliver(item)`.

**R-17 Reconstructible inputs.** An agent's entries MUST set `seen` and `refs` so that what drove them is identifiable. Model-provider calls aren't HAN entries; an operator who wants them auditable attaches them as a blob in the content of the `task.update`.
**R-18 Recovery is outbox + reading.** After a crash a host first finishes its outbox (R-07), then re-reads its spaces. It MUST NOT re-propose an intent whose proposal is in its outbox or the log.

**R-24 Hosts honour steer modes.** A host running a live session acts on input from anyone other than the session's actor only once that input is a `session.input` with a receipt, and treats it per the session's **effective** mode (P-15), which it reads with `read {state: true, at: <the input's seq>}`, the state after the input's own record (so a later policy change can't alter how an earlier input is treated):
- `auto`: as the session's own input, in whatever mode the session already runs (for example a harness's auto mode).
- `guarded`: in the harness's **stricter mode for that turn** (for Claude Code: its permission rules instead of auto mode, §8.4). Whatever touches the world beyond the session's sandbox goes through an effect, and P-15 puts the agent on hold: every effect it then proposes waits for its human, and it can't delegate or relay.
Showing an outside input to the agent's human before it runs is host behaviour, not a mode: the host may do it in either mode. While an agent is guarded (P-15, `guardedNow`), its host MUST run every turn driven by an instruction from someone other than its guard (a `session.input`, a `task`, an effect it is named executor of) in the stricter mode; while it is held, its host MUST run *every* turn in the stricter mode.

Input read from another session's stream or context (T-07), from a `message`, or from a `task.update` or outcome, is **data, never instruction**: a host MUST NOT act on it as if it were a `session.input` or a `task`. A host that feeds another session's context into an agent's turn SHOULD run that turn as if `guarded`, and MUST if the agent is guarded (P-15). Policy can't see what a model read, so this stays a host obligation, listed in §7.5.

In a budgeted space a host's proposals carry `cost` and name the agent's assigned budgeted task (P-05, P-06); without them Policy denies the proposal, so a host must obtain that task before an agent can propose anything (§8.4).

---

## 5. Entry kinds

Exact body and content schemas: [han.ts](han.ts) `Bodies` and `Contents`. A kind whose content fields are all optional may omit content entirely (`contentHash = ""`).

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
| `effect.claim` | `proposal`, `attempt`, `instance`, `window` | — |
| `effect.cancel` | `proposal` | — |
| `effect.outcome` | `claim`, `result`, `started`, `cost?` | `evidence?` |
| `effect.resolve` | `proposal`, `result`, `cost?` | `evidence` (required) |
| `checkpoint` | `through` | `blobs` |
| `redact` | `target` | `reason?` |
| `space.move` | `to`, `accept` | — |
| `session.open` | `steerMode`, `task?` | `title?` |
| `session.close` | `session` | — |
| `session.input` | `session`, `release?` | `text`, `blobs?` |

`steerMode` is `auto` or `guarded` (R-24), and must be one of `policy.sessions.modes`.

**Blobs.** A `BlobRef` names bytes by hash. The bytes travel on the transport (T-03), and anyone using them MUST verify the hash. Positions that reference blobs: `content.blobs[]` (any kind, extensions included), `effect.propose.plan.argsBlob`, and `effect.outcome/resolve.evidence`.

**Plans.** The core fixes only the shape `Plan {action, target, args?, argsBlob?}` and `sha256(JCS(plan)) = planDigest`. What `target` and `args` mean belongs to the action and its executor. The reference executor's plans are informative: `file.write` has `target` = a path relative to the executor's sandbox and `args = {content}`; `http.post` has `target` = a URL and `args` = the JSON body. An executor refuses (never claims) a plan it can't run or check (X-09).

**Extensions** add kinds `<prefix>.<name>` (grammar in §2) whose prefix the core doesn't use. The core prefixes `space`, `member`, `policy`, `key`, `message`, `task`, `effect`, `checkpoint`, `redact` and `session` are reserved, and an unknown kind under them is `invalid`. To the core, an extension's body and content are opaque Json. Actors holding at least `policy.extensionsMinRole` (default `member`) may append one, subject to size limits and the number rule. "At least" is by rank owner > member > guest; no role, or `banned`, is below every minimum, and the result is `denied` (P-16). Extensions define their own meaning; the core neither enforces nor depends on it.

---

## 6. Policy (normative; reference: ref/policy.py)

### 6.1 State and context

`SpaceState` (han.ts) is the fold of all records. `ctx = {time, profiles}` (§4.1).

**P-01 Relevant actors.** `relevantActors(state, entry, time)` is a list with each actor once, at its first position, in this order: the author; the target of a `member.set`; for an `effect.claim` of a proposal in the state, the proposer, then the authors of the approval *candidates* (E3, unexpired at `time`) in candidate order, then the effect's guards (P-15: at most two, the proposer's and the executor's). Policy can't see key validity, so the function lists **every** candidate. The sequencer MUST pin the author, the target, the proposer and the guards. It pins the candidates in that order (E3 order), **up to and including the first whose signing key is valid**; later candidates are not required and are not pinned. `decide` needs the author, the target and the proposer in `ctx.profiles`, and a candidate only if E3 reaches it (P-16). If the sequencer can't pin a required actor, or a candidate before the first valid one, it answers `unavailable`. So an unreachable approver matters only if every earlier approver's key was revoked (wire scenario "approvers are pinned up to the first valid key"). At most 16 approvals are open per proposal, plus at most two guards (P-08, P-15), which bounds `receipt.profiles`.


**P-16 `invalid` and `denied`, and the order of checks.** `decide` answers `allow`, `denied` or `invalid` (han.ts `Decision`). The codes are defined by the **class of the check that failed**, not by whether a later entry could change the answer. Neither can be retried (§3); only `unavailable` means "same request later".
- **`invalid`**: the entry is malformed or inconsistent with what it names. That is: its shape is wrong (R-09); it names something that isn't there, or is there as another kind (a `task` that is a message counts as missing); it contradicts what it names (a different `planDigest`, a `(result, started)` pair outside the table, a cost the claim's reservation can't cover); its place is impossible (`seen` or `checkpoint.through` beyond the head, a `space.create` on an existing space, a first entry that isn't one, a reused keyId, a `key.revoke` of a key that isn't valid); or the caller left a required profile out of `ctx` (R-25). `seen` > head is `invalid` although the head grows: a sequencer orders an entry only against its head at append, so an entry claiming to have seen records that don't exist yet is impossible at that moment, and the sender signs a new one once they do.
- **`denied`**: an authority, role, phase, time, budget or policy check failed: the key, role, kind, server or accountability of the author; the policy; the phase or flags of the task or effect; time (expiry, the abandon wait); budgets; counts and caps. This includes permanent failures, such as a revoked key, a closed task, a final phase or an expired proposal. A sequencer answers an abandon that is merely early `unavailable` (R-16), but Policy's own verdict on it is `denied`.
- **Order.** The verdict is that of the first failing check. All kinds share the common steps below, then follow the kind's own list. The kind lists are in the order `ref/policy.py` runs them, and where they interleave `invalid` and `denied` the order is part of the rule (vectors `decision-order` and `kind-check-order`).

Common steps:
1. Shape of the header fields Policy reads and of the body (R-25, R-09): `invalid`.
2. Pins: every actor of `relevantActors` has a view in `ctx.profiles` (for an `effect.claim` only the author and the proposer here; a candidate or guard is checked when E3 reaches it): else `invalid`.
3. The author's key is `valid` in `profiles[author]` (R-12): else `denied`.
4. The space has no state yet: only `space.create` is possible (else `invalid`). In a profile space, the author must be `@:<server of the actor>` (`denied`), the body needs `profile` (`invalid`) and an operator other than the actor (`invalid`). In a plain space the body must not carry `profile` (`invalid`).
5. The space exists: a `space.create` is `invalid`; `seen` > head is `invalid`; a `refs` entry not in the space is `invalid` (R-05).
6. The author's server is in `policy.servers.deny` and the kind isn't `effect.outcome` or `effect.resolve`: `denied`.
7. In a profile space: `key.add`, `key.revoke`, `member.set` follow the table, every other kind is `denied`.
8. The author is held (P-15, §6.6).
9. An extension kind: `denied` unless the author's role is at least `extensionsMinRole`. Nothing else is checked.
10. The kind's own list.

In the kind lists, **I** is `invalid`, **D** is `denied`, and a check that allows ends the list. "No role" means the author doesn't hold owner, member or guest. "Accountable" is P-03. A role named here is the author's *current* role.

| Kind | Checks in order |
|---|---|
| `member.set` | D the target is the only owner and the new role isn't owner · D a readable role for a target on a denied server · D owner for a non-human target · D the P-03 agent check · allow: author is an owner · allow: self-leave · allow: open join · allow: invite · D |
| `policy.set` | D author isn't an owner · D the home's server is in the new `deny` · D every current owner's server is in it · D the budget change is not allowed (P-06) |
| `message` | D no role |
| `task` | D no role · D an assignee has no role · with `parent`: I it isn't a task · D closed · D author isn't an assignee of it · D depth over `maxDelegationDepth` · D parent has a budget and the task has none · without `parent`: D author is a guest · with `budget`: D not accountable · D a non-human author whose `parent` isn't an open budgeted task assigned to it · D the space has no budget · D unit differs · D amount exceeds what the source holds |
| `task.update` | I not a task · D closed · D no role · allow: author is an owner and not an assignee, state final · D author is an assignee, state final, and an effect naming the task is `claimed` or `unknown` · allow: author is an assignee · D |
| `task.cancel` | I not a task · D closed · D no role · D author is neither the task's author nor an owner |
| `effect.propose` | D author isn't owner or member · with `task`: I it isn't a task · D closed · D author is neither an assignee nor an owner · D budgeted space and no `cost` or another unit · D not accountable · D budgeted space, non-human author, and `task` isn't an open budgeted task assigned to the author |
| `effect.approve` | I not a proposal · I `planDigest` differs · D phase isn't `awaiting-approval`, `ready` or `claimed` · D `expiresAt` already passed · D author isn't an approver of the rule that applies (P-08) and isn't a guard · D `separateApprover` and author is the proposer (guards exempt) · D 16 open approvals by other authors (guards exempt) |
| `effect.claim` | I not a proposal · D E1 author isn't the executor, or `instance` isn't the signing keyId · D executor isn't owner or member · D executor not accountable · D proposal expired · D E2 proposer isn't owner or member now, or its proposal key isn't valid · D proposer not accountable · D `window` ≠ `launchWindow` · E3: I a candidate reached has no pinned profile, D none has a valid key (P-15 guards follow) · D cancel requested · D phase isn't `ready` · D the task is closed · D `attempt` ≠ 1 + earlier claims · E6 in a budgeted space: D no `cost` or another unit · D less left than `cost` · D charged to the space and the proposer isn't human |
| `effect.cancel` | I not a proposal · D no role · D author isn't the proposer, an owner, a guard, or an earlier approver · D phase is final · D phase is `claimed` or `unknown` and a cancel was already requested |
| `effect.outcome` | I no such claim · D not the effect's latest claim, or phase isn't `claimed` · I `(result, started)` outside the table · with `cost`: I `started` isn't `yes` · I the cost exceeds the claim's reservation or is in another unit, or there is none and the space has a budget · allow: author is the executor signing with the claim's `instance` · allow: author is an owner, `unknown`/`unknown`, and `ctx.time` ≥ claim time + A · D otherwise |
| `effect.resolve` | I not a proposal · D phase isn't `unknown` · D author is neither the executor nor an owner · with `cost`: I result is `failed` · I the cost exceeds the latest claim's reservation or is in another unit, or there is none and the space has a budget |
| `checkpoint` | D author isn't owner or member · I `through` > head |
| `redact` | I target isn't in the space · D target is a `redact` · D target is an `effect.propose` whose effect isn't final · D author is neither the target's author nor an owner |
| `space.move` | D author isn't an owner · D an effect is `claimed` |
| `key.add`, `key.revoke` | in a plain space D · in a profile space D author is neither the actor nor `@:server` · `key.add`: I keyId already in the profile · `key.revoke`: I the key isn't currently valid |
| `member.set` in a profile space | allow the confirmation or the withdrawal of P-03 (confirmation: D if the operator isn't human) · D |
| `session.open`, `session.input`, `session.close` | §6.6 |

### 6.2 Roles, joining, reading

Roles: `owner`, `member`, `guest`, `banned`, or none (absent). **Owners are human.** Only actors of kind `human` may hold `owner`; agents and services are members at most.

**P-02 Read.** `canRead(state, reader, seq)` is true if:
- `policy.visibility = "public"`; or
- `reader` is owner/member/guest now; or
- `reader` held such a role at `seq`, meaning former members keep what they saw: `roleHistory` has an interval `{from, to?}` with `from ≤ seq < to` (no `to` while the role lasts). `from` is the seq of the `member.set` that gave a readable role to an actor with none (or banned), the creator's is 1, and `to` is the seq of the `member.set` that ended it (to `banned` or `none`). So a removed member reads what came before the removal, not the removal entry itself. Moving between readable roles opens no interval. A role held **now** reads every seq, the ones before it was granted included; or
- `reader` is a server actor `@:S`, and either `S` is the space's current home, or `S` is not in `policy.servers.deny` and some actor on `S` holds a readable role now or at `seq` (as above).

Profile spaces are public. The `state` item of `read` needs read rights at the head, or at `at` when `at` is given (§3). **Read rights are always evaluated on the current state** (`canRead(state, reader, seq)` with the state at the head): a role held now reads every seq, so a member who joined at seq 3 can read `{state, at: 2}` and the records before their join, and a reader with no role now and none at `seq` reads nothing (wire scenario "a role held now reads the state at an earlier seq"). The state returned for `at` is the historical fold; the rights are not. On a transport, chat visibility is the transport's business, and HAN's read rights govern only the HAN log.

**P-03 Membership** (`member.set {actor, role}` by `author`, with target's current role `cur`):
- An **owner** may set any role (their own included), except that a set leaving zero owners is denied (even when an owner demotes or bans themself), and a non-human can't be made owner.
- **Self-leave:** an owner, member or guest may set themselves to none (the last owner can't, above). A banned actor may not.
- **Open join:** if `join = "open"`, an actor with `cur` = none may set themselves to `member` (not `guest`). Banned actors may not.
- **Invite:** if `membersMayInvite`, a member (not a guest) may set an actor with `cur` = none to `guest` or `member`.
- Anything else is denied. `member.set` is never `invalid` once its shape is right.
- **Agents:** when a readable role is *granted* to a target whose profile `actorKind ≠ "human"` and who holds none (no role, or banned), `allowAgents = "none"` denies, and `"with-operator"` requires `operatorConfirmed`. Moving between readable roles, banning and removing are never re-checked. The same check, called *accountability*, is re-applied to a non-human author at every `effect.propose`, `effect.claim` and budgeted `task`, and to the *proposer* at claim time (E2). So withdrawing an operator's confirmation, or switching `allowAgents` to `"none"`, stops agents already in the space.
- **Servers:** *granting* a readable role to an actor on a denied server is denied. Banning or removing such an actor is always allowed.
- **Profile spaces:** the actor and `@:server` are the owners, fixed at creation, and `roles` never changes afterwards. The only `member.set`s allowed are (a) the **confirmation**: author = the profile's `operator`, body `{actor: operator, role: "owner"}`, and the operator is `human` in `ctx.profiles`; and (b) the **withdrawal**: author = the operator or `@:server`, body `{actor: operator, role: "none"}`. Each sets `operatorConfirmed` to `true` or `false`. Everything else is denied, including the actor's own and the server's confirmation, and any `member.set` in a profile with no operator. §4.3.
- **Creation.** `space.create` makes its author the owner (roleHistory `[{from: 1}]`) and `budgetLeft` the whole budget. Owners are human, so a creator that isn't human is `denied`; a policy whose `servers.deny` names the space's own home is `invalid`. Whether to host a space at all is still the sequencer's decision (§4.1 step 2). A fork's `forkOf` is checked for its shape only.

**P-04 Who may append what** (beyond the kind-specific rules below). "Holding a role" means owner, member or guest *now*. Approvers must also hold one. Authors on a denied server may append nothing, except fact reports (`effect.outcome`, `effect.resolve`).

| Kind | Allowed authors |
|---|---|
| `message` | owner, member, guest |
| `task` | owner, member; or an assignee of `parent` who still holds a role |
| `task.update` | an assignee of `task` who still holds a role; or an owner, with a final state only (force-close) |
| `task.cancel` | the task's author, or an owner, holding a role |
| `effect.propose` | owner, member |
| `effect.approve` | an actor matching the action's approvers by exact role or ID (P-08) and holding that role now, or one of the effect's guards (P-15) |
| `effect.claim` | the named executor, still a member or owner and accountable (P-07) |
| `effect.outcome`, `effect.resolve` | the claim's instance or executor, **regardless of current role or server**: they report facts about work already authorized; owners as P-09/P-10 allow |
| `effect.cancel` | proposer, anyone who ever approved it (consumed or superseded approvals count), any of its guards, or an owner, holding a role |
| extension kinds | roles at or above `policy.extensionsMinRole` |
| `checkpoint` | owner, member |
| `redact` | the target's author (whatever role they hold now), or an owner |
| `policy.set`, `space.move` | owner |
| `session.open` | any role holder, accountable (P-15) |
| `session.input` | holders of the session's steer right (P-15) |
| `session.close` | the session's actor, or an owner |
| `key.add`, `key.revoke` | profile spaces only (§4.3) |
| `space.create` | never in an existing space (`invalid`, P-16) |

### 6.3 Tasks and the budget

A space has at most **one budget**, `policy.budget = {unit, amount}`, or `null`. Amounts are integers in minor units. `budgetLeft` is the budget minus everything *committed* (reserved or spent). Amounts only move between the space, task budgets and claim reservations, or are spent at settlement. The fuzzer checks that task remainders never go negative, that no remainder exceeds its budget, and that the space remainder goes negative only through a cut.

**P-05 Tasks.**
- `to` is non-empty (an empty list is `invalid`) and every assignee currently holds a role (`denied` if not).
- A `parent` must be an open task in this space (an id that isn't a task is `invalid`), and the author must be one of its assignees: a guest assignee may delegate, an owner who isn't an assignee may not. Top-level tasks have depth 0; a child's depth is its parent's + 1, which must be ≤ `maxDelegationDepth` (so 0 forbids delegation).
- **Task budgets** exist only in budgeted spaces and use the space's unit. A task's budget is reserved from its parent if the parent has a budget, otherwise from the space, and must fit what is left there. A child of a budgeted parent MUST carry a budget. An agent or service may create a budgeted task only as a child of an open budgeted task assigned to it.
- A final `task.update` (`done`, `failed`, `declined`, `cancelled`) closes the task and refunds its unspent remainder to the nearest *open* budgeted ancestor, starting at its parent, or to the space if there is none. The closed task's own remainder becomes 0. An assignee can't close a task while an effect naming it is `claimed` or `unknown`. An **owner who is not an assignee** may close any open task with a final update (force-close), for example when its assignees are gone, even over a claimed effect; an owner who is an assignee closes under the assignee rule.
- On a closed task, these are refused: `task.update`, `task.cancel`, a child `task`, and an `effect.propose` that names it.
- `task.cancel` sets `cancelRequested`, and may be repeated. The task stays open until an assignee, or an owner, posts a final update.

**P-06 Costs.**
- **Budgeted spaces are closed.** If `policy.budget` is set, every `effect.propose` MUST carry a `cost` in its unit, and a claim of an effect without one is denied (E6). An outcome or resolve may report a cost only within its claim's reservation (the same unit, an amount no larger). With no reservation, it may report a cost only in an unbudgeted space, where costs are informational. Any other cost is `invalid`, not `denied` (P-16), because the reservation is fixed by the claim.
- **Only humans draw on the space budget.** In a budgeted space, an agent or service MUST name, in every proposal, an open budgeted task assigned to it, and its costs are charged there. E6 checks it again at claim, so a budget added after an agent's proposal still can't be spent by it. Agents can't be owners (§6.2), so they can't change the budget either.
- **Naming a task:** an `effect.propose` that names a task is `denied` unless its author is an assignee of that task or an owner, whether or not the task has a budget.
- **Charging:** an effect's cost goes to its named task if that task has a budget when the claim is ordered, otherwise to the space. It is reserved at claim and settled at outcome or resolve. Settling with a cost refunds the difference, and a settlement with no cost charges the whole reservation. A refund goes to the claim's source task if that task is still open and budgeted, otherwise to the nearest open budgeted ancestor of it, otherwise to the space (as in P-05).
- **Changing the budget** (`policy.set`): with the same unit, `budgetLeft` = new amount − committed, which can go negative after a cut. Nothing new can be reserved until refunds bring it back up. Changing the unit, or adding or removing the budget, is allowed only while nothing is reserved, meaning no open task holds a budget and no claim holds an unsettled reservation (money already spent isn't reserved); the remainder then starts fresh.

### 6.4 The effect state machine

Phases: `awaiting-approval`, `ready`, `claimed`, `unknown`, and the final phases `succeeded`, `failed`, `cancelled`. `cancelRequested` is a flag. A final phase never changes.

**Transition table.** Any (phase × kind) combination not listed is `denied`.

| Phase | `effect.approve` | `effect.cancel` | `effect.claim` | `effect.outcome` | `effect.resolve` |
|---|---|---|---|---|---|
| awaiting-approval | → ready once the rule's and every guard's approvals are in hand (P-08, P-15), else stays | → cancelled | — | — | — |
| ready | stays ready (adds an approval) | → cancelled | → claimed (P-07) | — | — |
| ready, when P-15 records a guard on it | → awaiting-approval (any entry; until the guard approves) | | | | |
| claimed | stays claimed | sets `cancelRequested` (denied if already set) | — | per P-09 | — |
| unknown | — | sets `cancelRequested` (denied if already set) | — | — | per P-10 |
| final | — | — | — | — | — |

`effect.propose` → `awaiting-approval` if its action needs approval (P-08) or its proposer is held (P-15), else `ready`. An executor's hold (P-15) doesn't change the phase; it adds a guard that E3 checks at claim.

**Re-arm.** After an attempt ends without success and the effect may run again, its phase is `cancelled` if `cancelRequested`; else `ready` if its approvals are in hand: the rule that applies, if any, has an unconsumed, undropped candidate (E3, without the expiry and key checks, which E3 repeats at claim), and each guard has one (P-15); else `awaiting-approval`. A fresh proposal, a returned approval and a failed resolve all use this test.

**P-07 Claim** (`effect.claim {proposal, attempt, instance, window}`). Allowed only when all of these hold:
- **E1** The author is the proposal's `executor`, still holds owner or member, is accountable (P-03), and `instance` is the valid keyId that signed this claim.
- **E2** The proposer still holds owner/member, is still accountable (P-03), and the key that signed the proposal is still valid in the proposer's pinned profile.
- **E3** If approval is needed, walk the **candidates**: unconsumed, undropped approvals in order whose `expiresAt` (if any) is after `ctx.time`, whose author satisfies the approvers rule *now*, and (with `separateApprover`) who aren't the proposer. The first candidate whose signing key is valid is consumed, and candidates with revoked keys are skipped. If none is valid, deny.
- **E4** `cancelRequested` is false. (Redundant with the table, kept so one rule states it.)
- **E5** The phase is `ready`; `attempt` = 1 + the number of earlier claims; `window` = the current `policy.launchWindow`; and the proposal's `expiresAt`, if any, is after `ctx.time`; and the task it names, if any, is still open.
- **E6** In a budgeted space, the effect has a cost in the space's unit that fits what is left where P-06 charges it. It is then reserved.

The claim records its receipt time and pins its `window`.

**P-08 Approvals.** A client MUST display the plan whose `planDigest` it is about to sign, and MUST NOT offer an approval for a plan it hasn't fetched and hash-checked. `policy.approvals` is a list of `{action, approvers}`. The first entry whose `action` equals the proposal's action, or is `"*"`, applies, and `approvers` is a non-empty list of roles (`owner`, `member`, `guest`) or actor IDs. A role entry matches an actor whose current role is exactly that role (an owner doesn't satisfy `["member"]`; list both to allow both), an ID entry matches that actor, and either way the approver must hold a role now. If none applies, no approval is needed. With `separateApprover: true`, the proposer can't approve their own proposal. An approval for a proposal not in this space, or with a wrong `planDigest`, is `invalid`. An approval is accepted while the effect is `awaiting-approval`, `ready` or `claimed`, and `denied` in any other phase. An approval whose `expiresAt` has already passed (at or before `ctx.time`) is denied. A new approval **supersedes** its author's open ones on the same proposal (they are dropped; a consumed one stays consumed), so approvals can be refreshed, and each author has at most one open. An approval is **open** while it is unconsumed, undropped and unexpired. At most 16 open approvals by other authors may exist when a new one arrives; a guard's approval (P-15) doesn't count, so others can't crowd it out. After a `policy.set`, an effect in `awaiting-approval` whose approvals are now in hand under the new policy (the re-arm test, §6.4; no rule at all is the simplest case) becomes `ready`.

**P-09 Outcome** (`effect.outcome {claim, result, started, cost?}` for the latest claim, at most once per claim: it needs phase `claimed`, so an older claim, or one that already has an outcome, is `denied`; an id that is no claim is `invalid`):
- **Author:** normally the claim's instance, i.e. the same actor with `keyId = claim.instance`.
- **Abandon:** an owner may append it with `result: "unknown"` and `started: "unknown"` only if `ctx.time ≥ claim.time + A`, where `A = ⌈claim.window · 101 / 99⌉ + 2` computed in integers, and the sequencer's steady clock agrees (R-16). For example, 30 s gives 33 s and 3600 s gives 3675 s. An abandon by anyone but an owner, or before `A`, is `denied` by Policy (a sequencer answers an owner's early abandon `unavailable` with `retryAfterMs` only when Policy would allow it at claim time + `A`, R-16). An abandon always leaves the effect `unknown`, even if it is repeatable, because it might still be running. An outcome counts as the executor's own report only when signed with the claim's `instance` key; any other key, even the executor's, is an abandon and leaves `unknown`.
- **`started: "no"`** (result `failed` or `cancelled`): the consumed approvals and the cost reservation are returned. A returned approval is **dropped** if its author has approved again since (a newer open approval on the same proposal supersedes it, P-08). The phase is then `cancelled` if `cancelRequested`, else re-armed (§6.4), normally `ready`.
- **`started: "yes"`:** cost settles at `cost`, which must be ≤ the reservation (default: the reservation), and the rest is refunded. `succeeded` → phase `succeeded`. `failed`, or `cancelled` (stopped partway) → phase `failed`; but if the effect is `repeatable` and no cancel is requested, → re-armed (§6.4) instead: the consumed approvals stay consumed, so an effect that needs approval waits in `awaiting-approval` for a fresh one.
- **The executor's own `unknown`:** the reservation stays charged and the phase becomes `unknown`. If `repeatable` and no cancel is requested, the reservation is instead settled as spent in full and the phase is re-armed (§6.4): the proposer declared that running it again is harmless.

Valid `(result, started)` pairs:

| result \ started | `yes` | `no` | `unknown` |
|---|---|---|---|
| `succeeded` | ✓ | — | — |
| `failed` | ✓ (ran and failed) | ✓ (never launched) | — |
| `cancelled` | ✓ (stopped partway) | ✓ (never launched) | — |
| `unknown` | ✓ (launched, result lost) | — | ✓ |

All other pairs are `invalid`. `cost` is allowed only with `started: "yes"` (else `invalid`), and must lie within the reservation (P-06).

**P-10 Resolve** (`effect.resolve {proposal, result, cost?}` with evidence, in phase `unknown`):
- **Author:** the proposal's executor (any instance), or an owner.
- `succeeded` → phase `succeeded`, with cost settled as in P-09. `failed` → the reservation is refunded, every unconsumed approval is dropped (none is carried), and the phase becomes `cancelled` if `cancelRequested`, otherwise re-armed as in §6.4 (`awaiting-approval` when approval is needed). A `cost` with `failed` is `invalid`.
- Resolving is a statement of fact, and the *only* way a non-repeatable effect with an uncertain past can run again. `unknown` is reached only by the executor's report, or by an abandon after the claim's window, so the claim can no longer launch. A resolve that is true when made therefore never leads to a second run (model property UnknownNeverSilentlyRetried, which counts only resolves already false when made).

### 6.5 Redaction, checkpoints, forks

**P-11 Kind-specific checks.**
- `checkpoint.through` ≤ the current head, else `invalid`; the author is an owner or a member.
- `space.move` is denied while any effect is `claimed`, so one sequencer orders a claim and its abandon (R-16). A move to the current home is allowed.
- `policy.set` replaces the whole policy, applying the P-06 budget rules and the P-08 release rule. It can't change the `window` pinned in existing claims. It can't put the current home's server in `servers.deny`, and can't leave every current owner on a denied server (denying some owners' servers is allowed). Roles are untouched, because roles live only in `member.set`.
- `space.create` is only valid as the first entry: in a space that has any entry it is `invalid`, and the first entry of a space must be one (P-16).

**P-12 Redact** (`redact {target}`): the target's content is deleted wherever honoured, its `redacted` flag set and its blob references released. The target must be in the space (`invalid`). A `redact` can't target another `redact`. The plan of an unfinished effect can't be redacted (cancel it first). The author must be the target's author, whatever role they hold now, or an owner. Both refusals are `denied`.

**P-13 Forks carry no authority.** A space with `forkOf {space, through}` starts from its own `space.create` alone: creator as owner, the new policy, and nothing carried over (no members, tasks, approvals, budget commitments or effects). The parent's records are history to read, never authority. Policy checks `forkOf` for its shape only.

### 6.6 Live sessions and steering

The point of a space is to see and steer work outside your own silo while it happens, not after a PR. A **session** is one actor's live working context (a Claude Code run, an editor, a human at a terminal). Its stream travels on the transport (T-07); the log holds the authority over it.

**P-15 Session rights and steering.**
- **Open and close.** `session.open {steerMode, task?}` opens a session whose actor is the author: any role holder, accountable (P-03), at most 64 open per actor. `steerMode` (`auto` or `guarded`) must be in `policy.sessions.modes`. An agent's confirmed operator is recorded as the session's **guard**; an agent may open a `guarded` session only if it has one who holds a role here. A `task` must be one the author is assigned to (or the author is an owner). `session.close` ends it (its actor or an owner).
- **Effective mode, live.** If owners later remove `guarded` from `modes`, a guarded session is dead (owners can make open sessions stricter, never looser); if they remove `auto`, an auto session behaves as guarded. A session is **live** while open, its actor holds a role, it has an effective mode, and its actor is human, or it is effectively `auto`, or its guard holds a role. An agent is **guarded** in the space while any of its live sessions is effectively guarded.
- **Rights.** `policy.sessions = {watch, read, steer, modes}`. The first three are each a minimum role, `guest`, `member`, `owner` or `nobody`, and `read` and `steer` may be no wider than `watch`. `sessionAccess(state, actor, session)` returns three booleans: on a live session its actor has all three, and so does its guard while the guard holds a role; anyone else has a right if their role is at least its level and the level isn't `nobody`; a dead session grants nothing. **Watch** = the output and status stream; **read** = its full context (transcript, files, plan), also for agents, which is how one agent reads another's session; **steer** = append `session.input`. Owners set these per space; a network's defaults pre-fill them. Typical: team `{member, member, member}`, pair session `{guest, guest, guest}`, demo `{guest, owner, owner}`, guarded sessions only `modes: [guarded]` (forcing applies to sessions; an agent with no session is guarded only by its own choice).
- **Steer.** `session.input {session}` needs the steer right on a live session, and an accountable author.
- **The invariant.** *While an agent `X` is guarded, any instruction to `X` from anyone but its guard `g` holds `X`; while `X` is held, nothing `X` proposes or is named executor of runs without `g`'s own approval; only `g` ends a hold.* Concretely:
- **Guarded.** `guardedNow(X)` is true, with guard `g`, when either `X` chose it (it opened a `guarded` session: `guarded[X] = g`, its confirmed operator, who must hold a role) or owners force it (one of `X`'s live sessions is effectively `guarded` under the allowed modes; `g` is that session's guard). The choice is sticky: owners closing `X`'s sessions, removing `g`, or changing modes don't end it. Only `X` ends it, while not held, by closing its last guarded session (re-closing one an owner already closed counts), so an agent can always undo its own choice.
- **Hold.** An *instruction* to `X` is a `session.input` into any of its sessions, a `task` naming it (each assignee), or an `effect.propose` naming it as executor. A steer from anyone but `X` and `g` while `guardedNow(X)` puts `X` **on hold for `g`** (`held[X] = g`). After **every** entry Policy also settles holds in one place: if `guardedNow(X)` and an outside task naming `X`, or an unfinished outside effect naming it as executor, is still open and newer than `X`'s last release, `X` is held, and each such effect records `g` among its guards. So becoming guarded by any route (choosing it, owners forcing it, a guard regaining its role) with outside work pending holds at once, and an effect aimed at a guarded executor keeps needing `g` after a release. One rule per agent and per path, so an auto side-session, a task or an executor slot is no side door. `g` ends the hold with `session.input {session, release: true}` on any of `X`'s sessions, even a closed one; nothing else does. A release covers what `g` had seen: it is denied if an outside instruction to `X` arrived after its `seen`, and work newer than its `seen` stays pending (so it re-holds). A client MUST show a guard a redacted instruction as "an instruction was received and redacted" before letting them release. While held, `X` may only: talk (`message`), work its own sessions (`session.open`, `session.close`, `session.input` into its own), report (`task.update`, `effect.outcome`, `effect.resolve`), claim, cancel (`effect.cancel`, `task.cancel`), and propose. Everything else, including `task`, `effect.approve`, `session.input` into others' sessions, `member.set`, `policy.set`, `redact` and extensions, is denied. Anything else a held agent asks of others is data (R-24) or an effect proposal, which needs `g`.
- **Guards approve.** An effect's **guards** are: `g` if its proposer was held for `g` when it proposed (recorded; it then starts `awaiting-approval`), plus the guards its proposer and its executor are held for *now*, so even a proposal made before the hold needs `g` while its proposer is held. Each guard may approve or cancel it whatever the approval rules say, while it holds a role; one approval by a guard who is also a rule approver satisfies both. E3 additionally requires, for each guard, an unconsumed, unexpired approval by that guard itself, signed with a key valid at claim. The claim consumes them, a not-started outcome returns them, and approval rules still apply on top.
- **A departed guard.** Nobody stands in for a guard. While `g` holds no role, `X`'s guarded sessions are dead and its guarded effects can't be claimed; owners may cancel them (owners may always cancel), and the hold waits for `g` to return. This keeps the guard's approval its own: an owner can't evict `g`, or close `X`'s guarded session, or toggle modes, and then instruct `X` without a hold, or approve its own instruction.

This is "my agent runs in auto mode, but when someone else steers it their turn is less permissive": the session's actor picks `guarded`, or owners require it. A hold is deliberately coarse (the whole agent, in this space) because Policy can't see which turns a steer drove; the agent's human ends it by reviewing and releasing. The outside turn runs in the harness's stricter mode (R-24), and whatever the agent then does in the world waits for its human, who reviews and releases it. Picking `auto` lets outside steers run as freely as the human's own. What a held agent writes in a `message` or `task.update` is data to its readers (R-24), like anything read; those channels, other spaces, actions inside the session's own sandbox, and effects proposed before the hold are outside what Policy gates (§7.5).

---

## 7. Verification and trust

### 7.1 Who is trusted for what

Anyone may run a sequencer or a verifier. A space's policy decides which actors may act. Its first sequencer is named in its space id, and later ones by signed moves. The transport is trusted only to deliver bytes; every HAN fact is signed, chained and replayable.

### 7.2 Consistency

- **Within a space:** one total order (seq), verifiable by anyone who holds the records (R-14).
- **Across spaces:** no order. Links are content only (R-05).
- **Availability (a stated trade-off):** while a space's sequencer is unreachable, no HAN entry can be appended. Chat on the transport continues. This is the price of one answer to "was this authorized?".

### 7.3 Moves

The handshake can't use profile-space signals (G-04 lets those carry only notices), and the target B has no read rights in the space yet. It uses two things: signals in the space being moved, between its owners and its current home A, and a server-to-server call `host` from A to B (§8.3, han.ts `BindingCalls`).

1. **Request.** An owner signals in the space, `{type: "host-request", to: B}` (T-05). Only readers can signal (G-04), and A, as home, can read the space as `@:A`. A dials B over the generic binding, authenticated as `@:A` (`hello`), and calls `host {space, records, head}`, pushing the chain in batches. B verifies every record (R-14), fetching the profile spaces it needs, which are public. B answers `{have}` (the highest contiguous seq it holds) until it holds the chain through `head`, A's current head.
2. **Accept.** If B is willing, its answer to the call that completes the chain through `head` also carries `accept = {keyId, sig}`, with `sig = Ed25519(B's key, "han-accept-move-v1\n" ‖ space ‖ "\n" ‖ receiptHash(head))`; `keyId` names a key in B's `/.well-known/han`. If B is unwilling the call fails `denied`, and if the chain fails R-14 it fails `invalid`. A signals the owners in the space, `{type: "host-accept", to: B, head, keyId, sig}`; they read it as ordinary readers.
3. **Move.** The owner appends `space.move {to: B, accept}`. A verifies `accept` against its *current* head. If the head moved since B signed, the move is `invalid`, and the owner repeats from step 1. The move is denied while an effect is `claimed` (P-11). A orders it at seq `n`. That is A's last record until a later move names A again; from then on A answers `moved`.
4. **Continue.** A pushes the records after `head` through `n` to B with `host` (without `head`). B, holding the chain through `n` whose last record moves the space to B, orders the next entry at `n+1`, with `prev = receiptHash(n)`, signed by B, and with a time ≥ A's last (it waits if its clock is behind, R-03a). Until then B answers `not_found` for the space.

Verifiers accept records above `n` only when they are signed by B (R-02) and the move carries a valid `accept` (R-14).

### 7.4 When a sequencer dies

HAN v1 has **no forced takeover**. Any rule letting someone other than the home keep writing a space reopens double-execution and rollback attacks; three review rounds showed this. Instead:
- The log stays **verifiable** at every verifier, with its profiles and server keys (§4.3).
- Owners continue in a **fork** (`space.create {forkOf}`) on any willing sequencer. A fork carries **no authority** (P-13). Effects that were open in the dead space are, honestly, of unknown fate. Re-doing one is a new proposal that a human decides to make.
- Forced takeover is left to an extension, which must show that the model properties still hold.

### 7.5 Trust assumptions, and what each misbehaviour is caught by

| Misbehaviour | Caught by |
|---|---|
| Sequencer accepts a forbidden entry | verifier replay (R-14) |
| Sequencer shows different histories to different parties | head gossip (R-15) → equivocation proof (R-02) |
| A server signs a seq when it isn't the home there | R-14 → wrong-home proof (R-02) |
| Sequencer uses stale profiles (ignores a recent key revocation) | partly: monotonicity is checked (R-10); 300 s freshness is trusted |
| Sequencer clock jumps forward | R-03a steady clock + R-16 elapsed-time abandon; verifiers flag receipts dated in the future. A sequencer that *lies* about time is trusted for clock rate within 1%. |
| Sequencer serves a record as redacted without a `redact` entry | R-14: a redaction must carry its signed proof |
| Owner moves a space to a sequencer that never agreed | §7.3: the move needs the target's signed acceptance of the current head |
| A server adds a key to one of its users' profiles (impersonation) | not prevented: users trust their own server for account recovery. The added key is public in the profile space, so the user can see it. |
| A server's retired key is stolen and used to back-date receipts | partly: verifiers keep the keys and receipts they have seen, and a `compromised` mark stops new trust; receipts recorded before the mark stay trusted |
| Sequencer refuses or delays appends, or dies | not prevented; remedy is a move, or a fork (§7.4) |
| Transport or sequencer reads plaintext | not prevented by HAN; use the transport's end-to-end encryption for content |
| An agent host acts on text it read (another session's stream or context, a message) as instruction: prompt injection | not prevented by Policy, which can't see what a model read: R-24 obliges hosts to treat it as data and SHOULD run such turns guarded. Effects it then proposes are attributable to the agent and its operator. |
| A held agent acts in another space, or persuades another agent through a `message` | not prevented: holds are per space and messages are data. R-24 obliges hosts not to carry an outside steer across spaces and to treat messages as data; the operator answers for it. |
| An instruction delivered to an agent before it became guarded and already finished | not held (it is history). Instructions still open when it becomes guarded (an open task, an unfinished effect naming it) hold it at once. |
| A harness's "stricter mode" lets a risky local action through | not prevented: only effects are gated by HAN. Local actions within the session's sandbox are the harness's (§8.4). |
| The transport delivers a stream to someone without the right | Matrix binding: per-session, per-right rooms whose membership the sequencer keeps equal to `sessionAccess` (§8.2). With E2EE (RECOMMENDED) the sequencer's room user has no device and can't decrypt; without it the publisher waits until membership matches and the homeserver can read the items. A homeserver admin who adds devices or users is out of scope, as for chat. |

HAN v1 runs no consensus.

---

## 8. Transport bindings

### 8.1 What HAN requires of a transport

| Rule | Requirement |
|---|---|
| **T-01 Carry the log** | Deliver every appended `LogRecord` to the space's members who may read it (P-02), in any order. A binding that enforces P-02 by pull (Matrix, §8.2) MAY deliver only a notice of the record and let the reader fetch it with `read`. Verifiers restore order from the chain, so the transport needs no ordering or integrity of its own. |
| **T-02 Map identities** | A HAN `ActorId` is the transport user with the same ID. v1 supports transport IDs that fit the ActorId grammar (§2; e.g. Matrix IDs without ports or uppercase); others need an alias profile in a later version. HAN authorship comes only from HAN signatures (R-12), never from the transport's sender field. |
| **T-03 Carry files** | Store and fetch blob bytes named by `BlobRef` (with its `uri`). Every reader MUST verify `sha256` before use. Access control to the bytes is the transport's, and a HAN reference grants no access by itself. |
| **T-04 Invite** | Let owners bring members into the room; first contact is the transport's (e.g. Matrix invites). |
| **T-05 Signal** | Carry ephemeral messages: presence, typing, streamed agent replies, verifier gossip (heads and faults, R-15), and an owner's request for a move and the answer (§7.3). Signals never carry authority: anything that matters is an entry. |
| **T-06 Room ↔ space** | Bind each room to at most one HAN space, so members can find its sequencer. |
| **T-07 Session streams** | Carry each live session's `StreamItem`s (han.ts: output, status, context snapshots as blobs) to the actors whose `sessionAccess` grants watch, and context items only to those granted read. After a receipt removes an actor's right (a lost role, a closed session, a policy change), items the publisher sends once it has seen that receipt MUST NOT reach that actor. Publishers check `sessionAccess` at their latest receipt before each item. Streams carry no authority: a steer is a `session.input` (P-15), never a stream message. Status MAY be taken from the harness's terminal program-status reports (OSC 7501: idle, working, blocked, done, error) and is presence, never fact. |

### 8.2 Matrix binding

The room is a **notification and chat channel**; the log is served by the sequencer. This is how the binding keeps P-02: a Matrix room is visible to all its members and its history rules can't express "read now, or read what you saw while you held a role". So the room carries only what every member may see, and the content goes out through `GET /_han/v1/read`, which enforces P-02. The alternative, making room membership equal `canRead` at the head with `history_visibility: joined`, breaks on a late joiner (P-02 says a role held now reads every seq, `joined` hides what came before) and on a member removed from HAN, who keeps the whole timeline instead of the seqs they held a role for; it also forbids rooms wider than the log, such as a chat that includes guests who may not read it. A room MAY therefore hold people who can't read the log.

| HAN | Matrix |
|---|---|
| space | a room with state event `org.han.space` `{spaceId, sequencer}` (T-06), see "Trust anchor" |
| `append(entry, content)` | a member sends the event `org.han.entry` `{entry, content?}` to the room. The sequencer runs as an **application service** for its server. It sees the event, runs §4.1, and answers with an `org.han.receipt` event `{receipt}` related to it (T-01). A resend of the same entry gets the same receipt (R-01), so Matrix's at-least-once delivery is harmless. The submitted event shows `entry` and `content` to every member until the sequencer replaces it (see "Redaction"), so a space whose readers are fewer than its room members MUST have its entries appended over the generic binding (§8.3) instead. |
| the record (T-01) | the sequencer posts `org.han.receipt` and a **header-only** `org.han.entry` `{id, kind, author}`; nothing more, so no content or body reaches a reader P-02 excludes. The full `LogRecord` (header, body, content, receipt) is fetched with the HTTP read below, and a verifier (R-14) works from that. |
| refusal | for a `denied` or `invalid` entry, the sequencer sends `org.han.reject {id, code, reason}`, related to the entry's Matrix event. A client shows an entry as *pending* until it sees a receipt or a reject. `unavailable` and `limit` get no event: the entry stays pending and the client's R-06 resend retries. Interactive clients MAY append over the generic binding instead, which answers at once. |
| `read` | `GET /_han/v1/read` on the sequencer, see "HTTP read". |
| signals (T-05) | room-wide signals (faults, executor progress, a move request or answer) are ordinary room events `org.han.signal {body}` that carry no authority. Executor progress uses `{type: "progress", claim, step, text}`, verifier gossip `{type: "heads", receipts}` and `{type: "fault", proof}` (R-15). Private ones (notices, G-03) travel server to server over the generic binding (§8.3). |
| verifier | any client or server that can read the space. Clients SHOULD show unverified or faulty HAN state as such. |
| redaction (P-12) | see "Redaction". |
| files (T-03) | Matrix authenticated media (`mxc://` URIs in `BlobRef.uri`), with HAN hash checks |
| invites, typing, presence, chat (T-04, T-05) | native Matrix |
| session streams (T-07) | **two rooms per session**, a *watch room* and a *read room*, created by the sequencer's application-service user on the `session.open` receipt and linked from the space room by state event `org.han.session` `{session, watch, read}` (state key = the session id; `watch` and `read` are room IDs). Power levels: the service user 100, everyone else 0; `invite`, `kick`, `ban`, `redact` and `state_default` 100; event `org.han.stream` 50, and the session's actor 50, so only it can post items. `history_visibility: joined`, `join_rule: invite`, `guest_access: forbidden`. After every receipt the service user invites or kicks so that each room's membership equals the actors `sessionAccess` grants that right on that session, and on `session.close` kicks everyone. Items are `org.han.stream` events whose content is a `StreamItem` (a context item adds `file`, the Matrix `EncryptedFile` info of its blob): output and status to the watch room, `context` to the read room. Context blobs are encrypted attachments (key in the event), so an `mxc://` URI alone reveals nothing. See "End-to-end encryption" for how the publisher keeps an item from a removed actor. Live audio and screen share use MatrixRTC in the watch room. |
| HAN roles vs Matrix power levels | independent. HAN authority comes only from the HAN log. HAN never reads power levels. Rooms SHOULD invite the people HAN made members, but needn't. |

**Trust anchor.** A client takes the sequencer for a room from the state event `org.han.space` only if all hold: the event was sent by the `sequencer` user it names; that user is `@han-seq:<server>`, with `<server>` the server named in the space id (the **first** home), a user in the exclusive namespace of that server's application service, so nobody else can act as it; and the room has no earlier binding (a room keeps its first binding). After a `space.move` the sequencer for the room is `@han-seq:<home>` for the home at that seq (R-02), which a client learns from the verified chain, not from the room state. A client honours receipts, rejects and `org.han.session` only from that user. For an entry sent to a room that has no binding yet, the client accepts a reject from the user expected for the entry's space. A forged binding, receipt or reject from a member is ignored.

**Size.** A Matrix event is limited to 64 KiB (65,536 bytes of event JSON), but HAN allows a header of 16 KiB plus content of 64 KiB. An entry whose `org.han.entry` event would exceed the cap MUST be appended over the generic binding (§8.3), or its bulk moved into a blob referenced from small content (§5). A client that can't send it fails it at once, locally, as `invalid`.

**Redaction.** A Matrix redaction strips a whole event, including the identifiers R-04 keeps. So on a `redact` receipt the sequencer first posts a header-only `org.han.entry` `{id, kind, author}` for each event that carried the target, then redacts the original events. Clients rebuild `redactedBy` from the `redact` record in the log (HTTP read). The room never carries the proof.

**HTTP read.** The sequencer serves two endpoints at `https://<server>/_han/v1/`, `<server>` being the home's name (`dialed`):
- `GET challenge` answers a `Challenge` (han.ts): `{challenge, server, versions, time}`. A challenge is single-use and valid for 120 s.
- `GET read?space=<SpaceId>&after=<Seq>[&state=true][&at=<Seq>][&limit=<n>]` answers `{items: ReadItem[]}`, with the semantics of `read` (§3) without `follow` and `knownHeads`; the same R-15 head exchange is available over the generic binding. The request carries `Authorization: HAN <actor> <keyId> <proof>` and `HAN-Challenge: <challenge>`. The `HAN-Challenge` header **names the challenge** that was signed (it is mandatory), `proof = Ed25519(key, "han-hello-v1\n" ‖ challenge ‖ "\n" ‖ <server>)` exactly as in `hello`, and the key is the actor's key `keyId` in its profile (R-12). A missing, used, expired or unknown challenge, or a bad proof, is `denied`. An error is `{error: WireError}` with the HTTP status 400 `invalid`, 401 `denied`, 404 `not_found`, 421 `moved` (with `movedTo`), 429 `limit` and 503 `unavailable` (`retryAfterMs` as `Retry-After` and in the body). A reader who may read no seq gets 404, as in §3. P-02 applies to every record and to `state`.

**End-to-end encryption** is RECOMMENDED for chat and for the session rooms. In session rooms the publishing host then encrypts each item only to the devices of actors `sessionAccess` grants at its latest receipt, and rotates its outbound Megolm session whenever a receipt removes any right, before sending again; the service user has no device, so it can't decrypt. Stock SDKs share room keys with every joined device, so a conforming host needs a key-sharing hook that filters recipients. **Without E2EE** the homeserver and any lagging member can read an item, so the publisher MUST NOT send an item until the room's membership matches `sessionAccess` at its latest receipt (it reads the head state over the HTTP read and waits for the kicks to land), and an unreachable service blocks publishing. Removal stops future items and keys; what someone already received stays received. The space room's `org.han.*` events can be encrypted only if the application-service user can decrypt them; otherwise the homeserver can read submitted entries.

### 8.3 The generic binding

For non-Matrix use and testing, a server MAY offer the **generic binding**: one authenticated WebSocket carrying the three calls (§3) plus the binding's seven operations (M7, han.ts `TransportBinding`) plus the server-to-server `host` (§7.3) encoded per connection: the connection authenticates `as`/`from`, `subscribe` is called `follow` in ref/wire.py, and `blobPut(bytes)` is sent as parts `blobPut(ref, offset, bytes)` of at most `maxBlobPartBytes`. `publish` is the home's own fan-out. [ref/wire.py](ref/wire.py) implements it. Servers talk to each other (head gossip, notices, blob transfer, `host`) over this binding whatever transport their users have, so a sequencer on the Matrix binding offers it too. Its rules realize T-01…T-05 and T-07:

**G-01 Access follows references; supply follows authors.** A record *references* a blob if its unredacted content holds the `BlobRef` at:
- `content.blobs[]`, for **every** kind, extensions included; or
- `effect.propose.plan.argsBlob`; or
- `effect.outcome.evidence` or `effect.resolve.evidence`.

A **context blob** is the `blob` of a `context` StreamItem (T-07) that a session's actor streamed in `space` (G-05).

`blobGet(ref, space)` succeeds only if both of these hold:
1. **Access:** a record in `space` that the caller may read (P-02) references `ref`; or `ref` is a context blob of a live session of `space` on which `sessionAccess` grants the caller `read` (P-15). Access to a context blob ends when the session is dead or the caller's read right is removed.
2. **Supply:** the server holds the bytes *for that space*. That is true when the referencing entry's author uploaded them to this server (for a context blob: the session's actor), or when the server fetched them for that space under G-02. An upload counts only if **every byte came from that uploader's own `blobPut` calls** and those bytes hash-match. A server MUST NOT report `complete` to an uploader just because the hash is already stored. Storage may be deduplicated by hash, but access never is. Knowing a hash, and mentioning it in a space you control, gets you nothing. That's why a hash proves bytes are equal but never permission, authorship or truth.

Uploaders may read their own unreferenced blobs for 24 h. A server MUST keep a blob while any unredacted record it stores references it, and MAY delete it after.

**G-02 Blob transfer.** A client uploads to its own server (`blobPut` as itself; the server may apply quotas). A server that needs a blob it lacks calls `blobGet` as `@:server` in the context of the referencing space. It asks the author's server first, then the home, then any mirror. G-01 is evaluated for the server actor (P-02 server reads). The receiver verifies the hash, moves on to the next server on a mismatch, and from then on holds the bytes for that space.

**G-03 Notices (first contact).** After ordering a `member.set` that gives an actor on server `S` a role, or an entry whose `to` names an actor that holds a role, the home MUST send `signal(profile:<actor>, {type: "notice", space, seq})` to `S`. `S` delivers it **only to that actor's own authenticated sessions**, as signal items on the actor's follow of its own profile space, and never to the profile space's other readers. A notice is only a hint: `S` verifies it by reading the space as `@:S` (P-02 server reads), and drops notices it can't verify. This is how a new member, human or agent, learns that a space exists.

**G-04 Who may signal.** Only actors who may read a space at its head (P-02) may `signal` it or `follow` it for signals. **Across servers:** a user's server relays the user's signal to the home over its own `@:server` connection. The home accepts a relayed `from` only for actors on that server. The home sends signals to other servers' follow streams, and each server fans them out to its local followers. **Profile spaces** are readable by anyone, but only the actor itself may follow its profile for signals, and the only signals they accept are G-03 notices from server actors. The move handshake (§7.3) is therefore not carried on profile spaces: its signals travel in the moved space, among readers (the owners, and the home as `@:A`), and its server-to-server part is the binding call `host`, from the home to the target, authenticated as `@:A`. On the wire a relayed signal carries `from` (the user it speaks for) beside the space and body; a client's own signal omits it, and the connection's authenticated actor is the sender. Servers rate-limit signals per actor and drop them under load.

**G-05 Session streams.** `stream(item)` is accepted only from the session's actor, on a live session, with `n` one more than its last item. `watch(space, session, as)` delivers the session's `StreamItem`s (han.ts) to `as` while `sessionAccess` grants it watch, and items of type `context` while it grants read. The home evaluates access at the head when it delivers each item, so nothing published after a receipt that removed a right reaches the actor that lost it. The blob of a `context` item is then readable by `blobGet(ref, space)` for every actor who has `read` on that live session (G-01); the supply is the session's actor's own upload.

**G-06 Move handover.** `host {space, records, head?}` (han.ts `BindingCalls`) is accepted only on a connection authenticated as a server actor `@:A` that is the home of `space` at the last record the target holds (R-02), and only with records contiguous from the target's `have + 1`. The target verifies them (R-14), answers `{have, accept?}` (§7.3), and is not yet a home: it serves no reads of the space and orders nothing until it holds a `space.move` to itself. A `host` call that can't be verified is `invalid`; one the target won't take is `denied`; rate limits apply (`limit`).

### 8.4 Profile: Claude Code as a session (informative)

How a harness maps onto P-15 and R-24, with Claude Code as the example. The *host* is a small process holding the agent's HAN key, which runs Claude Code through the Claude Agent SDK in streaming-input mode, so it controls every turn.
- **Open.** The host appends `session.open {steerMode}` as the agent and publishes `output` items from the SDK's message stream and `status` items from its turn state (and from OSC 7501 program-status reports where the harness emits them).
- **Context.** On request, or periodically, it publishes a `context` item: transcript and plan as an encrypted blob.
- **Steer in.** A `session.input` with a receipt becomes a user turn. For an outside input in `guarded` mode, the host first sets the SDK permission mode to `default` (instead of the session's `acceptEdits` or `bypassPermissions`), and its tool gate decides each tool call during that turn. The host MAY first show the input to the agent's human. Per R-24, while the agent is guarded every turn driven by someone else's instruction (steer, task, effect naming it) uses the stricter mode, and while it is held every turn does.
- **Seeing every tool call.** `canUseTool` is called only for calls the CLI itself would ask about: reads inside the working directory, and edits there in `acceptEdits`, never reach it. A gate built on `canUseTool` alone therefore can't veto them. The host MUST see every tool call with a `PreToolUse` hook (or an equivalent that fires for all calls) and decide there, and MAY keep `canUseTool` as a second check. Allowing everything inside the working directory is then the gate's decision, not a side effect of the CLI's rules.
- **World actions.** The host starts the SDK with `settingSources: []` (so no project or user settings add allow rules) and gives it **no allow rules** while guarded or held. While held, a call that would reach the world becomes `effect.propose`, and the model is answered at once, as a denial of that call carrying the text "proposed as <id>, waiting for <guard>" (the only way to answer a call without running it; no blocking, so no hook timeout). The id is real: the host signs the proposal when it answers, and appends it once the turn is journaled. A refusal (no executor, over budget) is answered "refused: …". `WebFetch` and `WebSearch` are world reads that can leak context through URLs, so they are proposed as effects too; MCP servers other than HAN's own are disabled for those turns. Edits inside the session's workspace stay local. Pushes, deploys, payments and outside messages reach the world only through an executor, as effects. The host, outside the session, holds the agent's HAN key and Matrix credentials and talks to the homeserver, sequencer and executor; nothing inside the session can post to any room. In a budgeted space the host's proposals carry `cost` and name the agent's assigned budgeted task (P-05, P-06), or Policy denies them. Dependencies are fetched by the host before the turn (or as effects), since even a read-only mirror's request URLs can leak context.
- **Sandbox.** Where an OS sandbox exists (the SDK's `sandbox` is macOS and Linux only), the session runs in it with network egress limited to the model API and the host, and it must cover every process the session can start, not only the shell tool. **Without one (Windows), `guarded` is advisory**: a shell can do what the gate's patterns miss. The host MUST then say so in its `status` items, for example `status.detail = "guarded: advisory (no sandbox)"`, and a client SHOULD show it.
- **Read others.** An MCP tool lets the agent `watch` sessions it has the read right on. The host runs turns that used it as if guarded (R-24).

---

## 9. Depth ledger

| Module | Interface | Replaces | Hidden concerns |
|---|---|---|---|
| Log (sequencer) | 2 calls | v0.4 CommandJournal (6), EventJournal (3), SubmissionBindingStore (4), WorkflowLedger, handlers, cursors | 13 |
| Policy | 5 pure functions | AuthorizationService (5), AuthorityService (4), approval, budget and grant records | 10 |
| Identity | 2 calls | IdentityDirectory (3), directory.resolve | 8 |
| Verifier | 1 call | v0.4 had none: authority wasn't verifiable by others | 7 |
| Executor (actor) | 1 call | ExecutionAdapter (7), WorkspaceProvider (10), CheckpointCoordinator executor side, ExecutionPermit | 12 |
| Agent host (actor) | 1 call | AgentRuntimeAdapter (11) | 5 |
| Transport binding | 7 calls | TransportAdapter, presence, live views, artifact transfer, collaboration (now the transport's) | 7 |
| **Total** | **19** | ~85 port methods; 26 wire methods; 19 commands | |

---

## 10. Verification: every rule and its check

Run `python ref/run_all.py`. Results: [ref/RESULTS.md](ref/RESULTS.md). `python ref/list_tests.py` lists the implementation's tests and the clean-room interop scenarios with the rule tags in their docstrings (`--rules`: rule to tests); the impl rows below are checked against it.

| Tool | What it does |
|---|---|
| `ref/policy.py` | executable reference of §6 and the R-09 body schemas |
| `ref/vectors/` + `run_vectors.py` | 133 vectors, 1370 steps (including the §1.1 worked example); expectations hand-written from this document; every step also checks purity and determinism |
| `ref/mutate_vectors.py` | 175 single-rule Policy mutations; the vectors must catch each one |
| `ref/fuzz_fold.py` | 2,000 random histories in populated rooms: `decide`/`fold` never fail on an allowed entry, stay pure, keep budgets in bounds, never change a final phase |
| `ref/wire.py` + `run_wire.py` | the generic binding with real Ed25519: reference sequencer (the §4.1 pipeline, with a steady clock for R-16, P-01 pinning, `read` with `at` and `limit`, context blobs) and verifier (R-14), gossip signals (R-15); 41 scenarios |
| `ref/mutate_wire.py` | 38 single-rule mutations of the sequencer and verifier; the scenarios must catch each one |
| `ref/model_check.py` | exhaustive bounded exploration driving the real `policy.py` in three scenarios (timing, drift, single); 10 mutations must each give a counterexample; a non-vacuity guard fails runs that never launch |
| `ref/model_guard.py` | exhaustive bounded exploration of P-15 against the real `policy.py`, depth 5 (5.2M transitions, ~10 min; `--quick`: depth 4). Its ghost is written from the invariant sentence, but is only **partially independent** of Policy: it tracks per agent whether it chose to be guarded and whether it is held, yet it reads Policy's state (roles, sessions, open tasks and effects) to know who holds a role and which instructions are open, it copies Policy's settle logic (outside work newer than the last release holds), and it models every release with `seen` = head, so stale releases are checked only by vectors. A flaw that ghost and Policy share would pass. Two agents with different operators, both able to become guarded (one later, with outside instructions pending); instructions by steer into any session, by tasks with several assignees, and by naming an executor; owners steer, close sessions, evict a guard and toggle modes; proposals made before and during holds; releases by the guard, another operator and an owner; key revocation; the approval rule. Properties GuardedClaimsNeedTheirHuman, HeldInstructNoOne, NeverLooser, GuardRightsNeedRole, OneOpenApprovalPerAuthor and ExactHolds (Policy holds exactly whom the invariant holds, never fewer, never more); 27 mutations, each caught by a property at depth 4; a non-vacuity guard |
| `ref/list_tests.py` | lists `impl/tests/*.py` (133 collected, one of them a live test skipped by default) and the `cleanroom-ts/interop.ts` scenarios (25) with their rule tags; reads source only |
| `ref/check_consistency.py` | §5 = han.ts = ref schemas; every folded state conforms to han.ts `SpaceState` |

| Rule | Checked by | |
|---|---|---|
| R-01 dedupe first | wire "dedupe precedes every check"; wire mutation; model | ✓ |
| R-02 one chain, proofs | wire "valid chain", "reordering", "equivocation proof", "move then old-home receipt", "moving back to a former home", "first home is pinned"; impl `test_wire_scenarios.py::test_equivocation_and_wrong_home_proofs`, `test_verify_faults.py::test_wrong_home_receipt`; wire mutations | ✓ |
| R-03 durable before receipt | impl: `test_cli.py::test_han_seq_process_and_restart` (kill -9 of the `han-seq` process after an acknowledged append; on restart the chain continues and verifies), `test_outbox.py::test_pending_survives_server_restart`. The store is SQLite WAL with `synchronous=FULL`. **Not tested:** a power cut (or any real disk fault), `replicated` durability (the implementation answers `unavailable` for it, R-08) and the loss of one machine. The procedure for those stays: cut the power to the disk after each acknowledged append; every receipted record must be present. | partial |
| R-03a receipt time | wire "receipt time non-decreasing", "a new home with a slow clock waits", "future-dated receipts: one 60 s tolerance"; model (offsets, rounding, drift); impl `test_clock.py` (restart with a clock behind, a wall-clock step ignored, mirror checks non-decrease), `test_verify_faults.py::test_future_dated_receipt`; wire mutations | ✓ |
| R-04 redaction proofs | wire "content binding and redaction" (including a faked redaction); vectors `redact`, `no-redacting-live-plans`; wire mutation | ✓ |
| R-05 references | vectors `refs-and-seen`, `missing-references-invalid`; mutations | ✓ |
| R-06 / R-07 retry and outbox | model: the executor's journal is its outbox, and claim resends are idempotent. **Procedure** for agent hosts: crash between sign and send, and between send and receipt, then check that exactly one entry exists. | partial |
| R-08 no silent downgrade | wire "no silent downgrade", "minDurability can't lock a space"; wire mutations | ✓ |
| R-09 closed shapes | vectors `closed-bodies`, `extension-kinds`, `impossible-dates`; wire "header is exact", "content is validated"; consistency check; mutations | ✓ |
| R-10 profile pin monotonic | wire "profile pin monotonic"; wire mutation | ✓ |
| R-11 determinism | every vector step and every fuzz step (purity + repeat); wire "forbidden accept caught by replay" | ✓ |
| R-12 identity is keys | vectors `revoked-author-key`, `E2-proposer-revoked`, `E3-approver-key-revoked`, `keyid-single-use` | ✓ |
| R-14 verifiers check everything | wire "tampered body", "forbidden accept", "first home is pinned", "server key validity window", "content is validated", "moves need the target's acceptance"; wire mutations | ✓ |
| R-15 head gossip | wire "head gossip finds a fork, privately", "gossip signals: heads compare, faults are adopted only if they check"; impl `test_verify_faults.py::test_equivocation_found_by_head_gossip`, `test_e2e.py::test_head_gossip_only_for_readable`; wire mutations; the 10-minute schedule is not exercised | ✓ |
| R-16 abandon needs elapsed time | wire "an early abandon is unavailable, with retryAfterMs" (a wall-clock step does not shorten the wait; a non-owner is `denied`; the executor's own report is not delayed), "an abandon after a restart waits a full A again" and "an abandon that could never be allowed gets Policy's verdict now" (a settled claim is `denied`, an unknown claim `invalid`, not `unavailable`); impl `test_clock.py::test_r16_abandon_waits_a_full_window`, `test_r16_after_restart_waits_a_full_window_again`, `test_r16_executor_outcome_is_not_delayed`, `test_r16_error_code_is_unavailable_with_retry_after`; model (drift scenario); wire mutations. A failover to another machine is the restart case and is not tested separately. | ✓ |
| R-17 / R-18 agent inputs, recovery | impl `test_host_recovery.py::test_killed_mid_turn_proposes_exactly_once` (host killed mid-turn, restarted: exactly one proposal, `seen`/`refs` name the instruction), `test_restart_does_not_replay_old_steers`; `test_host_p15.py` asserts `seen`/`refs` of the agent's proposals (R-17). **Partial:** the recovery tests drive a `ScriptedAgent`; `ClaudeCodeAgent` has one live run (`test_host_claude_live.py`, skipped unless `HAN_LIVE=1`), which shows a guarded turn and a proposal but not crash recovery or `seen`/`refs` reconstruction. | partial |
| R-25 validation boundary | vector `decide-ignores-log-fields` (Policy reads no `nonce`, header `time`, `contentHash` or `sig`); every vector stubs what the Log owns (ref/README.md); wire "header is exact", "content is validated", "seen and refs are Policy's verdict alone" (the Log does not check them; Policy answers `invalid`, and a mirror's replay gives the same verdict); mutation | ✓ |
| P-01 | vectors `relevant-actors`, `relevant-actors-order`, `open-approvals-capped`; wire "approvers are pinned up to the first valid key" (an unreachable later approver does not block while an earlier candidate's key is valid; it does when every earlier key is revoked); wire mutation; impl `test_federation_trust.py` (pins of a remote profile) | ✓ |
| P-02 | vectors `read-rights`, `role-history-bounds`, `read-public`; wire "state needs read rights at head", "state at a past seq" (`at` needs rights at `at`, not at the head), "a role held now reads the state at an earlier seq" (a member who joined at seq 3 reads the state at seq 2); impl `test_e2e.py::test_state_and_former_member`; mutations | ✓ |
| P-03 | vectors `membership-rules`, `open-join-and-ban`, `agents-*`, `server-deny`, `denied-server-users-can-be-removed`, `operator-withdrawal-stops-agent`, `operator-withdrawn-before-claim`, `owners-are-human`, `agents-off-stops-agents`, `agent-check-only-on-grant`, `profile-space-membership`; mutations | ✓ |
| P-04 | vectors `tasks-and-delegation`, `banned-loses-task-authority`, `owner-force-closes-task`, `cancel-by-past-approvers`, `small-p04-rules`; mutations | ✓ |
| P-05, P-06 | vectors `task-budgets`, `budgeted-child-of-unbudgeted-task`, `E6-*`, `budget-*`, `agent-*`, `no-closing-over-running-effects`, `owner-assignee-still-waits`, `owners-may-charge-any-task`, `refunds-skip-closed-tasks`, `reservations-block-budget-changes`; fuzz; mutations | ✓ |
| P-07 E1–E6 | vectors `E1`…`E6`, `banned-executor`, `proposal-expiry`, `launch-window-is-policy`, `closed-tasks-stop-their-effects`; mutations; | ✓ |
| P-08 | vectors `E3-*`, `approval-rule-removed`, `open-approvals-capped`, `approver-roles-are-exact`, `approval-time-boundaries`, `approve-phases-and-resolve`; mutations | ✓ |
| P-09 | vectors `outcome-rules`, `result-started-table`, `repeatable-*`, `window-pinned-in-claim`, `abandon-never-rereadies`, `abandon-never-rearms`, `repeatable-rearm-needs-approval`, `outcome-cost-codes`; model NoLaunchAfterAbandon and its five timing mutations; margin mutations | ✓ |
| P-10 | vectors `outcome-rules`, `resolve-cost-rules`, `approve-phases-and-resolve`, `outcome-cost-codes`; model UnknownNeverSilentlyRetried | ✓ |
| P-11 | vectors `checkpoint-move-policy`, `no-move-while-claimed`, `approval-rule-removed`, `budget-unit-locked-while-reserved`, `cant-deny-own-home`, `owner-servers-rule`, `small-p04-rules` | ✓ |
| P-12, P-13 | vectors `redact`, `redact-needs-no-role`, `no-redacting-live-plans`, `fork-fresh-authority`; mutations | ✓ |
| P-16 | vectors `references-must-be-the-right-kind`, `decision-order`, `kind-check-order`, `create-once`, `keyid-single-use`, `outcome-cost-codes`, `refs-and-seen`, `profile-space-membership`; one mutation per code and per order the vectors pin | ✓ |
| P-15 | vectors `session-rights`, `sessions-nobody`, `session-policy-shape`, `guarded-steer-holds`, `held-agents-instruct-no-one`, `only-release-releases`, `modes-apply-to-open-sessions`, `owners-only-tighten-modes`, `guard-rights-need-a-role`, `held-may-cancel-its-delegations`, `two-operators`, `guard-takes-the-wheel`, `guards-are-checked`, `guarded-needs-an-operator`, `open-approvals-capped`, `expired-approvals-crowd-no-one`, `nobody-stands-in-for-a-guard`, `held-executors-need-their-guard`, `tasks-hold-guarded-agents`, `side-door-sessions-are-covered`, `guarded-is-sticky`, `modes-toggle-cannot-unguard`, `only-the-agent-unguards-itself`, `naming-a-guarded-executor-holds-it`, `returned-approvals-stay-superseded`, `owner-forced-guarding-covers-every-instruction`, `held-proposers-need-their-guard-for-older-proposals`, `agents-can-always-unguard`, `guarding-holds-for-open-outside-instructions`, `tasks-hold-every-guarded-assignee`; policy mutations; guard model (depth 5) with six properties and 27 mutations; the fuzzer | ✓ |
| R-24, T-07, G-05, §8.4 | impl `test_host_p15.py`: a member steers a `guarded` agent, which is held and whose proposal waits for its human; an `auto` agent runs auto; a held agent can't delegate and its report is data; the guard's own steer is not outside input; a task naming a guarded agent is an outside turn; stream text and messages never drive a turn; watchers and context only for readers; a release ends the hold and a stale one is refused. `test_streams.py`: a removed watcher gets nothing after the receipt, a policy change revokes context, only the session's actor publishes, in order. `test_cli_e2e.py::test_session_steer_hold_and_release`. Wire "context blobs follow the session's read right" (G-01, G-05). `test_host_claude_gate.py` (12 tests with a fake client) covers the tool gate. **Partial:** the host tests use a `ScriptedAgent`; `ClaudeCodeAgent` has one live run (`test_host_claude_live.py`, skipped by default), so the §8.4 mapping is exercised once against the real SDK and not in CI; the implementation's gate sits on `canUseTool`, which does not see every call (§8.4 requires a `PreToolUse` hook), and there is no OS sandbox on Windows, so `guarded` there is advisory and the host does not yet say so in `status`; the Matrix session rooms are tested on a real Synapse without E2EE (the "wait until membership matches" path, `test_matrix_streams.py`), so Megolm key sharing and rotation are untested. | partial |
| §4.3 identity rules | vectors `profile-space`, `keyid-single-use`, `operator-must-be-another-human`; mutations | ✓ |
| X-01 | vector `E1-executor-instance` | ✓ |
| X-02 … X-06, X-10 | model, last full run during round 18, before later edits to `policy.py` ([ref/logs/model_check-r17.log](ref/logs/model_check-r17.log)): timing (1,555,218 states), drift (246,464), single (1,464,163), all properties hold; 10/10 mutations caught. Properties AtMostOneLaunchPerClaim, NoLaunchWithoutReceipt, NoLaunchAfterAbandon, Honest*; the model's executor follows X-10 when its outcome is refused. Impl: `test_exec_crash.py` (a crash after the receipt, before the send, after the launch, and a process killed after `launching`: no claim launches twice), `test_exec_basic.py` (`test_deadline_refuses_late_launch`, `test_deadline_boundary`, `test_abandon_then_resolve`). | ✓ |
| X-09 | the plan-digest check: wire "content is validated" (sequencer and verifier) | partial |
| X-11 | impl `test_exec_backoff.py::test_backoff_doubles_and_is_capped` (the back-off doubles and is capped) and `test_unsupported_and_unsafe_effects_are_declined_once` (an unsupported or unsafe effect is declined once, with a `message` to the proposer, and never claimed). The MUST, never claiming an effect it won't run, is also the X-09 check. | ✓ |
| X-07 | impl `test_exec_crash.py::test_second_instance_refuses_to_start` (a second process or object with the same instance key refuses to start, also through the CLI), `test_key_without_its_journal_refuses` (a used key without its journal, or with another journal, is refused). **Partial:** a key counts as used by a marker file beside it (`.used`), so a key copied to a new machine without that file is not detected; there is no custody, hardware key or attestation. | partial |
| T-01 … T-06 | the generic binding exercises T-01 and T-05 in wire scenarios ("signals ride follow streams", "notices stay private", "first contact across servers", "blob refs per kind and transfer", "blob theft by hash is refused") and impl `test_wire_scenarios.py` (notices, blob access and parts, signal rights). The Matrix binding is implemented (impl/han/matrix) and tested on a real Synapse v1.120.2 in a container, without E2EE: `test_matrix_basic.py` (a room bound to a space, authorship by signature not sender, forged reject/receipt/binding ignored, resend gets the same receipt, the HTTP read needs HAN auth and read rights, redaction reaches Matrix, an oversized entry is refused, outbox and restart, records ordered while Matrix was down are published), `test_matrix_effects.py`, `test_matrix_streams.py`. Federation (two servers, profile pinning and caching, notices, blob transfer, signals across servers, peer key faults, verifier on another server) is tested by `test_federation_core.py` and `test_federation_trust.py`. **Not tested:** the §8.2 rewrite of this round (header-only room events, the trust-anchor and size rules as now written) until impl follows it; to-device or move signals; Matrix media for entry blobs. | partial |

**Not covered** (partial or none above): durability under a power cut and `replicated` durability; any CI run of a real model-driven agent (`ClaudeCodeAgent` has one live run, skipped by default) and any OS sandbox or egress limit for a session; key custody beyond a marker file; end-to-end encryption in the Matrix binding (tested without it); moves between servers (`space.move` with the §7.3 handshake) over real servers; the 10-minute gossip schedule. **What exists beyond the reference.** The implementation (impl/, 133 tests collected by pytest, one live test skipped by default) exercises the generic binding in-process and over localhost, federation between two servers, and the Matrix binding on a real Synapse. A clean-room TypeScript implementation (cleanroom-ts/, written from the spec alone) passes the vectors and its own 25 interop scenarios against itself. **The TypeScript and Python implementations have not yet been run against each other**; an interop harness is in progress in interop/. Until it passes, two independent implementations agreeing on the wire is a claim, not a result.

**Defects the checks found while the design was written:**
- **Round 2, model:** a deadline check made before the `launching` journal write.
- **Round 4, model:** a deadline taken from receipt time was unsafe under clock offset.
- **Round 4, fuzzer:** a budget cut lost track of committed money.
- **Round 5:** attacks on window changes, whole-second rounding and blob hashes were each reproduced as a failing mutation before the fix.
- **Round 6, model:** a margin covering drift on one side only (a mutation the drift scenario now catches).
- **Round 7, mutation testing caught a flaw in the model itself:** the exaggerated 20% drift bound had leaked into every scenario. That widened the margin so much that the clock-skew mutation went undetected. The bound is now applied only in the drift scenario.

---

## 11. Decisions and trade-offs

| Decision | Chosen | Rejected | Cost accepted |
|---|---|---|---|
| Scope | the agent-safety layer, over existing transports (Matrix first) | a full chat and federation network | HAN depends on a transport for delivery, identity mapping, files and invites |
| Ordering | one sequencer per space; hash-chained receipts; cooperative moves with signed acceptance | consensus, merged event graphs, forced takeover | no HAN appends while the sequencer is down; after it dies, work continues in a fork |
| Interface | 3 sequencer calls + typed entries | one RPC per feature (v0.4) | strict schemas carry the detail (R-09) |
| Authorization | pure functions over the log + pinned profiles | live policy service; transport power levels | no outside data in decisions |
| Effects | claim in the same log; launch at once or never; window pinned and stated in the claim; elapsed-time deadlines | attempt/lease registry; clock comparisons | a crash between claim and launch costs one retry |
| Accountability | humans own spaces; agents have confirmed human operators, re-checked at every action | agents as ordinary users | an agent can't administer a space |
| Budget | one budget, one unit, per space | multi-currency budgets | a second currency needs a second space |
| Redaction | permanent header, redactable content, signed proof | full deletion | header fields (who, when, which action) are never erasable |
| Guarded steering | a coarse, sticky per-agent hold that only the agent's operator ends; nobody stands in | per-turn tracking (impossible: Policy can't see what a model read); taint propagation (leaked in rounds 9–10); owner stand-ins (let an owner approve its own steer) | anyone who may steer it, task it or name it as executor can hold a guarded agent repeatedly; if its operator leaves for good, the agent stays held in that space (owners can remove it) |

## 12. Open questions

- Q1: Should the Matrix binding be proposed as a Matrix Spec Change (MSC) for `org.han.*` events?
- Q2: Can a sequencer be co-hosted by several servers without consensus? (This is the forced-takeover extension.)
- Q3: Spam and abuse limits for public rooms: left to the transport and to sequencer policy.

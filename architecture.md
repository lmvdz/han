# HAN v1 — architecture

Status: design draft, round 16. Companions: [goal.md](goal.md) (why), [han.ts](han.ts) (exact types; normative for shapes), [coverage.md](coverage.md) (fate of every v0.4 guarantee), [ref/](ref/) (executable reference, conformance vectors, fuzzer, model checker; run `python ref/run_all.py`).
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

**Grammar and limits** (also in han.ts). Exceeding any size limit is `invalid`. The header limit binds first, so the list caps below are upper bounds, not guarantees.

| Item | Rule |
|---|---|
| ActorId | `@` + name `[a-z0-9._-]{0,64}` + `:` + ServerName; the empty name is the server actor. On Matrix, a HAN actor *is* the Matrix user of the same ID. |
| ServerName | lowercase DNS name: labels `[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?`, dot-separated |
| SpaceId | `[a-z0-9]{1,26}` + `:` + ServerName (the space's **first sequencer**; the name `profile` is reserved), or `profile:` + ActorId |
| EntryId, Hex | `[0-9a-f]{64}` |
| nonce | `[a-z2-7]{26}` (128 bits, base32) |
| KeyId | `[A-Za-z0-9_-]{1,64}`; single-use per actor (§4.3) |
| Time | `YYYY-MM-DDTHH:MM:SSZ`: UTC, a real proleptic Gregorian date, seconds 00–59, no fraction. A sequencer writes `receipt.time` by **rounding its clock down** to the second. |
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
**R-03a Receipt time.** Within a space, `receipt.time` MUST NOT decrease, including across a move. A sequencer MUST derive receipt times from a clock that doesn't jump: a steady, slewed clock anchored at startup, never a wall clock that can be stepped. If its clock is behind the last receipt time (for example after a move), it MUST answer `unavailable` until it catches up. Every party's clock MUST keep its rate within 1% of true time. Verifiers check non-decrease, and flag a receipt time more than 60 s ahead of their own clock as a fault (`future-time`).
**R-04 Immutability and redaction.** Headers never change and are kept for the life of the space (no compaction, so every `after` stays readable and every verifier can replay from seq 1). `redact` (P-12) deletes the *content* wherever honoured and sets the record's `redacted` flag. A redacted record MUST carry `redactedBy`: the signed `redact` entry and its receipt, which verifiers check (R-14), so a sequencer can't silently erase content.
**R-05 Same-space references.** `refs` and every `EntryId` inside a `body` MUST name an entry already in the same space; otherwise the entry is `invalid`. Cross-space links are content only (`Link` in han.ts) and carry no authority.

---

## 3. The sequencer interface: three calls

Every HAN binding offers these three operations. A **generic binding** (§8.1) carries them over one WebSocket at `wss://<server>/han`. The **Matrix binding** (§8.2) carries them as room events.

| Call | Args → result | Retry |
|---|---|---|
| `hello` | `{who, keyId, proof, version, knownHeads?}` → `{limits, durability, time, knownHeads}`. `proof = Ed25519(key, "han-hello-v1\n" ‖ challenge ‖ "\n" ‖ dialed)`, where `dialed` is the server name the client connected to; the client aborts if the server's challenge names another. `knownHeads` (≤ 256) are answered only for spaces the asker named and may read (R-15). | new connection |
| `append` | `{entry, content?}` → `Receipt` | **always safe with the same entry** (R-01) |
| `read` | `{space, after, follow?, state?, limit?, knownHeads?}` → stream of `ReadItem`. First `{type:"state"}` if asked (it needs read rights at the head and omits the `entries` index); then `{type:"heads"}` if `knownHeads` was sent; then `{type:"record"}` in seq order, only for seqs the reader may read (P-02). `limit` (1–1000, default 1000) bounds the backlog. Without `follow` the stream then ends; with `follow` it continues with new records. A reader who may read no seq gets `not_found`, the same as for a missing space. | safe |

**Errors:** `denied` (policy or authentication; never retry the same entry) · `invalid` (malformed, bad signature, bad schema, over a size limit; never retry) · `not_found` (never) · `limit` (rate limit; retry after `retryAfterMs`) · `unavailable` (same request later) · `moved` (same request at `movedTo`).

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
| M5 | **Executor** (an actor) | `deliver(item)` | deciding what to claim, the claim/launch/expired journal and X-01…X-10, elapsed-time deadlines, plan checking, sandboxes, credentials, workspaces, native lookups, cancellation of running work, its outbox |
| M6 | **Agent host** (an actor) | `deliver(item)` | models, prompts, memory, the agent framework, turning thoughts into entries, its outbox |
| M7 | **Transport binding** | `publish(space, record)`<br>`subscribe(space, as) → stream of ReadItem`<br>`signal(space, from, body)`<br>`blobPut(bytes, as) → BlobRef`<br>`blobGet(ref, space, as)`<br>`stream(item)`<br>`watch(space, session, as) → stream of StreamItem` | everything T-01…T-07 require: carrying entries and receipts, invites and notices, signals, files, identity mapping, live session streams with per-right delivery |

Policy is five functions, but its interface carries no state and has no side effects: everything it hides is *rules*. The executor and agent host are **actors**. Each is built with a `Client` (the three calls plus the transport) and fed every record and signal of the spaces it follows, as `ReadItem`s. It answers only through its client. That's the same interface a human app implements.

### 4.1 M1 Log: the sequencer pipeline

On `append(entry, content)` the sequencer runs these steps. Steps 2–9 run in one serial transaction per space.

1. **Dedupe:** if `id` exists, return its receipt (R-01).
2. **Not home?** Answer `moved` with the current home. A `space.create` is accepted only by the sequencer named in its space id, which decides by its own rules whether to host it.
3. **Shape:** the header has exactly the `Entry` fields, each in its grammar (§2); the body matches the kind's closed schema (R-09); content is present iff `contentHash ≠ ""`, matches it, and matches the kind's content schema and checks (§5: `planDigest = sha256(JCS(plan))`, `plan.action = action`); sizes are within limits; `seen` ≤ head; references resolve (R-05). For a `space.move`, `accept` verifies (§7.3). Rate limits and block lists apply here.
4. **Profiles:** pin `Identity.profile(actor)` for the author and the other actors `relevantActors` names (P-01, R-10). If a required profile is unavailable → `unavailable`.
5. **Signature:** `Identity.verify(entry, profiles[author])`.
6. **Policy:** `Policy.decide(state, entry, ctx)` with `ctx = {time: now, profiles}`. `now` is the steady clock rounded down (R-03a).
7. Assign `seq`, `prev`, `time`, `durability`, and `profiles: {actor: profileSeq}`. Sign.
8. Make durable at the stated level. Then `Policy.fold`, which never fails for an allowed entry (`ref/fuzz_fold.py`).
9. Return the receipt, and publish the record on the transport (T-01).

**R-09 Closed shapes.** Unknown fields, wrong types, impossible dates or out-of-range values in a known kind's header, body or content are `invalid`.
**R-10 Profile freshness.** The profile seq a sequencer records for an actor MUST be ≥ every profile seq it has recorded for that actor in this space before. It MUST be fetched within 300 s of the receipt `time`, from the profile's sequencer or from a verifier of that profile space. Verifiers check monotonicity exactly. Freshness is a stated trust assumption (§7.5).
**R-16 Abandon needs elapsed time.** A sequencer MUST NOT order an owner's abandon (P-09) unless its own steady clock shows at least `A` seconds since it ordered the claim. This holds even if receipt times say otherwise, so a clock step can't shorten the wait. Moves are denied while a claim is open (P-11), so the same sequencer ordered both.

### 4.2 M2 Policy: pure functions

**R-11 Determinism.** Policy's five functions depend only on their arguments. `ctx.time` is the receipt time, and `ctx.profiles` are the profile views at the seqs pinned in the receipt. Every conforming implementation MUST give identical results; [ref/policy.py](ref/policy.py) is the reference and [ref/vectors/](ref/vectors/) the tests. Wall clocks, outside reputation and rate limits are *not* inputs. Rate limits and block lists live in Log (step 3) and can only refuse.

### 4.3 M3 Identity: identities are spaces too

- An actor ID is `@name:server`; on Matrix it is the Matrix user ID. HAN keys are separate from transport credentials. HAN never trusts a transport login as authorship (R-12).
- The actor's **profile space** `profile:@name:server` is sequenced by `server`'s HAN sequencer and is world-readable. Its `space.create` is appended by the server actor `@:server`, with `profile: {actorKind, operator?, key}` naming the first key. **Verifiers of any space MUST also verify, retain and serve the profile spaces of every actor pinned in its receipts**, so profiles outlive their server.
- `key.add` / `key.revoke` may be appended by the actor (with any un-revoked key) or by `@:server` (account recovery). **KeyIds are single-use**, so a revoked key never becomes valid again. A stolen key can revoke the others, and the server can add keys and so impersonate its users. Both are stated trade-offs (§7.5).
- **Agents and services** name an `operator`, who must be a different, **human** actor. The operator confirms with `member.set {actor: <operator>, role: "owner"}` in the agent's profile space, and withdraws with `role: "none"`; the server may also withdraw. An actor naming itself as operator is `invalid`.
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

On failure it stops trusting the space above the last good seq, tells the room (T-05) with `{type: "fault", proof: {kind: "equivocation" | "wrong-home" | "forbidden-accept" | "bad-signature" | "chain" | "bad-redaction" | "bad-move" | "future-time", receipts, record?}}`, and keeps the proof.
**R-15 Head gossip.** In `hello` (both directions) and in `read` (`knownHeads` argument; `{type:"heads"}` item), each side lists the spaces it asks about. The other side answers only for listed spaces the asker may read, as `@:asker` for a server (P-02). A mismatch that forms an equivocation or wrong-home proof is handled as an R-14 failure. A server verifier MUST exchange heads with at least one other verifier of each space it verifies, at least every 10 minutes.

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

**Why no launch follows an abandon.** Let `d` be the drift bound (1%). Define a clock's rate as clock seconds per true second, within `[1−d, 1+d]`. Each side measures only an interval on its own clock, so clock offsets cancel.
1. The executor launches at most `W` executor-seconds after the send. That is at most `W/(1−d)` true seconds.
2. The claim reaches the sequencer no earlier than it was sent. Its `receipt.time` is rounded down, so it is at most 1 s early.
3. An owner may abandon only at sequencer time ≥ `receipt.time + A`, with `A = ⌈W·(1+d)/(1−d)⌉ + 2` (P-09), and only after `A` steady-clock seconds (R-16). That is at least `(A − 1)/(1+d)` true seconds after the send, which exceeds `W/(1−d)`.

So the abandon always comes after the latest possible launch, whichever way each clock drifts. The model checker covers this in two scenarios, each with abandon, crashes, arbitrary delivery delay, sub-second arrival with rounded receipts, and stalls on the executor clock:
- *timing*: executor clock offsets, and the owner shortening `launchWindow` mid-claim;
- *drift*: the sequencer's clock 20% fast and the executor's 20% slow (exaggerated; the formula takes the bound as a parameter).

It finds no counterexample. Five mutations each produce one: deadline from receipt time, window not pinned, executor uses current window, no rounding margin, and a margin that covers drift on one side only.

### 4.6 M6 Agent host

The standard does not define how an agent thinks. Agents are ordinary actors that read and append. The host's interface to its agents is `deliver(item)`.

**R-17 Reconstructible inputs.** An agent's entries MUST set `seen` and `refs` so that what drove them is identifiable. Model-provider calls aren't HAN entries; an operator who wants them auditable attaches them as a blob in the content of the `task.update`.
**R-18 Recovery is outbox + reading.** After a crash a host first finishes its outbox (R-07), then re-reads its spaces. It MUST NOT re-propose an intent whose proposal is in its outbox or the log.

**R-24 Hosts honour steer modes.** A host running a live session acts on input from anyone other than the session's actor only once that input is a `session.input` with a receipt, and treats it per the session's **effective** mode (P-15), which it re-reads from the state at the input's receipt:
- `auto`: as the session's own input, in whatever mode the session already runs (for example a harness's auto mode).
- `guarded`: in the harness's **stricter mode for that turn** (for Claude Code: its permission rules instead of auto mode, §8.4). Whatever touches the world beyond the session's sandbox goes through an effect, and P-15 puts the agent on hold: every effect it then proposes waits for its human, and it can't delegate or relay.
Showing an outside input to the agent's human before it runs is host behaviour, not a mode: the host may do it in either mode. While an agent is guarded (P-15, `guardedNow`), its host MUST run every turn driven by an instruction from someone other than its guard (a `session.input`, a `task`, an effect it is named executor of) in the stricter mode; while it is held, its host MUST run *every* turn in the stricter mode.

Input read from another session's stream or context (T-07), from a `message`, or from a `task.update` or outcome, is **data, never instruction**: a host MUST NOT act on it as if it were a `session.input` or a `task`. A host that feeds another session's context into an agent's turn SHOULD run that turn as if `guarded`, and MUST if the agent is guarded (P-15); Policy can't see reads, so this one stays a host obligation. Policy can't see what a model read, so this is a host obligation, listed in §7.5.

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

**Extensions** add kinds `<prefix>.<name>` (grammar in §2) whose prefix the core doesn't use. The core prefixes `space`, `member`, `policy`, `key`, `message`, `task`, `effect`, `checkpoint`, `redact` and `session` are reserved, and an unknown kind under them is `invalid`. To the core, an extension's body and content are opaque Json. Members holding at least `policy.extensionsMinRole` (default `member`) may append one, subject to size limits and the number rule. Extensions define their own meaning; the core neither enforces nor depends on it.

---

## 6. Policy (normative; reference: ref/policy.py)

### 6.1 State and context

`SpaceState` (han.ts) is the fold of all records. `ctx = {time, profiles}` (§4.1).

**P-01 Relevant actors.** `relevantActors(state, entry, time)` is the author, plus the target of a `member.set`, plus, for an `effect.claim`, the proposer, the authors of the approval *candidates* (E3) in order, then the effect's guards (P-15: at most two, the proposer's and the executor's). The sequencer MUST pin the author, the target, the proposer and the guards. It pins candidates in order, up to and including the first whose signing key is valid. If it can't pin a candidate before that point, it answers `unavailable`. So an unreachable approver matters only if every earlier approver's key was revoked. At most 16 approvals are open per proposal, plus at most two guards (P-08, P-15), which bounds `receipt.profiles`.

### 6.2 Roles, joining, reading

Roles: `owner`, `member`, `guest`, `banned`, or none (absent). **Owners are human.** Only actors of kind `human` may hold `owner`; agents and services are members at most.

**P-02 Read.** `canRead(state, reader, seq)` is true if:
- `policy.visibility = "public"`; or
- `reader` is owner/member/guest now; or
- `reader` held such a role at `seq`, meaning former members keep what they saw; or
- `reader` is a server actor `@:S`, and either `S` is the space's current home, or `S` is not in `policy.servers.deny` and some actor on `S` holds or held a readable role at `seq`.

Profile spaces are public. The `state` item of `read` needs read rights at the head. On a transport, chat visibility is the transport's business, and HAN's read rights govern only the HAN log.

**P-03 Membership** (`member.set {actor, role}` by `author`, with target's current role `cur`):
- An **owner** may set any role, except that a set leaving zero owners is denied, and a non-human can't be made owner.
- **Self-leave:** anyone except a banned actor may set themselves to none, unless they are the last owner.
- **Open join:** if `join = "open"`, an actor with `cur` = none may set themselves to `member`. Banned actors may not.
- **Invite:** if `membersMayInvite`, a member may set an actor with `cur` = none to `guest` or `member`.
- **Agents:** for a target whose profile `actorKind ≠ "human"`, `allowAgents = "none"` denies, and `"with-operator"` requires `operatorConfirmed`. The same check, called *accountability*, is re-applied to a non-human author at every `effect.propose`, `effect.claim` and budgeted `task`, and to the *proposer* at claim time (E2). So withdrawing an operator's confirmation, or switching `allowAgents` to `"none"`, stops agents already in the space.
- **Servers:** *granting* a readable role to an actor on a denied server is denied. Banning or removing such an actor is always allowed.
- **Profile spaces:** owners are the actor and `@:server`. The only other allowed `member.set` is the operator's confirmation or withdrawal (§4.3).

**P-04 Who may append what** (beyond the kind-specific rules below). "Holding a role" means owner, member or guest *now*. Approvers must also hold one. Authors on a denied server may append nothing, except fact reports (`effect.outcome`, `effect.resolve`).

| Kind | Allowed authors |
|---|---|
| `message` | owner, member, guest |
| `task` | owner, member; or an assignee of `parent` who still holds a role |
| `task.update` | an assignee of `task` who still holds a role; or an owner, with a final state only (force-close) |
| `task.cancel` | the task's author, or an owner, holding a role |
| `effect.propose` | owner, member |
| `effect.approve` | an actor matching the action's approvers (P-08), or one of the effect's guards (P-15) |
| `effect.claim` | the named executor, still a member or owner and accountable (P-07) |
| `effect.outcome`, `effect.resolve` | the claim's instance or executor, **regardless of current role or server**: they report facts about work already authorized; owners as P-09/P-10 allow |
| `effect.cancel` | proposer, any approver of it, any of its guards, or an owner, holding a role |
| extension kinds | roles at or above `policy.extensionsMinRole` |
| `checkpoint` | owner, member |
| `redact` | the target's author, or an owner |
| `policy.set`, `space.move` | owner |
| `session.open` | any role holder, accountable (P-15) |
| `session.input` | holders of the session's steer right (P-15) |
| `session.close` | the session's actor, or an owner |
| `key.add`, `key.revoke` | profile spaces only (§4.3) |
| `space.create` | only at seq 1 |

### 6.3 Tasks and the budget

A space has at most **one budget**, `policy.budget = {unit, amount}`, or `null`. Amounts are integers in minor units. `budgetLeft` is the budget minus everything *committed* (reserved or spent). Amounts only move between the space, task budgets and claim reservations, or are spent at settlement. The fuzzer checks that task remainders never go negative, that no remainder exceeds its budget, and that the space remainder goes negative only through a cut.

**P-05 Tasks.**
- `to` is non-empty and every assignee currently holds a role.
- A `parent` must be an open task in this space, and the author must be one of its assignees. Top-level tasks have depth 0; a child's depth is its parent's + 1, which must be ≤ `maxDelegationDepth` (so 0 forbids delegation).
- **Task budgets** exist only in budgeted spaces and use the space's unit. A task's budget is reserved from its parent if the parent has a budget, otherwise from the space, and must fit what is left there. A child of a budgeted parent MUST carry a budget. An agent or service may create a budgeted task only as a child of an open budgeted task assigned to it.
- A final `task.update` (`done`, `failed`, `declined`, `cancelled`) closes the task and refunds its unspent remainder to the nearest *open* budgeted ancestor, or to the space if there is none. An assignee can't close a task while an effect naming it is `claimed` or `unknown`. An **owner** may close any open task (force-close), for example when its assignees are gone.
- On a closed task, these are refused: `task.update`, `task.cancel`, a child `task`, and an `effect.propose` that names it.
- `task.cancel` sets `cancelRequested`. The task stays open until an assignee, or an owner, posts a final update.

**P-06 Costs.**
- **Budgeted spaces are closed.** If `policy.budget` is set, every `effect.propose` MUST carry a `cost` in its unit, and a claim of an effect without one is denied (E6). An outcome or resolve may report a cost only within its claim's reservation. With no reservation, it may report a cost only in an unbudgeted space, where costs are informational.
- **Only humans draw on the space budget.** In a budgeted space, an agent or service MUST name, in every proposal, an open budgeted task assigned to it, and its costs are charged there. Agents can't be owners (§6.2), so they can't change the budget either.
- **Charging:** an effect's cost goes to its named task if that task has a budget, otherwise to the space. It is reserved at claim and settled at outcome or resolve. The difference is refunded by the P-05 refund rule.
- **Changing the budget** (`policy.set`): with the same unit, `budgetLeft` = new amount − committed, which can go negative after a cut. Nothing new can be reserved until refunds bring it back up. Changing the unit, or adding or removing the budget, is allowed only while nothing is reserved; the remainder then starts fresh.

### 6.4 The effect state machine

Phases: `awaiting-approval`, `ready`, `claimed`, `unknown`, and the final phases `succeeded`, `failed`, `cancelled`. `cancelRequested` is a flag. A final phase never changes.

**Transition table.** Any (phase × kind) combination not listed is `denied`.

| Phase | `effect.approve` | `effect.cancel` | `effect.claim` | `effect.outcome` | `effect.resolve` |
|---|---|---|---|---|---|
| awaiting-approval | → ready once the rule's and every guard's approvals are in hand (P-08, P-15), else stays | → cancelled | — | — | — |
| ready | stays ready (adds an approval) | → cancelled | → claimed (P-07) | — | — |
| claimed | stays claimed | sets `cancelRequested` (denied if already set) | — | per P-09 | — |
| unknown | — | sets `cancelRequested` (denied if already set) | — | — | per P-10 |
| final | — | — | — | — | — |

`effect.propose` → `awaiting-approval` if its action needs approval (P-08) or its proposer is held (P-15), else `ready`. An executor's hold (P-15) doesn't change the phase; it adds a guard that E3 checks at claim.

**P-07 Claim** (`effect.claim {proposal, attempt, instance, window}`). Allowed only when all of these hold:
- **E1** The author is the proposal's `executor`, still holds owner or member, is accountable (P-03), and `instance` is the valid keyId that signed this claim.
- **E2** The proposer still holds owner/member, is still accountable (P-03), and the key that signed the proposal is still valid in the proposer's pinned profile.
- **E3** If approval is needed, walk the **candidates**: unconsumed, undropped approvals in order whose `expiresAt` (if any) is after `ctx.time`, whose author satisfies the approvers rule *now*, and (with `separateApprover`) who aren't the proposer. The first candidate whose signing key is valid is consumed, and candidates with revoked keys are skipped. If none is valid, deny.
- **E4** `cancelRequested` is false. (Redundant with the table, kept so one rule states it.)
- **E5** The phase is `ready`; `attempt` = 1 + the number of earlier claims; `window` = the current `policy.launchWindow`; and the proposal's `expiresAt`, if any, is after `ctx.time`; and the task it names, if any, is still open.
- **E6** In a budgeted space, the effect has a cost in the space's unit that fits what is left where P-06 charges it. It is then reserved.

The claim records its receipt time and pins its `window`.

**P-08 Approvals.** A client MUST display the plan whose `planDigest` it is about to sign, and MUST NOT offer an approval for a plan it hasn't fetched and hash-checked. `policy.approvals` is a list of `{action, approvers}`. The first entry whose `action` equals the proposal's action, or is `"*"`, applies, and `approvers` is a non-empty list of roles (`owner`, `member`, `guest`) or actor IDs. If none applies, no approval is needed. With `separateApprover: true`, the proposer can't approve their own proposal. An approval for a proposal not in this space, or with a wrong `planDigest`, is `invalid`. An approval whose `expiresAt` has already passed is denied. A new approval **supersedes** its author's open ones on the same proposal (so approvals can be refreshed, and each author has at most one). At most 16 unexpired approvals by other authors may be open; a guard's approval (P-15) doesn't count, so others can't crowd it out. After a `policy.set`, an effect in `awaiting-approval` whose action no longer needs approval becomes `ready`.

**P-09 Outcome** (`effect.outcome {claim, result, started, cost?}` for the latest claim, at most once per claim):
- **Author:** normally the claim's instance, i.e. the same actor with `keyId = claim.instance`.
- **Abandon:** an owner may append it with `result: "unknown"` and `started: "unknown"` only if `ctx.time ≥ claim.time + A`, where `A = ⌈claim.window × 1.01 / 0.99⌉ + 2`, and the sequencer's steady clock agrees (R-16). For example, 30 s gives 33 s and 3600 s gives 3675 s. An abandon always leaves the effect `unknown`, even if it is repeatable, because it might still be running. An outcome counts as the executor's own report only when signed with the claim's `instance` key; any other key, even the executor's, is an abandon and leaves `unknown`.
- **`started: "no"`** (result `failed` or `cancelled`): the consumed approval and the cost reservation are returned. The phase becomes `cancelled` if `cancelRequested`, else `ready`.
- **`started: "yes"`:** cost settles at `cost`, which must be ≤ the reservation (default: the reservation), and the rest is refunded. `succeeded` → phase `succeeded`. `failed`, or `cancelled` (stopped partway) → phase `failed`; but if the effect is `repeatable` and no cancel is requested, → `ready` instead.
- **The executor's own `unknown`:** the reservation stays charged and the phase becomes `unknown`. If `repeatable` and no cancel is requested, the reservation is instead settled as spent in full and the phase becomes `ready`: the proposer declared that running it again is harmless.

Valid `(result, started)` pairs:

| result \ started | `yes` | `no` | `unknown` |
|---|---|---|---|
| `succeeded` | ✓ | — | — |
| `failed` | ✓ (ran and failed) | ✓ (never launched) | — |
| `cancelled` | ✓ (stopped partway) | ✓ (never launched) | — |
| `unknown` | ✓ (launched, result lost) | — | ✓ |

All other pairs are `invalid`. `cost` is allowed only with `started: "yes"`.

**P-10 Resolve** (`effect.resolve {proposal, result, cost?}` with evidence, in phase `unknown`):
- **Author:** the proposal's executor (any instance), or an owner.
- `succeeded` → phase `succeeded`, with cost settled as in P-09. `failed` → the reservation is refunded, and the phase becomes `cancelled` if `cancelRequested`, otherwise `awaiting-approval` or `ready` as for a fresh proposal, with no approvals carried.
- Resolving is a statement of fact, and the *only* way a non-repeatable effect with an uncertain past can run again. `unknown` is reached only by the executor's report, or by an abandon after the claim's window, so the claim can no longer launch. A resolve that is true when made therefore never leads to a second run (model property UnknownNeverSilentlyRetried, which counts only resolves already false when made).

### 6.5 Redaction, checkpoints, forks

**P-11 Kind-specific checks.**
- `checkpoint.through` ≤ the current head.
- `space.move` is denied while any effect is `claimed`, so one sequencer orders a claim and its abandon (R-16).
- `policy.set` replaces the whole policy, applying the P-06 budget rules and the P-08 release rule. It can't change the `window` pinned in existing claims. It can't deny the current home's server, or the servers of every owner. Roles are untouched, because roles live only in `member.set`.
- `space.create` is only valid at seq 1.

**P-12 Redact** (`redact {target}`): the target's content is deleted wherever honoured, its `redacted` flag set and its blob references released. A `redact` can't target another `redact`. The plan of an unfinished effect can't be redacted (cancel it first).

**P-13 Forks carry no authority.** A space with `forkOf {space, through}` starts from its own `space.create` alone: creator as owner, the new policy, and nothing carried over (no members, tasks, approvals, budget commitments or effects). The parent's records are history to read, never authority.

### 6.6 Live sessions and steering

The point of a space is to see and steer work outside your own silo while it happens, not after a PR. A **session** is one actor's live working context (a Claude Code run, an editor, a human at a terminal). Its stream travels on the transport (T-07); the log holds the authority over it.

**P-15 Session rights and steering.**
- **Open and close.** `session.open {steerMode, task?}` opens a session whose actor is the author: any role holder, accountable (P-03), at most 64 open per actor. `steerMode` (`auto` or `guarded`) must be in `policy.sessions.modes`. An agent's confirmed operator is recorded as the session's **guard**; an agent may open a `guarded` session only if it has one who holds a role here. A `task` must be one the author is assigned to (or the author is an owner). `session.close` ends it (its actor or an owner).
- **Effective mode, live.** If owners later remove `guarded` from `modes`, a guarded session is dead (owners can make open sessions stricter, never looser); if they remove `auto`, an auto session behaves as guarded. A session is **live** while open, its actor holds a role, it has an effective mode, and its actor is human, or it is effectively `auto`, or its guard holds a role. An agent is **guarded** in the space while any of its live sessions is effectively guarded.
- **Rights.** `policy.sessions = {watch, read, steer, modes}`. The first three are each a minimum role, `guest`, `member`, `owner` or `nobody`, and `read` and `steer` may be no wider than `watch`. `sessionAccess(state, actor, session)` returns three booleans: on a live session its actor has all three, and so does its guard while the guard holds a role; anyone else has a right if their role is at least its level and the level isn't `nobody`; a dead session grants nothing. **Watch** = the output and status stream; **read** = its full context (transcript, files, plan), also for agents, which is how one agent reads another's session; **steer** = append `session.input`. Owners set these per space; a network's defaults pre-fill them. Typical: team `{member, member, member}`, pair session `{guest, guest, guest}`, demo `{guest, owner, owner}`, guarded sessions only `modes: [guarded]` (forcing applies to sessions; an agent with no session is guarded only by its own choice).
- **Steer.** `session.input {session}` needs the steer right on a live session, and an accountable author.
- **The invariant.** *While an agent `X` is guarded, any instruction to `X` from anyone but its guard `g` holds `X`; while `X` is held, nothing `X` proposes or is named executor of runs without `g`'s own approval; only `g` ends a hold.* Concretely:
- **Guarded.** `guardedNow(X)` is true, with guard `g`, when either `X` chose it (it opened a `guarded` session: `guarded[X] = g`, its confirmed operator, who must hold a role) or owners force it (one of `X`'s live sessions is effectively `guarded` under the allowed modes; `g` is that session's guard). The choice is sticky: owners closing `X`'s sessions, removing `g`, or changing modes don't end it. Only `X` ends it, while not held, by closing its last guarded session (re-closing one an owner already closed counts), so an agent can always undo its own choice.
- **Hold.** An *instruction* to `X` is a `session.input` into any of its sessions, a `task` naming it (each assignee), or an `effect.propose` naming it as executor. A steer from anyone but `X` and `g` while `guardedNow(X)` puts `X` **on hold for `g`** (`held[X] = g`). After **every** entry Policy also settles holds in one place: if `guardedNow(X)` and an outside task naming `X`, or an unfinished outside effect naming it as executor, is still open and newer than `X`'s last release, `X` is held, and each such effect records `g` among its guards. So becoming guarded by any route (choosing it, owners forcing it, a guard regaining its role) with outside work pending holds at once, and an effect aimed at a guarded executor keeps needing `g` after a release. One rule per agent and per path, so an auto side-session, a task or an executor slot is no side door. `g` ends the hold with `session.input {session, release: true}` on any of `X`'s sessions, even a closed one; nothing else does. While held, `X` may only: talk (`message`), work its own sessions (`session.open`, `session.close`, `session.input` into its own), report (`task.update`, `effect.outcome`, `effect.resolve`), claim, cancel (`effect.cancel`, `task.cancel`), and propose. Everything else, including `task`, `effect.approve`, `session.input` into others' sessions, `member.set`, `policy.set`, `redact` and extensions, is denied. Anything else a held agent asks of others is data (R-24) or an effect proposal, which needs `g`.
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

1. **Request.** An owner asks the target sequencer B to host the space: `signal(profile:@:B, {type: "host-request", space, head})` on the transport (T-05). B verifies the chain through `head` (R-14).
2. **Accept.** If willing, B returns `accept = Ed25519(B's key, "han-accept-move-v1\n" ‖ space ‖ "\n" ‖ receiptHash(head))` by signalling the owner's profile: `{type: "host-accept", space, head, keyId, sig}`.
3. **Move.** The owner appends `space.move {to: B, accept}`. The current home A verifies `accept` against its *current* head. If the head moved since B signed, the move is `invalid`, and the owner repeats from step 1. The move is denied while an effect is `claimed` (P-11). A orders it at seq `n`. That is A's last record until a later move names A again; from then on A answers `moved`.
4. **Continue.** B, which has been following the space since step 1, orders the next entry at `n+1`, with `prev = receiptHash(n)`, signed by B, and with a time ≥ A's last (it waits if its clock is behind, R-03a).

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
| The transport delivers a stream to someone without the right | Matrix binding: per-session, per-right E2EE rooms whose membership the sequencer keeps equal to `sessionAccess` (§8.2); the sequencer's room user has no device and can't decrypt. A homeserver admin who adds devices or users is out of scope, as for chat. |

HAN v1 runs no consensus.

---

## 8. Transport bindings

### 8.1 What HAN requires of a transport

| Rule | Requirement |
|---|---|
| **T-01 Carry the log** | Deliver every appended `LogRecord` to the space's members, in any order. Verifiers restore order from the chain, so the transport needs no ordering or integrity of its own. |
| **T-02 Map identities** | A HAN `ActorId` is the transport user with the same ID. v1 supports transport IDs that fit the ActorId grammar (§2; e.g. Matrix IDs without ports or uppercase); others need an alias profile in a later version. HAN authorship comes only from HAN signatures (R-12), never from the transport's sender field. |
| **T-03 Carry files** | Store and fetch blob bytes named by `BlobRef` (with its `uri`). Every reader MUST verify `sha256` before use. Access control to the bytes is the transport's, and a HAN reference grants no access by itself. |
| **T-04 Invite** | Let owners bring members into the room; first contact is the transport's (e.g. Matrix invites). |
| **T-05 Signal** | Carry ephemeral messages: presence, typing, streamed agent replies, faults, and the move handshake (§7.3). Signals never carry authority: anything that matters is an entry. |
| **T-06 Room ↔ space** | Bind each room to at most one HAN space, so members can find its sequencer. |
| **T-07 Session streams** | Carry each live session's `StreamItem`s (han.ts: output, status, context snapshots as blobs) to the actors whose `sessionAccess` grants watch, and context items only to those granted read. After a receipt removes an actor's right (a lost role, a closed session, a policy change), items the publisher sends once it has seen that receipt MUST NOT reach that actor. Publishers check `sessionAccess` at their latest receipt before each item. Streams carry no authority: a steer is a `session.input` (P-15), never a stream message. Status MAY be taken from the harness's terminal program-status reports (OSC 7501: idle, working, blocked, done, error) and is presence, never fact. |

### 8.2 Matrix binding

| HAN | Matrix |
|---|---|
| space | a room with state event `org.han.space` `{spaceId, sequencer}` (T-06) |
| `append(entry, content)` | a member sends the event `org.han.entry` `{entry, content}` to the room. The sequencer runs as an **application service** for its server. It sees the event, runs §4.1, and answers with an `org.han.receipt` event `{receipt}` (T-01). A resend of the same entry gets the same receipt (R-01), so Matrix's at-least-once delivery is harmless. |
| refusal | for a `denied` or `invalid` entry, the sequencer sends `org.han.reject {id, code, reason}`, related to the entry's Matrix event. A client shows an entry as *pending* until it sees a receipt or a reject. Interactive clients MAY append over the generic binding instead, which answers at once. |
| `read` | the room timeline (`org.han.*` events), plus `GET /_han/v1/read` on the sequencer for the full chain and state. That request carries `Authorization: HAN <actor> <keyId> <proof>`, where the proof signs a challenge from `GET /_han/v1/challenge` exactly as in `hello`, so P-02 read rights apply. |
| signals (T-05) | room-wide signals (faults, executor progress) are ordinary room events `org.han.signal {body}` that carry no authority. Private ones (notices, the move handshake) are to-device events of the same type. Executor progress uses `{type: "progress", claim, step, text}`. |
| verifier | any client or server in the room. Clients SHOULD show unverified or faulty HAN state as such. |
| redaction (P-12) | a HAN `redact` entry. The sequencer then also redacts the Matrix event's content, and serves `redactedBy`. |
| files (T-03) | Matrix authenticated media (`mxc://` URIs in `BlobRef.uri`), with HAN hash checks |
| invites, typing, presence, chat (T-04, T-05) | native Matrix |
| session streams (T-07) | **two end-to-end encrypted rooms per session**, a *watch room* and a *read room*, created by the sequencer's application-service user on the `session.open` receipt and linked from the space room by state event `org.han.session {session, watch, read}`. Power levels: the service user 100, everyone else 0; `invite`, `kick`, `ban`, `redact` and `state_default` 100; event `org.han.stream` 50, and the session's actor 50, so only it can post items. `history_visibility: joined`, `join_rule: invite`, `guest_access: forbidden`. After every receipt the service user invites or kicks so that each room's membership equals the actors `sessionAccess` grants that right on that session, and on `session.close` kicks everyone. Items are `org.han.stream` events whose content is a `StreamItem`: output and status to the watch room, `context` to the read room. Context blobs are encrypted attachments (key in the event), so an `mxc://` URI alone reveals nothing. The publishing host encrypts each item only to the devices of actors `sessionAccess` grants at its latest receipt, and rotates its outbound Megolm session whenever a receipt removes any right, before sending again; so a removed actor gets no key for later items even before the kick lands. Stock SDKs share room keys with every joined device, so a conforming host needs a key-sharing hook that filters recipients (or must wait for the kick to land before publishing again, which is also conforming). The service user has no device, so it can't decrypt. Removal stops future keys; what someone already received stays received. Live audio and screen share use MatrixRTC in the watch room. |
| HAN roles vs Matrix power levels | independent. HAN authority comes only from the HAN log. Rooms SHOULD keep Matrix membership consistent with HAN roles, but HAN never reads power levels. |
| end-to-end encryption | supported for chat. `org.han.*` events MAY be encrypted, provided the sequencer's application-service user is a room member that can decrypt them. |

### 8.3 The generic binding

For non-Matrix use and testing, a server MAY offer the **generic binding**: one authenticated WebSocket carrying the three calls (§3) plus the binding's seven operations (M7, han.ts `TransportBinding`) encoded per connection: the connection authenticates `as`/`from`, `subscribe` is called `follow` in ref/wire.py, and `blobPut(bytes)` is sent as parts `blobPut(ref, offset, bytes)` of at most `maxBlobPartBytes`. `publish` is the home's own fan-out. [ref/wire.py](ref/wire.py) implements it. Its rules realize T-01…T-05 and T-07:

**G-01 Access follows references; supply follows authors.** A record *references* a blob if its unredacted content holds the `BlobRef` at:
- `content.blobs[]`, for **every** kind, extensions included; or
- `effect.propose.plan.argsBlob`; or
- `effect.outcome.evidence` or `effect.resolve.evidence`.

`blobGet(ref, space)` succeeds only if both of these hold:
1. **Access:** a record in `space` that the caller may read (P-02) references `ref`.
2. **Supply:** the server holds the bytes *for that space*. That is true when the referencing entry's author uploaded them to this server, or when the server fetched them for that space under G-02. An upload counts only if **every byte came from that uploader's own `blobPut` calls** and those bytes hash-match. A server MUST NOT report `complete` to an uploader just because the hash is already stored. Storage may be deduplicated by hash, but access never is. Knowing a hash, and mentioning it in a space you control, gets you nothing. That's why a hash proves bytes are equal but never permission, authorship or truth.

Uploaders may read their own unreferenced blobs for 24 h. A server MUST keep a blob while any unredacted record it stores references it, and MAY delete it after.

**G-02 Blob transfer.** A client uploads to its own server (`blobPut` as itself; the server may apply quotas). A server that needs a blob it lacks calls `blobGet` as `@:server` in the context of the referencing space. It asks the author's server first, then the home, then any mirror. G-01 is evaluated for the server actor (P-02 server reads). The receiver verifies the hash, moves on to the next server on a mismatch, and from then on holds the bytes for that space.

**G-03 Notices (first contact).** After ordering a `member.set` that gives an actor on server `S` a role, or an entry whose `to` names an actor that holds a role, the home MUST send `signal(profile:<actor>, {type: "notice", space, seq})` to `S`. `S` delivers it **only to that actor's own authenticated sessions**, as signal items on the actor's follow of its own profile space, and never to the profile space's other readers. A notice is only a hint: `S` verifies it by reading the space as `@:S` (P-02 server reads), and drops notices it can't verify. This is how a new member, human or agent, learns that a space exists.

**G-04 Who may signal.** Only actors who may read a space at its head (P-02) may `signal` it or `follow` it for signals. **Across servers:** a user's server relays the user's signal to the home over its own `@:server` connection. The home accepts a relayed `from` only for actors on that server. The home sends signals to other servers' follow streams, and each server fans them out to its local followers. **Profile spaces** are readable by anyone, but only the actor itself may follow its profile for signals, and the only signals they accept are G-03 notices from server actors. Servers rate-limit signals per actor and drop them under load.

**G-05 Session streams.** `stream(item)` is accepted only from the session's actor, on a live session, with `n` one more than its last item. `watch(space, session, as)` delivers the session's `StreamItem`s (han.ts) to `as` while `sessionAccess` grants it watch, and items of type `context` while it grants read. The home evaluates access at the head when it delivers each item, so nothing published after a receipt that removed a right reaches the actor that lost it.

### 8.4 Profile: Claude Code as a session (informative)

How a harness maps onto P-15 and R-24, with Claude Code as the example. The *host* is a small process holding the agent's HAN key, which runs Claude Code through the Claude Agent SDK in streaming-input mode, so it controls every turn.
- **Open.** The host appends `session.open {steerMode}` as the agent and publishes `output` items from the SDK's message stream and `status` items from its turn state (and from OSC 7501 program-status reports where the harness emits them).
- **Context.** On request, or periodically, it publishes a `context` item: transcript and plan as an encrypted blob.
- **Steer in.** A `session.input` with a receipt becomes a user turn. For an outside input in `guarded` mode, the host first sets the SDK permission mode to `default` (instead of the session's `acceptEdits` or `bypassPermissions`), and its `canUseTool` callback decides each tool call during that turn. The host MAY first show the input to the agent's human. Per R-24, while the agent is guarded every turn driven by someone else's instruction (steer, task, effect naming it) uses the stricter mode, and while it is held every turn does.
- **World actions.** The session runs sandboxed. Its network egress is limited to the model API and the host itself. Dependencies are fetched by the host before the turn (or as effects), since even a read-only mirror's request URLs can leak context. The host, outside the sandbox, holds the agent's HAN key and Matrix credentials and talks to the homeserver, sequencer and executor; nothing inside the sandbox can post to any room. So pushes, deploys, payments and outside messages reach the world only through an executor, as effects. The host starts the SDK with `settingSources: []` (so no project or user settings add allow rules) and gives it **no allow rules** while guarded or held, so every tool call except the built-in read-only file tools (which stay inside the workspace) reaches `canUseTool`. There, while held, a call that would reach the world becomes `effect.propose`, and the model is answered at once with "proposed as <id>, waiting for <guard>" (no blocking, so no hook timeout). `WebFetch` and `WebSearch` are world reads that can leak context through URLs, so they are proposed as effects too; MCP servers other than HAN's own are disabled for those turns. Edits inside the session's workspace stay local. The sandbox must cover every process the session can start, not only the shell tool; without it, `guarded` is advisory, and the host MUST say so in its `status`.
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

Run `python ref/run_all.py`. Results: [ref/RESULTS.md](ref/RESULTS.md).

| Tool | What it does |
|---|---|
| `ref/policy.py` | executable reference of §6 and the R-09 body schemas |
| `ref/vectors/` + `run_vectors.py` | 107 vectors, 1074 steps (including the §1.1 worked example); expectations hand-written from this document; every step also checks purity and determinism |
| `ref/mutate_vectors.py` | 112 single-rule Policy mutations; the vectors must catch each one |
| `ref/fuzz_fold.py` | 2,000 random histories in populated rooms: `decide`/`fold` never fail on an allowed entry, stay pure, keep budgets in bounds, never change a final phase |
| `ref/wire.py` + `run_wire.py` | the generic binding with real Ed25519: reference sequencer (the §4.1 pipeline) and verifier (R-14); 30 scenarios |
| `ref/mutate_wire.py` | 24 single-rule mutations of the sequencer and verifier; the scenarios must catch each one |
| `ref/model_check.py` | exhaustive bounded exploration driving the real `policy.py` in three scenarios (timing, drift, single); 10 mutations must each give a counterexample; a non-vacuity guard fails runs that never launch |
| `ref/model_guard.py` | exhaustive bounded exploration of P-15 against the real `policy.py`, depth 5 (~6 min; `--quick`: depth 4), with a ghost derived from the invariant sentence, not from Policy's rules: per agent, whether it chose to be guarded and whether it is held. Two agents with different operators, both able to become guarded (one later, with outside instructions pending); instructions by steer into any session, by tasks with several assignees, and by naming an executor; owners steer, close sessions, evict a guard and toggle modes; proposals made before and during holds; releases by the guard, another operator and an owner; key revocation; the approval rule. Properties GuardedClaimsNeedTheirHuman, HeldInstructNoOne, NeverLooser, GuardRightsNeedRole, OneOpenApprovalPerAuthor and ExactHolds (Policy holds exactly whom the invariant holds, never fewer, never more); 27 mutations, each caught by a property at depth 4; a non-vacuity guard. The ghost settles holds after every step, as the spec says |
| `ref/check_consistency.py` | §5 = han.ts = ref schemas; every folded state conforms to han.ts `SpaceState` |

| Rule | Checked by | |
|---|---|---|
| R-01 dedupe first | wire "dedupe precedes every check"; wire mutation; model | ✓ |
| R-02 one chain, proofs | wire "valid chain", "reordering", "equivocation proof", "move then old-home receipt", "moving back to a former home", "first home is pinned"; wire mutations | ✓ |
| R-03 durable before receipt | **procedure:** kill -9 the sequencer, and separately power-cut its disk, after each acknowledged append. On restart every receipted record must be present, and none may be missing at `replicated` after losing one machine. Needs an implementation. | ✗ |
| R-03a receipt time | wire "receipt time non-decreasing", "a new home with a slow clock waits"; model (offsets, rounding, drift) | ✓ |
| R-04 redaction proofs | wire "content binding and redaction" (including a faked redaction); vectors `redact`, `no-redacting-live-plans`; wire mutation | ✓ |
| R-05 references | vectors `refs-and-seen`, `missing-references-invalid`; mutations | ✓ |
| R-06 / R-07 retry and outbox | model: the executor's journal is its outbox, and claim resends are idempotent. **Procedure** for agent hosts: crash between sign and send, and between send and receipt, then check that exactly one entry exists. | partial |
| R-08 no silent downgrade | wire "no silent downgrade", "minDurability can't lock a space"; wire mutations | ✓ |
| R-09 closed shapes | vectors `closed-bodies`, `extension-kinds`, `impossible-dates`; wire "header is exact", "content is validated"; consistency check; mutations | ✓ |
| R-10 profile pin monotonic | wire "profile pin monotonic"; wire mutation | ✓ |
| R-11 determinism | every vector step and every fuzz step (purity + repeat); wire "forbidden accept caught by replay" | ✓ |
| R-12 identity is keys | vectors `revoked-author-key`, `E2-proposer-revoked`, `E3-approver-key-revoked`, `keyid-single-use` | ✓ |
| R-14 verifiers check everything | wire "tampered body", "forbidden accept", "first home is pinned", "server key validity window", "content is validated", "moves need the target's acceptance"; wire mutations | ✓ |
| R-15 head gossip | wire "head gossip finds a fork, privately"; wire mutations; the 10-minute schedule is not exercised | ✓ |
| R-16 abandon needs elapsed time | model (the sequencer's clock is the model's true time; drift scenario). Not yet a wire scenario. | partial |
| R-17 / R-18 agent inputs, recovery | **procedure:** crash an agent host mid-task, restart, and check that no intent appears twice and every entry's `seen`/`refs` identify its inputs. Needs an implementation. | ✗ |
| P-01 | vector `relevant-actors`; vector `open-approvals-capped` | ✓ |
| P-02 | vectors `read-rights`, `read-public`; wire "state needs read rights at head"; mutations | ✓ |
| P-03 | vectors `membership-rules`, `open-join-and-ban`, `agents-*`, `server-deny`, `denied-server-users-can-be-removed`, `operator-withdrawal-stops-agent`, `operator-withdrawn-before-claim`, `owners-are-human`, `agents-off-stops-agents`; mutations | ✓ |
| P-04 | vectors `tasks-and-delegation`, `banned-loses-task-authority`, `owner-force-closes-task`; mutations | ✓ |
| P-05, P-06 | vectors `task-budgets`, `budgeted-child-of-unbudgeted-task`, `E6-*`, `budget-*`, `agent-*`, `no-closing-over-running-effects`; fuzz; mutations | ✓ |
| P-07 E1–E6 | vectors `E1`…`E6`, `banned-executor`, `proposal-expiry`, `launch-window-is-policy`, `closed-tasks-stop-their-effects`; mutations; model (E2, last run round 7) | ✓ (model: partial) |
| P-08 | vectors `E3-*`, `approval-rule-removed`, `open-approvals-capped`; mutations | ✓ |
| P-09 | vectors `outcome-rules`, `result-started-table`, `repeatable-*`, `window-pinned-in-claim`, `abandon-never-rereadies`, `abandon-never-rearms`; model NoLaunchAfterAbandon and its five timing mutations; margin mutations (model last run round 7) | ✓ (model: partial) |
| P-10 | vectors `outcome-rules`, `resolve-cost-rules`; model UnknownNeverSilentlyRetried (last run round 7) | ✓ (model: partial) |
| P-11 | vectors `checkpoint-move-policy`, `no-move-while-claimed`, `approval-rule-removed`, `budget-unit-locked-while-reserved`, `cant-deny-own-home` | ✓ |
| P-12, P-13 | vectors `redact`, `no-redacting-live-plans`, `fork-fresh-authority`; mutations | ✓ |
| P-15 | vectors `session-rights`, `sessions-nobody`, `session-policy-shape`, `guarded-steer-holds`, `held-agents-instruct-no-one`, `only-release-releases`, `modes-apply-to-open-sessions`, `owners-only-tighten-modes`, `guard-rights-need-a-role`, `held-may-cancel-its-delegations`, `two-operators`, `guard-takes-the-wheel`, `guards-are-checked`, `guarded-needs-an-operator`, `open-approvals-capped`, `expired-approvals-crowd-no-one`, `nobody-stands-in-for-a-guard`, `held-executors-need-their-guard`, `tasks-hold-guarded-agents`, `side-door-sessions-are-covered`, `guarded-is-sticky`, `modes-toggle-cannot-unguard`, `only-the-agent-unguards-itself`, `naming-a-guarded-executor-holds-it`, `returned-approvals-stay-superseded`, `owner-forced-guarding-covers-every-instruction`, `held-proposers-need-their-guard-for-older-proposals`, `agents-can-always-unguard`, `guarding-holds-for-open-outside-instructions`, `tasks-hold-every-guarded-assignee`; policy mutations; guard model (depth 5) with six properties and 27 mutations; the fuzzer | ✓ |
| R-24, T-07, G-05, §8.4 | **procedure:** steer a `guarded` Claude Code session from another member and check that a push becomes a proposal awaiting the agent's human; check that the agent can't relay or delegate while held; check that text in a watched stream never acts as input; remove a watcher's role and check delivery stops. Needs an implementation. | ✗ |
| §4.3 identity rules | vectors `profile-space`, `keyid-single-use`, `operator-must-be-another-human`; mutations | ✓ |
| X-01 | vector `E1-executor-instance` | ✓ |
| X-02 … X-06, X-10 | **partial since round 9:** the effect fold has changed (guard approvals, supersession, rearm) and the timing model hasn't been rerun on this machine for lack of memory; last full run round 7 (log: `v1-mc-r7.log`, not in the repo's RESULTS.md, which `--quick` regenerates). Model (timing, drift, single): AtMostOneLaunchPerClaim, NoLaunchWithoutReceipt, NoLaunchAfterAbandon, Honest*. The model's executor follows X-10 when its outcome is refused. Mutations. | partial |
| X-09 | the plan-digest check: wire "content is validated" (sequencer and verifier) | partial |
| X-07 | **procedure:** start two processes with one instance key; the second MUST refuse to start (lock), and restoring a key without its journal MUST be refused. Needs an implementation. | ✗ |
| T-01 … T-06 | the generic binding exercises T-01 and T-05 in wire scenarios ("signals ride follow streams", "notices stay private", "first contact across servers", "blob refs per kind and transfer", "blob theft by hash is refused"). The Matrix binding has no tests yet. | partial |

**Not covered** (✗ above): durability under storage faults, agent-host recovery, key custody, and the Matrix binding. Each has a written test procedure, but needs a running implementation.

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

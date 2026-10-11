# Matrix binding: what it does, and where §8.2 is open, ambiguous or not implementable

Code: `appservice.py` (the sequencer as an application service), `client.py` (an actor's client), `api.py` (client-server API over aiohttp, no SDK), `media.py` (encrypted attachments), `store.py`, `proto.py`. Tests: `tests/test_matrix_*.py` against a real Synapse v1.120.2 in Podman (`matrix-dev/synapse_dev.py`). Nothing in `ref/`, `han/seq` or `han/client` was changed.

## Run it

```
# Synapse (Podman through the WSL machine; set CONTAINERS_MACHINE_PROVIDER=wsl, HAN_PODMAN_CONNECTION=actual-wsl by default)
python matrix-dev/synapse_dev.py up --port 18008 --as-url http://127.0.0.1:9300 --state matrix-dev/state
# the sequencer as an application service (+ the generic binding for executors on --ws-port)
python -m han.matrix.appservice --registration matrix-dev/state/registration.yaml --homeserver http://127.0.0.1:18008 \
    --server a.localhost --listen 127.0.0.1:9300 --db seq.db --key server-key.json --ws-port 8701 --register @alice:a.localhost=alice.key.json
python matrix-dev/synapse_dev.py down
```

Synapse runs on the host network (`--network host`), so it reaches the appservice at `127.0.0.1:9300` and Windows reaches Synapse at `127.0.0.1:PORT`. That works with WSL "mirrored" networking (this machine). With NAT networking, `host.containers.internal` pointed at the VM, not Windows, so pass `--as-url` with an address Synapse can reach. The tests use free ports and a unique container name per pytest session, remove the container and volume at the end, and are skipped with a reason if Podman or the image is missing (`HAN_SKIP_MATRIX=1` skips them on purpose).

## Implemented

- **Appservice.** `@han-seq:<server>` joins rooms it is invited to. An `org.han.entry {entry, content}` event goes through the unchanged `Sequencer.append` (§4.1, R-01 dedupe first). The answer is `org.han.receipt {receipt}` or `org.han.reject {id, code, reason}` with `m.relates_to` (`m.reference`) on the entry's event. A resend (new event, same entry) gets a receipt event for the new event with the same receipt. Answers are sent with deterministic transaction ids, so redelivery of a Matrix transaction is harmless.
- **T-06.** The first entry of an unbound room must be a `space.create`; on success the service stores room↔space (1:1) and sets `org.han.space {spaceId, sequencer}`. Other entries in an unbound room, or for another space than the room's, are rejected.
- **T-01.** Records that arrive by another binding (the generic one) are published too: the service posts the `org.han.entry` itself, then the receipt. A restart, or an earlier Matrix outage, is caught up from the `published` counter. A client joins an entry event to the receipt by entry id, so it works in any order; `MatrixClient.records(room)` gives LogRecords that `ref/wire.py` `Mirror` verifies.
- **read.** `GET /_han/v1/challenge`, `GET /_han/v1/read?space=&after=&state=&limit=&at=` with `Authorization: HAN <actor> <keyId> <proof>`; single-use challenges, 120 s. P-02 applies (a non-reader gets `not_found`).
- **Redaction.** After a `redact` receipt the service redacts every Matrix event that carried the target (see "Redaction" below).
- **T-07 rooms.** On the `session.open` receipt: two rooms with the stated power levels, `history_visibility: joined`, `join_rule: invite`, `guest_access: forbidden`, linked by `org.han.session {session, watch, read}` (state key = session id) in the space room. After every flush the service makes each room's invited-or-joined set equal the actors `sessionAccess` grants watch (read room: read), kicking the rest; on `session.close` (or a dead session) everyone, including the actor, is kicked.
- **Client.** `append` (outbox, resend as a new event every `resend_after`, R-06/R-07), `submit` (send once), `read_http`, `follow`, `records`, `signal`, `join_session`, `watch`, `publish`, `fetch_context`. Rejects, receipts and `org.han.session` count only from the room's bound sequencer user.
- **Context blobs** are AES-256-CTR encrypted attachments (Matrix `EncryptedFile` shape); the key is in the `org.han.stream` event, `BlobRef.uri` is the `mxc://` URI, and `BlobRef.hash` is checked after decryption.

## Not implemented (honest list)

1. **End-to-end encryption of the session rooms (and of any `org.han.*` event) is omitted.** The Python stack here has no Olm/Megolm, and writing one was out of scope. Consequences: the service user and the homeserver can read stream items, so §8.2's "the service user has no device, so it can't decrypt" does not hold in this version; the Megolm key-sharing filter and rotation are absent. What remains is the membership logic and a stand-in for "no item reaches a removed actor": `MatrixClient.publish` reads the state at the sequencer's head and waits until each room's invited-or-joined members are within what `sessionAccess` grants before it sends (the "wait for the kick to land" alternative §8.2 allows). `test_matrix_streams.py` shows that a demoted or removed actor's Synapse no longer serves the next item. Context blobs are the only encrypted thing.
2. Space-room membership is not mirrored from HAN roles (§8.2: SHOULD). Invites are the native Matrix kind (T-04). See "P-02 and the timeline".
3. T-05 beyond room signals: to-device signals (notices, move handshake) and the `heads`/`fault` gossip signals are not sent or acted on. `org.han.signal` is sent and delivered by `signal` and `follow`.
4. T-03 for entries: `BlobRef.uri` of entry blobs through Matrix media, and `blobPut`/`blobGet` on this binding. Only context attachments use media.
5. `han-exec` and `han-host` are written against `han.client.Client`, not `MatrixClient`. The effect test connects the executor over the generic binding of the same sequencer (`--ws-port`); its records still reach the room. A `Client`-shaped adapter over `MatrixClient` (`read`, `subscribe`, `signal`, blobs, `stream`, `watch`) is not written.
6. R-24 / §8.4 (the Claude Code host) is host work, not binding work; not touched.
7. Moves (`space.move`), forks and federation of the appservice (a sequencer on another server than the room's members) are not exercised; Synapse runs with federation off.

## Spec gaps and readings (also for the spec author)

**Binding and trust**
- §8.2 does not say who creates the room, how the service user gets in, or who sets `org.han.space`. Reading: the room creator invites `@han-seq:<server>` and gives it power to set `org.han.space` and `org.han.session` (`MatrixClient.create_space_room` does); the first entry is `space.create`; the service sets the state.
- "Honour a reject only from the sequencer's user named in `org.han.space`" is only as strong as that state event, and anyone with state power can overwrite it. The client therefore accepts `org.han.space` only if its sender equals the sequencer it names **and** that is `@han-seq:` on the server in the space id (`sequencer_for`, overridable), and a room keeps its first binding. Since the user is in the appservice's exclusive namespace, only the appservice can act as it. A forged binding, receipt or reject from a member is ignored and recorded in `RoomView.ignored` (tested). This convention is mine; the spec should say how a client learns the legitimate sequencer user (by space id convention, or pinned out of band). It also breaks after a `space.move` to a server with another service user.
- A room that is not bound yet has no named sequencer, so a refused `space.create` could never be answered. Rule: for an entry sent to an unbound room the client accepts a reject from the user expected for the entry's space. The spec should say so.
- Spec says a reject is for `denied` or `invalid`. I also send `not_found` (unbound/wrong room) and `moved`. `unavailable` and `limit` get **no** event: the entry stays *pending* and the client's R-06 resend retries. Hence "pending until receipt or reject" never ends for a sequencer that is down; that is intended.
- A receipt event carries only `{receipt}`, so the record needs the entry from somewhere: the entry event in the room. For records ordered over another binding nobody posted one, so the service posts `org.han.entry` itself (its sender is irrelevant to authorship). The spec does not say so.
- R-01 says dedupe before **any** other check. Here the room-binding check comes first, so a resend from a different room is refused (otherwise a non-member could learn that an entry exists). A resend in the right room is R-01-exact.
- T-02 and the task text "a user posting an entry signed by someone else's key is rejected": the signature, not the Matrix sender, decides (R-12). So bob posting an entry that claims to be alice's but is signed with bob's key is `invalid`, while bob relaying an entry alice really signed is accepted. Both are tested.
- The HAN `Authorization` header carries only `<actor> <keyId> <proof>`, not which challenge was signed. The server keeps recent challenges and tries each (single use). An optional `HAN-Challenge` header names it. The spec should add the challenge to the header or fix the mechanism.
- `GET /_han/v1/read` has no response schema: here `{items: ReadItem[]}`, no `follow`; errors are `{error: WireError}` with HTTP 400/401/404/429/503/421.

**Sizes**
- A Matrix event is limited to 64 KiB (`M_TOO_LARGE`), but HAN allows header 16 KiB + content 64 KiB. Such an entry cannot be appended over Matrix; the client fails it at once with `invalid` (use the generic binding). The spec should state a Matrix-specific cap or require the generic fallback.

**Redaction (P-12, R-04)**
- A Matrix redaction strips the whole event, including the entry header that R-04 says is permanent. So the service first posts a header-only copy (`org.han.entry {entry, redacted: true}`), then redacts every event that carried the content. Clients rebuild `redactedBy` from the `redact` record, which is in the log. §8.2's "also redacts the Matrix event's content" does not work as written. Also, `serves redactedBy` is only the HTTP read; the room gets the proof from the redact record.

**P-02 and the timeline**
- The room timeline shows every record to every room member, whatever P-02 says; `canRead` is per seq and per role, which a Matrix room (membership + `history_visibility`) cannot express. Only `GET /_han/v1/read` enforces P-02. §8.2 says rooms SHOULD keep membership consistent with roles, but even then a member removed from HAN keeps what the timeline already delivered, and a late joiner with `shared` history sees records from before they held a role. A space with read restrictions needs the HTTP read as its only source, or per-reader rooms. The spec should say which.

**Session streams (T-07)**
- "Items are `org.han.stream` events whose content is a StreamItem": a context item needs the attachment key, so the event content is a StreamItem plus one extra field `file` (Matrix EncryptedFile info). The StreamItem schema is closed; this is an addition.
- "Everyone else 0": `events_default` is not stated; left at 0, so any member of a stream room may post ordinary Matrix messages (they are not stream items). `state_default` 100 stops them from changing the room.
- "After every receipt the service user invites or kicks": implemented as an idempotent reconcile (desired set from `sessionAccess` over the roles in the state, the session's actor and guard; actors with no Matrix account fail to invite and are logged). Invited-but-not-joined users count as members, and kicking an invitee revokes the invite. A user whose `sessionAccess` is regained can be invited again.
- "Publishers check `sessionAccess` at their latest receipt before each item": the publisher cannot know the receipt the service has processed, so it reads the head state over HTTP and waits for the kicks to land, which is stricter. Without E2EE this wait is the *only* protection (a lagging kick would otherwise leak the item), and an unreachable service blocks publishing (`TimeoutError`).
- Creating the two rooms and writing `org.han.session` is not atomic. A crash between them leaves an unused room, and the flush retries and makes new rooms. The service has no way to find the old ones.
- A watcher joins the rooms by invite; the invite may land after the receipt, so `join_session` retries `M_FORBIDDEN` for a while.

**Transport behaviour found by running it**
- A second initial `/sync` of the same user shortly after the first came back without the rooms created in between (apparently a response cache in Synapse), so a client that restarts can start with a stale room list. `MatrixClient.start` syncs incrementally until nothing is new before it resends its outbox.
- A transaction is answered 200 only after the whole batch is processed; if Synapse or the sequencer is unreachable the appservice answers 500 so Synapse redelivers (R-01 makes that safe).
- Synapse's default rate limits would slow the tests; `synapse_dev.py` switches them off in the throwaway config.

## Changes wanted in other modules

- `han.seq.Sequencer`: a public post-append hook (or a way for a binding to subscribe to every new record as the server). The appservice now wraps the instance's `append` method to learn about records appended by the generic binding. That works but is a monkeypatch.
- `han.client`: a small `Protocol` for what actors need from a client (`append`, `read`, `subscribe`, `signal`, blobs, `stream`, `watch`), so an executor or host can be handed a `MatrixClient` adapter.
- `han.ts`: the HTTP read frame and the `org.han.session` / `org.han.stream` event shapes (with `file`) belong next to `TransportBinding`.

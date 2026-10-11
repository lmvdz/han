# Matrix binding: what it does, and where §8.2 is open, ambiguous or not implementable

Code: `appservice.py` (the sequencer as an application service), `client.py` (an actor's client), `api.py` (client-server API over aiohttp, no SDK), `media.py` (encrypted attachments), `store.py`, `proto.py`. Tests: `tests/test_matrix_*.py` against a real Synapse v1.120.2 in Podman (`matrix-dev/synapse_dev.py`). Round 20 added a public post-append hook to `han.seq.Sequencer` (`add_ordered_hook`) and `han.client.ClientProtocol`; nothing in `ref/` was changed.

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

- **Appservice.** `@han-seq:<server>` joins rooms it is invited to. An `org.han.entry {entry, contentHash}` event (header only, P-02) goes through the unchanged `Sequencer.append` (§4.1, R-01 dedupe first); the entry's content arrives ahead of it by `POST /_han/v1/content` (HAN-authenticated, held 10 minutes under its hash; an event whose content is missing gets no answer and the client's R-06 resend uploads it again). Records ordered by any binding reach the room through `Sequencer.add_ordered_hook` (no monkeypatch). The answer is `org.han.receipt {receipt}` or `org.han.reject {id, code, reason}` with `m.relates_to` (`m.reference`) on the entry's event. A resend (new event, same entry) gets a receipt event for the new event with the same receipt. Answers are sent with deterministic transaction ids, so redelivery of a Matrix transaction is harmless.
- **T-06.** The first entry of an unbound room must be a `space.create`; on success the service stores room↔space (1:1) and sets `org.han.space {spaceId, sequencer}`. Other entries in an unbound room, or for another space than the room's, are rejected.
- **T-01.** Records that arrive by another binding (the generic one) are published too: the service posts the header-only `org.han.entry` itself, then the receipt. A restart, or an earlier Matrix outage, is caught up from the `published` counter. A client joins an entry event to the receipt by entry id, so it works in any order; `MatrixClient.records(room)` gives LogRecords without content, `fill_content(room)` adds what the HTTP read lets this actor see, and then `ref/wire.py` `Mirror` verifies them.
- **HTTP side (`/_han/v1/...`).** `GET challenge`; `GET read?space=&after=&state=&limit=&at=` → `{items: [ReadItem]}` (errors `{error: WireError}`); `POST content`; `PUT blob {ref, offset, data}` and `GET blob?hash=&size=&type=&space=&offset=` (Sequencer.blob_put / blob_get, G-01). All carry `Authorization: HAN <actor> <keyId> <proof>` and `HAN-Challenge: <challenge>` (required; single use, 120 s). P-02 applies to read (a non-reader gets `not_found`).
- **Redaction.** Rooms never hold content, so a `redact` leaves nothing to strip there (see "Redaction" below).
- **T-07 rooms.** On the `session.open` receipt: two rooms with the stated power levels, `history_visibility: joined`, `join_rule: invite`, `guest_access: forbidden`, linked by `org.han.session {session, watch, read}` (state key = session id) in the space room. After every flush the service makes each room's invited-or-joined set equal the actors `sessionAccess` grants watch (read room: read), kicking the rest; on `session.close` (or a dead session) everyone, including the actor, is kicked.
- **Client.** `MatrixClient` satisfies `han.client.ClientProtocol` (`append`, `read`, `read_all`, `subscribe`, `signal`, `blob_put`, `blob_get`, `stream`, `watch`, `connect`, `close`): reads go through the HTTP read (follow waits for the room's receipts, polling once a second as a fallback), signals through the room, blobs through the HTTP blob calls, `stream`/`watch` through the T-07 rooms. Also `submit`, `read_http`, `fill_content`, `follow`, `records`, `join_session`, `publish`, `watch_room`, `fetch_context`. `append` refuses an entry over 60 KiB (header + content) with `invalid` and a message naming the generic binding. han-exec runs over it (`Executor(..., client=matrix_client)`, tested against Synapse). Rejects, receipts and `org.han.session` count only from the room's bound sequencer user.
- **Context blobs** are AES-256-CTR encrypted attachments (Matrix `EncryptedFile` shape); the key is in the `org.han.stream` event, `BlobRef.uri` is the `mxc://` URI, and `BlobRef.hash` is checked after decryption.

## Not implemented (honest list)

1. **End-to-end encryption of the session rooms (and of any `org.han.*` event) is omitted.** The Python stack here has no Olm/Megolm, and writing one was out of scope. Consequences: the service user and the homeserver can read stream items, so §8.2's "the service user has no device, so it can't decrypt" does not hold in this version; the Megolm key-sharing filter and rotation are absent. What remains is the membership logic and a stand-in for "no item reaches a removed actor": `MatrixClient.publish` reads the state at the sequencer's head and waits until each room's invited-or-joined members are within what `sessionAccess` grants before it sends (the "wait for the kick to land" alternative §8.2 allows). `test_matrix_streams.py` shows that a demoted or removed actor's Synapse no longer serves the next item. Context blobs are the only encrypted thing.
2. Space-room membership is not mirrored from HAN roles (§8.2: SHOULD). Invites are the native Matrix kind (T-04). See "Redaction (P-12, R-04) and P-02".
3. T-05 beyond room signals: to-device signals (notices, move handshake) and the `heads`/`fault` gossip signals are not sent or acted on. `org.han.signal` is sent and delivered by `signal` and `follow`.
4. T-03 for entries: `BlobRef.uri` of entry blobs through Matrix media. Blobs of `blob_put`/`blob_get` go over the HTTP blob calls, not Matrix media; only context attachments use media.
5. `han-host` over Matrix is possible by the protocol (the host only uses `ClientProtocol`) but only han-exec is exercised against Synapse. `stream` of a context item needs the bytes from this client's own `blob_put`; `watch(space, session)` joins the watch room and needs the invite.
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
- The HAN `Authorization` header carries only `<actor> <keyId> <proof>`; the challenge it signed is named in a `HAN-Challenge` header (required here). The spec should say so.
- `GET /_han/v1/read` response: `{items: ReadItem[]}`, no `follow`; errors are `{error: WireError}` with HTTP 400/401/404/429/503/421.
- The spec does not say how an entry's content gets to the service when the room must not carry it. Reading taken: `POST /_han/v1/content` before the header-only event.

**Sizes**
- A Matrix event is limited to 64 KiB, and the room only carries headers (16 KiB at most), but the content goes over HTTP; to keep one rule the client refuses an entry whose header + content exceed 60 KiB and says to use the generic binding (WebSocket), where the caller falls back. The spec should state a Matrix-specific cap.

**Redaction (P-12, R-04) and P-02**
- Content never enters a room (rooms carry `org.han.receipt` and header-only `org.han.entry {entry, contentHash}`), so there is no event content to redact; the header stays (R-04) and the HTTP read gives the redacted record with its `redactedBy` proof. Clients rebuild `redactedBy` from the `redact` record in the log as before.
- P-02 is enforced by the HTTP read alone. A member removed from the space, or a user who joins the Matrix room late with no role, sees headers and receipts (who wrote what kind of entry, when) but never content; the headers are permanent anyway (R-04). A space that must also hide headers needs per-reader rooms (not done).

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

- `han.ts`: the HTTP frames (read, content, blob) and the `org.han.session` / `org.han.stream` event shapes (with `file`) belong next to `TransportBinding`.

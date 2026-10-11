# Implementation notes: spec gaps and the readings taken

Each entry says where the spec is silent or ambiguous, what `impl/` does (always the most conservative reading), and the section it touches. Nothing in `ref/` was changed.

## Wire framing (§3, §8.3, han.ts)

1. **Frames for the six transport operations are not in han.ts** (`RequestFrame` covers only `hello`/`append`/`read`). Defined here, same envelope `{id, call, args}`: `subscribe {space, after?}`, `signal {space, body}`, `blobPut {ref, offset, data(base64url)}` → `{have, complete}`, `blobGet {ref, space, offset?, length?}` → `{data, size, offset}` (at most `maxBlobRangeBytes` per call), `stream {item}`, `watch {space, session}` (replies are `item` frames carrying a `StreamItem`). A `cancel {target}` call ends a streaming call (needed to stop a follow without closing the socket); it is not in §3/§8.3. Proposed addition to han.ts.
2. `hello` must be the first call; the server speaks first with the `Challenge` object (§3, han.ts `Challenge`). Calls before it are `denied`.
3. `subscribe` without `after` is live-only; with `after` it replays first, atomically with registration (no gap). §8.3 says only "subscribe is called follow in ref/wire.py".
4. A connection may append an entry whose `author` is not the connection's actor (R-12: authorship is the signature, never the sender).

## Identity and profiles (§4.1 step 4, §4.3, R-10)

5. **Profiles are served only for this server's actors.** An actor on another server has no pinnable profile, so appends needing it answer `unavailable` (§4.1 step 4). No federation, no profile fetch, no `.well-known/han` or `han-peers` documents (§4.3 calls them part of the wire surface; not served). The key window `from` is `1970-01-01T00:00:00Z`.
6. **How a profile space is first created is not specified over the wire.** §4.3 only says the server actor appends `space.create` with `profile`. Here the server signs it with its own home key as `@:S` (the synthetic view of `@:S` lists the home key as valid), through the normal pipeline. Trigger: `python -m han.seq register …`, `--register` at startup, or `Sequencer.register()`.
7. The pinned profile seq is the profile space's head at append time (fresh by construction, R-10). Records store the pinned views so a reload replays `fold` without the profile spaces.
8. `hello` accepts `@:S` (this server's actor) with the server key. This is the "home reads its own space" actor (P-02); it equals root on this server, which is what it is.

## Clock and time (R-03a, R-16)

9. **R-03a "refuse with `unavailable` if the clock is behind"**: no tolerance. The receipt clock is `wall at startup + steady elapsed`; if it is earlier than the space's last receipt time, every append that is not a dedupe hit answers `unavailable` with `retryAfterMs` = the gap, until it catches up. After a restart on a clock that is hours behind, the server is unavailable for hours (as written). The "persisted last receipt time" is the chain itself (per space, which is what R-03a requires), not a separate global value.
10. **R-16 error code.** The spec says the sequencer "MUST NOT order" an abandon before A steady seconds; it does not say what the sender sees. Answer: `unavailable` with `retryAfterMs` (the same request succeeds later), not `denied`, so a client's R-06 retry delivers it when due. It applies only to an owner's `effect.outcome` with `unknown/unknown` by someone other than the claiming instance (P-09); everyone else is left to Policy (`denied`). A claim ordered in an earlier run counts from the start of this run.
11. The R-16 check runs after dedupe (R-01 first) and before Policy, so an early abandon is `unavailable` instead of Policy's `denied "abandon before the launch window closed"`.

## Reads, follow, limits (§3)

12. `read` with `follow` and a backlog larger than `limit`: the backlog is cut at `limit`, then the live stream continues from records newer than the head at registration. The skipped records are a visible gap in `seq`; the client re-reads from its last seq. The spec says both "limit bounds the backlog" and "with follow it continues", not how to join them.
13. `subscribe`/`read follow` on a `profile:` space delivers its records to anyone (profile spaces are world-readable, §4.3); only the actor itself also gets notices (G-03/G-04 say "only the actor may follow its profile for signals").
14. A reader with no readable seq gets `not_found`; `subscribe` on a space the caller can't read gets `denied` (as wire.Home.follow). These differ on purpose of the two spec/ref texts; both avoid revealing more than the reference.
15. Rate limits (`limit` errors, §3, G-04) and `maxFollows` are not enforced. Header size is measured as the JCS of the whole entry including `sig` (≤ 16384), content as JCS (≤ 65536).

## Blobs, notices, sessions (G-01…G-05)

16. **Upload order.** Supply (G-01) is decided by wire.Home when the referencing record is ordered: the author must have completed the upload before the append. An upload after the append does not retroactively supply. Conservative; clients upload first.
17. **Context blobs of `StreamItem`s are not served by `blobGet`.** G-01 grants access only through a referencing *record*; a context item is not a record, so the blob is `denied`/`unavailable` unless a record also references it. T-07/G-05 do not say how read-right holders fetch a context blob in the generic binding. (Proposed: serve it to actors for whom `sessionAccess.read` holds, as the Matrix binding does with encrypted attachments.)
18. "Uploaders may read their own unreferenced blobs for 24 h" (G-01) is not implemented (`blobGet` needs a space); no blob garbage collection.
19. Notices (G-03) are only delivered between actors of this server; they are persisted per actor (newest 256) and replayed on the next `subscribe` of the actor's own profile, so an offline actor still learns of a space. They are hints and not acknowledged.
20. `stream` re-send: a repeat of the last accepted item (same `n`, identical content) is `ok`, so a client that lost the reply can retry; any other `n` that is not last+1 is `invalid` (G-05 only says "n one more than its last item"). `watch` delivers live items only (no backlog), ends only when the connection or the call is closed, and does not announce a lost right.
21. Items of a `StreamItem` are stored (SQLite) but only `n` is used after a restart.
22. `blobPut` completion is idempotent per uploader: if the uploader already completed that hash, a repeat answers `complete` at once (needed when the last reply is lost). In-progress parts are kept in memory and lost on restart; the client resumes at the `have` offset the server reports.

## Verification (R-14, R-15)

23. The server returns its own heads for `knownHeads` (hello and `read`) but does not compute equivocation proofs itself; the asker (a mirror) compares. `Sequencer.profile(actor, seq)` gives a mirror the `profile_source` it needs.
24. Durability is only `"local"` (SQLite WAL, `synchronous=FULL`). A space whose `minDurability` is `replicated` gets `unavailable` (R-08), as wire.Home does.
25. A storage error after wire.Home has already advanced its in-memory state makes the sequencer stop serving (`unavailable`) rather than risk a receipt for a record that is not on disk; restart reloads from disk. The reload re-verifies every stored receipt chain and signature and refuses to start on a mismatch.
26. Moves (`space.move`) need the target server's key, which this server doesn't know, so a move is `invalid` here; a stored move would be honoured on reload (`moved`).

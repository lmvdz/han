# Carrier vectors (`han/1`, offline)

Vectors for the parts of the QUIC binding `han/1` (architecture.md §8.2) that need no QUIC stack: the peer filter, frames, the control stream, the well-known document, invite tickets and the dial check, the hello bytes and binding, stream items, typed streams, strict Ed25519 and the header checks the Log makes (the nonce grammar, strict keys at parse). They are pure inputs and outputs, so any implementation checks them offline; live carrier tests (QUIC, dialling from addresses, a revoked device disconnected, a ticket used once) are `crates/han-net/tests/live.rs` and `revoke.rs` (M2, architecture.md §10); the vectors here are replayed by `crates/han-net/tests/carrier_vectors.rs` and `crates/han-core/tests/carrier.rs`.

- Written by `python ref/make_carrier_vectors.py`; expectations are hand-written from the spec, only keys, signatures and encodings are computed.
- Checked by `python ref/run_carrier_vectors.py` against `ref/carrier.py` and `ref/wire.py`; `--mutations` shows the vectors catch single-rule mutations of both.

## Format

Every file is one JSON object: `name` (the file's kind, below), `rules` (the rule ids it pins), `description`, and its cases. Byte strings are lowercase hex (`hex`, `msg`, `data`, `message`); keys, signatures, node ids and secrets are base64url without padding (§2). A case's `name` or `note` is informative only.

| File | Function under test | A case |
|---|---|---|
| `admit.json` | N-02 `admit(conn, spaces)` (han.ts `PeerFilter`; the member tier per space, round 32 Z7) | `conn` (han.ts `AdmitConn`), `spaces` (the file's, or the case's own; han.ts `AdmitSpaces`), `expect`: `{allow: true, tier}` or `{allow: false}`. Compare `allow`, and `tier` when allowed; the reason is free text |
| `frames.json` | G-07 | `encode`: `text` → `hex` (4-byte big-endian length, then the UTF-8 bytes). `decode`: `hex` → `{frame, rest}`, `"more"` (incomplete), or `"close"` (a length over 1,310,720, or a payload that isn't UTF-8). `requests`: the frame `text` (and `test_mode`) → `null` (a request frame: go on to `hello`/`admit` and args) or `[id, close]`: answer `{id, error: invalid}`, and close the connection when `close` |
| `control.json` | G-07, G-08, N-03 (round 32 Z4, SR-13) | `dials`: a ticket's or hint's `node` for `server`, and the `wellknown` fetched from `server` → `"ok"` (dial it) or `"abort"` (the node isn't the server's). `challenges`: the control stream's frames so far, `dialed`, the `node` reached and dialed's well-known `nodes` → `"ok"` (say `hello`) or `"abort"`. `proofs`: `challenge`, `dialed`, the server's `node` → the exact `message` a proof signs, and whether `proof` verifies under `publicKey` (`valid`); with `checkedAs`, the proof must not verify for that node. `bindings`: `who`, `keyId`, the connection's `node`, and `keys` (a user's pinned profile keys, or a server's well-known) → `"ok"` or `"denied"` |
| `wellknown.json` | §4.3 `WellKnownHan`, `NodeAddr` | `doc` → `"ok"` or `"invalid"` |
| `tickets.json` | N-03 `InviteTicket` | `text` → the parsed ticket, or `"invalid"`; a parsed ticket must also encode back to exactly `text` |
| `stream-items.json` | G-05 `StreamItem` (with `profile` items, whose body the server never checks, round 32 Z6); a face's check of `han.status/1` bodies | `cases`: `item` → `"ok"` or `"invalid"`; `right`: the `sessionAccess` right it needs (`watch` or `read`). `bodies`: a `han.status/1` `body` → `"ok"` or `"invalid"` (spec/profiles/han.status-1.md, with the SR-59 text bounds) |
| `typed-streams.json` | G-07 typed one-way streams (round 32 Z5) | `encode`: `type`, `frames` → `hex`. `cases`: a stream's bytes `hex` and whether it is `finished` → `{type, frames}`, `"more"` or `"close"` |
| `ed25519.json` | §2 strict Ed25519 | `publicKey` → `key` (`"ok"`, or `"invalid"` where it is parsed); `publicKey`, `msg`, `sig` → `valid` |
| `header.json` | §2, R-25: the Log's header checks | a full Entry header `entry` → `"ok"` or `"invalid"`: the nonce grammar, and a `key.add`'s public key judged strictly at parse |

An implementation passes when every case gives the expected result. The signed message of a `han/1` hello proof is `"han-hello-v2\n" ‖ challenge ‖ "\n" ‖ dialed ‖ "\n" ‖ the server's node id` (round 32; the legacy WebSocket carrier keeps round 31's `"han-hello-v1\n" ‖ challenge ‖ "\n" ‖ dialed`). The core signs no bulk data: a typed stream's frames are its profile's, which signs them itself (han.timeline/1, TL-93). Test keys are Ed25519 keys whose private key is `sha256(UTF-8 seed)`, seeds `han-carrier-<name>` (the same derivation as `conformance/wire-vectors/config.json`).

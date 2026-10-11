# HAN wire conformance vectors

What a server must put on the wire, as frames. Built from the transcripts in `interop/golden/` (D15 of `interop/DECISIONS-r21.md`): every operation file is something **both** reference servers (Python `impl/`, clean-room TypeScript `cleanroom-ts/`) answered byte for byte alike for both clients. The rules they pin are architecture.md §2, §3, §4.1, §4.3, G-01…G-11.

```
python ref/run_wire_vectors.py --url ws://127.0.0.1:8787/han     # a server already in test mode
python ref/run_wire_vectors.py --launch py | ts                  # start a reference server in test mode, run, stop it
python ref/run_wire_vectors.py --check                           # offline: the files are well formed and self-consistent
```

## How a server passes

1. Start it in **test mode** with an empty data directory (G-11): ServerName `a.localhost`; a **frozen clock** (wall time `2026-01-01T00:00:00Z`, steady time 0, never advancing, so every `receipt.time` is that instant and every `homeSig` is reproducible); the server key of `config.json` (`keyId` `h1`, `from` `1970-01-01T00:00:00Z`, private key = `sha256(UTF-8 seed)`); the profile spaces of `config.json`'s actors provisioned (`@alice`, `@bob`, `@carol`, `@vera` human, `@x` service, key `k1` each). How a server is put into this mode is its own business (flags are not specified). `interop/py_host.py` and `interop/ts_host.mjs` are two examples, and `--launch` uses them. The stock CLIs have no frozen clock (`python -m han.seq` has `--register` but no clock option; `node dist/seq.js SERVER DIR PORT` provisions nobody), so they fail every receipt `time` and `homeSig`.
2. Run the runner against its URL. **A server passes when every vector passes**: each `expect` step matched the next frame on its connection after the masking below, and no frame arrived that no step expected.
3. Run once per fresh server: the operation files build on one another (seq numbers, ids), in the order of `index.json`; the probe files follow.

Vectors never depend on the server's key beyond the config: `check` verifies that every client entry is validly signed, every receipt in the files carries a valid `homeSig` under the config key, the 18 receipts of `s1` form one chain, and every record's entry id matches its receipt.

## Files

| File | What |
|---|---|
| `config.json` | the test-mode contract: server name, clock, server key, actors with seeds and public keys, blob generator and hashes, the nine `Limits`, runner defaults |
| `index.json` | the order to run the files in |
| `01-hello.json` … `21-redact.json` | one operation each: hello, space.create, member.set, message, read, read.state, follow, subscribe, signal, propose, approve, blobPut, blobGet, blobPut.multipart, blobGet.multipart, session.open, session.input, stream+watch, session.close, task, redact |
| `probes.json` | error paths and edges both reference servers answered alike, and the text confirms (`kind: agreed`) |
| `probes-decided.json` | paths where the two servers differed or the text was silent, now fixed by DECISIONS-r21.md and G-07…G-11 (`kind: decided`). An implementation that has not converged fails these |

`ref/make_wire_vectors.py` regenerates the directory from `interop/golden/` (provenance only).

## Format `han-wire-vector/1`

An operation file is one vector `{format, name, description, rules, steps}`; a probe file is `{format, kind, note, vectors: [{id, title, expect?, rules?, source?, steps}]}`. `steps` run in order. Every step except `http_get` names a connection with `conn`:

| Step | Meaning |
|---|---|
| `{conn, send: Frame}` | write the request frame as one text message (after placeholder expansion, below) |
| `{conn, send_raw: "text"}` | write this exact text message (used for frames that are not valid requests) |
| `{conn, send_binary: "hex"}` | write a binary message |
| `{conn, expect: Frame}` | the **next** message on that connection must be a text frame equal to this one (matching below) |
| `{conn, expect_close: true}` | the connection must be closed by the server (a close frame or end of stream) before the timeout |
| `{conn, expect_silence: true}` | no message arrives for `silenceMs` (400 ms) |
| `{http_get: "/path", expect_status, expect_type, expect_json}` | `GET` on the HTTP(S) origin of the WebSocket URL; the body is I-JSON equal to `expect_json` |

**Connections.** A label opens its own WebSocket. A label that is an ActorId (`@alice:a.localhost`) is **authenticated** before its first step with a set-up `hello` that is not part of the vector, unless that label's first step is a `hello` send or the expectation of the Challenge frame; any other label (`raw:1`) stays unauthenticated. Every connection first receives the server's Challenge, which the runner reads and checks (shape: `{challenge, server, versions, time}`, challenge 22–128 characters of `[A-Za-z0-9_-]`); only a step that expects it (`01-hello`, D11a, D12a, D9a) sees it. Messages on different connections are compared per connection, not in global order.

**Frames** are the JSON of the message. Request ids are as written. The runner sends compact JSON (no spaces); servers' text formatting is irrelevant.

**Comparison.** `expect` is equal to the received frame when both parse (I-JSON: a duplicate key, a fractional number or an exponent in the server's text is a failure) to **equal values of equal type with the same members**; that is equality of their JCS bytes, so key order and whitespace do not matter and everything else does, including every hash, signature and time. Before comparing, the received frame is masked:

| Masked | Rule |
|---|---|
| `error.message` | removed from the received frame (free text, never normative). `code`, `movedTo` and `retryAfterMs` stay and must match |
| blob `data` longer than 300 characters | replaced by `<b64u len=N sha256=H>`, `N` the length in characters and `H` the first 16 hex digits of the SHA-256 of the string's UTF-8; the vector holds the masked form |

Matchers (a string value in `expect` that stands for a class of values):

| Matcher | Accepts |
|---|---|
| `<challenge>` | 43 characters of canonical base64url (32 random bytes), D12 |
| `<challenge-shape>` | 22–128 characters of `[A-Za-z0-9_-]` |
| `<receipt>`, `<receipt seq=N>` | a Receipt (exactly its ten members) whose `home` and `homeKey` are the server and key of the config, whose `homeSig` verifies under that key, whose `id` is the id of the entry sent in the request with that frame id, and whose `seq` is `N` if given |
| `<url-of-the-binding>` | a `ws://` or `wss://` URL ending in `/han` (the `sequencer` of `/.well-known/han`) |

**Placeholders in client frames.**

| In `send` | Expands to |
|---|---|
| `"proof": "<proof>"` in a `hello` | the Ed25519 signature of the `who` actor over `"han-hello-v1\n" ‖ challenge ‖ "\n" ‖ "a.localhost"`, `challenge` being the one this connection received |
| `<proof challenge=X>`, `<proof dialed=Y>` | the same over another challenge string, or another dialed name (the server must answer `denied`) |
| `"data": "<b64u len=N sha256=H>"` in a `blobPut` | the base64url of the next `N*3/4` bytes of the blob generator starting at `args.offset` (byte *i* of a blob is `(i*7+3) mod 251`; `config.json` lists the two blobs by hash). The runner checks that the expansion hashes to `H` |
| `"<repeat c n>"` (a whole string) | the character `c` repeated `n` times |

After a `send` of `subscribe`, `watch` or `read` with `follow`, the runner waits `settleMs` (250 ms) so the registration has happened before another connection acts. A server that registers a follow asynchronously and slower than that is not conforming to G-09 ("atomically with registration").

## Hello, byte by byte (worked example)

The signed bytes of a `hello` for `@alice:a.localhost` (seed `han-interop-key-alice`) on a connection whose challenge is the 43 characters `AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA`, dialed `a.localhost`:

```
68616e2d68656c6c6f2d76310a            "han-hello-v1\n"
41 x 43                                the challenge as sent: 43 UTF-8 bytes, not decoded
0a                                     "\n"
612e6c6f63616c686f7374                 "a.localhost"
```

The Ed25519 signature of those bytes, base64url without padding, is `MlgcxEfCrmI7yfYRMa-ZI8vfUYNGtF9dV75MYk55TT6Y3WkUB5yeIvG-8K0dY7N8gPXwGK_LzoR8m1NSSZALAA`.

## Counts at the time of writing

21 operation files (109 expectations), 39 agreed probe vectors (50 expectations), 22 decided probe vectors (44 expectations): 82 vectors, 203 expectations, 335 steps. The results against the two reference servers are in architecture.md §10 and `ref/RESULTS.md`.

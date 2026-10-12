## 4. Root causes, ranked

Counting rule: one failure = one check that did not match the spec reading on one implementation (a probe on one server, or a matrix cell in a required direction; section 1, 2, 3.4). 22 failures in all (Python server 15 probes, TS server 4 probes, 3 matrix cells), plus one state-view difference (W-51) found by reading the executor cells. Where a cell repeats in a baseline (ts->ts) it is added in brackets. "Text" says whether the architecture.md / han.ts wording is clear enough to say who is wrong; where it is silent the entry is a gap and both behaviours are legal today.

Headline: **every normal operation interoperates** (22 client operations plus verifier plus executor in both directions; byte-identical receipts from the two servers for the same entries, see golden/). The divergences are in error paths, argument validation, routing and server-to-server surface, plus one URL path.

| # | root cause | failures | W-xx | who diverges from the text |
|---|---|---|---|---|
| 1 | Foreign/unknown-space routing and `host`: `moved` is not returned consistently, `host` exists in neither server | 5 (P18 py; P56 py+ts; P58 py+ts) | W-13, W-19 (G-06 now exists) | Python for P18; both for P56 and P58 (text: "Not home? Answer moved"; G-06) |
| 2 | Python's frame and `args` checking is open; non-I-JSON reply id | 4 (P09, P10, P11, P22 py) | W-04, W-21a | Python on P11 (text: "no valid id/call/args" is `{id:0, error:invalid}`); P09, P10, P22 text silent |
| 3 | Python lacks "uploaders may read their own unreferenced blobs for 24 h" | 3 (P37, P38, P39 py) | W-29 | Python (G-01 last paragraph is explicit) |
| 4 | Python base64url decoding is lenient (padding, non-canonical last character) | 2 (P34, P28b py) | W-17, W-27 | Python on padding (text: "base64url without padding"); canonical form: text silent |
| 5 | Executor started before the space exists: `read` answers `not_found`; TS executor swallows it and never retries | 2 (exec-ts early, ts->py, [ts->ts]) | **new W-48** | neither (text silent); the Python executor retries and works |
| 6 | (round 21, resolved as C21) TS verifier hard-coded the server key id `srv1` for `@:S`; could not verify a server-actor entry signed under any other key id | 2 (profile verification ts->py, [ts->ts]) | W-10 | TS verifier (text: keys of `@:S` are the server's published keys) |
| 7 | WebSocket path: TS server only upgrades `/han`; Python accepts any path, its README and well-known publish a path-less URL | 2 (P51 ts; py->ts `hello` PASS*; without the workaround all 25 py->ts operations are blocked) | W-07 | Python docs/well-known (text: `wss://<server>/han`) |
| 8 | `blobPut` failure model: Python never errors, it answers `{have, complete:false}` for a gap and for a hash mismatch | 2 (P32, P33 py) | W-27 | text silent (han.ts: `have` = bytes held so far) |
| 9 | Python `hello` result omits `limits.maxClockSkewSeconds` | 1 (P50 py) | **new W-49** | Python (han.ts `Limits` lists nine members) |
| 10 | `signal` `from`: TS does not accept it at all | 1 (P27 ts) | W-23 | TS (G-04: "a relayed signal carries `from`"); han.ts `BindingCalls.signal` omits it |

Not failures but disagreements where the text is silent (four more gaps, each seen on the wire): hello for an unknown actor, Python `denied` vs TS `invalid` (P05, W-03); `blobGet` without access, Python `denied` vs TS `not_found` (P36, W-29a); challenge length, Python 32 chars vs TS 43 (P49, W-01); `/.well-known/han` and `/han-peers` shape (new W-50, below).

### Root cause 1: routing of spaces the server does not host, and `host`

Frames (section 3.2, P18, P56, P58). Python: `append` of a `space.create` for `q1:other.localhost` answers `{"error":{"code":"invalid","message":"space id names another home"}}`; TS answers `{"code":"moved","movedTo":"other.localhost"}`. For `read` of `q1:other.localhost` both answer `not_found` ("no such space"). `host` (G-06) is `invalid` ("unknown call host") on both; the text says a non-server caller gets `denied`.
Proposed wording (§4.1 step 2, replacing "Not home? Answer `moved` with the current home."):
> A sequencer that does not host a space answers EVERY call that names it (`append`, `read`, `subscribe`, `signal`, `blobGet`, `watch`, `stream`) with `{"code":"moved","movedTo":<ServerName>}` when the space id's ServerName part is not its own, where `movedTo` is that ServerName; and with `not_found` (read-type calls) or `invalid` (an `append` whose kind is not `space.create`) when the ServerName is its own and the space does not exist. A `space.create` for a space id naming another ServerName is `moved`, never `invalid`. A server that implements G-06 answers `host` from a connection that is not authenticated as a server actor with `denied`; a server that does not implement it answers `host` with `invalid` and message `unsupported`.

### Root cause 2: the request envelope and `args`

Frames (3.2, P09, P10, P11, P22). Python: `{"id":7,"call":"read","args":{"space":"s1:a.localhost","after":0.5}}` is answered `{"id":0,"error":{"code":"invalid","message":"bad frame"}}` (TS: id 7); `{"id":5,"call":"cancel","args":{"target":1},"extra":true}` and `{"id":5,"call":"cancel"}` are answered `{"id":5,"ok":null}` (TS: `{"id":0,"error":invalid}`); `read` with `"bogus":1` is served.
Proposed wording (§8.3, after "A frame that doesn't parse ..."):
> A request frame is one JSON object with exactly the members `id` (integer in [0, 2^53)), `call` (string) and `args` (object, present even when empty). A frame with another member, a missing member or a member of another type is `invalid`. `args` of every call is closed: a member not listed for that call in §3, §8.3 and han.ts is `invalid`. The error reply carries the request's own `id` when the text parses as JSON and has an `id` member that is an integer in [0, 2^53), including when the text then fails I-JSON; otherwise it carries `id` 0. A client that receives an error with `id` 0 MUST treat every request still open on that connection as failed.

### Root cause 3: own unreferenced blobs

Frames (3.2, P37 to P39). After a complete `blobPut`, the uploader's `blobGet {"ref":…,"space":"s1:a.localhost"}` is `{"error":{"code":"denied","message":"blob not available"}}` on Python and `{"ok":{"data":"cHJvYmUgYmxvYiBieXRlcw","size":16,"offset":0}}` on TS. P38 and P39 (offset past the end, oversized `length`) are then untestable on Python.
Proposed wording (G-01, replacing the one-line paragraph) plus the range rules the text lacks (W-29c):
> `blobGet(ref, space)` also succeeds, whether or not any record references `ref`, when the caller is an actor whose own `blobPut` calls completed `ref` less than 86,400 seconds earlier on this server's clock; `space` must still be a well-formed SpaceId but is otherwise ignored. `offset` greater than `ref.size` is `invalid`; `offset` equal to `ref.size` returns `data: ""`. `length` absent means `maxBlobRangeBytes`; `length` greater than `maxBlobRangeBytes` is clamped to it; `length` 0 is `invalid`. `data` is base64url without padding.

### Root cause 4: base64url decoding

Frames (P34, P28b). Python accepts `blobPut.data` ending in `=` (`{"have":16,"complete":true}`) and an entry whose `sig` has non-zero trailing bits (it orders it and answers a receipt). TS answers `invalid` ("data must be base64url without padding"; "bad signature").
Proposed wording (§2 Encoding, after "both are base64url without padding."):
> A base64url string is valid only if it uses the alphabet `A-Z a-z 0-9 - _`, has no `=`, has a length that is not 1 mod 4, and re-encodes to itself (so the unused low bits of the last character are 0). A receiver MUST reject anything else as `invalid`. This applies to `publicKey`, `sig`, `homeSig`, `proof` and `blobPut.data`.

### Root cause 5 (new W-48): following a space that does not exist yet

Frames (3.4): the TS executor sends `{"id":2,"call":"read","args":{"space":"e1:a.localhost","after":0,"follow":true}}` before the space exists and is answered `{"id":2,"error":{"code":"not_found","message":"no such space"}}` (both servers); `exec.ts` ignores the failed stream (`s.done.catch(() => {})`) and nothing ever re-reads. The Python executor retries every 0.2 s and picks the effect up. The text does not say whether a follow of a missing space waits.
Proposed wording (§3, `read` row, after "A reader who may read no seq gets `not_found`, the same as for a missing space."):
> `read` with `follow` of a space that does not exist, or that the reader may not read yet, is answered `not_found` at once and opens no stream; a server MUST NOT hold the request open. A follower that expects the space (an executor, a verifier, a host) MUST retry the read until it gets a stream or `denied`, waiting 1 s after the first failure and doubling up to 30 s. §4.5: an executor MUST do this for every space it is configured with.

### Root cause 6 (W-10): which keys sign `@:S`

Frames (3.4): `profile:@alice:a.localhost` seq 1 is authored by `@:a.localhost` with `keyId` `h1`, receipt `homeKey` `h1`, `profiles` `{"@:a.localhost":0}`; `verify.ts` looked only for key id `srv1` (fixed in round 21, C21). With the TS sequencer's own key id `srv1` the same verifier passes on both servers (supplementary table), and so does the Python verifier everywhere. This run therefore also shows that the two servers pin the same `receipt.profiles` for profile creation (`{"@:S":0}`) and that W-09 (b),(c) are consistent, because the TS verifier replayed Python's profile receipts.
Proposed wording (§4.3, "Server actor"):
> The keys of `@:S` in the synthetic ProfileView are exactly the keys listed in S's `/.well-known/han` whose window `[from, until)` contains the receipt time, each with status `valid` and its listed `keyId`; a KeyId for a server key is any string matching the KeyId grammar. A verifier MUST NOT assume a particular KeyId for a server key. The key that signs a receipt (`homeKey`) and the key that signs an entry authored by `@:S` are chosen independently from that set.

### Root cause 7 (W-07): WebSocket path

Frames (3.1): the Python client dials `ws://127.0.0.1:PORT`; the TS server answers the upgrade `HTTP 400` (`{"ev":"upgrade-fail","status":400,"path":"/"}`); `…/han` works. Python's `/.well-known/han` publishes `"sequencer":"ws://127.0.0.1:PORT"` with no path, so a client that follows the document to the letter fails on a TS server, and the TS document (`…/han`) would work on both.
Proposed wording (§3 first paragraph and §4.3):
> The generic binding is served at the path `/han` of the WebSocket endpoint. A server MUST upgrade on `/han` and MAY upgrade on other paths. The `sequencer` member of `/.well-known/han` is a complete URL including that path. A client dials exactly that URL, or `wss://<server>/han` when it has none; it never dials a bare host.

### Root cause 8 (W-27): `blobPut` errors

Frames (3.2, P32, P33). Python: part at offset 5 with nothing held: `{"ok":{"have":0,"complete":false}}`; 3 bytes that do not hash to `ref.hash`: `{"ok":{"have":3,"complete":false}}`. TS: `invalid` for both. A client written for either misreads the other: the Python client resumes at `have`, which loops forever against a hash mismatch only because it has a guard (`res["have"] == offset`).
Proposed wording (§8.3, G-01):
> `blobPut {ref, offset, data}`: `offset` greater than the bytes held from this uploader for `ref` is `invalid` (parts are contiguous). Bytes that overlap bytes already held MUST equal them, else `invalid`. `offset + len(data)` greater than `ref.size` is `invalid`. When the held bytes reach `ref.size` and do not hash to `ref.hash` they are discarded and the call is `invalid`. After `complete: true` a repeat of any part answers `{have: ref.size, complete: true}`. A reply `{have, complete}` is only ever an `ok`.

### Root cause 9 (new W-49): `Limits` in the hello result

Frames (3.2, P50): Python `{"limits":{"maxFrameBytes":1310720,…,"maxFollows":256}}` (eight members); TS adds `"maxClockSkewSeconds":60`. han.ts `Limits` has nine.
Proposed wording (§3 `hello`): "`limits` has exactly the nine members of han.ts `Limits`, all integers. A client MUST ignore members it does not know and MUST NOT assume a default for one that is missing: it fails the connection."

### Root cause 10 (W-23): `signal` from

Frames (3.2, P27): alice sends `signal {space, body, from: "@bob:a.localhost"}`; Python `denied` ("a server may relay signals only from its own actors"), TS `invalid` ("signal args"). G-04 now says a relayed signal carries `from`; han.ts `BindingCalls.signal.args` still lists `{space, body}` only.
Proposed wording (han.ts and §8.3): `signal: { args: { space: SpaceId; body: Json; from?: ActorId }; ok: null }`, and: "`from` is accepted only on a connection authenticated as a server actor `@:S` and only for an ActorId on S; any other use is `denied`. Items delivered to followers carry `from` = the relayed actor, or the connection's actor when `from` is absent."

### New gap W-50: `/.well-known/han` and `/han-peers` documents

Seen in `runs/*/wellknown.json`. Python `/.well-known/han`: `{"server":"a.localhost","sequencer":"ws://127.0.0.1:PORT","keys":[{"keyId":"h1","publicKey":…,"from":"1970-01-01T00:00:00Z"}]}`. TS: `{"sequencer":"ws://127.0.0.1:PORT/han","keys":[…]}` (no `server`). `/han-peers`: Python `{"a.localhost":[keys]}` (its own keys included), TS `{}` (peers only). The text gives `{keys:[…]}` and `{server: keys}`. A TS verifier and the Python verifier both consumed `keys` arrays, so the intersection works.
Proposed wording (§4.3): "`/.well-known/han` is `{server: ServerName, sequencer: URL, keys: [Key]}` with all three members; `Key = {keyId, publicKey, from, until?, compromised?}`, times in the Time grammar. `/.well-known/han-peers` is `{ServerName: [Key]}` and lists every server whose keys this server verified with, and not itself. Both are `200 application/json`."

### New gap W-51: the StateView is not pinned byte for byte (claim `settled`)

Found by reading the final state after an executor run (section 1.1; `runs/exec-*/exec-result.json`). A claim with an outcome and no reservation is `{approval, attempt, id, instance, outcome, started, time, window}` on the Python server and the same plus `"settled": true` on the TS server (`policy.ts` lines 893 to 922 set it for unreserved claims; `ref/policy.py` sets it only when there is a reservation; han.ts: `settled?: true; // P-06 reservation`). A Policy replay on either side reaches the same verdicts, so no verifier disagrees, but `read {state:true}` differs, and so would any state hash. Every other part of the final state of the scenario (roles, policy, tasks, effects, sessions, held) is identical on both servers.
Proposed wording (§6.1 State and context): "`settled` is present, with value `true`, only on a claim whose reservation was released or consumed (P-06). A claim without a `reserved` member never has `settled`. A StateView is the JCS of exactly the members of han.ts `SpaceState` minus `entries`; an optional member that is absent is omitted, never `null`."

### Other gaps seen on the wire, no failure

W-01 and W-02: hello proofs interoperate in all four pairs with the transport on `127.0.0.1:PORT` and `dialed` = the ServerName (so the reading "the UTF-8 bytes of the challenge as sent; `dialed` is the ServerName, never host:port" is what both do). Challenge: 32 chars (Python) vs 43 (TS), both `[A-Za-z0-9_-]`. W-03: P05. W-05: P31 (two pipelined appends are ordered by both). W-06: every Python client `read` sends `cancel` for the already-ended id (golden/read.json): TS answers `ok null`, so W-06 is load-bearing. W-09/W-10/W-15 settled by identical receipts (golden/). W-16 (a)-(d): P15, P29, P30 agree. W-21/W-22: `subscribe` and `read follow` both delivered records in all pairs; `signal` items are delivered to a `subscribe`. W-31/W-32: P41 to P48 agree on every code; P57 shows both servers leave a `watch` open silently when its session is closed. W-34/W-35 (executor): Python and TS executors both built a valid claim (`window` 15, `attempt` 1, instance `k1`) against the other's server. Not exercised: federation, G-03/G-04 relays, G-06 `host`, moves, key rotation, executor crash recovery, R-16 clock tests.

## 5. Harness and reproduction

`interop/` (nothing outside it was edited; no commit made):
- `run.py` runs every cell, one server of each kind at a time, on free localhost ports, each behind `tap.mjs` (a transparent WebSocket proxy that logs every frame to `runs/<cell>/tap.jsonl`, with `/__mark` markers the drivers write at each operation). Cells: client x server x clock (`live` = real clocks and the unmodified `python -m han.seq`; `frozen` = both servers' clocks at 2026-01-01T00:00:00Z, fixed keys, nonces and entry times, for the golden transcripts), executor cells (`early` and `late` start), and `-srv1` cells.
- Adapters (the TS tree has no provisioning CLI, so these are needed): `ts_host.mjs` (TS sequencer with actors provisioned from a list), `py_host.py` (Python sequencer with a frozen clock, golden runs only), `ts_driver.mjs` and `py_driver.py` (the same scenario through `HanClient` and `han.client.Client`), `ts_verify.mjs` (follows a space with `verify.ts`; the Python verifier is `python -m han.verify follow --once`), `ts_exec.mjs` (runs `exec.ts`; the Python executor is `python -m han.exec`), `probe.mjs` (59 raw-frame probes), `lib.py` (keys, scenario, ports, process helpers), `report.py` (this file and `golden/`).
- Run: `cd interop; ..\impl\.venv\Scripts\python.exe run.py` (about 8 minutes), then `..\impl\.venv\Scripts\python.exe report.py`. Needs `cleanroom-ts` built (`npm run build`).
- `golden/`: one JSON per operation (`hello`, `space.create`, `member.set`, `message`, `read`, `read.state`, `follow`, `subscribe`, `signal`, `propose`, `approve`, `blobPut`, `blobGet`, `blobPut.multipart`, `blobGet.multipart`, `session.open`, `session.input`, `stream_watch`, `session.close`, `task`, `redact`) plus `probes.json` (error-path vectors both servers agree on) and `index.json`. Frames are what crossed the tap, keys and clocks fixed, request ids renumbered per connection, hello challenge/proof masked, long blob data summarised by length and hash. Every operation except `hello` is answered byte for byte the same by both servers for both clients (`pythonServerAndTsServerAgree: true`); `hello` differs only in `limits.maxClockSkewSeconds` (root cause 9).
- Limits of this measurement: one Python implementation and one TS implementation; the Python server and Python client share `ref/wire.py` and `ref/policy.py`, so "Python agrees with itself" says nothing about the spec; both sides had been tuned against the same vectors; probes encode my reading of the text (the `expect` column), and where a probe says "unspecified" no failure is counted.

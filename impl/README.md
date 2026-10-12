# HAN v1 minimal reference implementation (step 1)

A runnable sequencer (`han-seq`) and a client library over the generic binding (architecture.md §8.3: one WebSocket per connection). It imports `ref/policy.py` and `ref/wire.py` (`han/core.py` puts `../../ref` on `sys.path`; nothing is copied), so the conformance vectors test the code that runs. Plan: [PLAN.md](PLAN.md). Spec gaps and the readings taken: [NOTES.md](NOTES.md).

```
han/core.py      policy + wire from ref/, LIMITS, WireError
han/keys.py      Ed25519 key files (JSON): generate, load, public_of
han/entries.py   make_entry (id-relevant fields, nonce, time, contentHash, keyId, sig), default_policy
han/seq/         sequencer.py (pipeline, clock, R-16, fan-out, blobs, notices, streams) · store.py (SQLite) · server.py (frames, /.well-known) · federation.py (peers) · __main__.py
han/client/      client.py (Client, outbox + retry) · outbox.py (SQLite) · protocol.py (ClientProtocol: what actors need from a client; Client and MatrixClient satisfy it)
tests/           pytest-asyncio, a real server on a free port; each docstring names the rule it checks
```

## Setup

The venv `H:\HAN\v1\impl\.venv` already has every dependency, so nothing needs installing: run from `impl/` (`python -m han.seq`; pytest is configured with `pythonpath = .`). `pyproject.toml` also declares the package for `pip install -e .[dev]` (not exercised here).

## Run the server

```
python -m han.seq --server a.localhost --port 8701 --db seq.db --key server-key.json
```

`--key` is the server's Ed25519 key file (created on first start, key id `h1`). `--host` defaults to 127.0.0.1. Actors need a profile before they can connect; the server creates it (§4.3) from their public key:

```
# keys: python -c "from han import keys; keys.generate('alice.json')"
python -m han.seq register --server a.localhost --db seq.db --key server-key.json --actor @alice:a.localhost --pub alice.json
python -m han.seq register ... --actor @bot:a.localhost --pub bot.json --kind agent --operator @alice:a.localhost
python -m han.seq --server a.localhost --db seq.db --key server-key.json --register @bob:a.localhost=bob.json   # or at startup
```

(`register` writes to the database directly: run it while the server is stopped, or use `--register` at startup. Registering twice is a no-op.) An agent's operator then confirms by appending `member.set {actor: <operator>, role: "owner"}` to `profile:<agent>`.

State is in the `--db` file (SQLite, WAL, `synchronous=FULL`: records, receipts, pinned profiles, blobs, uploads, stream items; notices are never stored). On restart the server reloads the chain, re-verifies its receipts, and continues. Receipt time comes from a clock anchored at startup (R-03a); a clock restarted behind a space's last receipt makes appends `unavailable` until it catches up; after a restart an owner's abandon waits a full `A` (R-16).

## Federation: several servers

Each server serves `GET /.well-known/han` (`{server, sequencer, keys: [{keyId, publicKey, from, until?, compromised?}]}`) and `GET /.well-known/han-peers` (`{peers: [{server, url}]}`, the other servers it verifies with) **on the same port as the WebSocket**, over plain HTTP here (TLS is deployment). Servers find each other through a **han-peers config**, a JSON map from server name to the base URL of its `.well-known` (a name not listed is looked up at `https://<name>`):

```
python -m han.seq --server a.localhost --port 8701 --db a.db --key a-key.json --peers han-peers.json      # {"b.localhost": "http://127.0.0.1:8702"}
python -m han.seq --server b.localhost --port 8702 --db b.db --key b-key.json --peer a.localhost=http://127.0.0.1:8701 [--advertise wss://b.example/han]
```

What it does (NOTES.md items 28-37 give the readings; `tests/test_federation_*.py` check each):

| Duty | Behaviour |
|---|---|
| Remote actors | A user of `b` dials the **home** `a` (`Client(url_of_a, "@bob:b.localhost", ..., server="a.localhost")`). `a` reads `profile:@bob:b.localhost` from `b` as `@:a`, verifies the chain with `b`'s `.well-known` keys, keeps it, uses it for 300 s and then reads it again (R-10); receipts pin its seq (P-01). Revocations are honoured within that window. |
| Server keys | Windows `[from, until)`; a key changed for a seen window is a fault; a `compromised` key is trusted only for what `a` had recorded before it saw the mark. |
| Notices (G-03) | `a` orders a `member.set` for `@bob:b…` and sends `b` a notice; `b` checks it against `a` and delivers it only to bob's own follow of his profile. |
| Blobs (G-02) | A blob bob uploaded to `b` and referenced in a space on `a`: after ordering, `a` fetches it from the author's server (then the home), checks the hash and holds it for that space. |
| Signals (G-04) | A user of `b` signals a space homed on `a` over `b`'s connection; `a` accepts the relayed `from` only for `b`'s actors. |

`Federation(seq, peers)` (`han/seq/federation.py`) is attached by `python -m han.seq`; `SeqServer` asks it to prepare appends, hellos and reads, so `Sequencer` itself stays synchronous.

## Use the client

```python
import asyncio
from han import keys
from han.client import Client, default_policy

async def main():
    keys.generate("alice.json")                         # once; register its public key with the server as above
    c = Client("ws://127.0.0.1:8701/han", "@alice:a.localhost", "alice.json", "alice-outbox.db")   # dials server name a.localhost
    await c.connect()                                   # authenticates, then finishes any unconfirmed outbox entries (R-07)
    e, content = c.make_entry("space.create", "s1:a.localhost", {"policy": default_policy()}, {"name": "room"})
    receipt = await c.append(e, content)                # outbox first, send, retry with the same entry (R-06)
    async for item in c.read("s1:a.localhost", 0, follow=True): ...
    async for item in c.subscribe("s1:a.localhost"): ...          # records + signals, resumes after reconnects
    await c.signal("s1:a.localhost", {"typing": True})
    ref = await c.blob_put(b"bytes"); data = await c.blob_get(ref, "s1:a.localhost")
    await c.stream(item); async for it in c.watch(space, session): ...     # session streams (G-05)

asyncio.run(main())
```

`Client(url, actor, key_path, outbox_db, server=None)`: `server` is the dialed name bound into the hello proof (default: the actor's server part); a challenge naming another server aborts. `append(..., timeout=None)` retries forever on a lost connection or `unavailable`; definitive errors (`denied`, `invalid`, `not_found`) raise `WireError` and are recorded in the outbox.

## Tests

```
cd H:\HAN\v1\impl
.venv\Scripts\python.exe -m pytest -q
```

91 tests before federation, plus 16 in `test_federation_*.py` (the whole suite, with the other tracks' files, was 132 passed and 1 skipped in about 135 s); no network beyond localhost. Files: `test_e2e.py` (scenario, reads, follow, restart, hello, I-JSON, head gossip), `test_outbox.py` (R-01, R-06, R-07), `test_clock.py` (R-03a, R-16 with an injectable clock), `test_streams.py` (G-05, T-07), `test_wire_scenarios.py` (ports of `ref/run_wire.py`: content validation, header exactness, equivocation and wrong-home proofs, forbidden accept, notices, blobs, signals, dedupe after revocation, operator confirmation), `test_federation_core.py` and `test_federation_trust.py` (two servers `a.localhost` and `b.localhost`: `.well-known`, remote profiles and the 300 s cache, notices, agent with a remote operator, blob transfer, signal relay, revocation, `compromised`, a changed key window, a verifier over a remote profile, two `python -m han.seq` processes), `test_cli.py` (`python -m han.seq` as a subprocess, killed and restarted), `test_durability.py` (han-seq hard-killed during a burst of appends: no acknowledged receipt lost, no seq reused), `test_review_fixes.py` (context blobs, state at a past seq, follow with `limit`, the 60 s skew, gossip signals); the step 2 files are listed below.

The reference checks stay as they were: `python ref/run_wire.py`, `python ref/run_all.py --quick` (from `H:\HAN\v1`, `PYTHONIOENCODING=utf-8`).

## Executor, agent host, verifier, CLI (step 2)

| Program | Run | Details |
|---|---|---|
| `han-exec` | `python -m han.exec --url ws://…/han --actor @x:S --key KEY --instance k1 --journal PATH --spaces SPACE --actions file.write,http.post [--allow-url PREFIX] [--sandbox DIR]` | X-01…X-10; refuses to start (exit 3) if its instance is already running or its journal is missing. [han/exec/NOTES.md](han/exec/NOTES.md) |
| `han-host` | `python -m han.host --url … --actor @bot:S --key KEY --space SPACE --mode auto\|guarded --agent scripted\|claude --state PATH [--executor @x:S]` | P-15 and R-24 host duties, R-17/R-18 recovery; `claude` drives Claude Code through the Agent SDK (run live: [demo/README.md](demo/README.md)). [han/host/NOTES.md](han/host/NOTES.md) |
| `han-verify` | `python -m han.verify follow --url … --actor … --key … --space SPACE --state PATH --server-key S=KEYFILE` · `python -m han.verify proof FILE` | R-14/R-15 mirror; exit 0 ok, 2 faulty (writes a portable proof), 3 stopped, 4 moved. [han/verify/NOTES.md](han/verify/NOTES.md) |
| `han` | `python -m han.cli <keygen\|space create\|member set\|message\|task\|propose\|approve\|cancel\|session …\|agent release\|watch\|read\|status>` (or `HAN_URL`, `HAN_ACTOR`, `HAN_KEY`, `HAN_OUTBOX`) | thin over `han.client`; `approve` shows the plan and checks its digest first (P-08) |

Options added in the fix round:

- `han-exec --backoff-base SECONDS --backoff-cap SECONDS` (default 1 and 60): wait `base * 2^(n-1)` (at most `cap`) before re-claiming an effect after `failed`/`not started` (X-11). Effects it will not run (action not enabled, bad plan, outside the sandbox or allow-list) are never claimed; the proposer gets one `message` citing the proposal.
- `han-host --cost UNIT:AMOUNT`: the cost attached to a world action that names none, in a budgeted space. The host also names its open budgeted task; with none it says in its stream that it cannot spend.
- Wire/client: the call `state {space, at? | cursor? | effect?}` (`Client.state(space, at=seq)`; `read_state` merges the pages) returns one page of the state, after record `seq` with `at`; `read` is entries only; `blobGet` serves a live session's `context` blob to actors with the read right; `Client.read(..., follow=True, limit=N)` re-reads gaps by itself.
- Verifier: receipts may lead its clock by 60 s (`han.verify.MAX_CLOCK_SKEW_SECONDS`, the spec's `maxClockSkewSeconds`); in-space gossip is `{type: "heads", receipts}` and `{type: "fault", proof}`.

Tests: `python -m pytest -q` from `impl/` runs everything (steps 1 and 2, the Matrix binding against Synapse, round 20): 143 passed, 1 skipped (the live Claude run) in about 2 minutes.

Round 20: `Sequencer.add_ordered_hook` (bindings subscribe to every newly ordered record; the Matrix appservice no longer wraps `append`); `Executor(..., client=)` takes any `ClientProtocol`, so han-exec runs over `MatrixClient`; `tests/test_round20.py` covers read-at on the current state, R-16 and approver pinning.

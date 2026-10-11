# HAN v1 minimal reference implementation (step 1)

A runnable sequencer (`han-seq`) and a client library over the generic binding (architecture.md §8.3: one WebSocket per connection). It imports `ref/policy.py` and `ref/wire.py` (`han/core.py` puts `../../ref` on `sys.path`; nothing is copied), so the conformance vectors test the code that runs. Plan: [PLAN.md](PLAN.md). Spec gaps and the readings taken: [NOTES.md](NOTES.md).

```
han/core.py      policy + wire from ref/, LIMITS, WireError
han/keys.py      Ed25519 key files (JSON): generate, load, public_of
han/entries.py   make_entry (id-relevant fields, nonce, time, contentHash, keyId, sig), default_policy
han/seq/         sequencer.py (pipeline, clock, R-16, fan-out, blobs, notices, streams) · store.py (SQLite) · server.py (frames) · __main__.py
han/client/      client.py (Client, outbox + retry) · outbox.py (SQLite)
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

State is in the `--db` file (SQLite, WAL, `synchronous=FULL`: records, receipts, pinned profiles, blobs, uploads, notices, stream items). On restart the server reloads the chain, re-verifies its receipts, and continues. Receipt time comes from a clock anchored at startup (R-03a); a clock restarted behind a space's last receipt makes appends `unavailable` until it catches up; after a restart an owner's abandon waits a full `A` (R-16).

## Use the client

```python
import asyncio
from han import keys
from han.client import Client, default_policy

async def main():
    keys.generate("alice.json")                         # once; register its public key with the server as above
    c = Client("ws://127.0.0.1:8701", "@alice:a.localhost", "alice.json", "alice-outbox.db")   # dials server name a.localhost
    await c.connect()                                   # authenticates, then finishes any unconfirmed outbox entries (R-07)
    e, content = c.make_entry("space.create", "s1:a.localhost", {"policy": default_policy()}, {"name": "room"})
    receipt = await c.append(e, content)                # outbox first, send, retry with the same entry (R-06)
    async for item in c.read("s1:a.localhost", 0, follow=True, state=True): ...
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

38 tests, about 10 s, no network beyond localhost. Files: `test_e2e.py` (scenario, reads, follow, restart, hello, I-JSON, head gossip), `test_outbox.py` (R-01, R-06, R-07), `test_clock.py` (R-03a, R-16 with an injectable clock), `test_streams.py` (G-05, T-07), `test_wire_scenarios.py` (ports of `ref/run_wire.py`: content validation, header exactness, equivocation and wrong-home proofs, forbidden accept, notices, blobs, signals, dedupe after revocation, operator confirmation), `test_cli.py` (`python -m han.seq` as a subprocess, killed and restarted).

The reference checks stay as they were: `python ref/run_wire.py`, `python ref/run_all.py --quick` (from `H:\HAN\v1`, `PYTHONIOENCODING=utf-8`).

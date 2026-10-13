"""Helpers for the verifier tests: a verifier over a real client, and the ways a sequencer can lie (each built from the real one)."""
import asyncio, copy, os
from conftest import S
from han import keys
from han.core import P, W
from han.entries import make_entry
from han.verify import Verifier

def verifier(env, client, state=None, **kw) -> Verifier:
    """A Verifier that trusts exactly the server keys of the test server."""
    return Verifier(client, env.seq.server_keys, state, **kw)

async def until(pred, timeout=5.0):
    async def poll():
        while not pred(): await asyncio.sleep(0.02)
    await asyncio.wait_for(poll(), timeout)

def forge_head(env, author, rec, text):
    """A different, validly signed and validly receipted record for the same seq as `rec` (the home signs two histories)."""
    e0, r0 = rec["entry"], rec["receipt"]
    key = keys.load(os.path.join(env.tmp, f"{author}.key.json"))
    e, c = make_entry(key, e0["author"], "message", e0["space"], {}, {"text": text}, seen=e0["seen"], time=e0["time"])
    r = W.sign_receipt(dict(r0, id=P.entry_id(e)), env.seq.key)
    return {"entry": e, "content": c, "redacted": False, "receipt": r}

def doctor_reads(env, who, fn):
    """Make the sequencer show `who` a different story: fn(items) rewrites the items of every read/subscribe for that actor."""
    seq = env.seq
    orig_read, orig_sub = seq.read, seq.subscribe
    def read(w, *a, **kw):
        items, sub = orig_read(w, *a, **kw)
        return (fn(items) if w == who else items), sub
    def subscribe(w, *a, **kw):
        items, sub = orig_sub(w, *a, **kw)
        return (fn(items) if w == who else items), sub
    seq.read, seq.subscribe = read, subscribe

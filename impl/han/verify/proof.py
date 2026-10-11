"""Portable fault proofs (R-02, R-14): build, and check offline with only trusted server keys.

A proof is JSON: {v, kind, space, through, reason, receipts, record?, chain?, profiles?, serverKeys, observedAt?}.
- equivocation: two valid receipts of one home for one (space, seq) with different hashes: `receipts` alone.
- future-time: a valid receipt dated after `observedAt` + skew: `receipts`, `observedAt`.
- every other kind: `chain` (the verified records before the fault), `profiles` (the profile chains they pin) and `record`
  (the offending record). The checker replays them from nothing and must hit the same kind of fault at `record`.
`serverKeys` are what the prover used; a checker that does not trust them passes its own."""
from __future__ import annotations
import json, time as _time
from ..core import P, W
from .chains import Chains, classify, KINDS

SKEW = 300          # seconds a receipt may lead the verifier's clock (§7.5: "receipts dated in the future")

def make(kind: str, space: str, through: int, reason: str, receipts: list, server_keys: dict, record=None, chain=None,
         profiles=None, observed_at: str | None = None) -> dict:
    p = {"v": 1, "kind": kind, "space": space, "through": through, "reason": reason, "receipts": receipts, "serverKeys": server_keys}
    if record is not None: p["record"] = record
    if chain is not None: p["chain"], p["profiles"] = chain, profiles or {}
    if observed_at: p["observedAt"] = observed_at
    return p

def dumps(proof: dict) -> str: return json.dumps(proof, separators=(",", ":"), ensure_ascii=False)

def check(proof: dict, server_keys: dict | None = None, skew: int = SKEW) -> tuple[bool, str]:
    """(accepted, message). `server_keys` (trusted, {server: {keyId: {publicKey, from, until?}}}) override the bundled ones."""
    try:
        return _check(proof, server_keys, skew)
    except (KeyError, TypeError, ValueError, AttributeError, IndexError) as ex:
        return False, f"malformed proof: {type(ex).__name__}: {ex}"

def _check(proof, server_keys, skew):
    if not isinstance(proof, dict) or proof.get("v") != 1: return False, "not a v1 proof"
    kind = proof["kind"]
    if kind not in KINDS: return False, f"unknown kind {kind}"
    keys = server_keys if server_keys is not None else proof["serverKeys"]
    src = "trusted keys" if server_keys is not None else "keys bundled in the proof (not independently trusted)"
    rs = proof["receipts"]
    if kind == "equivocation":
        if len(rs) != 2: return False, "an equivocation proof carries two receipts"
        ok = W.equivocation_proof(rs[0], rs[1], keys)
        return ok, ("two valid receipts of one home for one seq with different hashes" if ok else "not an equivocation") + f" ({src})"
    if kind == "future-time":
        r = rs[0]
        if len(rs) != 1 or not W.verify_receipt(r, keys): return False, "no valid receipt"
        seen = proof.get("observedAt")
        if not (isinstance(seen, str) and P._time(seen)): return False, "observedAt missing"
        ok = P.ts(r["time"]) > P.ts(seen) + skew
        return ok, (f"receipt {r['seq']} dated {r['time']}, observed at {seen} (the prover's clock is trusted) ({src})" if ok else "receipt is not in the future")
    c = Chains(keys)
    for sp, recs in proof.get("profiles", {}).items():                 # the profile chains first: they verify themselves
        for rec in recs:
            ok, why = c.ingest(rec)
            if not ok: return False, f"profile chain {sp} does not verify: {why}"
    for rec in proof["chain"]:
        ok, why = c.ingest(rec)
        if not ok: return False, f"the chain before the fault does not verify: {why}"
    if kind == "wrong-home" and "record" not in proof:             # found by gossip: a bare receipt against the verified chain before it
        ok = len(rs) == 1 and W.wrong_home_proof(c.records(proof["space"]), rs[0], keys)
        return ok, ("a valid receipt signed by a server that was not the home at that seq" if ok else "not a wrong-home proof") + f" ({src})"
    rec = proof["record"]
    ok, why = c.ingest(rec)
    if ok: return False, "the record verifies; no fault"
    got = classify(why)
    if got != kind: return False, f"the record fails ({why}) but as {got}, not {kind}"
    attributable = W.verify_receipt(rec["receipt"], keys)
    note = "" if attributable else "; its receipt is not validly signed, so the fault is the delivery's, not provably the home's"
    return True, f"{kind}: {why}{note} ({src})"

def load_server_keys(specs: list[str] | None, files: list[str] | None = None) -> dict:
    """{server: {keyId: {publicKey, from}}} from `SERVER=KEYFILE|PUBKEY[,keyId]` specs (han.keys.public_of) and JSON files in the same shape."""
    from .. import keys
    out: dict = {}
    for spec in specs or []:
        server, rest = spec.split("=", 1)
        src, _, kid = rest.partition(",")
        key_id, pub = keys.public_of(src)
        out.setdefault(server, {})[kid or key_id] = {"publicKey": pub, "from": "1970-01-01T00:00:00Z"}
    for f in files or []:
        with open(f, encoding="utf-8") as fh:
            for server, ks in json.load(fh).items(): out.setdefault(server, {}).update(ks)
    return out

def fmt(epoch: float) -> str:
    return _time.strftime("%Y-%m-%dT%H:%M:%SZ", _time.gmtime(int(epoch)))

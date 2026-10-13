"""A verified view of chains: wire.Mirror (R-14) plus a profile source that folds the profile spaces the mirror itself has
verified. Used live by the Verifier and offline by `check_proof`; it hides how a ProfileView is rebuilt from a profile chain."""
from __future__ import annotations
from ..core import P, W

KINDS = ("equivocation", "wrong-home", "forbidden-accept", "bad-signature", "chain", "bad-redaction", "bad-move", "future-time")

def classify(why: str) -> str:
    """Map a wire.Mirror fault reason to a han.ts proof kind."""
    if why.startswith("home accepted a forbidden entry"): return "forbidden-accept"
    if "not the home" in why: return "wrong-home"
    if "redact" in why: return "bad-redaction"
    if why.startswith("move without"): return "bad-move"
    if why in ("entry signature", "receipt signature or key window"): return "bad-signature"
    return "chain"

class Chains:
    def __init__(self, server_keys: dict):
        self.server_keys = server_keys
        self.mirror = W.Mirror(server_keys, self.profile)
        self._views: dict[tuple, dict] = {}

    def records(self, space: str) -> list:
        S = self.mirror.spaces.get(space)
        return S["records"] if S else []

    def head(self, space: str) -> int:
        r = self.records(space)
        return r[-1]["receipt"]["seq"] if r else 0

    def faulty(self, space: str):
        S = self.mirror.spaces.get(space)
        return S.get("faulty") if S else None

    def profile(self, actor: str, seq: int) -> dict:
        """ProfileView of `actor` at profile seq `seq` (§4.3); a view with no keys when the chain isn't (yet) held."""
        if actor.startswith("@:"):                                   # the server actor: its keys, valid
            ks = self.server_keys.get(actor[2:], {})
            return {"actor": actor, "seq": 0, "actorKind": "service", "operatorConfirmed": False,
                    "keys": {k: {"publicKey": v["publicKey"], "status": "valid"} for k, v in ks.items()}}
        recs = self.records("profile:" + actor)
        if seq < 1 or seq > len(recs): return {"actor": actor, "seq": seq, "actorKind": "human", "operatorConfirmed": False, "keys": {}}
        view = self._views.get((actor, seq))
        if view is None:
            st = None
            for rec in recs[:seq]:
                st = P.fold(st, {"entry": dict(rec["entry"], id=rec["receipt"]["id"]), "content": None, "receipt": rec["receipt"]},
                            {"time": rec["receipt"]["time"], "profiles": {}})
            p = st["profile"]
            view = {"actor": actor, "seq": st["head"], "actorKind": p["actorKind"], "operatorConfirmed": p["operatorConfirmed"],
                    "keys": {k: dict(x) for k, x in p["keys"].items()}}
            if p.get("operator"): view["operator"] = p["operator"]
            self._views[(actor, seq)] = view
        return view

    def ingest(self, rec: dict) -> tuple[bool, str]:
        """wire.Mirror.ingest, but a record too malformed to even read is a fault, not a crash."""
        try: return self.mirror.ingest(rec)
        except (KeyError, TypeError, ValueError, AttributeError, IndexError, RecursionError) as ex:
            try: sp = rec["entry"]["space"]
            except Exception: return False, f"malformed record: {type(ex).__name__}"
            why = f"malformed record: {type(ex).__name__}"
            self.mirror.spaces.setdefault(sp, {"records": [], "state": None, "lastProfiles": {}})["faulty"] = why
            return False, why

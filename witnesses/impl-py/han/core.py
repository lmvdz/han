"""han.core: the reference Policy and wire layer, imported from ref/ (never copied), so the conformance vectors
test the running code. `policy` is the six pure functions (decide, fold, can_read, relevant_actors, session_access,
guard_status); `wire` holds ids, JCS, signatures, receipts, validation, Home and Mirror."""
import os, sys

REF = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "..", "ref"))
if REF not in sys.path: sys.path.insert(0, REF)

import policy                       # noqa: E402
import wire                         # noqa: E402

P, W = policy, wire

LIMITS = {"maxFrameBytes": 1310720, "maxHeaderBytes": 16384, "maxContentBytes": 65536, "maxBlobBytes": 4294967296,
          "maxBlobPartBytes": 786432, "maxBlobRangeBytes": 786432, "maxStreamItemBytes": 65536, "maxFollows": 256,
          "maxClockSkewSeconds": W.MAX_CLOCK_SKEW,
          "maxStateItems": getattr(W, "MAX_STATE_ITEMS", 1000)}   # han.ts Limits (D9, E8, H3): items of each paged map per page of a state; read at call time, test mode lowers maxFrameBytes


class WireError(Exception):
    """A han.ts WireError as an exception (server replies and client raises)."""
    def __init__(self, code, message="", retry_after_ms=None, moved_to=None):
        super().__init__(f"{code}: {message}")
        self.code, self.message, self.retry_after_ms, self.moved_to = code, message, retry_after_ms, moved_to
    def to_wire(self) -> dict:
        d = {"code": self.code, "message": self.message}
        if self.retry_after_ms is not None: d["retryAfterMs"] = self.retry_after_ms
        if self.moved_to is not None: d["movedTo"] = self.moved_to
        return d
    @classmethod
    def from_wire(cls, d: dict): return cls(d["code"], d.get("message", ""), d.get("retryAfterMs"), d.get("movedTo"))

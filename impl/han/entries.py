"""Building and signing entries (architecture.md §2)."""
import base64, os, time as _time
from .core import P, W

def fmt_time(epoch: float) -> str:
    """Time (§2): UTC, rounded down to the second."""
    return _time.strftime("%Y-%m-%dT%H:%M:%SZ", _time.gmtime(int(epoch)))

def new_nonce() -> str:
    return base64.b32encode(os.urandom(16)).decode().lower().rstrip("=")[:26]      # 128 bits, [a-z2-7]{26}

def make_entry(key, author: str, kind: str, space: str, body: dict, content=None, refs=(), seen: int = 0,
               key_id: str | None = None, time: str | None = None):
    """A signed entry and its content: fills v, contentHash, nonce, time, keyId and sig. Returns (entry, content)."""
    e = {"v": 1, "space": space, "kind": kind, "author": author, "keyId": key_id or key.key_id, "body": body,
         "contentHash": W.content_hash(content), "refs": list(refs), "seen": seen, "nonce": new_nonce(),
         "time": time or fmt_time(_time.time())}
    return W.sign_entry(e, key), content

def default_policy(**over) -> dict:
    """A complete PolicyDoc (han.ts) for a private team space; override fields by keyword."""
    p = {"visibility": "members", "join": "invite", "membersMayInvite": False, "allowAgents": "any", "approvals": [],
         "separateApprover": False, "budget": None, "maxDelegationDepth": 4, "minDurability": "local", "launchWindow": 30,
         "extensionsMinRole": "member",
         "sessions": {"watch": "member", "read": "member", "steer": "owner", "modes": ["auto", "guarded"]}, "servers": {"deny": []}}
    p.update(over)
    return p

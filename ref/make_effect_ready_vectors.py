"""Hand-expected readiness boundaries; generates only probes-effect-ready.json."""
import base64
import hashlib
import json
from pathlib import Path
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
import wire as W

ROOT = Path(__file__).resolve().parent.parent
ALICE, BOB, BOT = (f"@{n}:a.localhost" for n in ("alice", "bob", "x"))
steps, serial = [], 1


def request(actor, call, args, expected=None, error=None):
    global serial
    serial += 1
    steps.append({"conn": actor, "send": {"id": serial, "call": call, "args": args}})
    reply = {"id": serial, "error": {"code": error}} if error else {"id": serial, "ok": expected}
    steps.append({"conn": actor, "expect": reply})


def append(actor, space, kind, body, seq=None, content=None, error=None):
    name = actor.split(":")[0][1:]
    key = Ed25519PrivateKey.from_private_bytes(hashlib.sha256(f"han-interop-key-{name}".encode()).digest())
    nonce = base64.b32encode(hashlib.sha256(f"effect-ready-{serial}".encode()).digest()).decode().lower()[:26]
    entry = {"v": 1, "space": space, "kind": kind, "author": actor, "keyId": "k1", "body": body,
             "contentHash": W.content_hash(content), "refs": [], "seen": 0, "nonce": nonce, "time": "2026-01-01T00:00:00Z"}
    identifier = W.entry_id(entry)
    entry["sig"] = W.b64(key.sign(b"han-sig-v1\n" + identifier.encode()))
    request(actor, "append", {"entry": entry, **({"content": content} if content is not None else {})},
            f"<receipt seq={seq}>" if seq else "<receipt>", error=error)
    return identifier


append(ALICE, "profile:" + BOT, "member.set", {"actor": ALICE, "role": "owner"})
for action, automatic in (("vcs.try", True), ("vcs.land", True), ("vcs.try", False), ("vcs.land", False)):
    space = "ready" + action.split(".")[1] + ("auto" if automatic else "manual") + ":a.localhost"
    rule = {"action": action, "approvers": ["owner"]}
    if automatic: rule["proposer"] = True
    if action == "vcs.land": rule.update(repo="github:o/r", maxTier=1)
    policy = {"visibility": "members", "join": "invite", "membersMayInvite": False, "allowAgents": "with-operator",
              "approvals": [rule], "separateApprover": False, "budget": None, "maxDelegationDepth": 2,
              "minDurability": "local", "launchWindow": 30, "extensionsMinRole": "member",
              "sessions": {"watch": "member", "read": "member", "steer": "owner", "modes": ["auto", "guarded"], "forceGuarded": True},
              "servers": {"deny": []}, "gates": {"small": 20, "large": 500, "paths": [], "owners": []}}
    append(ALICE, space, "space.create", {"policy": policy}, 1, {"name": "Readiness"})
    append(ALICE, space, "member.set", {"actor": BOT, "role": "member"}, 2)
    append(ALICE, space, "member.set", {"actor": BOB, "role": "owner"}, 3)
    change = {"repo": "github:o/r", "deltaDigest": "a" * 64, "total": 1, "files": [{"path": "src/main.rs", "kind": "modify", "lines": 1}]}
    plan = {"action": action, "target": "github:o/r", "args": {"change": change}}
    digest = hashlib.sha256(W.P.jcs(plan)).hexdigest()
    body = {"executor": ALICE, "action": action, "planDigest": digest, "repeatable": False}
    if action == "vcs.land": body["change"] = change
    proposal = append(BOT, space, "effect.propose", body, 4, {"plan": plan})
    args = {"space": space, "proposal": proposal, "instance": "k1"}
    request(ALICE, "effectReady", args, error="denied")
    # A proposer-suffices rule refuses redundant non-guard approvals (P-17).
    append(BOB, space, "effect.approve", {"proposal": proposal, "planDigest": digest}, 5, error="denied" if automatic else None)
    request(ALICE, "effectReady", args, error="denied")
    guard_seq = 5 if automatic else 6
    append(ALICE, space, "effect.approve", {"proposal": proposal, "planDigest": digest}, guard_seq)
    request(ALICE, "effectReady", args)
    request(ALICE, "effectReady", args)
    request(BOB, "effectReady", args, error="denied")
    request(ALICE, "effectReady", {**args, "extra": True}, error="invalid")
    request(ALICE, "effectReady", {**args, "instance": "missing"}, error="denied")
    # These two readiness successes must not consume approvals or append entries.
    append(ALICE, space, "effect.claim", {"proposal": proposal, "instance": "k1", "attempt": 1, "window": 30}, guard_seq + 1)
    request(ALICE, "effectReady", args, error="denied")

vector = {"format": "han-wire-vectors/1", "kind": "effect-ready", "note": "Hand-expected authorization boundaries, not generated from a verdict oracle.",
          "vectors": [{"id": "ready-guards", "title": "Readiness waits for the actual guard for try and scoped landing, and never consumes authority",
                       "rules": ["P-07", "P-15", "P-17", "G-07"], "expect": "denied until guard approval; two successes append nothing and consume nothing", "steps": steps}]}
(ROOT / "conformance/wire-vectors/probes-effect-ready.json").write_text(json.dumps(vector, indent=2) + "\n", encoding="utf-8")

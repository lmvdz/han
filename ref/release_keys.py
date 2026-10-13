"""Release signing for `han update --check` (docs/release.md, "Self-update").

  python ref/release_keys.py keygen
      Prints a new Ed25519 key pair: the private seed (keep it secret) and the public key (base64url, the value
      of HAN_UPDATE_PUBKEY that is embedded in the binary).
  python ref/release_keys.py sign --version 0.2.0 [--notes-url URL] [--out latest.json]
      Writes the manifest and its detached signature (<out>.sig). The private seed is read from the
      environment variable HAN_UPDATE_SIGNING_KEY (64 hex digits), never from the command line.

The signature covers b"han-release-manifest/1\n" followed by the exact bytes of the manifest file, as
apps/han/src/update.rs expects. Signatures and keys are base64url without padding.
"""
import argparse
import base64
import json
import os
import sys

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives import serialization

DOMAIN = b"han-release-manifest/1\n"


def b64u(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def public_b64u(key: Ed25519PrivateKey) -> str:
    raw = key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
    return b64u(raw)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("keygen")
    sg = sub.add_parser("sign")
    sg.add_argument("--version", required=True)
    sg.add_argument("--notes-url")
    sg.add_argument("--out", default="latest.json")
    args = ap.parse_args()

    if args.cmd == "keygen":
        seed = os.urandom(32)
        key = Ed25519PrivateKey.from_private_bytes(seed)
        print("private seed (HAN_UPDATE_SIGNING_KEY, secret):", seed.hex())
        print("public key   (HAN_UPDATE_PUBKEY):", public_b64u(key))
        return 0

    seed_hex = os.environ.get("HAN_UPDATE_SIGNING_KEY", "").strip()
    if len(seed_hex) != 64:
        print("HAN_UPDATE_SIGNING_KEY must be 64 hex digits", file=sys.stderr)
        return 1
    key = Ed25519PrivateKey.from_private_bytes(bytes.fromhex(seed_hex))
    manifest = {"version": args.version}
    if args.notes_url:
        manifest["notes_url"] = args.notes_url
    body = json.dumps(manifest, sort_keys=True).encode()
    with open(args.out, "wb") as f:
        f.write(body)
    with open(args.out + ".sig", "w") as f:
        f.write(b64u(key.sign(DOMAIN + body)) + "\n")
    print("wrote", args.out, "and", args.out + ".sig")
    return 0


if __name__ == "__main__":
    sys.exit(main())

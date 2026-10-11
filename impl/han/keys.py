"""Ed25519 key files (JSON): {"keyId", "public", "private"}, base64url. Compatible with wire.Key (sign, pub)."""
import json, os
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives import serialization
from .core import W

class KeyPair(W.Key):
    def __init__(self, private: bytes, key_id: str = "k1"):
        self.sk = Ed25519PrivateKey.from_private_bytes(private)
        self.pub = W.b64(self.sk.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw))
        self.key_id, self._private = key_id, private
    def to_json(self) -> dict: return {"keyId": self.key_id, "public": self.pub, "private": W.b64(self._private)}

def generate(path: str | None = None, key_id: str = "k1") -> KeyPair:
    sk = Ed25519PrivateKey.generate()
    kp = KeyPair(sk.private_bytes(serialization.Encoding.Raw, serialization.PrivateFormat.Raw, serialization.NoEncryption()), key_id)
    if path: save(kp, path)
    return kp

def save(kp: KeyPair, path: str):
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)      # 0600 (a no-op for the mode bits on Windows)
    with os.fdopen(fd, "w", encoding="utf-8") as fh: json.dump(kp.to_json(), fh)

def load(path: str) -> KeyPair:
    with open(path, encoding="utf-8") as fh: d = json.load(fh)
    return KeyPair(W.unb64(d["private"]), d.get("keyId", "k1"))

def load_or_generate(path: str, key_id: str = "k1") -> KeyPair:
    return load(path) if os.path.exists(path) else generate(path, key_id)

def public_of(path_or_pub: str) -> tuple[str, str]:
    """(keyId, publicKey) from a key file (public part only is used) or a bare base64url public key (keyId k1)."""
    if os.path.exists(path_or_pub):
        with open(path_or_pub, encoding="utf-8") as fh: d = json.load(fh)
        return d.get("keyId", "k1"), d["public"]
    return "k1", path_or_pub

def default_key_path(actor: str) -> str:
    return os.path.join(os.path.expanduser("~"), ".han", "keys", actor.replace(":", "_").replace("@", "") + ".json")

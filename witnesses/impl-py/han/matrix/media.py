"""Encrypted attachments for context blobs (§8.2 session streams): AES-256-CTR as in the Matrix spec's EncryptedFile, so an
`mxc://` URI alone reveals nothing. The key travels in the `org.han.stream` event; the HAN `BlobRef.hash` (sha256 of the
plaintext) is checked by the reader after decryption (T-03)."""
import base64, hashlib, os
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

def _b64(b: bytes) -> str: return base64.urlsafe_b64encode(b).decode().rstrip("=")
def _unb64(s: str) -> bytes: return base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))

def encrypt(data: bytes) -> tuple[bytes, dict]:
    """→ (ciphertext, file info without `url`)."""
    key, iv = os.urandom(32), os.urandom(8) + bytes(8)            # 64-bit random counter prefix, low 64 bits zero (Matrix)
    c = Cipher(algorithms.AES(key), modes.CTR(iv)).encryptor()
    ct = c.update(data) + c.finalize()
    return ct, {"v": "v2", "key": {"kty": "oct", "key_ops": ["encrypt", "decrypt"], "alg": "A256CTR", "k": _b64(key), "ext": True},
                "iv": _b64(iv), "hashes": {"sha256": _b64(hashlib.sha256(ct).digest())}}

def decrypt(ct: bytes, info: dict) -> bytes:
    if _b64(hashlib.sha256(ct).digest()) != info["hashes"]["sha256"]: raise ValueError("encrypted attachment hash mismatch")
    d = Cipher(algorithms.AES(_unb64(info["key"]["k"])), modes.CTR(_unb64(info["iv"]))).decryptor()
    return d.update(ct) + d.finalize()

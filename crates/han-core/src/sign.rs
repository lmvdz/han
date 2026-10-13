//! Ed25519 with the protocol's domain separation. Verification is always `verify_strict`.

use ed25519_dalek::Signer as _;

use crate::base64url;
use crate::error::Error;
use crate::types::{Entry, Receipt, Sig};

/// Domain prefix of an entry signature (over the entry id's hex).
pub const ENTRY_SIG_DOMAIN: &str = "han-sig-v1\n";
/// Domain prefix of a receipt signature (over the receipt hash's hex).
pub const RECEIPT_SIG_DOMAIN: &str = "han-home-sig-v1\n";
/// Domain prefix of a hello proof.
pub const HELLO_DOMAIN: &str = "han-hello-v1\n";
/// Domain prefix of a `space.move` acceptance.
pub const ACCEPT_MOVE_DOMAIN: &str = "han-accept-move-v1\n";

/// An Ed25519 secret key. It never prints its bytes.
pub struct SigningKey(ed25519_dalek::SigningKey);

impl std::fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SigningKey(..)")
    }
}

impl SigningKey {
    /// The key whose 32-byte seed is `seed`.
    pub fn from_seed(seed: [u8; 32]) -> Self {
        SigningKey(ed25519_dalek::SigningKey::from_bytes(&seed))
    }

    /// The matching public key.
    pub fn public_key(&self) -> PublicKey {
        PublicKey(self.0.verifying_key())
    }

    /// Signs raw bytes; the result is base64url without padding.
    pub fn sign(&self, message: &[u8]) -> Sig {
        base64url::encode(&self.0.sign(message).to_bytes())
    }
}

/// An Ed25519 public key (a profile `publicKey`, a node id, a server key).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicKey(ed25519_dalek::VerifyingKey);

impl PublicKey {
    /// Parses a `publicKey`: canonical base64url of exactly 32 bytes that are the canonical
    /// encoding of a curve point of large order (a small-order key is refused here, at parse).
    pub fn from_base64url(s: &str) -> Result<Self, Error> {
        let bytes: [u8; 32] = base64url::decode(s)?
            .try_into()
            .map_err(|_| Error::invalid("publicKey length"))?;
        let key = ed25519_dalek::VerifyingKey::from_bytes(&bytes)
            .map_err(|_| Error::invalid("publicKey is not a curve point"))?;
        if key.is_weak() || key.as_bytes() != &bytes {
            return Err(Error::invalid(
                "publicKey is of small order or not canonical",
            ));
        }
        Ok(PublicKey(key))
    }

    /// The canonical base64url form.
    pub fn to_base64url(&self) -> String {
        base64url::encode(self.0.as_bytes())
    }

    /// Checks `signature` (canonical base64url, 64 bytes) over `message` with `verify_strict`.
    /// A non-canonical signature fails before any verification.
    pub fn verify(&self, message: &[u8], signature: &str) -> Result<(), Error> {
        let bytes: [u8; 64] = base64url::decode(signature)?
            .try_into()
            .map_err(|_| Error::BadSignature)?;
        self.0
            .verify_strict(message, &ed25519_dalek::Signature::from_bytes(&bytes))
            .map_err(|_| Error::BadSignature)
    }
}

fn domain_message(domain: &str, hex: &str) -> Vec<u8> {
    [domain.as_bytes(), hex.as_bytes()].concat()
}

impl Entry {
    /// Sets `sig` to the author's signature over this entry's id.
    pub fn sign(&mut self, key: &SigningKey) -> Result<(), Error> {
        let id = self.id()?;
        self.sig = key.sign(&domain_message(ENTRY_SIG_DOMAIN, &id));
        Ok(())
    }

    /// Checks `sig` against the author's public key for `keyId`. Key status (revoked or not) is
    /// Policy's question, not this one's.
    pub fn verify(&self, key: &PublicKey) -> Result<(), Error> {
        key.verify(&domain_message(ENTRY_SIG_DOMAIN, &self.id()?), &self.sig)
    }
}

impl Receipt {
    /// Sets `homeSig` to the home's signature over this receipt's hash.
    pub fn sign(&mut self, key: &SigningKey) -> Result<(), Error> {
        let hash = self.hash()?;
        self.home_sig = key.sign(&domain_message(RECEIPT_SIG_DOMAIN, &hash));
        Ok(())
    }

    /// Checks `homeSig` against the home's key. The caller checks that the key's validity window
    /// contains `time` (R-14).
    pub fn verify(&self, key: &PublicKey) -> Result<(), Error> {
        key.verify(
            &domain_message(RECEIPT_SIG_DOMAIN, &self.hash()?),
            &self.home_sig,
        )
    }
}

fn hello_message(challenge: &str, dialed: &str) -> Vec<u8> {
    format!("{HELLO_DOMAIN}{challenge}\n{dialed}").into_bytes()
}

/// The proof of a `hello`: a signature over `"han-hello-v1\n" || challenge || "\n" || dialed`,
/// the challenge as sent and `dialed` the ServerName the client intends (G-08).
pub fn hello_proof(key: &SigningKey, challenge: &str, dialed: &str) -> Sig {
    key.sign(&hello_message(challenge, dialed))
}

/// Checks a hello proof.
pub fn verify_hello_proof(
    key: &PublicKey,
    proof: &str,
    challenge: &str,
    dialed: &str,
) -> Result<(), Error> {
    key.verify(&hello_message(challenge, dialed), proof)
}

fn accept_message(space: &str, head: &Receipt) -> Result<Vec<u8>, Error> {
    Ok(format!("{ACCEPT_MOVE_DOMAIN}{space}\n{}", head.hash()?).into_bytes())
}

/// The target server's acceptance of a `space.move`: a signature over
/// `"han-accept-move-v1\n" || space || "\n" || receiptHash(head)`.
pub fn accept_move_sig(key: &SigningKey, space: &str, head: &Receipt) -> Result<Sig, Error> {
    Ok(key.sign(&accept_message(space, head)?))
}

/// Checks a `space.move` acceptance.
pub fn verify_accept_move(
    key: &PublicKey,
    sig: &str,
    space: &str,
    head: &Receipt,
) -> Result<(), Error> {
    key.verify(&accept_message(space, head)?, sig)
}

/// N-01: whether `s` is a node id: a public key HAN accepts, 43 characters of canonical
/// base64url, a curve point not of small order. Well-known `publicKey`s are judged the same way
/// with [`PublicKey::from_base64url`].
pub fn is_node_id(s: &str) -> bool {
    s.len() == 43 && PublicKey::from_base64url(s).is_ok()
}

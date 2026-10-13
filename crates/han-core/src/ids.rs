//! Ids and hashes (architecture.md section 2). Every hash is SHA-256 shown as 64 lowercase hex digits.

use sha2::{Digest, Sha256};

use crate::error::Error;
use crate::json::{self, Value};
use crate::types::{Entry, Receipt};

/// Domain prefix of an entry id.
pub const ENTRY_ID_DOMAIN: &str = "han-entry-v1\n";
/// Domain prefix of a receipt hash.
pub const RECEIPT_DOMAIN: &str = "han-receipt-v1\n";

/// SHA-256 of `bytes`, lowercase hex. Also the hash of a blob.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[usize::from(b >> 4)] as char);
        s.push(DIGITS[usize::from(b & 15)] as char);
    }
    s
}

/// `sha256(JCS(v))`: the hash of a content, and a plan's `planDigest`.
pub fn json_digest(v: &Value) -> Result<String, Error> {
    Ok(sha256_hex(&json::canonicalize(v)?))
}

/// An entry's `contentHash`: [`json_digest`] of the content, or `""` when there is none.
pub fn content_hash(content: Option<&Value>) -> Result<String, Error> {
    content.map_or_else(|| Ok(String::new()), json_digest)
}

/// The JCS of `value` with the named member removed, behind a domain prefix.
fn preimage(domain: &str, value: &Value, drop: &str) -> Result<Vec<u8>, Error> {
    let Value::Object(mut members) = value.clone() else {
        return Err(Error::invalid("preimage: not an object"));
    };
    members.remove(drop);
    let mut out = domain.as_bytes().to_vec();
    out.extend(json::canonicalize(&Value::Object(members))?);
    Ok(out)
}

impl Entry {
    /// The exact bytes the id hashes: `"han-entry-v1\n" || JCS(entry minus sig)`.
    pub fn id_preimage(&self) -> Result<Vec<u8>, Error> {
        preimage(ENTRY_ID_DOMAIN, &json::to_value(self)?, "sig")
    }

    /// The entry id.
    pub fn id(&self) -> Result<String, Error> {
        Ok(sha256_hex(&self.id_preimage()?))
    }
}

impl Receipt {
    /// The exact bytes the receipt hash covers: `"han-receipt-v1\n" || JCS(receipt minus homeSig)`.
    pub fn hash_preimage(&self) -> Result<Vec<u8>, Error> {
        preimage(RECEIPT_DOMAIN, &json::to_value(self)?, "homeSig")
    }

    /// The receipt hash, which the next receipt carries as `prev`.
    pub fn hash(&self) -> Result<String, Error> {
        Ok(sha256_hex(&self.hash_preimage()?))
    }
}

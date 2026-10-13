//! Building signed entries: what a client does before `append` (and what the sequencer does for the
//! profile spaces it creates).

use han_core::ids;
use han_core::json::{self, Value};
use han_core::{Entry, SigningKey};

use crate::error::{Result, WireError};
use crate::time::format_time;

/// The members of an entry a caller chooses.
#[derive(Clone, Debug)]
pub struct EntrySpec {
    /// The space.
    pub space: String,
    /// The kind.
    pub kind: String,
    /// The signing actor.
    pub author: String,
    /// The signing key's id.
    pub key_id: String,
    /// The body.
    pub body: Value,
    /// The content, if the kind has one.
    pub content: Option<Value>,
    /// Entries of the space this one refers to.
    pub refs: Vec<String>,
    /// The head the author has seen.
    pub seen: u64,
    /// The author's clock, in seconds since the epoch (informational).
    pub time_secs: i64,
}

/// 26 random base32 characters: 130 bits, of which at least 128 are random (section 2).
pub fn random_nonce() -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut bytes = [0u8; 26];
    // The system random source failing is not recoverable.
    getrandom::fill(&mut bytes).expect("system random source");
    bytes
        .iter()
        .map(|b| ALPHABET[usize::from(b & 31)] as char)
        .collect()
}

/// Builds and signs an entry. Returns it as the `Value` an `append` carries, with its content.
pub fn sign_entry(spec: EntrySpec, key: &SigningKey) -> Result<(Value, Option<Value>)> {
    let content_hash =
        han_core::ids::content_hash(spec.content.as_ref()).map_err(WireError::from)?;
    let mut entry = Entry {
        v: 1,
        space: spec.space,
        kind: spec.kind,
        author: spec.author,
        key_id: spec.key_id,
        body: spec.body,
        content_hash,
        refs: spec.refs,
        seen: spec.seen,
        nonce: random_nonce(),
        time: format_time(spec.time_secs),
        sig: String::new(),
    };
    entry.sign(key).map_err(WireError::from)?;
    let value = json::to_value(&entry).map_err(|e| WireError::invalid(e.to_string()))?;
    Ok((value, spec.content))
}

/// The id of an entry given as Json, whatever its shape: the hash of the domain prefix and the JCS of the entry minus `sig`.
pub fn entry_id(entry: &Value) -> Result<String> {
    let Value::Object(members) = entry else {
        return Err(WireError::invalid("entry is not an object"));
    };
    let mut members = members.clone();
    members.remove("sig");
    let mut preimage = ids::ENTRY_ID_DOMAIN.as_bytes().to_vec();
    preimage.extend(
        json::canonicalize(&Value::Object(members)).map_err(|e| WireError::invalid(e.to_string()))?,
    );
    Ok(ids::sha256_hex(&preimage))
}

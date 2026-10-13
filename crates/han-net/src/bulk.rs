//! Bulk batches (X15): the CBOR map `{1: author, 2: keyId, 3: profile, 4: data, 5: sig}` that a profile's
//! operations and sync batches travel in on unidirectional streams. `data` is the batch's exact bytes, and
//! `sig` the author's strict Ed25519 signature over `"han/bulk/1\n" ‖ profile ‖ "\n" ‖ data`.

use std::collections::BTreeMap;

use han_core::PublicKey;
use han_core::policy::KeyStatus;
use han_seq::PinKey;
use han_seq::stream_item::is_profile_name;
use minicbor::{Decode, Encode};

/// The signed message of a batch.
pub fn bulk_message(profile: &str, data: &[u8]) -> Vec<u8> {
    let mut m = b"han/bulk/1\n".to_vec();
    m.extend_from_slice(profile.as_bytes());
    m.push(b'\n');
    m.extend_from_slice(data);
    m
}

/// One batch of bulk data.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct BulkBatch {
    /// The author.
    #[n(1)]
    pub author: String,
    /// The author's key.
    #[n(2)]
    pub key_id: String,
    /// The profile's name, such as `han.timeline/1`.
    #[n(3)]
    pub profile: String,
    /// The batch's exact bytes.
    #[n(4)]
    #[cbor(with = "minicbor::bytes")]
    pub data: Vec<u8>,
    /// The signature, base64url.
    #[n(5)]
    pub sig: String,
}

impl BulkBatch {
    /// Encodes the batch.
    pub fn to_cbor(&self) -> Vec<u8> {
        minicbor::to_vec(self).unwrap_or_default()
    }

    /// Decodes a batch.
    pub fn from_cbor(bytes: &[u8]) -> Option<BulkBatch> {
        minicbor::decode(bytes).ok()
    }

    /// A batch is genuine when its profile name is well formed, `key_id` is a `valid` key of `keys` (the
    /// author's pinned profile) and `sig` verifies strictly. A receiver drops a batch that does not verify.
    pub fn verify(&self, keys: &BTreeMap<String, PinKey>) -> bool {
        if !is_profile_name(&self.profile) {
            return false;
        }
        let Some(key) = keys
            .get(&self.key_id)
            .filter(|k| k.status == KeyStatus::Valid)
        else {
            return false;
        };
        PublicKey::from_base64url(&key.public_key)
            .and_then(|pk| pk.verify(&bulk_message(&self.profile, &self.data), &self.sig))
            .is_ok()
    }
}

//! Signed envelopes (TL-93, TL-94): zstd payloads, batch ids and Ed25519 signatures.

use ed25519_dalek::Signer as _;
use sha2::{Digest, Sha256};

use crate::cbor::Cbor;
use crate::codec::{self, DecodeError};
use crate::ids::{BatchId, hex};
use crate::op::Batch;

/// The largest encoded envelope (§17).
pub const MAX_ENVELOPE: usize = 4 * 1024 * 1024 + 4 * 1024;
/// The largest decompressed batch (§17).
pub const MAX_BATCH: usize = 4 * 1024 * 1024;

const BATCH_DOMAIN: &[u8] = b"han-timeline-batch-v1\n";
const SIG_DOMAIN: &[u8] = b"han-timeline-sig-v1\n";
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// Signs batches with one device key of one actor. Keys never leave the device: the history
/// only asks for signatures.
pub trait Signer {
    /// The ActorId whose device key this is.
    fn actor(&self) -> &str;
    /// The KeyId of the device key.
    fn key_id(&self) -> &str;
    /// The Ed25519 signature of `message`.
    fn sign(&self, message: &[u8]) -> [u8; 64];
}

/// A [`Signer`] holding an Ed25519 key in memory.
pub struct KeySigner {
    actor: String,
    key_id: String,
    key: ed25519_dalek::SigningKey,
}

impl std::fmt::Debug for KeySigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "KeySigner({}/{})", self.actor, self.key_id)
    }
}

impl KeySigner {
    /// A signer for `actor`'s device key `key_id` with the 32-byte Ed25519 seed `seed`.
    pub fn from_seed(actor: impl Into<String>, key_id: impl Into<String>, seed: [u8; 32]) -> Self {
        KeySigner {
            actor: actor.into(),
            key_id: key_id.into(),
            key: ed25519_dalek::SigningKey::from_bytes(&seed),
        }
    }

    /// The public key, as profiles list it (32 bytes).
    pub fn public_key(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }
}

impl Signer for KeySigner {
    fn actor(&self) -> &str {
        &self.actor
    }
    fn key_id(&self) -> &str {
        &self.key_id
    }
    fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.key.sign(message).to_bytes()
    }
}

/// How a payload is compressed. Both are valid zstd frames that declare their content size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Compression {
    /// zstd compression (level 3).
    #[default]
    Zstd,
    /// Raw (stored) blocks: byte-for-byte predictable, as the envelope vectors use.
    Raw,
}

/// A decoded envelope (TL-93).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Envelope {
    /// The SpaceId of the history's space.
    pub space: String,
    /// The history id, or `None` (an empty byte string) for a genesis envelope.
    pub history: Option<BatchId>,
    /// The author's ActorId.
    pub actor: String,
    /// The KeyId of the signing device key.
    pub key: String,
    /// One zstd frame holding the batch's deterministic CBOR.
    pub payload: Vec<u8>,
    /// The Ed25519 signature.
    pub sig: [u8; 64],
}

impl Envelope {
    /// Decodes and shape-checks an envelope (TL-101 step 1).
    pub fn decode(bytes: &[u8]) -> Result<Envelope, DecodeError> {
        if bytes.len() > MAX_ENVELOPE {
            return Err(DecodeError::TooLarge);
        }
        let item = Cbor::decode(bytes)?;
        let Cbor::Map(entries) = &item else {
            return Err(DecodeError::Malformed);
        };
        if entries.len() != 7 {
            return Err(DecodeError::Malformed);
        }
        let get = |k| item.get(k).ok_or(DecodeError::Malformed);
        let text = |k| match get(k)? {
            Cbor::Text(s) => Ok(s.clone()),
            _ => Err(DecodeError::Malformed),
        };
        let bytes_of = |k| match get(k)? {
            Cbor::Bytes(b) => Ok(b.clone()),
            _ => Err(DecodeError::Malformed),
        };
        if get(1)? != &Cbor::Uint(1) {
            return Err(DecodeError::Malformed);
        }
        let space = text(2)?;
        let history = bytes_of(3)?;
        let actor = text(4)?;
        let key = text(5)?;
        let payload = bytes_of(6)?;
        let sig: [u8; 64] = bytes_of(7)?
            .try_into()
            .map_err(|_| DecodeError::Malformed)?;
        let history = match history.len() {
            0 => None,
            32 => Some(BatchId(history.try_into().unwrap_or([0; 32]))),
            _ => return Err(DecodeError::Malformed),
        };
        if !han_core::grammar::is_space_id(&space)
            || !han_core::grammar::is_actor_id(&actor)
            || !han_core::grammar::is_key_id(&key)
        {
            return Err(DecodeError::Malformed);
        }
        Ok(Envelope {
            space,
            history,
            actor,
            key,
            payload,
            sig,
        })
    }

    /// Hash the value spans of an already shape-checked deterministic envelope. The id
    /// preimage is a six-item array, rather than the seven-entry envelope map.
    pub(crate) fn received_id(&self, bytes: &[u8]) -> BatchId {
        let lengths = [
            1,
            head_len(self.space.len()) + self.space.len(),
            head_len(self.history.map_or(0, |_| 32)) + self.history.map_or(0, |_| 32),
            head_len(self.actor.len()) + self.actor.len(),
            head_len(self.key.len()) + self.key.len(),
            head_len(self.payload.len()) + self.payload.len(),
        ];
        let mut hash = Sha256::new();
        hash.update(BATCH_DOMAIN);
        hash.update([0x86]);
        let mut pos = 1;
        for len in lengths {
            pos += 1; // Unsigned map keys 1 through 6.
            hash.update(&bytes[pos..pos + len]);
            pos += len;
        }
        BatchId(hash.finalize().into())
    }

    /// The deterministic encoding.
    pub fn encode(&self) -> Vec<u8> {
        Cbor::Map(vec![
            (Cbor::u(1), Cbor::u(1)),
            (Cbor::u(2), Cbor::text(self.space.clone())),
            (Cbor::u(3), Cbor::Bytes(self.history_bytes())),
            (Cbor::u(4), Cbor::text(self.actor.clone())),
            (Cbor::u(5), Cbor::text(self.key.clone())),
            (Cbor::u(6), Cbor::Bytes(self.payload.clone())),
            (Cbor::u(7), Cbor::Bytes(self.sig.to_vec())),
        ])
        .encode()
    }

    fn history_bytes(&self) -> Vec<u8> {
        self.history.map_or_else(Vec::new, |h| h.0.to_vec())
    }

    /// `"han-timeline-batch-v1\n" ‖ dCBOR([1, space, history, actor, key, payload])`.
    pub fn batch_id_preimage(&self) -> Vec<u8> {
        let body = Cbor::Array(vec![
            Cbor::u(1),
            Cbor::text(self.space.clone()),
            Cbor::Bytes(self.history_bytes()),
            Cbor::text(self.actor.clone()),
            Cbor::text(self.key.clone()),
            Cbor::Bytes(self.payload.clone()),
        ]);
        [BATCH_DOMAIN, &body.encode()].concat()
    }

    /// The batch id (TL-93); for a genesis envelope, the history id.
    pub fn batch_id(&self) -> BatchId {
        let mut hash = Sha256::new();
        hash.update(BATCH_DOMAIN);
        hash.update([0x86, 1]);
        hash_field(&mut hash, 3, self.space.as_bytes());
        hash_field(&mut hash, 2, self.history.as_ref().map_or(&[], |h| &h.0));
        hash_field(&mut hash, 3, self.actor.as_bytes());
        hash_field(&mut hash, 3, self.key.as_bytes());
        hash_field(&mut hash, 2, &self.payload);
        BatchId(hash.finalize().into())
    }

    /// Checks the signature with `public_key` using strict verification.
    pub fn verify(&self, public_key: &[u8; 32]) -> bool {
        self.verify_id(public_key, &self.batch_id())
    }

    pub(crate) fn verify_id(&self, public_key: &[u8; 32], id: &BatchId) -> bool {
        let Ok(key) = ed25519_dalek::VerifyingKey::from_bytes(public_key) else {
            return false;
        };
        key.verify_strict(
            &sig_message(id),
            &ed25519_dalek::Signature::from_bytes(&self.sig),
        )
        .is_ok()
    }

    /// Decompresses and decodes the batch (TL-101 step 5).
    pub fn batch(&self) -> Result<(Batch, Vec<u8>), DecodeError> {
        let raw = decompress(&self.payload)?;
        let batch = codec::decode_batch(&raw, &self.actor, &self.key)?;
        Ok((batch, raw))
    }
}

fn head_len(n: usize) -> usize {
    match n {
        0..=23 => 1,
        24..=255 => 2,
        256..=65535 => 3,
        65536..=4294967295 => 5,
        _ => 9,
    }
}

/// Hash a CBOR string header and its borrowed contents without allocating a preimage.
fn hash_field(hash: &mut Sha256, major: u8, bytes: &[u8]) {
    let len = head_len(bytes.len());
    if len == 1 {
        hash.update([(major << 5) | bytes.len() as u8]);
    } else {
        let additional = match len {
            2 => 24,
            3 => 25,
            5 => 26,
            _ => 27,
        };
        hash.update([(major << 5) | additional]);
        let n = (bytes.len() as u64).to_be_bytes();
        hash.update(&n[9 - len..]);
    }
    hash.update(bytes);
}

/// `"han-timeline-sig-v1\n" ‖ hex(batchId)`.
pub fn sig_message(id: &BatchId) -> Vec<u8> {
    [SIG_DOMAIN, hex(&id.0).as_bytes()].concat()
}

/// Encodes, compresses and signs `batch` into envelope bytes. `history` is `None` for the
/// genesis batch. The signer's actor and key must be the batch's replica's.
pub fn seal(
    space: &str,
    history: Option<BatchId>,
    batch: &Batch,
    signer: &dyn Signer,
    compression: Compression,
) -> Vec<u8> {
    seal_raw(space, history, batch, signer, compression).0
}

/// Keep the deterministic batch bytes for local validation instead of decompressing them.
pub(crate) fn seal_raw(
    space: &str,
    history: Option<BatchId>,
    batch: &Batch,
    signer: &dyn Signer,
    compression: Compression,
) -> (Vec<u8>, Vec<u8>) {
    let raw = codec::encode_batch(batch);
    let payload = compress(&raw, compression);
    let mut env = Envelope {
        space: space.to_string(),
        history,
        actor: signer.actor().to_string(),
        key: signer.key_id().to_string(),
        payload,
        sig: [0; 64],
    };
    env.sig = signer.sign(&sig_message(&env.batch_id()));
    (env.encode(), raw)
}

/// One zstd frame holding `raw`, with its content size declared and no dictionary.
pub fn compress(raw: &[u8], compression: Compression) -> Vec<u8> {
    if compression == Compression::Zstd
        && raw.len() >= 128
        && let Ok(frame) = zstd::bulk::compress(raw, 3)
        && frame.len() < raw.len()
    {
        return frame;
    }
    raw_frame(raw)
}

/// A single-segment zstd frame of raw blocks.
fn raw_frame(raw: &[u8]) -> Vec<u8> {
    let mut out = ZSTD_MAGIC.to_vec();
    let n = raw.len() as u64;
    // Single segment; the content size field takes 1, 2, 4 or 8 bytes.
    if n < 256 {
        out.push(0x20);
        out.push(n as u8);
    } else if n < 65_536 + 256 {
        out.push(0x60);
        out.extend_from_slice(&((n - 256) as u16).to_le_bytes());
    } else if n <= u64::from(u32::MAX) {
        out.push(0xa0);
        out.extend_from_slice(&(n as u32).to_le_bytes());
    } else {
        out.push(0xe0);
        out.extend_from_slice(&n.to_le_bytes());
    }
    const BLOCK: usize = 128 * 1024;
    let mut chunks: Vec<&[u8]> = raw.chunks(BLOCK).collect();
    if chunks.is_empty() {
        chunks.push(&[]);
    }
    let last = chunks.len() - 1;
    for (i, chunk) in chunks.into_iter().enumerate() {
        let header = ((chunk.len() as u32) << 3) | u32::from(i == last);
        out.extend_from_slice(&header.to_le_bytes()[..3]);
        out.extend_from_slice(chunk);
    }
    out
}

/// Decompresses one zstd frame that declares its content size, uses no dictionary, spans the
/// whole payload and decompresses to exactly that size (TL-93).
pub fn decompress(payload: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let malformed = DecodeError::Malformed;
    if payload.len() < 5 || payload[..4] != ZSTD_MAGIC {
        return Err(malformed);
    }
    let fhd = payload[4];
    let fcs_flag = fhd >> 6;
    let single_segment = fhd & 0x20 != 0;
    let reserved = fhd & 0x08 != 0;
    let dict_flag = fhd & 0x03;
    if reserved || dict_flag != 0 || (fcs_flag == 0 && !single_segment) {
        return Err(malformed);
    }
    let mut pos = 5 + usize::from(!single_segment);
    let fcs_len = match fcs_flag {
        0 => 1,
        1 => 2,
        2 => 4,
        _ => 8,
    };
    let field = payload.get(pos..pos + fcs_len).ok_or(malformed)?;
    pos += fcs_len;

    let mut le = [0u8; 8];
    le[..fcs_len].copy_from_slice(field);
    let mut size = u64::from_le_bytes(le);
    if fcs_flag == 1 {
        size += 256;
    }
    if size > MAX_BATCH as u64 {
        return Err(DecodeError::TooLarge);
    }
    match zstd::zstd_safe::find_frame_compressed_size(payload) {
        Ok(n) if n == payload.len() => {}
        _ => return Err(malformed),
    }
    // The stored single-block frame emitted for tiny batches needs no zstd decoder.
    // Exact header/length checks leave every other frame on the full validation path.
    if fhd == 0x20 && size < 128 && payload.len() == pos + 3 + size as usize {
        let header = ((size as u32) << 3) | 1;
        if payload[pos..pos + 3] == header.to_le_bytes()[..3] {
            return Ok(payload[pos + 3..].to_vec());
        }
    }
    let out = zstd::bulk::decompress(payload, size as usize).map_err(|_| malformed)?;
    if out.len() as u64 != size {
        return Err(malformed);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_and_compressed_frames_round_trip() {
        for n in [1usize, 255, 256, 1000, 70_000, 300_000] {
            let raw: Vec<u8> = (0..n).map(|i| (i % 7) as u8).collect();
            for c in [Compression::Raw, Compression::Zstd] {
                assert_eq!(decompress(&compress(&raw, c)).unwrap(), raw);
            }
        }
    }

    #[test]
    fn rejects_trailing_frames() {
        let mut p = compress(b"abc", Compression::Raw);
        p.extend(compress(b"abc", Compression::Raw));
        assert!(decompress(&p).is_err());
    }
}

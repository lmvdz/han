//! The shapes of architecture.md section 2, mirroring `han.ts`. Field names on the wire are camelCase.
//!
//! These types are closed (`deny_unknown_fields`) and derive serde. An entry's body is typed per
//! kind ([`Body`]) and checked as it is decoded; the rest of deserializing is not validation: use
//! [`Entry::from_json`], [`Entry::validate`] and [`LogRecord::validate`], which also check grammar
//! and limits.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

use crate::error::Error;
use crate::json::{self, Value};
use crate::policy::Body;

/// `@name:server`; `@:server` is the server actor.
pub type ActorId = String;
/// `<1-26 chars [a-z0-9]>:<first home server>` or `profile:<ActorId>`.
pub type SpaceId = String;
/// 64 lowercase hex digits: `sha256("han-entry-v1\n" || JCS(entry minus sig))`.
pub type EntryId = String;
/// 64 lowercase hex digits.
pub type Hex = String;
/// 1 to 64 characters of `[A-Za-z0-9_-]`.
pub type KeyId = String;
/// Base64url, no padding.
pub type Sig = String;
/// `YYYY-MM-DDTHH:MM:SSZ`.
pub type Time = String;
/// A position in a space's log: 1, 2, ...; 0 is before the first record.
pub type Seq = u64;

/// A signed log entry: the permanent header.
///
/// Deserializing decodes the body by `kind` with [`Body::parse`], so an entry whose body breaks its
/// kind's schema does not deserialize.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// Protocol version; always 1.
    pub v: u64,
    /// The space the entry belongs to.
    pub space: SpaceId,
    /// The kind, such as `message` or `effect.propose`.
    pub kind: String,
    /// The signing actor.
    pub author: ActorId,
    /// Which of the author's keys signed.
    pub key_id: KeyId,
    /// The kind's permanent body, typed by kind; [`Entry::validate`] checks it matches `kind`.
    pub body: Body,
    /// `sha256(JCS(content))`, or `""` when no content is sent.
    pub content_hash: String,
    /// Entries of the same space this one refers to.
    pub refs: Vec<EntryId>,
    /// The head the author had seen.
    pub seen: Seq,
    /// 26 characters of `[a-z2-7]`.
    pub nonce: String,
    /// The author's clock; informational.
    pub time: Time,
    /// `Ed25519(key, "han-sig-v1\n" || id)`.
    pub sig: Sig,
}

/// An entry as it arrives, its body not yet typed.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RawEntry {
    v: u64,
    space: SpaceId,
    kind: String,
    author: ActorId,
    key_id: KeyId,
    body: Value,
    content_hash: String,
    refs: Vec<EntryId>,
    seen: Seq,
    nonce: String,
    time: Time,
    sig: Sig,
}

impl RawEntry {
    /// Types the body by `kind` ([`Body::parse`]).
    pub(crate) fn typed(self) -> Result<Entry, Error> {
        Ok(Entry {
            body: Body::parse(&self.kind, &self.body)?,
            v: self.v,
            space: self.space,
            kind: self.kind,
            author: self.author,
            key_id: self.key_id,
            content_hash: self.content_hash,
            refs: self.refs,
            seen: self.seen,
            nonce: self.nonce,
            time: self.time,
            sig: self.sig,
        })
    }
}

impl<'de> Deserialize<'de> for Entry {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        RawEntry::deserialize(d)?
            .typed()
            .map_err(serde::de::Error::custom)
    }
}

impl Entry {
    /// Reads an entry from a [`Value`]: the exact `Entry` fields, and the body typed by kind
    /// ([`Body::parse`]). It does not [`Entry::validate`] the header.
    pub fn from_value(v: &Value) -> Result<Entry, Error> {
        json::from_value::<RawEntry>(v)?.typed()
    }
}

/// Whether the sequencer's durable storage survives a process crash or a machine loss.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Durability {
    /// Survives a process crash.
    Local,
    /// Survives the loss of any one machine.
    Replicated,
}

/// The sequencer's signed statement that it ordered an entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Receipt {
    /// The space.
    pub space: SpaceId,
    /// Position in the chain, from 1.
    pub seq: Seq,
    /// The entry's id.
    pub id: EntryId,
    /// `receiptHash` of seq - 1; `""` for seq 1.
    pub prev: String,
    /// The home's server name.
    pub home: String,
    /// The home's key id in its `/.well-known/han`.
    pub home_key: KeyId,
    /// The home's clock, rounded down to the second.
    pub time: Time,
    /// The durability achieved.
    pub durability: Durability,
    /// The pinned profile seq of each relevant actor.
    pub profiles: BTreeMap<ActorId, Seq>,
    /// `Ed25519(homeKey, "han-home-sig-v1\n" || receiptHash)`.
    pub home_sig: Sig,
}

/// The proof that a record's content was redacted: the signed `redact` entry and its receipt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactedBy {
    /// The `redact` entry.
    pub entry: Entry,
    /// Its receipt.
    pub receipt: Receipt,
}

/// An entry with its content and receipt, as stored and served.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LogRecord {
    /// True when the content was erased by a `redact`.
    pub redacted: bool,
    /// Required when `redacted`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redacted_by: Option<RedactedBy>,
    /// The entry.
    pub entry: Entry,
    /// The content; `None` (JSON `null`) when the kind has none, or after redaction.
    pub content: Option<Value>,
    /// The receipt.
    pub receipt: Receipt,
}

/// A named blob: bytes by hash. Verify the hash before using the bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlobRef {
    /// `sha256` of the bytes, lowercase hex.
    pub hash: Hex,
    /// Size in bytes, at most 4 GiB.
    pub size: u64,
    /// A media type.
    #[serde(rename = "type")]
    pub media_type: String,
    /// A transport locator, at most 1,024 bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

/// A cross-space pointer; content only, no authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    /// The other space.
    pub space: SpaceId,
    /// A position in it.
    pub seq: Seq,
    /// The entry id there.
    pub id: EntryId,
}

/// An amount of a unit, in integer minor units.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    /// `[A-Za-z0-9_-]{1,32}`.
    pub unit: String,
    /// An integer in `[0, 2^53)`.
    pub amount: u64,
}

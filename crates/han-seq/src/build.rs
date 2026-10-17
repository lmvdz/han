//! Building signed entries: what a client does before `append` (and what the sequencer does for the
//! profile spaces it creates).

use han_core::SigningKey;
use han_core::ids;
use han_core::json::{self, Value};
use han_core::sign::ENTRY_SIG_DOMAIN;

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

/// Builds and signs an entry. Returns it as the `Value` an `append` carries, with its content. The
/// body is signed as given, whatever its shape: checking it is the sequencer's job.
pub fn sign_entry(spec: EntrySpec, key: &SigningKey) -> Result<(Value, Option<Value>)> {
    use crate::jv::{int, obj, s};
    let content_hash =
        han_core::ids::content_hash(spec.content.as_ref()).map_err(WireError::from)?;
    let mut entry = obj([
        ("v", int(1)),
        ("space", s(&spec.space)),
        ("kind", s(&spec.kind)),
        ("author", s(&spec.author)),
        ("keyId", s(&spec.key_id)),
        ("body", spec.body),
        ("contentHash", s(&content_hash)),
        (
            "refs",
            Value::Array(spec.refs.iter().map(|r| s(r)).collect()),
        ),
        ("seen", int(spec.seen)),
        ("nonce", s(&random_nonce())),
        ("time", s(&format_time(spec.time_secs))),
    ]);
    let id = entry_id(&entry)?;
    let sig = key.sign(format!("{ENTRY_SIG_DOMAIN}{id}").as_bytes());
    if let Value::Object(members) = &mut entry {
        members.insert("sig".into(), s(&sig));
    }
    Ok((entry, spec.content))
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
        json::canonicalize(&Value::Object(members))
            .map_err(|e| WireError::invalid(e.to_string()))?,
    );
    Ok(ids::sha256_hex(&preimage))
}

/// A `PolicyDoc` for a new space: `members` or `public` to read, joining by `invite` or `open` or `closed`.
/// Agents need a confirmed operator (`allowAgents: "with-operator"`): an agent whose operator never confirmed
/// it gets no role and can't propose or claim (P-03). The rest are the defaults the vectors use: no approval
/// rules, sessions visible to members, steerable by owners.
pub fn space_policy(visibility: &str, join: &str) -> Value {
    use crate::jv::{int, obj, s};
    obj([
        ("visibility", s(visibility)),
        ("join", s(join)),
        ("membersMayInvite", Value::Bool(false)),
        ("allowAgents", s("with-operator")),
        ("approvals", Value::Array(vec![])),
        ("separateApprover", Value::Bool(false)),
        ("budget", Value::Null),
        ("maxDelegationDepth", int(2)),
        ("minDurability", s("local")),
        ("launchWindow", int(30)),
        ("extensionsMinRole", s("member")),
        (
            "sessions",
            obj([
                ("watch", s("member")),
                ("read", s("member")),
                ("steer", s("owner")),
                ("modes", Value::Array(vec![s("auto"), s("guarded")])),
            ]),
        ),
        ("servers", obj([("deny", Value::Array(vec![]))])),
    ])
}

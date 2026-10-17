//! Shape and limit validation: the Log's step 3 (architecture.md section 4.1), R-09 and R-25. No
//! Policy decision is made here.
//!
//! Every failure is [`Error`] (the protocol's `invalid`). The schema of a body or a content is its
//! kind's type ([`Body`], [`Content`]); an unknown field, a wrong type, an impossible date or an
//! out-of-range number fails.

use crate::content::{self, Content};
use crate::error::Error;
use crate::grammar::{self as g, is_opaque_json};
use crate::ids;
use crate::json::{self, MAX_SAFE_INT, Value};
use crate::limits::{MAX_BLOB_BYTES, MAX_CONTENT_BYTES, MAX_HEADER_BYTES, MAX_LIST};
use crate::policy::gate::{
    Change, Evidence, FileKind, MAX_CHANGE_BYTES, MAX_CHANGE_FILES, is_evidence_kind, is_repo_name,
    is_repo_path,
};
use crate::policy::{Body, ProfileDoc};
use crate::sign::PublicKey;
use crate::types::{BlobRef, Budget, Entry, Link, LogRecord, RawEntry, Receipt};

impl Budget {
    /// A unit in its grammar and an amount in `[0, 2^53)`.
    pub(crate) fn is_valid(&self) -> bool {
        g::is_unit(&self.unit) && self.amount <= MAX_SAFE_INT.unsigned_abs()
    }
}

impl Change {
    /// P-17: a repo name, a hex delta digest, files in their grammar and strictly ascending by
    /// UTF-8 bytes, `truncated` exactly when `total` leaves files out, and the JCS within
    /// [`MAX_CHANGE_BYTES`].
    pub(crate) fn is_valid(&self) -> bool {
        let listed = self.files.len() as u64;
        is_repo_name(&self.repo)
            && g::is_hex(&self.delta_digest)
            && self.total <= MAX_SAFE_INT.unsigned_abs()
            && self.files.len() <= MAX_CHANGE_FILES
            && self.truncated.is_none_or(|t| t)
            && self.revert_of.as_deref().is_none_or(g::is_hex)
            && self.files.iter().all(|f| {
                is_repo_path(&f.path)
                    && f.lines <= MAX_SAFE_INT.unsigned_abs()
                    && match (f.kind, f.from_path.as_deref()) {
                        (FileKind::Rename, Some(from)) => is_repo_path(from) && from != f.path,
                        (FileKind::Rename, None) | (_, Some(_)) => false,
                        (_, None) => true,
                    }
            })
            // strictly ascending by UTF-8 bytes: one entry per path
            && self
                .files
                .windows(2)
                .all(|w| w[0].path.as_bytes() < w[1].path.as_bytes())
            // truncated exactly when files are left out
            && self.total >= listed
            && (self.total > listed) == self.truncated.is_some()
            && json::to_value(self)
                .and_then(|v| json::canonicalize(&v))
                .is_ok_and(|b| b.len() <= MAX_CHANGE_BYTES)
    }
}

/// P-17: whether `v` is the summary of a change (`effect.propose.change`, and the plan's
/// `args.change`): it decodes strictly as a [`Change`] and is valid.
pub fn change_summary(v: &Value) -> bool {
    json::from_value_strict::<Change>(v).is_ok_and(|c| c.is_valid())
}

impl Evidence {
    /// An evidence kind and a hex delta digest.
    pub(crate) fn is_valid(&self) -> bool {
        is_evidence_kind(&self.kind) && g::is_hex(&self.over)
    }
}

impl BlobRef {
    /// A hex hash, a size of at most 4 GiB, a media type and a `uri` of at most 1,024 bytes.
    pub(crate) fn is_valid(&self) -> bool {
        g::is_hex(&self.hash)
            && self.size <= MAX_BLOB_BYTES
            && g::is_media_type(&self.media_type)
            && self.uri.as_ref().is_none_or(|u| u.len() <= 1024)
    }
}

impl Link {
    /// A space id, a seq in `[0, 2^53)` and a hex entry id.
    pub(crate) fn is_valid(&self) -> bool {
        g::is_space_id(&self.space)
            && g::is_hex(&self.id)
            && self.seq <= MAX_SAFE_INT.unsigned_abs()
    }
}

/// R-25, §2: the Log owns the canonical form of a public key: canonical base64url of 32 bytes
/// that are a curve point of large order, judged here at parse.
fn canonical_key_bytes(key: &str) -> Result<(), Error> {
    PublicKey::from_base64url(key).map(|_| ())
}

impl Entry {
    /// Parses one entry from JSON bytes: strict I-JSON, the exact `Entry` fields, the body typed
    /// by kind ([`Body::parse`]), then [`Entry::validate`].
    pub fn from_json(bytes: &[u8]) -> Result<Entry, Error> {
        let raw: RawEntry = json::from_value(&json::parse(bytes)?)?;
        let entry = raw.typed()?;
        entry.validate()?;
        Ok(entry)
    }

    /// Checks the header: every field in its grammar, the body of the entry's kind and in its
    /// schema ([`Body::check`]), the canonical form of any public key it carries, and the header
    /// size limit (measured with `sig` included). It does not check the signature, `seen <= head`,
    /// or `refs` existence.
    pub fn validate(&self) -> Result<(), Error> {
        if self.v != 1 {
            return Err(Error::invalid("v: unsupported version"));
        }
        if !g::is_nonce(&self.nonce) {
            return Err(Error::invalid("nonce"));
        }
        if !g::is_time(&self.time) {
            return Err(Error::invalid("time"));
        }
        if !(self.content_hash.is_empty() || g::is_hex(&self.content_hash)) {
            return Err(Error::invalid("contentHash"));
        }
        if !g::is_actor_id(&self.author) {
            return Err(Error::invalid("author"));
        }
        if !g::is_key_id(&self.key_id) {
            return Err(Error::invalid("keyId"));
        }
        if !g::is_space_id(&self.space) {
            return Err(Error::invalid("space"));
        }
        if self.refs.len() > MAX_LIST || !self.refs.iter().all(|r| g::is_hex(r)) {
            return Err(Error::invalid("refs"));
        }
        if self.seen > MAX_SAFE_INT as u64 {
            return Err(Error::invalid("seen"));
        }
        let kind_matches = match self.body.kind() {
            Some(kind) => kind == self.kind,
            None => g::is_extension_kind(&self.kind),
        };
        if !kind_matches {
            return Err(Error::invalid("kind"));
        }
        self.body.check()?;
        match &self.body {
            Body::KeyAdd(b) => canonical_key_bytes(&b.public_key)?,
            Body::SpaceCreate(b) => {
                if let Some(ProfileDoc { key, .. }) = &b.profile {
                    canonical_key_bytes(&key.public_key)?;
                }
            }
            _ => {}
        }
        if json::canonicalize(&json::to_value(self)?)?.len() > MAX_HEADER_BYTES {
            return Err(Error::invalid("header over the size limit"));
        }
        Ok(())
    }

    /// Checks `content` against this entry: it is present exactly when `contentHash` is not `""`
    /// (JSON `null` counts as absent), hashes to it, fits the size limit and the kind's content
    /// schema ([`Content::parse`]), and for `effect.propose` carries a plan with
    /// `plan.action = body.action` and `sha256(JCS(plan)) = body.planDigest`. Call
    /// [`Entry::validate`] first.
    pub fn validate_content(&self, content: Option<&Value>) -> Result<(), Error> {
        let content = content.filter(|c| **c != Value::Null);
        if content.is_none() != self.content_hash.is_empty() {
            return Err(Error::invalid("content presence"));
        }
        let Some(content) = content else {
            // Absent content is fine only if the kind needs none.
            return match Content::required(&self.kind) {
                Some(true) => Err(Error::invalid("content missing")),
                _ => Ok(()),
            };
        };
        let bytes = json::canonicalize(content)?;
        if bytes.len() > MAX_CONTENT_BYTES {
            return Err(Error::invalid("content over the size limit"));
        }
        if ids::sha256_hex(&bytes) != self.content_hash {
            return Err(Error::invalid("contentHash does not match the content"));
        }
        if let Body::Extension(_) = self.body {
            // Opaque to the core, but `blobs` is the generic blob position.
            if !is_opaque_json(content) || content.as_object().is_none() {
                return Err(Error::invalid(
                    "extension content must be an object of plain Json",
                ));
            }
            if content.get("blobs").is_some_and(|b| {
                !json::from_value_strict::<Vec<BlobRef>>(b).is_ok_and(|b| content::blobs(&b))
            }) {
                return Err(Error::invalid("content: bad field blobs"));
            }
            return Ok(());
        }
        let typed = Content::parse(&self.kind, content)?;
        if let (Body::Propose(body), Content::Propose(c)) = (&self.body, &typed) {
            if c.plan.action != body.action {
                return Err(Error::invalid("plan.action differs from action"));
            }
            if ids::json_digest(&content["plan"])? != body.plan_digest {
                return Err(Error::invalid("planDigest does not match the plan"));
            }
        }
        Ok(())
    }
}

impl Receipt {
    /// Checks the receipt's shape: grammar of every field, `seq >= 1`, `prev` empty exactly when
    /// `seq` is 1. The signature and chain are the verifier's.
    pub fn validate(&self) -> Result<(), Error> {
        let max = MAX_SAFE_INT as u64;
        let ok = g::is_space_id(&self.space)
            && (1..=max).contains(&self.seq)
            && g::is_hex(&self.id)
            && (self.prev.is_empty() == (self.seq == 1))
            && (self.prev.is_empty() || g::is_hex(&self.prev))
            && g::is_server_name(&self.home)
            && g::is_key_id(&self.home_key)
            && g::is_time(&self.time)
            && self
                .profiles
                .iter()
                .all(|(a, n)| g::is_actor_id(a) && *n <= max);
        if ok {
            Ok(())
        } else {
            Err(Error::invalid("receipt"))
        }
    }
}

impl LogRecord {
    /// Checks a record's shape and its internal consistency: the entry (and its content unless
    /// redacted), the receipt, `receipt.id` equal to the entry's id, and `receipt.space` equal to
    /// the entry's. A redacted record must carry `redactedBy` and no content; checking that proof
    /// is the verifier's job (R-14).
    pub fn validate(&self) -> Result<(), Error> {
        self.entry.validate()?;
        self.receipt.validate()?;
        if self.redacted {
            if self.redacted_by.is_none() || self.content.is_some() {
                return Err(Error::invalid("redacted record"));
            }
        } else {
            if self.redacted_by.is_some() {
                return Err(Error::invalid(
                    "redactedBy on a record that is not redacted",
                ));
            }
            self.entry.validate_content(self.content.as_ref())?;
        }
        if self.receipt.space != self.entry.space {
            return Err(Error::invalid("receipt for another space"));
        }
        if self.receipt.id != self.entry.id()? {
            return Err(Error::invalid("receipt id differs from the entry id"));
        }
        Ok(())
    }
}

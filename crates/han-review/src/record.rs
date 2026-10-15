//! The review record and the typed verdict a model returns.
//!
//! The model's answer is data and nothing else: [`Review`] has no field that names an action. The record
//! ([`ReviewRecord`]) is what the reviewer signs over the changeset: it is a JSON blob, canonicalised (RFC
//! 8785) so its bytes, and so its hash, are the same wherever it is rebuilt.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The model's verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Fit to land as it is.
    Approve,
    /// Not fit to land until the findings are dealt with.
    RequestChanges,
    /// The reviewer can't judge it: a person looks.
    NeedsHuman,
}

/// How serious a finding is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// A remark.
    Info,
    /// Should be changed; does not block.
    Warning,
    /// Blocks landing.
    Error,
}

/// One finding about one path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    /// A path of the changeset's delta.
    pub path: String,
    /// The line in the new file, when the finding is about one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// How serious.
    pub severity: Severity,
    /// What is wrong, in a sentence or two.
    pub text: String,
}

/// What the model returns: the whole of its authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    /// The verdict.
    pub verdict: Verdict,
    /// The findings; an `approve` carries none of severity `error`.
    pub findings: Vec<Finding>,
    /// How sure the model is, an integer from 0 to 100 (HAN's JSON has no floats).
    pub confidence: u8,
}

/// Tokens used by the model call, as the backend reports them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    /// Tokens read.
    pub input_tokens: u64,
    /// Tokens written.
    pub output_tokens: u64,
}

/// The record a review leaves: signed (by the reviewer's entry in the log) over `delta_digest`, so a change
/// made after the review is not the reviewed one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewRecord {
    /// The record's schema, `han.review/1`.
    pub schema: String,
    /// The `vcs.land` proposal reviewed.
    pub proposal: String,
    /// The changeset's delta digest, checked against the bundle before the model saw anything.
    pub delta_digest: String,
    /// The verdict.
    pub verdict: Verdict,
    /// The findings.
    pub findings: Vec<Finding>,
    /// The model's confidence, 0 to 100.
    pub confidence: u8,
    /// The model backend, for example `omniroute`.
    pub backend: String,
    /// The model as configured, for example `gemini-flash`.
    pub model: String,
    /// The model as the backend resolved it for this call; `unresolved` when the backend did not say, in
    /// which case the verdict is never `approve`.
    pub model_version: String,
    /// `sha256` of the system prompt.
    pub prompt_hash: String,
    /// `sha256` of the canonical output schema the model was held to.
    pub schema_hash: String,
    /// Tokens used.
    pub usage: Usage,
    /// When the review was made, RFC 3339 UTC.
    pub at: String,
}

/// The record's schema name.
pub const RECORD_SCHEMA: &str = "han.review/1";

/// The media type of the record blob.
pub const RECORD_MEDIA_TYPE: &str = "application/vnd.han.review+json";

/// Why a record could not be encoded or read.
#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    /// The value can't be canonicalised.
    #[error("record encoding: {0}")]
    Encode(String),
    /// The bytes are not a review record.
    #[error("not a review record: {0}")]
    Decode(String),
}

impl ReviewRecord {
    /// The record as a canonical JSON blob.
    pub fn to_blob(&self) -> Result<Vec<u8>, RecordError> {
        let v = han_core::json::to_value(self).map_err(|e| RecordError::Encode(e.to_string()))?;
        han_core::json::canonicalize(&v).map_err(|e| RecordError::Encode(e.to_string()))
    }

    /// The blob's hash: what an approval cites as evidence.
    pub fn blob_hash(&self) -> Result<String, RecordError> {
        Ok(han_core::ids::sha256_hex(&self.to_blob()?))
    }

    /// Reads a record blob, refusing unknown fields and other schemas.
    pub fn from_blob(bytes: &[u8]) -> Result<ReviewRecord, RecordError> {
        let r: ReviewRecord =
            serde_json::from_slice(bytes).map_err(|e| RecordError::Decode(e.to_string()))?;
        if r.schema != RECORD_SCHEMA {
            return Err(RecordError::Decode(format!("schema {}", r.schema)));
        }
        Ok(r)
    }
}

/// The JSON schema the model is held to. Part of the request, and its canonical hash is the record's
/// `schemaHash`.
pub fn output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["verdict", "findings", "confidence"],
        "properties": {
            "verdict": {"enum": ["approve", "request_changes", "needs_human"]},
            "findings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["path", "severity", "text"],
                    "properties": {
                        "path": {"type": "string"},
                        "line": {"type": "integer", "minimum": 1},
                        "severity": {"enum": ["info", "warning", "error"]},
                        "text": {"type": "string"}
                    }
                }
            },
            "confidence": {"type": "integer", "minimum": 0, "maximum": 100}
        }
    })
}

/// `sha256` of the canonical [`output_schema`].
pub fn schema_hash() -> String {
    let bytes = han_core::json::to_value(&output_schema())
        .and_then(|v| han_core::json::canonicalize(&v))
        .unwrap_or_default();
    han_core::ids::sha256_hex(&bytes)
}

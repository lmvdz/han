//! The one error type of the crate.

use thiserror::Error;

use crate::base64url::Base64Error;
use crate::json::JsonError;

/// Why something is not acceptable. Every variant corresponds to the protocol's `invalid`
/// (architecture.md section 3): none of them is worth retrying with the same input.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum Error {
    /// The bytes are not strict I-JSON, or a value cannot be canonicalized.
    #[error(transparent)]
    Json(#[from] JsonError),
    /// A key or signature is not canonical base64url.
    #[error(transparent)]
    Base64(#[from] Base64Error),
    /// A shape, grammar or size rule is broken.
    #[error("invalid {0}")]
    Invalid(String),
    /// A signature does not verify (or the public key is unusable).
    #[error("signature does not verify")]
    BadSignature,
}

impl Error {
    pub(crate) fn invalid(what: impl Into<String>) -> Self {
        Error::Invalid(what.into())
    }
}

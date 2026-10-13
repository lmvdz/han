//! HAN core: the primitives every HAN implementation needs, with no async and no I/O.
//!
//! * [`json`]: strict I-JSON parsing and JCS (RFC 8785) canonicalization.
//! * [`base64url`]: canonical unpadded base64url.
//! * [`grammar`]: the identifier grammars of architecture.md section 2.
//! * [`types`]: [`Entry`], [`Receipt`], [`LogRecord`] and the other shapes of `han.ts`.
//! * [`ids`]: [`Entry::id`], [`Receipt::hash`] and the content and blob hashes.
//! * [`sign`]: Ed25519 with the protocol's domain separation, verified with `verify_strict`.
//! * [`Entry::validate`], [`Entry::validate_content`], [`LogRecord::validate`]: shape and limits.
//!
//! Policy is not here yet; it joins this crate after spec round 31.

pub mod base64url;
pub mod error;
pub mod grammar;
pub mod ids;
pub mod json;
pub mod limits;
pub mod sign;
pub mod types;
mod validate;

pub use error::Error;
pub use json::Value;
pub use sign::{PublicKey, SigningKey};
pub use types::{Entry, LogRecord, Receipt};

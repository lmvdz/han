//! base64url without padding, canonical form only (architecture.md section 2).

use base64::Engine as _;
use base64::alphabet::URL_SAFE;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use thiserror::Error;

const ENGINE: GeneralPurpose = GeneralPurpose::new(
    &URL_SAFE,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::RequireNone)
        .with_decode_allow_trailing_bits(false),
);

/// The input is not canonical base64url: wrong alphabet, padding, impossible length, or nonzero
/// unused bits in the last character.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("not canonical base64url")]
pub struct Base64Error;

/// Encodes `bytes` as base64url without padding.
pub fn encode(bytes: &[u8]) -> String {
    ENGINE.encode(bytes)
}

/// Decodes canonical base64url. Anything that would not re-encode to the same string is an error.
pub fn decode(s: &str) -> Result<Vec<u8>, Base64Error> {
    let bytes = ENGINE.decode(s).map_err(|_| Base64Error)?;
    // Belt and braces: the spec's rule is "decoding and re-encoding gives the same string".
    if encode(&bytes) == s {
        Ok(bytes)
    } else {
        Err(Base64Error)
    }
}

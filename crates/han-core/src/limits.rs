//! The size limits of architecture.md section 2 (`Limits` in han.ts). Exceeding any is `invalid`.

/// One frame: the UTF-8 length of the frame's JSON (G-07).
pub const MAX_FRAME_BYTES: usize = 1_310_720;
/// The JCS of an Entry as sent, `sig` included.
pub const MAX_HEADER_BYTES: usize = 16_384;
/// The JCS of a content.
pub const MAX_CONTENT_BYTES: usize = 65_536;
/// A blob's size.
pub const MAX_BLOB_BYTES: u64 = 4_294_967_296;
/// Items in `to`, `refs`, `blobs` and `links`, and approval rules.
pub const MAX_LIST: usize = 64;
/// Entries in `servers.deny`.
pub const MAX_DENIED_SERVERS: usize = 256;

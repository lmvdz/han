//! Frames on a stream (G-07): a 4-byte big-endian length, then that many bytes of UTF-8 I-JSON.

use han_core::frame::encode_frame;
use han_core::limits::MAX_FRAME_BYTES;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::{NetError, Result};

/// Reads one frame. `Ok(None)` is a clean end of the stream before any byte of a frame. A length over
/// `maxFrameBytes` is refused from the four length bytes alone, before the payload is read, and a payload
/// that is not UTF-8 is refused: both are [`NetError::Frame`], and the connection closes.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<String>> {
    let mut head = [0u8; 4];
    let mut got = 0;
    while got < head.len() {
        match r.read(&mut head[got..]).await? {
            0 if got == 0 => return Ok(None),
            0 => return Err(NetError::Closed("stream ended inside a frame length".into())),
            n => got += n,
        }
    }
    let len = u32::from_be_bytes(head) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(NetError::Frame("frame too long"));
    }
    let mut payload = Vec::with_capacity(len.min(64 * 1024));
    let read = r.take(len as u64).read_to_end(&mut payload).await?;
    if read < len {
        return Err(NetError::Closed("stream ended inside a frame".into()));
    }
    String::from_utf8(payload)
        .map(Some)
        .map_err(|_| NetError::Frame("not UTF-8"))
}

/// Writes one frame.
pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, text: &str) -> Result<()> {
    let bytes = encode_frame(text).map_err(|_| NetError::Frame("frame too long"))?;
    w.write_all(&bytes).await?;
    Ok(())
}

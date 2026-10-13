//! Typed one-way streams (G-07, round 32 Z5): framing only. After `hello`, either end may open a
//! unidirectional stream whose first frame is its type name (a 4-byte length, then UTF-8 in the profile-name
//! grammar, at most 64 bytes), followed by the profile's frames, each a 4-byte length (at most
//! `maxFrameBytes`) and that many bytes. The core signs and checks nothing inside the frames; a bad header
//! or an over-long frame resets the stream, and an unknown type is simply not read.

use han_core::limits::MAX_FRAME_BYTES;
use han_seq::stream_item::is_profile_name;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::{NetError, Result};

/// The longest stream type name.
pub const MAX_TYPE_BYTES: usize = 64;

/// A whole typed stream, as read to its end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedStream {
    /// The stream type: a profile name such as `han.timeline/1`.
    pub kind: String,
    /// The profile's frames.
    pub frames: Vec<Vec<u8>>,
}

/// How a byte string reads as a typed stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// A finished stream that reads whole.
    Stream(TypedStream),
    /// An unfinished stream that ends inside a frame: wait for more bytes.
    More,
    /// A bad header, an over-long frame, or a finished stream that ends inside a frame: reset it.
    Close,
}

/// Reads a 4-byte length; `None` is a clean end before its first byte.
async fn read_len<R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<usize>> {
    let mut head = [0u8; 4];
    let mut got = 0;
    while got < head.len() {
        match r.read(&mut head[got..]).await? {
            0 if got == 0 => return Ok(None),
            0 => {
                return Err(NetError::Closed(
                    "stream ended inside a frame length".into(),
                ));
            }
            n => got += n,
        }
    }
    Ok(Some(u32::from_be_bytes(head) as usize))
}

async fn read_body<R: AsyncRead + Unpin>(r: &mut R, len: usize) -> Result<Vec<u8>> {
    let mut payload = Vec::with_capacity(len.min(64 * 1024));
    let read = r.take(len as u64).read_to_end(&mut payload).await?;
    if read < len {
        return Err(NetError::Closed("stream ended inside a frame".into()));
    }
    Ok(payload)
}

/// A typed stream being read: the header has been read and checked, the frames come one at a time.
pub struct TypedReader<R> {
    kind: String,
    inner: R,
}

impl<R: AsyncRead + Unpin> TypedReader<R> {
    /// Reads and checks the header frame: a length over 64 is refused from the four length bytes alone, and the
    /// name must be a profile name.
    pub async fn open(mut inner: R) -> Result<Self> {
        let len = read_len(&mut inner)
            .await?
            .ok_or(NetError::Frame("typed stream without a header"))?;
        if len > MAX_TYPE_BYTES {
            return Err(NetError::Frame("stream type too long"));
        }
        let name = read_body(&mut inner, len).await?;
        let kind = String::from_utf8(name)
            .ok()
            .filter(|n| is_profile_name(n))
            .ok_or(NetError::Frame("stream type is not a profile name"))?;
        Ok(TypedReader { kind, inner })
    }

    /// The stream's type.
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// The next frame; `None` at a clean end. A length over `maxFrameBytes` is refused from the four length
    /// bytes alone.
    pub async fn next(&mut self) -> Result<Option<Vec<u8>>> {
        let Some(len) = read_len(&mut self.inner).await? else {
            return Ok(None);
        };
        if len > MAX_FRAME_BYTES {
            return Err(NetError::Frame("frame too long"));
        }
        read_body(&mut self.inner, len).await.map(Some)
    }
}

/// Writes a stream's header frame.
pub async fn write_header<W: AsyncWrite + Unpin>(w: &mut W, kind: &str) -> Result<()> {
    if kind.len() > MAX_TYPE_BYTES || !is_profile_name(kind) {
        return Err(NetError::Frame("stream type is not a profile name"));
    }
    w.write_all(&(kind.len() as u32).to_be_bytes()).await?;
    w.write_all(kind.as_bytes()).await?;
    Ok(())
}

/// Writes one of the profile's frames.
pub async fn write_typed_frame<W: AsyncWrite + Unpin>(w: &mut W, frame: &[u8]) -> Result<()> {
    if frame.len() > MAX_FRAME_BYTES {
        return Err(NetError::Frame("frame too long"));
    }
    w.write_all(&(frame.len() as u32).to_be_bytes()).await?;
    w.write_all(frame).await?;
    Ok(())
}

/// The bytes of a whole stream of type `kind`.
pub async fn encode(kind: &str, frames: &[Vec<u8>]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    write_header(&mut out, kind).await?;
    for f in frames {
        write_typed_frame(&mut out, f).await?;
    }
    Ok(out)
}

/// Reads `bytes` as a typed stream; `finished` says whether the sender has ended it.
pub async fn decode(bytes: &[u8], finished: bool) -> Verdict {
    let ended_early = if finished {
        Verdict::Close
    } else {
        Verdict::More
    };
    let mut reader = match TypedReader::open(bytes).await {
        Ok(r) => r,
        Err(NetError::Closed(_)) => return ended_early,
        Err(NetError::Frame("typed stream without a header")) if !finished => return Verdict::More,
        Err(_) => return Verdict::Close,
    };
    let mut frames = Vec::new();
    loop {
        match reader.next().await {
            Ok(Some(f)) => frames.push(f),
            Ok(None) if finished => {
                return Verdict::Stream(TypedStream {
                    kind: reader.kind,
                    frames,
                });
            }
            Ok(None) => return Verdict::More,
            Err(NetError::Closed(_)) => return ended_early,
            Err(_) => return Verdict::Close,
        }
    }
}

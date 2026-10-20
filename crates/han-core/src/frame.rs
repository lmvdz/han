//! The frames of the `han/1` carrier (architecture.md section 8.2, G-07): a 4-byte big-endian
//! length, then that many bytes of UTF-8 I-JSON, and the first checks of a request frame.

use crate::error::Error;
use crate::json::{self, MAX_SAFE_INT, Value};
use crate::limits::MAX_FRAME_BYTES;

/// A frame nests at most this many arrays and objects, the frame object itself counted.
pub const MAX_FRAME_DEPTH: usize = json::MAX_DEPTH;

/// The calls of section 3 and the ten of section 8.2.
const CALLS: [&str; 15] = [
    "effectReady",
    "hello",
    "append",
    "read",
    "state",
    "subscribe",
    "signal",
    "host",
    "blobPut",
    "blobGet",
    "stream",
    "watch",
    "cancel",
    "invite",
    "join",
];

/// The control calls of test mode (G-11), and of nothing else.
const TEST_CALLS: [&str; 2] = ["test.clock", "test.limits"];

/// A frame on a stream: the 4-byte big-endian length of the UTF-8 bytes, then the bytes. Fails
/// if the text is longer than `maxFrameBytes`.
pub fn encode_frame(text: &str) -> Result<Vec<u8>, Error> {
    let len = u32::try_from(text.len())
        .ok()
        .filter(|_| text.len() <= MAX_FRAME_BYTES)
        .ok_or_else(|| Error::invalid("frame over maxFrameBytes"))?;
    let mut out = Vec::with_capacity(4 + text.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(text.as_bytes());
    Ok(out)
}

/// What the front of a receive buffer holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decoded<'a> {
    /// A complete frame: its text, and the bytes after it.
    Frame {
        /// The frame's JSON text.
        text: &'a str,
        /// The rest of the buffer.
        rest: &'a [u8],
    },
    /// Less than one frame: read more.
    More,
    /// Close the connection: the length is over `maxFrameBytes` (judged from the four length
    /// bytes alone, before any payload arrives) or the payload is not UTF-8.
    Close(&'static str),
}

/// Reads one frame from the front of `buf`.
pub fn decode_frame(buf: &[u8]) -> Decoded<'_> {
    let Some((head, body)) = buf.split_first_chunk::<4>() else {
        return Decoded::More;
    };
    let len = u32::from_be_bytes(*head) as usize;
    if len > MAX_FRAME_BYTES {
        return Decoded::Close("frame too long");
    }
    if body.len() < len {
        return Decoded::More;
    }
    let (payload, rest) = body.split_at(len);
    match std::str::from_utf8(payload) {
        Ok(text) => Decoded::Frame { text, rest },
        Err(_) => Decoded::Close("not UTF-8"),
    }
}

/// What G-07 (1), (1a) and (2) say about a frame that is not a request frame: the id of the
/// `invalid` error to send (0 when there is no usable id) and whether to close the connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rejected {
    /// The id of the error reply.
    pub id: u64,
    /// Whether the connection closes after it.
    pub close: bool,
}

/// G-07 (1), (1a), (2) for one frame on a call stream. `None` means it is a request frame
/// `{id, call, args}` with a usable id and a known call: go on to `hello` or `admit`, and to the
/// call's args. `test_mode` adds the control calls of G-11.
pub fn request_verdict(text: &str, test_mode: bool) -> Option<Rejected> {
    let reject = |id, close| Some(Rejected { id, close });
    let Ok((value, wide)) = json::parse_frame(text.as_bytes()) else {
        return reject(0, true); // not I-JSON: -0, a fraction, a duplicate key, nested too deep
    };
    let Value::Object(members) = &value else {
        return reject(0, true); // (1) an array, a scalar or null
    };
    let id = match members.get("id") {
        Some(Value::Int(i)) if (1..=MAX_SAFE_INT).contains(i) => Some(i.unsigned_abs()),
        _ => None,
    };
    if wide {
        // (1a) an integer outside the safe range: answered on the request's own id when usable
        return match id {
            Some(id) => reject(id, false),
            None => reject(0, true),
        };
    }
    let known_call = members
        .get("call")
        .and_then(Value::as_str)
        .is_some_and(|c| CALLS.contains(&c) || (test_mode && TEST_CALLS.contains(&c)));
    let shape = members.len() == 3
        && members.contains_key("call")
        && members.contains_key("args")
        && matches!(members.get("args"), Some(Value::Object(_)));
    match id {
        Some(id) if shape && known_call => {
            let _ = id;
            None
        }
        Some(id) => reject(id, false),
        None => reject(0, false),
    }
}

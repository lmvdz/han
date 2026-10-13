//! The error type of the carrier.

use han_seq::WireError;

/// What can go wrong carrying frames.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    /// An I/O error on a stream.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The connection (or a stream of it) is closed.
    #[error("closed: {0}")]
    Closed(String),
    /// The peer broke the framing rules (G-07): the connection is closed.
    #[error("frame: {0}")]
    Frame(&'static str),
    /// The peer broke a protocol rule the client checks (G-08).
    #[error("protocol: {0}")]
    Protocol(String),
    /// The node could not be reached.
    #[error("dial: {0}")]
    Dial(String),
    /// The call ended with a wire error.
    #[error("{0}")]
    Remote(#[from] WireError),
}

impl NetError {
    /// The wire error, if the call ended with one.
    pub fn wire(&self) -> Option<&WireError> {
        match self {
            NetError::Remote(e) => Some(e),
            _ => None,
        }
    }
}

/// Results of the carrier.
pub type Result<T> = std::result::Result<T, NetError>;

//! The wire errors of architecture.md section 3.

use han_core::json::Value;

/// The closed set of error codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    /// Policy or authentication refused it; never retry the same entry.
    Denied,
    /// Malformed; never retry.
    Invalid,
    /// There is no such thing, or the caller may not know of it.
    NotFound,
    /// A rate limit; retry after `retryAfterMs`.
    Limit,
    /// The same request later, after `retryAfterMs` when given.
    Unavailable,
    /// The same request at `movedTo`.
    Moved,
}

impl Code {
    /// The code as written on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            Code::Denied => "denied",
            Code::Invalid => "invalid",
            Code::NotFound => "not_found",
            Code::Limit => "limit",
            Code::Unavailable => "unavailable",
            Code::Moved => "moved",
        }
    }
}

/// An error a call ends with (`WireError` of `han.ts`).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{}: {message}", code.as_str())]
pub struct WireError {
    /// The code.
    pub code: Code,
    /// Free text, never normative.
    pub message: String,
    /// For `limit`, and `unavailable` when waiting helps.
    pub retry_after_ms: Option<u64>,
    /// For `moved`: the ServerName that has the space.
    pub moved_to: Option<String>,
}

impl WireError {
    fn new(code: Code, message: impl Into<String>) -> Self {
        WireError {
            code,
            message: message.into(),
            retry_after_ms: None,
            moved_to: None,
        }
    }

    /// `denied`.
    pub fn denied(message: impl Into<String>) -> Self {
        Self::new(Code::Denied, message)
    }

    /// `invalid`.
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(Code::Invalid, message)
    }

    /// `not_found`.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(Code::NotFound, message)
    }

    /// `unavailable`.
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(Code::Unavailable, message)
    }

    /// `limit` with a retry hint.
    pub fn limit(message: impl Into<String>, retry_after_ms: u64) -> Self {
        WireError {
            retry_after_ms: Some(retry_after_ms),
            ..Self::new(Code::Limit, message)
        }
    }

    /// `moved` to `server`.
    pub fn moved(server: impl Into<String>) -> Self {
        WireError {
            moved_to: Some(server.into()),
            ..Self::new(Code::Moved, "this space is homed elsewhere")
        }
    }

    /// Adds a retry hint.
    #[must_use]
    pub fn retry_after(mut self, ms: u64) -> Self {
        self.retry_after_ms = Some(ms);
        self
    }

    /// The `WireError` object.
    pub fn to_value(&self) -> Value {
        let mut m = std::collections::BTreeMap::new();
        m.insert("code".into(), Value::Str(self.code.as_str().into()));
        m.insert("message".into(), Value::Str(self.message.clone()));
        if let Some(ms) = self.retry_after_ms {
            m.insert(
                "retryAfterMs".into(),
                Value::Int(i64::try_from(ms).unwrap_or(i64::MAX)),
            );
        }
        if let Some(s) = &self.moved_to {
            m.insert("movedTo".into(), Value::Str(s.clone()));
        }
        Value::Object(m)
    }
}

impl From<han_core::Error> for WireError {
    fn from(e: han_core::Error) -> Self {
        WireError::invalid(e.to_string())
    }
}

/// The result of a call.
pub type Result<T> = std::result::Result<T, WireError>;

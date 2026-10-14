//! The closed set of errors, mirroring the protocol's verdicts (PLAN section 5).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The start of the message of a `denied` error when a person did not confirm an approval on the device (the
/// presence check was cancelled, timed out, failed or does not exist). Faces show what follows it as is.
pub const APPROVAL_NOT_SIGNED: &str = "approval not signed: ";

/// Why a call or a view failed. The set is closed: new failures map onto one of these.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum NodeError {
    /// Policy, authentication or the peer filter refused it; retrying the same thing is pointless.
    #[error("denied: {message}")]
    Denied {
        /// Free text, never normative.
        message: String,
    },
    /// The request is malformed, or names something that is not valid.
    #[error("invalid: {message}")]
    Invalid {
        /// Free text.
        message: String,
    },
    /// There is no such space, session, task or proposal, or this device may not know of it.
    #[error("not_found: {message}")]
    NotFound {
        /// Free text.
        message: String,
    },
    /// The device cannot do it now (no route to the home, a rate limit, a role that is not running); the
    /// same request later may work.
    #[error("unavailable: {message}")]
    Unavailable {
        /// Free text.
        message: String,
        /// When waiting helps, how long.
        #[ts(type = "number | null")]
        retry_after_ms: Option<u64>,
        /// What the request waits for, when it is state that is on its way (a space still syncing).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        pending: Option<Pending>,
    },
    /// The thing is in a state that rules the request out (a task already closed, a space already followed).
    #[error("conflict: {message}")]
    Conflict {
        /// Free text.
        message: String,
    },
}

/// State a request needs that has not arrived yet: waiting, not retrying the request unchanged, is what helps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "what", rename_all = "snake_case")]
pub enum Pending {
    /// The first records of a followed space have not arrived from its home.
    Space {
        /// The space.
        space: String,
    },
    /// The profile of an actor (its operator, its keys) has not arrived from the actor's home.
    Profile {
        /// The actor.
        actor: String,
    },
}

impl NodeError {
    /// `denied`.
    pub fn denied(message: impl Into<String>) -> Self {
        NodeError::Denied {
            message: message.into(),
        }
    }

    /// `invalid`.
    pub fn invalid(message: impl Into<String>) -> Self {
        NodeError::Invalid {
            message: message.into(),
        }
    }

    /// `not_found`.
    pub fn not_found(message: impl Into<String>) -> Self {
        NodeError::NotFound {
            message: message.into(),
        }
    }

    /// `unavailable`, with no retry hint.
    pub fn unavailable(message: impl Into<String>) -> Self {
        NodeError::Unavailable {
            message: message.into(),
            retry_after_ms: None,
            pending: None,
        }
    }

    /// `unavailable` because `what` has not arrived yet.
    pub fn pending(message: impl Into<String>, what: Pending) -> Self {
        NodeError::Unavailable {
            message: message.into(),
            retry_after_ms: None,
            pending: Some(what),
        }
    }

    /// What the request waits for, if this is `unavailable` for that reason.
    pub fn waiting_for(&self) -> Option<&Pending> {
        match self {
            NodeError::Unavailable { pending, .. } => pending.as_ref(),
            _ => None,
        }
    }

    /// `conflict`.
    pub fn conflict(message: impl Into<String>) -> Self {
        NodeError::Conflict {
            message: message.into(),
        }
    }

    /// The code as written in JSON.
    pub fn code(&self) -> &'static str {
        match self {
            NodeError::Denied { .. } => "denied",
            NodeError::Invalid { .. } => "invalid",
            NodeError::NotFound { .. } => "not_found",
            NodeError::Unavailable { .. } => "unavailable",
            NodeError::Conflict { .. } => "conflict",
        }
    }
}

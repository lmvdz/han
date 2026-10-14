//! The check that refuses.

use crate::{Presence, PresenceError, PresencePrompt, PresenceProof};

/// Refuses every prompt with a reason. It is what a device without a check uses, and what an unsupported
/// backend falls back to: a missing check must never read as a passed one.
pub struct DenyAll {
    reason: String,
}

impl DenyAll {
    /// A check that fails with `reason`.
    pub fn new(reason: impl Into<String>) -> Self {
        DenyAll {
            reason: reason.into(),
        }
    }
}

impl Presence for DenyAll {
    fn name(&self) -> &'static str {
        "none"
    }

    fn confirm(&self, _: &PresencePrompt) -> Result<PresenceProof, PresenceError> {
        Err(PresenceError::Unavailable(self.reason.clone()))
    }
}

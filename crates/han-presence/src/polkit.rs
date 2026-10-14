//! Linux: polkit, through `pkcheck`, under the action id [`ACTION_ID`] (policy file in `polkit/`).
//!
//! `pkcheck` cannot carry the prompt text, so polkit shows the policy's fixed message and the person confirms
//! "a HAN approval", not the digest.
//! TODO(presence): call `org.freedesktop.PolicyKit1.Authority.CheckAuthorization` over D-Bus with the
//! `polkit.message` detail so the prompt shows the action and the short digest, and add an optional FIDO2
//! backend (a security key's user-presence touch) for devices that have one.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::{Presence, PresenceError, PresencePrompt, PresenceProof};

/// The polkit action the node asks about.
pub const ACTION_ID: &str = "dev.han.approve";

/// polkit through `pkcheck`.
pub struct Polkit {
    program: String,
}

impl Polkit {
    /// Uses `pkcheck` from the path.
    pub fn new() -> Self {
        Polkit {
            program: "pkcheck".into(),
        }
    }

    /// Uses another program, for tests.
    pub fn with_program(program: &str) -> Self {
        Polkit {
            program: program.into(),
        }
    }

    fn args() -> Vec<String> {
        vec![
            "--action-id".into(),
            ACTION_ID.into(),
            "--process".into(),
            std::process::id().to_string(),
            "--allow-user-interaction".into(),
        ]
    }
}

impl Default for Polkit {
    fn default() -> Self {
        Self::new()
    }
}

/// What `pkcheck`'s exit status says.
fn from_status(code: Option<i32>) -> Result<(), PresenceError> {
    match code {
        Some(0) => Ok(()),
        Some(1) => Err(PresenceError::Denied("polkit did not authorize it".into())),
        Some(2) => Err(PresenceError::Cancelled),
        Some(3) => Err(PresenceError::Unavailable(
            "no polkit authentication agent is running in this session, so nobody can be asked"
                .into(),
        )),
        other => Err(PresenceError::Unavailable(format!(
            "pkcheck failed ({other:?}); is dev.han.approve.policy installed?"
        ))),
    }
}

impl Presence for Polkit {
    fn name(&self) -> &'static str {
        "polkit"
    }

    fn confirm(&self, prompt: &PresencePrompt) -> Result<PresenceProof, PresenceError> {
        let mut child = Command::new(&self.program)
            .args(Self::args())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| PresenceError::Unavailable(format!("cannot run {}: {e}", self.program)))?;
        let deadline = Instant::now() + prompt.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(PresenceError::TimedOut);
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => return Err(PresenceError::Unavailable(format!("pkcheck: {e}"))),
            }
        };
        from_status(status.code())?;
        Ok(PresenceProof::new("polkit", &prompt.plan_digest_short))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_statuses_map_to_outcomes() {
        assert!(from_status(Some(0)).is_ok());
        assert_eq!(from_status(Some(2)), Err(PresenceError::Cancelled));
        assert!(matches!(
            from_status(Some(1)),
            Err(PresenceError::Denied(_))
        ));
        assert!(matches!(
            from_status(Some(3)),
            Err(PresenceError::Unavailable(_))
        ));
        assert!(matches!(
            from_status(None),
            Err(PresenceError::Unavailable(_))
        ));
    }

    #[test]
    fn a_missing_program_is_unavailable_not_allowed() {
        let p = Polkit::with_program("han-no-such-pkcheck");
        let prompt = PresencePrompt {
            action: "a".into(),
            target: "t".into(),
            plan_digest_short: "d".into(),
            space: "s".into(),
            requester: "r".into(),
            timeout: Duration::from_secs(1),
        };
        assert!(matches!(
            p.confirm(&prompt),
            Err(PresenceError::Unavailable(_))
        ));
    }
}

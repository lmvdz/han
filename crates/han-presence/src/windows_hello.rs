//! Windows: Windows Hello through WinRT `Windows.Security.Credentials.UI.UserConsentVerifier`.
//!
//! The message of the request is the text Windows shows in the Hello dialog (the action and the short plan
//! digest). Hello offers face, fingerprint or PIN, whichever the person set up.

use std::time::{Duration, Instant};

use windows::Security::Credentials::UI::{
    UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability,
};
use windows::core::HSTRING;
use windows_future::{AsyncStatus, IAsyncOperation};

use crate::{Presence, PresenceError, PresencePrompt, PresenceProof};

/// Windows Hello.
pub struct WindowsHello;

/// Whether Hello can verify anyone on this device, without showing anything.
pub fn availability() -> Result<UserConsentVerifierAvailability, PresenceError> {
    let op = UserConsentVerifier::CheckAvailabilityAsync()
        .map_err(|e| PresenceError::Unavailable(format!("Windows Hello: {e}")))?;
    wait(&op, Duration::from_secs(10))?
        .map_err(|e| PresenceError::Unavailable(format!("Windows Hello: {e}")))
}

/// Polls `op` until it ends or `timeout` passes. On a timeout the operation is cancelled (which closes the
/// dialog) and the wait is `Err(TimedOut)`; otherwise the operation's own result is the inner one.
fn wait<T: windows::core::RuntimeType + 'static>(
    op: &IAsyncOperation<T>,
    timeout: Duration,
) -> Result<windows::core::Result<T>, PresenceError> {
    let deadline = Instant::now() + timeout;
    loop {
        match op.Status() {
            Ok(AsyncStatus::Started) => {}
            Ok(AsyncStatus::Completed) => return Ok(op.GetResults()),
            Ok(_) => return Ok(op.GetResults()),
            Err(e) => return Ok(Err(e)),
        }
        if Instant::now() >= deadline {
            let _ = op.Cancel();
            return Err(PresenceError::TimedOut);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn unavailable(a: UserConsentVerifierAvailability) -> Option<&'static str> {
    if a == UserConsentVerifierAvailability::Available {
        None
    } else if a == UserConsentVerifierAvailability::DeviceNotPresent {
        Some(
            "this device has no Windows Hello hardware (no camera, fingerprint reader or PIN support)",
        )
    } else if a == UserConsentVerifierAvailability::NotConfiguredForUser {
        Some(
            "Windows Hello is not set up for this user; set up a PIN or biometrics in Settings > Accounts",
        )
    } else if a == UserConsentVerifierAvailability::DisabledByPolicy {
        Some("Windows Hello is disabled by policy on this device")
    } else {
        Some("Windows Hello is busy or unavailable")
    }
}

fn outcome(r: UserConsentVerificationResult) -> Result<(), PresenceError> {
    if r == UserConsentVerificationResult::Verified {
        Ok(())
    } else if r == UserConsentVerificationResult::Canceled {
        Err(PresenceError::Cancelled)
    } else if r == UserConsentVerificationResult::RetriesExhausted {
        Err(PresenceError::Denied("too many failed attempts".into()))
    } else if r == UserConsentVerificationResult::DeviceNotPresent
        || r == UserConsentVerificationResult::NotConfiguredForUser
        || r == UserConsentVerificationResult::DisabledByPolicy
    {
        Err(PresenceError::Unavailable(
            "Windows Hello is not usable on this device".into(),
        ))
    } else {
        Err(PresenceError::Denied(
            "Windows Hello is busy; try again".into(),
        ))
    }
}

impl Presence for WindowsHello {
    fn name(&self) -> &'static str {
        "windows-hello"
    }

    fn confirm(&self, prompt: &PresencePrompt) -> Result<PresenceProof, PresenceError> {
        if let Some(why) = unavailable(availability()?) {
            return Err(PresenceError::Unavailable(why.into()));
        }
        let op = UserConsentVerifier::RequestVerificationAsync(&HSTRING::from(prompt.message()))
            .map_err(|e| PresenceError::Unavailable(format!("Windows Hello: {e}")))?;
        match wait(&op, prompt.timeout)? {
            Ok(r) => outcome(r)?,
            Err(e) => return Err(PresenceError::Unavailable(format!("Windows Hello: {e}"))),
        }
        Ok(PresenceProof::new(
            "windows-hello",
            &prompt.plan_digest_short,
        ))
    }
}

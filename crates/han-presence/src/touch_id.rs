//! macOS: LocalAuthentication (`LAContext evaluatePolicy`), through objc2. Touch ID, with the Mac's password as
//! the fallback `DeviceOwnerAuthentication` allows. The reason string is the prompt text.
//!
//! TODO(presence): this compiles for macOS (`cargo check --target aarch64-apple-darwin`) but has not run on a Mac.

#![allow(unsafe_code)]

use std::sync::mpsc;

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_foundation::{NSError, NSString};
use objc2_local_authentication::{LAContext, LAPolicy};

use crate::{Presence, PresenceError, PresencePrompt, PresenceProof};

/// Touch ID.
pub struct TouchId;

/// `LAErrorUserCancel`, `LAErrorSystemCancel` and `LAErrorAppCancel`.
const CANCELLED: [isize; 3] = [-2, -4, -9];

impl Presence for TouchId {
    fn name(&self) -> &'static str {
        "touch-id"
    }

    fn confirm(&self, prompt: &PresencePrompt) -> Result<PresenceProof, PresenceError> {
        let policy = LAPolicy::DeviceOwnerAuthentication;
        let ctx = unsafe { LAContext::new() };
        if let Err(e) = unsafe { ctx.canEvaluatePolicy_error(policy) } {
            return Err(PresenceError::Unavailable(format!(
                "Touch ID is not usable on this Mac: {}",
                e.localizedDescription()
            )));
        }
        let (tx, rx) = mpsc::channel::<Result<(), isize>>();
        let reply = RcBlock::new(move |ok: Bool, err: *mut NSError| {
            let r = if ok.as_bool() {
                Ok(())
            } else if err.is_null() {
                Err(0)
            } else {
                Err(unsafe { (*err).code() })
            };
            let _ = tx.send(r);
        });
        let reason = NSString::from_str(&prompt.message());
        unsafe { ctx.evaluatePolicy_localizedReason_reply(policy, &reason, &reply) };
        match rx.recv_timeout(prompt.timeout) {
            Ok(Ok(())) => Ok(PresenceProof::new("touch-id", &prompt.plan_digest_short)),
            Ok(Err(code)) if CANCELLED.contains(&code) => Err(PresenceError::Cancelled),
            Ok(Err(code)) => Err(PresenceError::Denied(format!(
                "LocalAuthentication error {code}"
            ))),
            Err(_) => {
                unsafe { ctx.invalidate() };
                Err(PresenceError::TimedOut)
            }
        }
    }
}

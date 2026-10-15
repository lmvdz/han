//! `han-presence`: a human-presence check the node runs before it signs an approval.
//!
//! Terminal input is shared and injectable (other processes, terminal plugins, built-in agents, attached
//! clients), so a key press in a face proves nothing about who is at the device. The node therefore asks the
//! operating system to confirm a person out of band, with a prompt that names the action and the short plan
//! digest: Windows Hello on Windows, Touch ID on macOS, polkit on Linux. The prompt runs on the node, never in
//! a face.
//!
//! The check is local to the device and changes nothing on the wire. A build without a backend for its
//! platform denies; nothing here allows by default. Doubles that allow exist only under the `test-doubles`
//! feature (and `cfg(test)`).

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

mod deny;
pub mod polkit;
#[cfg(any(test, feature = "test-doubles"))]
pub mod testing;
#[cfg(target_os = "macos")]
mod touch_id;
#[cfg(windows)]
pub mod windows_hello;

pub use deny::DenyAll;

/// What the person is asked to confirm.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresencePrompt {
    /// The action the plan names, such as `git_push`.
    pub action: String,
    /// The target the plan names.
    pub target: String,
    /// The first characters of the plan digest, the same the person saw in the sign panel.
    pub plan_digest_short: String,
    /// The space.
    pub space: String,
    /// Who proposed the effect.
    pub requester: String,
    /// How long to wait for the person before giving up.
    pub timeout: Duration,
}

impl PresencePrompt {
    /// The action of a prompt that unlocks a face connection for governing and steering, not an approval.
    pub const UNLOCK: &'static str = "unlock";

    /// How many characters of the digest the prompt shows.
    pub const DIGEST_CHARS: usize = 12;

    /// The short form of a plan digest.
    pub fn short(digest: &str) -> String {
        digest.chars().take(Self::DIGEST_CHARS).collect()
    }

    /// The text of the system prompt: one line, so every backend can show it.
    pub fn message(&self) -> String {
        if self.action == Self::UNLOCK {
            return format!(
                "Unlock governing and steering for a local connection on {} (changes who may do what, and sends input to sessions)",
                clip(&self.space),
            );
        }
        format!(
            "Approve {} on {} (plan {}) in {}, proposed by {}",
            clip(&self.action),
            clip(&self.target),
            self.plan_digest_short,
            clip(&self.space),
            clip(&self.requester),
        )
    }
}

/// Control characters would let a plan forge the prompt's text; long values would push the digest off it.
fn clip(s: &str) -> String {
    let clean: String = s.chars().filter(|c| !c.is_control()).collect();
    if clean.chars().count() > 60 {
        let head: String = clean.chars().take(59).collect();
        format!("{head}…")
    } else {
        clean
    }
}

/// Evidence that a person confirmed one prompt. It names the digest it was given, so the node can check that
/// what was confirmed is what it is about to sign.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresenceProof {
    backend: &'static str,
    plan_digest_short: String,
}

impl PresenceProof {
    /// A proof by `backend` for the prompt that carried `plan_digest_short`.
    pub fn new(backend: &'static str, plan_digest_short: impl Into<String>) -> Self {
        PresenceProof {
            backend,
            plan_digest_short: plan_digest_short.into(),
        }
    }

    /// The backend that confirmed.
    pub fn backend(&self) -> &'static str {
        self.backend
    }

    /// The short digest the person was shown.
    pub fn plan_digest_short(&self) -> &str {
        &self.plan_digest_short
    }
}

/// Why no proof was made.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PresenceError {
    /// The person dismissed the prompt.
    #[error("presence check cancelled")]
    Cancelled,
    /// Nobody answered in time.
    #[error("presence check timed out")]
    TimedOut,
    /// The system looked at the person and said no (a wrong finger, too many tries, a policy).
    #[error("presence check failed: {0}")]
    Denied(String),
    /// This device cannot check presence (no backend, nothing enrolled, no agent running).
    #[error("{0}")]
    Unavailable(String),
}

/// Checks that a person is at the device.
pub trait Presence: Send + Sync {
    /// The backend's name, as the `presence.backend` setting spells it.
    fn name(&self) -> &'static str;

    /// Shows `prompt` to the person and waits for them. It blocks (up to `prompt.timeout`), so an async caller
    /// runs it on a blocking thread. It returns a proof only if the person confirmed.
    fn confirm(&self, prompt: &PresencePrompt) -> Result<PresenceProof, PresenceError>;
}

impl dyn Presence {
    /// A check that refuses everything: the backend of a device without one.
    pub fn deny_all() -> Arc<dyn Presence> {
        Arc::new(DenyAll::new(
            "this device has no presence check; set presence.backend (windows-hello, touch-id or polkit), or \
             set presence.require to the empty list to approve without one",
        ))
    }
}

/// The `presence.backend` setting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackendChoice {
    /// The platform's own.
    #[default]
    Auto,
    /// Windows Hello.
    WindowsHello,
    /// Touch ID (and the Mac's password fallback).
    TouchId,
    /// polkit, through `pkcheck`.
    Polkit,
    /// No check: approvals that require presence fail.
    None,
    /// Confirms every prompt at once. Exists only in builds with `test-doubles` (the `conformance` builds of
    /// the test rigs); a release build neither parses nor builds it.
    #[cfg(feature = "test-doubles")]
    TestAllow,
}

impl BackendChoice {
    /// The setting's spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            BackendChoice::Auto => "auto",
            BackendChoice::WindowsHello => "windows-hello",
            BackendChoice::TouchId => "touch-id",
            BackendChoice::Polkit => "polkit",
            BackendChoice::None => "none",
            #[cfg(feature = "test-doubles")]
            BackendChoice::TestAllow => "test-allow",
        }
    }

    /// The backend this choice names on this platform. A backend the build or the platform lacks is a
    /// [`DenyAll`] that says why, not an allow.
    pub fn build(self) -> Arc<dyn Presence> {
        match self {
            BackendChoice::Auto => {
                if cfg!(windows) {
                    BackendChoice::WindowsHello.build()
                } else if cfg!(target_os = "macos") {
                    BackendChoice::TouchId.build()
                } else if cfg!(target_os = "linux") {
                    BackendChoice::Polkit.build()
                } else {
                    <dyn Presence>::deny_all()
                }
            }
            BackendChoice::WindowsHello => {
                #[cfg(windows)]
                {
                    Arc::new(windows_hello::WindowsHello)
                }
                #[cfg(not(windows))]
                {
                    unsupported("windows-hello", "Windows")
                }
            }
            BackendChoice::TouchId => {
                #[cfg(target_os = "macos")]
                {
                    Arc::new(touch_id::TouchId)
                }
                #[cfg(not(target_os = "macos"))]
                {
                    unsupported("touch-id", "macOS")
                }
            }
            BackendChoice::Polkit => {
                if cfg!(target_os = "linux") {
                    Arc::new(polkit::Polkit::new())
                } else {
                    unsupported("polkit", "Linux")
                }
            }
            BackendChoice::None => <dyn Presence>::deny_all(),
            #[cfg(feature = "test-doubles")]
            BackendChoice::TestAllow => Arc::new(testing::AllowAll),
        }
    }
}

#[allow(dead_code)]
fn unsupported(backend: &str, only: &str) -> Arc<dyn Presence> {
    Arc::new(DenyAll::new(format!(
        "the presence backend {backend} is only available on {only}; set presence.backend for this device"
    )))
}

impl fmt::Display for BackendChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for BackendChoice {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        Ok(match s.trim() {
            "auto" => BackendChoice::Auto,
            "windows-hello" => BackendChoice::WindowsHello,
            "touch-id" => BackendChoice::TouchId,
            "polkit" => BackendChoice::Polkit,
            "none" => BackendChoice::None,
            #[cfg(feature = "test-doubles")]
            "test-allow" => BackendChoice::TestAllow,
            other => {
                return Err(format!(
                    "presence.backend: {other:?} is not auto, windows-hello, touch-id, polkit or none"
                ));
            }
        })
    }
}

/// The `presence.require` setting: for which action kinds a person must be present.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Require {
    /// Every action kind.
    #[default]
    All,
    /// These kinds (`effect.approve`); the empty list requires nothing.
    Kinds(Vec<String>),
}

impl Require {
    /// Whether a person must be present for `kind`.
    pub fn covers(&self, kind: &str) -> bool {
        match self {
            Require::All => true,
            Require::Kinds(k) => k.iter().any(|x| x == kind),
        }
    }
}

impl fmt::Display for Require {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Require::All => f.write_str("all"),
            Require::Kinds(k) => f.write_str(&k.join(",")),
        }
    }
}

impl FromStr for Require {
    type Err = String;

    /// `all`, or a comma-separated list of action kinds; the empty string is the empty list.
    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s == "all" {
            return Ok(Require::All);
        }
        let kinds: Vec<String> = s
            .split(',')
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(str::to_owned)
            .collect();
        if let Some(bad) = kinds.iter().find(|k| {
            !k.bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_' || b == b'-')
        }) {
            return Err(format!("presence.require: {bad:?} is not an action kind"));
        }
        Ok(Require::Kinds(kinds))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt() -> PresencePrompt {
        PresencePrompt {
            action: "git_push".into(),
            target: "origin".into(),
            plan_digest_short: "abc".into(),
            space: "s".into(),
            requester: "agent".into(),
            timeout: Duration::from_secs(1),
        }
    }

    #[test]
    fn require_parses_all_lists_and_the_empty_list() {
        assert_eq!("all".parse::<Require>().unwrap(), Require::All);
        let r: Require = "effect.approve, effect.cancel".parse().unwrap();
        assert!(
            r.covers("effect.approve") && r.covers("effect.cancel") && !r.covers("task.create")
        );
        let none: Require = "".parse().unwrap();
        assert!(!none.covers("effect.approve"));
        assert!("Effect Approve".parse::<Require>().is_err());
        assert!(Require::All.covers("anything"));
        assert_eq!(Require::default(), Require::All);
    }

    #[test]
    fn backend_choice_round_trips_and_none_denies() {
        for b in ["auto", "windows-hello", "touch-id", "polkit", "none"] {
            assert_eq!(b.parse::<BackendChoice>().unwrap().as_str(), b);
        }
        assert!("fido".parse::<BackendChoice>().is_err());
        let err = BackendChoice::None.build().confirm(&prompt()).unwrap_err();
        assert!(matches!(err, PresenceError::Unavailable(m) if m.contains("no presence check")));
    }

    #[test]
    fn the_prompt_text_names_the_action_and_digest_and_cannot_be_forged() {
        let mut p = prompt();
        p.target = "origin\nApprove nothing (plan 000000000000)".into();
        p.plan_digest_short = PresencePrompt::short("0123456789abcdef0123");
        let m = p.message();
        assert!(m.contains("git_push") && m.contains("0123456789ab"));
        assert!(!m.contains('\n'));
    }
}

//! The human-presence check before the node signs an approval (SR-106).
//!
//! A key press in a face proves nothing about who is at the device: terminal input is shared and injectable.
//! The node therefore asks the operating system to confirm a person (`han-presence`), with a prompt that names
//! the action and the short plan digest, and signs only on success. The setting is device-local: it lives in
//! `node.db` (`presence.require`, `presence.backend`), is read at each approval, and no command of the node
//! API can change it, so a face or an agent cannot switch the check off.
//!
//! The same check unlocks a face connection (`face.unlock`): any process of the person's OS user can open the
//! face pipe, so a connection starts locked for the commands that govern or steer, and the OS prompt, which
//! a process cannot answer for the person, opens them for a short window on that connection only.

use std::sync::Arc;
use std::time::Duration;

pub use han_presence::{BackendChoice, Require};
use han_presence::{Presence, PresenceError, PresencePrompt, PresenceProof};

use crate::api::{APPROVAL_NOT_SIGNED, NodeError};
use crate::db::NodeDb;
use crate::views::ApprovalSubject;

/// The `config` key listing the action kinds that need a person (`all` by default).
pub const REQUIRE_KEY: &str = "presence.require";
/// The `config` key naming the backend (`auto` by default).
pub const BACKEND_KEY: &str = "presence.backend";
/// How long a person has to answer.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
/// The action kind of an approval, as `presence.require` names it.
pub const EFFECT_APPROVE: &str = "effect.approve";
/// The action kind of unlocking a face connection to govern and steer, as `presence.require` names it.
pub const FACE_UNLOCK: &str = "face.unlock";
/// The `config` key holding how many seconds an unlock lasts (`600` by default).
pub const UNLOCK_SECS_KEY: &str = "presence.unlock_secs";
/// How long an unlock lasts when nothing says.
pub const DEFAULT_UNLOCK: Duration = Duration::from_secs(600);
/// The shortest and longest unlock a setting may name, in seconds.
pub const UNLOCK_SECS_RANGE: std::ops::RangeInclusive<u64> = 10..=3600;

/// The start of the message of a `denied` error when a person did not unlock a face connection.
pub const NOT_UNLOCKED: &str = "not unlocked: ";

/// A presence check handed to a node by whoever builds it (tests, an embedding app). It keeps
/// [`NodeConfig`](crate::NodeConfig) `Debug` and `Clone`.
#[derive(Clone)]
pub struct PresenceSlot(pub Arc<dyn Presence>);

impl std::fmt::Debug for PresenceSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PresenceSlot({})", self.0.name())
    }
}

/// The settings in `db`. A value that does not parse is an error, never a default: a typo must not turn the
/// check off.
pub fn settings(db: &NodeDb) -> Result<(Require, BackendChoice), NodeError> {
    let bad = |e: String| NodeError::invalid(format!("node.db: {e}"));
    let read = |key| {
        db.config(key)
            .map_err(|e| NodeError::unavailable(format!("node.db: {e}")))
    };
    let require = read(REQUIRE_KEY)?.map_or(Ok(Require::All), |v| v.parse().map_err(bad))?;
    let backend = read(BACKEND_KEY)?.map_or(Ok(BackendChoice::Auto), |v| v.parse().map_err(bad))?;
    Ok((require, backend))
}

/// How long one unlock of a face connection lasts. A value that does not parse, or is outside
/// [`UNLOCK_SECS_RANGE`], is an error, never a default.
pub fn unlock_window(db: &NodeDb) -> Result<Duration, NodeError> {
    let read = db
        .config(UNLOCK_SECS_KEY)
        .map_err(|e| NodeError::unavailable(format!("node.db: {e}")))?;
    let Some(v) = read else {
        return Ok(DEFAULT_UNLOCK);
    };
    v.trim()
        .parse::<u64>()
        .ok()
        .filter(|n| UNLOCK_SECS_RANGE.contains(n))
        .map(Duration::from_secs)
        .ok_or_else(|| {
            NodeError::invalid(format!(
                "node.db: {UNLOCK_SECS_KEY} must be {} to {} seconds",
                UNLOCK_SECS_RANGE.start(),
                UNLOCK_SECS_RANGE.end()
            ))
        })
}

/// Changes the device's presence settings (`han node presence`). While approvals or unlocks require a person,
/// a change needs one too, so a process that can only type into a terminal cannot switch the check off or
/// stretch the unlock window. `presence` is the check to ask; none means the backend the stored setting
/// names. `unlock_secs` is the unlock window in seconds ([`UNLOCK_SECS_RANGE`]). Blocks while the person
/// answers.
pub fn change(
    db: &NodeDb,
    require: Option<Require>,
    backend: Option<BackendChoice>,
    unlock_secs: Option<u64>,
    presence: Option<Arc<dyn Presence>>,
    timeout: Duration,
) -> Result<(Require, BackendChoice), NodeError> {
    let (old_require, old_backend) = settings(db)?;
    let old_window = unlock_window(db)?;
    let (new_require, new_backend) = (
        require.unwrap_or_else(|| old_require.clone()),
        backend.unwrap_or(old_backend),
    );
    let new_window = match unlock_secs {
        None => old_window,
        Some(n) if UNLOCK_SECS_RANGE.contains(&n) => Duration::from_secs(n),
        Some(_) => {
            return Err(NodeError::invalid(format!(
                "the unlock window is {} to {} seconds",
                UNLOCK_SECS_RANGE.start(),
                UNLOCK_SECS_RANGE.end()
            )));
        }
    };
    if (new_require.clone(), new_backend, new_window)
        == (old_require.clone(), old_backend, old_window)
    {
        return Ok((old_require, old_backend));
    }
    if old_require.covers(EFFECT_APPROVE) || old_require.covers(FACE_UNLOCK) {
        let check = presence.unwrap_or_else(|| old_backend.build());
        let prompt = PresencePrompt {
            action: "presence.set".into(),
            target: format!(
                "require={new_require} backend={new_backend} unlock_secs={}",
                new_window.as_secs()
            ),
            plan_digest_short: "-".into(),
            space: "this device".into(),
            requester: "you".into(),
            timeout,
        };
        let proof = check.confirm(&prompt).map_err(|e| refusal(&e))?;
        check_proof(&proof, &prompt)?;
    }
    let put = |k, v: String| {
        db.set_config(k, &v)
            .map_err(|e| NodeError::unavailable(format!("node.db: {e}")))
    };
    put(REQUIRE_KEY, new_require.to_string())?;
    put(BACKEND_KEY, new_backend.to_string())?;
    put(UNLOCK_SECS_KEY, new_window.as_secs().to_string())?;
    Ok((new_require, new_backend))
}

/// How a failed check reads to the person: the approval was not signed, and why.
pub fn refusal(e: &PresenceError) -> NodeError {
    match e {
        PresenceError::Cancelled => {
            NodeError::denied(format!("{APPROVAL_NOT_SIGNED}presence check cancelled"))
        }
        PresenceError::TimedOut => NodeError::denied(format!(
            "{APPROVAL_NOT_SIGNED}presence check timed out, nobody confirmed on this device"
        )),
        PresenceError::Denied(why) => NodeError::denied(format!(
            "{APPROVAL_NOT_SIGNED}presence check failed ({why})"
        )),
        PresenceError::Unavailable(why) => NodeError::denied(format!("{APPROVAL_NOT_SIGNED}{why}")),
    }
}

/// Whether the proof is for the plan the prompt named.
fn check_proof(proof: &PresenceProof, prompt: &PresencePrompt) -> Result<(), NodeError> {
    if proof.plan_digest_short() == prompt.plan_digest_short {
        Ok(())
    } else {
        Err(NodeError::denied(format!(
            "{APPROVAL_NOT_SIGNED}the presence check confirmed another plan"
        )))
    }
}

/// Asks for a person if `kind` needs one, and returns once they confirmed `subject`'s plan. Prompts are
/// one at a time: a second approval waits for the first.
pub(crate) async fn confirm(
    db: &NodeDb,
    injected: Option<&PresenceSlot>,
    timeout: Duration,
    gate: &tokio::sync::Mutex<()>,
    kind: &str,
    space: &str,
    subject: &ApprovalSubject,
) -> Result<(), NodeError> {
    let (require, backend) = settings(db)?;
    if !require.covers(kind) {
        return Ok(());
    }
    let presence: Arc<dyn Presence> = match injected {
        Some(p) => Arc::clone(&p.0),
        None => backend.build(),
    };
    let prompt = PresencePrompt {
        action: subject.action.clone(),
        target: subject.target.clone(),
        plan_digest_short: PresencePrompt::short(&subject.digest),
        space: space.to_owned(),
        requester: subject.requester.clone(),
        timeout,
    };
    let _one_at_a_time = gate.lock().await;
    let asked = prompt.clone();
    let outcome = tokio::task::spawn_blocking(move || presence.confirm(&asked))
        .await
        .map_err(|e| NodeError::unavailable(format!("{APPROVAL_NOT_SIGNED}presence check: {e}")))?;
    match outcome {
        Ok(proof) => check_proof(&proof, &prompt),
        Err(e) => Err(refusal(&e)),
    }
}

/// Whether a face connection must confirm a person before it governs or steers.
pub(crate) fn unlock_required(db: &NodeDb) -> Result<bool, NodeError> {
    Ok(settings(db)?.0.covers(FACE_UNLOCK))
}

/// Asks the person to unlock one face connection and returns how long the unlock lasts. `window` overrides the
/// stored window (tests, embedding apps). When no person is required it returns at once. Prompts are one at a
/// time, with approvals': a second one waits for the first.
pub(crate) async fn confirm_unlock(
    db: &NodeDb,
    injected: Option<&PresenceSlot>,
    timeout: Duration,
    window: Option<Duration>,
    gate: &tokio::sync::Mutex<()>,
) -> Result<Duration, NodeError> {
    let (require, backend) = settings(db)?;
    let window = match window {
        Some(w) => w,
        None => unlock_window(db)?,
    };
    if !require.covers(FACE_UNLOCK) {
        return Ok(window);
    }
    let presence: Arc<dyn Presence> = match injected {
        Some(p) => Arc::clone(&p.0),
        None => backend.build(),
    };
    let prompt = PresencePrompt {
        action: PresencePrompt::UNLOCK.into(),
        target: "govern and steer".into(),
        plan_digest_short: "-".into(),
        space: "this device".into(),
        requester: "a local connection".into(),
        timeout,
    };
    let _one_at_a_time = gate.lock().await;
    let asked = prompt.clone();
    let outcome = tokio::task::spawn_blocking(move || presence.confirm(&asked))
        .await
        .map_err(|e| NodeError::unavailable(format!("{NOT_UNLOCKED}presence check: {e}")))?;
    match outcome {
        Ok(proof) if proof.plan_digest_short() == prompt.plan_digest_short => Ok(window),
        Ok(_) => Err(NodeError::denied(format!(
            "{NOT_UNLOCKED}the presence check confirmed something else"
        ))),
        Err(e) => Err(NodeError::denied(format!(
            "{NOT_UNLOCKED}{}",
            refusal(&e)
                .to_string()
                .trim_start_matches("denied: ")
                .trim_start_matches(APPROVAL_NOT_SIGNED)
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use han_presence::testing::{AllowAll, Scripted};

    fn subject() -> ApprovalSubject {
        ApprovalSubject {
            digest: "0123456789abcdef0123456789abcdef".into(),
            open: true,
            action: "git_push".into(),
            target: "origin".into(),
            requester: "agent".into(),
        }
    }

    async fn run(db: &NodeDb, p: Arc<dyn Presence>) -> Result<(), NodeError> {
        confirm(
            db,
            Some(&PresenceSlot(p)),
            Duration::from_secs(1),
            &tokio::sync::Mutex::new(()),
            EFFECT_APPROVE,
            "space",
            &subject(),
        )
        .await
    }

    #[tokio::test]
    async fn no_proof_no_approval() {
        let db = NodeDb::in_memory().unwrap();
        let script = Scripted::new();
        // an empty script, a cancel and a timeout all refuse; none of them is a pass
        assert!(run(&db, script.clone()).await.is_err());
        script.fail(PresenceError::Cancelled);
        let e = run(&db, script.clone()).await.unwrap_err();
        assert!(
            e.to_string()
                .contains("approval not signed: presence check cancelled"),
            "{e}"
        );
        script.fail(PresenceError::TimedOut);
        assert!(run(&db, script.clone()).await.is_err());
        assert!(run(&db, <dyn Presence>::deny_all()).await.is_err());
        script.allow();
        run(&db, script.clone())
            .await
            .expect("a confirmed prompt passes");
        let seen = script.prompts();
        assert_eq!(seen.last().unwrap().plan_digest_short, "0123456789ab");
        assert_eq!(seen.last().unwrap().action, "git_push");
    }

    #[tokio::test]
    async fn a_proof_for_another_digest_is_refused() {
        let db = NodeDb::in_memory().unwrap();
        let script = Scripted::new();
        script.confirm_other_digest();
        let e = run(&db, script).await.unwrap_err();
        assert!(e.to_string().contains("another plan"), "{e}");
    }

    #[tokio::test]
    async fn the_setting_decides_what_needs_a_person() {
        let db = NodeDb::in_memory().unwrap();
        // not required: the backend is not even asked
        db.set_config(REQUIRE_KEY, "").unwrap();
        run(&db, <dyn Presence>::deny_all())
            .await
            .expect("nothing required");
        db.set_config(REQUIRE_KEY, "effect.cancel").unwrap();
        run(&db, <dyn Presence>::deny_all())
            .await
            .expect("only cancel required");
        db.set_config(REQUIRE_KEY, "effect.approve").unwrap();
        assert!(run(&db, <dyn Presence>::deny_all()).await.is_err());
        // a typo is an error, not a default
        db.set_config(REQUIRE_KEY, "Effect Approve").unwrap();
        assert!(matches!(
            run(&db, Arc::new(AllowAll)).await,
            Err(NodeError::Invalid { .. })
        ));
        db.set_config(REQUIRE_KEY, "all").unwrap();
        db.set_config(BACKEND_KEY, "fido").unwrap();
        assert!(run(&db, Arc::new(AllowAll)).await.is_err());
    }

    #[test]
    fn weakening_the_check_needs_a_person_while_it_is_required() {
        let db = NodeDb::in_memory().unwrap();
        let none = || Some(Require::Kinds(Vec::new()));
        let t = Duration::from_secs(1);
        let script = Scripted::new();
        // required (the default): an empty script, a cancel and a missing backend all leave it as it is
        assert!(change(&db, none(), None, None, Some(script.clone()), t).is_err());
        script.fail(PresenceError::Cancelled);
        assert!(change(&db, none(), None, None, Some(script.clone()), t).is_err());
        assert!(change(&db, none(), None, None, Some(<dyn Presence>::deny_all()), t).is_err());
        assert_eq!(settings(&db).unwrap().0, Require::All);
        // a person confirms: it changes
        script.allow();
        let (r, _) = change(&db, none(), None, None, Some(script.clone()), t).unwrap();
        assert_eq!(r, Require::Kinds(Vec::new()));
        // no longer required: raising it again needs nobody
        let (r, b) = change(
            &db,
            Some(Require::All),
            Some(BackendChoice::None),
            None,
            None,
            t,
        )
        .unwrap();
        assert_eq!((r, b), (Require::All, BackendChoice::None));
    }

    #[test]
    fn only_a_requirement_that_names_the_unlock_locks_a_connection() {
        let db = NodeDb::in_memory().unwrap();
        assert!(unlock_required(&db).unwrap(), "required by default");
        db.set_config(REQUIRE_KEY, "effect.approve,effect.cancel")
            .unwrap();
        assert!(!unlock_required(&db).unwrap());
        db.set_config(REQUIRE_KEY, "face.unlock").unwrap();
        assert!(unlock_required(&db).unwrap());
        db.set_config(REQUIRE_KEY, "").unwrap();
        assert!(!unlock_required(&db).unwrap());
    }

    #[tokio::test]
    async fn backend_none_says_the_device_has_no_check() {
        let db = NodeDb::in_memory().unwrap();
        db.set_config(BACKEND_KEY, "none").unwrap();
        let e = confirm(
            &db,
            None,
            Duration::from_secs(1),
            &tokio::sync::Mutex::new(()),
            EFFECT_APPROVE,
            "space",
            &subject(),
        )
        .await
        .unwrap_err();
        assert!(
            e.to_string().contains("this device has no presence check"),
            "{e}"
        );
    }
}

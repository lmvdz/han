//! The human-presence check before the node signs an approval (SR-106).
//!
//! A key press in a face proves nothing about who is at the device: terminal input is shared and injectable.
//! The node therefore asks the operating system to confirm a person (`han-presence`), with a prompt that names
//! the action and the short plan digest, and signs only on success. The setting is device-local: it lives in
//! `node.db` (`presence.require`, `presence.backend`, `presence.unlock_secs`). The node reads it once, when it
//! starts, and ignores later edits of the file; no command of the node API can change it, so a face or an
//! agent cannot switch the check off. It is sealed with a key that `node.db` does not hold (the node's key
//! store: the OS keychain unless `--file-keys`), so an edit made while the node is down fails the seal and
//! the node starts with the strictest settings. [`change`] (`han node presence`) is the one writer, and it
//! needs a person while any check is required.
//!
//! The same check unlocks a face connection (`face.unlock`): any process of the person's OS user can open the
//! face pipe, so a connection starts locked for the commands that govern or steer, and the OS prompt, which
//! a process cannot answer for the person, opens them for a short window on that connection only.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use han_presence::{BackendChoice, Require};
use han_presence::{Presence, PresenceError, PresencePrompt, PresenceProof};

use crate::api::{APPROVAL_NOT_SIGNED, NodeError};
use crate::db::NodeDb;
use crate::keys::KeyStore;
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

/// The longest an unlock prompt waits for the person.
pub const UNLOCK_PROMPT_TIMEOUT: Duration = Duration::from_secs(30);
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

/// The presence settings of a node, as it holds them while it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// For which action kinds a person must be present.
    pub require: Require,
    /// The backend that checks.
    pub backend: BackendChoice,
    /// How long one unlock of a face connection lasts.
    pub unlock: Duration,
}

impl Default for Settings {
    /// The strictest: a person for everything, the platform's own backend.
    fn default() -> Self {
        Settings {
            require: Require::All,
            backend: BackendChoice::Auto,
            unlock: DEFAULT_UNLOCK,
        }
    }
}

/// The settings in `db`, parsed. Anything that does not parse is an error, never a default.
pub fn read(db: &NodeDb) -> Result<Settings, NodeError> {
    let (require, backend) = settings(db)?;
    Ok(Settings {
        require,
        backend,
        unlock: unlock_window(db)?,
    })
}

/// Where root records which nodes are headless (`han runner setup` writes it).
pub const HEADLESS_DIR: &str = "/etc/han/headless.d";

/// Whether the node with the data folder `name` (see [`seal_name`]) is headless: root made a marker file of
/// that name in `dir`. A service node has nobody at it to answer a prompt, so the operating system's boundary,
/// who can run as the service user, is the authority there; the marker is what says so. The marker, its folder
/// and that folder's parent must belong to `owner` (root) and be writable by no one else, so a process of the
/// service user cannot make a node headless, or make it a person's again; only root can. Not available off
/// Unix: a node there is a person's.
pub fn headless_marker(dir: &std::path::Path, name: &str, owner: u32) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let ok = |p: &std::path::Path| {
            std::fs::metadata(p).is_ok_and(|m| m.uid() == owner && m.mode() & 0o022 == 0)
        };
        let marker = dir.join(name);
        ok(dir) && dir.parent().is_none_or(ok) && ok(&marker)
    }
    #[cfg(not(unix))]
    {
        let _ = (dir, name, owner);
        false
    }
}

/// The settings of a headless node: no unlock (nobody is there to give it), and no approval at all, because a
/// service node never approves: the check is a backend that has none, so an approval is refused.
pub fn headless_settings() -> Settings {
    Settings {
        require: Require::Kinds(vec![EFFECT_APPROVE.to_owned()]),
        backend: BackendChoice::None,
        unlock: DEFAULT_UNLOCK,
    }
}

/// The `config` row holding the seal of the settings.
pub const SEAL_KEY: &str = "presence.seal";

/// The name of the seal key in the key store for the node whose data is in `data`.
pub fn seal_name(data: &std::path::Path) -> String {
    let canon = std::fs::canonicalize(data).unwrap_or_else(|_| data.to_path_buf());
    let h = han_core::ids::sha256_hex(canon.to_string_lossy().as_bytes());
    format!("presence-seal-{}", &h[..16])
}

/// HMAC-SHA256 of `msg` under `key`, in hex.
fn hmac_hex(key: &[u8; 32], msg: &[u8]) -> String {
    let unhex = |h: String| -> Vec<u8> {
        (0..h.len() / 2)
            .filter_map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).ok())
            .collect()
    };
    let (mut ipad, mut opad) = ([0x36u8; 64], [0x5cu8; 64]);
    for (i, k) in key.iter().enumerate() {
        ipad[i] ^= k;
        opad[i] ^= k;
    }
    let inner = unhex(han_core::ids::sha256_hex(&[&ipad[..], msg].concat()));
    han_core::ids::sha256_hex(&[&opad[..], &inner[..]].concat())
}

/// What the seal covers: the three raw values, `None` for a missing one.
fn sealed_text(db: &NodeDb) -> Result<String, NodeError> {
    let get = |k| {
        db.config(k)
            .map_err(|e| NodeError::unavailable(format!("node.db: {e}")))
    };
    Ok(format!(
        "{:?}\n{:?}\n{:?}",
        get(REQUIRE_KEY)?,
        get(BACKEND_KEY)?,
        get(UNLOCK_SECS_KEY)?
    ))
}

fn write_seal(db: &NodeDb, key: &[u8; 32]) -> Result<(), NodeError> {
    let tag = hmac_hex(key, sealed_text(db)?.as_bytes());
    db.set_config(SEAL_KEY, &tag)
        .map_err(|e| NodeError::unavailable(format!("node.db: {e}")))
}

/// The settings a node runs with. Without `seal` (an in-memory db, or no way to hold a key) it is what the
/// db says. With one, the db must carry the seal the key makes of its values: the first start with a new key
/// trusts and seals what is there; a later mismatch (the db was edited, or the seal removed) falls back to
/// the strictest settings. A value that does not parse also falls back to them. It logs why, and it never
/// fails: a node with no usable settings is one that asks for a person for everything.
pub fn load(db: &NodeDb, seal: Option<(&dyn KeyStore, &str)>) -> Settings {
    let strict = |why: String| {
        tracing::warn!("presence settings: {why}; using the strictest");
        Settings::default()
    };
    let parsed = || read(db).unwrap_or_else(|e| strict(e.to_string()));
    let Some((keys, name)) = seal else {
        return parsed();
    };
    match keys.load(name) {
        Err(e) => {
            tracing::warn!("presence settings are not sealed: {e}");
            parsed()
        }
        Ok(None) => {
            let sealed = keys
                .load_or_create(name)
                .map_err(|e| NodeError::unavailable(e.to_string()))
                .and_then(|k| write_seal(db, &k));
            if let Err(e) = sealed {
                tracing::warn!("presence settings are not sealed: {e}");
            }
            parsed()
        }
        Ok(Some(k)) => {
            let stored = db.config(SEAL_KEY).ok().flatten();
            let ok = sealed_text(db)
                .ok()
                .is_some_and(|t| stored.as_deref() == Some(hmac_hex(&k, t.as_bytes()).as_str()));
            if ok {
                parsed()
            } else {
                strict(
                    "the stored settings fail their seal (edited outside `han node presence`?)"
                        .into(),
                )
            }
        }
    }
}

/// Spacing of prompts after refusals: each refused prompt doubles the wait before the next, from two seconds
/// to five minutes, and an allowed one resets it. A process that keeps asking cannot keep the person's screen
/// busy.
#[derive(Debug, Default)]
pub struct Backoff {
    refused: u32,
    until: Option<Instant>,
}

impl Backoff {
    /// How long until another prompt may be shown; none when it may be shown now.
    pub fn wait(&self) -> Option<Duration> {
        self.until
            .and_then(|t| t.checked_duration_since(Instant::now()))
            .filter(|d| !d.is_zero())
    }

    /// A prompt was refused.
    pub fn refused(&mut self) {
        self.refused = self.refused.saturating_add(1);
        let secs = 2u64.saturating_pow(self.refused.min(16)).min(300);
        self.until = Some(Instant::now() + Duration::from_secs(secs));
    }

    /// A prompt was allowed.
    pub fn allowed(&mut self) {
        *self = Backoff::default();
    }
}

/// The refusal of an unlock that comes too soon after refused ones: `unavailable`, with the time to wait
/// (`retry_after_ms`), so a caller waits exactly that long instead of guessing.
pub fn spaced_out(wait: Duration) -> NodeError {
    NodeError::Unavailable {
        message: format!(
            "{NOT_UNLOCKED}an unlock was refused a moment ago; try again in {} s",
            wait.as_secs() + 1
        ),
        retry_after_ms: Some(
            u64::try_from(wait.as_millis())
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        ),
        pending: None,
    }
}

/// The node's one place where the person is asked: one prompt at a time, approvals before unlocks, and a
/// spacing after refused unlocks.
pub(crate) struct Prompter {
    gate: Arc<tokio::sync::Mutex<()>>,
    approvals: AtomicUsize,
    unlocks: Mutex<Backoff>,
}

impl Prompter {
    pub(crate) fn new() -> Self {
        Prompter {
            gate: Arc::new(tokio::sync::Mutex::new(())),
            approvals: AtomicUsize::new(0),
            unlocks: Mutex::new(Backoff::default()),
        }
    }
}

/// Counts an approval that waits for the prompt, so an unlock lets it go first.
struct Waiting<'a>(&'a AtomicUsize);

impl<'a> Waiting<'a> {
    fn new(n: &'a AtomicUsize) -> Self {
        n.fetch_add(1, Ordering::SeqCst);
        Waiting(n)
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Changes the device's presence settings (`han node presence`). While approvals or unlocks require a person,
/// a change needs one too, so a process that can only type into a terminal cannot switch the check off or
/// stretch the unlock window. `presence` is the check to ask; none means the backend the stored setting
/// names. `unlock_secs` is the unlock window in seconds ([`UNLOCK_SECS_RANGE`]). Blocks while the person
/// answers.
pub fn change(
    db: &NodeDb,
    seal: Option<(&dyn KeyStore, &str)>,
    require: Option<Require>,
    backend: Option<BackendChoice>,
    unlock_secs: Option<u64>,
    presence: Option<Arc<dyn Presence>>,
    timeout: Duration,
) -> Result<(Require, BackendChoice), NodeError> {
    let Settings {
        require: old_require,
        backend: old_backend,
        unlock: old_window,
    } = load(db, seal);
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
    // Any change needs a person while any check is required, so a requirement that lists only cancels cannot
    // be dropped, or `face.unlock` left out of it, by a process that cannot be at the device.
    if old_require != Require::Kinds(Vec::new()) {
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
    if let Some((keys, name)) = seal {
        let k = keys
            .load_or_create(name)
            .map_err(|e| NodeError::unavailable(e.to_string()))?;
        write_seal(db, &k)?;
    }
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

/// Shows the prompt while holding the gate: the guard goes into the blocking task that shows
/// the prompt, so a caller that gives up (its connection closed) does not free the gate while the operating
/// system's prompt is still on the screen.
async fn show(
    held: tokio::sync::OwnedMutexGuard<()>,
    presence: Arc<dyn Presence>,
    prompt: PresencePrompt,
) -> Result<Result<PresenceProof, PresenceError>, tokio::task::JoinError> {
    tokio::task::spawn_blocking(move || {
        let outcome = presence.confirm(&prompt);
        drop(held);
        outcome
    })
    .await
}

impl Prompter {
    /// Asks for a person if `kind` needs one, and returns once they confirmed `subject`'s plan.
    pub(crate) async fn confirm(
        &self,
        settings: &Settings,
        injected: Option<&PresenceSlot>,
        timeout: Duration,
        kind: &str,
        space: &str,
        subject: &ApprovalSubject,
    ) -> Result<(), NodeError> {
        if !settings.require.covers(kind) {
            return Ok(());
        }
        let presence: Arc<dyn Presence> = match injected {
            Some(p) => Arc::clone(&p.0),
            None => settings.backend.build(),
        };
        let prompt = PresencePrompt {
            action: subject.action.clone(),
            target: subject.target.clone(),
            plan_digest_short: PresencePrompt::short(&subject.digest),
            space: space.to_owned(),
            requester: subject.requester.clone(),
            timeout,
        };
        let _waiting = Waiting::new(&self.approvals);
        let held = Arc::clone(&self.gate).lock_owned().await;
        let outcome = show(held, presence, prompt.clone()).await.map_err(|e| {
            NodeError::unavailable(format!("{APPROVAL_NOT_SIGNED}presence check: {e}"))
        })?;
        match outcome {
            Ok(proof) => check_proof(&proof, &prompt),
            Err(e) => Err(refusal(&e)),
        }
    }

    /// Asks the person to unlock one face connection (`client` names what asks, as far as the system says)
    /// and returns how long the unlock lasts. When no person is required it returns at once. An unlock waits
    /// for approvals that are waiting, and after refused ones it waits out the spacing.
    pub(crate) async fn confirm_unlock(
        &self,
        settings: &Settings,
        injected: Option<&PresenceSlot>,
        timeout: Duration,
        window: Option<Duration>,
        client: Option<String>,
    ) -> Result<Duration, NodeError> {
        let window = window.unwrap_or(settings.unlock);
        if !settings.require.covers(FACE_UNLOCK) {
            return Ok(window);
        }
        let presence: Arc<dyn Presence> = match injected {
            Some(p) => Arc::clone(&p.0),
            None => settings.backend.build(),
        };
        let prompt = PresencePrompt {
            action: PresencePrompt::UNLOCK.into(),
            target: "govern and steer".into(),
            plan_digest_short: "-".into(),
            space: "this device".into(),
            requester: client.unwrap_or_else(|| "a local connection".into()),
            // an unlock never holds the screen as long as an approval may
            timeout: timeout.min(UNLOCK_PROMPT_TIMEOUT),
        };
        // Approvals first: an unlock takes the gate only when no approval waits for it, and gives it back if
        // one turns up while it queued. Refused unlocks space the next one out, also for those that queued.
        let held = loop {
            while self.approvals.load(Ordering::SeqCst) > 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let held = Arc::clone(&self.gate).lock_owned().await;
            if self.approvals.load(Ordering::SeqCst) > 0 {
                continue;
            }
            let wait = self
                .unlocks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .wait();
            if let Some(wait) = wait {
                return Err(spaced_out(wait));
            }
            break held;
        };
        let outcome = show(held, presence, prompt.clone())
            .await
            .map_err(|e| NodeError::unavailable(format!("{NOT_UNLOCKED}presence check: {e}")))?;
        let result = match outcome {
            Ok(proof) if proof.plan_digest_short() == prompt.plan_digest_short => Ok(window),
            Ok(_) => Err("the presence check confirmed something else".to_owned()),
            Err(e) => Err(refusal(&e)
                .to_string()
                .trim_start_matches("denied: ")
                .trim_start_matches(APPROVAL_NOT_SIGNED)
                .to_owned()),
        };
        let mut b = self.unlocks.lock().unwrap_or_else(|e| e.into_inner());
        match result {
            Ok(w) => {
                b.allowed();
                Ok(w)
            }
            Err(why) => {
                b.refused();
                Err(NodeError::denied(format!("{NOT_UNLOCKED}{why}")))
            }
        }
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
        Prompter::new()
            .confirm(
                &read(db)?,
                Some(&PresenceSlot(p)),
                Duration::from_secs(1),
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
        assert!(change(&db, None, none(), None, None, Some(script.clone()), t).is_err());
        script.fail(PresenceError::Cancelled);
        assert!(change(&db, None, none(), None, None, Some(script.clone()), t).is_err());
        assert!(
            change(
                &db,
                None,
                none(),
                None,
                None,
                Some(<dyn Presence>::deny_all()),
                t
            )
            .is_err()
        );
        assert_eq!(settings(&db).unwrap().0, Require::All);
        // a person confirms: it changes
        script.allow();
        let (r, _) = change(&db, None, none(), None, None, Some(script.clone()), t).unwrap();
        assert_eq!(r, Require::Kinds(Vec::new()));
        // no longer required: raising it again needs nobody
        let (r, b) = change(
            &db,
            None,
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
        let locks = |db: &NodeDb| read(db).unwrap().require.covers(FACE_UNLOCK);
        assert!(locks(&db), "required by default");
        db.set_config(REQUIRE_KEY, "effect.approve,effect.cancel")
            .unwrap();
        assert!(!locks(&db));
        db.set_config(REQUIRE_KEY, "face.unlock").unwrap();
        assert!(locks(&db));
        db.set_config(REQUIRE_KEY, "").unwrap();
        assert!(!locks(&db));
    }

    fn keys(tag: &str) -> crate::keys::FileKeyStore {
        let dir = std::env::temp_dir().join(format!("han-seal-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::keys::FileKeyStore::new(dir)
    }

    #[test]
    fn an_edit_of_the_db_does_not_weaken_a_sealed_node() {
        let (db, keys) = (NodeDb::in_memory().unwrap(), keys("edit"));
        let seal = Some((&keys as &dyn KeyStore, "presence-seal-test"));
        // the first start trusts and seals what is stored
        db.set_config(REQUIRE_KEY, "effect.cancel").unwrap();
        assert_eq!(
            load(&db, seal).require,
            Require::Kinds(vec!["effect.cancel".into()])
        );
        // an edit while the node is down fails the seal: the strictest settings
        db.set_config(REQUIRE_KEY, "").unwrap();
        assert_eq!(load(&db, seal), Settings::default());
        db.set_config(REQUIRE_KEY, "effect.cancel").unwrap();
        assert_eq!(
            load(&db, seal).require,
            Require::Kinds(vec!["effect.cancel".into()])
        );
        // so does removing the seal, or a longer window
        db.set_config(SEAL_KEY, "").unwrap();
        assert_eq!(load(&db, seal), Settings::default());
        // the one writer reseals
        let t = Duration::from_secs(1);
        let script = Scripted::new();
        script.allow();
        // (the old settings were tampered, so the strictest hold and a person is asked: the script allows)
        change(
            &db,
            seal,
            Some(Require::All),
            None,
            Some(60),
            Some(script),
            t,
        )
        .unwrap();
        let now = load(&db, seal);
        assert_eq!(
            (now.require, now.unlock),
            (Require::All, Duration::from_secs(60))
        );
    }

    #[test]
    fn dropping_any_requirement_needs_a_person_even_from_a_cancel_only_list() {
        let db = NodeDb::in_memory().unwrap();
        db.set_config(REQUIRE_KEY, "effect.cancel").unwrap();
        let t = Duration::from_secs(1);
        let none = || Some(Require::Kinds(Vec::new()));
        let script = Scripted::new();
        assert!(change(&db, None, none(), None, None, Some(script.clone()), t).is_err());
        assert_eq!(
            read(&db).unwrap().require,
            Require::Kinds(vec!["effect.cancel".into()])
        );
        script.allow();
        change(&db, None, none(), None, None, Some(script), t).unwrap();
    }

    #[test]
    fn refused_prompts_back_off_and_cap() {
        let mut b = Backoff::default();
        assert!(b.wait().is_none());
        b.refused();
        let first = b.wait().expect("spaced");
        assert!(first <= Duration::from_secs(2));
        for _ in 0..20 {
            b.refused();
        }
        assert!(b.wait().unwrap() <= Duration::from_secs(300));
        b.allowed();
        assert!(b.wait().is_none());
    }

    /// A check that takes a while and notes when it was asked and what for.
    struct Slow(Mutex<Vec<String>>);

    impl Presence for Slow {
        fn name(&self) -> &'static str {
            "test-slow"
        }

        fn confirm(&self, p: &PresencePrompt) -> Result<PresenceProof, PresenceError> {
            self.0.lock().unwrap().push(p.action.clone());
            std::thread::sleep(Duration::from_millis(150));
            if p.action == PresencePrompt::UNLOCK {
                Err(PresenceError::Cancelled)
            } else {
                Ok(PresenceProof::new("test-slow", &p.plan_digest_short))
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn unlock_prompts_do_not_starve_an_approval_and_a_refusal_spaces_the_next() {
        let slow = Arc::new(Slow(Mutex::new(Vec::new())));
        let slot = PresenceSlot(slow.clone());
        let prompter = Arc::new(Prompter::new());
        let settings = Settings::default();
        let t = Duration::from_secs(5);
        // ten unlocks ask at once; an approval arrives while the first one's prompt is showing
        let mut unlocks = Vec::new();
        for _ in 0..10 {
            let (p, s, slot) = (Arc::clone(&prompter), settings.clone(), slot.clone());
            unlocks.push(tokio::spawn(async move {
                p.confirm_unlock(&s, Some(&slot), t, None, None).await
            }));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        prompter
            .confirm(
                &settings,
                Some(&slot),
                t,
                EFFECT_APPROVE,
                "space",
                &subject(),
            )
            .await
            .expect("the approval is shown and passes");
        let mut refused = 0;
        for u in unlocks {
            assert!(u.await.unwrap().is_err());
            refused += 1;
        }
        assert_eq!(refused, 10);
        let seen = slow.0.lock().unwrap().clone();
        // the first unlock was already on the screen; the approval is the very next prompt, and the other nine
        // unlocks never reached the screen: the first refusal spaced them out
        assert_eq!(seen, ["unlock", "git_push"], "{seen:?}");
        // and after that a later unlock is spaced out as well
        let e = prompter
            .confirm_unlock(&settings, Some(&slot), t, None, None)
            .await
            .unwrap_err();
        assert!(e.to_string().contains("refused a moment ago"), "{e}");
    }

    #[tokio::test]
    async fn backend_none_says_the_device_has_no_check() {
        let db = NodeDb::in_memory().unwrap();
        db.set_config(BACKEND_KEY, "none").unwrap();
        let e = Prompter::new()
            .confirm(
                &read(&db).unwrap(),
                None,
                Duration::from_secs(1),
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

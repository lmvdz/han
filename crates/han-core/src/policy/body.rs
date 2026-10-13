//! The entry as Policy reads it: the header fields it needs and a typed body per kind (R-09).

use serde::{Deserialize, Serialize};

use super::doc::{NewRole, PolicyDoc, ProfileDoc, SteerMode};
use crate::error::Error;
use crate::grammar as g;
use crate::json::{self, MAX_SAFE_INT, Value};
use crate::types::{ActorId, Budget, Entry, EntryId, KeyId, Seq, SpaceId, Time};

/// `space.create`: the space it forks.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkOf {
    /// The forked space.
    pub space: SpaceId,
    /// The seq of it the fork follows.
    pub through: Seq,
}

/// Body of `space.create`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SpaceCreate {
    /// The first policy.
    pub policy: PolicyDoc,
    /// Present on a fork.
    pub fork_of: Option<ForkOf>,
    /// Present, and required, in a profile space.
    pub profile: Option<ProfileDoc>,
}

/// Body of `member.set`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberSet {
    /// The actor whose role changes.
    pub actor: ActorId,
    /// Its new role.
    pub role: NewRole,
}

/// Body of `policy.set`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicySet {
    /// The whole new policy.
    pub policy: PolicyDoc,
}

/// Body of `key.add`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KeyAdd {
    /// The new key's id.
    pub key_id: KeyId,
    /// Its public key.
    pub public_key: String,
}

/// Body of `key.revoke`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KeyRevoke {
    /// The key to revoke.
    pub key_id: KeyId,
}

/// Body of `message`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    /// Addressees.
    pub to: Option<Vec<ActorId>>,
}

/// Body of `task`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskBody {
    /// The assignees, at least one.
    pub to: Vec<ActorId>,
    /// Budget reserved for the task.
    pub budget: Option<Budget>,
    /// The parent task, for a delegation.
    pub parent: Option<EntryId>,
    /// A deadline; informational.
    pub deadline: Option<Time>,
    /// An opaque external reference.
    pub external: Option<String>,
}

/// The state of a task: what a `task.update` reports, and what a task holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    /// Created and not yet taken up (a task's initial state; never in a body).
    Pending,
    /// The assignee accepts.
    Accepted,
    /// Work is under way.
    Progress,
    /// Finished.
    Done,
    /// Finished unsuccessfully.
    Failed,
    /// The assignee refuses.
    Declined,
    /// Called off.
    Cancelled,
}

impl TaskState {
    /// Done, failed, declined or cancelled.
    pub fn is_final(self) -> bool {
        matches!(
            self,
            TaskState::Done | TaskState::Failed | TaskState::Declined | TaskState::Cancelled
        )
    }
}

/// Body of `task.update`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskUpdate {
    /// The task.
    pub task: EntryId,
    /// Its new state.
    pub state: TaskState,
}

/// Body of `task.cancel`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCancel {
    /// The task.
    pub task: EntryId,
}

/// Body of `effect.propose`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Propose {
    /// Who may claim and run it.
    pub executor: ActorId,
    /// The action name.
    pub action: String,
    /// `sha256(JCS(plan))`.
    pub plan_digest: String,
    /// The cost, in the space's unit when it has a budget.
    pub cost: Option<Budget>,
    /// Whether a failed attempt may be retried.
    pub repeatable: bool,
    /// The task it is charged to.
    pub task: Option<EntryId>,
    /// No claim at or after this time.
    pub expires_at: Option<Time>,
}

/// Body of `effect.approve`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Approve {
    /// The proposal.
    pub proposal: EntryId,
    /// Must equal the proposal's `planDigest`.
    pub plan_digest: String,
    /// The approval lapses at this time.
    pub expires_at: Option<Time>,
}

/// Body of `effect.claim`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    /// The proposal.
    pub proposal: EntryId,
    /// One more than the number of earlier claims.
    pub attempt: u64,
    /// The claiming key id.
    pub instance: KeyId,
    /// The `launchWindow` in force.
    pub window: u64,
}

/// Body of `effect.cancel`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectCancel {
    /// The proposal.
    pub proposal: EntryId,
}

/// How an attempt ended, as the executor reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectResult {
    /// It happened.
    Succeeded,
    /// It did not.
    Failed,
    /// It was called off.
    Cancelled,
    /// The executor cannot tell.
    Unknown,
}

/// Whether the executor started the action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Started {
    /// It did.
    Yes,
    /// It did not.
    No,
    /// The executor cannot tell.
    Unknown,
}

/// Body of `effect.outcome`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    /// The claim.
    pub claim: EntryId,
    /// The result.
    pub result: EffectResult,
    /// Whether the action started.
    pub started: Started,
    /// What it cost, with `started: yes` only.
    pub cost: Option<Budget>,
}

/// The result an `effect.resolve` settles an unknown outcome with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResolveResult {
    /// It happened.
    Succeeded,
    /// It did not.
    Failed,
}

/// Body of `effect.resolve`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resolve {
    /// The proposal.
    pub proposal: EntryId,
    /// The settled result.
    pub result: ResolveResult,
    /// What it cost, on success.
    pub cost: Option<Budget>,
}

/// Body of `checkpoint`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    /// The last seq the checkpoint covers.
    pub through: Seq,
}

/// Body of `redact`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Redact {
    /// The entry whose content goes.
    pub target: EntryId,
}

/// Body of `session.open`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionOpen {
    /// The task the session works.
    pub task: Option<EntryId>,
    /// How outside input is treated.
    pub steer_mode: SteerMode,
}

/// Body of `session.close` and `session.input`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRef {
    /// The session (its `session.open` entry).
    pub session: EntryId,
}

/// Body of `agent.unguard`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentUnguard {
    /// The agent to unguard.
    pub agent: ActorId,
}

/// The target's acceptance of a `space.move`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MoveAccept {
    /// The target's key id.
    pub key_id: KeyId,
    /// Its signature.
    pub sig: String,
}

/// Body of `space.move`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpaceMove {
    /// The new home.
    pub to: String,
    /// The new home's acceptance.
    pub accept: MoveAccept,
}

/// An entry's body, typed by kind. Extension kinds keep their opaque body.
#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    /// `space.create`
    SpaceCreate(SpaceCreate),
    /// `member.set`
    MemberSet(MemberSet),
    /// `policy.set`
    PolicySet(PolicySet),
    /// `key.add`
    KeyAdd(KeyAdd),
    /// `key.revoke`
    KeyRevoke(KeyRevoke),
    /// `message`
    Message(Message),
    /// `task`
    Task(TaskBody),
    /// `task.update`
    TaskUpdate(TaskUpdate),
    /// `task.cancel`
    TaskCancel(TaskCancel),
    /// `effect.propose`
    Propose(Propose),
    /// `effect.approve`
    Approve(Approve),
    /// `effect.claim`
    Claim(Claim),
    /// `effect.cancel`
    EffectCancel(EffectCancel),
    /// `effect.outcome`
    Outcome(Outcome),
    /// `effect.resolve`
    Resolve(Resolve),
    /// `checkpoint`
    Checkpoint(Checkpoint),
    /// `redact`
    Redact(Redact),
    /// `session.open`
    SessionOpen(SessionOpen),
    /// `session.close`
    SessionClose(SessionRef),
    /// `session.input`
    SessionInput(SessionRef),
    /// `agent.unguard`
    AgentUnguard(AgentUnguard),
    /// `space.move`
    SpaceMove(SpaceMove),
    /// An extension kind: opaque to Policy, plain Json (integers in `[0, 2^53)`, 32 levels).
    Extension(Value),
}

fn typed<T: serde::de::DeserializeOwned>(body: &Value) -> Result<T, Error> {
    Ok(json::from_value(body)?)
}

impl Body {
    /// Checks `body` against the closed schema of `kind` (the check [`Entry::validate`] makes)
    /// and types it. An extension kind's body must be plain Json.
    pub fn parse(kind: &str, body: &Value) -> Result<Body, Error> {
        if !matches!(body, Value::Object(_)) {
            return Err(Error::invalid("body not object"));
        }
        let Some(checked) = crate::validate::check_core_body(kind, body) else {
            if !g::is_extension_kind(kind) {
                return Err(Error::invalid("unknown kind"));
            }
            if !g::is_opaque_json(body) {
                return Err(Error::invalid(
                    "extension body: only non-negative integers, no floats",
                ));
            }
            return Ok(Body::Extension(body.clone()));
        };
        checked?;
        Ok(match kind {
            "space.create" => Body::SpaceCreate(typed(body)?),
            "member.set" => Body::MemberSet(typed(body)?),
            "policy.set" => Body::PolicySet(typed(body)?),
            "key.add" => Body::KeyAdd(typed(body)?),
            "key.revoke" => Body::KeyRevoke(typed(body)?),
            "message" => Body::Message(typed(body)?),
            "task" => Body::Task(typed(body)?),
            "task.update" => Body::TaskUpdate(typed(body)?),
            "task.cancel" => Body::TaskCancel(typed(body)?),
            "effect.propose" => Body::Propose(typed(body)?),
            "effect.approve" => Body::Approve(typed(body)?),
            "effect.claim" => Body::Claim(typed(body)?),
            "effect.cancel" => Body::EffectCancel(typed(body)?),
            "effect.outcome" => Body::Outcome(typed(body)?),
            "effect.resolve" => Body::Resolve(typed(body)?),
            "checkpoint" => Body::Checkpoint(typed(body)?),
            "redact" => Body::Redact(typed(body)?),
            "session.open" => Body::SessionOpen(typed(body)?),
            "session.close" => Body::SessionClose(typed(body)?),
            "session.input" => Body::SessionInput(typed(body)?),
            "agent.unguard" => Body::AgentUnguard(typed(body)?),
            "space.move" => Body::SpaceMove(typed(body)?),
            _ => return Err(Error::invalid("kind")),
        })
    }
}

/// The part of an entry Policy reads: the space, `kind`, `author`, `keyId`, `body`, `refs` and
/// `seen`. The signature, the content and the exact wire header are the Log's and Identity's
/// checks.
#[derive(Clone, Debug, PartialEq)]
pub struct PolicyEntry {
    /// The space.
    pub space: SpaceId,
    /// The kind, such as `message` or `effect.propose`.
    pub kind: String,
    /// The signing actor.
    pub author: ActorId,
    /// The signing key.
    pub key_id: KeyId,
    /// The typed body.
    pub body: Body,
    /// Entries of the same space this one refers to.
    pub refs: Vec<EntryId>,
    /// The head the author had seen.
    pub seen: Seq,
}

const HEADER_FIELDS: [&str; 13] = [
    "v",
    "space",
    "kind",
    "author",
    "keyId",
    "body",
    "contentHash",
    "refs",
    "seen",
    "nonce",
    "time",
    "sig",
    "id",
];

impl PolicyEntry {
    /// Parses and checks an entry given as Json (R-09 for the fields Policy reads): no header
    /// field outside the entry's, `v` 1, the grammar of `author`, `keyId`, `space` and `refs`,
    /// `seen` in range, and the body against its kind's closed schema. An error is Policy's
    /// `invalid`.
    pub fn from_value(entry: &Value) -> Result<PolicyEntry, Error> {
        let m = entry
            .as_object()
            .ok_or_else(|| Error::invalid("unknown header field"))?;
        if m.keys().any(|k| !HEADER_FIELDS.contains(&k.as_str())) {
            return Err(Error::invalid("unknown header field"));
        }
        if m.get("v") != Some(&Value::Int(1)) {
            return Err(Error::invalid("version"));
        }
        let text = |k: &str, ok: fn(&str) -> bool| {
            m.get(k)
                .and_then(Value::as_str)
                .filter(|s| ok(s))
                .map(str::to_owned)
                .ok_or_else(|| Error::invalid(k.to_owned()))
        };
        let author = text("author", g::is_actor_id)?;
        let key_id = text("keyId", g::is_key_id)?;
        let space = text("space", g::is_space_id)?;
        let refs = m
            .get("refs")
            .and_then(Value::as_array)
            .filter(|a| a.len() <= crate::limits::MAX_LIST)
            .and_then(|a| {
                a.iter()
                    .map(|r| r.as_str().filter(|s| g::is_hex(s)).map(str::to_owned))
                    .collect::<Option<Vec<_>>>()
            })
            .ok_or_else(|| Error::invalid("refs"))?;
        let seen = match m.get("seen") {
            Some(Value::Int(i)) if (0..=MAX_SAFE_INT).contains(i) => i.unsigned_abs(),
            _ => return Err(Error::invalid("seen")),
        };
        let kind = m
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::invalid("unknown kind"))?
            .to_owned();
        let body = Body::parse(&kind, m.get("body").unwrap_or(&Value::Null))?;
        Ok(PolicyEntry {
            space,
            kind,
            author,
            key_id,
            body,
            refs,
            seen,
        })
    }

    /// Reads a log [`Entry`] the same way as [`PolicyEntry::from_value`].
    pub fn from_entry(entry: &Entry) -> Result<PolicyEntry, Error> {
        PolicyEntry::from_value(&json::to_value(entry)?)
    }
}

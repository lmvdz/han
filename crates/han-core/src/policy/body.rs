//! An entry's body, typed per kind (R-09): the one schema of every core kind's body. [`Body::parse`]
//! decodes a body strictly into its kind's type and checks what the types cannot say (grammars,
//! ranges, list sizes); a body that passes encodes back to the same Json. Also the entry as Policy
//! reads it.

use serde::{Deserialize, Serialize, Serializer};

use super::doc::{NewRole, PolicyDoc, ProfileDoc, SteerMode};
use super::gate::{Change, Evidence};
use crate::error::Error;
use crate::grammar as g;
use crate::json::{self, MAX_SAFE_INT, Value};
use crate::limits::MAX_LIST;
use crate::types::{ActorId, Budget, Entry, EntryId, KeyId, Seq, SpaceId, Time};

/// `space.create`: the space it forks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkOf {
    /// The forked space.
    pub space: SpaceId,
    /// The seq of it the fork follows.
    pub through: Seq,
}

/// Body of `space.create`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SpaceCreate {
    /// The first policy.
    pub policy: PolicyDoc,
    /// Present on a fork.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork_of: Option<ForkOf>,
    /// Present, and required, in a profile space.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<ProfileDoc>,
}

/// Body of `member.set`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberSet {
    /// The actor whose role changes.
    pub actor: ActorId,
    /// Its new role.
    pub role: NewRole,
}

/// Body of `policy.set`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicySet {
    /// The whole new policy.
    pub policy: PolicyDoc,
}

/// Body of `key.add`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KeyAdd {
    /// The new key's id.
    pub key_id: KeyId,
    /// Its public key.
    pub public_key: String,
}

/// Body of `key.revoke`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KeyRevoke {
    /// The key to revoke.
    pub key_id: KeyId,
}

/// Body of `message`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    /// Addressees.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<Vec<ActorId>>,
}

/// Body of `task`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskBody {
    /// The assignees, at least one.
    pub to: Vec<ActorId>,
    /// Budget reserved for the task.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<Budget>,
    /// The parent task, for a delegation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<EntryId>,
    /// A deadline; informational.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline: Option<Time>,
    /// An opaque external reference.
    #[serde(skip_serializing_if = "Option::is_none")]
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
    /// The wire name of a final state (`None` for the others).
    pub fn final_name(self) -> Option<&'static str> {
        match self {
            TaskState::Done => Some("done"),
            TaskState::Failed => Some("failed"),
            TaskState::Declined => Some("declined"),
            TaskState::Cancelled => Some("cancelled"),
            _ => None,
        }
    }

    /// Done, failed, declined or cancelled.
    pub fn is_final(self) -> bool {
        matches!(
            self,
            TaskState::Done | TaskState::Failed | TaskState::Declined | TaskState::Cancelled
        )
    }
}

/// Body of `task.update`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskUpdate {
    /// The task.
    pub task: EntryId,
    /// Its new state.
    pub state: TaskState,
}

/// Body of `task.cancel`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCancel {
    /// The task.
    pub task: EntryId,
}

/// Body of `effect.propose`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Propose {
    /// Who may claim and run it.
    pub executor: ActorId,
    /// The action name.
    pub action: String,
    /// `sha256(JCS(plan))`.
    pub plan_digest: String,
    /// The cost, in the space's unit when it has a budget.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<Budget>,
    /// Whether a failed attempt may be retried.
    pub repeatable: bool,
    /// The task it is charged to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<EntryId>,
    /// No claim at or after this time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Time>,
    /// The summary of a `vcs.land`'s change, which Policy derives the tier from (P-17).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<Change>,
}

/// Body of `effect.approve`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Approve {
    /// The proposal.
    pub proposal: EntryId,
    /// Must equal the proposal's `planDigest`.
    pub plan_digest: String,
    /// The approval lapses at this time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Time>,
    /// The record the approval rests on (P-17).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
}

/// Body of `effect.claim`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    /// The claim.
    pub claim: EntryId,
    /// The result.
    pub result: EffectResult,
    /// Whether the action started.
    pub started: Started,
    /// What it cost, with `started: yes` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<Budget>,
}

/// The result an `effect.resolve` settles an unknown outcome with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResolveResult {
    /// It happened.
    Succeeded,
    /// It did not.
    Failed,
}

/// Body of `effect.resolve`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resolve {
    /// The proposal.
    pub proposal: EntryId,
    /// The settled result.
    pub result: ResolveResult,
    /// What it cost, on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<Budget>,
}

/// Body of `checkpoint`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    /// The last seq the checkpoint covers.
    pub through: Seq,
}

/// Body of `redact`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Redact {
    /// The entry whose content goes.
    pub target: EntryId,
}

/// Body of `session.open`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionOpen {
    /// The task the session works.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<EntryId>,
    /// How outside input is treated.
    pub steer_mode: SteerMode,
}

/// Body of `session.close` and `session.input`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRef {
    /// The session (its `session.open` entry).
    pub session: EntryId,
}

/// Body of `agent.unguard`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentUnguard {
    /// The agent to unguard.
    pub agent: ActorId,
}

/// The target's acceptance of a `space.move`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MoveAccept {
    /// The target's key id.
    pub key_id: KeyId,
    /// Its signature.
    pub sig: String,
}

/// Body of `space.move`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    json::from_value_strict(body).map_err(|e| Error::invalid(format!("body: {e}")))
}

/// `Ok` if `ok`, else `invalid` naming the body field.
fn field(ok: bool, name: &str) -> Result<(), Error> {
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!("body: bad field {name}")))
    }
}

fn hex(s: &str) -> bool {
    g::is_hex(s)
}

fn opt(v: &Option<String>, ok: fn(&str) -> bool) -> bool {
    v.as_deref().is_none_or(ok)
}

fn int(n: u64) -> bool {
    n <= MAX_SAFE_INT.unsigned_abs()
}

fn cost(c: &Option<Budget>) -> bool {
    c.as_ref().is_none_or(Budget::is_valid)
}

fn actors(to: &[ActorId]) -> bool {
    to.len() <= MAX_LIST && to.iter().all(|a| g::is_actor_id(a))
}

impl Body {
    /// Decodes `body` strictly into the type of `kind` and checks it ([`Body::check`]): an
    /// unknown field, a wrong type, a missing field, a `null` where a field is optional, an
    /// impossible date or an out-of-range number is `invalid`. An extension kind's body must be
    /// plain Json (integers in `[0, 2^53)`, 32 levels).
    pub fn parse(kind: &str, body: &Value) -> Result<Body, Error> {
        if !matches!(body, Value::Object(_)) {
            return Err(Error::invalid("body: not an object"));
        }
        let typed = match kind {
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
            _ if g::is_extension_kind(kind) => Body::Extension(body.clone()),
            _ => return Err(Error::invalid("kind")),
        };
        typed.check()?;
        Ok(typed)
    }

    /// The core kind this body is of; `None` for an extension body.
    pub fn kind(&self) -> Option<&'static str> {
        Some(match self {
            Body::SpaceCreate(_) => "space.create",
            Body::MemberSet(_) => "member.set",
            Body::PolicySet(_) => "policy.set",
            Body::KeyAdd(_) => "key.add",
            Body::KeyRevoke(_) => "key.revoke",
            Body::Message(_) => "message",
            Body::Task(_) => "task",
            Body::TaskUpdate(_) => "task.update",
            Body::TaskCancel(_) => "task.cancel",
            Body::Propose(_) => "effect.propose",
            Body::Approve(_) => "effect.approve",
            Body::Claim(_) => "effect.claim",
            Body::EffectCancel(_) => "effect.cancel",
            Body::Outcome(_) => "effect.outcome",
            Body::Resolve(_) => "effect.resolve",
            Body::Checkpoint(_) => "checkpoint",
            Body::Redact(_) => "redact",
            Body::SessionOpen(_) => "session.open",
            Body::SessionClose(_) => "session.close",
            Body::SessionInput(_) => "session.input",
            Body::AgentUnguard(_) => "agent.unguard",
            Body::SpaceMove(_) => "space.move",
            Body::Extension(_) => return None,
        })
    }

    /// Checks what the types cannot say: every grammar, range and list size of the kind's schema.
    /// [`Body::parse`] makes this check; a body built in code gets it from [`Entry::validate`].
    pub fn check(&self) -> Result<(), Error> {
        match self {
            Body::SpaceCreate(b) => {
                field(b.policy.is_valid(), "policy")?;
                field(
                    b.fork_of
                        .as_ref()
                        .is_none_or(|f| g::is_space_id(&f.space) && int(f.through)),
                    "forkOf",
                )?;
                field(
                    b.profile.as_ref().is_none_or(ProfileDoc::is_valid),
                    "profile",
                )
            }
            Body::MemberSet(b) => field(g::is_actor_id(&b.actor), "actor"),
            Body::PolicySet(b) => field(b.policy.is_valid(), "policy"),
            Body::KeyAdd(b) => {
                field(g::is_key_id(&b.key_id), "keyId")?;
                field(g::is_public_key_text(&b.public_key), "publicKey")
            }
            Body::KeyRevoke(b) => field(g::is_key_id(&b.key_id), "keyId"),
            Body::Message(b) => field(b.to.as_deref().is_none_or(actors), "to"),
            Body::Task(b) => {
                field(!b.to.is_empty() && actors(&b.to), "to")?;
                field(cost(&b.budget), "budget")?;
                field(opt(&b.parent, hex), "parent")?;
                field(opt(&b.deadline, g::is_time), "deadline")?;
                field(opt(&b.external, g::is_external), "external")
            }
            Body::TaskUpdate(b) => {
                field(hex(&b.task), "task")?;
                field(b.state != TaskState::Pending, "state")
            }
            Body::TaskCancel(b) => field(hex(&b.task), "task"),
            Body::Propose(b) => {
                field(g::is_actor_id(&b.executor), "executor")?;
                field(g::is_action(&b.action), "action")?;
                field(hex(&b.plan_digest), "planDigest")?;
                field(cost(&b.cost), "cost")?;
                field(opt(&b.task, hex), "task")?;
                field(opt(&b.expires_at, g::is_time), "expiresAt")?;
                field(b.change.as_ref().is_none_or(Change::is_valid), "change")
            }
            Body::Approve(b) => {
                field(hex(&b.proposal), "proposal")?;
                field(hex(&b.plan_digest), "planDigest")?;
                field(opt(&b.expires_at, g::is_time), "expiresAt")?;
                field(
                    b.evidence.as_ref().is_none_or(Evidence::is_valid),
                    "evidence",
                )
            }
            Body::Claim(b) => {
                field(hex(&b.proposal), "proposal")?;
                field(b.attempt >= 1 && int(b.attempt), "attempt")?;
                field(g::is_key_id(&b.instance), "instance")?;
                field((15..=3600).contains(&b.window), "window")
            }
            Body::EffectCancel(b) => field(hex(&b.proposal), "proposal"),
            Body::Outcome(b) => {
                field(hex(&b.claim), "claim")?;
                field(cost(&b.cost), "cost")
            }
            Body::Resolve(b) => {
                field(hex(&b.proposal), "proposal")?;
                field(cost(&b.cost), "cost")
            }
            Body::Checkpoint(b) => field(int(b.through), "through"),
            Body::Redact(b) => field(hex(&b.target), "target"),
            Body::SessionOpen(b) => field(opt(&b.task, hex), "task"),
            Body::SessionClose(b) | Body::SessionInput(b) => field(hex(&b.session), "session"),
            Body::AgentUnguard(b) => field(g::is_actor_id(&b.agent), "agent"),
            Body::SpaceMove(b) => {
                field(g::is_server_name(&b.to), "to")?;
                field(g::is_key_id(&b.accept.key_id), "accept")
            }
            Body::Extension(v) => {
                if !matches!(v, Value::Object(_)) {
                    return Err(Error::invalid("body: not an object"));
                }
                if !g::is_opaque_json(v) {
                    return Err(Error::invalid(
                        "extension body: only non-negative integers, no floats",
                    ));
                }
                Ok(())
            }
        }
    }
}

impl Body {
    /// The body's Json, exactly as it was parsed. A body built in code with an integer of 2^53
    /// or more is an error.
    pub fn to_value(&self) -> Result<Value, Error> {
        let v = match self {
            Body::SpaceCreate(b) => json::to_value(b),
            Body::MemberSet(b) => json::to_value(b),
            Body::PolicySet(b) => json::to_value(b),
            Body::KeyAdd(b) => json::to_value(b),
            Body::KeyRevoke(b) => json::to_value(b),
            Body::Message(b) => json::to_value(b),
            Body::Task(b) => json::to_value(b),
            Body::TaskUpdate(b) => json::to_value(b),
            Body::TaskCancel(b) => json::to_value(b),
            Body::Propose(b) => json::to_value(b),
            Body::Approve(b) => json::to_value(b),
            Body::Claim(b) => json::to_value(b),
            Body::EffectCancel(b) => json::to_value(b),
            Body::Outcome(b) => json::to_value(b),
            Body::Resolve(b) => json::to_value(b),
            Body::Checkpoint(b) => json::to_value(b),
            Body::Redact(b) => json::to_value(b),
            Body::SessionOpen(b) => json::to_value(b),
            Body::SessionClose(b) | Body::SessionInput(b) => json::to_value(b),
            Body::AgentUnguard(b) => json::to_value(b),
            Body::SpaceMove(b) => json::to_value(b),
            Body::Extension(v) => return Ok(v.clone()),
        };
        Ok(v?)
    }
}

impl Serialize for Body {
    /// The body's Json, exactly as it was parsed: through [`Value`], so members come in the
    /// same order as any other Json object's and the wire bytes are those of the parsed body.
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_value()
            .map_err(serde::ser::Error::custom)?
            .serialize(s)
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

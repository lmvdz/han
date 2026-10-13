//! The state of a space, mirroring `SpaceState` of `han.ts`. It serializes to exactly the JSON the
//! reference produces, so two implementations compare states as JCS bytes.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::body::{EffectResult, Started, TaskState};
use super::doc::{ActorKind, PolicyDoc, Role, SteerMode};
use crate::types::{ActorId, Budget, EntryId, KeyId, Seq, SpaceId, Time};

/// A set of actors. On the wire it is a map whose every value is `true`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActorSet(pub BTreeSet<ActorId>);

impl ActorSet {
    /// Whether `actor` is in the set.
    pub fn contains(&self, actor: &str) -> bool {
        self.0.contains(actor)
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Serialize for ActorSet {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_map(self.0.iter().map(|a| (a, true)))
    }
}

impl<'de> Deserialize<'de> for ActorSet {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let map = BTreeMap::<ActorId, bool>::deserialize(d)?;
        if map.values().any(|v| !v) {
            return Err(serde::de::Error::custom("a set's values are all true"));
        }
        Ok(ActorSet(map.into_keys().collect()))
    }
}

/// An amount of a unit that may be negative: a remainder after a budget cut (P-06).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Remaining {
    /// The unit.
    pub unit: String,
    /// The amount.
    pub amount: i64,
}

/// Where a budget amount came from or goes back to: a task, or the space.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    try_from = "(String, Option<String>)",
    into = "(String, Option<String>)"
)]
pub enum Source {
    /// `["task", id]`
    Task(EntryId),
    /// `["space", null]`
    Space,
}

impl TryFrom<(String, Option<String>)> for Source {
    type Error = String;

    fn try_from(v: (String, Option<String>)) -> Result<Self, String> {
        match v {
            (k, Some(id)) if k == "task" => Ok(Source::Task(id)),
            (k, None) if k == "space" => Ok(Source::Space),
            _ => Err("a source is [\"task\", id] or [\"space\", null]".into()),
        }
    }
}

impl From<Source> for (String, Option<String>) {
    fn from(s: Source) -> Self {
        match s {
            Source::Task(id) => ("task".into(), Some(id)),
            Source::Space => ("space".into(), None),
        }
    }
}

/// A readable-role interval in a space's seqs (P-02): held at `k` iff `from <= k < to`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Interval {
    /// The first seq at which the role was held.
    pub from: Seq,
    /// The seq at which it ended; absent while it lasts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<Seq>,
}

/// A task (P-05, P-15).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Task {
    /// The asker.
    pub author: ActorId,
    /// The assignees.
    pub to: Vec<ActorId>,
    /// Whether it is open.
    pub open: bool,
    /// The last state reported while it was open.
    pub state: TaskState,
    /// Whether a cancel was requested.
    pub cancel_requested: bool,
    /// Delegation depth, 0 for a root task.
    pub depth: u64,
    /// The seq of its creation.
    pub seq: Seq,
    /// The assignees that accepted it, in order.
    pub accepted: Vec<ActorId>,
    /// The guarded agents whose limits it carries; fixed at creation.
    pub guarded_for: Vec<ActorId>,
    /// The parent task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<EntryId>,
    /// The budget reserved for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<Budget>,
    /// What is left of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<Remaining>,
}

/// The phase of an effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Proposed and waiting for a claim.
    Open,
    /// Claimed; the executor is working.
    Claimed,
    /// The outcome is unknown; an `effect.resolve` settles it.
    Unknown,
    /// Done.
    Succeeded,
    /// Failed for good.
    Failed,
    /// Cancelled.
    Cancelled,
}

impl Phase {
    /// Succeeded, failed or cancelled.
    pub fn is_final(self) -> bool {
        matches!(self, Phase::Succeeded | Phase::Failed | Phase::Cancelled)
    }
}

/// An approval of an effect (P-08).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Approval {
    /// The approving entry.
    pub id: EntryId,
    /// Its author.
    pub author: ActorId,
    /// The key that signed it.
    pub key_id: KeyId,
    /// The author's kind in the profile pinned for the approval; it never changes.
    pub actor_kind: ActorKind,
    /// The approval lapses at this time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Time>,
    /// The claim that consumed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_by: Option<EntryId>,
}

/// A claim of an effect (P-07).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ClaimState {
    /// The claiming entry.
    pub id: EntryId,
    /// The attempt number, from 1.
    pub attempt: u64,
    /// The claiming key.
    pub instance: KeyId,
    /// The receipt time of the claim.
    pub time: Time,
    /// The `launchWindow` in force when it was ordered.
    pub window: u64,
    /// The approval it consumed under the approval rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<EntryId>,
    /// The guards' approvals it consumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_approvals: Option<Vec<EntryId>>,
    /// The budget it reserved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserved: Option<Budget>,
    /// Where the reservation came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    /// Whether the reservation is settled.
    pub settled: bool,
    /// The reported result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<EffectResult>,
    /// Whether the action started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<Started>,
}

/// An effect: a proposal and what became of it (P-06 to P-09).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Effect {
    /// The proposer.
    pub proposer: ActorId,
    /// The key that signed the proposal.
    pub proposer_key: KeyId,
    /// The executor.
    pub executor: ActorId,
    /// The action name.
    pub action: String,
    /// The plan's digest.
    pub plan_digest: String,
    /// The seq of the proposal.
    pub seq: Seq,
    /// The cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Budget>,
    /// Whether a failed attempt may be retried.
    pub repeatable: bool,
    /// The task it is charged to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<EntryId>,
    /// The phase.
    pub phase: Phase,
    /// Whether a cancel was requested.
    pub cancel_requested: bool,
    /// The guarded agents whose guards must approve a claim; fixed at the proposal.
    pub guarded_for: Vec<ActorId>,
    /// No claim at or after this time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Time>,
    /// The approvals that still matter.
    pub approvals: Vec<Approval>,
    /// The claims, oldest first.
    pub claims: Vec<ClaimState>,
}

/// What the state keeps of an entry (R-05 resolution, P-04 redaction).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryRef {
    /// Its author.
    pub author: ActorId,
    /// Its kind.
    pub kind: String,
}

/// An open session (P-15).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Session {
    /// The actor whose session it is.
    pub actor: ActorId,
    /// The receipt time of the opening.
    pub opened: Time,
    /// How outside input is treated.
    pub steer_mode: SteerMode,
    /// The number of steers it has taken.
    pub steers: u64,
    /// The task it works.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<EntryId>,
}

/// The state of a profile space (section 4.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProfileState {
    /// The actor's kind.
    pub actor_kind: ActorKind,
    /// The operator of a non-human actor; `null` for a human.
    pub operator: Option<ActorId>,
    /// Whether the operator confirmed.
    pub operator_confirmed: bool,
    /// The actor's keys.
    pub keys: BTreeMap<KeyId, KeyState>,
}

/// A key of a profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KeyState {
    /// The public key.
    pub public_key: String,
    /// Whether it is valid or revoked.
    pub status: super::KeyStatus,
}

/// The folded state of a space after some record (`SpaceState` of `han.ts`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SpaceState {
    /// The space.
    pub space: SpaceId,
    /// The seq of the last folded record.
    pub head: Seq,
    /// The current home server.
    pub home: String,
    /// The policy in force.
    pub policy: PolicyDoc,
    /// Current roles; an actor with no role is absent.
    pub roles: BTreeMap<ActorId, Role>,
    /// Readable-role intervals, the 64 most recent per actor.
    pub role_history: BTreeMap<ActorId, Vec<Interval>>,
    /// Tasks by the id of their entry.
    pub tasks: BTreeMap<EntryId, Task>,
    /// Effects by the id of their proposal.
    pub effects: BTreeMap<EntryId, Effect>,
    /// Budget minus what is committed; negative only after a cut.
    pub budget_left: Option<Remaining>,
    /// Every folded entry.
    pub entries: BTreeMap<EntryId, EntryRef>,
    /// Open sessions.
    pub sessions: BTreeMap<EntryId, Session>,
    /// Agents that chose guarding.
    pub guarded: ActorSet,
    /// Present in profile spaces only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<ProfileState>,
}

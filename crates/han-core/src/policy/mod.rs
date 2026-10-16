//! HAN Policy (architecture.md sections 4 to 6, rules P-01 to P-16): six pure functions over a
//! typed [`SpaceState`], with no I/O and no clock. This is a port of `ref/policy.py`, the
//! executable form of the rules; `conformance/vectors` and the differential fuzz check it.
//!
//! * [`decide`]: may this entry be appended to this state? ([`Verdict`])
//! * [`fold`]: the state after a record the Log ordered.
//! * [`can_read`]: P-02 read rights at a seq.
//! * [`relevant_actors`]: P-01, the profiles `decide` needs pinned.
//! * [`session_access`]: P-15, what an actor may do with a live session.
//! * [`guard_status`]: P-15, the one guard function.
//!
//! An entry reaches Policy as a [`PolicyEntry`], with its body typed per kind ([`Body`]).
//! [`decide_value`] takes an entry as Json and answers `invalid` when it is malformed.

mod access;
mod body;
mod decide;
mod doc;
mod fold;
pub(crate) mod gate;
mod guard;
mod state;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::grammar;
use crate::types::{ActorId, EntryId, KeyId, LogRecord, Seq, Time};

pub use crate::validate::change_summary;
pub use access::{SessionAccess, can_read, session_access};
pub use body::{
    AgentUnguard, Approve, Body, Checkpoint, Claim, EffectCancel, EffectResult, ForkOf, KeyAdd,
    KeyRevoke, MemberSet, Message, MoveAccept, Outcome, PolicyEntry, PolicySet, Propose, Redact,
    Resolve, ResolveResult, SessionOpen, SessionRef, SpaceCreate, SpaceMove, Started, TaskBody,
    TaskCancel, TaskState, TaskUpdate,
};
pub use decide::{decide, decide_value, relevant_actors};
pub use doc::{
    ActorKind, AllowAgents, ApprovalRule, CHECK_ONLY, GatesDoc, JoinMode, KeyDoc, NewRole,
    OwnerRule, PathClass, PathRule, PolicyDoc, ProfileDoc, Role, ServersDoc, SessionLevel,
    SessionsDoc, SteerMode, Visibility,
};
pub use fold::fold;
pub use gate::{
    Change, ChangeFile, Evidence, FileKind, Gate, MAX_CHANGE_BYTES, MAX_CHANGE_FILES,
    MAX_GATE_RULES, MAX_PATH_OWNERS, NEVER_AUTO, PATH_OWNER, change_tier, glob_match,
};
pub use guard::{GuardInput, GuardStatus, abandon_after, guard_status, proposer_suffices};
pub use state::{
    ActorSet, Approval, ClaimState, Effect, EntryRef, Interval, KeyState, Phase, ProfileState,
    Remaining, Session, Source, SpaceState, Task,
};

/// P-08: countable open approvals by other authors a non-guard's approval may find on a proposal.
pub const MAX_OPEN_APPROVALS: usize = 16;
/// P-15: open sessions per actor per space.
pub const MAX_OPEN_SESSIONS: usize = 64;
/// P-07 E5: claims per effect; the 65th is denied.
pub const MAX_ATTEMPTS: usize = 64;
/// P-02: role intervals kept per actor (the most recent).
pub const ROLE_INTERVALS: usize = 64;
/// P-15: the most agents a proposal's `guardedFor`, its proposer and its executor may hold; a
/// task's `guardedFor` may hold two fewer.
pub const MAX_GUARDED_FOR: usize = 19;
/// P-08, derived and never checked: bounds an effect's E3 candidates.
pub const MAX_UNCONSUMED_APPROVALS: usize = MAX_OPEN_APPROVALS + 1 + MAX_GUARDED_FOR;
/// P-08, derived and never checked: bounds an effect's approvals.
pub const MAX_APPROVALS: usize = MAX_OPEN_APPROVALS + 1 + 2 * MAX_GUARDED_FOR;

/// Policy's answer to `decide` (P-16). The reason is free text, not part of the contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Append it.
    Allow,
    /// Well-formed, but the rules refuse it now.
    Denied(String),
    /// Malformed, or it names something that is not there.
    Invalid(String),
}

impl Verdict {
    /// Whether the entry may be appended.
    pub fn is_allow(&self) -> bool {
        matches!(self, Verdict::Allow)
    }

    /// `allow`, `denied` or `invalid`: the code vectors compare.
    pub fn code(&self) -> &'static str {
        match self {
            Verdict::Allow => "allow",
            Verdict::Denied(_) => "denied",
            Verdict::Invalid(_) => "invalid",
        }
    }

    /// The reason, empty for `allow`.
    pub fn reason(&self) -> &str {
        match self {
            Verdict::Allow => "",
            Verdict::Denied(r) | Verdict::Invalid(r) => r,
        }
    }
}

/// Whether a key of a profile is usable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyStatus {
    /// Signs.
    Valid,
    /// Revoked.
    Revoked,
}

/// A key as a pinned profile shows it. Policy reads only `status`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyView {
    /// Valid or revoked.
    pub status: KeyStatus,
}

/// A profile pinned for an entry (`ProfileView` of `han.ts`). Policy reads only `actorKind`,
/// `operator`, `operatorConfirmed` and the status of each key; other members are ignored.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileView {
    /// The actor's kind.
    pub actor_kind: ActorKind,
    /// The operator of a non-human actor.
    #[serde(default)]
    pub operator: Option<ActorId>,
    /// Whether the operator confirmed.
    #[serde(default)]
    pub operator_confirmed: bool,
    /// The actor's keys by id.
    #[serde(default)]
    pub keys: BTreeMap<KeyId, KeyView>,
}

/// What `decide`, `fold` and `relevant_actors` know besides the state: the receipt time and the
/// profiles pinned for the entry (P-01).
#[derive(Clone, Debug)]
pub struct Ctx {
    time: Time,
    secs: i64,
    profiles: BTreeMap<ActorId, ProfileView>,
}

impl Ctx {
    /// A context at `time` (`YYYY-MM-DDTHH:MM:SSZ`) with the pinned `profiles`.
    pub fn new(time: &str, profiles: BTreeMap<ActorId, ProfileView>) -> Result<Ctx, Error> {
        let secs = grammar::epoch_seconds(time).ok_or_else(|| Error::invalid("time"))?;
        Ok(Ctx {
            time: time.to_owned(),
            secs,
            profiles,
        })
    }

    /// The receipt time.
    pub fn time(&self) -> &str {
        &self.time
    }

    /// The profile pinned for `actor`, if any.
    pub fn profile(&self, actor: &str) -> Option<&ProfileView> {
        self.profiles.get(actor)
    }
}

/// A record the Log ordered, as `fold` reads it: the entry, its id and the receipt's `seq` and
/// `time`.
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    /// The entry.
    pub entry: PolicyEntry,
    /// Its id (`receipt.id`).
    pub id: EntryId,
    /// The receipt's seq.
    pub seq: Seq,
    /// The receipt's time.
    pub time: Time,
}

impl Record {
    /// Reads a [`LogRecord`]: its entry as [`PolicyEntry::from_entry`] does, and the receipt's
    /// id, seq and time.
    pub fn from_log_record(record: &LogRecord) -> Result<Record, Error> {
        Ok(Record {
            entry: PolicyEntry::from_entry(&record.entry)?,
            id: record.receipt.id.clone(),
            seq: record.receipt.seq,
            time: record.receipt.time.clone(),
        })
    }
}

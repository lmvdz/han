//! The policy document and the small enums that bodies, profiles and state share.

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};

use super::gate::{MAX_GATE_RULES, PATH_OWNER, is_evidence_kind, is_glob, is_repo_name};
use crate::grammar as g;
use crate::json::{self, MAX_SAFE_INT, Value};
use crate::limits::{MAX_DENIED_SERVERS, MAX_LIST};
use crate::types::{ActorId, Budget, Durability};

/// A role in a space. `member.set` with the role `none` removes the entry, so `none` is not a
/// state value (see [`NewRole`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Governs the space; always a human (P-03).
    Owner,
    /// Takes part.
    Member,
    /// Reads and may be assigned.
    Guest,
    /// Was removed and may not return unless an owner says so.
    Banned,
}

impl Role {
    /// Owner, member or guest: the roles that read (the protocol's `READ_ROLES`).
    pub fn reads(self) -> bool {
        !matches!(self, Role::Banned)
    }

    /// Owner or member: the roles that create work.
    pub(crate) fn writes(self) -> bool {
        matches!(self, Role::Owner | Role::Member)
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Member => "member",
            Role::Guest => "guest",
            Role::Banned => "banned",
        }
    }

    /// Owner 0, member 1, guest 2: lower is wider. Banned has no rank.
    pub(crate) fn rank(self) -> Option<u8> {
        match self {
            Role::Owner => Some(0),
            Role::Member => Some(1),
            Role::Guest => Some(2),
            Role::Banned => None,
        }
    }
}

/// The role a `member.set` assigns: a [`Role`] or `none` (the actor leaves the role table).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NewRole {
    /// See [`Role::Owner`].
    Owner,
    /// See [`Role::Member`].
    Member,
    /// See [`Role::Guest`].
    Guest,
    /// See [`Role::Banned`].
    Banned,
    /// No role.
    None,
}

impl NewRole {
    /// The role, or `None` for `none`.
    pub fn role(self) -> Option<Role> {
        match self {
            NewRole::Owner => Some(Role::Owner),
            NewRole::Member => Some(Role::Member),
            NewRole::Guest => Some(Role::Guest),
            NewRole::Banned => Some(Role::Banned),
            NewRole::None => None,
        }
    }
}

/// What kind of actor a profile describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActorKind {
    /// A person.
    Human,
    /// An autonomous agent; has an operator.
    Agent,
    /// A service; has an operator.
    Service,
}

/// Who may read a space (P-02).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    /// Role holders only.
    Members,
    /// Anyone.
    Public,
}

/// How an actor becomes a member (P-03).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JoinMode {
    /// By an owner's (or, if allowed, a member's) invitation.
    Invite,
    /// Anyone joins as a member.
    Open,
    /// Nobody joins.
    Closed,
}

/// Which non-human actors may hold a role (P-03).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AllowAgents {
    /// Any.
    #[serde(rename = "any")]
    Any,
    /// Only agents whose operator confirmed them.
    #[serde(rename = "with-operator")]
    WithOperator,
    /// None.
    #[serde(rename = "none")]
    None,
}

/// A minimum role for a session right, least to most restrictive (P-15); the order is that one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionLevel {
    /// Any role holder.
    Guest,
    /// Members and owners.
    Member,
    /// Owners.
    Owner,
    /// Nobody but the session's own actor.
    Nobody,
}

/// How a session's host treats input from others (P-15).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SteerMode {
    /// Like its own input.
    Auto,
    /// In the harness's stricter mode; the agent chooses guarding.
    Guarded,
}

/// One approval rule: the first rule whose `action` matches (or is `*`) and whose tier predicate holds applies
/// (P-08, P-17).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ApprovalRule {
    /// An action name, or `*`.
    pub action: String,
    /// Role names (`owner`, `member`, `guest`), actor ids and `@path-owner`.
    pub approvers: Vec<String>,
    /// The rule applies only to an effect whose gate has at least this tier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_tier: Option<u8>,
    /// The rule applies only to an effect whose gate has at most this tier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tier: Option<u8>,
    /// Only human approvals count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub humans: Option<bool>,
    /// A non-human approval counts only with evidence of this kind over the effect's delta digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    /// Distinct approvals a claim consumes, 1 to 3 (at tier 3, at least 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs: Option<u8>,
    /// Only a revert (`change.revertOf`) is covered, and the lane's own approval of it is exempt from
    /// `separateApprover` (A2). Always `true` when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revert: Option<bool>,
    /// The proposal itself satisfies E3: no approval is asked, made or consumed (P-17, round 33). Always `true`
    /// when present, and valid only on a rule for the check-only action [`CHECK_ONLY`] with no other member than
    /// `action` and `approvers`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposer: Option<bool>,
}

/// The one action a rule with `proposer` may name: `vcs.try` runs a landing lane's checks on a change and lands
/// nothing (P-17, round 33).
pub const CHECK_ONLY: &str = "vcs.try";

/// The class of a path (P-17); its tier floor is docs, tests and generated 0, code 1, sensitive 2, critical 3.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PathClass {
    /// Documentation.
    Docs,
    /// Tests.
    Tests,
    /// Generated files.
    Generated,
    /// Ordinary code (the default).
    Code,
    /// Sensitive paths.
    Sensitive,
    /// Releases, infrastructure, secrets, auth and crypto, migrations.
    Critical,
}

/// A path-class rule: the first whose glob matches a path (in its repo) gives the path its class.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathRule {
    /// The path glob.
    pub glob: String,
    /// The class.
    pub class: PathClass,
    /// The repository it applies to; absent means every repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// A path-owner rule: the first whose glob matches a path (in its repo) names the path's owners.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerRule {
    /// The path glob.
    pub glob: String,
    /// One to eight owners.
    pub owners: Vec<ActorId>,
    /// The repository it applies to; absent means every repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// Semantic gates and risk tiers (P-17).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatesDoc {
    /// A change of at most this many lines, in tier-0 classes only, is tier 0.
    pub small: u64,
    /// A change of more than this many lines is at least tier 2.
    pub large: u64,
    /// Path classes, first match wins per path.
    pub paths: Vec<PathRule>,
    /// Path owners, first match wins per path.
    pub owners: Vec<OwnerRule>,
}

/// The session rights of a policy (P-15).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionsDoc {
    /// Who may watch a session's output.
    pub watch: SessionLevel,
    /// Who may read its full context; no wider than `watch`.
    pub read: SessionLevel,
    /// Who may steer it; no wider than `watch`.
    pub steer: SessionLevel,
    /// The steer modes `session.open` may use.
    pub modes: Vec<SteerMode>,
    /// Every non-human actor is guarded for its operator. Absent means off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force_guarded: Option<bool>,
}

/// The servers a space refuses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServersDoc {
    /// Server names whose actors hold no role and whose nodes do not read.
    pub deny: Vec<String>,
}

/// A space's policy (P-02 to P-15); `PolicyDoc` of `han.ts`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PolicyDoc {
    /// Who may read.
    pub visibility: Visibility,
    /// How actors join.
    pub join: JoinMode,
    /// Whether members may invite.
    pub members_may_invite: bool,
    /// Which non-human actors may hold a role.
    pub allow_agents: AllowAgents,
    /// Approval rules, first match wins.
    pub approvals: Vec<ApprovalRule>,
    /// Whether a proposal's author may not approve it.
    pub separate_approver: bool,
    /// The space's budget (one unit), or `None` for costs unlimited. Always present: `null`
    /// when unlimited.
    #[serde(deserialize_with = "nullable")]
    pub budget: Option<Budget>,
    /// Deepest task delegation, 0 to 16.
    pub max_delegation_depth: u64,
    /// The durability the home must reach.
    pub min_durability: Durability,
    /// Seconds, 15 to 3600: abandon threshold and executor deadline.
    pub launch_window: u64,
    /// The lowest role that may append extension kinds.
    pub extensions_min_role: Role,
    /// Session rights.
    pub sessions: SessionsDoc,
    /// Denied servers.
    pub servers: ServersDoc,
    /// Semantic gates and risk tiers (P-17). Absent means no tiers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gates: Option<GatesDoc>,
}

/// A field that must be present and may be `null`.
fn nullable<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned>(
    d: D,
) -> Result<Option<T>, D::Error> {
    match Value::deserialize(d)? {
        Value::Null => Ok(None),
        v => json::from_value_strict(&v)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

const READ_ROLES: [&str; 3] = ["owner", "member", "guest"];

impl ApprovalRule {
    fn is_valid(&self) -> bool {
        let tier = |t: Option<u8>| t.is_none_or(|t| t <= 3);
        (self.action == "*" || g::is_action(&self.action))
            && (1..=MAX_LIST).contains(&self.approvers.len())
            && self.approvers.iter().all(|a| {
                READ_ROLES.contains(&a.as_str()) || a == PATH_OWNER || g::is_actor_id(a)
            })
            && tier(self.min_tier)
            && tier(self.max_tier)
            && self
                .min_tier
                .zip(self.max_tier)
                .is_none_or(|(lo, hi)| lo <= hi)
            && self.evidence.as_deref().is_none_or(is_evidence_kind)
            && self.needs.is_none_or(|n| (1..=3).contains(&n))
            && self.revert.is_none_or(|r| r)
            // P-17: `proposer` is `true`, on the check-only action and a rule with nothing else on it
            && self.proposer.is_none_or(|p| {
                p && self.action == CHECK_ONLY
                    && self.min_tier.is_none()
                    && self.max_tier.is_none()
                    && self.humans.is_none()
                    && self.evidence.is_none()
                    && self.needs.is_none()
                    && self.revert.is_none()
            })
    }
}

impl GatesDoc {
    fn is_valid(&self) -> bool {
        let repo = |r: &Option<String>| r.as_deref().is_none_or(is_repo_name);
        self.small <= MAX_SAFE_INT.unsigned_abs()
            && self.large <= MAX_SAFE_INT.unsigned_abs()
            && self.small <= self.large
            && self.paths.len() <= MAX_GATE_RULES
            && self.paths.iter().all(|r| is_glob(&r.glob) && repo(&r.repo))
            && self.owners.len() <= MAX_GATE_RULES
            && self.owners.iter().all(|r| {
                is_glob(&r.glob)
                    && (1..=8).contains(&r.owners.len())
                    && r.owners.iter().all(|o| g::is_actor_id(o))
                    && r.owners.iter().collect::<BTreeSet<_>>().len() == r.owners.len()
                    && repo(&r.repo)
            })
    }
}

impl SessionsDoc {
    fn is_valid(&self) -> bool {
        // Read and steer need watch: watch may not be more restrictive than either.
        self.watch <= self.read
            && self.watch <= self.steer
            && !self.modes.is_empty()
            && self
                .modes
                .iter()
                .map(|m| *m as u8)
                .collect::<BTreeSet<_>>()
                .len()
                == self.modes.len()
    }
}

impl PolicyDoc {
    /// The rules its types cannot say: grammars, list sizes and ranges.
    pub(crate) fn is_valid(&self) -> bool {
        self.approvals.len() <= MAX_LIST
            && self.approvals.iter().all(ApprovalRule::is_valid)
            && self.budget.as_ref().is_none_or(Budget::is_valid)
            && self.max_delegation_depth <= 16
            && (15..=3600).contains(&self.launch_window)
            && self.sessions.is_valid()
            && self.servers.deny.len() <= MAX_DENIED_SERVERS
            && self.servers.deny.iter().all(|s| g::is_server_name(s))
            && self.gates.as_ref().is_none_or(GatesDoc::is_valid)
    }

    pub(crate) fn forced(&self) -> bool {
        self.sessions.force_guarded == Some(true)
    }

    pub(crate) fn denies(&self, server: &str) -> bool {
        self.servers.deny.iter().any(|d| d == server)
    }
}

/// The profile a `space.create` of a profile space carries (§4.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProfileDoc {
    /// The actor's kind.
    pub actor_kind: ActorKind,
    /// The operator of a non-human actor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<ActorId>,
    /// The actor's first key.
    pub key: KeyDoc,
}

impl ProfileDoc {
    /// The grammar of the operator and the key.
    pub(crate) fn is_valid(&self) -> bool {
        self.operator.as_deref().is_none_or(g::is_actor_id)
            && g::is_key_id(&self.key.key_id)
            && g::is_public_key_text(&self.key.public_key)
    }
}

/// A key as a profile or `key.add` names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KeyDoc {
    /// The key's id.
    pub key_id: String,
    /// Base64url public key.
    pub public_key: String,
}

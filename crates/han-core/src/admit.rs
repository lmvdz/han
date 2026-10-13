//! The peer filter of the `han/1` carrier (architecture.md section 8.2, N-02): a pure function of
//! what a connection has proved and of the spaces the node serves, judged per call before the
//! call's own checks (G-07 (3)).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::policy::{JoinMode, PolicyDoc, Role, SpaceState, Visibility};
use crate::types::{ActorId, SpaceId};

/// A node id: a device's Ed25519 public key in canonical base64url (N-01).
pub type NodeId = String;

/// What a connection proved by a successful `hello`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Hello {
    /// The authenticated actor.
    pub who: ActorId,
    /// The node ids that vouch for it: the `valid` keys of its pinned profile (N-01), or for a
    /// server actor `@:S` the `sequencer` node ids of S's well-known document.
    pub nodes: Vec<NodeId>,
}

/// The facts of one call on a connection (`AdmitConn` of `han.ts`).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct AdmitConn {
    /// The peer's node id (its TLS key).
    pub node: NodeId,
    /// The call's name.
    pub call: String,
    /// The space the call names: `args.space`, or the `space` of an appended entry.
    #[serde(default)]
    pub space: Option<SpaceId>,
    /// Present after a successful `hello`.
    #[serde(default)]
    pub hello: Option<Hello>,
}

/// The part of a policy `admit` reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub struct AdmitPolicy {
    /// Who may read the space.
    pub visibility: Visibility,
    /// How actors join it.
    pub join: JoinMode,
}

/// A space the node serves, as much of its state as `admit` reads (`Pick<SpaceState, "policy" |
/// "roles">`). A profile space's roles name its actor and `@:server`, so serving an actor's
/// profile makes its devices members here.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct AdmitSpace {
    /// The policy.
    pub policy: AdmitPolicy,
    /// Who holds which role.
    pub roles: BTreeMap<ActorId, Role>,
}

impl From<&SpaceState> for AdmitSpace {
    fn from(s: &SpaceState) -> Self {
        AdmitSpace {
            policy: policy_of(&s.policy),
            roles: s.roles.clone(),
        }
    }
}

fn policy_of(p: &PolicyDoc) -> AdmitPolicy {
    AdmitPolicy {
        visibility: p.visibility,
        join: p.join,
    }
}

/// The spaces this node serves.
pub type AdmitSpaces = BTreeMap<SpaceId, AdmitSpace>;

/// How far a call is admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// A registered device of an actor holding a role in a space served here: every call, which
    /// then judges itself.
    Member,
    /// A node its server's well-known document lists: every call, likewise.
    Server,
    /// A stranger's call, rate-limited per node by the host.
    Open,
}

/// The peer filter's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdmitVerdict {
    /// Let the call reach its rules.
    Allow(Tier),
    /// Refuse it; the reason is free text.
    Deny(&'static str),
}

/// The calls a stranger may make on a public or profile space, or on one not held here.
const READ_CALLS: [&str; 4] = ["read", "subscribe", "state", "blobGet"];

/// N-02: judges one call. Any node may say `hello`; nothing else is admitted before it (`join`
/// too). A node that vouches for its hello's actor is admitted as `Server` when the actor is a
/// server actor, as `Member` when the actor holds a role in a space served here. Anyone else
/// may `join` or `cancel`, read public, profile and not-held spaces, and append to a space with
/// `join: "open"`.
pub fn admit(conn: &AdmitConn, spaces: &AdmitSpaces) -> AdmitVerdict {
    let call = conn.call.as_str();
    if call == "hello" {
        return AdmitVerdict::Allow(Tier::Open); // any node may say hello (X9)
    }
    let Some(hello) = &conn.hello else {
        return AdmitVerdict::Deny("hello first"); // G-07 (3); `join` too (X12)
    };
    let bound = hello.nodes.contains(&conn.node);
    if bound && hello.who.starts_with("@:") {
        return AdmitVerdict::Allow(Tier::Server); // G-10: a node listed by the server it speaks for
    }
    if bound
        && spaces
            .values()
            .any(|s| s.roles.get(&hello.who).is_some_and(|r| r.reads()))
    {
        return AdmitVerdict::Allow(Tier::Member); // a registered device of a role holder here (N-01)
    }
    if call == "join" || call == "cancel" {
        return AdmitVerdict::Allow(Tier::Open); // N-03 after hello; ending one's own stream
    }
    let space = conn.space.as_deref();
    if READ_CALLS.contains(&call) {
        let held = space.and_then(|sp| spaces.get(sp).map(|s| (sp, s)));
        return match held {
            Some((sp, s))
                if !sp.starts_with("profile:") && s.policy.visibility != Visibility::Public =>
            {
                AdmitVerdict::Deny("members only")
            }
            _ => AdmitVerdict::Allow(Tier::Open), // public reads (P-02); a space not held: section 4.1 step 2
        };
    }
    if call == "append"
        && space
            .and_then(|sp| spaces.get(sp))
            .is_some_and(|s| s.policy.join == JoinMode::Open)
    {
        return AdmitVerdict::Allow(Tier::Open); // an open join (P-03) is a stranger's append
    }
    AdmitVerdict::Deny("not a member's device")
}

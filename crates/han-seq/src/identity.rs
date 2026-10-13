//! Identity (M3): the profile views a sequencer pins, and the server's own keys.

use std::collections::BTreeMap;

use han_core::PublicKey;
use han_core::policy::{ActorKind, KeyStatus, KeyView, ProfileState, ProfileView};
use han_core::types::{ActorId, KeyId, Seq};
use serde::{Deserialize, Serialize};

/// A key of a pinned profile: the public key and whether it is usable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinKey {
    /// Base64url public key.
    pub public_key: String,
    /// Valid or revoked.
    pub status: KeyStatus,
}

/// A profile as pinned for one entry (`ProfileView` of `han.ts`). A receipt records only its `seq`; the
/// sequencer stores the whole view next to the record so that the state can be replayed from its own file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinView {
    /// The profile space's head when it was pinned (0 for a server actor's synthetic view).
    pub seq: Seq,
    /// The actor's kind.
    pub actor_kind: ActorKind,
    /// The operator of a non-human actor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<ActorId>,
    /// Whether the operator confirmed.
    pub operator_confirmed: bool,
    /// The actor's keys by id.
    pub keys: BTreeMap<KeyId, PinKey>,
}

impl PinView {
    /// The view of a profile space whose state is `profile` at head `seq`.
    pub fn from_profile(seq: Seq, profile: &ProfileState) -> PinView {
        PinView {
            seq,
            actor_kind: profile.actor_kind,
            operator: profile.operator.clone(),
            operator_confirmed: profile.operator_confirmed,
            keys: profile
                .keys
                .iter()
                .map(|(id, k)| {
                    (
                        id.clone(),
                        PinKey {
                            public_key: k.public_key.clone(),
                            status: k.status,
                        },
                    )
                })
                .collect(),
        }
    }

    /// What Policy reads of it.
    pub fn for_policy(&self) -> ProfileView {
        ProfileView {
            actor_kind: self.actor_kind,
            operator: self.operator.clone(),
            operator_confirmed: self.operator_confirmed,
            keys: self
                .keys
                .iter()
                .map(|(id, k)| (id.clone(), KeyView { status: k.status }))
                .collect(),
        }
    }

    /// The public keys that vouch for the actor's devices: its valid keys, which are also its node ids (N-01).
    pub fn valid_nodes(&self) -> Vec<String> {
        self.keys
            .values()
            .filter(|k| k.status == KeyStatus::Valid)
            .map(|k| k.public_key.clone())
            .collect()
    }

    /// The public key of `key_id`, whatever its status (Policy judges status, R-12).
    pub fn public_key(&self, key_id: &str) -> Option<PublicKey> {
        PublicKey::from_base64url(&self.keys.get(key_id)?.public_key).ok()
    }
}

/// A server key as published at `/.well-known/han` (section 4.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerKey {
    /// The key's id.
    pub key_id: KeyId,
    /// Base64url public key.
    pub public_key: String,
    /// The first instant the key signs.
    pub from: String,
    /// The first instant it no longer signs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    /// Marked compromised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compromised: Option<bool>,
}

impl ServerKey {
    /// Whether `time` lies in the key's window `[from, until)`.
    pub fn covers(&self, time: &str) -> bool {
        time >= self.from.as_str() && self.until.as_deref().is_none_or(|u| time < u)
    }
}

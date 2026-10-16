//! The local gate seam (PLAN §10 "Risk tiers", "Semantic gates"). Spec round 33 will define risk tiers and
//! change-kind rules in the PolicyDoc; until then every action outside the sandbox needs an approval, and the
//! executor does not claim an effect without one, whatever the space's policy says.

use han_vcs::ChangeSummary;
use serde::{Deserialize, Serialize};

/// What the gate reads about one effect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateInput {
    /// The action.
    pub action: String,
    /// The plan's target.
    pub target: String,
    /// The changeset summary the plan carries (paths and kinds), when the action lands code.
    pub changes: Option<ChangeSummary>,
    /// Authors of the open approvals the node shows.
    pub approvals: Vec<String>,
    /// Whether Policy says the proposal itself satisfies E3 for this effect (the rule that applies has
    /// `proposer: true`, P-17): the node reports it in the effect row. The executor keeps no list of actions that
    /// need no approval; it asks Policy.
    pub proposer_suffices: bool,
}

/// The gate's decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum GateDecision {
    /// Claim it now (Policy still judges the claim, E3).
    Run,
    /// Wait for an approval.
    NeedsApproval {
        /// Why, for the approval inbox.
        why: String,
    },
}

/// Decides whether the executor may claim an effect now.
pub trait Gate: Send + Sync + 'static {
    /// The decision for one effect.
    fn decide(&self, input: &GateInput) -> GateDecision;
}

/// The pre-round-33 gate: everything needs at least one approval, except where Policy says the proposal itself
/// suffices (a `proposer: true` rule, which it allows only for a check-only action).
pub struct NeedsApproval;

impl Gate for NeedsApproval {
    fn decide(&self, input: &GateInput) -> GateDecision {
        if input.approvals.is_empty() && !input.proposer_suffices {
            GateDecision::NeedsApproval {
                why: format!(
                    "{} reaches outside the sandbox: every such action needs an approval until risk tiers \
                     are specified",
                    input.action
                ),
            }
        } else {
            GateDecision::Run
        }
    }
}

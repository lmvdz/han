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

/// The pre-round-33 gate: everything needs at least one approval.
pub struct NeedsApproval;

impl Gate for NeedsApproval {
    fn decide(&self, input: &GateInput) -> GateDecision {
        if input.approvals.is_empty() {
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

//! Policy presets shared by faces and the node, without runtime dependencies.

use serde_json::{Value, json};

use super::NodeError;

/// Builds the Trusted team policy for exact repository identifiers.
/// Ordinary changes in these repositories may land automatically; other effects need an owner.
pub fn trusted_team_policy(repos: &[String]) -> Result<Value, NodeError> {
    if repos.is_empty() || repos.len() > 63 {
        return Err(NodeError::invalid("choose between 1 and 63 repositories"));
    }
    let mut unique = std::collections::BTreeSet::new();
    for repo in repos {
        if repo.is_empty()
            || repo.len() > 128
            || !repo
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-:/@".contains(&b))
            || !unique.insert(repo)
        {
            return Err(NodeError::invalid(
                "repositories must be distinct exact identifiers (letters, digits, . _ - : / @; no wildcards or spaces)",
            ));
        }
    }
    let mut approvals: Vec<Value> = repos
        .iter()
        .map(|repo| {
            json!({
                "action": "vcs.land", "repo": repo, "maxTier": 1,
                "approvers": ["owner"], "proposer": true
            })
        })
        .collect();
    approvals.push(json!({"action": "*", "approvers": ["owner"], "humans": true}));
    Ok(json!({
        "visibility": "members", "join": "invite", "membersMayInvite": false,
        "allowAgents": "with-operator", "approvals": approvals,
        "separateApprover": false, "budget": null, "maxDelegationDepth": 2,
        "minDurability": "local", "launchWindow": 30, "extensionsMinRole": "member",
        "sessions": {"watch": "member", "read": "member", "steer": "owner", "modes": ["auto", "guarded"]},
        "servers": {"deny": []},
        "gates": {"small": 20, "large": 500, "paths": [], "owners": []}
    }))
}

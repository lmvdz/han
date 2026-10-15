//! The independence rules (PLAN.md §12 "Machine review"), enforced before the model sees a byte.
//!
//! A machine review counts for tier 1 only if the reviewer is:
//! * of a different model vendor from the author's harness and from the author's model; and
//! * outside the author's operator chain: the reviewer actor is not the author or one of its operators, and
//!   no operator of the reviewer is the author or one of the author's operators.
//!
//! Both rules fail closed: a vendor this table does not know is refused, never assumed different.

use serde::{Deserialize, Serialize};

/// Who made the change, as far as independence is concerned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorInfo {
    /// The proposing actor.
    pub actor: String,
    /// The author's operators, nearest first, then theirs: everything above the author in its chain.
    pub operator_chain: Vec<String>,
    /// The harness that ran the author, for example `claude-code` or `pi`.
    pub harness: String,
    /// The model the author ran, for example `claude-opus-4-1`; a harness such as pi can run any vendor's.
    pub model: String,
}

/// The reviewer, as far as independence is concerned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewerInfo {
    /// The reviewer actor.
    pub actor: String,
    /// The reviewer's operators, nearest first, then theirs.
    pub operator_chain: Vec<String>,
    /// The model it reviews with.
    pub model: String,
}

/// Why a review is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// The reviewer's model and the author's harness or model are of one vendor.
    #[error("same vendor: reviewer model {reviewer} is {vendor}, as is the author's {of}")]
    SameVendor {
        /// The shared vendor.
        vendor: String,
        /// The reviewer's model.
        reviewer: String,
        /// Which of the author's it matches: `harness` or `model`.
        of: &'static str,
    },
    /// A vendor could not be told from the name, so independence can't be shown.
    #[error("unknown vendor of {what} `{name}`")]
    UnknownVendor {
        /// What was being classified: `reviewer model`, `author harness` or `author model`.
        what: &'static str,
        /// The name.
        name: String,
    },
    /// The reviewer actor is the author or one of its operators.
    #[error("the reviewer {0} is the author or in its operator chain")]
    ReviewerIsAuthorSide(String),
    /// An operator of the reviewer is the author or in the author's operator chain.
    #[error("the reviewer's operator {0} is the author or in its operator chain")]
    SharedOperator(String),
}

/// The vendor of a model or harness name, lower case, or `None` when unknown.
///
/// Matches on well-known families and on a `provider/` prefix as OmniRoute and pi write it
/// (`agy/gemini-flash`, `anthropic/claude-sonnet`). `claude-code` and `claude` are Anthropic's harness;
/// a harness that can run any vendor's model (`pi`) has no vendor of its own, so only the model counts
/// for it ([`independent`] does not classify such a harness).
pub fn vendor_of(name: &str) -> Option<&'static str> {
    let n = name.trim().to_ascii_lowercase();
    let (prefix, rest) = n.split_once('/').unwrap_or(("", n.as_str()));
    for key in [prefix, rest] {
        let v = match key {
            "anthropic" | "claude" | "claude-code" => Some("anthropic"),
            "google" | "gemini" | "agy" | "antigravity" | "vertex" => Some("google"),
            "openai" | "codex" | "chatgpt" => Some("openai"),
            "xai" | "grok" => Some("xai"),
            "mistral" => Some("mistral"),
            "deepseek" => Some("deepseek"),
            _ => None,
        };
        if v.is_some() {
            return v;
        }
    }
    let families: [(&str, &str); 14] = [
        ("claude", "anthropic"),
        ("gemini", "google"),
        ("gemma", "google"),
        ("gpt", "openai"),
        ("o1", "openai"),
        ("o3", "openai"),
        ("o4", "openai"),
        ("codex", "openai"),
        ("grok", "xai"),
        ("mistral", "mistral"),
        ("codestral", "mistral"),
        ("deepseek", "deepseek"),
        ("qwen", "alibaba"),
        ("llama", "meta"),
    ];
    families
        .iter()
        .find(|(f, _)| rest.starts_with(f))
        .map(|(_, v)| *v)
}

/// Harnesses that run models of any vendor: they say nothing about the vendor.
fn neutral_harness(harness: &str) -> bool {
    matches!(
        harness.trim().to_ascii_lowercase().as_str(),
        "pi" | "pi-acp" | "acp"
    )
}

/// How much independence a review needs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Independence {
    /// The vendor rules and the operator-chain rule (PLAN.md §12). The default.
    #[default]
    Strict,
    /// The dogfood, where one person operates every agent: a different actor and a different vendor. Only
    /// the operator-chain rule is dropped; the vendor rules stay. A record made under it says so.
    DogfoodRelaxed,
}

/// `Ok` when the reviewer is independent of the author under `mode`, else the first refusal.
pub fn independent(
    author: &AuthorInfo,
    reviewer: &ReviewerInfo,
    mode: Independence,
) -> Result<(), Refusal> {
    let rv = vendor_of(&reviewer.model).ok_or_else(|| Refusal::UnknownVendor {
        what: "reviewer model",
        name: reviewer.model.clone(),
    })?;
    if !neutral_harness(&author.harness) {
        let hv = vendor_of(&author.harness).ok_or_else(|| Refusal::UnknownVendor {
            what: "author harness",
            name: author.harness.clone(),
        })?;
        if hv == rv {
            return Err(Refusal::SameVendor {
                vendor: rv.into(),
                reviewer: reviewer.model.clone(),
                of: "harness",
            });
        }
    }
    let mv = vendor_of(&author.model).ok_or_else(|| Refusal::UnknownVendor {
        what: "author model",
        name: author.model.clone(),
    })?;
    if mv == rv {
        return Err(Refusal::SameVendor {
            vendor: rv.into(),
            reviewer: reviewer.model.clone(),
            of: "model",
        });
    }
    if mode == Independence::DogfoodRelaxed {
        if reviewer.actor == author.actor {
            return Err(Refusal::ReviewerIsAuthorSide(reviewer.actor.clone()));
        }
        return Ok(());
    }
    let author_side = |a: &str| a == author.actor || author.operator_chain.iter().any(|o| o == a);
    if author_side(&reviewer.actor) {
        return Err(Refusal::ReviewerIsAuthorSide(reviewer.actor.clone()));
    }
    if let Some(op) = reviewer.operator_chain.iter().find(|o| author_side(o)) {
        return Err(Refusal::SharedOperator(op.clone()));
    }
    Ok(())
}

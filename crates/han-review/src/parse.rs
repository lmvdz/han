//! Reading the model's answer. Anything that is not exactly a [`Review`] is a `needs_human`: the reviewer
//! never guesses at what a model meant.

use std::collections::BTreeSet;

use crate::record::{Finding, Review, Severity, Verdict};

/// At most this many findings are kept (the rest are dropped, in order).
pub const MAX_FINDINGS: usize = 100;
/// A finding's text is cut to this many characters.
pub const MAX_FINDING_TEXT: usize = 2000;

/// A review that stands for "the model's answer could not be used": `needs_human` with one finding saying
/// why, on no particular path (`""`).
pub fn needs_human(reason: impl Into<String>) -> Review {
    Review {
        verdict: Verdict::NeedsHuman,
        findings: vec![Finding {
            path: String::new(),
            line: None,
            severity: Severity::Warning,
            text: reason.into(),
        }],
        confidence: 0,
    }
}

/// Parses the model's text as a [`Review`] about a changeset touching `paths`.
///
/// Accepts one JSON object, bare or in one fenced block. A finding on a path outside the changeset, an
/// `approve` that carries an `error` finding, a confidence over 100, or anything else off the schema gives
/// `needs_human`. Findings are capped ([`MAX_FINDINGS`], [`MAX_FINDING_TEXT`]).
pub fn parse_review(text: &str, paths: &BTreeSet<String>) -> Review {
    let body = unfence(text.trim());
    let mut r: Review = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => return needs_human(format!("the model's answer is not a review: {e}")),
    };
    if r.confidence > 100 {
        return needs_human("the model's confidence is over 100");
    }
    if let Some(f) = r.findings.iter().find(|f| !paths.contains(&f.path)) {
        return needs_human(format!(
            "the model reported a finding on `{}`, which the changeset does not touch",
            f.path.chars().take(200).collect::<String>()
        ));
    }
    if r.verdict == Verdict::Approve && r.findings.iter().any(|f| f.severity == Severity::Error) {
        return needs_human("the model approved with an error finding");
    }
    r.findings.truncate(MAX_FINDINGS);
    for f in &mut r.findings {
        if f.text.chars().count() > MAX_FINDING_TEXT {
            f.text = f.text.chars().take(MAX_FINDING_TEXT).collect();
        }
    }
    r
}

/// The inside of one fenced block, when the whole text is one; else the text.
fn unfence(t: &str) -> &str {
    let Some(rest) = t.strip_prefix("```") else {
        return t;
    };
    let Some(body) = rest.strip_suffix("```") else {
        return t;
    };
    match body.split_once('\n') {
        Some((tag, inner)) if tag.trim().is_empty() || tag.trim().eq_ignore_ascii_case("json") => {
            inner.trim()
        }
        _ => t,
    }
}

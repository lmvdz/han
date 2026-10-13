//! The actions: [`GitPush`], [`OpenPr`], [`HttpRequest`].

mod git_push;
mod http_request;
mod open_pr;

use std::sync::Arc;

pub use git_push::GitPush;
pub use http_request::{HttpRequest, is_public};
pub use open_pr::OpenPr;

use crate::{Action, PlanError};

/// Every built-in action.
pub fn builtin() -> Vec<Arc<dyn Action>> {
    vec![Arc::new(GitPush), Arc::new(OpenPr), Arc::new(HttpRequest)]
}

pub(crate) fn is_hex(s: &str, lens: &[usize]) -> bool {
    lens.contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// A branch name git accepts and HAN may move (no `..`, no control or special characters).
pub(crate) fn check_branch(branch: &str, prefixes: &[String]) -> Result<(), PlanError> {
    let bad = branch.is_empty()
        || branch.len() > 255
        || branch.starts_with('/')
        || branch.ends_with('/')
        || branch.ends_with(".lock")
        || branch.contains("..")
        || branch.contains("//")
        || branch.contains("@{")
        || branch
            .chars()
            .any(|c| c.is_control() || " ~^:?*[\\".contains(c));
    if bad {
        return Err(PlanError::Schema(format!("not a branch name: {branch:?}")));
    }
    if !prefixes.iter().any(|p| branch.starts_with(p.as_str())) {
        return Err(PlanError::OutOfScope(format!(
            "branch {branch} is outside {}",
            prefixes.join(", ")
        )));
    }
    Ok(())
}

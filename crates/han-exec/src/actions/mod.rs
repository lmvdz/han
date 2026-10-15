//! The actions: [`GitPush`], [`VcsLand`], [`OpenPr`], [`HttpRequest`].

mod git_push;
mod http_request;
mod land;
mod open_pr;

use std::sync::Arc;

pub use git_push::GitPush;
pub use http_request::{HttpRequest, is_public};
pub use land::{REF_MOVED, VcsLand, change_digest, commit_title, depends_cycle};
pub use open_pr::OpenPr;
pub(crate) use open_pr::ensure_pr;

use crate::{Action, ActionCtx, PlanError};

/// Every built-in action.
pub fn builtin() -> Vec<Arc<dyn Action>> {
    vec![
        Arc::new(GitPush),
        Arc::new(VcsLand),
        Arc::new(OpenPr),
        Arc::new(HttpRequest),
    ]
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

/// The write credential for the plan's repository: an installation token scoped to it with `contents: write`
/// (SR-67), or none for a repository that isn't on GitHub.
pub(crate) async fn write_auth(
    plan: &serde_json::Value,
    ctx: &ActionCtx,
) -> Result<Option<han_vcs::RemoteAuth>, String> {
    let repo = ctx
        .config
        .repo(crate::plan_target(plan))
        .ok_or("unknown repository")?;
    let Some(gh) = &repo.github else {
        return Ok(None);
    };
    let minter = ctx.github.as_ref().ok_or("no GitHub App is configured")?;
    let token = minter.token(gh, &[("contents", "write")]).await?;
    Ok(Some(han_vcs::RemoteAuth::new(
        "x-access-token",
        token.expose(),
    )))
}

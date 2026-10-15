//! `open_pr`: open a pull request on GitHub (octocrab), as desired state: an open PR for the head branch is
//! looked for first and counts as done (SR-69). The installation token is minted for this one repository with
//! `pull_requests: write` only (SR-67).
//!
//! Plan: `{action: "open_pr", target: <repo name>, args: {head, base, title, body?}}`.

use serde::Deserialize;
use serde_json::{Value, json};

use super::check_branch;
use crate::{Action, ActionCtx, ExecConfig, IdempotencyKey, Outcome, PlanError, plan_args};

/// The action.
pub struct OpenPr;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    head: String,
    base: String,
    title: String,
    #[serde(default)]
    body: Option<String>,
}

fn args(plan: &Value) -> Result<Args, PlanError> {
    serde_json::from_value(plan_args(plan, "open_pr")?)
        .map_err(|e| PlanError::Schema(e.to_string()))
}

enum Found {
    Open(String),
    None,
}

impl OpenPr {
    async fn client(
        &self,
        plan: &Value,
        ctx: &ActionCtx,
    ) -> Result<(octocrab::Octocrab, String), String> {
        let repo = ctx
            .config
            .repo(crate::plan_target(plan))
            .ok_or("unknown repository")?;
        let gh = repo
            .github
            .clone()
            .ok_or("the repository is not on GitHub")?;
        let minter = ctx.github.as_ref().ok_or("no GitHub App is configured")?;
        let octo = minter.client(&gh, &[("pull_requests", "write")]).await?;
        Ok((octo, gh))
    }

    async fn find(&self, octo: &octocrab::Octocrab, gh: &str, head: &str) -> Result<Found, String> {
        find_pr(octo, gh, head).await
    }
}

/// The open pull request for `head` on `gh` (`owner/name`), if there is one.
async fn find_pr(octo: &octocrab::Octocrab, gh: &str, head: &str) -> Result<Found, String> {
    let owner = gh.split('/').next().unwrap_or_default();
    let route = format!("/repos/{gh}/pulls?state=open&head={owner}:{head}");
    let list: Value = octo
        .get(route, None::<&()>)
        .await
        .map_err(|_| "listing pull requests failed".to_string())?;
    Ok(match list.as_array().and_then(|a| a.first()) {
        Some(pr) => Found::Open(describe(pr)),
        None => Found::None,
    })
}

/// A pull request for `head` into `base` on `gh` (`owner/name`): the open one when there is one (a push to
/// `head` already updated it), else a new one. Returns what it found or opened, as `pull request #N URL`.
/// The landing lane calls this for the branch it pushes the commit under test to.
pub(crate) async fn ensure_pr(
    minter: &crate::github::GithubMinter,
    gh: &str,
    head: &str,
    base: &str,
    title: &str,
    body: &str,
) -> Result<String, String> {
    let octo = minter.client(gh, &[("pull_requests", "write")]).await?;
    if let Found::Open(d) = find_pr(&octo, gh, head).await? {
        return Ok(d);
    }
    let req = json!({"title": title, "head": head, "base": base, "body": body});
    match octo.post(format!("/repos/{gh}/pulls"), Some(&req)).await {
        Ok::<Value, _>(pr) => Ok(describe(&pr)),
        // The request went out; whether GitHub made the pull request is known from a lookup.
        Err(_) => match find_pr(&octo, gh, head).await? {
            Found::Open(d) => Ok(d),
            Found::None => Err("creating the pull request failed".into()),
        },
    }
}

fn describe(pr: &Value) -> String {
    format!(
        "pull request #{} {}",
        pr.get("number").and_then(Value::as_u64).unwrap_or(0),
        pr.get("html_url").and_then(Value::as_str).unwrap_or("")
    )
}

#[async_trait::async_trait]
impl Action for OpenPr {
    fn kind(&self) -> &'static str {
        "open_pr"
    }

    fn plan_check(&self, plan: &Value, config: &ExecConfig) -> Result<(), PlanError> {
        let a = args(plan)?;
        let target = crate::plan_target(plan);
        let repo = config
            .repo(target)
            .ok_or_else(|| PlanError::OutOfScope(format!("repository {target}")))?;
        if repo.github.is_none() {
            return Err(PlanError::OutOfScope(format!("{target} is not on GitHub")));
        }
        check_branch(&a.head, &config.branch_prefixes)?;
        check_branch(&a.base, &[String::new()])?;
        if a.title.trim().is_empty() || a.title.len() > 256 {
            return Err(PlanError::Schema("a title of 1 to 256 bytes".into()));
        }
        if a.body.as_ref().is_some_and(|b| b.len() > 65_536) {
            return Err(PlanError::Schema("the body is over 64 KiB".into()));
        }
        Ok(())
    }

    async fn preflight(&self, _plan: &Value, _ctx: &ActionCtx) -> Result<(), PlanError> {
        Ok(())
    }

    async fn run(&self, plan: &Value, _key: &IdempotencyKey, ctx: &ActionCtx) -> Outcome {
        let Ok(a) = args(plan) else {
            return Outcome::Failed {
                started: false,
                detail: "plan".into(),
            };
        };
        let (octo, gh) = match self.client(plan, ctx).await {
            Ok(c) => c,
            Err(e) => {
                return Outcome::Failed {
                    started: false,
                    detail: format!("credential: {e}"),
                };
            }
        };
        match self.find(&octo, &gh, &a.head).await {
            Ok(Found::Open(d)) => {
                return Outcome::Succeeded {
                    detail: format!("found {d}"),
                };
            }
            Ok(Found::None) => {}
            Err(e) => {
                return Outcome::Failed {
                    started: false,
                    detail: e,
                };
            }
        }
        let body = json!({"title": a.title, "head": a.head, "base": a.base, "body": a.body.unwrap_or_default()});
        let created: Result<Value, _> = octo.post(format!("/repos/{gh}/pulls"), Some(&body)).await;
        match created {
            Ok(pr) => Outcome::Succeeded {
                detail: format!("opened {}", describe(&pr)),
            },
            // The request went out; whether GitHub made the PR is unknown until a lookup.
            Err(_) => match self.find(&octo, &gh, &a.head).await {
                Ok(Found::Open(d)) => Outcome::Succeeded {
                    detail: format!("found {d}"),
                },
                _ => Outcome::Unknown {
                    detail: "creating the pull request failed".into(),
                },
            },
        }
    }

    async fn lookup(
        &self,
        plan: &Value,
        _key: &IdempotencyKey,
        ctx: &ActionCtx,
    ) -> Option<Outcome> {
        let a = args(plan).ok()?;
        let (octo, gh) = self.client(plan, ctx).await.ok()?;
        match self.find(&octo, &gh, &a.head).await.ok()? {
            Found::Open(d) => Some(Outcome::Succeeded {
                detail: format!("found {d}"),
            }),
            Found::None => None,
        }
    }
}

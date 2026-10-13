//! `git_push`: move `refs/heads/<branch>` on the target's remote from `from` to `to` by compare-and-swap
//! (desired state, SR-69), after checking that `base..to` is exactly the approved changeset.
//!
//! Plan: `{action: "git_push", target: <repo name>, args: {branch, base, from, to, changeset, source,
//! changes?}}`:
//! * `branch`: the branch, under a configured prefix (default `han/`);
//! * `base`: the commit the session started from; `from`: where the remote branch must point now (`null`:
//!   it must not exist); `to`: the commit to move it to;
//! * `changeset`: the approved [`han_vcs::ChangeSet::digest`] of `base..to`;
//! * `source`: the session workspace (a repository holding `to`);
//! * `changes`: optionally the [`han_vcs::ChangeSummary`] gates read (its digest must be `changeset`).

use serde::Deserialize;
use serde_json::Value;

use super::{check_branch, is_hex};
use crate::{Action, ActionCtx, ExecConfig, IdempotencyKey, Outcome, PlanError, plan_args};

/// The action.
pub struct GitPush;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Args {
    pub branch: String,
    pub base: String,
    pub from: Option<String>,
    pub to: String,
    pub changeset: String,
    pub source: String,
    #[serde(default)]
    pub changes: Option<han_vcs::ChangeSummary>,
}

pub(crate) fn args(plan: &Value) -> Result<Args, PlanError> {
    serde_json::from_value(plan_args(plan, "git_push")?)
        .map_err(|e| PlanError::Schema(e.to_string()))
}

fn refname(branch: &str) -> String {
    format!("refs/heads/{branch}")
}

impl GitPush {
    async fn auth(
        &self,
        plan: &Value,
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
}

#[async_trait::async_trait]
impl Action for GitPush {
    fn kind(&self) -> &'static str {
        "git_push"
    }

    fn plan_check(&self, plan: &Value, config: &ExecConfig) -> Result<(), PlanError> {
        let a = args(plan)?;
        let target = crate::plan_target(plan);
        if config.repo(target).is_none() {
            return Err(PlanError::OutOfScope(format!("repository {target}")));
        }
        check_branch(&a.branch, &config.branch_prefixes)?;
        for (name, sha) in [("base", &a.base), ("to", &a.to)] {
            if !is_hex(sha, &[40, 64]) {
                return Err(PlanError::Schema(format!("{name} is not a commit id")));
            }
        }
        if let Some(f) = &a.from
            && !is_hex(f, &[40, 64])
        {
            return Err(PlanError::Schema("from is not a commit id".into()));
        }
        if !is_hex(&a.changeset, &[64]) {
            return Err(PlanError::Schema("changeset is not a digest".into()));
        }
        if a.source.is_empty() {
            return Err(PlanError::Schema("source is empty".into()));
        }
        if let Some(c) = &a.changes
            && c.digest != a.changeset
        {
            return Err(PlanError::Changeset(
                "the summary names another changeset".into(),
            ));
        }
        Ok(())
    }

    async fn preflight(&self, plan: &Value, ctx: &ActionCtx) -> Result<(), PlanError> {
        let a = args(plan)?;
        let cs = ctx
            .vcs
            .changeset_between(std::path::Path::new(&a.source), &a.base, &a.to)
            .map_err(|e| PlanError::Preflight(e.to_string()))?;
        if cs.digest != a.changeset {
            return Err(PlanError::Changeset(format!(
                "{}..{} is not the approved changeset",
                &a.base[..8],
                &a.to[..8]
            )));
        }
        Ok(())
    }

    async fn run(&self, plan: &Value, _key: &IdempotencyKey, ctx: &ActionCtx) -> Outcome {
        let Ok(a) = args(plan) else {
            return Outcome::Failed {
                started: false,
                detail: "plan".into(),
            };
        };
        let Some(repo) = ctx.config.repo(crate::plan_target(plan)) else {
            return Outcome::Failed {
                started: false,
                detail: "unknown repository".into(),
            };
        };
        let auth = match self.auth(plan, ctx).await {
            Ok(a) => a,
            Err(e) => {
                return Outcome::Failed {
                    started: false,
                    detail: format!("credential: {e}"),
                };
            }
        };
        let spec = han_vcs::LandSpec {
            remote: repo.remote.clone(),
            refname: refname(&a.branch),
            expected: a.from.clone(),
            new: a.to.clone(),
        };
        match ctx
            .vcs
            .land(std::path::Path::new(&a.source), &spec, auth.as_ref())
        {
            Ok(han_vcs::LandOutcome::Landed) => Outcome::Succeeded {
                detail: format!("{} moved to {}", spec.refname, a.to),
            },
            Ok(han_vcs::LandOutcome::AlreadyThere) => Outcome::Succeeded {
                detail: format!("{} already at {}", spec.refname, a.to),
            },
            Ok(han_vcs::LandOutcome::Conflict { actual }) => Outcome::Failed {
                started: true,
                detail: format!(
                    "{} is at {}, not the expected {}; nothing forced",
                    spec.refname,
                    actual.as_deref().unwrap_or("(absent)"),
                    a.from.as_deref().unwrap_or("(absent)")
                ),
            },
            Err(e) => Outcome::Unknown {
                detail: format!("push: {e}"),
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
        let repo = ctx.config.repo(crate::plan_target(plan))?;
        let auth = self.auth(plan, ctx).await.ok()?;
        let now = ctx
            .vcs
            .remote_ref(
                std::path::Path::new(&a.source),
                &repo.remote,
                &refname(&a.branch),
                auth.as_ref(),
            )
            .ok()?;
        (now.as_deref() == Some(a.to.as_str())).then(|| Outcome::Succeeded {
            detail: format!("found {} at {}", refname(&a.branch), a.to),
        })
    }
}

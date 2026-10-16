//! `vcs.try` (R33, P-17 "Check-only try"): run the landing lane's fast checks on a change, and land nothing. The
//! lane squash-rebases the approved change onto the target ref's tip and tests that exact commit, as it does for
//! `vcs.land`; this action only verifies that what the lane tested is the change the plan names, and reports
//! `succeeded` with the checks' record as the outcome's evidence. No ref moves, no commit reaches the target.
//!
//! Plan: `{action: "vcs.try", target: <repo name>, args: {ref, base, to, deltaDigest, change?, title?, bundleBase?},
//! argsBlob}`, a `vcs.land` plan without `tier`, `rebase`, `dependsOn` and `rebaseOf` (a try is judged on the tip
//! as it is, path-disjoint or not it conflicts or it does not, the same as a land would) and without a
//! `change.revertOf`: a revert lands, it is not tried. `ref` is one of the repository's
//! [`crate::RepoTarget::landing_refs`], the ref the change would land on.
//!
//! The lane's instruction is handed over as for a land ([`crate::LaneInstruction`]): `run` checks that `commit` is
//! one commit on `onto` and that `delta(onto, commit)` is the plan's `deltaDigest`, and refuses to run without it.

use serde::Deserialize;
use serde_json::Value;

use super::land::{change_of, short};
use super::{check_branch, is_hex};
use crate::{Action, ActionCtx, ExecConfig, IdempotencyKey, Outcome, PlanError, plan_args_with};

/// The action.
pub struct VcsTry;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Args {
    #[serde(rename = "ref")]
    refname: String,
    base: String,
    to: String,
    delta_digest: String,
    #[serde(default)]
    change: Option<Value>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    bundle_base: Option<String>,
}

fn args(plan: &Value) -> Result<Args, PlanError> {
    serde_json::from_value(plan_args_with(plan, "vcs.try", true)?)
        .map_err(|e| PlanError::Schema(e.to_string()))
}

/// The commit subject of a plan's try: its `title`, or a line naming the change.
pub fn try_title(plan: &Value) -> String {
    let a = args(plan).ok();
    match a.as_ref().and_then(|a| a.title.as_deref()) {
        Some(t) if !t.trim().is_empty() => t.trim().to_owned(),
        _ => match a {
            Some(a) => format!(
                "try {}..{} on {}",
                short(&a.base),
                short(&a.to),
                a.refname.strip_prefix("refs/heads/").unwrap_or(&a.refname)
            ),
            None => "try".into(),
        },
    }
}

fn not_started(detail: impl Into<String>) -> Outcome {
    Outcome::Failed {
        started: false,
        detail: detail.into(),
    }
}

fn tried(lane: &crate::LaneInstruction) -> Outcome {
    Outcome::Succeeded {
        detail: format!(
            "tried: the checks passed on {} (one commit on {}); nothing was landed",
            lane.commit, lane.onto
        ),
    }
}

/// Every check on the lane's instruction against the plan; nothing is written. `Err` is the reason.
fn verify(a: &Args, lane: &crate::LaneInstruction, ctx: &ActionCtx) -> Result<(), String> {
    for (name, sha) in [("onto", &lane.onto), ("commit", &lane.commit)] {
        if !is_hex(sha, &[40, 64]) {
            return Err(format!("{name} is not a commit id"));
        }
    }
    let dir = &lane.repository;
    let parents = ctx
        .vcs
        .parents(dir, &lane.commit)
        .map_err(|e| format!("reading the commit: {e}"))?;
    if parents != [lane.onto.clone()] {
        return Err(format!(
            "{} is not one commit on {}",
            short(&lane.commit),
            short(&lane.onto)
        ));
    }
    let tested = ctx
        .vcs
        .delta(dir, &lane.onto, &lane.commit)
        .map_err(|e| e.to_string())
        .and_then(|d| d.digest().map_err(|e| e.to_string()))
        .map_err(|e| format!("delta of the commit: {e}"))?;
    if tested != a.delta_digest {
        return Err(format!(
            "{} is not the change the plan names (delta {} is not {})",
            short(&lane.commit),
            short(&tested),
            short(&a.delta_digest)
        ));
    }
    Ok(())
}

impl VcsTry {
    /// The executor's repository holding `base..to`, from the plan's bundle (as `vcs.land` does).
    async fn import(
        &self,
        plan: &Value,
        a: &Args,
        ctx: &ActionCtx,
    ) -> Result<std::path::PathBuf, PlanError> {
        let bytes = ctx
            .args_blob
            .as_ref()
            .ok_or_else(|| PlanError::Preflight("the changeset's bundle is not at hand".into()))?;
        let repo = ctx
            .config
            .repo(crate::plan_target(plan))
            .ok_or_else(|| PlanError::OutOfScope("unknown repository".into()))?;
        let auth = super::write_auth(plan, ctx)
            .await
            .map_err(|e| PlanError::Preflight(format!("credential: {e}")))?;
        let needs = a.bundle_base.as_deref().unwrap_or(&a.base);
        ctx.vcs
            .import_bundle(&repo.remote, auth.as_ref(), bytes, needs, &a.to)
            .map_err(|e| match e {
                han_vcs::VcsError::Invalid(m) => PlanError::Changeset(m),
                other => PlanError::Preflight(other.to_string()),
            })
    }
}

#[async_trait::async_trait]
impl Action for VcsTry {
    fn kind(&self) -> &'static str {
        "vcs.try"
    }

    fn plan_check(&self, plan: &Value, config: &ExecConfig) -> Result<(), PlanError> {
        let a = args(plan)?;
        let target = crate::plan_target(plan);
        let repo = config
            .repo(target)
            .ok_or_else(|| PlanError::OutOfScope(format!("repository {target}")))?;
        let Some(branch) = a.refname.strip_prefix("refs/heads/") else {
            return Err(PlanError::Schema(format!(
                "not a branch ref: {:?}",
                a.refname
            )));
        };
        check_branch(branch, &[String::new()])?;
        if !repo.landing_refs.contains(&a.refname) {
            return Err(PlanError::OutOfScope(format!(
                "ref {} is not a landing ref",
                a.refname
            )));
        }
        for (name, sha) in [("base", &a.base), ("to", &a.to)]
            .into_iter()
            .chain(a.bundle_base.as_ref().map(|b| ("bundleBase", b)))
        {
            if !is_hex(sha, &[40, 64]) {
                return Err(PlanError::Schema(format!("{name} is not a commit id")));
            }
        }
        if !is_hex(&a.delta_digest, &[64]) {
            return Err(PlanError::Schema("deltaDigest is not a digest".into()));
        }
        if let Some(c) = &a.change {
            let change = change_of(c)?;
            if change.repo != target {
                return Err(PlanError::Schema(format!(
                    "the change summary is for {}, not {target}",
                    change.repo
                )));
            }
            if change.delta_digest != a.delta_digest {
                return Err(PlanError::Schema(
                    "the change summary's deltaDigest is not the plan's".into(),
                ));
            }
            if change.revert_of.is_some() {
                return Err(PlanError::Schema("a revert lands; it is not tried".into()));
            }
        }
        if let Some(t) = &a.title
            && (t.trim().is_empty() || t.len() > 256 || t.chars().any(char::is_control))
        {
            return Err(PlanError::Schema(
                "a title of 1 to 256 bytes on one line".into(),
            ));
        }
        match crate::args_blob(plan)? {
            Some(b) if b.media_type == han_vcs::BUNDLE_MEDIA_TYPE => Ok(()),
            Some(_) => Err(PlanError::Schema(format!(
                "argsBlob is not a {}",
                han_vcs::BUNDLE_MEDIA_TYPE
            ))),
            None => Err(PlanError::Schema("no argsBlob".into())),
        }
    }

    fn lane_only(&self) -> bool {
        true
    }

    async fn preflight(&self, plan: &Value, ctx: &ActionCtx) -> Result<(), PlanError> {
        let a = args(plan)?;
        let dir = self.import(plan, &a, ctx).await?;
        let delta = ctx
            .vcs
            .delta(&dir, &a.base, &a.to)
            .map_err(|e| PlanError::Preflight(e.to_string()))?;
        let digest = delta
            .digest()
            .map_err(|e| PlanError::Preflight(e.to_string()))?;
        if digest != a.delta_digest {
            return Err(PlanError::Changeset(format!(
                "{}..{} is not the change the plan names",
                short(&a.base),
                short(&a.to)
            )));
        }
        if let Some(c) = &a.change {
            let claimed = change_of(c)?;
            let lines = ctx
                .vcs
                .delta_lines(&dir, &a.base, &a.to, &delta)
                .map_err(|e| PlanError::Preflight(e.to_string()))?;
            han_vcs::check_land_change(
                &claimed,
                crate::plan_target(plan),
                &a.delta_digest,
                &delta,
                &lines,
            )
            .map_err(|why| PlanError::Changeset(format!("the change summary: {why}")))?;
        }
        Ok(())
    }

    /// Verifies the lane's instruction against the plan, as `vcs.land` would before it moves the ref, and
    /// reports `succeeded`. It writes nothing: no ref moves and nothing is pushed.
    async fn run(&self, plan: &Value, _key: &IdempotencyKey, ctx: &ActionCtx) -> Outcome {
        let Ok(a) = args(plan) else {
            return not_started("plan");
        };
        let Some(lane) = &ctx.lane else {
            return not_started("no lane instruction: only a landing lane may claim vcs.try");
        };
        match verify(&a, lane, ctx) {
            Ok(()) => tried(lane),
            Err(why) => not_started(why),
        }
    }

    /// A try changes nothing in the world. The lane claims only after its checks are green, so after a restart
    /// the instruction the journal re-attached, if it still checks out against the plan, is the answer.
    async fn lookup(
        &self,
        plan: &Value,
        _key: &IdempotencyKey,
        ctx: &ActionCtx,
    ) -> Option<Outcome> {
        let a = args(plan).ok()?;
        let lane = ctx.lane.as_ref()?;
        verify(&a, lane, ctx).ok().map(|()| tried(lane))
    }
}

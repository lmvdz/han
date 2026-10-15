//! `vcs.land` (R33): move a protected target ref from the tip a landing lane tested on to the commit it
//! tested, by compare-and-swap, with the write credential. What is tested is exactly what lands.
//!
//! Plan: `{action: "vcs.land", target: <repo name>, args: {ref, base, to, deltaDigest, tier, rebase,
//! dependsOn?, rebaseOf?}, argsBlob}`:
//! * `ref`: the full ref to move, one of the repository's [`crate::RepoTarget::landing_refs`] (default none);
//! * `base`, `to`: the approved change is `base..to`; `deltaDigest` is its [`han_vcs::Delta`] digest, which
//!   does not depend on the base. The approval binds the plan, so these are what a person approved;
//! * `tier`: the risk tier 0 to 3, informative (Policy derives its own);
//! * `change`: the summary Policy derives the tier from (architecture.md P-17), optional: `{repo, deltaDigest,
//!   total, files, truncated?}`. The executor checks it against the bundle (X-13) before it claims, and the
//!   claim checks that it is the one the proposal's body carries ([`crate::executor::check_plan`]);
//! * `rebase`: `"path-disjoint"` lets the lane land the change on a later tip of `ref` when the delta is
//!   path-disjoint from it; `"none"` lands only on `base` itself;
//! * `dependsOn`: other proposals (`{repo, proposal}`) to land first; ordering is the lane's job, the plan
//!   check refuses what is cyclic on its face (see [`depends_cycle`] for the lane's graph);
//! * `rebaseOf`: the proposal this one re-proposes after a failed path-disjoint check;
//! * `title`: the subject line of the commit the lane makes (the approval binds it); default a line naming
//!   the change;
//! * `bundleBase`: the commit the bundle needs, one the executor can get from the remote; default `base`. A
//!   proposal that depends on another has the other's `to` as its `base`, and the lane squashes, so that `to`
//!   never reaches the remote. The host bundles from the session's first base, `bundleBase..to`, which holds
//!   `base` and imports on any executor, after a restart or on another machine;
//! * `argsBlob`: the commits `bundleBase..to` (`base..to` without `bundleBase`) as a git bundle, imported as
//!   `git_push` does (R15).
//!
//! The lane's rebase happens after approval, so the executor gets it out of the plan: the claim context
//! carries a [`crate::LaneInstruction`] (`onto`, `commit` and the repository that holds them). Before the
//! ref moves, `run` checks that `commit` is one commit on `onto`; that `delta(base, to)` is path-disjoint
//! from `onto` (or `onto == base` for `rebase: "none"`); and that `delta(onto, commit)` is the approved
//! `deltaDigest`. Then it moves `ref` from `onto` to `commit`, never forced. A lost compare-and-swap is
//! `Failed { started: true }` whose detail starts with [`REF_MOVED`]: the lane starts again from the new tip. The
//! success detail records the landed base, commit and landed (`changeset_digest`) digest.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::Value;

use super::{check_branch, is_hex};
use crate::{Action, ActionCtx, ExecConfig, IdempotencyKey, Outcome, PlanError, plan_args_with};

/// The action.
pub struct VcsLand;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Dep {
    repo: String,
    proposal: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Args {
    #[serde(rename = "ref")]
    refname: String,
    base: String,
    to: String,
    delta_digest: String,
    tier: u8,
    rebase: Rebase,
    #[serde(default)]
    change: Option<Value>,
    #[serde(default)]
    depends_on: Vec<Dep>,
    #[serde(default)]
    rebase_of: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    bundle_base: Option<String>,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
enum Rebase {
    #[serde(rename = "path-disjoint")]
    PathDisjoint,
    #[serde(rename = "none")]
    None,
}

/// What the detail of a lost compare-and-swap starts with: the ref is not where the lane tested, and nothing
/// was forced. The landing lane starts again from the new tip.
pub const REF_MOVED: &str = "ref moved";

/// The commit subject of a plan: its `title`, or a line naming the change.
pub fn commit_title(plan: &Value) -> String {
    let a = args(plan).ok();
    match a.as_ref().and_then(|a| a.title.as_deref()) {
        Some(t) if !t.trim().is_empty() => t.trim().to_owned(),
        _ => match a {
            Some(a) => format!(
                "land {}..{} on {}",
                short(&a.base),
                short(&a.to),
                a.refname.strip_prefix("refs/heads/").unwrap_or(&a.refname)
            ),
            None => "land".into(),
        },
    }
}

/// The commit a plan's bundle needs: its `bundleBase`, else its `base` (empty when the plan has neither).
pub fn bundle_base(plan: &Value) -> String {
    let a = plan.get("args");
    ["bundleBase", "base"]
        .iter()
        .find_map(|k| a.and_then(|a| a.get(*k)).and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned()
}

/// The plan's `change` as a valid summary (architecture.md P-17): the shape and the bounds Policy checks.
fn change_of(c: &Value) -> Result<han_vcs::Change, PlanError> {
    let text = serde_json::to_string(c).map_err(|e| PlanError::Schema(e.to_string()))?;
    let v = han_core::json::parse(text.as_bytes())
        .map_err(|e| PlanError::Schema(format!("the change summary: {e}")))?;
    if !han_core::policy::change_summary(&v) {
        return Err(PlanError::Schema(
            "the change summary is not a valid summary (at most 64 files in byte order, 12,288 bytes)"
                .into(),
        ));
    }
    serde_json::from_value(c.clone()).map_err(|e| PlanError::Schema(e.to_string()))
}

/// The digest of a plan's `args.change`: what the proposal's body must carry (X-13). `None` when the plan has
/// none or it is not Json the protocol can digest.
pub fn change_digest(plan: &Value) -> Option<String> {
    let c = plan.get("args")?.get("change")?;
    let text = serde_json::to_string(c).ok()?;
    let v = han_core::json::parse(text.as_bytes()).ok()?;
    han_core::ids::json_digest(&v).ok()
}

fn args(plan: &Value) -> Result<Args, PlanError> {
    serde_json::from_value(plan_args_with(plan, "vcs.land", true)?)
        .map_err(|e| PlanError::Schema(e.to_string()))
}

/// A cycle in a graph of proposals (`proposal -> proposals it depends on`), if there is one: the proposals on
/// it, in order, the first repeated at the end. The plan check can see only one plan, so the lane runs this
/// over everything it knows before it orders a batch.
pub fn depends_cycle(edges: &BTreeMap<String, Vec<String>>) -> Option<Vec<String>> {
    fn visit(
        n: &str,
        edges: &BTreeMap<String, Vec<String>>,
        path: &mut Vec<String>,
        done: &mut BTreeSet<String>,
    ) -> Option<Vec<String>> {
        if let Some(i) = path.iter().position(|p| p == n) {
            let mut c = path[i..].to_vec();
            c.push(n.to_owned());
            return Some(c);
        }
        if done.contains(n) {
            return None;
        }
        path.push(n.to_owned());
        for m in edges.get(n).into_iter().flatten() {
            if let Some(c) = visit(m, edges, path, done) {
                return Some(c);
            }
        }
        path.pop();
        done.insert(n.to_owned());
        None
    }
    let mut done = BTreeSet::new();
    edges
        .keys()
        .find_map(|n| visit(n, edges, &mut Vec::new(), &mut done))
}

fn short(s: &str) -> &str {
    &s[..s.len().min(8)]
}

fn not_started(detail: impl Into<String>) -> Outcome {
    Outcome::Failed {
        started: false,
        detail: detail.into(),
    }
}

impl VcsLand {
    /// The executor's repository holding `base..to`, from the plan's bundle (as `git_push` does).
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

    /// Every check on the lane's instruction against the plan; nothing is written. `Err` is the reason.
    fn verify(
        &self,
        a: &Args,
        lane: &crate::LaneInstruction,
        ctx: &ActionCtx,
    ) -> Result<(han_vcs::Delta, String), String> {
        for (name, sha) in [("onto", &lane.onto), ("commit", &lane.commit)] {
            if !is_hex(sha, &[40, 64]) {
                return Err(format!("{name} is not a commit id"));
            }
        }
        if a.rebase == Rebase::None && lane.onto != a.base {
            return Err("rebase is none, but onto is not the plan's base".into());
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
        let approved = ctx
            .vcs
            .delta(dir, &a.base, &a.to)
            .map_err(|e| format!("delta of the approved change: {e}"))?;
        if a.rebase == Rebase::PathDisjoint && lane.onto != a.base {
            let pd = ctx
                .vcs
                .path_disjoint(dir, &approved, &lane.onto)
                .map_err(|e| format!("path-disjoint check: {e}"))?;
            if !pd.ok {
                return Err(format!(
                    "the change is not path-disjoint from {}: {}",
                    short(&lane.onto),
                    pd.touched.join(", ")
                ));
            }
        }
        let landed = ctx
            .vcs
            .delta(dir, &lane.onto, &lane.commit)
            .map_err(|e| format!("delta of the commit: {e}"))?;
        let digest = landed.digest().map_err(|e| e.to_string())?;
        if digest != a.delta_digest {
            return Err(format!(
                "{} is not the approved change (delta {} is not {})",
                short(&lane.commit),
                short(&digest),
                short(&a.delta_digest)
            ));
        }
        let landed_digest = ctx
            .vcs
            .changeset_between(dir, &lane.onto, &lane.commit)
            .map(|c| c.digest)
            .map_err(|e| format!("landed digest: {e}"))?;
        Ok((landed, landed_digest))
    }
}

#[async_trait::async_trait]
impl Action for VcsLand {
    fn kind(&self) -> &'static str {
        "vcs.land"
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
        if a.tier > 3 {
            return Err(PlanError::Schema("tier is 0 to 3".into()));
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
        }
        // A plan sees only itself: what is a cycle on its face is a dependency named twice, or a
        // dependency on the proposal this one replaces (the lane checks its whole graph, `depends_cycle`).
        let mut seen = BTreeSet::new();
        for d in &a.depends_on {
            if d.repo.is_empty() || d.proposal.is_empty() {
                return Err(PlanError::Schema(
                    "a dependsOn entry names a proposal".into(),
                ));
            }
            if !seen.insert((&d.repo, &d.proposal)) {
                return Err(PlanError::Schema(format!(
                    "dependsOn names {} twice",
                    d.proposal
                )));
            }
            if a.rebase_of.as_deref() == Some(d.proposal.as_str()) {
                return Err(PlanError::Schema(format!(
                    "dependsOn cycle: the plan depends on {}, which it replaces",
                    d.proposal
                )));
            }
        }
        if a.rebase_of.as_deref() == Some("") {
            return Err(PlanError::Schema("rebaseOf is empty".into()));
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
                "{}..{} is not the approved change",
                short(&a.base),
                short(&a.to)
            )));
        }
        // X-13: what Policy derived the tier from is the change the bundle holds
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

    async fn run(&self, plan: &Value, _key: &IdempotencyKey, ctx: &ActionCtx) -> Outcome {
        let Ok(a) = args(plan) else {
            return not_started("plan");
        };
        let Some(repo) = ctx.config.repo(crate::plan_target(plan)) else {
            return not_started("unknown repository");
        };
        let Some(lane) = &ctx.lane else {
            return not_started("no lane instruction: only a landing lane may claim vcs.land");
        };
        let (_, landed_digest) = match self.verify(&a, lane, ctx) {
            Ok(v) => v,
            Err(why) => return not_started(why),
        };
        let auth = match super::write_auth(plan, ctx).await {
            Ok(a) => a,
            Err(e) => return not_started(format!("credential: {e}")),
        };
        let spec = han_vcs::LandSpec {
            remote: repo.remote.clone(),
            refname: a.refname.clone(),
            expected: Some(lane.onto.clone()),
            new: lane.commit.clone(),
        };
        let evidence = format!(
            "base {}, commit {}, landed digest {landed_digest}",
            lane.onto, lane.commit
        );
        match ctx.vcs.land(&lane.repository, &spec, auth.as_ref()) {
            Ok(han_vcs::LandOutcome::Landed) => Outcome::Succeeded {
                detail: format!("{} moved to {}; {evidence}", spec.refname, lane.commit),
            },
            Ok(han_vcs::LandOutcome::AlreadyThere) => Outcome::Succeeded {
                detail: format!("{} already at {}; {evidence}", spec.refname, lane.commit),
            },
            Ok(han_vcs::LandOutcome::Conflict { actual }) => Outcome::Failed {
                started: true,
                detail: format!(
                    "{REF_MOVED}: {} is at {}, not the tested tip {}; nothing forced, retry from the new tip",
                    spec.refname,
                    actual.as_deref().unwrap_or("(absent)"),
                    lane.onto
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
        let lane = ctx.lane.as_ref()?;
        let repo = ctx.config.repo(crate::plan_target(plan))?;
        let auth = super::write_auth(plan, ctx).await.ok()?;
        std::fs::create_dir_all(&ctx.scratch).ok()?;
        let now = ctx
            .vcs
            .remote_ref(&ctx.scratch, &repo.remote, &a.refname, auth.as_ref())
            .ok()?;
        (now.as_deref() == Some(lane.commit.as_str())).then(|| Outcome::Succeeded {
            detail: format!("found {} at {}", a.refname, lane.commit),
        })
    }
}

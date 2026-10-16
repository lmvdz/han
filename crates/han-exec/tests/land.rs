//! `vcs.land` against real git: a bare remote, a work repository and `GitVcs`. The lane (PR 3) is played by
//! the test: it squash-rebases the approved change onto the ref's tip and passes the result in the claim
//! context's `LaneInstruction`.

use han_proc::NoWindow as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use han_exec::actions::{VcsLand, VcsTry, depends_cycle};
use han_exec::{
    Action, ActionCtx, ExecConfig, IdempotencyKey, LaneInstruction, Outcome, PlanError, RepoTarget,
};
use han_vcs::{GitVcs, RebaseOutcome, Vcs};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const REF: &str = "refs/heads/main";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .no_window()
        .current_dir(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(["-c", "commit.gpgsign=false"])
        .args(["-c", "protocol.file.allow=always"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

struct World {
    _tmp: tempfile::TempDir,
    remote: PathBuf,
    work: PathBuf,
    vcs: Arc<GitVcs>,
    config: Arc<ExecConfig>,
    base: String,
    /// The approved change `base..to` edits `a.txt`.
    to: String,
}

impl World {
    fn new() -> World {
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("remote.git");
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&remote).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        git(&remote, &["init", "-q", "--bare", "-b", "main"]);
        git(&work, &["init", "-q", "-b", "main"]);
        std::fs::write(work.join("a.txt"), "a\n").unwrap();
        std::fs::write(work.join("b.txt"), "b\n").unwrap();
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "-q", "-m", "base"]);
        let base = git(&work, &["rev-parse", "HEAD"]);
        git(&work, &["push", "-q", remote.to_str().unwrap(), "main"]);
        git(&work, &["checkout", "-q", "-b", "change"]);
        std::fs::write(work.join("a.txt"), "a, edited\n").unwrap();
        git(&work, &["commit", "-q", "-am", "change"]);
        let to = git(&work, &["rev-parse", "HEAD"]);
        git(&work, &["checkout", "-q", "main"]);
        let config = ExecConfig {
            repos: vec![RepoTarget {
                landing_refs: vec![REF.into()],
                ..RepoTarget::new("local:origin", remote.to_string_lossy().into_owned())
            }],
            ..ExecConfig::default()
        };
        World {
            vcs: Arc::new(GitVcs::new(tmp.path().join("store"))),
            config: Arc::new(config),
            _tmp: tmp,
            remote,
            work,
            base,
            to,
        }
    }

    /// A commit on the remote's `main` (through the work repository), returning it.
    fn advance(&self, file: &str, content: &str) -> String {
        git(&self.work, &["checkout", "-q", "main"]);
        std::fs::write(self.work.join(file), content).unwrap();
        git(&self.work, &["add", "-A"]);
        git(&self.work, &["commit", "-q", "-m", "tip"]);
        git(
            &self.work,
            &["push", "-q", self.remote.to_str().unwrap(), "main"],
        );
        git(&self.work, &["rev-parse", "HEAD"])
    }

    fn tip(&self) -> String {
        git(&self.remote, &["rev-parse", REF])
    }

    fn delta_digest(&self) -> String {
        self.vcs
            .delta(&self.work, &self.base, &self.to)
            .unwrap()
            .digest()
            .unwrap()
    }

    fn bundle(&self) -> Vec<u8> {
        self.vcs.bundle(&self.work, &self.base, &self.to).unwrap()
    }

    fn plan(&self) -> Value {
        let b = self.bundle();
        json!({"action": "vcs.land", "target": "local:origin",
            "args": {"ref": REF, "base": self.base, "to": self.to, "deltaDigest": self.delta_digest(),
                     "tier": 1, "rebase": "path-disjoint"},
            "argsBlob": {"hash": Sha256::digest(&b).iter().map(|x| format!("{x:02x}")).collect::<String>(), "size": b.len(),
                         "type": han_vcs::BUNDLE_MEDIA_TYPE}})
    }

    /// The lane's work: the approved change squash-rebased onto `onto`.
    fn squash(&self, onto: &str) -> String {
        let RebaseOutcome::Rebased(r) = self
            .vcs
            .squash_rebase(
                &self.work,
                &self.base,
                &self.to,
                onto,
                "land",
                "lane",
                1_700_000_000,
            )
            .unwrap()
        else {
            panic!("conflict")
        };
        r.commit
    }

    fn ctx(&self, lane: Option<(&str, &str)>, blob: bool) -> ActionCtx {
        ActionCtx {
            config: self.config.clone(),
            vcs: self.vcs.clone(),
            scratch: self.work.join(".git").join("scratch"),
            github: None,
            args_blob: blob.then(|| Arc::new(self.bundle())),
            lane: lane.map(|(onto, commit)| LaneInstruction {
                onto: onto.into(),
                commit: commit.into(),
                repository: self.work.clone(),
                evidence: None,
            }),
        }
    }
}

fn key() -> IdempotencyKey {
    IdempotencyKey {
        space: "s".into(),
        proposal: "p".into(),
        attempt: 1,
    }
}

fn failed_unstarted(o: &Outcome, needle: &str) {
    match o {
        Outcome::Failed {
            started: false,
            detail,
        } if detail.contains(needle) => {}
        other => panic!("expected a refusal naming {needle:?}, got {other:?}"),
    }
}

#[tokio::test]
async fn lands_by_compare_and_swap_onto_a_later_disjoint_tip() {
    let w = World::new();
    let plan = w.plan();
    VcsLand.plan_check(&plan, &w.config).unwrap();
    VcsLand.preflight(&plan, &w.ctx(None, true)).await.unwrap();
    let onto = w.advance("b.txt", "b, moved on\n");
    let commit = w.squash(&onto);
    let ctx = w.ctx(Some((&onto, &commit)), true);
    assert!(VcsLand.lookup(&plan, &key(), &ctx).await.is_none());
    let o = VcsLand.run(&plan, &key(), &ctx).await;
    let Outcome::Succeeded { detail } = &o else {
        panic!("{o:?}")
    };
    assert!(
        detail.contains(&onto) && detail.contains(&commit),
        "{detail}"
    );
    assert_eq!(w.tip(), commit);
    assert_eq!(git(&w.remote, &["rev-parse", &format!("{commit}^")]), onto);
}

#[tokio::test]
async fn a_ref_that_moved_since_onto_fails_for_a_retry_and_nothing_is_forced() {
    let w = World::new();
    let plan = w.plan();
    let onto = w.advance("b.txt", "one\n");
    let commit = w.squash(&onto);
    let newer = w.advance("b.txt", "two\n");
    let o = VcsLand
        .run(&plan, &key(), &w.ctx(Some((&onto, &commit)), true))
        .await;
    match o {
        Outcome::Failed {
            started: true,
            detail,
        } => assert!(detail.contains("moved"), "{detail}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(w.tip(), newer, "the ref is untouched");
}

#[tokio::test]
async fn a_commit_whose_parent_is_not_onto_is_refused() {
    let w = World::new();
    let plan = w.plan();
    let onto = w.advance("b.txt", "one\n");
    let base = w.base.clone();
    let stale = w.squash(&base); // sits on base, not on the tip
    let o = VcsLand
        .run(&plan, &key(), &w.ctx(Some((&onto, &stale)), true))
        .await;
    failed_unstarted(&o, "not one commit on");
    assert_eq!(w.tip(), onto);
}

#[tokio::test]
async fn a_different_change_smuggled_in_is_refused_before_any_push() {
    let w = World::new();
    // Preflight: the plan names a delta digest the bundle does not have.
    let mut bad = w.plan();
    bad["args"]["deltaDigest"] = json!("e".repeat(64));
    assert!(matches!(
        VcsLand.preflight(&bad, &w.ctx(None, true)).await,
        Err(PlanError::Changeset(_))
    ));
    // Run: the lane's commit is a child of `onto` with the approved edit plus a file nobody approved.
    let plan = w.plan();
    let onto = w.advance("b.txt", "one\n");
    let good = w.squash(&onto);
    git(&w.work, &["checkout", "-q", "--detach", &good]);
    std::fs::write(w.work.join("evil.txt"), "x\n").unwrap();
    git(&w.work, &["add", "-A"]);
    git(&w.work, &["commit", "-q", "--amend", "-m", "smuggled"]);
    let smuggled = git(&w.work, &["rev-parse", "HEAD"]);
    git(&w.work, &["checkout", "-q", "main"]);
    let o = VcsLand
        .run(&plan, &key(), &w.ctx(Some((&onto, &smuggled)), true))
        .await;
    failed_unstarted(&o, "not the approved change");
    assert_eq!(w.tip(), onto);
}

#[tokio::test]
async fn a_failed_path_disjoint_check_is_refused() {
    let w = World::new();
    let plan = w.plan();
    // The tip changed a.txt too, so the approved change is not path-disjoint from it.
    let onto = w.advance("a.txt", "someone else\n");
    let tree = format!("{}^{{tree}}", w.to);
    let commit = git(&w.work, &["commit-tree", "-p", &onto, "-m", "x", &tree]);
    let o = VcsLand
        .run(&plan, &key(), &w.ctx(Some((&onto, &commit)), true))
        .await;
    failed_unstarted(&o, "not path-disjoint");
    assert_eq!(w.tip(), onto);
}

#[tokio::test]
async fn rebase_none_lands_only_on_the_base() {
    let w = World::new();
    let mut plan = w.plan();
    plan["args"]["rebase"] = json!("none");
    let onto = w.advance("b.txt", "one\n");
    let commit = w.squash(&onto);
    let o = VcsLand
        .run(&plan, &key(), &w.ctx(Some((&onto, &commit)), true))
        .await;
    failed_unstarted(&o, "onto is not the plan's base");
    assert_eq!(w.tip(), onto);
    // On the base it lands.
    let w = World::new();
    let mut plan = w.plan();
    plan["args"]["rebase"] = json!("none");
    let base = w.base.clone();
    let commit = w.squash(&base);
    let o = VcsLand
        .run(&plan, &key(), &w.ctx(Some((&base, &commit)), true))
        .await;
    assert!(matches!(o, Outcome::Succeeded { .. }), "{o:?}");
    assert_eq!(w.tip(), commit);
}

#[tokio::test]
async fn without_a_lane_instruction_nothing_runs() {
    let w = World::new();
    let o = VcsLand.run(&w.plan(), &key(), &w.ctx(None, true)).await;
    failed_unstarted(&o, "no lane instruction");
}

#[test]
fn plan_check_refuses_what_is_out_of_scope_or_malformed() {
    let w = World::new();
    let ok = w.plan();
    VcsLand.plan_check(&ok, &w.config).unwrap();
    // A ref outside the landing refs, and with none configured at all.
    let mut other = ok.clone();
    other["args"]["ref"] = json!("refs/heads/release");
    assert!(matches!(
        VcsLand.plan_check(&other, &w.config),
        Err(PlanError::OutOfScope(_))
    ));
    let mut none = (*w.config).clone();
    none.repos[0].landing_refs.clear();
    assert!(matches!(
        VcsLand.plan_check(&ok, &none),
        Err(PlanError::OutOfScope(_))
    ));
    // Unknown repository, bad ids and digests, bad rebase, unknown member, wrong blob type.
    type Edit = Box<dyn Fn(&mut Value)>;
    let edits: Vec<Edit> = vec![
        Box::new(|p| p["target"] = json!("nope")),
        Box::new(|p| p["args"]["to"] = json!("zz")),
        Box::new(|p| p["args"]["deltaDigest"] = json!("abc")),
        Box::new(|p| p["args"]["rebase"] = json!("merge")),
        Box::new(|p| p["args"]["tier"] = json!(4)),
        Box::new(|p| p["args"]["force"] = json!(true)),
        Box::new(|p| p["args"]["bundleBase"] = json!("main")),
        Box::new(|p| p["argsBlob"]["type"] = json!("text/plain")),
        Box::new(|p| {
            p.as_object_mut().unwrap().remove("argsBlob");
        }),
    ];
    for (i, edit) in edits.iter().enumerate() {
        let mut p = ok.clone();
        edit(&mut p);
        assert!(VcsLand.plan_check(&p, &w.config).is_err(), "edit {i}");
    }
}

#[test]
fn a_depends_on_cycle_is_refused() {
    let w = World::new();
    // On the face of one plan: it depends on the proposal it replaces, or names a dependency twice.
    let mut p = w.plan();
    p["args"]["rebaseOf"] = json!("p1");
    p["args"]["dependsOn"] = json!([{"repo": "local:origin", "proposal": "p1"}]);
    let e = VcsLand.plan_check(&p, &w.config).unwrap_err();
    assert!(e.to_string().contains("cycle"), "{e}");
    let mut p = w.plan();
    p["args"]["dependsOn"] = json!([
        {"repo": "local:origin", "proposal": "p1"},
        {"repo": "local:origin", "proposal": "p1"}
    ]);
    assert!(VcsLand.plan_check(&p, &w.config).is_err());
    let mut p = w.plan();
    p["args"]["dependsOn"] = json!([{"repo": "local:origin", "proposal": "p1"}]);
    VcsLand.plan_check(&p, &w.config).unwrap();
    // Across plans, the lane's graph.
    let g = |edges: &[(&str, &[&str])]| {
        edges
            .iter()
            .map(|(k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect()))
            .collect()
    };
    assert_eq!(
        depends_cycle(&g(&[("a", &["b"]), ("b", &["c"]), ("c", &[])])),
        None
    );
    assert_eq!(
        depends_cycle(&g(&[("a", &["b"]), ("b", &["c"]), ("c", &["a"])])),
        Some(vec!["a".into(), "b".into(), "c".into(), "a".into()])
    );
    assert!(depends_cycle(&g(&[("a", &["a"])])).is_some());
}

#[tokio::test]
async fn a_rerun_finds_the_ref_already_there() {
    let w = World::new();
    let plan = w.plan();
    let onto = w.advance("b.txt", "one\n");
    let commit = w.squash(&onto);
    let ctx = w.ctx(Some((&onto, &commit)), true);
    assert!(matches!(
        VcsLand.run(&plan, &key(), &ctx).await,
        Outcome::Succeeded { .. }
    ));
    let first = w.tip();
    // lookup sees it, and so does a second run (the push is a no-op, not a conflict).
    assert!(matches!(
        VcsLand.lookup(&plan, &key(), &ctx).await,
        Some(Outcome::Succeeded { .. })
    ));
    let again = VcsLand.run(&plan, &key(), &ctx).await;
    assert!(matches!(again, Outcome::Succeeded { .. }), "{again:?}");
    assert_eq!(w.tip(), first);
}

// ───────────────────────── the change summary (P-17, X-13) ─────────────────────────

impl World {
    /// The summary of the approved change as a host builds it.
    fn change(&self) -> han_vcs::Change {
        let d = self.vcs.delta(&self.work, &self.base, &self.to).unwrap();
        let n = self
            .vcs
            .delta_lines(&self.work, &self.base, &self.to, &d)
            .unwrap();
        han_vcs::land_change("local:origin", &d, &n).unwrap()
    }

    fn plan_with(&self, change: &han_vcs::Change) -> Value {
        let mut plan = self.plan();
        plan["args"]["change"] = serde_json::to_value(change).unwrap();
        plan
    }
}

#[tokio::test]
async fn a_summary_of_the_change_passes_the_plan_check_and_the_bundle() {
    let w = World::new();
    let change = w.change();
    assert_eq!(
        (change.total, change.files.len(), change.truncated),
        (1, 1, None)
    );
    assert_eq!(
        (change.files[0].path.as_str(), change.files[0].lines),
        ("a.txt", 2)
    );
    let plan = w.plan_with(&change);
    VcsLand.plan_check(&plan, &w.config).unwrap();
    VcsLand.preflight(&plan, &w.ctx(None, true)).await.unwrap();
    // The digest the executor compares with the body's is the plan's `args.change`.
    assert_eq!(
        han_exec::actions::change_digest(&plan),
        Some(change.digest())
    );
    // A host that lists fewer paths than the change has, and says so, is accepted: Policy makes it tier 3.
    let mut cut = change.clone();
    cut.files.clear();
    cut.truncated = Some(true);
    let plan = w.plan_with(&cut);
    VcsLand.plan_check(&plan, &w.config).unwrap();
    VcsLand.preflight(&plan, &w.ctx(None, true)).await.unwrap();
    // A larger size than the change has only raises the tier.
    let mut more = change.clone();
    more.files[0].lines = 50;
    VcsLand
        .preflight(&w.plan_with(&more), &w.ctx(None, true))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_summary_that_understates_or_misdescribes_the_change_is_refused() {
    let w = World::new();
    let good = w.change();
    let cases: Vec<(&str, han_vcs::Change)> = vec![
        ("understates", {
            let mut c = good.clone();
            c.files[0].lines = 1;
            c
        }),
        ("is not the change's", {
            let mut c = good.clone();
            c.files[0].path = "b.txt".into();
            c
        }),
        ("is not the change's", {
            let mut c = good.clone();
            c.files[0].kind = han_vcs::FileKind::Add;
            c
        }),
        ("not a valid summary", {
            let mut c = good.clone();
            c.files.clear(); // left out and not said
            c
        }),
        ("not a valid summary", {
            let mut c = good.clone();
            c.truncated = Some(true); // said, with everything listed
            c
        }),
        ("not a valid summary", {
            let mut c = good.clone();
            c.total = 2;
            c
        }),
    ];
    for (needle, c) in cases {
        let plan = w.plan_with(&c);
        match VcsLand.preflight(&plan, &w.ctx(None, true)).await {
            Err(PlanError::Changeset(m) | PlanError::Schema(m)) => {
                assert!(m.contains(needle), "{m}")
            }
            other => panic!("expected a refusal naming {needle:?}, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_summary_that_is_not_one_or_is_for_another_plan_fails_the_plan_check() {
    let w = World::new();
    let good = w.change();
    let schema = |plan: Value, needle: &str| match VcsLand.plan_check(&plan, &w.config) {
        Err(PlanError::Schema(m)) => assert!(m.contains(needle), "{m}"),
        other => panic!("expected a schema refusal naming {needle:?}, got {other:?}"),
    };
    let mut c = good.clone();
    c.repo = "local:elsewhere".into();
    schema(w.plan_with(&c), "is for local:elsewhere");
    let mut c = good.clone();
    c.delta_digest = "f".repeat(64);
    schema(w.plan_with(&c), "deltaDigest");
    // Not a summary at all: an unknown member, a repeated path, a path that climbs out.
    let mut plan = w.plan_with(&good);
    plan["args"]["change"]["tier"] = json!(0);
    schema(plan, "not a valid summary");
    let mut c = good.clone();
    let f = c.files[0].clone();
    c.files = vec![f.clone(), f];
    c.total = 2;
    schema(w.plan_with(&c), "not a valid summary");
    let mut plan = w.plan_with(&good);
    plan["args"]["change"]["files"][0]["path"] = json!("../a.txt");
    schema(plan, "not a valid summary");
}

// ───────────────────────── vcs.try (P-17): the checks, and nothing lands ─────────────────────────

impl World {
    /// The plan of trying the change: a land's plan without `tier` and `rebase`.
    fn try_plan(&self) -> Value {
        let mut plan = self.plan();
        plan["action"] = json!("vcs.try");
        let args = plan["args"].as_object_mut().unwrap();
        args.remove("tier");
        args.remove("rebase");
        plan
    }
}

#[tokio::test]
async fn a_try_verifies_the_lanes_commit_and_moves_no_ref() {
    let w = World::new();
    let plan = w.try_plan();
    VcsTry.plan_check(&plan, &w.config).unwrap();
    VcsTry.preflight(&plan, &w.ctx(None, true)).await.unwrap();
    let onto = w.advance(
        "b.txt",
        "b, moved on
",
    );
    let commit = w.squash(&onto);
    let ctx = w.ctx(Some((&onto, &commit)), true);
    let o = VcsTry.run(&plan, &key(), &ctx).await;
    let Outcome::Succeeded { detail } = &o else {
        panic!("{o:?}")
    };
    assert!(
        detail.contains(&commit) && detail.contains("nothing was landed"),
        "{detail}"
    );
    assert_eq!(w.tip(), onto, "the ref did not move");
    // After a restart the journal's instruction answers for the launch; without it nothing can be said.
    assert!(matches!(
        VcsTry.lookup(&plan, &key(), &ctx).await,
        Some(Outcome::Succeeded { .. })
    ));
    assert!(
        VcsTry
            .lookup(&plan, &key(), &w.ctx(None, true))
            .await
            .is_none()
    );
    assert!(VcsTry.lane_only());
}

#[tokio::test]
async fn a_try_refuses_a_commit_that_is_not_the_change_or_not_on_onto() {
    let w = World::new();
    let plan = w.try_plan();
    let onto = w.advance(
        "b.txt", "one
",
    );
    let base = w.base.clone();
    let stale = w.squash(&base);
    let o = VcsTry
        .run(&plan, &key(), &w.ctx(Some((&onto, &stale)), true))
        .await;
    failed_unstarted(&o, "not one commit on");
    // A commit on `onto` with another change in it.
    let tree = format!("{onto}^{{tree}}");
    let other = git(&w.work, &["commit-tree", "-p", &onto, "-m", "x", &tree]);
    let o = VcsTry
        .run(&plan, &key(), &w.ctx(Some((&onto, &other)), true))
        .await;
    failed_unstarted(&o, "not the change the plan names");
    let o = VcsTry.run(&plan, &key(), &w.ctx(None, true)).await;
    failed_unstarted(&o, "no lane instruction");
    assert_eq!(w.tip(), onto);
}

#[tokio::test]
async fn a_try_plan_is_a_lands_without_the_landing_members_and_never_a_revert() {
    let w = World::new();
    let ok = w.try_plan();
    VcsTry.plan_check(&ok, &w.config).unwrap();
    // `tier`, `rebase`, `dependsOn` and `rebaseOf` are a land's.
    for (k, v) in [
        ("tier", json!(1)),
        ("rebase", json!("none")),
        ("dependsOn", json!([])),
        ("rebaseOf", json!("p0")),
    ] {
        let mut bad = ok.clone();
        bad["args"][k] = v;
        assert!(
            matches!(
                VcsTry.plan_check(&bad, &w.config),
                Err(PlanError::Schema(_))
            ),
            "{k}"
        );
    }
    // A ref outside the landing refs, an action that is not a try, and a revert.
    let mut other = ok.clone();
    other["args"]["ref"] = json!("refs/heads/release");
    assert!(matches!(
        VcsTry.plan_check(&other, &w.config),
        Err(PlanError::OutOfScope(_))
    ));
    assert!(matches!(
        VcsTry.plan_check(&w.plan(), &w.config),
        Err(PlanError::Schema(_))
    ));
    let mut revert = ok.clone();
    revert["args"]["change"] = json!({"repo": "local:origin", "deltaDigest": w.delta_digest(), "total": 0,
        "files": [], "revertOf": "a".repeat(64)});
    assert!(matches!(
        VcsTry.plan_check(&revert, &w.config),
        Err(PlanError::Schema(_))
    ));
    // The landing action refuses a try's plan, as a try refuses a land's.
    assert!(VcsLand.plan_check(&ok, &w.config).is_err());
    let mut none = (*w.config).clone();
    none.repos[0].landing_refs.clear();
    assert!(matches!(
        VcsTry.plan_check(&ok, &none),
        Err(PlanError::OutOfScope(_))
    ));
}

//! The `land` finish flow (R33 §2, §3, §7) against real git: the plan the host proposes, the frozen changeset,
//! the lane's states in the status, and pruning on every final state. The lane is played by a fake feed that
//! serves `han.land/1` change items; `lane_items_reach_the_host_through_the_node` covers the real path.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use han_exec::{Action, ExecConfig, RepoTarget};
use han_host::driver::builtin;
use han_host::host::{HostConfig, RepoConfig, SandboxChoice};
use han_host::land::{Flow, LaneChange, LaneFeed, Pruned};
use han_host::sandbox::ShimCommand;
use han_node::api::BoxFuture;
use han_registry::{FinishSpec, LandFinish, TierRule};
use han_vcs::{GitVcs, Vcs, WorkspaceSpec};
use serde_json::{Map, Value, json};

const REPO: &str = "local:origin";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
        .args(["-c", "protocol.file.allow=always"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// What the test plays the lane with.
#[derive(Default)]
struct FakeFeed {
    changes: Mutex<Vec<LaneChange>>,
    blobs: Mutex<Vec<(String, Vec<u8>)>>,
}

impl FakeFeed {
    fn say(&self, proposal: &str, state: &str, reason: Option<&str>, seq: u64) {
        self.changes.lock().unwrap().push(LaneChange {
            proposal: proposal.into(),
            state: state.into(),
            reason: reason.map(str::to_owned),
            detail: None,
            stalled: None,
            position: None,
            conflict: None,
            evidence: None,
            seq,
        });
    }
}

impl LaneFeed for FakeFeed {
    fn changes(&self) -> BoxFuture<'_, Result<Vec<LaneChange>, String>> {
        Box::pin(async move { Ok(self.changes.lock().unwrap().clone()) })
    }

    fn blob<'a>(&'a self, blob: &'a Value) -> BoxFuture<'a, Result<Vec<u8>, String>> {
        Box::pin(async move {
            let hash = blob["hash"].as_str().unwrap_or_default();
            self.blobs
                .lock()
                .unwrap()
                .iter()
                .find(|(h, _)| h == hash)
                .map(|(_, b)| b.clone())
                .ok_or_else(|| "no such blob".to_owned())
        })
    }
}

struct Rig {
    _tmp: tempfile::TempDir,
    origin: PathBuf,
    session_dir: PathBuf,
    vcs: Arc<GitVcs>,
    feed: Arc<FakeFeed>,
    flow: Flow,
    base: String,
}

impl Rig {
    fn dir(&self) -> PathBuf {
        self.session_dir.join("wt")
    }

    fn write(&self, path: &str, text: &str) {
        let p = self.dir().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    /// The agent commits (its own work, not a host proposal).
    fn commit(&self, path: &str, text: &str, message: &str) -> String {
        self.write(path, text);
        self.vcs
            .commit_all(&self.dir(), message, "@coder:han.example")
            .unwrap()
    }

    /// Drafts and proposes as `id`.
    fn propose(&self, id: &str, summary: &str) -> Value {
        let d = self
            .flow
            .draft(summary, &Map::new())
            .expect("the plan is drafted");
        self.flow.proposed(id);
        d.plan
    }
}

fn rig(finish: FinishSpec) -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let seed = root.join("seed");
    std::fs::create_dir_all(seed.join("src")).unwrap();
    std::fs::write(seed.join("README.md"), "a demo repository\n").unwrap();
    std::fs::write(seed.join("src/lib.rs"), "pub mod auth;\n").unwrap();
    git(&seed, &["init", "-q", "-b", "main"]);
    git(&seed, &["add", "-A"]);
    git(&seed, &["commit", "-q", "-m", "init"]);
    let origin = root.join("origin.git");
    git(
        root,
        &[
            "clone",
            "-q",
            "--bare",
            seed.to_str().unwrap(),
            origin.to_str().unwrap(),
        ],
    );
    let session_dir = root.join("s1");
    std::fs::create_dir_all(&session_dir).unwrap();
    let vcs = Arc::new(GitVcs::new(root.join("objects")));
    let ws = vcs
        .create_workspace(&WorkspaceSpec {
            source: origin.to_string_lossy().into_owned(),
            base: "main".into(),
            branch: "han/12-coder".into(),
            dir: session_dir.join("wt"),
            sparse: Vec::new(),
        })
        .unwrap();
    // build output next to the worktree, as the toolchain makes it
    std::fs::create_dir_all(session_dir.join("target/debug")).unwrap();
    std::fs::write(session_dir.join("target/debug/x"), "x").unwrap();
    let cfg = HostConfig {
        space: "s1:han.example".into(),
        agent: "@coder:han.example".into(),
        operator: None,
        driver: builtin("pi").unwrap(),
        repo: RepoConfig {
            name: REPO.into(),
            source: origin.to_string_lossy().into_owned(),
            base: "main".into(),
            default_branch: "main".into(),
        },
        issue: Some("github:o/r#12".into()),
        task: None,
        executor: "@exec:han.example".into(),
        session_dir: session_dir.clone(),
        taint_db: root.join("taint.db"),
        guarded: false,
        strict_on_read: true,
        sandbox: SandboxChoice {
            backend: "none".into(),
            allow_advisory: true,
            ro: Vec::new(),
            login_dir: None,
            allow_hosts: Vec::new(),
            toolchains: Default::default(),
        },
        shim: ShimCommand::new("han", ["shim"]),
        node: PathBuf::from("node"),
        adapter_dir: PathBuf::new(),
        model_key: None,
        budget: None,
        turn_timeout: Duration::from_secs(60),
        exclusion: Vec::new(),
        finish,
    };
    let feed = Arc::new(FakeFeed::default());
    let base = ws.base.clone();
    let flow = Flow::new(
        &cfg,
        "S1",
        Some("Work on github:o/r#12: Fix the login\n\n----- BEGIN ISSUE TEXT -----\nx".into()),
        ws,
        vcs.clone(),
        feed.clone(),
    )
    .expect("the land flow is on");
    Rig {
        _tmp: tmp,
        origin,
        session_dir,
        vcs,
        feed,
        flow,
        base,
    }
}

fn on() -> FinishSpec {
    FinishSpec {
        land: Some(LandFinish::default()),
    }
}

fn args(plan: &Value) -> &Value {
    &plan["args"]
}

fn exec_config(origin: &Path) -> ExecConfig {
    let mut repo = RepoTarget::new(REPO, origin.to_string_lossy().into_owned());
    repo.landing_refs = vec!["refs/heads/main".into()];
    ExecConfig {
        repos: vec![repo],
        http_hosts: Vec::new(),
        branch_prefixes: vec!["han/".into()],
        github: None,
        test_allow_private_http: false,
    }
}

#[test]
fn the_flow_is_off_unless_the_spec_turns_it_on() {
    let r = rig(on());
    // a spec without it gives no flow at all
    let tmp = tempfile::tempdir().unwrap();
    let ws = r
        .vcs
        .create_workspace(&WorkspaceSpec {
            source: r.origin.to_string_lossy().into_owned(),
            base: "main".into(),
            branch: "han/x".into(),
            dir: tmp.path().join("wt"),
            sparse: Vec::new(),
        })
        .unwrap();
    let cfg = HostConfig {
        finish: FinishSpec::default(),
        ..config_of(&r)
    };
    assert!(Flow::new(&cfg, "S", None, ws, r.vcs.clone(), r.feed.clone()).is_none());
}

fn config_of(r: &Rig) -> HostConfig {
    HostConfig {
        space: "s1:han.example".into(),
        agent: "@coder:han.example".into(),
        operator: None,
        driver: builtin("pi").unwrap(),
        repo: RepoConfig {
            name: REPO.into(),
            source: r.origin.to_string_lossy().into_owned(),
            base: "main".into(),
            default_branch: "main".into(),
        },
        issue: None,
        task: None,
        executor: "@exec:han.example".into(),
        session_dir: r.session_dir.clone(),
        taint_db: r.session_dir.join("taint.db"),
        guarded: false,
        strict_on_read: true,
        sandbox: SandboxChoice {
            backend: "none".into(),
            allow_advisory: true,
            ro: Vec::new(),
            login_dir: None,
            allow_hosts: Vec::new(),
            toolchains: Default::default(),
        },
        shim: ShimCommand::new("han", ["shim"]),
        node: PathBuf::from("node"),
        adapter_dir: PathBuf::new(),
        model_key: None,
        budget: None,
        turn_timeout: Duration::from_secs(60),
        exclusion: Vec::new(),
        finish: on(),
    }
}

// ───────────────────────── the plan ─────────────────────────

#[test]
fn the_land_flow_proposes_a_correct_plan() {
    let r = rig(on());
    r.write("src/auth.rs", "pub fn login() {}\n");
    r.write("README.md", "a demo repository, edited\n");
    let d = r
        .flow
        .draft("add the login check", &Map::new())
        .expect("the plan is drafted");
    let a = args(&d.plan);
    assert_eq!(d.plan["action"], "vcs.land");
    assert_eq!(d.plan["target"], REPO);
    assert_eq!(a["ref"], "refs/heads/main");
    assert_eq!(a["base"], r.base.as_str());
    assert_eq!(a["rebase"], "path-disjoint");
    assert_eq!(a["tier"], 1);
    // the title comes from the task, not from the proposal's summary
    assert_eq!(a["title"], "Fix the login (github:o/r#12)");
    assert!(a.get("dependsOn").is_none() && a.get("rebaseOf").is_none());

    // the digest is the delta's digest, and the commit is the workspace's head
    let to = a["to"].as_str().unwrap();
    assert_eq!(r.vcs.head(&r.dir()).unwrap(), to);
    assert_eq!(
        r.vcs.is_dirty(&r.dir()).ok(),
        Some(false),
        "the workspace is committed"
    );
    let delta = r.vcs.delta(&r.dir(), &r.base, to).unwrap();
    assert_eq!(delta.entries.len(), 2);
    assert_eq!(a["deltaDigest"], delta.digest().unwrap().as_str());

    // the bundle is the one the plan names, and it verifies: it imports into a repository of the
    // executor's own, which then has the same delta
    assert_eq!(
        d.plan["argsBlob"]["hash"],
        han_core::ids::sha256_hex(&d.bundle).as_str()
    );
    assert_eq!(d.plan["argsBlob"]["size"], d.bundle.len());
    assert_eq!(d.plan["argsBlob"]["type"], han_vcs::BUNDLE_MEDIA_TYPE);
    let exec_vcs = GitVcs::new(r.session_dir.join("exec-objects"));
    let imported = exec_vcs
        .import_bundle(&r.origin.to_string_lossy(), None, &d.bundle, &r.base, to)
        .expect("the bundle imports");
    assert_eq!(
        exec_vcs
            .delta(&imported, &r.base, to)
            .unwrap()
            .digest()
            .unwrap(),
        a["deltaDigest"].as_str().unwrap()
    );

    // and the executor's own plan check accepts it
    han_exec::actions::VcsLand
        .plan_check(&d.plan, &exec_config(&r.origin))
        .expect("the executor accepts the plan");
}

#[test]
fn the_tier_comes_from_the_path_rules_else_one() {
    let finish = FinishSpec {
        land: Some(LandFinish {
            tier_rules: vec![
                TierRule {
                    path: "docs/**".into(),
                    tier: 0,
                },
                TierRule {
                    path: "src/auth/**".into(),
                    tier: 3,
                },
            ],
            ..LandFinish::default()
        }),
    };
    // docs only: tier 0
    let r = rig(finish.clone());
    r.write("docs/guide.md", "words\n");
    let d = r.flow.draft("docs", &Map::new()).unwrap();
    assert_eq!(args(&d.plan)["tier"], 0);
    // a touched path in a rule's reach raises it to the highest
    let r = rig(finish.clone());
    r.write("docs/guide.md", "words\n");
    r.write("src/auth/mod.rs", "// auth\n");
    let d = r.flow.draft("both", &Map::new()).unwrap();
    assert_eq!(args(&d.plan)["tier"], 3);
    // no rule matches: 1
    let r = rig(finish);
    r.write("src/other.rs", "// other\n");
    let d = r.flow.draft("other", &Map::new()).unwrap();
    assert_eq!(args(&d.plan)["tier"], 1);
}

#[test]
fn nothing_to_land_is_refused() {
    let r = rig(on());
    let e = r.flow.draft("nothing", &Map::new()).err().unwrap();
    assert!(e.contains("nothing to land"), "{e}");
}

#[test]
fn the_target_ref_follows_the_spec() {
    let finish = FinishSpec {
        land: Some(LandFinish {
            target_ref: Some("refs/heads/release".into()),
            ..LandFinish::default()
        }),
    };
    let r = rig(finish);
    r.write("a.txt", "a\n");
    let d = r.flow.draft("a", &Map::new()).unwrap();
    assert_eq!(args(&d.plan)["ref"], "refs/heads/release");
}

// ───────────────────────── frozen once proposed ─────────────────────────

#[test]
fn a_commit_after_proposing_does_not_change_the_proposed_plan() {
    let r = rig(on());
    r.write("src/auth.rs", "pub fn login() {}\n");
    let plan1 = r.propose("P1", "the login check");
    let to1 = args(&plan1)["to"].as_str().unwrap().to_owned();
    let frozen = r.flow.proposals();
    assert_eq!(frozen.len(), 1);
    assert_eq!(frozen[0].to, to1);

    // the agent goes on in the same workspace
    let later = r.commit("src/more.rs", "pub fn more() {}\n", "more");
    assert_ne!(later, to1);

    // the proposed state is what it was: same head, same digest
    let still = r.flow.proposals();
    assert_eq!(still, frozen);
    assert_eq!(still[0].to, to1);
    assert_eq!(
        still[0].delta_digest,
        args(&plan1)["deltaDigest"].as_str().unwrap()
    );
    // and the proposal's own commits are still exactly base..to1
    let delta = r.vcs.delta(&r.dir(), &r.base, &to1).unwrap();
    assert_eq!(delta.digest().unwrap(), still[0].delta_digest);
    assert_eq!(delta.entries.len(), 1);
}

#[test]
fn later_work_is_a_new_changeset_that_depends_on_the_first() {
    let r = rig(on());
    r.write("src/auth.rs", "pub fn login() {}\n");
    let plan1 = r.propose("P1", "the login check");
    let to1 = args(&plan1)["to"].as_str().unwrap().to_owned();

    r.write("src/more.rs", "pub fn more() {}\n");
    let d2 = r.flow.draft("more", &Map::new()).unwrap();
    let a2 = args(&d2.plan);
    // based on the old `to`, naming the old proposal
    assert_eq!(a2["base"], to1.as_str());
    assert_ne!(a2["to"], to1.as_str());
    assert_eq!(a2["dependsOn"], json!([{"repo": REPO, "proposal": "P1"}]));
    // its delta is the new work only, and its digest matches
    let delta = r
        .vcs
        .delta(&r.dir(), &to1, a2["to"].as_str().unwrap())
        .unwrap();
    assert_eq!(delta.entries.len(), 1);
    assert_eq!(delta.entries[0].path, "src/more.rs");
    assert_eq!(a2["deltaDigest"], delta.digest().unwrap().as_str());
    // the bundle carries base(=P1's to)..to, and imports on top of what the lane imported for P1
    let exec_vcs = GitVcs::new(r.session_dir.join("exec-objects"));
    let first = r.flow.proposals();
    assert_eq!(first.len(), 1, "the draft is not a proposal yet");
    // (the lane has P1's commits from its bundle)
    let b1 = r.vcs.bundle(&r.dir(), &r.base, &to1).unwrap();
    exec_vcs
        .import_bundle(&r.origin.to_string_lossy(), None, &b1, &r.base, &to1)
        .unwrap();
    exec_vcs
        .import_bundle(
            &r.origin.to_string_lossy(),
            None,
            &d2.bundle,
            &to1,
            a2["to"].as_str().unwrap(),
        )
        .expect("the second bundle imports on the first");
    han_exec::actions::VcsLand
        .plan_check(&d2.plan, &exec_config(&r.origin))
        .expect("the executor accepts the dependent plan");
    r.flow.proposed("P2");
    // a third depends on the second (the chain), not on the first
    r.write("src/third.rs", "pub fn third() {}\n");
    let d3 = r.flow.draft("third", &Map::new()).unwrap();
    assert_eq!(
        args(&d3.plan)["dependsOn"],
        json!([{"repo": REPO, "proposal": "P2"}])
    );
}

#[test]
fn proposing_again_with_nothing_new_is_refused() {
    let r = rig(on());
    r.write("src/auth.rs", "pub fn login() {}\n");
    r.propose("P1", "the login check");
    let e = r.flow.draft("again", &Map::new()).err().unwrap();
    assert!(e.contains("nothing new") && e.contains("P1"), "{e}");
}

#[test]
fn a_rewrite_of_the_proposed_history_is_refused() {
    let r = rig(on());
    r.write("src/auth.rs", "pub fn login() {}\n");
    r.propose("P1", "the login check");
    // the agent resets the branch to the base and does something else
    git(&r.dir(), &["reset", "-q", "--hard", &r.base]);
    r.write("src/other.rs", "// other\n");
    let e = r.flow.draft("other", &Map::new()).err().unwrap();
    assert!(e.contains("frozen") && e.contains("P1"), "{e}");
    assert_eq!(r.flow.proposals().len(), 1);
}

#[test]
fn a_refused_proposal_freezes_nothing() {
    let r = rig(on());
    r.write("src/auth.rs", "pub fn login() {}\n");
    r.flow.draft("x", &Map::new()).unwrap();
    // the host refused or failed to propose it
    r.flow.abandon();
    r.flow.proposed("never");
    assert!(r.flow.proposals().is_empty());
    // queued, then refused by the home: the news may come before the proposal is recorded
    r.write("src/b.rs", "// b\n");
    r.flow.draft("y", &Map::new()).unwrap();
    r.flow.withdrawn("Q1");
    r.flow.proposed("Q1");
    assert!(r.flow.proposals().is_empty());
}

#[test]
fn a_rejected_proposal_is_proposed_again_only_with_rebase_of() {
    let r = rig(on());
    r.write("src/auth.rs", "pub fn login() {}\n");
    let plan1 = r.propose("P1", "the login check");
    r.feed.say("P1", "rejected", Some("conflict"), 3);
    assert!(tokio_block(r.flow.poll()).unwrap());

    // without rebaseOf: refused, and told how
    r.write("src/auth.rs", "pub fn login() { /* resolved */ }\n");
    let e = r.flow.draft("resolved", &Map::new()).err().unwrap();
    assert!(e.contains("rebaseOf") && e.contains("rejected"), "{e}");

    // with it: same base, depends on nothing, names the rejected one
    let mut effect = Map::new();
    effect.insert("rebaseOf".into(), json!("P1"));
    let d = r.flow.draft("resolved", &effect).unwrap();
    let a = args(&d.plan);
    assert_eq!(a["rebaseOf"], "P1");
    assert_eq!(a["base"], args(&plan1)["base"]);
    assert!(a.get("dependsOn").is_none());
    han_exec::actions::VcsLand
        .plan_check(&d.plan, &exec_config(&r.origin))
        .expect("the executor accepts the rebaseOf plan");

    // a proposal that is not rejected is not re-proposed
    let mut bad = Map::new();
    bad.insert("rebaseOf".into(), json!("nope"));
    assert!(r.flow.draft("x", &bad).is_err());
}

// ───────────────────────── following the lane ─────────────────────────

fn tokio_block<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

#[test]
fn the_lanes_states_reach_the_status_and_the_agent() {
    let r = rig(on());
    r.write("src/auth.rs", "pub fn login() {}\n");
    r.propose("P1", "the login check");
    // nothing from the lane yet: the proposal still waits for its approval, which the status says already
    assert!(!tokio_block(r.flow.poll()).unwrap());
    assert!(r.flow.status_line().is_none());
    assert_eq!(r.flow.notices()[0]["state"], "proposed");

    for (state, seq) in [("queued", 1), ("testing", 2), ("landing", 3), ("landed", 4)] {
        r.feed.say("P1", state, None, seq);
        assert!(tokio_block(r.flow.poll()).unwrap(), "{state}");
        let (line, blocked) = r.flow.status_line().unwrap();
        assert!(line.contains(state) && !blocked, "{line}");
        // asking again with nothing new changes nothing
        assert!(!tokio_block(r.flow.poll()).unwrap());
    }
    // the older item of the same change does not move it back
    r.feed.say("P1", "queued", None, 1);
    assert!(!tokio_block(r.flow.poll()).unwrap());
    assert!(r.flow.status_line().unwrap().0.contains("landed"));

    r.feed.say("P1", "confirmed", None, 5);
    tokio_block(r.flow.poll()).unwrap();
    assert!(r.flow.status_line().unwrap().0.contains("confirmed"));
    let n = r.flow.notices();
    assert_eq!(n[0]["state"], "confirmed");
    assert_eq!(n[0]["final"], true);
    // the file survives a restart of the host
    let saved = std::fs::read_to_string(r.session_dir.join("land.json")).unwrap();
    assert!(saved.contains("confirmed"));
}

#[test]
fn a_stalled_change_is_blocked_and_says_why() {
    let r = rig(on());
    r.write("src/auth.rs", "pub fn login() {}\n");
    r.propose("P1", "the login check");
    r.feed.changes.lock().unwrap().push(LaneChange {
        proposal: "P1".into(),
        state: "testing".into(),
        reason: None,
        detail: None,
        stalled: Some("no required check appeared".into()),
        position: None,
        conflict: None,
        evidence: None,
        seq: 2,
    });
    tokio_block(r.flow.poll()).unwrap();
    let (line, blocked) = r.flow.status_line().unwrap();
    assert!(
        blocked && line.contains("stalled: no required check appeared"),
        "{line}"
    );
    assert_eq!(r.flow.notices()[0]["stalled"], "no required check appeared");
}

#[test]
fn a_conflict_rejection_gives_the_agent_the_conflict_record() {
    let r = rig(on());
    r.write("src/auth.rs", "pub fn login() {}\n");
    r.propose("P1", "the login check");
    let record = json!({
        "changeset": "P1", "onto": "abc", "clean": ["README.md"],
        "conflicts": [{"path": "src/auth.rs", "kind": "add_add"}],
        "against": [{"proposal": "P0", "task": "T0"}],
    });
    let bytes = serde_json::to_vec(&record).unwrap();
    let hash = han_core::ids::sha256_hex(&bytes);
    r.feed
        .blobs
        .lock()
        .unwrap()
        .push((hash.clone(), bytes.clone()));
    r.feed.changes.lock().unwrap().push(LaneChange {
        proposal: "P1".into(),
        state: "rejected".into(),
        reason: Some("conflict".into()),
        detail: Some("not path-disjoint".into()),
        stalled: None,
        position: None,
        conflict: Some(json!({"hash": hash, "size": bytes.len(), "type": "application/json"})),
        evidence: None,
        seq: 4,
    });
    tokio_block(r.flow.poll()).unwrap();

    let (line, blocked) = r.flow.status_line().unwrap();
    assert!(
        blocked && line.contains("rejected") && line.contains("conflict"),
        "{line}"
    );
    let n = &r.flow.notices()[0];
    assert_eq!(n["reason"], "conflict");
    assert_eq!(
        n["conflictRecord"], record,
        "the record was fetched, as data"
    );
    assert!(n["next"].as_str().unwrap().contains("rebaseOf"));
    assert_eq!(n["conflict"]["hash"], hash.as_str());
}

#[test]
fn lane_items_are_read_the_way_the_lane_writes_them() {
    // the shape han-exec's lane publishes (lane.rs `body`)
    let item = json!({
        "item": "change", "proposal": "E-9", "repo": REPO, "ref": "refs/heads/main",
        "lane": "local:origin#refs/heads/main", "state": "rejected", "seq": 7,
        "at": "2026-10-15T10:00:00Z", "position": 2, "reason": "test", "detail": "a check failed",
        "stalled": "past its deadline",
        "conflict": {"hash": "h", "size": 3, "type": "application/json"},
        "evidence": {"hash": "e", "size": 4, "type": "application/json"},
    });
    let c = LaneChange::parse(&item).unwrap();
    assert_eq!(c.proposal, "E-9");
    assert_eq!(c.state, "rejected");
    assert_eq!(c.reason.as_deref(), Some("test"));
    assert_eq!(c.position, Some(2));
    assert_eq!(c.seq, 7);
    assert!(c.conflict.is_some() && c.evidence.is_some());
    // a lane item is not a change
    assert!(LaneChange::parse(&json!({"item": "lane", "repo": REPO})).is_none());
}

// ───────────────────────── pruning ─────────────────────────

fn settle_as(r: &Rig, state: &str) {
    r.feed.say("P1", state, None, 9);
    tokio_block(r.flow.poll()).unwrap();
}

fn proposed_rig(finish: FinishSpec) -> Rig {
    let r = rig(finish);
    r.write("src/auth.rs", "pub fn login() {}\n");
    r.propose("P1", "the login check");
    r
}

#[test]
fn a_confirmed_proposal_prunes_the_workspace_its_build_output_and_refs() {
    let r = proposed_rig(on());
    assert!(r.dir().exists() && r.session_dir.join("target").exists());
    // not final: nothing is removed
    assert!(matches!(r.flow.prune(false, false), Pruned::Waiting(_)));
    settle_as(&r, "confirmed");
    // a turn is running: the agent is in there
    assert!(matches!(r.flow.prune(true, false), Pruned::Waiting(_)));
    assert!(r.dir().exists());
    assert_eq!(r.flow.prune(false, false), Pruned::Removed);
    assert!(!r.dir().exists(), "the workspace (and its refs with it)");
    assert!(!r.session_dir.join("target").exists(), "CARGO_TARGET_DIR");
    assert!(r.flow.pruned());
    // once only
    assert!(matches!(r.flow.prune(false, false), Pruned::Waiting(_)));
    // a pruned workspace proposes nothing
    assert!(r.flow.draft("late", &Map::new()).is_err());
}

#[test]
fn rejected_cancelled_and_reverted_prune_once_the_session_is_over() {
    for state in ["rejected", "cancelled", "reverted"] {
        let r = proposed_rig(on());
        settle_as(&r, state);
        // while the session lives the agent may still re-propose with rebaseOf
        assert!(
            matches!(r.flow.prune(false, false), Pruned::Waiting(_)),
            "{state}"
        );
        assert!(r.dir().exists());
        assert_eq!(r.flow.prune(false, true), Pruned::Removed, "{state}");
        assert!(!r.dir().exists(), "{state}");
        assert!(!r.session_dir.join("target").exists(), "{state}");
    }
}

#[test]
fn a_rejected_proposal_replaced_by_a_rebase_does_not_hold_the_workspace() {
    let r = proposed_rig(on());
    settle_as(&r, "rejected");
    r.write("src/auth.rs", "pub fn login() { /* resolved */ }\n");
    let mut effect = Map::new();
    effect.insert("rebaseOf".into(), json!("P1"));
    r.flow.draft("resolved", &effect).unwrap();
    r.flow.proposed("P2");
    r.feed.say("P2", "confirmed", None, 3);
    tokio_block(r.flow.poll()).unwrap();
    // P1 is superseded: the session need not be over
    assert_eq!(r.flow.prune(false, false), Pruned::Removed);
}

#[test]
fn landed_is_final_only_where_the_repository_has_no_full_stage() {
    let r = proposed_rig(on());
    settle_as(&r, "landed");
    assert!(matches!(r.flow.prune(false, true), Pruned::Waiting(_)));
    assert!(r.dir().exists());

    let no_full = FinishSpec {
        land: Some(LandFinish {
            no_full_stage: true,
            ..LandFinish::default()
        }),
    };
    let r = proposed_rig(no_full);
    settle_as(&r, "landed");
    assert_eq!(r.flow.prune(false, false), Pruned::Removed);
    assert!(!r.dir().exists());
}

#[test]
fn commits_after_the_proposed_head_are_reported_unlanded_and_kept() {
    let r = proposed_rig(on());
    // the agent kept working after the proposal (the #81 case)
    r.commit("src/late.rs", "pub fn late() {}\n", "a late fix");
    settle_as(&r, "confirmed");
    let Pruned::Kept(u) = r.flow.prune(false, false) else {
        panic!("the workspace holds work that never landed")
    };
    assert_eq!(u.proposal, "P1");
    assert_eq!(u.commits.len(), 1);
    assert_eq!(u.commits[0].subject, "a late fix");
    assert_eq!(u.commits[0].id.len(), 8);
    assert!(!u.uncommitted);
    assert!(r.dir().exists(), "the workspace is kept");
    assert!(
        r.session_dir.join("target").exists(),
        "the build output stays with the workspace"
    );
    // the item is `{workspace, proposal, commits: [{id, subject}]}`
    let item = u.item();
    assert_eq!(item["item"], "unlanded");
    assert_eq!(item["proposal"], "P1");
    assert_eq!(item["commits"][0]["subject"], "a late fix");
    assert!(item["workspace"].as_str().unwrap().ends_with("wt"));
    // the agent sees it as data, and the status says so
    assert!(
        r.flow
            .notices()
            .iter()
            .any(|n| n["item"] == "unlanded" && n["commits"][0]["subject"] == "a late fix")
    );
    assert!(r.flow.status_line().unwrap().0.contains("never landed"));
    // reported once, not on every look
    assert!(matches!(r.flow.prune(false, true), Pruned::Waiting(_)));
    // proposing the late work clears the report: it is a new changeset now
    r.flow.draft("late", &Map::new()).unwrap();
    r.flow.proposed("P2");
    assert!(r.flow.unlanded().is_none());
}

#[test]
fn uncommitted_changes_keep_the_workspace_too() {
    let r = proposed_rig(on());
    settle_as(&r, "confirmed");
    r.write("src/scratch.rs", "// not committed\n");
    let Pruned::Kept(u) = r.flow.prune(false, false) else {
        panic!("uncommitted work is kept")
    };
    assert!(u.uncommitted);
    assert!(u.commits.is_empty());
    assert!(r.dir().exists());
}

#[test]
fn a_late_commit_whose_patch_is_on_the_target_is_not_unlanded() {
    let r = proposed_rig(on());
    // the same patch lands on the target by another route (as a squash of a later proposal would)
    r.commit("src/late.rs", "pub fn late() {}\n", "a late fix");
    let tmp = tempfile::tempdir().unwrap();
    let other = tmp.path().join("other");
    git(
        tmp.path(),
        &[
            "clone",
            "-q",
            r.origin.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    std::fs::create_dir_all(other.join("src")).unwrap();
    std::fs::write(other.join("src/late.rs"), "pub fn late() {}\n").unwrap();
    git(&other, &["add", "-A"]);
    git(
        &other,
        &["commit", "-q", "-m", "late fix, landed by the lane"],
    );
    git(&other, &["push", "-q", "origin", "HEAD:main"]);
    settle_as(&r, "confirmed");
    assert_eq!(r.flow.prune(false, false), Pruned::Removed);
}

#[test]
fn a_clean_workspace_at_the_proposed_head_prunes_without_a_report() {
    let r = proposed_rig(on());
    settle_as(&r, "confirmed");
    assert_eq!(r.flow.prune(false, false), Pruned::Removed);
    assert!(r.flow.unlanded().is_none());
}

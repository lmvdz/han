//! The landing lane (R33 §3, §4, §8) against real git (a bare remote, a work repository, `GitVcs`), the fake node
//! of `common`, and a fake `Checks` backend that the test scripts. The lane's pull request and its GitHub
//! backend are in `lane_github.rs`.

mod common;

use han_proc::NoWindow as _;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use han_exec::actions::{self, VcsLand};
use han_exec::checks::{CheckError, CheckRequest, CheckRun, GithubChecks, Stage};
use han_exec::github::{GithubMinter, JwtSource};
use han_exec::{
    Action, ActionCtx, Checks, ChecksConfig, Deadlines, ExecConfig, Executor, ExecutorConfig,
    GithubApp, IdempotencyKey, Lane, NeedsApproval, Outcome, PlanError, RepoTarget, Secret,
};
use han_vcs::{GitVcs, Vcs};
use serde_json::{Value, json};

const REF: &str = "refs/heads/main";
const T0: i64 = 1_792_000_000;

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

// ───────────────────────── the world ─────────────────────────

struct World {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    remote: PathBuf,
    work: PathBuf,
    base: String,
}

fn advance(work: &Path, remote: &Path, file: &str, content: &str) -> String {
    git(work, &["checkout", "-q", "main"]);
    std::fs::write(work.join(file), content).unwrap();
    git(work, &["add", "-A"]);
    git(work, &["commit", "-q", "-m", "tip"]);
    git(work, &["push", "-q", remote.to_str().unwrap(), "main"]);
    git(work, &["rev-parse", "HEAD"])
}

impl World {
    fn new() -> World {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let remote = root.join("remote.git");
        let work = root.join("work");
        std::fs::create_dir_all(&remote).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        git(&remote, &["init", "-q", "--bare", "-b", "main"]);
        git(&work, &["init", "-q", "-b", "main"]);
        for (f, c) in [("a.txt", "a\n"), ("b.txt", "b\n"), ("c.txt", "c\n")] {
            std::fs::write(work.join(f), c).unwrap();
        }
        let ten: String = (1..=10).map(|i| format!("line {i}\n")).collect();
        std::fs::write(work.join("m.txt"), ten).unwrap();
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "-q", "-m", "base"]);
        let base = git(&work, &["rev-parse", "HEAD"]);
        git(&work, &["push", "-q", remote.to_str().unwrap(), "main"]);
        World {
            _tmp: tmp,
            root,
            remote,
            work,
            base,
        }
    }

    /// A change off the base editing one file; returns its head commit.
    fn change(&self, branch: &str, file: &str, content: &str) -> String {
        git(&self.work, &["checkout", "-q", "-b", branch, &self.base]);
        if let Some(parent) = Path::new(file).parent() {
            std::fs::create_dir_all(self.work.join(parent)).unwrap();
        }
        std::fs::write(self.work.join(file), content).unwrap();
        git(&self.work, &["add", "-A"]);
        git(&self.work, &["commit", "-q", "-m", branch]);
        let to = git(&self.work, &["rev-parse", "HEAD"]);
        git(&self.work, &["checkout", "-q", "main"]);
        to
    }

    fn advance(&self, file: &str, content: &str) -> String {
        advance(&self.work, &self.remote, file, content)
    }

    /// Two commits off the base on one branch, the second on top of the first and editing the same file:
    /// the changesets of a follow-up proposal. Returns `(first, second)`.
    fn chain(&self, branch: &str) -> (String, String) {
        git(&self.work, &["checkout", "-q", "-b", branch, &self.base]);
        std::fs::write(self.work.join("a.txt"), "a, first\n").unwrap();
        git(&self.work, &["commit", "-q", "-am", "first"]);
        let first = git(&self.work, &["rev-parse", "HEAD"]);
        std::fs::write(self.work.join("a.txt"), "a, second\n").unwrap();
        std::fs::write(self.work.join("b.txt"), "b, second\n").unwrap();
        git(&self.work, &["add", "-A"]);
        git(&self.work, &["commit", "-q", "-m", "second"]);
        let second = git(&self.work, &["rev-parse", "HEAD"]);
        git(&self.work, &["checkout", "-q", "main"]);
        (first, second)
    }

    /// Whether the remote holds the commit.
    fn remote_has(&self, commit: &str) -> bool {
        Command::new("git")
            .no_window()
            .current_dir(&self.remote)
            .args(["cat-file", "-e", commit])
            .status()
            .unwrap()
            .success()
    }

    fn tip(&self) -> String {
        git(&self.remote, &["rev-parse", REF])
    }

    fn show(&self, rev: &str, file: &str) -> String {
        git(&self.remote, &["show", &format!("{rev}:{file}")])
    }
}

// ───────────────────────── the scripted checks ─────────────────────────

#[derive(Default)]
struct ChecksState {
    on_fast_poll: Option<Arc<dyn Fn() + Send + Sync>>,
    fast: Vec<CheckRun>,
    full: Vec<CheckRun>,
    /// What `full` says about one commit, where it differs from the default.
    full_by_commit: HashMap<String, Vec<CheckRun>>,
    /// What `fast` becomes once a re-run was asked for.
    after_rerun: Option<Vec<CheckRun>>,
    requests: Vec<(Stage, String)>,
    started: Vec<String>,
    reruns: Vec<Vec<String>>,
    broken: bool,
    /// The commits whose `full` run was asked for again.
    full_reruns: Vec<String>,
    /// The backend cannot run `full` again.
    no_full_rerun: bool,
    /// What a commit's `full` run becomes once it was asked for again.
    after_full_rerun: HashMap<String, Vec<CheckRun>>,
    /// Polls that still say `in_progress` after a re-run, per commit.
    rerun_pending: HashMap<String, u32>,
}

#[derive(Default)]
struct FakeChecks(Mutex<ChecksState>);

fn run(name: &str, conclusion: &str) -> CheckRun {
    CheckRun::new(name, conclusion, Some(format!("https://ci.invalid/{name}")))
}

impl FakeChecks {
    fn fast(&self, runs: Vec<CheckRun>) {
        self.0.lock().unwrap().fast = runs;
    }
    fn full(&self, runs: Vec<CheckRun>) {
        self.0.lock().unwrap().full = runs;
    }
    fn full_for(&self, commit: &str, runs: Vec<CheckRun>) {
        self.0
            .lock()
            .unwrap()
            .full_by_commit
            .insert(commit.to_owned(), runs);
    }
    /// What the commit's `full` run becomes once it was asked for again.
    fn after_full_rerun(&self, commit: &str, runs: Vec<CheckRun>) {
        self.0
            .lock()
            .unwrap()
            .after_full_rerun
            .insert(commit.to_owned(), runs);
    }
    fn no_full_rerun(&self) {
        self.0.lock().unwrap().no_full_rerun = true;
    }
    fn full_reruns(&self) -> Vec<String> {
        self.0.lock().unwrap().full_reruns.clone()
    }
    fn started(&self) -> Vec<String> {
        self.0.lock().unwrap().started.clone()
    }
    fn commits(&self, stage: Stage) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for (s, c) in &self.0.lock().unwrap().requests {
            if *s == stage && !v.contains(c) {
                v.push(c.clone());
            }
        }
        v
    }
}

#[async_trait::async_trait]
impl Checks for FakeChecks {
    fn name(&self) -> &'static str {
        "fake"
    }
    async fn start(&self, req: &CheckRequest) -> Result<(), CheckError> {
        self.0.lock().unwrap().started.push(req.commit.clone());
        Ok(())
    }
    async fn poll(&self, req: &CheckRequest) -> Result<Vec<CheckRun>, CheckError> {
        let mut st = self.0.lock().unwrap();
        if req.stage == Stage::Fast
            && let Some(hook) = st.on_fast_poll.take()
        {
            hook();
        }
        st.requests.push((req.stage, req.commit.clone()));
        if st.broken {
            return Err(CheckError("the backend is down".into()));
        }
        if req.stage == Stage::Full
            && let Some(n) = st.rerun_pending.get_mut(&req.commit)
            && *n > 0
        {
            *n -= 1;
            return Ok(req.names.iter().map(|n| run(n, "in_progress")).collect());
        }
        Ok(match req.stage {
            Stage::Fast => st.fast.clone(),
            Stage::Full => st
                .full_by_commit
                .get(&req.commit)
                .cloned()
                .unwrap_or_else(|| st.full.clone()),
        })
    }
    async fn rerun(&self, req: &CheckRequest, names: &[String]) -> Result<bool, CheckError> {
        let mut st = self.0.lock().unwrap();
        if req.stage == Stage::Full {
            st.full_reruns.push(req.commit.clone());
            if st.no_full_rerun {
                return Ok(false);
            }
            st.rerun_pending.insert(req.commit.clone(), 1);
            if let Some(next) = st.after_full_rerun.remove(&req.commit) {
                st.full_by_commit.insert(req.commit.clone(), next);
            }
            return Ok(true);
        }
        st.reruns.push(names.to_vec());
        if let Some(next) = st.after_rerun.take() {
            st.fast = next;
            return Ok(true);
        }
        Ok(false)
    }
}

// ───────────────────────── the rig ─────────────────────────

struct Rig {
    w: World,
    vcs: Arc<GitVcs>,
    node: FakeNode,
    clock: Arc<TestClock>,
    wall: Arc<TestWall>,
    checks: Arc<FakeChecks>,
    config: ExecConfig,
    state: PathBuf,
    minter: Option<Arc<GithubMinter>>,
}

impl Rig {
    fn new() -> Rig {
        Rig::with(|_| {})
    }

    fn with(tweak: impl FnOnce(&mut RepoTarget)) -> Rig {
        let w = World::new();
        let mut repo = RepoTarget {
            landing_refs: vec![REF.into()],
            checks: ChecksConfig {
                required: vec!["quick".into()],
                full: vec![],
                poll_secs: 0,
                appear_secs: 300,
                rerun: 0,
                ..ChecksConfig::default()
            },
            // The tests that move the clock read the items of a change state by state: the periodic
            // publishing is switched on where it is the subject.
            republish_secs: 0,
            ..RepoTarget::new("local:origin", w.remote.to_string_lossy().into_owned())
        };
        tweak(&mut repo);
        Rig {
            vcs: Arc::new(GitVcs::new(w.root.join("store"))),
            state: w.root.join("state"),
            w,
            node: FakeNode::default(),
            clock: Arc::new(TestClock::default()),
            wall: Arc::new(TestWall::default()),
            checks: Arc::new(FakeChecks::default()),
            minter: None,
            config: ExecConfig {
                repos: vec![repo],
                branch_prefixes: vec!["han/".into()],
                ..ExecConfig::default()
            },
        }
    }

    fn boot(&self) -> (Arc<Executor>, Lane) {
        self.boot_with(han_exec::actions::builtin())
    }

    fn boot_with(&self, actions: Vec<Arc<dyn Action>>) -> (Arc<Executor>, Lane) {
        // Tests run in threads of one process: a `git` child that another test forks while this state's
        // lock is open holds it for an instant after the executor is dropped, so a restart may have to ask
        // twice.
        let mut tries = 0;
        let exec = loop {
            let opened = Executor::open(
                ExecutorConfig {
                    actor: EXEC.into(),
                    actions: actions.clone(),
                    gate: Arc::new(NeedsApproval),
                    config: Arc::new(self.config.clone()),
                    vcs: self.vcs.clone(),
                    github: self.minter.clone(),
                    state_dir: self.state.clone(),
                    stall_bound: Duration::from_secs(10),
                },
                Arc::new(self.node.clone()),
                self.clock.clone(),
            );
            match opened {
                Ok(e) => break Arc::new(e),
                Err(han_exec::ExecError::Ledger(han_exec::LedgerError::Held(_))) if tries < 100 => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("{e}"),
            }
        };
        let lane = Lane::new(exec.clone(), self.checks.clone(), self.wall.clone());
        (exec, lane)
    }

    /// The plan of landing `to` (a head from `World::change`), with its bundle in the space.
    fn plan(&self, to: &str, extra: Value) -> Value {
        let base = self.w.base.clone();
        self.plan_from(&base, &base, to, extra)
    }

    /// The plan of landing `base..to`, with the bundle `bundle_base..to` in the space (`bundleBase` is named
    /// when it is not the base).
    fn plan_from(&self, base: &str, bundle_base: &str, to: &str, extra: Value) -> Value {
        let delta = self
            .vcs
            .delta(&self.w.work, base, to)
            .unwrap()
            .digest()
            .unwrap();
        let bundle = self.vcs.bundle(&self.w.work, bundle_base, to).unwrap();
        let hash = self.node.put_blob(&bundle);
        let mut args = json!({"ref": REF, "base": base, "to": to, "deltaDigest": delta,
            "tier": 1, "rebase": "path-disjoint", "title": format!("Land {}", &to[..7])});
        if bundle_base != base {
            args["bundleBase"] = json!(bundle_base);
        }
        for (k, v) in extra.as_object().cloned().unwrap_or_default() {
            args[k] = v;
        }
        json!({"action": "vcs.land", "target": "local:origin", "args": args,
            "argsBlob": {"hash": hash, "size": bundle.len(), "type": han_vcs::BUNDLE_MEDIA_TYPE}})
    }

    /// Proposes a change of one file and approves it; returns the proposal id.
    fn land(&self, name: &str, file: &str, content: &str) -> String {
        let to = self.w.change(name, file, content);
        let p = self.node.propose(&self.plan(&to, json!({})));
        self.node.approve(&p);
        p
    }

    fn items(&self, proposal: &str) -> Vec<Value> {
        self.node
            .items("han.land/1")
            .into_iter()
            .filter(|i| i["item"] == "change" && i["proposal"] == proposal)
            .collect()
    }

    fn states(&self, proposal: &str) -> Vec<String> {
        self.items(proposal)
            .iter()
            .map(|i| i["state"].as_str().unwrap().to_owned())
            .collect()
    }

    fn last(&self, proposal: &str) -> Value {
        self.items(proposal).pop().expect("an item")
    }

    /// The `effect.cancel`s the executor sent: `(proposal, reason, evidence)`.
    fn cancels(&self) -> Vec<(String, String, Option<han_node::api::EvidenceRef>)> {
        self.node
            .state()
            .commands
            .iter()
            .filter_map(|c| match c {
                han_node::api::Command::EffectCancel {
                    proposal,
                    reason,
                    evidence,
                    ..
                } => Some((
                    proposal.clone(),
                    reason.clone().unwrap_or_default(),
                    evidence.clone(),
                )),
                _ => None,
            })
            .collect()
    }

    /// The evidence of each `effect.outcome` the executor sent: `(claim, evidence)`.
    fn outcome_evidence(&self) -> Vec<(String, Option<han_node::api::EvidenceRef>)> {
        self.node
            .state()
            .commands
            .iter()
            .filter_map(|c| match c {
                han_node::api::Command::EffectOutcome {
                    claim, evidence, ..
                } => Some((claim.clone(), evidence.clone())),
                _ => None,
            })
            .collect()
    }

    /// The JSON a `BlobRef` of an item points at, from the space.
    fn blob(&self, r: &Value) -> Value {
        let hash = r["hash"].as_str().unwrap();
        let bytes = self.node.state().blobs.get(hash).cloned().unwrap();
        assert_eq!(han_core::ids::sha256_hex(&bytes), hash);
        assert_eq!(r["size"], bytes.len());
        serde_json::from_slice(&bytes).unwrap()
    }
}

fn green() -> Vec<CheckRun> {
    vec![run("quick", "success")]
}

// ───────────────────────── tests ─────────────────────────

#[tokio::test]
async fn happy_path_queued_testing_landing_landed_confirmed() {
    let rig = Rig::new();
    rig.checks.fast(green());
    let p = rig.land("one", "a.txt", "a, edited\n");
    let (_exec, lane) = rig.boot();
    let rep = lane.pass().await.unwrap();
    assert!(
        rep.stalled.is_empty() && rep.escalated.is_empty(),
        "{rep:?}"
    );

    // The status items of the change: one per state, `seq` counting from 1.
    assert_eq!(
        rig.states(&p),
        ["queued", "testing", "landing", "landed", "confirmed"]
    );
    let seqs: Vec<u64> = rig
        .items(&p)
        .iter()
        .map(|i| i["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, [1, 2, 3, 4, 5]);
    for i in rig.items(&p) {
        assert_eq!(i["lane"], "local:origin#refs/heads/main");
        assert_eq!(i["ref"], REF);
        let live = i["state"] != "confirmed";
        assert_eq!(i.get("deadline").is_some(), live, "{i}");
        assert!(han_core::grammar::is_time(i["at"].as_str().unwrap()));
    }
    // The testing item names the commit and the lane branch; the landing item carries the evidence.
    let testing = &rig.items(&p)[1];
    let commit = testing["commit"].as_str().unwrap().to_owned();
    assert_eq!(testing["branch"], "han/lane/main/p1");
    assert_eq!(rig.w.tip(), commit);
    assert_eq!(
        git(&rig.w.remote, &["rev-parse", "refs/heads/han/lane/main/p1"]),
        commit
    );
    let evidence = rig.blob(&rig.items(&p)[2]["evidence"]);
    assert_eq!(evidence["commit"], commit.as_str());
    assert_eq!(evidence["backend"], "fake");
    assert_eq!(evidence["checks"][0]["name"], "quick");
    assert_eq!(evidence["checks"][0]["conclusion"], "success");

    // What landed: the approved edit as one deterministic commit on the tip (author, time, message).
    assert_eq!(rig.w.show("main", "a.txt"), "a, edited");
    let info = git(
        &rig.w.remote,
        &["log", "-1", "--format=%an <%ae>|%cn <%ce>|%at|%B", &commit],
    );
    assert!(
        info.starts_with(
            "@agent:a.localhost <agent@a.localhost>|@agent:a.localhost <agent@a.localhost>|"
        ),
        "{info}"
    );
    let at = han_core::grammar::epoch_seconds("2026-10-15T10:00:00Z").unwrap();
    assert!(info.contains(&format!("|{at}|")), "{info}");
    assert!(info.contains("HAN-Proposal: p1"), "{info}");
    // The outcome carries the same checks evidence, a blob of the space (G-01).
    let sent = rig.outcome_evidence();
    assert_eq!(sent.len(), 1);
    let sent = sent[0].1.clone().expect("the outcome carries evidence");
    let shown = &rig.items(&p)[2]["evidence"];
    assert_eq!(shown["hash"], sent.hash.as_str());
    assert_eq!(shown["size"], sent.size);
    assert_eq!(shown["type"], sent.media_type.as_str());
    // The claim went through the normal path and the home heard the outcome.
    assert_eq!(rig.node.claims(), 1);
    assert_eq!(
        rig.node.state().outcomes,
        vec![("p1".into(), "succeeded".into(), "yes".into())]
    );
    // The lane's own item.
    let lane_items: Vec<Value> = rig
        .node
        .items("han.land/1")
        .into_iter()
        .filter(|i| i["item"] == "lane")
        .collect();
    assert!(!lane_items.is_empty());
    assert_eq!(lane_items.last().unwrap()["depth"], 0);
    // Nothing more happens, and nothing more is published.
    let before = rig.node.items("han.land/1").len();
    lane.pass().await.unwrap();
    assert_eq!(rig.node.items("han.land/1").len(), before);
}

impl Rig {
    /// The change summary of landing `to`, as a host builds it (architecture.md P-17), as a plan's `args.change`.
    fn summary(&self, to: &str) -> Value {
        let d = self.vcs.delta(&self.w.work, &self.w.base, to).unwrap();
        let n = self
            .vcs
            .delta_lines(&self.w.work, &self.w.base, to, &d)
            .unwrap();
        serde_json::to_value(han_vcs::land_change("local:origin", &d, &n).unwrap()).unwrap()
    }
}

#[tokio::test]
async fn a_change_summary_that_matches_the_bundle_lands_and_is_the_one_the_body_carries() {
    let rig = Rig::new();
    rig.checks.fast(green());
    let to = rig.w.change("one", "a.txt", "a, edited\n");
    let change = rig.summary(&to);
    assert_eq!(change["files"][0]["path"], "a.txt");
    assert_eq!(
        change["files"][0]["lines"], 2,
        "one line added, one removed"
    );
    let p = rig.node.propose(&rig.plan(&to, json!({"change": change})));
    assert!(
        rig.node.row(&p).change_digest.is_some(),
        "the node digests the body's copy"
    );
    rig.node.approve(&p);
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    assert_eq!(
        rig.states(&p),
        ["queued", "testing", "landing", "landed", "confirmed"]
    );
}

#[tokio::test]
async fn an_understated_summary_is_rejected_before_anything_is_tested() {
    let rig = Rig::new();
    rig.checks.fast(green());
    let to = rig.w.change("one", "a.txt", "a, edited\n");
    let mut change = rig.summary(&to);
    change["files"][0]["lines"] = json!(1);
    let p = rig.node.propose(&rig.plan(&to, json!({"change": change})));
    rig.node.approve(&p);
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    assert_eq!(rig.states(&p), ["queued", "rejected"]);
    let last = rig.last(&p);
    assert_eq!(last["reason"], "policy");
    assert!(
        last["detail"].as_str().unwrap().contains("understates"),
        "{last}"
    );
    assert_eq!(rig.node.claims(), 0);
    assert_eq!(rig.cancels().len(), 1);
}

#[tokio::test]
async fn a_summary_for_other_files_or_one_the_body_does_not_carry_is_rejected() {
    let rig = Rig::new();
    rig.checks.fast(green());
    // The summary names a file the bundle does not change.
    let to = rig.w.change("one", "a.txt", "a, edited\n");
    let mut change = rig.summary(&to);
    change["files"][0]["path"] = json!("b.txt");
    let p = rig.node.propose(&rig.plan(&to, json!({"change": change})));
    rig.node.approve(&p);
    // The body's summary is not the plan's: the proposer signed one for Policy and another into the plan.
    let to2 = rig.w.change("two", "c.txt", "c, edited\n");
    let q = rig
        .node
        .propose(&rig.plan(&to2, json!({"change": rig.summary(&to2)})));
    rig.node
        .state()
        .rows
        .iter_mut()
        .find(|r| r.proposal == q)
        .unwrap()
        .change_digest = Some("0".repeat(64));
    rig.node.approve(&q);
    // A plan that carries a summary the body lacks is the same lie the other way round.
    let to3 = rig.w.change("three", "m.txt", "line 1, edited\n");
    let r = rig
        .node
        .propose(&rig.plan(&to3, json!({"change": rig.summary(&to3)})));
    rig.node
        .state()
        .rows
        .iter_mut()
        .find(|x| x.proposal == r)
        .unwrap()
        .change_digest = None;
    rig.node.approve(&r);
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    for (proposal, needle) in [
        (&p, "is not the change's"),
        (&q, "not the plan's"),
        (&r, "not the plan's"),
    ] {
        assert_eq!(rig.states(proposal), ["queued", "rejected"], "{proposal}");
        let last = rig.last(proposal);
        assert_eq!(last["reason"], "policy");
        assert!(last["detail"].as_str().unwrap().contains(needle), "{last}");
    }
    assert_eq!(rig.node.claims(), 0);
}

#[tokio::test]
async fn the_ref_moving_by_a_path_disjoint_change_prepares_again_and_lands() {
    let rig = Rig::new();
    // `quick` is still running when the ref moves.
    rig.checks.fast(vec![run("quick", "in_progress")]);
    let p = rig.land("one", "a.txt", "a, edited\n");
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    assert_eq!(rig.states(&p), ["queued", "testing"]);
    let first = rig.last(&p)["commit"].as_str().unwrap().to_owned();

    let moved = rig.w.advance("c.txt", "c, moved on\n");
    rig.checks.fast(green());
    lane.pass().await.unwrap();

    assert_eq!(
        rig.states(&p),
        [
            "queued",
            "testing",
            "queued",
            "testing",
            "landing",
            "landed",
            "confirmed"
        ]
    );
    let second = rig.items(&p)[3]["commit"].as_str().unwrap().to_owned();
    assert_ne!(first, second, "tested again on the new tip");
    assert_eq!(rig.w.tip(), second);
    assert_eq!(
        git(&rig.w.remote, &["rev-parse", &format!("{second}^")]),
        moved
    );
    assert_eq!(rig.w.show("main", "a.txt"), "a, edited");
    assert_eq!(rig.w.show("main", "c.txt"), "c, moved on");
    // The first commit was never claimed for: one claim, on the second.
    assert_eq!(rig.node.claims(), 1);
    assert_eq!(rig.checks.commits(Stage::Fast), vec![first, second]);
}

#[tokio::test]
async fn the_ref_moving_by_an_overlapping_change_rejects_with_a_conflict_record() {
    let rig = Rig::new();
    rig.checks.fast(vec![run("quick", "queued")]);
    let p = rig.land("one", "a.txt", "a, edited\n");
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    rig.w.advance("a.txt", "a, edited by someone else\n");
    rig.checks.fast(green());
    lane.pass().await.unwrap();

    assert_eq!(rig.states(&p), ["queued", "testing", "queued", "rejected"]);
    let last = rig.last(&p);
    assert_eq!(last["reason"], "conflict");
    let record = rig.blob(&last["conflict"]);
    assert_eq!(record["changeset"], "p1");
    assert_eq!(record["conflicts"][0]["path"], "a.txt");
    assert_eq!(record["conflicts"][0]["kind"], "content");
    assert_eq!(record["reason"], "merge_conflict");
    assert_eq!(rig.node.claims(), 0);
    assert_eq!(rig.w.show("main", "a.txt"), "a, edited by someone else");
    // The rejection ends the effect: the executor cancels it, with the reason and the conflict record.
    let cancels = rig.cancels();
    assert_eq!(cancels.len(), 1, "{cancels:?}");
    assert_eq!(cancels[0].0, p);
    assert!(cancels[0].1.starts_with("conflict: "), "{:?}", cancels[0]);
    let proof = cancels[0].2.clone().expect("the conflict record");
    assert_eq!(proof.hash, last["conflict"]["hash"].as_str().unwrap());
    assert_eq!(rig.node.row(&p).phase, "done");
    // Told once.
    rig.wall.advance(1);
    lane.pass().await.unwrap();
    assert_eq!(rig.cancels().len(), 1);
}

#[tokio::test]
async fn a_clean_merge_that_is_not_path_disjoint_is_gated_again() {
    let rig = Rig::new();
    rig.checks.fast(vec![run("quick", "queued")]);
    let lines = |first: &str, last: &str| {
        let mut v: Vec<String> = (1..=10).map(|i| format!("line {i}")).collect();
        v[0] = first.into();
        v[9] = last.into();
        v.join("\n") + "\n"
    };
    let p = rig.land("one", "m.txt", &lines("first, edited", "line 10"));
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    // Someone else edits the other end of the same file: git merges it, but it is not what was approved.
    rig.w.advance("m.txt", &lines("line 1", "last, edited"));
    rig.checks.fast(green());
    lane.pass().await.unwrap();

    let last = rig.last(&p);
    assert_eq!(last["state"], "rejected");
    assert_eq!(last["reason"], "conflict");
    assert!(
        last["detail"]
            .as_str()
            .unwrap()
            .contains("not path-disjoint")
    );
    let record = rig.blob(&last["conflict"]);
    assert_eq!(record["reason"], "not_path_disjoint");
    assert_eq!(record["touched"][0], "m.txt");
    assert_eq!(rig.node.claims(), 0);
}

#[tokio::test]
async fn a_lost_compare_and_swap_is_reported_not_started_and_prepares_again() {
    let rig = Rig::new();
    rig.checks.fast(green());
    let p = rig.land("one", "a.txt", "a, edited\n");
    let (exec, lane) = rig.boot();
    // The ref moves just as the claim is appended: after the lane's own check, before the push.
    let (work, remote) = (rig.w.work.clone(), rig.w.remote.clone());
    rig.node.state().on_claim = Some(Arc::new(move || {
        advance(&work, &remote, "c.txt", "c, moved under the claim\n");
    }));
    lane.pass().await.unwrap();

    // Attempt 1 lost the compare-and-swap: failed/no, the effect opened again; attempt 2 landed.
    assert_eq!(
        rig.node.state().outcomes,
        vec![
            ("p1".into(), "failed".into(), "no".into()),
            ("p1".into(), "succeeded".into(), "yes".into())
        ]
    );
    assert_eq!(
        rig.states(&p),
        [
            "queued",
            "testing",
            "landing",
            "queued",
            "testing",
            "landing",
            "landed",
            "confirmed"
        ]
    );
    let attempts = exec.with_ledger(|l| l.attempts_of(SPACE, "p1")).unwrap();
    assert_eq!(attempts.len(), 2);
    let lanes: Vec<_> = attempts.iter().map(|a| a.lane.clone().unwrap()).collect();
    assert_ne!(lanes[0].onto, lanes[1].onto);
    assert_eq!(rig.w.tip(), lanes[1].commit);
    assert_eq!(rig.w.show("main", "c.txt"), "c, moved under the claim");
    assert_eq!(rig.w.show("main", "a.txt"), "a, edited");
}

#[tokio::test]
async fn a_red_required_check_rejects_with_the_evidence() {
    let rig = Rig::new();
    rig.checks
        .fast(vec![run("quick", "failure"), run("lint", "success")]);
    let p = rig.land("one", "a.txt", "a, edited\n");
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();

    assert_eq!(rig.states(&p), ["queued", "testing", "rejected"]);
    let last = rig.last(&p);
    assert_eq!(last["reason"], "test");
    assert!(last["detail"].as_str().unwrap().contains("quick"));
    let evidence = rig.blob(&last["evidence"]);
    assert_eq!(evidence["backend"], "fake");
    assert_eq!(evidence["commit"], rig.items(&p)[1]["commit"]);
    // Sorted by name, with the links.
    assert_eq!(evidence["checks"][0]["name"], "lint");
    assert_eq!(evidence["checks"][1]["name"], "quick");
    assert_eq!(evidence["checks"][1]["conclusion"], "failure");
    assert_eq!(evidence["checks"][1]["url"], "https://ci.invalid/quick");
    assert_eq!(rig.node.claims(), 0);
    assert_ne!(rig.w.tip(), rig.items(&p)[1]["commit"].as_str().unwrap());
    let cancels = rig.cancels();
    assert_eq!(cancels.len(), 1, "{cancels:?}");
    assert!(cancels[0].1.starts_with("test: "), "{:?}", cancels[0]);
    assert!(cancels[0].1.contains("quick"), "{:?}", cancels[0]);
    assert_eq!(
        cancels[0].2.as_ref().expect("the failing checks").hash,
        last["evidence"]["hash"].as_str().unwrap()
    );
}

#[tokio::test]
async fn a_configured_rerun_is_tried_once_before_a_red_check_rejects() {
    let rig = Rig::with(|r| r.checks.rerun = 1);
    rig.checks.fast(vec![run("quick", "failure")]);
    rig.checks.0.lock().unwrap().after_rerun = Some(green());
    let p = rig.land("one", "a.txt", "a, edited\n");
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    assert_eq!(
        rig.checks.0.lock().unwrap().reruns,
        vec![vec!["quick".to_owned()]]
    );
    assert_eq!(rig.last(&p)["state"], "confirmed");

    // With no re-run left, the same red rejects.
    let rig = Rig::with(|r| r.checks.rerun = 1);
    rig.checks.fast(vec![run("quick", "failure")]);
    let p = rig.land("one", "a.txt", "a, edited\n");
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    assert_eq!(rig.checks.0.lock().unwrap().reruns.len(), 1);
    assert_eq!(rig.last(&p)["reason"], "test");
}

#[tokio::test]
async fn a_required_check_that_never_appears_is_stalled_not_silent() {
    // The #99 regression: GitHub ran no pull_request CI, so the required check never came.
    let rig = Rig::new();
    rig.checks.fast(vec![run("lint", "success")]);
    let p = rig.land("one", "a.txt", "a, edited\n");
    let (_exec, lane) = rig.boot();
    let rep = lane.pass().await.unwrap();
    assert!(rep.stalled.is_empty());
    rig.wall.advance(299);
    assert!(
        lane.pass().await.unwrap().stalled.is_empty(),
        "still within appear_secs"
    );
    assert_eq!(rig.states(&p), ["queued", "testing"]);

    rig.wall.advance(2);
    let rep = lane.pass().await.unwrap();
    assert_eq!(rep.stalled.len(), 1, "{rep:?}");
    assert!(rep.stalled[0].1.contains("quick") && rep.stalled[0].1.contains("not appeared"));
    let last = rig.last(&p);
    assert_eq!(last["state"], "testing");
    assert!(
        last["stalled"]
            .as_str()
            .unwrap()
            .contains("nothing is pending"),
        "{last}"
    );
    assert_eq!(last["seq"], 3);
    assert_eq!(
        lane.changes().unwrap()[0].stalled,
        last["stalled"].as_str().map(str::to_owned)
    );
    // Told once, not on every pass.
    let n = rig.items(&p).len();
    rig.wall.advance(10);
    assert!(lane.pass().await.unwrap().stalled.is_empty());
    assert_eq!(rig.items(&p).len(), n);
    assert_eq!(rig.node.claims(), 0);

    // The check turns up late: the stall clears and the change lands.
    rig.checks.fast(green());
    lane.pass().await.unwrap();
    assert_eq!(rig.last(&p)["state"], "confirmed");
    assert!(rig.last(&p).get("stalled").is_none());
}

#[tokio::test]
async fn a_change_past_a_states_deadline_is_stalled() {
    let rig = Rig::with(|r| {
        r.deadlines = Deadlines {
            testing_secs: 600,
            ..Deadlines::default()
        };
    });
    rig.checks.fast(vec![run("quick", "in_progress")]);
    let p = rig.land("one", "a.txt", "a, edited\n");
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    let testing = rig.last(&p);
    let dl = han_core::grammar::epoch_seconds(testing["deadline"].as_str().unwrap()).unwrap();
    assert_eq!(dl, T0 + 600);
    rig.wall.advance(601);
    let rep = lane.pass().await.unwrap();
    assert_eq!(rep.stalled.len(), 1);
    assert!(
        rep.stalled[0]
            .1
            .contains("past the testing deadline of 600 s"),
        "{rep:?}"
    );
    assert!(rig.last(&p)["stalled"].is_string());

    // A backend that cannot be read is a stall too, with its reason.
    let rig = Rig::new();
    rig.checks.0.lock().unwrap().broken = true;
    let p = rig.land("one", "a.txt", "a, edited\n");
    let (_exec, lane) = rig.boot();
    let rep = lane.pass().await.unwrap();
    assert!(rep.stalled[0].1.contains("the backend is down"), "{rep:?}");
    assert_eq!(rig.last(&p)["state"], "testing");
}

#[tokio::test]
async fn a_stalled_head_past_its_deadline_yields_then_times_out() {
    let rig = Rig::with(|r| {
        r.deadlines = Deadlines {
            testing_secs: 600,
            stall_secs: 1000,
            ..Deadlines::default()
        };
    });
    rig.checks.fast(vec![run("quick", "in_progress")]);
    let one = rig.land("one", "a.txt", "a, edited\n");
    let two = rig.land("two", "b.txt", "b, edited\n");
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    // The head holds the slot while it is within its deadline.
    assert_eq!(rig.last(&one)["state"], "testing");
    assert_eq!(rig.last(&two)["state"], "queued");
    assert_eq!(rig.last(&two)["detail"], "waiting for the lane's slot");
    rig.wall.advance(601);
    lane.pass().await.unwrap();
    // Stalled past its deadline, the head steps aside: the change behind it is tested, and the head stays
    // visible as stalled.
    assert!(rig.last(&one)["stalled"].is_string());
    assert_eq!(rig.last(&one)["state"], "testing");
    assert_eq!(rig.last(&two)["state"], "testing");
    assert!(rig.cancels().is_empty());

    // Still stalled at the hard bound (a pass later than the stall began): rejected, `timeout`.
    rig.wall.advance(1000);
    lane.pass().await.unwrap();
    let last = rig.last(&one);
    assert_eq!(last["state"], "rejected");
    assert_eq!(last["reason"], "timeout");
    let cancels = rig.cancels();
    assert_eq!(cancels.len(), 1, "{cancels:?}");
    assert_eq!(cancels[0].0, one);
    assert!(cancels[0].1.starts_with("timeout: "), "{:?}", cancels[0]);
    assert_eq!(rig.node.row(&one).phase, "done");
    // The change behind it was never held up: it lands.
    rig.checks.fast(green());
    lane.pass().await.unwrap();
    assert_eq!(rig.last(&two)["state"], "confirmed");
}

#[tokio::test]
async fn a_stalled_head_that_unstalls_resumes_and_lands() {
    let rig = Rig::with(|r| {
        r.deadlines = Deadlines {
            testing_secs: 600,
            stall_secs: 100_000,
            ..Deadlines::default()
        };
    });
    rig.checks.fast(vec![run("quick", "in_progress")]);
    let one = rig.land("one", "a.txt", "a, edited\n");
    let two = rig.land("two", "b.txt", "b, edited\n");
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    rig.wall.advance(601);
    lane.pass().await.unwrap();
    assert!(rig.last(&one)["stalled"].is_string());
    assert_eq!(rig.last(&two)["state"], "testing");
    // The head's checks turn green before the bound: it lands first, the change behind it prepares again on
    // the new tip, and both land. Nothing was rejected or cancelled.
    rig.checks.fast(green());
    lane.pass().await.unwrap();
    assert_eq!(rig.last(&one)["state"], "confirmed");
    assert!(rig.last(&one).get("stalled").is_none());
    assert_eq!(rig.last(&two)["state"], "confirmed");
    assert_eq!(rig.w.show("main", "a.txt"), "a, edited");
    assert_eq!(rig.w.show("main", "b.txt"), "b, edited");
    assert!(rig.cancels().is_empty());
}

#[tokio::test]
async fn a_red_post_merge_run_leaves_the_change_landed_and_escalates() {
    let rig = Rig::with(|r| r.checks.full = vec!["full".into()]);
    rig.checks.fast(green());
    rig.checks.full(vec![run("full", "in_progress")]);
    let p = rig.land("one", "a.txt", "a, edited\n");
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    // Landed, and not confirmed before a green full run on the landed commit.
    assert_eq!(rig.states(&p), ["queued", "testing", "landing", "landed"]);
    let commit = rig.items(&p)[1]["commit"].as_str().unwrap().to_owned();
    assert_eq!(rig.w.tip(), commit);

    rig.checks.full(vec![run("full", "failure")]);
    let rep = lane.pass().await.unwrap();
    assert_eq!(rep.escalated.len(), 1, "{rep:?}");
    assert!(rep.escalated[0].1.contains("full"));
    let last = rig.last(&p);
    assert_eq!(last["state"], "landed", "it stays landed");
    assert_eq!(last["reason"], "post_merge_failed");
    assert_eq!(last["postMergeFailed"], true);
    assert_eq!(last["escalated"], true);
    let evidence = rig.blob(&last["evidence"]);
    assert_eq!(evidence["commit"], commit.as_str());
    assert_eq!(evidence["checks"][0]["conclusion"], "failure");
    assert_eq!(rig.w.tip(), commit, "nothing was reverted");
    // Told once.
    let n = rig.items(&p).len();
    assert!(lane.pass().await.unwrap().escalated.is_empty());
    assert_eq!(rig.items(&p).len(), n);
    assert_eq!(
        rig.checks.commits(Stage::Full),
        vec![commit],
        "the post-merge run is read for the landed commit"
    );
}

#[tokio::test]
async fn a_green_post_merge_run_confirms() {
    let rig = Rig::with(|r| r.checks.full = vec!["full".into()]);
    rig.checks.fast(green());
    let p = rig.land("one", "a.txt", "a, edited\n");
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    // The full run has not appeared: waiting, then stalled once appear_secs passed.
    assert_eq!(rig.last(&p)["state"], "landed");
    rig.wall.advance(301);
    let rep = lane.pass().await.unwrap();
    assert!(
        rep.stalled[0].1.contains("post-merge check full"),
        "{rep:?}"
    );
    rig.checks.full(vec![run("full", "success")]);
    lane.pass().await.unwrap();
    assert_eq!(
        rig.states(&p),
        [
            "queued",
            "testing",
            "landing",
            "landed",
            "landed",
            "confirmed"
        ]
    );
    let evidence = rig.blob(&rig.last(&p)["evidence"]);
    assert_eq!(evidence["checks"][0]["name"], "full");
}

/// `vcs.land` that lands, and then the process dies before the outcome is journalled as a result.
struct CrashAfterLand {
    reached: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl Action for CrashAfterLand {
    fn kind(&self) -> &'static str {
        "vcs.land"
    }
    fn lane_only(&self) -> bool {
        true
    }
    fn plan_check(&self, plan: &Value, config: &ExecConfig) -> Result<(), PlanError> {
        VcsLand.plan_check(plan, config)
    }
    async fn preflight(&self, plan: &Value, ctx: &ActionCtx) -> Result<(), PlanError> {
        VcsLand.preflight(plan, ctx).await
    }
    async fn run(&self, plan: &Value, key: &IdempotencyKey, ctx: &ActionCtx) -> Outcome {
        let o = VcsLand.run(plan, key, ctx).await;
        assert!(matches!(o, Outcome::Succeeded { .. }), "{o:?}");
        self.reached.notify_one();
        std::future::pending::<()>().await;
        o
    }
    async fn lookup(&self, plan: &Value, key: &IdempotencyKey, ctx: &ActionCtx) -> Option<Outcome> {
        VcsLand.lookup(plan, key, ctx).await
    }
}

#[tokio::test]
async fn a_restart_between_claim_and_outcome_is_answered_from_the_journal() {
    let rig = Rig::new();
    rig.checks.fast(green());
    let p = rig.land("one", "a.txt", "a, edited\n");
    let reached = Arc::new(tokio::sync::Notify::new());
    let (exec, lane) = rig.boot_with(vec![Arc::new(CrashAfterLand {
        reached: reached.clone(),
    })]);
    tokio::select! {
        r = lane.pass() => panic!("the pass should not finish: {r:?}"),
        () = reached.notified() => {}
    }
    // The ref moved, the journal holds the claim with its lane instruction, the home heard nothing.
    let commit = rig.last(&p)["commit"].as_str().unwrap().to_owned();
    assert_eq!(rig.states(&p), ["queued", "testing", "landing"]);
    assert_eq!(rig.w.tip(), commit);
    assert!(rig.node.state().outcomes.is_empty());
    let attempts = exec.with_ledger(|l| l.attempts_of(SPACE, "p1")).unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].state, "launching");
    let instr = attempts[0]
        .lane
        .clone()
        .expect("the lane instruction is journalled");
    assert_eq!(instr.commit, commit);
    assert_eq!(
        git(&rig.w.remote, &["rev-parse", &format!("{commit}^")]),
        instr.onto
    );
    drop(lane);
    drop(exec);

    // A new process: recovery re-attaches the instruction, so `lookup` finds the ref at the commit.
    let (exec, lane) = rig.boot();
    let rec = exec.recover().await.unwrap();
    assert_eq!(
        rec.reported,
        vec![("p1".to_owned(), han_exec::Report::new("succeeded", "yes"))]
    );
    assert_eq!(
        rig.node.state().outcomes,
        vec![("p1".into(), "succeeded".into(), "yes".into())]
    );
    lane.pass().await.unwrap();
    assert_eq!(
        rig.states(&p),
        ["queued", "testing", "landing", "landed", "confirmed"]
    );
    // Nothing was claimed or pushed again.
    assert_eq!(rig.node.claims(), 1);
    assert_eq!(rig.w.tip(), commit);
}

#[tokio::test]
async fn an_unfinished_claim_without_its_instruction_could_not_be_looked_up() {
    // The point of journalling the instruction: `lookup` has nothing to compare the ref with without it.
    let rig = Rig::new();
    let to = rig.w.change("one", "a.txt", "a, edited\n");
    let plan = rig.plan(&to, json!({}));
    let key = IdempotencyKey {
        space: SPACE.into(),
        proposal: "p1".into(),
        attempt: 1,
    };
    let ctx = ActionCtx {
        config: Arc::new(rig.config.clone()),
        vcs: rig.vcs.clone(),
        scratch: rig.w.root.join("scratch"),
        github: None,
        args_blob: None,
        lane: None,
    };
    assert!(VcsLand.lookup(&plan, &key, &ctx).await.is_none());
}

#[tokio::test]
async fn the_generic_claim_loop_never_claims_vcs_land() {
    let rig = Rig::new();
    rig.checks.fast(green());
    let p = rig.land("one", "a.txt", "a, edited\n");
    let (exec, _lane) = rig.boot();
    for _ in 0..3 {
        let r = exec.step().await.unwrap();
        assert!(r.claimed.is_empty() && r.launched.is_empty(), "{r:?}");
        assert!(r.refused.is_empty() && r.waiting.is_empty(), "{r:?}");
    }
    assert_eq!(rig.node.claims(), 0);
    assert_eq!(rig.node.row(&p).phase, "open");
    assert!(actions::VcsLand.lane_only());
}

#[tokio::test]
async fn depends_on_orders_the_changes_and_a_rejected_dependency_rejects() {
    // B depends on A; B was approved first, but waits for A to land.
    let rig = Rig::new();
    rig.checks.fast(green());
    let a = rig.w.change("a", "a.txt", "a, edited\n");
    let b = rig.w.change("b", "b.txt", "b, edited\n");
    let pa = rig.node.propose(&rig.plan(&a, json!({})));
    let pb = rig.node.propose(&rig.plan(
        &b,
        json!({"dependsOn": [{"repo": "local:origin", "proposal": pa}]}),
    ));
    rig.node.approve(&pb);
    rig.node.approve(&pa);
    rig.checks.fast(vec![run("quick", "in_progress")]);
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    assert_eq!(rig.last(&pa)["state"], "testing");
    let waiting = rig.last(&pb);
    assert_eq!(waiting["state"], "queued");
    assert!(
        waiting["detail"]
            .as_str()
            .unwrap()
            .contains("dependency p1"),
        "{waiting}"
    );
    assert_eq!(waiting["position"], 1);
    assert_eq!(rig.node.claims(), 0, "b is not even prepared");
    assert_eq!(rig.checks.commits(Stage::Fast).len(), 1);
    // a lands; b follows in the same pass.
    rig.checks.fast(green());
    lane.pass().await.unwrap();
    assert_eq!(rig.last(&pa)["state"], "confirmed");
    assert_eq!(rig.last(&pb)["state"], "confirmed");
    // Landed in dependency order.
    let log = git(&rig.w.remote, &["log", "--format=%s", "-3", "main"]);
    let titles: Vec<&str> = log.lines().collect();
    assert_eq!(
        titles[..2],
        [format!("Land {}", &b[..7]), format!("Land {}", &a[..7])]
    );

    // A is rejected (the ref changed its file): B is rejected for it.
    let rig = Rig::new();
    rig.checks.fast(green());
    let a = rig.w.change("a", "a.txt", "a, edited\n");
    let b = rig.w.change("b", "b.txt", "b, edited\n");
    let pa = rig.node.propose(&rig.plan(&a, json!({})));
    let pb = rig.node.propose(&rig.plan(
        &b,
        json!({"dependsOn": [{"repo": "local:origin", "proposal": pa}]}),
    ));
    rig.node.approve(&pb);
    rig.node.approve(&pa);
    rig.w.advance("a.txt", "a, edited by someone else\n");
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    lane.pass().await.unwrap();
    assert_eq!(rig.last(&pa)["reason"], "conflict");
    let last = rig.last(&pb);
    assert_eq!(last["state"], "rejected");
    assert_eq!(last["reason"], "dependency");
    assert!(last["detail"].as_str().unwrap().contains("p1"), "{last}");
    assert_eq!(rig.node.claims(), 0);
    let mut reasons: Vec<(String, String)> = rig
        .cancels()
        .into_iter()
        .map(|(p, r, e)| {
            (
                p,
                r.split(':').next().unwrap().to_owned() + if e.is_some() { "+proof" } else { "" },
            )
        })
        .collect();
    reasons.sort();
    assert_eq!(
        reasons,
        [
            (pa.clone(), "conflict+proof".to_owned()),
            (pb.clone(), "dependency".to_owned())
        ]
    );
}

/// A follow-up proposal has its predecessor's `to` as its base, and the lane squashes: that `to` never reaches
/// the remote. The host bundles from the session's first base, so the bundle imports on an executor that never
/// saw the first proposal, as after a restart that lost the executor's store or on another machine.
#[tokio::test]
async fn a_dependent_proposal_lands_on_an_executor_that_never_imported_the_first() {
    let mut rig = Rig::new();
    rig.checks.fast(green());
    let (a, b) = rig.w.chain("chain");
    let base = rig.w.base.clone();
    let pa = rig.node.propose(&rig.plan(&a, json!({})));
    rig.node.approve(&pa);
    let (exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    assert_eq!(rig.last(&pa)["state"], "confirmed");
    // The squashed commit is on the remote; the proposal's own `to` never was.
    assert!(!rig.w.remote_has(&a));
    drop(lane);
    drop(exec);

    // Another executor: an object store of its own, no memory of the first proposal. The follow-up is based
    // on the first `to`, and its bundle starts at the base the remote holds.
    rig.vcs = Arc::new(GitVcs::new(rig.w.root.join("store-2")));
    let pb = rig.node.propose(&rig.plan_from(
        &a,
        &base,
        &b,
        json!({"dependsOn": [{"repo": "local:origin", "proposal": pa}]}),
    ));
    rig.node.approve(&pb);
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    assert_eq!(
        rig.states(&pb).last().unwrap(),
        "confirmed",
        "{:?}",
        rig.last(&pb)
    );
    assert_eq!(rig.w.show("main", "a.txt"), "a, second");
    assert_eq!(rig.w.show("main", "b.txt"), "b, second");
    let log = git(&rig.w.remote, &["log", "--format=%s", "-2", "main"]);
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        [format!("Land {}", &b[..7]), format!("Land {}", &a[..7])]
    );
    assert!(!rig.w.remote_has(&b));
}

/// The failure the fix is for: a bundle that needs the first `to` imports only where the first was imported.
#[tokio::test]
async fn a_bundle_that_needs_the_first_to_does_not_import_on_another_executor() {
    let mut rig = Rig::new();
    rig.checks.fast(green());
    let (a, b) = rig.w.chain("chain");
    let pa = rig.node.propose(&rig.plan(&a, json!({})));
    rig.node.approve(&pa);
    let (exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    assert_eq!(rig.last(&pa)["state"], "confirmed");
    drop(lane);
    drop(exec);
    rig.vcs = Arc::new(GitVcs::new(rig.w.root.join("store-2")));
    let pb = rig.node.propose(&rig.plan_from(
        &a,
        &a,
        &b,
        json!({"dependsOn": [{"repo": "local:origin", "proposal": pa}]}),
    ));
    rig.node.approve(&pb);
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    let last = rig.last(&pb);
    assert_eq!(last["state"], "queued", "{last}");
    assert!(
        last["stalled"]
            .as_str()
            .unwrap()
            .contains("does not hold the base"),
        "{last}"
    );
}

/// The home does not replay stream items: a host that was away when one went out learns the state from the
/// next. A change that is not final is published again, with its `seq`, every `republish_secs`; after a restart
/// every change is, final ones once.
#[tokio::test]
async fn a_change_is_published_again_and_after_a_restart_every_change_once_more() {
    let rig = Rig::with(|r| r.republish_secs = 60);
    rig.checks.fast(vec![run("quick", "in_progress")]);
    let p = rig.land("one", "a.txt", "a, edited\n");
    let (exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    let n = rig.items(&p).len();
    assert_eq!(rig.last(&p)["state"], "testing");
    let seq = rig.last(&p)["seq"].clone();

    // Not due yet, then due: one more item, the same state and `seq`.
    rig.wall.advance(30);
    lane.pass().await.unwrap();
    assert_eq!(rig.items(&p).len(), n);
    rig.wall.advance(31);
    lane.pass().await.unwrap();
    assert_eq!(rig.items(&p).len(), n + 1);
    let again = rig.last(&p);
    assert_eq!((&again["state"], &again["seq"]), (&json!("testing"), &seq));
    assert_ne!(again["at"], rig.items(&p)[n - 1]["at"]);
    // And not again until another interval has passed.
    lane.pass().await.unwrap();
    assert_eq!(rig.items(&p).len(), n + 1);

    // Final: published as it happens, and not on a timer.
    rig.checks.fast(green());
    lane.pass().await.unwrap();
    assert_eq!(rig.last(&p)["state"], "confirmed");
    let m = rig.items(&p).len();
    rig.wall.advance(600);
    lane.pass().await.unwrap();
    assert_eq!(rig.items(&p).len(), m);

    // A restart: the final item goes out once more, with the same `seq`, and then no more.
    let seq = rig.last(&p)["seq"].clone();
    drop(lane);
    drop(exec);
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    assert_eq!(rig.items(&p).len(), m + 1);
    let last = rig.last(&p);
    assert_eq!((&last["state"], &last["seq"]), (&json!("confirmed"), &seq));
    lane.pass().await.unwrap();
    assert_eq!(rig.items(&p).len(), m + 1);
}

#[tokio::test]
async fn an_old_final_change_is_not_published_again_after_a_restart_and_zero_disables_the_timer() {
    let rig = Rig::with(|r| {
        r.republish_secs = 0;
        r.deadlines.testing_secs = 10_000_000;
    });
    rig.checks.fast(vec![run("quick", "in_progress")]);
    let p = rig.land("one", "a.txt", "a, edited\n");
    let (exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    let n = rig.items(&p).len();
    rig.wall.advance(100_000);
    lane.pass().await.unwrap();
    assert_eq!(rig.items(&p).len(), n, "0 never publishes on a timer");
    rig.checks.fast(green());
    lane.pass().await.unwrap();
    let m = rig.items(&p).len();
    assert_eq!(rig.last(&p)["state"], "confirmed");
    // Past the week a final item is kept: the log has it.
    rig.wall.advance(8 * 86_400);
    drop(lane);
    drop(exec);
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    assert_eq!(rig.items(&p).len(), m);
}

#[tokio::test]
async fn a_depends_on_cycle_rejects_everything_on_it() {
    let rig = Rig::new();
    rig.checks.fast(green());
    let a = rig.w.change("a", "a.txt", "a, edited\n");
    let b = rig.w.change("b", "b.txt", "b, edited\n");
    // The proposals are p1 and p2.
    let pa = rig.node.propose(&rig.plan(
        &a,
        json!({"dependsOn": [{"repo": "local:origin", "proposal": "p2"}]}),
    ));
    let pb = rig.node.propose(&rig.plan(
        &b,
        json!({"dependsOn": [{"repo": "local:origin", "proposal": "p1"}]}),
    ));
    rig.node.approve(&pa);
    rig.node.approve(&pb);
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    for p in [&pa, &pb] {
        let last = rig.last(p);
        assert_eq!(last["reason"], "dependency", "{last}");
        assert!(last["detail"].as_str().unwrap().contains("cycle"), "{last}");
    }
    assert_eq!(rig.node.claims(), 0);
}

#[tokio::test]
async fn changes_are_taken_in_the_order_of_their_approval() {
    let rig = Rig::new();
    rig.checks.fast(green());
    let a = rig.w.change("a", "a.txt", "a, edited\n");
    let b = rig.w.change("b", "b.txt", "b, edited\n");
    let pa = rig.node.propose(&rig.plan(&a, json!({})));
    let pb = rig.node.propose(&rig.plan(&b, json!({})));
    rig.node.approve(&pb); // b is approved first
    rig.node.approve(&pa);
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    let log = git(&rig.w.remote, &["log", "--format=%s", "-3", "main"]);
    let titles: Vec<&str> = log.lines().collect();
    assert_eq!(
        titles[..2],
        [format!("Land {}", &a[..7]), format!("Land {}", &b[..7])]
    );
    // The second was queued behind the first in the first pass.
    assert_eq!(rig.items(&pa)[0]["state"], "queued");
    let b_items = rig.items(&pb);
    assert_eq!(b_items[0]["state"], "queued");
}

#[tokio::test]
async fn nothing_lands_until_landing_refs_is_set() {
    let rig = Rig::with(|r| r.landing_refs.clear());
    rig.checks.fast(green());
    let p = rig.land("one", "a.txt", "a, edited\n");
    let (exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    exec.step().await.unwrap();
    assert!(rig.node.items("han.land/1").is_empty());
    assert_eq!(rig.node.claims(), 0);
    assert_eq!(rig.node.row(&p).phase, "open");
    assert!(!Lane::configured(&rig.config));

    // A ref outside the configured ones is rejected, with the reason.
    let rig = Rig::new();
    let to = rig.w.change("one", "a.txt", "a, edited\n");
    let plan = rig.plan(&to, json!({"ref": "refs/heads/release"}));
    let p = rig.node.propose(&plan);
    rig.node.approve(&p);
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    assert_eq!(rig.last(&p)["reason"], "policy");
    assert!(Lane::configured(&rig.config));
}

#[tokio::test]
async fn green_checks_without_an_approval_stall_instead_of_flapping() {
    let rig = Rig::new();
    rig.checks.fast(vec![run("quick", "queued")]);
    let p = rig.land(
        "one",
        "a.txt",
        "a, edited
",
    );
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    rig.node.state().rows[0].approvals.clear(); // the approval lapsed while the checks ran
    rig.checks.fast(green());
    let rep = lane.pass().await.unwrap();
    assert!(
        rep.stalled[0].1.contains("waiting for an approval"),
        "{rep:?}"
    );
    assert_eq!(rig.states(&p), ["queued", "testing", "testing"]);
    assert_eq!(rig.node.claims(), 0);
    // Passing again changes nothing; approving again lets it land.
    lane.pass().await.unwrap();
    assert_eq!(rig.items(&p).len(), 3);
    rig.node.approve(&p);
    lane.pass().await.unwrap();
    assert_eq!(rig.last(&p)["state"], "confirmed");
}

#[tokio::test]
async fn trusted_landing_still_requires_green_checks_on_the_landed_commit() {
    for (required, conclusion, lands) in [
        (true, "success", true),
        (true, "failure", false),
        (false, "success", false),
    ] {
        let rig = Rig::with(|r| {
            if !required {
                r.checks.required.clear();
            }
        });
        rig.checks.fast(vec![run("quick", conclusion)]);
        let p = rig.land("trusted", "a.txt", "trusted change\n");
        rig.node.state().rows[0].approvals.clear();
        rig.node.set_proposer_suffices(&p, true);
        let before = rig.w.tip();
        let (_exec, lane) = rig.boot();
        lane.pass().await.unwrap();
        if lands {
            assert_eq!(rig.last(&p)["state"], "confirmed");
            assert_ne!(rig.w.tip(), before);
            let evidence = rig.blob(&rig.items(&p)[2]["evidence"]);
            assert_eq!(evidence["commit"], rig.w.tip());
            assert_eq!(evidence["checks"][0]["conclusion"], "success");
        } else {
            assert_eq!(rig.w.tip(), before);
            assert_eq!(rig.node.claims(), 0);
            assert_eq!(rig.last(&p)["state"], "rejected");
            if !required {
                assert!(
                    rig.cancels()[0]
                        .1
                        .contains("at least one configured required check")
                );
            }
        }
    }
}

#[tokio::test]
async fn remote_preparation_waits_for_home_authority_even_with_an_approval() {
    for check_only in [false, true] {
        let rig = Rig::with(|r| r.checks.rerun = 1);
        rig.checks.fast(vec![run("quick", "queued")]);
        let p = if check_only {
            rig.try_it("guarded", "a.txt", "guarded change\n")
        } else {
            rig.land("guarded", "a.txt", "guarded change\n")
        };
        rig.node.set_proposer_suffices(&p, true);
        // An unrelated human's approval is not the operator's guard approval.
        rig.node.approve(&p);
        rig.node.state().preparation_denied = true;
        let (exec, lane) = rig.boot();
        lane.pass().await.unwrap();
        assert_eq!(rig.last(&p)["state"], "queued");
        assert!(git(&rig.w.remote, &["branch", "--list", "han/*"]).is_empty());
        assert!(rig.checks.started().is_empty());
        drop(lane);
        drop(exec);
        let (exec, lane) = rig.boot();
        rig.wall.advance(31);
        lane.pass().await.unwrap();
        assert!(git(&rig.w.remote, &["branch", "--list", "han/*"]).is_empty());
        assert!(rig.checks.started().is_empty());
        rig.node.state().preparation_denied = false;
        rig.wall.advance(31);
        lane.pass().await.unwrap();
        assert_eq!(rig.last(&p)["state"], "testing");
        assert_eq!(rig.checks.started().len(), 1);
        drop(lane);
        drop(exec);
        // A restored testing record may not request a rerun after authority is withdrawn.
        rig.node.state().preparation_denied = true;
        rig.checks.fast(vec![run("quick", "failure")]);
        let (_exec, lane) = rig.boot();
        lane.pass().await.unwrap();
        assert!(rig.checks.0.lock().unwrap().reruns.is_empty());
        assert_eq!(rig.node.claims(), 0);
    }
}

#[tokio::test]
async fn preparation_revocation_during_check_poll_does_not_start_a_rerun() {
    let rig = Rig::with(|r| r.checks.rerun = 1);
    rig.checks.fast(vec![run("quick", "queued")]);
    let p = rig.try_it("revoked", "a.txt", "revoked change\n");
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    assert_eq!(rig.last(&p)["state"], "testing");
    rig.checks.fast(vec![run("quick", "failure")]);
    let node = rig.node.clone();
    rig.checks.0.lock().unwrap().on_fast_poll = Some(Arc::new(move || {
        node.state().preparation_denied = true;
    }));
    lane.pass().await.unwrap();
    assert!(rig.checks.0.lock().unwrap().reruns.is_empty());
    assert_eq!(rig.node.claims(), 0);
    assert_eq!(rig.last(&p)["state"], "testing");
}

#[tokio::test]
async fn a_cancelled_proposal_leaves_the_lane() {
    let rig = Rig::new();
    rig.checks.fast(vec![run("quick", "queued")]);
    let p = rig.land("one", "a.txt", "a, edited\n");
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    rig.node.state().rows[0].phase = "done".into();
    lane.pass().await.unwrap();
    assert_eq!(rig.last(&p)["state"], "cancelled");
}

#[test]
fn the_config_is_per_repository_and_defaults_are_off() {
    let cfg: ExecConfig =
        serde_json::from_str(r#"{"repos": [{"name": "r", "remote": "x"}]}"#).unwrap();
    let r = &cfg.repos[0];
    assert!(r.landing_refs.is_empty());
    assert_eq!(r.checks.required, ["quick"]);
    assert!(r.checks.full.is_empty());
    assert_eq!(
        (r.checks.poll_secs, r.checks.appear_secs, r.checks.rerun),
        (30, 300, 0)
    );
    assert_eq!(r.lane_branch_prefix, "han/lane/");
    assert_eq!(r.republish_secs, 60);
    assert_eq!(r.deadlines, Deadlines::default());
    assert_eq!(
        (
            r.deadlines.queued_secs,
            r.deadlines.testing_secs,
            r.deadlines.landed_secs
        ),
        (1800, 2400, 3600)
    );
    assert!(!Lane::configured(&cfg));

    let cfg: ExecConfig = serde_json::from_str(
        r#"{"repos": [{"name": "r", "remote": "x", "landing_refs": ["refs/heads/main"],
            "checks": {"required": ["fmt", "test"], "poll_secs": 5},
            "deadlines": {"testing_secs": 60, "landing_secs": 20},
            "lane_branch_prefix": "han/q/"}]}"#,
    )
    .unwrap();
    let r = &cfg.repos[0];
    assert_eq!(r.landing_refs, ["refs/heads/main"]);
    assert_eq!(r.checks.required, ["fmt", "test"]);
    assert_eq!(r.checks.poll_secs, 5);
    assert_eq!(r.deadlines.landing_secs, Some(20));
    assert_eq!(r.lane_branch_prefix, "han/q/");
    assert!(Lane::configured(&cfg));

    // The global list of the first slice is gone.
    assert!(
        serde_json::from_str::<ExecConfig>(r#"{"landing_refs": ["refs/heads/main"]}"#).is_err()
    );
}

// ───────────────────────── check-only tries (vcs.try, P-17) ─────────────────────────

impl Rig {
    /// The plan of trying `to`: a land's plan without `tier` and `rebase`.
    fn try_plan(&self, to: &str) -> Value {
        let mut plan = self.plan(to, json!({}));
        plan["action"] = json!("vcs.try");
        let args = plan["args"].as_object_mut().unwrap();
        args.remove("tier");
        args.remove("rebase");
        args.insert("title".into(), json!(format!("Try {}", &to[..7])));
        plan
    }

    /// Proposes a try of a change of one file. Policy says the proposer suffices: nobody approves it.
    fn try_it(&self, name: &str, file: &str, content: &str) -> String {
        let to = self.w.change(name, file, content);
        let p = self.node.propose(&self.try_plan(&to));
        self.node.set_proposer_suffices(&p, true);
        p
    }

    /// The try branches the lane left on the remote.
    fn branches(&self) -> String {
        git(&self.w.remote, &["branch", "--list", "han/try/*"])
    }
}

#[tokio::test]
async fn a_green_try_runs_the_checks_lands_nothing_and_removes_its_branch() {
    let rig = Rig::new();
    rig.checks.fast(vec![run("quick", "queued")]);
    let tip = rig.w.tip();
    let p = rig.try_it("one", "a.txt", "a, edited\n");
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();

    // Squash-rebased onto the ref's tip and pushed to han/try/<id>, where CI runs on that exact commit.
    assert_eq!(rig.states(&p), ["queued", "testing"]);
    let testing = rig.last(&p);
    let commit = testing["commit"].as_str().unwrap().to_owned();
    assert_eq!(testing["try"], true);
    assert_eq!(testing["branch"], "han/try/p1");
    assert_eq!(testing["onto"], tip.as_str());
    assert_eq!(
        git(&rig.w.remote, &["rev-parse", "refs/heads/han/try/p1"]),
        commit
    );
    assert_eq!(
        git(&rig.w.remote, &["rev-parse", &format!("{commit}^")]),
        tip
    );
    assert_eq!(rig.node.claims(), 0);

    rig.checks.fast(green());
    lane.pass().await.unwrap();
    assert_eq!(rig.states(&p), ["queued", "testing", "landing", "tried"]);
    assert!(rig.items(&p).iter().all(|i| i["try"] == true));
    // Nothing landed: the ref is where it was, and the main branch has not got the edit.
    assert_eq!(rig.w.tip(), tip);
    assert_eq!(rig.w.show("main", "a.txt"), "a");
    // The effect succeeded, with the checks' record as the outcome's evidence.
    assert_eq!(rig.node.claims(), 1);
    assert_eq!(
        rig.node.state().outcomes,
        [(p.clone(), "succeeded".to_owned(), "yes".to_owned())]
    );
    let proof = rig.items(&p)[2]["evidence"].clone();
    let sent = rig.outcome_evidence();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1.as_ref().expect("evidence").hash, proof["hash"]);
    let evidence = rig.blob(&proof);
    assert_eq!(evidence["commit"], commit.as_str());
    assert_eq!(evidence["checks"][0]["conclusion"], "success");
    assert_eq!(rig.node.row(&p).phase, "done");
    assert!(rig.cancels().is_empty());
    // The branch is gone, and the lane's own branch was never made.
    assert_eq!(rig.branches(), "");
    assert_eq!(rig.checks.commits(Stage::Fast), vec![commit]);
    // Told once, and cleaned once.
    rig.wall.advance(1);
    lane.pass().await.unwrap();
    assert_eq!(rig.node.claims(), 1);
    assert_eq!(rig.states(&p).len(), 4);
}

#[tokio::test]
async fn a_try_has_a_group_of_its_own_and_never_takes_the_land_slot() {
    let rig = Rig::new();
    rig.checks.fast(vec![run("quick", "queued")]);
    let land = rig.land("one", "a.txt", "a, edited\n");
    let t = rig.try_it("two", "b.txt", "b, tried\n");
    let land2 = rig.land("three", "c.txt", "c, edited\n");
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();

    // The land and the try are both testing; the second land waits for the land's slot, not for the try.
    assert_eq!(rig.states(&land), ["queued", "testing"]);
    assert_eq!(rig.states(&t), ["queued", "testing"]);
    assert_eq!(rig.last(&land2)["state"], "queued");
    assert_eq!(rig.last(&land2)["detail"], "waiting for the lane's slot");
    assert_eq!(rig.branches().trim(), format!("han/try/{t}"));
    git(
        &rig.w.remote,
        &["rev-parse", &format!("refs/heads/han/lane/main/{land}")],
    );

    rig.checks.fast(green());
    for _ in 0..4 {
        lane.pass().await.unwrap();
    }
    assert_eq!(rig.last(&land)["state"], "confirmed");
    assert_eq!(rig.last(&land2)["state"], "confirmed");
    assert_eq!(rig.last(&t)["state"], "tried");
    // Only the lands moved the ref; the try's change (b.txt) is not on it.
    assert_eq!(rig.w.show("main", "a.txt"), "a, edited");
    assert_eq!(rig.w.show("main", "c.txt"), "c, edited");
    assert_eq!(rig.w.show("main", "b.txt"), "b");
    assert_eq!(rig.branches(), "");
    // The lane's own items name the lands only.
    for i in rig.node.items("han.land/1") {
        if i["item"] == "lane" {
            let running = i["running"].to_string();
            assert!(!running.contains(&t), "{i}");
        }
    }
}

#[tokio::test]
async fn a_red_try_is_cancelled_with_the_test_reason_and_the_evidence_and_removes_its_branch() {
    let rig = Rig::new();
    rig.checks
        .fast(vec![run("quick", "failure"), run("lint", "success")]);
    let tip = rig.w.tip();
    let p = rig.try_it("one", "a.txt", "a, edited\n");
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();

    assert_eq!(rig.states(&p), ["queued", "testing", "rejected"]);
    let last = rig.last(&p);
    assert_eq!(last["reason"], "test");
    assert_eq!(rig.node.claims(), 0);
    assert_eq!(rig.w.tip(), tip);
    let cancels = rig.cancels();
    assert_eq!(cancels.len(), 1, "{cancels:?}");
    assert_eq!(cancels[0].0, p);
    assert!(cancels[0].1.starts_with("test: "), "{:?}", cancels[0]);
    assert!(cancels[0].1.contains("quick"), "{:?}", cancels[0]);
    let proof = cancels[0].2.as_ref().expect("the failing checks");
    assert_eq!(proof.hash, last["evidence"]["hash"].as_str().unwrap());
    let evidence = rig.blob(&last["evidence"]);
    assert_eq!(evidence["checks"][1]["name"], "quick");
    assert_eq!(evidence["checks"][1]["conclusion"], "failure");
    assert_eq!(rig.node.row(&p).phase, "done");
    assert!(rig.node.state().outcomes.is_empty());
    assert_eq!(rig.branches(), "", "the red try's branch is deleted");
}

#[tokio::test]
async fn a_try_that_conflicts_with_the_tip_is_cancelled_with_the_conflict_record() {
    let rig = Rig::new();
    rig.checks.fast(green());
    let p = rig.try_it("one", "a.txt", "a, edited\n");
    rig.w.advance("a.txt", "a, edited by someone else\n");
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();

    assert_eq!(rig.states(&p), ["queued", "rejected"]);
    let last = rig.last(&p);
    assert_eq!(last["reason"], "conflict");
    let record = rig.blob(&last["conflict"]);
    assert_eq!(record["conflicts"][0]["path"], "a.txt");
    let cancels = rig.cancels();
    assert_eq!(cancels.len(), 1, "{cancels:?}");
    assert!(cancels[0].1.starts_with("conflict: "), "{:?}", cancels[0]);
    assert_eq!(
        cancels[0].2.as_ref().expect("the record").hash,
        last["conflict"]["hash"].as_str().unwrap()
    );
    assert_eq!(rig.node.claims(), 0);
    assert_eq!(rig.branches(), "", "nothing was pushed");
}

#[tokio::test]
async fn a_try_that_needs_an_approval_waits_for_one_and_a_cancelled_try_leaves_the_lane() {
    let rig = Rig::new();
    rig.checks.fast(green());
    let to = rig.w.change("one", "a.txt", "a, edited\n");
    // No rule lets the proposer suffice here: Policy would ask for an approval.
    let p = rig.node.propose(&rig.try_plan(&to));
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    assert!(rig.items(&p).is_empty(), "the lane waits for the approval");
    rig.node.approve(&p);
    lane.pass().await.unwrap();
    assert_eq!(rig.states(&p), ["queued", "testing", "landing", "tried"]);

    // One cancelled while it is testing leaves the lane, and its branch goes.
    rig.checks.fast(vec![run("quick", "queued")]);
    let q = rig.try_it("two", "b.txt", "b, edited\n");
    lane.pass().await.unwrap();
    assert_eq!(rig.states(&q), ["queued", "testing"]);
    rig.node.state().rows.iter_mut().for_each(|r| {
        if r.proposal == q {
            r.phase = "done".into();
        }
    });
    lane.pass().await.unwrap();
    assert_eq!(rig.last(&q)["state"], "cancelled");
    assert_eq!(
        rig.branches(),
        "",
        "the cancelled try's branch is deleted too"
    );
}

#[tokio::test]
async fn the_generic_claim_loop_never_claims_vcs_try() {
    let rig = Rig::new();
    rig.checks.fast(green());
    let p = rig.try_it("one", "a.txt", "a, edited\n");
    let (exec, _lane) = rig.boot();
    for _ in 0..3 {
        let r = exec.step().await.unwrap();
        assert!(r.claimed.is_empty() && r.launched.is_empty(), "{r:?}");
        assert!(r.refused.is_empty() && r.waiting.is_empty(), "{r:?}");
    }
    assert_eq!(rig.node.claims(), 0);
    assert_eq!(rig.node.row(&p).phase, "open");
    assert!(actions::VcsTry.lane_only());
}

// ───────────────────────── GitHub ─────────────────────────

const CANARY: &str = "ghs_CANARYinstallationTOKEN0123456789";

#[derive(Default)]
struct Seen {
    requests: Vec<(String, String, String, String)>, // method, path, authorization, body
    pr_open: bool,
    checks_ready: bool,
}

/// A one-request-per-connection HTTP/1.1 server answering like the parts of GitHub the lane uses.
async fn mock_github(seen: Arc<Mutex<Seen>>) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let (head, mut body) = loop {
                    let n = s.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break (
                            String::from_utf8_lossy(&buf[..p]).to_string(),
                            buf[p + 4..].to_vec(),
                        );
                    }
                };
                let mut lines = head.split("\r\n");
                let first = lines.next().unwrap_or("").to_owned();
                let headers: Vec<(String, String)> = lines
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
                    .collect();
                let get = |k: &str| headers.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
                let len: usize = get("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                while body.len() < len {
                    let n = s.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    body.extend_from_slice(&tmp[..n]);
                }
                let mut parts = first.split(' ');
                let method = parts.next().unwrap_or("").to_owned();
                let path = parts.next().unwrap_or("").to_owned();
                let auth = get("authorization").unwrap_or_default();
                let body_s = String::from_utf8_lossy(&body).to_string();
                let (status, reply) = {
                    let mut st = seen.lock().unwrap();
                    st.requests
                        .push((method.clone(), path.clone(), auth, body_s));
                    if method == "POST" && path == "/app/installations/7/access_tokens" {
                        (
                            201,
                            json!({"token": CANARY, "expires_at": "2099-01-01T00:00:00Z"}),
                        )
                    } else if method == "GET" && path.starts_with("/repos/o/r/pulls") {
                        if st.pr_open {
                            (
                                200,
                                json!([{"number": 5, "html_url": "https://example/pr/5"}]),
                            )
                        } else {
                            (200, json!([]))
                        }
                    } else if method == "POST" && path == "/repos/o/r/pulls" {
                        st.pr_open = true;
                        (
                            201,
                            json!({"number": 5, "html_url": "https://example/pr/5"}),
                        )
                    } else if method == "PATCH" && path == "/repos/o/r/pulls/5" {
                        st.pr_open = false;
                        (200, json!({"number": 5, "state": "closed"}))
                    } else if method == "GET"
                        && path.starts_with("/repos/o/r/commits/")
                        && path.contains("/check-runs")
                    {
                        if st.checks_ready {
                            (
                                200,
                                json!({"total_count": 2, "check_runs": [
                                    {"id": 11, "name": "quick", "status": "completed",
                                     "conclusion": "success", "html_url": "https://gh/run/11"},
                                    {"id": 12, "name": "slow", "status": "in_progress",
                                     "conclusion": null, "html_url": "https://gh/run/12"}]}),
                            )
                        } else {
                            (200, json!({"total_count": 0, "check_runs": []}))
                        }
                    } else if method == "GET"
                        && path.starts_with("/repos/o/r/commits/")
                        && path.contains("/status")
                    {
                        (
                            200,
                            json!({"state": "success", "statuses": [
                                {"context": "ci/legacy", "state": "success",
                                 "target_url": "https://ci/legacy"}]}),
                        )
                    } else {
                        (404, json!({"message": "Not Found"}))
                    }
                };
                let text = reply.to_string();
                let resp = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

struct StaticJwt;

impl JwtSource for StaticJwt {
    fn jwt(&self, _: u64, _: u64) -> Result<Secret, String> {
        Ok(Secret::new("test-app-jwt"))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_lane_opens_a_pull_request_and_reads_github_checks() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let api = mock_github(seen.clone()).await;
    let mut rig = Rig::with(|r| {
        r.name = "github:o/r".into();
        r.github = Some("o/r".into());
        r.checks.required = vec!["quick".into(), "ci/legacy".into()];
    });
    let app = GithubApp {
        app_id: 1,
        installation_id: 7,
        api_base: Some(api),
        private_key: String::new(),
    };
    rig.config.github = Some(app.clone());
    let minter = Arc::new(GithubMinter::new(&app, Box::new(StaticJwt)).unwrap());
    rig.minter = Some(minter.clone());
    // The plans name the repository `github:o/r`.
    let to = rig.w.change("one", "a.txt", "a, edited\n");
    let mut plan = rig.plan(&to, json!({}));
    plan["target"] = json!("github:o/r");
    let p = rig.node.propose(&plan);
    rig.node.approve(&p);
    let (exec, _) = rig.boot();
    let lane = Lane::new(exec, Arc::new(GithubChecks::new(minter)), rig.wall.clone());

    // `quick` has not appeared yet: the commit is pushed, the pull request opened, nothing is claimed.
    lane.pass().await.unwrap();
    assert_eq!(rig.states(&p), ["queued", "testing"]);
    let testing = rig.last(&p);
    assert_eq!(testing["pr"], "pull request #5 https://example/pr/5");
    let commit = testing["commit"].as_str().unwrap().to_owned();
    assert_eq!(
        git(&rig.w.remote, &["rev-parse", "refs/heads/han/lane/main/p1"]),
        commit
    );
    {
        let st = seen.lock().unwrap();
        let creates: Vec<&(String, String, String, String)> = st
            .requests
            .iter()
            .filter(|r| r.0 == "POST" && r.1 == "/repos/o/r/pulls")
            .collect();
        assert_eq!(creates.len(), 1);
        let body: Value = serde_json::from_str(&creates[0].3).unwrap();
        assert_eq!(body["head"], "han/lane/main/p1");
        assert_eq!(body["base"], "main");
        assert_eq!(body["title"], format!("Land {}", &to[..7]));
        // SR-67: every token is for the one repository, with the permissions of one purpose.
        let mut perms: Vec<Value> = st
            .requests
            .iter()
            .filter(|r| r.1 == "/app/installations/7/access_tokens")
            .map(|r| serde_json::from_str::<Value>(&r.3).unwrap())
            .map(|b| {
                assert_eq!(b["repositories"], json!(["r"]));
                b["permissions"].clone()
            })
            .collect();
        perms.sort_by_key(Value::to_string);
        perms.dedup();
        assert_eq!(
            perms,
            vec![
                json!({"checks": "read", "statuses": "read"}),
                json!({"contents": "write"}),
                json!({"pull_requests": "write"}),
            ]
        );
        assert!(
            st.requests
                .iter()
                .filter(|r| r.1.contains("/commits/"))
                .all(|r| r.1.contains(&commit) && r.2.contains(CANARY))
        );
    }
    assert_eq!(rig.node.claims(), 0);

    // `quick` ends green (and `slow`, which nobody requires, is still running): the lane lands.
    seen.lock().unwrap().checks_ready = true;
    lane.pass().await.unwrap();
    assert_eq!(
        rig.states(&p),
        ["queued", "testing", "landing", "landed", "confirmed"]
    );
    assert_eq!(rig.w.tip(), commit);
    let evidence = rig.blob(&rig.items(&p)[2]["evidence"]);
    assert_eq!(evidence["backend"], "github-actions");
    let names: Vec<(&str, &str)> = evidence["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["name"].as_str().unwrap(),
                c["conclusion"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        names,
        [
            ("ci/legacy", "success"),
            ("quick", "success"),
            ("slow", "in_progress")
        ]
    );
    // The pull request was found, not opened twice.
    let creates = seen
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|r| r.0 == "POST" && r.1 == "/repos/o/r/pulls")
        .count();
    assert_eq!(creates, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_try_opens_a_draft_pull_request_for_ci_and_closes_it_and_deletes_the_branch_after() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let api = mock_github(seen.clone()).await;
    let mut rig = Rig::with(|r| {
        r.name = "github:o/r".into();
        r.github = Some("o/r".into());
        r.checks.required = vec!["quick".into(), "ci/legacy".into()];
    });
    let app = GithubApp {
        app_id: 1,
        installation_id: 7,
        api_base: Some(api),
        private_key: String::new(),
    };
    rig.config.github = Some(app.clone());
    let minter = Arc::new(GithubMinter::new(&app, Box::new(StaticJwt)).unwrap());
    rig.minter = Some(minter.clone());
    let to = rig.w.change(
        "one",
        "a.txt",
        "a, edited
",
    );
    let mut plan = rig.try_plan(&to);
    plan["target"] = json!("github:o/r");
    let p = rig.node.propose(&plan);
    rig.node.set_proposer_suffices(&p, true);
    let (exec, _) = rig.boot();
    let lane = Lane::new(exec, Arc::new(GithubChecks::new(minter)), rig.wall.clone());
    let tip = rig.w.tip();

    // The repo's CI runs on pull requests, so the try gets a draft one from han/try/<id>.
    lane.pass().await.unwrap();
    assert_eq!(rig.states(&p), ["queued", "testing"]);
    let testing = rig.last(&p);
    assert_eq!(testing["pr"], "pull request #5 https://example/pr/5");
    assert_eq!(testing["branch"], "han/try/p1");
    let commit = testing["commit"].as_str().unwrap().to_owned();
    assert_eq!(
        git(&rig.w.remote, &["rev-parse", "refs/heads/han/try/p1"]),
        commit
    );
    {
        let st = seen.lock().unwrap();
        let creates: Vec<&(String, String, String, String)> = st
            .requests
            .iter()
            .filter(|r| r.0 == "POST" && r.1 == "/repos/o/r/pulls")
            .collect();
        assert_eq!(creates.len(), 1);
        let body: Value = serde_json::from_str(&creates[0].3).unwrap();
        assert_eq!(body["head"], "han/try/p1");
        assert_eq!(body["base"], "main");
        assert_eq!(body["draft"], true);
        assert_eq!(body["title"], format!("try: Try {}", &to[..7]));
        assert!(!st.requests.iter().any(|r| r.0 == "PATCH"), "still open");
    }

    // Green: the effect succeeds, nothing lands, the pull request is closed and the branch deleted.
    seen.lock().unwrap().checks_ready = true;
    lane.pass().await.unwrap();
    assert_eq!(rig.states(&p), ["queued", "testing", "landing", "tried"]);
    assert_eq!(rig.w.tip(), tip);
    assert_eq!(rig.branches(), "");
    let st = seen.lock().unwrap();
    let closes: Vec<&(String, String, String, String)> =
        st.requests.iter().filter(|r| r.0 == "PATCH").collect();
    assert_eq!(closes.len(), 1, "{:?}", st.requests);
    assert_eq!(closes[0].1, "/repos/o/r/pulls/5");
    assert_eq!(
        serde_json::from_str::<Value>(&closes[0].3).unwrap(),
        json!({"state": "closed"})
    );
    assert!(!st.pr_open);
    assert!(closes[0].2.contains(CANARY));
}

// ───────────────────────── the revert of a change that broke the post-merge run (A2) ─────────────────────────

/// A rig whose repository runs `full` after the merge and reverts what breaks it.
fn revert_rig() -> Rig {
    let rig = Rig::with(|r| {
        r.checks.full = vec!["full".into()];
        r.checks.auto_revert = true;
    });
    rig.node.state().hex_ids = true;
    rig.checks.fast(green());
    rig
}

/// The commit a change landed as.
fn landed_commit(rig: &Rig, proposal: &str) -> String {
    rig.items(proposal)
        .iter()
        .rev()
        .find_map(|i| i["commit"].as_str().map(str::to_owned))
        .expect("the change was prepared")
}

/// The proposals the executor made as itself, with their plans: the lane's reverts.
fn reverts(rig: &Rig) -> Vec<(String, Value)> {
    rig.node
        .state()
        .rows
        .iter()
        .filter(|r| r.proposer == EXEC && r.action == "vcs.land")
        .map(|r| {
            (
                r.proposal.clone(),
                serde_json::from_str(r.plan.as_deref().unwrap()).unwrap(),
            )
        })
        .collect()
}

/// Passes until `done` holds (at most 25).
async fn until(lane: &Lane, rig: &Rig, done: impl Fn(&Rig) -> bool) -> (Vec<String>, Vec<String>) {
    let (mut stalled, mut escalated) = (Vec::new(), Vec::new());
    for _ in 0..25 {
        let rep = lane.pass().await.unwrap();
        stalled.extend(rep.stalled.into_iter().map(|(_, w)| w));
        escalated.extend(rep.escalated.into_iter().map(|(p, _)| p));
        if done(rig) {
            break;
        }
    }
    (stalled, escalated)
}

fn failing(commit: &str, rig: &Rig) {
    rig.checks.full(vec![run("full", "success")]);
    rig.checks.full_for(commit, vec![run("full", "failure")]);
}

#[tokio::test]
async fn a_tier_1_culprit_is_reverted_automatically_and_its_agent_gets_a_fix_forward_task() {
    let rig = revert_rig();
    let p = rig.land("one", "a.txt", "a, edited\n");
    rig.checks.full(vec![run("full", "in_progress")]);
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    assert_eq!(rig.last(&p)["state"], "landed");
    let commit = landed_commit(&rig, &p);
    assert_eq!(rig.w.tip(), commit);
    failing(&commit, &rig);
    let (_, escalated) = until(&lane, &rig, |rig| {
        rig.states(&p).last().map(String::as_str) == Some("reverted")
            && reverts(rig)
                .first()
                .is_some_and(|(r, _)| rig.states(r).last().map(String::as_str) == Some("confirmed"))
    })
    .await;
    assert_eq!(escalated.as_slice(), std::slice::from_ref(&p), "told once");

    // The proposal: the lane's own, a vcs.land whose change names the culprit.
    let made = reverts(&rig);
    assert_eq!(made.len(), 1);
    let (rv, plan) = &made[0];
    assert_eq!(plan["args"]["change"]["revertOf"], p.as_str());
    assert_eq!(plan["args"]["base"], commit.as_str());
    // The revert commit's tree is the tree the culprit landed on: the delta is the exact inverse.
    let tip = rig.w.tip();
    assert_ne!(tip, commit);
    assert_eq!(
        git(&rig.w.remote, &["rev-parse", &format!("{tip}^{{tree}}")]),
        git(
            &rig.w.remote,
            &["rev-parse", &format!("{}^{{tree}}", rig.w.base)]
        )
    );
    assert_eq!(rig.w.show("main", "a.txt"), "a");
    let inverse = rig.vcs.delta(&rig.w.remote, &commit, &rig.w.base).unwrap();
    let landed = rig.vcs.delta(&rig.w.remote, &commit, &tip).unwrap();
    assert_eq!(landed.digest().unwrap(), inverse.digest().unwrap());
    assert_eq!(
        plan["args"]["deltaDigest"],
        inverse.digest().unwrap().as_str()
    );
    assert!(
        git(&rig.w.remote, &["log", "-1", "--format=%s", "main"]).starts_with("Revert"),
        "squashed into one commit"
    );

    // The red was reproduced before it was reverted: `full` ran on the culprit once more.
    assert_eq!(rig.checks.full_reruns(), std::slice::from_ref(&commit));
    // The lane approved it through its own narrow command, never `EffectApprove` (SR-06), with the failing
    // run over the culprit's delta as evidence.
    assert!(
        !rig.node
            .state()
            .commands
            .iter()
            .any(|c| matches!(c, han_node::api::Command::EffectApprove { .. }))
    );
    let signed = rig.node.state().signed.clone();
    assert_eq!(signed.len(), 1);
    assert_eq!(&signed[0].0, rv);
    let ev = signed[0].1.clone().unwrap();
    let culprit_delta = rig.vcs.delta(&rig.w.remote, &rig.w.base, &commit).unwrap();
    assert_eq!(ev.kind, "han.ci/1");
    assert_eq!(ev.over, culprit_delta.digest().unwrap());

    // The culprit: reverted, with the failing run and the revert's id.
    let last = rig.last(&p);
    assert_eq!(last["state"], "reverted");
    assert_eq!(last["revertedBy"], rv.as_str());
    assert_eq!(last["revertProposal"], rv.as_str());
    let record = rig.blob(&last["evidence"]);
    assert_eq!(record["commit"], commit.as_str());
    assert_eq!(record["checks"][0]["conclusion"], "failure");
    // Its agent: one fix-forward task, with the failing run.
    let tasks = rig.node.state().tasks.clone();
    assert_eq!(tasks.len(), 1, "{tasks:?}");
    assert_eq!(tasks[0].0, ["@agent:a.localhost"]);
    assert!(
        tasks[0].1.contains(&p) && tasks[0].1.contains("full"),
        "{}",
        tasks[0].1
    );
    assert_eq!(last["fixTask"], "t1");
    // The revert went the normal way: fast checks on its own commit, then the compare-and-swap.
    assert_eq!(
        rig.states(rv),
        ["queued", "testing", "landing", "landed", "confirmed"]
    );
    assert!(rig.checks.commits(Stage::Fast).contains(&tip));
    assert!(rig.cancels().is_empty());
}

#[tokio::test]
async fn the_lane_does_nothing_about_a_red_run_unless_auto_revert_is_on() {
    let rig = Rig::with(|r| r.checks.full = vec!["full".into()]);
    rig.node.state().hex_ids = true;
    rig.checks.fast(green());
    let p = rig.land("one", "a.txt", "a, edited\n");
    rig.checks.full(vec![run("full", "in_progress")]);
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    failing(&landed_commit(&rig, &p), &rig);
    for _ in 0..4 {
        lane.pass().await.unwrap();
    }
    assert_eq!(rig.last(&p)["state"], "landed");
    assert_eq!(rig.last(&p)["postMergeFailed"], true);
    assert!(reverts(&rig).is_empty());
    assert!(rig.node.state().tasks.is_empty());
}

/// A culprit whose revert no machine may approve: the lane proposes it at once, with the evidence, and it waits.
async fn revert_waits_for_a_person(file: &str, sensitive: &[&str]) {
    let rig = revert_rig();
    rig.node.state().sensitive = sensitive.iter().map(|s| (*s).to_owned()).collect();
    let p = rig.land("one", file, "edited\n");
    rig.checks.full(vec![run("full", "in_progress")]);
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    let commit = landed_commit(&rig, &p);
    failing(&commit, &rig);
    until(&lane, &rig, |rig| {
        rig.last(&p).get("needsHuman").is_some() && !reverts(rig).is_empty()
    })
    .await;
    let made = reverts(&rig);
    assert_eq!(made.len(), 1, "proposed at once");
    let rv = made[0].0.clone();
    // Nothing is approved, and nothing lands.
    for _ in 0..4 {
        lane.pass().await.unwrap();
    }
    assert!(rig.node.state().signed.is_empty());
    assert!(rig.node.row(&rv).approvals.is_empty());
    assert_eq!(rig.w.tip(), commit);
    let last = rig.last(&p);
    assert_eq!(last["state"], "landed");
    assert_eq!(last["needsHuman"], true);
    assert_eq!(last["revertProposal"], rv.as_str());
    assert!(
        last["detail"].as_str().unwrap().contains("person"),
        "{last}"
    );
    assert!(
        rig.items(&rv).is_empty(),
        "the lane does not queue what nobody approved"
    );
    // The proposer still gets its task, with the evidence.
    assert_eq!(rig.node.state().tasks.len(), 1);
    assert_eq!(
        rig.blob(&last["evidence"])["checks"][0]["conclusion"],
        "failure"
    );

    // A person approves; the revert lands through the normal path, and the culprit is reverted.
    rig.node.approve(&rv);
    until(&lane, &rig, |rig| {
        rig.states(&p).last().map(String::as_str) == Some("reverted")
    })
    .await;
    assert_eq!(rig.last(&p)["revertedBy"], rv.as_str());
    assert_ne!(rig.w.tip(), commit);
    assert_eq!(
        rig.node.state().tasks.len(),
        1,
        "one task, not one per pass"
    );
}

#[tokio::test]
async fn a_tier_2_culprit_is_proposed_for_revert_at_once_and_waits_for_a_person() {
    revert_waits_for_a_person("payments/pay.txt", &["payments/"]).await;
}

#[tokio::test]
async fn a_culprit_on_a_never_auto_path_waits_for_a_person() {
    revert_waits_for_a_person(".github/workflows/ci.yml", &[]).await;
}

#[tokio::test]
async fn bisection_among_three_landed_changes_finds_the_culprit() {
    let rig = revert_rig();
    let one = rig.land("one", "a.txt", "a, edited\n");
    let two = rig.land("two", "b.txt", "b, edited\n");
    let three = rig.land("three", "c.txt", "c, edited\n");
    rig.checks.full(vec![run("full", "in_progress")]);
    let (_exec, lane) = rig.boot();
    for _ in 0..4 {
        lane.pass().await.unwrap();
    }
    let (c1, c2, c3) = (
        landed_commit(&rig, &one),
        landed_commit(&rig, &two),
        landed_commit(&rig, &three),
    );
    assert_eq!(rig.w.tip(), c3);
    for p in [&one, &two, &three] {
        assert_eq!(rig.last(p)["state"], "landed");
    }

    // Only the tip's run has ended, and it is red: the three changes are the candidates.
    rig.checks.full_for(&c3, vec![run("full", "failure")]);
    lane.pass().await.unwrap();
    assert!(
        rig.last(&three)["detail"]
            .as_str()
            .unwrap()
            .contains("bisecting"),
        "{}",
        rig.last(&three)
    );
    assert!(
        rig.checks.started().contains(&c2),
        "the middle one is run first"
    );
    assert!(reverts(&rig).is_empty(), "no culprit yet");

    // The middle one is red: the culprit is the first or the second.
    rig.checks.full_for(&c2, vec![run("full", "failure")]);
    lane.pass().await.unwrap();
    assert!(rig.checks.started().contains(&c1), "then the first");
    assert!(reverts(&rig).is_empty());

    // The first is green: the second broke it.
    rig.checks.full_for(&c1, vec![run("full", "success")]);
    rig.checks.full(vec![run("full", "success")]);
    rig.checks.full_for(&c2, vec![run("full", "failure")]);
    rig.checks.full_for(&c3, vec![run("full", "failure")]);
    until(&lane, &rig, |rig| {
        ["reverted", "confirmed"].contains(&rig.last(&three)["state"].as_str().unwrap())
            && rig.last(&two)["state"] == "reverted"
    })
    .await;
    assert_eq!(rig.last(&two)["state"], "reverted");
    assert_eq!(rig.last(&two)["fixTask"], "t1");
    assert_eq!(rig.node.state().tasks.len(), 1);
    assert_eq!(rig.last(&one)["state"], "confirmed");
    // The change behind the culprit was red because of it: the revert's green run confirms it.
    assert_eq!(rig.last(&three)["state"], "confirmed");
    assert!(rig.last(&three).get("postMergeFailed").is_none());
    // Only the culprit's file went back.
    assert_eq!(rig.w.show("main", "a.txt"), "a, edited");
    assert_eq!(rig.w.show("main", "b.txt"), "b");
    assert_eq!(rig.w.show("main", "c.txt"), "c, edited");
    assert_eq!(reverts(&rig).len(), 1);
    assert_eq!(
        reverts(&rig)[0].1["args"]["change"]["revertOf"],
        two.as_str()
    );
}

#[tokio::test]
async fn a_revert_that_conflicts_with_later_changes_escalates_as_stalled_and_forces_nothing() {
    let rig = revert_rig();
    let (a, b) = rig.w.chain("chain");
    let base = rig.w.base.clone();
    let pa = rig.node.propose(&rig.plan(&a, json!({})));
    rig.node.approve(&pa);
    let pb = rig.node.propose(&rig.plan_from(
        &a,
        &base,
        &b,
        json!({"dependsOn": [{"repo": "local:origin", "proposal": pa}]}),
    ));
    rig.node.approve(&pb);
    rig.checks.full(vec![run("full", "in_progress")]);
    let (_exec, lane) = rig.boot();
    for _ in 0..3 {
        lane.pass().await.unwrap();
    }
    assert_eq!(rig.last(&pa)["state"], "landed");
    assert_eq!(rig.last(&pb)["state"], "landed");
    let tip = rig.w.tip();
    assert_eq!(rig.w.show("main", "a.txt"), "a, second");

    // The first broke the run, but the second edited the same file after it.
    rig.checks.full(vec![run("full", "success")]);
    rig.checks
        .full_for(&landed_commit(&rig, &pa), vec![run("full", "failure")]);
    rig.checks
        .full_for(&landed_commit(&rig, &pb), vec![run("full", "failure")]);
    let (stalled, _) = until(&lane, &rig, |rig| {
        reverts(rig).first().is_some_and(|(r, _)| {
            rig.items(r)
                .last()
                .is_some_and(|i| i.get("stalled").is_some())
        })
    })
    .await;
    let made = reverts(&rig);
    assert_eq!(made.len(), 1);
    let rv = &made[0].0;
    let last = rig.last(rv);
    assert_eq!(last["state"], "queued", "{last}");
    assert!(
        last["stalled"].as_str().unwrap().contains("does not apply"),
        "{last}"
    );
    assert!(
        stalled.iter().any(|w| w.contains("does not apply")),
        "{stalled:?}"
    );
    // The conflict record names the path.
    let record = rig.blob(&last["conflict"]);
    assert_eq!(record["conflicts"][0]["path"], "a.txt");
    // Nothing was forced, rejected or cancelled; the culprit waits, escalated.
    assert_eq!(rig.w.tip(), tip);
    assert!(rig.cancels().is_empty());
    assert_eq!(rig.last(&pa)["state"], "landed");
    assert_eq!(rig.last(&pa)["postMergeFailed"], true);
    assert_eq!(rig.last(&pa)["escalated"], true);
    // It does not hold the lane: a later change still lands.
    let c = rig.land("later", "c.txt", "c, edited\n");
    for _ in 0..4 {
        lane.pass().await.unwrap();
    }
    assert!(
        ["landed", "confirmed"].contains(&rig.last(&c)["state"].as_str().unwrap()),
        "{}",
        rig.last(&c)
    );
    assert_eq!(rig.last(rv)["state"], "queued");
}

#[tokio::test]
async fn a_restart_in_the_middle_of_a_revert_carries_on_without_repeating_a_step() {
    let rig = revert_rig();
    let p = rig.land("one", "a.txt", "a, edited\n");
    rig.checks.full(vec![run("full", "in_progress")]);
    let commit;
    {
        let (exec, lane) = rig.boot();
        lane.pass().await.unwrap();
        commit = landed_commit(&rig, &p);
        drop(lane);
        drop(exec);
    }
    failing(&commit, &rig);
    // A new executor and lane for every pass: whatever one left half done, the next finds in the journal.
    for _ in 0..25 {
        let (exec, lane) = rig.boot();
        lane.pass().await.unwrap();
        drop(lane);
        drop(exec);
        if rig.states(&p).last().map(String::as_str) == Some("reverted") {
            break;
        }
    }
    assert_eq!(rig.last(&p)["state"], "reverted");
    assert_eq!(reverts(&rig).len(), 1, "one revert proposal");
    assert_eq!(rig.node.state().tasks.len(), 1, "one task");
    assert_eq!(rig.node.state().signed.len(), 1, "one approval");
    assert_eq!(
        rig.checks.full_reruns().len(),
        1,
        "the re-run was asked for once, whatever the restarts"
    );
    assert_eq!(rig.w.show("main", "a.txt"), "a");
    let rv = reverts(&rig)[0].0.clone();
    assert_eq!(
        rig.node.state().outcomes.len(),
        2,
        "two landings: the change and its revert"
    );
    assert!(
        rig.node
            .state()
            .outcomes
            .iter()
            .all(|(_, result, started)| result == "succeeded" && started == "yes"),
        "{:?}",
        rig.node.state().outcomes
    );
    assert_eq!(rig.last(&p)["revertedBy"], rv.as_str());
}

#[tokio::test]
async fn a_revert_of_something_this_executor_did_not_land_is_refused() {
    let rig = revert_rig();
    let p = rig.land("one", "a.txt", "a, edited\n");
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    // An agent proposes a "revert" of it whose delta is not the inverse.
    let other = rig.w.change("other", "b.txt", "b, edited\n");
    let mut plan = rig.plan(&other, json!({}));
    let delta = plan["args"]["deltaDigest"].as_str().unwrap().to_owned();
    plan["args"]["change"] = json!({
        "repo": "local:origin", "deltaDigest": delta, "total": 1,
        "files": [{"path": "b.txt", "kind": "modify", "lines": 2}],
        "revertOf": p,
    });
    let fake = rig.node.propose(&plan);
    rig.node.approve(&fake);
    // And one that names a change that never landed.
    let mut plan = rig.plan(&other, json!({}));
    plan["args"]["change"] = json!({
        "repo": "local:origin", "deltaDigest": delta, "total": 1,
        "files": [{"path": "b.txt", "kind": "modify", "lines": 2}],
        "revertOf": "0".repeat(64),
    });
    let unknown = rig.node.propose(&plan);
    rig.node.approve(&unknown);
    lane.pass().await.unwrap();
    for (q, why) in [(&fake, "inverse"), (&unknown, "no land this executor made")] {
        let last = rig.last(q);
        assert_eq!(last["state"], "rejected", "{last}");
        assert_eq!(last["reason"], "policy");
        assert!(last["detail"].as_str().unwrap().contains(why), "{last}");
    }
    assert_eq!(rig.w.show("main", "b.txt"), "b");
}

/// Every pass is a restart: a new executor and lane read the journal.
async fn restarting(rig: &Rig, done: impl Fn(&Rig) -> bool) {
    for _ in 0..25 {
        let (exec, lane) = rig.boot();
        lane.pass().await.unwrap();
        drop(lane);
        drop(exec);
        if done(rig) {
            return;
        }
    }
    panic!("not done after 25 restarts");
}

/// A change landed on a tier 1 file whose `full` run then turns red; returns the proposal, the commit it
/// landed as, and the lane.
async fn red_culprit(rig: &Rig) -> (String, String, Lane, Arc<Executor>) {
    let p = rig.land("one", "a.txt", "a, edited\n");
    rig.checks.full(vec![run("full", "in_progress")]);
    let (exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    let commit = landed_commit(rig, &p);
    failing(&commit, rig);
    (p, commit, lane, exec)
}

#[tokio::test]
async fn a_red_that_does_not_reproduce_is_flaky_escalated_and_not_reverted() {
    let rig = revert_rig();
    let (p, commit, lane, _exec) = red_culprit(&rig).await;
    rig.checks
        .after_full_rerun(&commit, vec![run("full", "success")]);
    let (_, escalated) = until(&lane, &rig, |rig| rig.last(&p)["reason"] == "flaky").await;
    for _ in 0..6 {
        lane.pass().await.unwrap();
    }
    assert_eq!(
        rig.checks.full_reruns(),
        std::slice::from_ref(&commit),
        "re-run once"
    );
    assert!(escalated.contains(&p), "{escalated:?}");
    let last = rig.last(&p);
    assert_eq!(last["state"], "landed");
    assert_eq!(last["reason"], "flaky");
    assert_eq!(last["flaky"], true);
    assert_eq!(last["postMergeFailed"], true);
    assert_eq!(last["escalated"], true);
    assert_eq!(last["needsHuman"], true);
    assert!(last["detail"].as_str().unwrap().contains("flaky"), "{last}");
    // The first red run is the evidence it was suspected on.
    assert_eq!(
        rig.blob(&last["evidence"])["checks"][0]["conclusion"],
        "failure"
    );
    // Nothing was proposed, approved or asked of its agent, and the ref did not move.
    assert!(reverts(&rig).is_empty());
    assert!(rig.node.state().signed.is_empty());
    assert!(rig.node.state().tasks.is_empty());
    assert_eq!(rig.w.tip(), commit);
}

#[tokio::test]
async fn a_flaky_suspect_keeps_the_changes_behind_it_waiting_too() {
    let rig = revert_rig();
    let one = rig.land("one", "a.txt", "a, edited\n");
    let two = rig.land("two", "b.txt", "b, edited\n");
    rig.checks.full(vec![run("full", "in_progress")]);
    let (_exec, lane) = rig.boot();
    for _ in 0..3 {
        lane.pass().await.unwrap();
    }
    let (c1, c2) = (landed_commit(&rig, &one), landed_commit(&rig, &two));
    rig.checks.full(vec![run("full", "success")]);
    rig.checks.full_for(&c1, vec![run("full", "failure")]);
    rig.checks.full_for(&c2, vec![run("full", "failure")]);
    rig.checks
        .after_full_rerun(&c1, vec![run("full", "success")]);
    until(&lane, &rig, |rig| rig.last(&one)["reason"] == "flaky").await;
    for _ in 0..4 {
        lane.pass().await.unwrap();
    }
    assert_eq!(rig.last(&one)["flaky"], true);
    assert_eq!(rig.last(&two)["state"], "landed");
    assert_eq!(rig.last(&two)["postMergeFailed"], true);
    assert!(
        rig.last(&two)["detail"]
            .as_str()
            .unwrap()
            .contains("not reverted"),
        "{}",
        rig.last(&two)
    );
    assert_eq!(
        rig.checks.full_reruns(),
        [c1],
        "no search starts again on the change behind"
    );
    assert!(reverts(&rig).is_empty());
    assert!(rig.node.state().tasks.is_empty());
}

#[tokio::test]
async fn a_restart_in_the_middle_of_a_rerun_neither_asks_twice_nor_reverts_a_flake() {
    let rig = revert_rig();
    let p = rig.land("one", "a.txt", "a, edited\n");
    rig.checks.full(vec![run("full", "in_progress")]);
    let commit;
    {
        let (exec, lane) = rig.boot();
        lane.pass().await.unwrap();
        commit = landed_commit(&rig, &p);
        drop(lane);
        drop(exec);
    }
    failing(&commit, &rig);
    rig.checks
        .after_full_rerun(&commit, vec![run("full", "success")]);
    restarting(&rig, |rig| rig.last(&p)["reason"] == "flaky").await;
    for _ in 0..3 {
        let (exec, lane) = rig.boot();
        lane.pass().await.unwrap();
        drop(lane);
        drop(exec);
    }
    assert_eq!(
        rig.checks.full_reruns(),
        std::slice::from_ref(&commit),
        "asked once"
    );
    assert!(reverts(&rig).is_empty());
    assert!(rig.node.state().tasks.is_empty());
    assert_eq!(rig.w.tip(), commit);
}

#[tokio::test]
async fn a_backend_that_cannot_run_it_again_leaves_the_red_unconfirmed_and_escalated() {
    let rig = revert_rig();
    let (p, commit, lane, _exec) = red_culprit(&rig).await;
    rig.checks.no_full_rerun();
    let (_, escalated) = until(&lane, &rig, |rig| {
        rig.last(&p)["detail"]
            .as_str()
            .is_some_and(|d| d.contains("cannot run"))
    })
    .await;
    for _ in 0..4 {
        lane.pass().await.unwrap();
    }
    assert!(escalated.contains(&p));
    let last = rig.last(&p);
    assert_eq!(last["state"], "landed");
    assert_eq!(last["reason"], "post_merge_failed");
    assert!(last.get("flaky").is_none());
    assert_eq!(last["needsHuman"], true);
    assert!(reverts(&rig).is_empty());
    assert!(rig.node.state().tasks.is_empty());
    assert_eq!(rig.w.tip(), commit);
    assert_eq!(rig.checks.full_reruns().len(), 1, "asked once");
}

#[tokio::test]
async fn a_fix_forward_task_created_before_a_lost_reply_is_adopted_not_made_twice() {
    let rig = revert_rig();
    rig.node.state().task_lost_replies = 1;
    let (p, _commit, lane, _exec) = red_culprit(&rig).await;
    // The step that failed is tried again after `poll_secs`.
    for _ in 0..25 {
        lane.pass().await.unwrap();
        rig.wall.advance(2);
        if rig.states(&p).last().map(String::as_str) == Some("reverted") {
            break;
        }
    }
    assert_eq!(rig.last(&p)["state"], "reverted");
    let tasks = rig.node.state().tasks.clone();
    assert_eq!(tasks.len(), 1, "{tasks:?}");
    assert!(
        tasks[0]
            .1
            .starts_with(&format!("[han-lane fix-forward {p}]"))
    );
    assert_eq!(rig.last(&p)["fixTask"], "t1");
}

#[tokio::test]
async fn a_revert_policy_refuses_is_escalated_to_a_person_with_the_reason_and_not_retried() {
    let rig = revert_rig();
    rig.node.state().deny_own_proposals =
        Some("budgeted space: cost in the space's unit required".into());
    let (p, commit, lane, _exec) = red_culprit(&rig).await;
    let (_, escalated) = until(&lane, &rig, |rig| rig.last(&p).get("needsHuman").is_some()).await;
    for _ in 0..4 {
        lane.pass().await.unwrap();
    }
    assert!(escalated.contains(&p));
    let last = rig.last(&p);
    assert_eq!(last["state"], "landed");
    assert_eq!(last["needsHuman"], true);
    let detail = last["detail"].as_str().unwrap();
    assert!(
        detail.contains("cannot be proposed") && detail.contains("budgeted space"),
        "{detail}"
    );
    // Not retried every pass, and not dropped: the agent still has its task and the evidence is up.
    let proposals = rig
        .node
        .state()
        .commands
        .iter()
        .filter(|c| matches!(c, han_node::api::Command::EffectPropose { .. }))
        .count();
    assert_eq!(proposals, 1);
    assert_eq!(rig.node.state().tasks.len(), 1);
    assert!(last.get("evidence").is_some());
    assert_eq!(rig.w.tip(), commit);
}

/// A red culprit landed in a budgeted space under the budgeted task `tb` (open or not), at a cost of 50.
async fn red_budgeted_culprit(rig: &Rig, open: bool) -> (String, Lane) {
    rig.node.state().budget_tasks = Some(vec![("tb".into(), open)]);
    let p = rig.land(
        "one",
        "a.txt",
        "a, edited
",
    );
    for r in rig.node.state().rows.iter_mut().filter(|r| r.proposal == p) {
        r.task = Some("tb".into());
        r.cost = Some(("USD-cent".into(), 50));
    }
    rig.checks.full(vec![run("full", "in_progress")]);
    let (_exec, lane) = rig.boot();
    lane.pass().await.unwrap();
    failing(&landed_commit(rig, &p), rig);
    (p, lane)
}

#[tokio::test]
async fn in_a_budgeted_space_a_revert_names_the_open_task_its_culprit_named_and_lands() {
    let rig = revert_rig();
    let (p, lane) = red_budgeted_culprit(&rig, true).await;
    until(&lane, &rig, |rig| {
        rig.states(&p).last().map(String::as_str) == Some("reverted")
    })
    .await;
    assert_eq!(rig.last(&p)["state"], "reverted");
    let revert = rig
        .node
        .state()
        .rows
        .iter()
        .find(|r| r.proposer == EXEC && r.action == "vcs.land")
        .cloned()
        .expect("the lane proposed the revert");
    assert_eq!(revert.task.as_deref(), Some("tb"));
    assert_eq!(revert.cost, Some(("USD-cent".to_owned(), 50)));
}

#[tokio::test]
async fn in_a_budgeted_space_a_revert_whose_culprit_task_is_closed_escalates_with_the_reason() {
    let rig = revert_rig();
    let (p, lane) = red_budgeted_culprit(&rig, false).await;
    let (_, escalated) = until(&lane, &rig, |rig| rig.last(&p).get("needsHuman").is_some()).await;
    for _ in 0..4 {
        lane.pass().await.unwrap();
    }
    assert!(escalated.contains(&p));
    let last = rig.last(&p);
    assert_eq!(last["state"], "landed");
    assert_eq!(last["needsHuman"], true);
    let detail = last["detail"].as_str().unwrap();
    assert!(
        detail.contains("cannot be proposed")
            && detail.contains("the culprit's task is closed or absent"),
        "{detail}"
    );
    assert!(reverts(&rig).is_empty());
}

#[tokio::test]
async fn already_stopped_lane_does_not_discover_or_claim_pending_work() {
    let rig = Rig::new();
    rig.checks.fast(green());
    let head = rig.w.change("change", "a.txt", "edited\n");
    let p = rig.node.propose(&rig.plan(&head, json!({})));
    rig.node.approve(&p);
    let (_exec, lane) = rig.boot();
    let (_tx, rx) = tokio::sync::watch::channel(true);
    tokio::time::timeout(Duration::from_millis(100), lane.run(rx))
        .await
        .unwrap();
    assert_eq!(rig.node.claims(), 0);
    assert!(lane.changes().unwrap().is_empty());
    lane.pass().await.unwrap();
    assert!(
        rig.node.claims() > 0,
        "the pending work is real and claimable on explicit restart"
    );
}

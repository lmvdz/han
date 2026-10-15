//! The landing lane (R33 §3, §4, §8) against real git (a bare remote, a work repository, `GitVcs`), the fake node
//! of `common`, and a fake `Checks` backend that the test scripts. The lane's pull request and its GitHub
//! backend are in `lane_github.rs`.

mod common;

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
        std::fs::write(self.work.join(file), content).unwrap();
        git(&self.work, &["commit", "-q", "-am", branch]);
        let to = git(&self.work, &["rev-parse", "HEAD"]);
        git(&self.work, &["checkout", "-q", "main"]);
        to
    }

    fn advance(&self, file: &str, content: &str) -> String {
        advance(&self.work, &self.remote, file, content)
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
    fast: Vec<CheckRun>,
    full: Vec<CheckRun>,
    /// What `fast` becomes once a re-run was asked for.
    after_rerun: Option<Vec<CheckRun>>,
    requests: Vec<(Stage, String)>,
    started: Vec<String>,
    reruns: Vec<Vec<String>>,
    broken: bool,
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
        st.requests.push((req.stage, req.commit.clone()));
        if st.broken {
            return Err(CheckError("the backend is down".into()));
        }
        Ok(match req.stage {
            Stage::Fast => st.fast.clone(),
            Stage::Full => st.full.clone(),
        })
    }
    async fn rerun(&self, _: &CheckRequest, names: &[String]) -> Result<bool, CheckError> {
        let mut st = self.0.lock().unwrap();
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
        let delta = self
            .vcs
            .delta(&self.w.work, &self.w.base, to)
            .unwrap()
            .digest()
            .unwrap();
        let bundle = self.vcs.bundle(&self.w.work, &self.w.base, to).unwrap();
        let hash = self.node.put_blob(&bundle);
        let mut args = json!({"ref": REF, "base": self.w.base, "to": to, "deltaDigest": delta,
            "tier": 1, "rebase": "path-disjoint", "title": format!("Land {}", &to[..7])});
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

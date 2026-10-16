//! The land lane's auto-revert against a real node and home (A2): a tier 1 change lands, its post-merge run turns
//! red, the lane reverts it with no person. The lane and its executor reach the node through a
//! `ConnectionClass::Executor` connection, so the lane's own approval goes through `RevertApprove` (an executor
//! may not send `EffectApprove`), and Policy, not the lane, decides that it counts.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use han_exec::checks::{CheckError, CheckRequest, CheckRun, Stage};
use han_exec::lane::State;
use han_exec::{
    Checks, ChecksConfig, ExecConfig, Executor, ExecutorConfig, Lane, NeedsApproval, RepoTarget,
    SystemElapsed, SystemWall,
};
use han_node::ClassApi;
use han_node::api::{CallResult, Command, ConnectionClass, NodeApi, Query, Row, Window};
use han_vcs::{GitVcs, Vcs};
use serde_json::json;

mod common;
use common::*;

/// A checks backend the test scripts: everything is green but the `full` run, which is pending while it is held
/// and red for the commits marked red (also when asked again).
struct ScriptChecks {
    red: Mutex<HashSet<String>>,
    hold: AtomicBool,
}

#[async_trait::async_trait]
impl Checks for ScriptChecks {
    fn name(&self) -> &'static str {
        "script"
    }
    async fn poll(&self, req: &CheckRequest) -> Result<Vec<CheckRun>, CheckError> {
        let full = req.stage == Stage::Full;
        let conclusion = if full && self.red.lock().unwrap().contains(&req.commit) {
            "failure"
        } else if full && self.hold.load(Ordering::SeqCst) {
            "in_progress"
        } else {
            "success"
        };
        Ok(req
            .names
            .iter()
            .map(|n| CheckRun::new(n, conclusion, None))
            .collect())
    }
    async fn rerun(&self, _: &CheckRequest, _: &[String]) -> Result<bool, CheckError> {
        Ok(true)
    }
}

/// The default gated policy with a rule for the lane's revert: tier 0-1 approvals by the lane executor on
/// `han.ci/1` evidence, and an owner for the rest.
fn revert_policy() -> String {
    let mut p = han_seq::build::space_policy("members", "invite");
    if let han_core::Value::Object(m) = &mut p {
        m.insert(
            "approvals".into(),
            han_core::json::parse(
                format!(
                    r#"[{{"action":"vcs.land","revert":true,"maxTier":1,"approvers":["{EXEC}","owner"],"evidence":"han.ci/1"}},
                        {{"action":"vcs.land","approvers":["owner"]}}]"#
                )
                .as_bytes(),
            )
            .unwrap(),
        );
        m.insert("separateApprover".into(), han_core::Value::Bool(true));
        m.insert(
            "gates".into(),
            han_core::json::parse(br#"{"small":20,"large":500,"paths":[],"owners":[]}"#).unwrap(),
        );
    }
    String::from_utf8(han_core::json::canonicalize(&p).unwrap()).unwrap()
}

fn rev(dir: &Path, what: &str) -> String {
    git(dir, &["rev-parse", what])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_tier_1_culprit_is_reverted_through_the_real_node_with_no_person() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let remote = origin(root);
    let world = World::new(root);
    let alice = world.node(ALICE, 1);
    let coder = world.node(CODER, 2);
    let exec = world.node(EXEC, 4);
    let space = setup_space(&world, &alice, &[(CODER, &coder), (EXEC, &exec)], &[]).await;
    call(
        &alice,
        Command::PolicySet {
            space: space.clone(),
            policy: revert_policy(),
        },
    )
    .await;

    // The change: one tier 1 file, bundled, proposed by the agent and approved by the owner.
    let work = root.join("work");
    git(
        root,
        &[
            "clone",
            "-q",
            remote.to_str().unwrap(),
            work.to_str().unwrap(),
        ],
    );
    let base = rev(&work, "HEAD");
    git(&work, &["checkout", "-q", "-b", "change"]);
    std::fs::write(work.join("src/lib.rs"), "pub mod auth;\npub mod more;\n").unwrap();
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "more"]);
    let to = rev(&work, "HEAD");
    let vcs = Arc::new(GitVcs::new(root.join("exec-store")));
    let delta = vcs.delta(&work, &base, &to).unwrap();
    let lines = vcs.delta_lines(&work, &base, &to, &delta).unwrap();
    let change = han_vcs::land_change_of("local:origin", &delta, &lines, None).unwrap();
    let bundle = vcs.bundle(&work, &base, &to).unwrap();
    let CallResult::BlobRef {
        hash,
        size,
        media_type,
    } = call(
        &coder,
        Command::BlobPut {
            space: space.clone(),
            media_type: han_vcs::BUNDLE_MEDIA_TYPE.into(),
            data: han_core::base64url::encode(&bundle),
        },
    )
    .await
    else {
        panic!("a blob reference")
    };
    let plan = json!({
        "action": "vcs.land", "target": "local:origin",
        "args": {
            "ref": "refs/heads/main", "base": base, "to": to,
            "deltaDigest": delta.digest().unwrap(), "tier": 1, "rebase": "path-disjoint",
            "title": "More", "change": serde_json::to_value(&change).unwrap(),
        },
        "argsBlob": {"hash": hash, "size": size, "type": media_type},
    });
    let CallResult::Receipt { id: land, .. } = call(
        &coder,
        Command::EffectPropose {
            space: space.clone(),
            executor: EXEC.into(),
            plan: plan.to_string(),
            task: None,
            cost: None,
            repeatable: false,
            expires_at: None,
        },
    )
    .await
    else {
        panic!("a proposal")
    };
    let effects = || -> Vec<han_node::api::EffectRow> {
        rows(
            &exec,
            Query::Effects {
                space: Some(space.clone()),
                executor: Some(EXEC.into()),
                include_done: true,
                window: Window::default(),
            },
        )
        .into_iter()
        .filter_map(|r| match r {
            Row::Effect(e) => Some(e),
            _ => None,
        })
        .collect()
    };
    until("the executor sees the proposal", 15, || {
        effects()
            .iter()
            .any(|e| e.proposal == land && e.plan.is_some())
    })
    .await;
    // the owner's node has the proposal before the owner approves it (it arrives by sync)
    until("alice sees the proposal", 15, || {
        rows(
            &alice,
            Query::Effects {
                space: Some(space.clone()),
                executor: None,
                include_done: true,
                window: Window::default(),
            },
        )
        .iter()
        .any(|r| matches!(r, Row::Effect(e) if e.proposal == land))
    })
    .await;
    call(
        &alice,
        Command::EffectApprove {
            space: space.clone(),
            proposal: land.clone(),
            expires_at: None,
            note: None,
            evidence: None,
        },
    )
    .await;
    until("the executor sees the approval", 15, || {
        effects()
            .iter()
            .any(|e| e.proposal == land && !e.approvals.is_empty())
    })
    .await;

    // The executor and its lane, on an executor-class connection.
    let api: Arc<dyn NodeApi> = Arc::new(ClassApi::new(
        Arc::new(exec.clone()),
        ConnectionClass::Executor,
    ));
    let mut repo = RepoTarget::new("local:origin", remote.to_string_lossy().into_owned());
    repo.landing_refs = vec!["refs/heads/main".into()];
    repo.republish_secs = 0;
    repo.checks = ChecksConfig {
        required: vec!["quick".into()],
        full: vec!["full".into()],
        poll_secs: 0,
        appear_secs: 300,
        rerun: 0,
        auto_revert: true,
        ..ChecksConfig::default()
    };
    let config = ExecConfig {
        repos: vec![repo],
        http_hosts: Vec::new(),
        branch_prefixes: vec!["han/".into()],
        github: None,
        test_allow_private_http: false,
    };
    let executor = Arc::new(
        Executor::open(
            ExecutorConfig {
                actor: EXEC.into(),
                actions: han_exec::actions::builtin(),
                gate: Arc::new(NeedsApproval),
                config: Arc::new(config),
                vcs: vcs.clone(),
                github: None,
                state_dir: root.join("exec-state"),
                stall_bound: Duration::from_secs(10),
            },
            api,
            Arc::new(SystemElapsed::new()),
        )
        .unwrap(),
    );
    let checks = Arc::new(ScriptChecks {
        red: Mutex::default(),
        hold: AtomicBool::new(true),
    });
    let lane = Lane::new(executor, checks.clone(), Arc::new(SystemWall));
    let state_of = |p: &str| {
        lane.changes()
            .unwrap()
            .into_iter()
            .find(|c| c.proposal == p)
            .map(|c| c.state)
    };

    // It lands; then its post-merge run is red.
    for _ in 0..300 {
        lane.pass().await.unwrap();
        if state_of(&land) == Some(State::Landed) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(state_of(&land), Some(State::Landed));
    let landed = rev(&remote, "refs/heads/main");
    assert_ne!(landed, base);
    checks.red.lock().unwrap().insert(landed.clone());
    checks.hold.store(false, Ordering::SeqCst);

    // The lane reverts it: no person approves anything after the original.
    for _ in 0..600 {
        lane.pass().await.unwrap();
        if state_of(&land) == Some(State::Reverted) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(state_of(&land), Some(State::Reverted));
    assert_eq!(
        git(&remote, &["rev-parse", "refs/heads/main^{tree}"]),
        git(&remote, &["rev-parse", &format!("{base}^{{tree}}")]),
        "the ref is back to the tree it had"
    );

    // The log: the revert is the lane executor's own proposal, approved by the executor alone (a service, not
    // a person) and consumed by the executor's claim; the original was approved by the owner.
    let state = world.home.state_of(&space).expect("the space");
    let revert = state
        .effects
        .iter()
        .find(|(id, e)| id.as_str() != land && e.action == "vcs.land")
        .expect("the revert proposal");
    assert_eq!(revert.1.proposer, EXEC);
    // The approval the claim consumed, as the log folds it.
    let approver = |e: &han_core::policy::Effect| {
        let id = e.claims[0]
            .approval
            .clone()
            .expect("a claim consumed an approval");
        let entry = &state.entries[&id];
        assert_eq!(entry.kind, "effect.approve");
        entry.author.clone()
    };
    assert_eq!(approver(revert.1), EXEC, "the lane approved its own revert");
    assert_eq!(revert.1.claims.len(), 1);
    assert_eq!(approver(&state.effects[land.as_str()]), ALICE);

    // Its agent has the fix-forward task.
    let tasks = rows(
        &coder,
        Query::Tasks {
            space: space.clone(),
            assignee: Some(CODER.into()),
            window: Window::default(),
        },
    );
    assert!(
        tasks.iter().any(|r| matches!(r, Row::Task(t)
            if t.ask.as_deref().is_some_and(|a| a.starts_with(&format!("[han-lane fix-forward {land}]"))))),
        "{tasks:?}"
    );
}

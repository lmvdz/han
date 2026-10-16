//! A check-only try through a real host session, node, home, executor and lane (P-17, round 33): the agent calls
//! `land_try`; the host commits its workspace and proposes a `vcs.try`; Policy (a rule with `proposer: true`) lets
//! the proposal suffice, so nobody approves it; the executor's lane squash-rebases it onto the ref's tip, pushes
//! it to `han/try/<id>` and tests it. Red: the lane cancels it with a `test:` reason, the host steers the agent
//! (reason `try_failed`) and the agent tries again. Green: the effect succeeds with the checks as evidence, the
//! branch is gone, nothing landed, and the answer reaches the host's ledger. The session's land attempts are
//! untouched throughout.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use han_exec::checks::{CheckError, CheckRequest, CheckRun};
use han_exec::lane::State;
use han_exec::{
    Action, Checks, ChecksConfig, ExecConfig, Executor, ExecutorConfig, Lane, NeedsApproval,
    RepoTarget, SystemElapsed, SystemWall,
};
use han_host::host::{HostConfig, HostDeps, RepoConfig, SandboxChoice, run_session};
use han_node::ClassApi;
use han_node::api::{Command, ConnectionClass, NodeApi, Query, Row, SessionState, Window};
use han_registry::{FinishSpec, LandFinish};
use han_vcs::GitVcs;
use serde_json::Value;

mod common;
mod production_shim;
use common::*;

/// Every check is red while `red` is set, green after.
struct ScriptChecks {
    red: AtomicBool,
}

#[async_trait::async_trait]
impl Checks for ScriptChecks {
    fn name(&self) -> &'static str {
        "script"
    }
    async fn poll(&self, req: &CheckRequest) -> Result<Vec<CheckRun>, CheckError> {
        let conclusion = if self.red.load(Ordering::SeqCst) {
            "failure"
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
        Ok(false)
    }
}

/// The default gated policy with the rule of the check-only action first: the proposal suffices for a `vcs.try`.
fn try_policy() -> String {
    let mut p = han_seq::build::space_policy("members", "invite");
    if let han_core::Value::Object(m) = &mut p {
        m.insert(
            "approvals".into(),
            han_core::json::parse(
                br#"[{"action":"vcs.try","approvers":["owner"],"proposer":true},
                     {"action":"*","approvers":["owner"]}]"#,
            )
            .unwrap(),
        );
    }
    String::from_utf8(han_core::json::canonicalize(&p).unwrap()).unwrap()
}

/// What the fake agent was sent, one entry per prompt: the text of each content block.
fn prompts_of(log: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["event"] == "prompt")
        .map(|v| {
            v["prompt"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| b["text"].as_str().unwrap_or("").to_owned())
                .collect()
        })
        .collect()
}

fn host_config(
    root: &Path,
    (space, remote, script): (&str, &Path, &Path),
    session_dir: PathBuf,
    agent_log: &Path,
) -> HostConfig {
    let mut driver = fake_driver(script);
    driver.env.push((
        "HAN_FAKE_AGENT_LOG".into(),
        agent_log.to_string_lossy().into_owned(),
    ));
    HostConfig {
        space: space.into(),
        agent: CODER.into(),
        operator: Some(ALICE.into()),
        driver,
        repo: RepoConfig {
            name: "local:origin".into(),
            source: remote.to_string_lossy().into_owned(),
            base: "main".into(),
            default_branch: "main".into(),
        },
        issue: Some(ISSUE.into()),
        task: None,
        executor: EXEC.into(),
        session_dir,
        taint_db: root.join("coder").join("taint.db"),
        guarded: false,
        strict_on_read: true,
        sandbox: SandboxChoice {
            backend: "none".into(),
            allow_advisory: true,
            ro: vec![script.to_path_buf()],
            login_dir: None,
            allow_hosts: Vec::new(),
            toolchains: Default::default(),
        },
        shim: production_shim::shim(),
        node: PathBuf::from("node"),
        adapter_dir: PathBuf::new(),
        model_key: None,
        budget: None,
        turn_timeout: Duration::from_secs(120),
        exclusion: Vec::new(),
        finish: FinishSpec {
            land: Some(LandFinish {
                settle_secs: 0,
                ..LandFinish::default()
            }),
        },
    }
}

fn tries(exec: &impl NodeApi, space: &str) -> Vec<han_node::api::EffectRow> {
    rows(
        exec,
        Query::Effects {
            space: Some(space.to_owned()),
            executor: Some(EXEC.into()),
            include_done: true,
            window: Window::default(),
        },
    )
    .into_iter()
    .filter_map(|r| match r {
        Row::Effect(e) if e.action == "vcs.try" => Some(e),
        _ => None,
    })
    .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_red_try_steers_the_agent_and_a_green_one_succeeds_with_nothing_landed() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let remote = origin(root);
    let main_before = git(&remote, &["rev-parse", "refs/heads/main"]);
    let world = World::new(root);
    let alice = world.node(ALICE, 1);
    let coder = world.node(CODER, 2);
    let exec = world.node(EXEC, 4);
    let space = setup_space(&world, &alice, &[(CODER, &coder), (EXEC, &exec)], &[]).await;
    call(
        &alice,
        Command::PolicySet {
            space: space.clone(),
            policy: try_policy(),
        },
    )
    .await;

    // The executor and its lane, on an executor-class connection, with checks that are red at first.
    let api: Arc<dyn NodeApi> = Arc::new(ClassApi::new(
        Arc::new(exec.clone()),
        ConnectionClass::Executor,
    ));
    let mut repo = RepoTarget::new("local:origin", remote.to_string_lossy().into_owned());
    repo.landing_refs = vec!["refs/heads/main".into()];
    repo.republish_secs = 0;
    repo.checks = ChecksConfig {
        required: vec!["quick".into()],
        poll_secs: 0,
        ..ChecksConfig::default()
    };
    let config = ExecConfig {
        repos: vec![repo.clone()],
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
                config: Arc::new(config.clone()),
                vcs: Arc::new(GitVcs::new(root.join("exec-store"))),
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
        red: AtomicBool::new(true),
    });
    let lane = Arc::new(Lane::new(executor, checks.clone(), Arc::new(SystemWall)));
    let (lane_stop, lane_rx) = tokio::sync::watch::channel(false);
    let lane_task = {
        let lane = lane.clone();
        tokio::spawn(async move { lane.run(lane_rx).await })
    };

    // The host session: the agent writes, calls land_try, and waits for what the lane finds.
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/try-session.json");
    let session_dir = root.join("coder").join("s1");
    let agent_log = session_dir.join("agent").join("fake-agent.jsonl");
    let cfg = host_config(
        root,
        (&space, &remote, &script),
        session_dir.clone(),
        &agent_log,
    );
    let deps = HostDeps {
        api: Arc::new(coder.clone()),
        vcs: Arc::new(GitVcs::new(root.join("coder").join("objects"))),
        signer: Some((CODER.into(), "d1".into(), seed(2))),
        proxy_hook: None,
    };
    let (stop, rx) = tokio::sync::watch::channel(false);
    let host = tokio::spawn(run_session(cfg, deps, rx));

    // 1. The first try is in the log with a plan the executor's own check accepts, and Policy reports that the
    // proposal suffices: nobody approves anything.
    until("the first try is proposed", 90, || {
        !tries(&exec, &space).is_empty()
    })
    .await;
    let first = tries(&exec, &space).remove(0);
    assert!(first.proposer_suffices, "Policy reports it: {first:?}");
    let plan: Value = serde_json::from_str(first.plan.as_deref().unwrap()).unwrap();
    assert_eq!(plan["action"], "vcs.try");
    assert_eq!(plan["args"]["ref"], "refs/heads/main");
    assert!(plan["args"].get("tier").is_none() && plan["args"].get("rebase").is_none());
    han_exec::actions::VcsTry
        .plan_check(&plan, &config)
        .expect("the executor accepts the plan");

    // 2. Red: the lane rejects it (a `test:` cancel), nothing landed, the branch is gone.
    let state_of = |p: &str| {
        lane.changes()
            .unwrap()
            .into_iter()
            .find(|c| c.proposal == p)
            .map(|c| (c.state, c.reason))
    };
    until("the lane rejects the red try", 90, || {
        state_of(&first.proposal) == Some((State::Rejected, Some("test".into())))
    })
    .await;
    until("the effect ends cancelled in the log", 30, || {
        tries(&exec, &space)
            .iter()
            .any(|e| e.proposal == first.proposal && e.phase == "done")
    })
    .await;
    let cancelled = world.home.state_of(&space).expect("the space");
    assert!(cancelled.effects[first.proposal.as_str()].claims.is_empty());
    assert_eq!(git(&remote, &["rev-parse", "refs/heads/main"]), main_before);
    until("the try branch is deleted", 30, || {
        git(&remote, &["branch", "--list", "han/try/*"]).is_empty()
    })
    .await;

    // 3. The host steers the idle agent, once, with reason try_failed; the agent fixes the work and tries again.
    checks.red.store(false, Ordering::SeqCst);
    until("the agent gets its second turn", 90, || {
        prompts_of(&agent_log).len() >= 2
    })
    .await;
    let second = prompts_of(&agent_log).remove(1);
    assert!(
        second[0].contains(&first.proposal)
            && second[0].contains("check-only try")
            && second[0].contains("reason: test"),
        "{second:?}"
    );
    assert!(second[1].contains("\"item\":\"try_failed\""), "{second:?}");
    assert!(second[2].contains("`land_try`"), "{second:?}");

    // 4. Green: the second try succeeds, with no approval consumed and the checks as evidence; nothing landed.
    until("the second try is proposed", 90, || {
        tries(&exec, &space).len() >= 2
    })
    .await;
    let second_try = tries(&exec, &space)
        .into_iter()
        .find(|e| e.proposal != first.proposal)
        .unwrap();
    until("the lane reports it tried", 90, || {
        state_of(&second_try.proposal).map(|s| s.0) == Some(State::Tried)
    })
    .await;
    until("the effect succeeded", 30, || {
        tries(&exec, &space)
            .iter()
            .any(|e| e.proposal == second_try.proposal && e.phase == "done")
    })
    .await;
    let state = world.home.state_of(&space).expect("the space");
    let done = &state.effects[second_try.proposal.as_str()];
    assert_eq!(done.phase, han_core::policy::Phase::Succeeded);
    assert_eq!(done.claims.len(), 1);
    assert_eq!(done.claims[0].approval, None, "no approval was consumed");
    assert!(done.approvals.is_empty());
    assert_eq!(git(&remote, &["rev-parse", "refs/heads/main"]), main_before);
    until("the try branch is deleted", 30, || {
        git(&remote, &["branch", "--list", "han/try/*"]).is_empty()
    })
    .await;

    // 5. The answer reaches the host's ledger: the first try rejected (and steered), the second tried; the land
    // proposals are none, so the attempts cap was never in play.
    let ledger = session_dir.join("land.json");
    until("the host has the second try's answer", 30, || {
        std::fs::read_to_string(&ledger).is_ok_and(|t| {
            serde_json::from_str::<Value>(&t).is_ok_and(|v| {
                v["tries"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|t| t["state"] == "tried"))
            })
        })
    })
    .await;
    let l: Value = serde_json::from_str(&std::fs::read_to_string(&ledger).unwrap()).unwrap();
    assert_eq!(l["tries"][0]["proposal"], first.proposal.as_str());
    assert_eq!(l["tries"][0]["state"], "rejected");
    assert_eq!(l["tries"][0]["steered"], true);
    assert_eq!(l["tries"][1]["state"], "tried");
    assert!(l["proposals"].as_array().unwrap().is_empty());
    assert_eq!(prompts_of(&agent_log).len(), 2, "the steer came once");

    let _ = stop.send(true);
    let _ = lane_stop.send(true);
    let end = tokio::time::timeout(Duration::from_secs(90), host)
        .await
        .expect("the host stops")
        .unwrap()
        .expect("the session ends");
    assert_ne!(end.state, SessionState::Failed);
    let _ = tokio::time::timeout(Duration::from_secs(10), lane_task).await;
}

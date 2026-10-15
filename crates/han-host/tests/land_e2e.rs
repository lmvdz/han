//! The land finish flow through a real host session: the agent proposes kind `land`, the executor's
//! proposal carries a plan the executor's own check accepts, the lane's items reach the session's status, and
//! the workspace is pruned when the lane says `confirmed`. The lane is played by the executor's node, which
//! publishes `han.land/1` items on a `han.land lane` session as `han-exec`'s lane does.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use han_exec::{Action, ExecConfig, RepoTarget};
use han_host::host::{HostConfig, HostDeps, RepoConfig, SandboxChoice, run_session};
use han_node::api::{CallResult, Command, Query, Row, SessionState, Window};
use han_registry::{FinishSpec, LandFinish};
use han_vcs::GitVcs;
use serde_json::{Value, json};

mod common;
mod production_shim;
use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_session_lands_through_the_lane_and_is_pruned_when_it_confirms() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let remote = origin(root);
    let world = World::new(root);
    let alice = world.node(ALICE, 1);
    let coder = world.node(CODER, 2);
    let exec = world.node(EXEC, 4);
    let space = setup_space(&world, &alice, &[(CODER, &coder), (EXEC, &exec)], &[]).await;

    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/land-session.json");
    let session_dir = root.join("coder").join("s1");
    // gone, or an empty directory the agent's working directory keeps open (Windows)
    let gone =
        |p: PathBuf| !p.exists() || std::fs::read_dir(&p).is_ok_and(|mut d| d.next().is_none());
    let cfg = HostConfig {
        space: space.clone(),
        agent: CODER.into(),
        operator: Some(ALICE.into()),
        driver: fake_driver(&script),
        repo: RepoConfig {
            name: "local:origin".into(),
            source: remote.to_string_lossy().into_owned(),
            base: "main".into(),
            default_branch: "main".into(),
        },
        issue: Some(ISSUE.into()),
        task: None,
        executor: EXEC.into(),
        session_dir: session_dir.clone(),
        taint_db: root.join("coder").join("taint.db"),
        guarded: false,
        strict_on_read: true,
        sandbox: SandboxChoice {
            backend: "none".into(),
            allow_advisory: true,
            ro: vec![script.clone()],
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
            land: Some(LandFinish::default()),
        },
    };
    let deps = HostDeps {
        api: Arc::new(coder.clone()),
        vcs: Arc::new(GitVcs::new(root.join("coder").join("objects"))),
        signer: Some((CODER.into(), "d1".into(), seed(2))),
        proxy_hook: None,
    };
    // build output beside the worktree, as the toolchain's is
    std::fs::create_dir_all(session_dir.join("target")).unwrap();
    std::fs::write(session_dir.join("target").join("x"), "x").unwrap();
    let (stop, rx) = tokio::sync::watch::channel(false);
    let host = tokio::spawn(run_session(cfg, deps, rx));

    // 1. The proposal exists with a vcs.land plan the executor's own check accepts.
    let mut effect = None;
    for _ in 0..600 {
        effect = rows(
            &exec,
            Query::Effects {
                space: Some(space.clone()),
                executor: Some(EXEC.into()),
                include_done: false,
                window: Window::default(),
            },
        )
        .into_iter()
        .find_map(|r| match r {
            Row::Effect(e) if e.action == "vcs.land" && e.plan.is_some() => Some(e),
            _ => None,
        });
        if effect.is_some() || host.is_finished() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let effect = effect.expect("the agent's land proposal reached the executor");
    let plan: Value = serde_json::from_str(effect.plan.as_deref().unwrap()).unwrap();
    assert_eq!(plan["target"], "local:origin");
    assert_eq!(plan["args"]["ref"], "refs/heads/main");
    assert_eq!(plan["args"]["rebase"], "path-disjoint");
    let mut repo = RepoTarget::new("local:origin", remote.to_string_lossy().into_owned());
    repo.landing_refs = vec!["refs/heads/main".into()];
    han_exec::actions::VcsLand
        .plan_check(
            &plan,
            &ExecConfig {
                repos: vec![repo],
                http_hosts: Vec::new(),
                branch_prefixes: vec!["han/".into()],
                github: None,
                test_allow_private_http: false,
            },
        )
        .expect("the executor accepts the plan");
    let ledger = session_dir.join("land.json");
    until("the host froze the proposal", 15, || ledger.exists()).await;
    assert!(
        std::fs::read_to_string(&ledger)
            .unwrap()
            .contains(&effect.proposal)
    );

    // 2. The lane says "testing": the session's status says so.
    let CallResult::Receipt { id: lane, .. } = call(
        &exec,
        Command::SessionOpen {
            space: space.clone(),
            steer_mode: None,
            task: None,
            title: Some("han.land lane".into()),
        },
    )
    .await
    else {
        panic!("a session")
    };
    let say = |state: &'static str, seq: u64| {
        let (exec, space, lane, proposal) = (
            exec.clone(),
            space.clone(),
            lane.clone(),
            effect.proposal.clone(),
        );
        async move {
            call(
                &exec,
                Command::StreamPublish {
                    space,
                    session: lane,
                    name: "han.land/1".into(),
                    body: json!({
                        "item": "change", "proposal": proposal, "repo": "local:origin",
                        "ref": "refs/heads/main", "lane": "local:origin#refs/heads/main",
                        "state": state, "seq": seq, "at": "2026-10-15T10:00:00Z",
                    })
                    .to_string(),
                },
            )
            .await;
        }
    };
    let step = || {
        rows(
            &alice,
            Query::Board {
                space: space.clone(),
                group: Default::default(),
                collapsed: vec![],
                window: Window::default(),
            },
        )
        .into_iter()
        .find_map(|r| match r {
            Row::Board(b) if b.agent == CODER => Some(b.step),
            _ => None,
        })
        .unwrap_or_default()
    };
    // the agent's turn is over: only an idle agent shows the lane's word
    for _ in 0..600 {
        if step().starts_with("waiting for input") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // (stream items are live only: say it until the host's node has it)
    for _ in 0..60 {
        say("testing", 2).await;
        if step().contains("testing") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(step().contains("testing"), "status: {}", step());
    assert!(session_dir.join("wt").exists());

    // 3. The lane confirms: the workspace and its build output are removed.
    for _ in 0..60 {
        say("confirmed", 3).await;
        if gone(session_dir.join("wt")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        gone(session_dir.join("wt")),
        "the workspace is pruned once the proposal is confirmed; status: {}; ledger: {}",
        step(),
        std::fs::read_to_string(&ledger).unwrap_or_default()
    );
    assert!(!session_dir.join("target").exists(), "CARGO_TARGET_DIR");
    assert!(
        std::fs::read_to_string(&ledger)
            .unwrap()
            .contains("\"pruned\": true"),
        "the ledger records it"
    );

    let _ = stop.send(true);
    let end = tokio::time::timeout(Duration::from_secs(60), host)
        .await
        .expect("the host stops")
        .unwrap()
        .expect("the session ends");
    assert_ne!(end.state, SessionState::Failed);
}

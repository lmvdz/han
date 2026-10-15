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
use han_vcs::{GitVcs, Vcs, WorkspaceSpec};
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

    // 4. A steer to the idle session, whose workspace is gone, is refused with a reason: no turn runs in a
    // removed directory, and the session does not fail.
    let session = rows(
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
        Row::Board(b) if b.agent == CODER => Some(b.session),
        _ => None,
    })
    .expect("the agent's session");
    call(
        &alice,
        Command::SessionInput {
            space: space.clone(),
            session,
            text: "one more thing".into(),
        },
    )
    .await;
    until("the steer is refused", 30, || step().contains("refused")).await;
    assert!(step().contains("workspace was removed"), "{}", step());

    let _ = stop.send(true);
    let end = tokio::time::timeout(Duration::from_secs(60), host)
        .await
        .expect("the host stops")
        .unwrap()
        .expect("the session ends");
    assert_ne!(end.state, SessionState::Failed);
}

fn host_config(
    root: &Path,
    space: &str,
    remote: &Path,
    script: &Path,
    session_dir: PathBuf,
) -> HostConfig {
    HostConfig {
        space: space.into(),
        agent: CODER.into(),
        operator: Some(ALICE.into()),
        driver: fake_driver(script),
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
            land: Some(LandFinish::default()),
        },
    }
}

fn effects_of(exec: &impl han_node::api::NodeApi, space: &str) -> Vec<han_node::api::EffectRow> {
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
        Row::Effect(e) if e.action == "vcs.land" => Some(e),
        _ => None,
    })
    .collect()
}

/// The backstop: the agent committed work (here the test does, for it) and never proposed it. When the session
/// ends the host proposes the commit as a `vcs.land`, titled from the task; what was not committed is left
/// alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_session_that_ends_with_committed_work_nobody_proposed_has_it_proposed_by_the_host() {
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
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/land-backstop-session.json");
    let session_dir = root.join("coder").join("s1");
    let cfg = host_config(root, &space, &remote, &script, session_dir.clone());
    let deps = HostDeps {
        api: Arc::new(coder.clone()),
        vcs: Arc::new(GitVcs::new(root.join("coder").join("objects"))),
        signer: Some((CODER.into(), "d1".into(), seed(2))),
        proxy_hook: None,
    };
    let (stop, rx) = tokio::sync::watch::channel(false);
    let host = tokio::spawn(run_session(cfg, deps, rx));

    // The agent wrote its file and did not propose. The test commits it (as the agent would), and leaves one
    // more file uncommitted.
    let wt = session_dir.join("wt");
    until("the agent wrote its file", 60, || {
        wt.join("src/auth/login.rs").exists()
    })
    .await;
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
    until("the turn is over", 60, || {
        step().starts_with("waiting for input")
    })
    .await;
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-q", "-m", "the login check"]);
    let committed = git(&wt, &["rev-parse", "HEAD"]);
    std::fs::write(wt.join("scratch.txt"), "not committed\n").unwrap();
    assert!(effects_of(&exec, &space).is_empty(), "nothing proposed yet");

    let _ = stop.send(true);
    tokio::time::timeout(Duration::from_secs(90), host)
        .await
        .expect("the host stops")
        .unwrap()
        .expect("the session ends");

    until("the host's proposal is in the log", 30, || {
        !effects_of(&exec, &space).is_empty()
    })
    .await;
    let effects = effects_of(&exec, &space);
    assert_eq!(effects.len(), 1, "{effects:?}");
    let plan: Value = serde_json::from_str(effects[0].plan.as_deref().unwrap()).unwrap();
    assert_eq!(plan["args"]["to"], committed.as_str());
    assert!(
        plan["args"]["title"].as_str().unwrap().contains(ISSUE),
        "titled from the task: {}",
        plan["args"]["title"]
    );
    assert_eq!(effects[0].proposer, CODER);
    // The uncommitted file was neither committed nor lost.
    assert_eq!(
        std::fs::read_to_string(wt.join("scratch.txt")).unwrap(),
        "not committed\n"
    );
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), committed);
    assert!(git(&wt, &["status", "--porcelain"]).contains("scratch.txt"));
}

/// The sweep: a host that starts finds an earlier session's directory with a ledger whose proposal was
/// rejected by the lane (the lane's item was never heard; the executor's cancel is in the log), follows it
/// through the Effects view and prunes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_host_that_starts_prunes_an_earlier_sessions_workspace_from_the_log() {
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

    // An earlier session of this agent left its directory: a workspace with a commit, and a ledger that has
    // one proposal. The proposal exists in the log and the executor refused it (a conflict), and then the
    // earlier host died.
    let old = root.join("coder").join("s0");
    let objects = GitVcs::new(root.join("coder").join("objects"));
    let ws = objects
        .create_workspace(&WorkspaceSpec {
            source: remote.to_string_lossy().into_owned(),
            base: "main".into(),
            branch: "han/12-coder".into(),
            dir: old.join("wt"),
            sparse: Vec::new(),
        })
        .unwrap();
    std::fs::write(old.join("wt").join("old.rs"), "// old\n").unwrap();
    git(&old.join("wt"), &["add", "-A"]);
    git(&old.join("wt"), &["commit", "-q", "-m", "old work"]);
    let to = git(&old.join("wt"), &["rev-parse", "HEAD"]);
    let CallResult::Receipt { id: proposal, .. } = call(
        &coder,
        Command::EffectPropose {
            space: space.clone(),
            executor: EXEC.into(),
            plan: format!(
                r#"{{"action":"vcs.land","target":"local:origin","args":{{"base":"{}","to":"{to}"}}}}"#,
                ws.base
            ),
            task: None,
            cost: None,
            repeatable: false,
            expires_at: None,
        },
    )
    .await
    else {
        panic!("a receipt")
    };
    std::fs::create_dir_all(old.join("target")).unwrap();
    std::fs::write(old.join("target").join("x"), "x").unwrap();
    std::fs::write(
        old.join("land.json"),
        serde_json::to_string_pretty(&json!({
            "space": space,
            "proposals": [{
                "proposal": proposal, "base": ws.base, "to": to,
                "deltaDigest": "0".repeat(64), "title": "old work", "tier": 1, "state": "proposed",
            }],
        }))
        .unwrap(),
    )
    .unwrap();
    until("the executor sees the proposal", 30, || {
        !effects_of(&exec, &space).is_empty()
    })
    .await;
    call(
        &exec,
        Command::EffectCancel {
            space: space.clone(),
            proposal: proposal.clone(),
            reason: Some("conflict: conflicts with abc: old.rs".into()),
            evidence: None,
        },
    )
    .await;

    // A new host starts, in the same state directory, for another session.
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/land-backstop-session.json");
    let cfg = host_config(
        root,
        &space,
        &remote,
        &script,
        root.join("coder").join("s1"),
    );
    let deps = HostDeps {
        api: Arc::new(coder.clone()),
        vcs: Arc::new(GitVcs::new(root.join("coder").join("objects"))),
        signer: Some((CODER.into(), "d1".into(), seed(2))),
        proxy_hook: None,
    };
    let (stop, rx) = tokio::sync::watch::channel(false);
    let host = tokio::spawn(run_session(cfg, deps, rx));
    let gone =
        |p: PathBuf| !p.exists() || std::fs::read_dir(&p).is_ok_and(|mut d| d.next().is_none());
    until("the earlier workspace is pruned", 60, || {
        gone(old.join("wt"))
    })
    .await;
    assert!(!old.join("target").exists(), "its build output too");
    until("the ledger says so", 10, || {
        std::fs::read_to_string(old.join("land.json")).is_ok_and(|t| t.contains("\"pruned\": true"))
    })
    .await;
    let ledger = std::fs::read_to_string(old.join("land.json")).unwrap();
    assert!(
        ledger.contains("rejected") && ledger.contains("conflict"),
        "{ledger}"
    );

    let _ = stop.send(true);
    let end = tokio::time::timeout(Duration::from_secs(90), host)
        .await
        .expect("the host stops")
        .unwrap()
        .expect("the session ends");
    assert_ne!(end.state, SessionState::Failed);
}

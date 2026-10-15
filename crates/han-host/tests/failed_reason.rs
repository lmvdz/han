//! A session that fails says why: the first turn ends with no output, and the reason (not a generic "the
//! session stopped") is in the host's `SessionEnd` and in the last status the board shows. The agent is
//! `han-fake-agent` replaying `fixtures/empty-turn-session.json`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use han_host::host::{HostConfig, HostDeps, RepoConfig, SandboxChoice, run_session};
use han_node::api::{CallResult, Command, Query, Row, SessionState, Window};
use han_vcs::GitVcs;

mod common;
mod production_shim;
use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_failed_turn_keeps_its_reason_in_the_end_and_the_status() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let remote = origin(root);
    let world = World::new(root);
    let alice = world.node(ALICE, 1);
    let coder = world.node(CODER, 2);
    let space = setup_space(&world, &alice, &[(CODER, &coder)], &[]).await;

    let ask = "Work on github:o/r#12: Fix the login\n\n----- BEGIN ISSUE TEXT (untrusted, from the issue's author) -----\n\
               The login check is missing.\n----- END ISSUE TEXT -----";
    let CallResult::Receipt { id: task, .. } = call(
        &alice,
        Command::TaskCreate {
            space: space.clone(),
            to: vec![CODER.into()],
            ask: ask.into(),
            external: Some(ISSUE.into()),
        },
    )
    .await
    else {
        panic!("a task")
    };
    until("the coder sees its task", 15, || {
        rows(
            &coder,
            Query::Tasks {
                space: space.clone(),
                assignee: Some(CODER.into()),
                window: Window::default(),
            },
        )
        .iter()
        .any(|r| matches!(r, Row::Task(t) if t.task == task && t.ask.is_some()))
    })
    .await;

    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/empty-turn-session.json");
    let log = root.join("agent-log.jsonl");
    let mut driver = fake_driver(&script);
    driver.env.push((
        "HAN_FAKE_AGENT_LOG".into(),
        log.to_string_lossy().into_owned(),
    ));
    let taint_db = root.join("coder").join("taint.db");
    std::fs::create_dir_all(taint_db.parent().unwrap()).unwrap();
    let cfg = HostConfig {
        space: space.clone(),
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
        session_dir: root.join("coder").join("s1"),
        taint_db,
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
        turn_timeout: Duration::from_secs(60),
        exclusion: Vec::new(),
    };
    let deps = HostDeps {
        api: Arc::new(coder.clone()),
        vcs: Arc::new(GitVcs::new(root.join("coder").join("objects"))),
        signer: Some((CODER.into(), "d1".into(), seed(2))),
        proxy_hook: None,
    };
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let end = tokio::time::timeout(Duration::from_secs(90), run_session(cfg, deps, rx))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(end.state, SessionState::Failed);
    let why = "the turn ended with no output and no tool calls";
    assert_eq!(end.reason.as_deref(), Some(why));

    // the board's last word on the session carries the same reason
    let row = loop {
        let found = rows(
            &alice,
            Query::Session {
                id: end.session.clone(),
                part: han_node::api::SessionPart::Facts,
                window: Window {
                    filter: han_node::api::Filter {
                        include_closed: true,
                        ..Default::default()
                    },
                    ..Window::default()
                },
            },
        )
        .into_iter()
        .find_map(|r| match r {
            Row::Session(s) if s.closed => Some(s),
            _ => None,
        });
        if let Some(s) = found {
            break s;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(row.state, SessionState::Failed);
    let said = row.state_reason.unwrap_or_default();
    assert!(said.contains(why), "{said}");
}

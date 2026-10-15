//! The publisher drains on close (R5): with a node that is slow to take stream items, the session's last items
//! still arrive, because shutdown closes the signal channel and joins the publisher instead of sleeping and
//! aborting it. The agent is `han-fake-agent`; the node is a real one behind a wrapper that delays each
//! `StreamPublish`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use han_host::host::{HostConfig, HostDeps, RepoConfig, SandboxChoice, run_session};
use han_node::api::{
    BoxFuture, CallResult, Command, ItemBody, NodeApi, NodeError, Query, Row, SessionPart,
    SessionState, ViewStream, Window,
};
use han_vcs::GitVcs;

mod common;
mod production_shim;
use common::*;

/// A node whose stream publishes take `delay` each.
struct SlowStreams<N> {
    inner: N,
    delay: Duration,
}

impl<N: NodeApi> NodeApi for SlowStreams<N> {
    fn call(&self, cmd: Command) -> BoxFuture<'_, Result<CallResult, NodeError>> {
        let slow = matches!(cmd, Command::StreamPublish { .. });
        let delay = self.delay;
        let fut = self.inner.call(cmd);
        Box::pin(async move {
            if slow {
                tokio::time::sleep(delay).await;
            }
            fut.await
        })
    }

    fn view(&self, query: Query) -> ViewStream {
        self.inner.view(query)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn the_final_message_arrives_through_a_slow_node_after_stop() {
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

    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/items-session.json");
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
        finish: Default::default(),
    };
    let deps = HostDeps {
        api: Arc::new(SlowStreams {
            inner: coder.clone(),
            delay: Duration::from_millis(700),
        }),
        vcs: Arc::new(GitVcs::new(root.join("coder").join("objects"))),
        signer: Some((CODER.into(), "d1".into(), seed(2))),
        proxy_hook: None,
    };
    let (stop, rx) = tokio::sync::watch::channel(false);
    let host = tokio::spawn(run_session(cfg, deps, rx));

    // Stop as soon as the first (slow) item publish is under way: the rest of the session's items are still
    // queued behind it when the host shuts down.
    let session = loop {
        let found = rows(
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
        });
        if let Some(s) = found {
            break s;
        }
        assert!(!host.is_finished(), "the session ended early");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    stop.send(true).unwrap();
    let end = tokio::time::timeout(Duration::from_secs(60), host)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(end.state, SessionState::Done);

    // No waiting: when run_session returns, everything the agent said has been sent.
    let items: Vec<_> = rows(
        &alice,
        Query::Session {
            id: session,
            part: SessionPart::Items,
            window: Window::default(),
        },
    )
    .into_iter()
    .filter_map(|r| match r {
        Row::Item(i) => Some(i),
        _ => None,
    })
    .collect();
    assert!(
        items.iter().any(|i| matches!(
            &i.item,
            ItemBody::Message { text, done: true, .. } if text.starts_with("Which branch")
        )),
        "the final message was dropped: {items:?}"
    );
}

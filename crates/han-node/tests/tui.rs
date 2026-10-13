//! The terminal face against a real in-process node, over the real local pipe: the board shows a session
//! published through the node, the session pane steers it, and the TUI finds the daemon again when it restarts.

mod common;

use std::sync::Arc;

use common::{World, call, id_of, space_of, until};
use han_node::NodeApi;
use han_node::api::{Command, FileTouch, Query, Row, SessionState, StatusBody, Tokens, ViewEvent};
use han_node::rpc::{RpcClient, default_endpoint, serve};
use han_tui::config::Config;
use han_tui::host::{Connector, RpcConnector};
use han_tui::testing::Harness;

const ALICE: &str = "@alice:a.localhost";

fn create(world: &World) -> Command {
    Command::SpaceCreate {
        home: world.home_ref.clone(),
        name: "team".into(),
        visibility: "members".into(),
        join: "invite".into(),
    }
}

fn status(session: &str, step: &str, seq: u64) -> StatusBody {
    StatusBody {
        session: session.into(),
        agent: ALICE.into(),
        delegate_of: None,
        harness: "pi".into(),
        issue: Some("github:o/r#41".into()),
        repo: Some("github:o/r".into()),
        base: None,
        state: SessionState::Working,
        step: step.into(),
        tool: Some("cargo test".into()),
        plan: vec![],
        files: vec![FileTouch {
            path: "src/lib.rs".into(),
            hunks: vec![(1, 4)],
        }],
        tests: None,
        tokens: Some(Tokens {
            input: 4000,
            out: 500,
            cost: 1,
        }),
        summary: None,
        sandbox: None,
        seq,
        at: "2026-10-13T10:00:00Z".into(),
    }
}

/// Serves the node API on a runtime of its own; shutting that runtime down closes every connection, as the
/// end of a daemon process does.
fn serve_on_own_runtime(api: Arc<dyn NodeApi>, endpoint: &str) -> tokio::runtime::Runtime {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let server = {
        let _in_rt = rt.enter();
        serve(api, endpoint).unwrap()
    };
    rt.spawn(async move {
        let _server = server;
        std::future::pending::<()>().await;
    });
    rt
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_board_the_session_pane_and_steering_work_against_a_real_node() {
    let world = World::new(&[(ALICE, 31)]);
    let node = world.node(ALICE, 31);
    let dir = world.dir.join("tui-test");
    std::fs::create_dir_all(&dir).unwrap();
    let endpoint = default_endpoint(&dir);
    let api: Arc<dyn NodeApi> = Arc::new(node.clone());
    let _server = serve(api, &endpoint).unwrap();

    let space = space_of(&call(&node, create(&world)).await);
    let session = id_of(
        &call(
            &node,
            Command::SessionOpen {
                steer_mode: None,
                space: space.clone(),
                task: None,
                title: Some("alice".into()),
            },
        )
        .await,
    );
    call(
        &node,
        Command::StatusPublish {
            space: space.clone(),
            status: Box::new(status(&session, "running the tests", 1)),
        },
    )
    .await;

    // The face: only the RPC client between it and the node.
    let face: Arc<dyn NodeApi> = Arc::new(RpcClient::connect(&endpoint).await.unwrap());
    let mut h = Harness::new(face, Some(&space), (140, 30));
    let screen = h.wait_for("running the tests").await;
    assert!(screen.contains("● node"), "{screen}");
    assert!(screen.contains(ALICE), "{screen}");

    // Enter on the row opens the session pane with its live stream.
    h.keys("j<enter>").await;
    let screen = h.wait_for("tool: cargo test").await;
    assert!(screen.contains("Session ·"), "{screen}");

    // Steering: `i`, a line, Enter. The node signs a session.input and the Inputs view brings it back.
    h.keys("iuse the cache<enter>").await;
    let screen = h.wait_for("use the cache").await;
    assert!(screen.contains("sent"), "{screen}");
    until("the node holds the input", || {
        matches!(
            node.read(&Query::Inputs {
                session: session.clone(),
                window: Default::default(),
            }),
            Ok(ViewEvent::Snapshot { rows, .. }) if rows.iter().any(|r| matches!(r, Row::Input(i) if i.text.as_deref() == Some("use the cache")))
        )
    })
    .await;

    // A new status item reaches the open pane as a line of its stream.
    call(
        &node,
        Command::StatusPublish {
            space: space.clone(),
            status: Box::new(status(&session, "fixing the freshness bug", 2)),
        },
    )
    .await;
    h.wait_for("fixing the freshness bug").await;

    // The timeline is not served by this node's views yet or is; either way the pane says which.
    h.keys("2").await;
    let screen = h.screen();
    assert!(
        screen.contains("pending M4")
            || screen.contains("No points yet")
            || screen.contains("file(s)"),
        "{screen}"
    );
    node.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_tui_reconnects_when_the_daemon_restarts() {
    let world = World::new(&[(ALICE, 31)]);
    let node = world.node(ALICE, 31);
    let dir = world.dir.join("tui-restart");
    std::fs::create_dir_all(&dir).unwrap();
    let endpoint = default_endpoint(&dir);
    // The daemon runs on a runtime of its own, so that stopping it drops its connections like a dying process.
    let daemon = serve_on_own_runtime(Arc::new(node.clone()), &endpoint);
    let space = space_of(&call(&node, create(&world)).await);
    let session = id_of(
        &call(
            &node,
            Command::SessionOpen {
                steer_mode: None,
                space: space.clone(),
                task: None,
                title: None,
            },
        )
        .await,
    );
    call(
        &node,
        Command::StatusPublish {
            space: space.clone(),
            status: Box::new(status(&session, "before the restart", 1)),
        },
    )
    .await;

    let connector: Arc<dyn Connector> = Arc::new(RpcConnector {
        endpoint: endpoint.clone(),
    });
    let mut h = Harness::build(
        None,
        Some(connector),
        Some(&space),
        (140, 24),
        Config::default(),
        None,
    );
    h.wait_for("before the restart").await;

    // The daemon stops: the TUI keeps what it shows and says it is reconnecting.
    daemon.shutdown_background();
    h.wait_for("reconnecting").await;
    assert!(h.screen().contains("before the restart"));

    // The daemon comes back (same node, new listener) with newer news.
    let daemon = serve_on_own_runtime(Arc::new(node.clone()), &endpoint);
    call(
        &node,
        Command::StatusPublish {
            space: space.clone(),
            status: Box::new(status(&session, "after the restart", 2)),
        },
    )
    .await;
    let screen = h.wait_for("after the restart").await;
    assert!(screen.contains("● node"), "{screen}");
    daemon.shutdown_background();
    node.shutdown();
}

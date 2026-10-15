//! A host that exits without closing its session (killed by SIGTERM or a stop timeout) leaves the session open
//! with its last status, `idle`. The node sees the role end and marks the session ended: the board says
//! `failed` with the reason "host exited", until the session's own close arrives, which wins.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Watched, World, call, id_of, space_of, until, window};
use han_node::api::{
    BoardRow, Command, Filter, Group, NodeApi, Query, Row, SessionPart, SessionState, StatusBody,
    Window,
};
use han_node::supervisor::{HOST_EXITED, RoleSpec, RoleStatus, Supervisor, SupervisorError};

const BOB: &str = "@bob:a.localhost";
const ROLE: &str = "host-@bob:a.localhost#github:o/r#1";

/// One role whose state the test sets.
#[derive(Default)]
struct OneRole(Mutex<Option<RoleStatus>>);

impl Supervisor for OneRole {
    fn start(&self, _spec: RoleSpec) -> Result<(), SupervisorError> {
        Ok(())
    }
    fn stop(&self, name: &str) -> Result<(), SupervisorError> {
        Err(SupervisorError::NotRunning(name.into()))
    }
    fn status(&self) -> Vec<(String, RoleStatus)> {
        self.0
            .lock()
            .unwrap()
            .clone()
            .map(|s| vec![(ROLE.to_owned(), s)])
            .unwrap_or_default()
    }
}

fn idle_status(session: &str) -> StatusBody {
    StatusBody {
        session: session.into(),
        agent: BOB.into(),
        delegate_of: None,
        harness: "pi".into(),
        issue: Some("github:o/r#1".into()),
        repo: None,
        base: None,
        state: SessionState::Idle,
        step: "waiting for input".into(),
        tool: None,
        plan: vec![],
        files: vec![],
        tests: None,
        tokens: None,
        summary: None,
        sandbox: None,
        budget: None,
        publish_errors: 0,
        seq: 0,
        at: "2026-01-01T00:00:00Z".into(),
    }
}

fn board(node: &dyn NodeApi, space: &str) -> Vec<BoardRow> {
    let mut v = node.view(Query::Board {
        space: space.into(),
        group: Group::Issue,
        collapsed: vec![],
        window: window(),
    });
    match v.try_next() {
        Some(han_node::api::ViewEvent::Snapshot { rows, .. }) => rows
            .into_iter()
            .filter_map(|r| match r {
                Row::Board(b) => Some(b),
                _ => None,
            })
            .collect(),
        other => panic!("no snapshot: {other:?}"),
    }
}

async fn rig() -> (
    World,
    han_node::Node<han_net::MemTransport>,
    Arc<OneRole>,
    String,
) {
    let world = World::new(&[(BOB, 32)]);
    let sup = Arc::new(OneRole::default());
    *sup.0.lock().unwrap() = Some(RoleStatus::Running);
    let bob = world.node_supervised(BOB, 32, sup.clone(), |c| {
        c.tick_every = Duration::from_millis(50);
        c.exit_grace = Duration::ZERO;
    });
    let space = space_of(
        &call(
            &bob,
            Command::SpaceCreate {
                home: world.home_ref.clone(),
                name: "team".into(),
                visibility: "members".into(),
                join: "invite".into(),
            },
        )
        .await,
    );
    (world, bob, sup, space)
}

async fn open_idle_session(bob: &dyn NodeApi, space: &str) -> String {
    let session = id_of(
        &call(
            bob,
            Command::SessionOpen {
                space: space.into(),
                steer_mode: None,
                task: None,
                title: Some("github:o/r#1".into()),
            },
        )
        .await,
    );
    common::session_known(bob, space, &session).await;
    call(
        bob,
        Command::StatusPublish {
            space: space.into(),
            status: Box::new(idle_status(&session)),
        },
    )
    .await;
    session
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_that_exits_without_closing_leaves_its_session_failed_with_the_reason() {
    let (_world, bob, sup, space) = rig().await;
    let session = open_idle_session(&bob, &space).await;
    until("the board shows the idle session", || {
        board(&bob, &space)
            .iter()
            .any(|b| b.session == session && b.state == SessionState::Idle)
    })
    .await;
    // still running: nothing changes
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(board(&bob, &space)[0].state, SessionState::Idle);

    // the host is killed: the role ends and no `session.close` was sent
    *sup.0.lock().unwrap() = Some(RoleStatus::Exited(None));
    until("the board shows the session ended", || {
        board(&bob, &space)
            .iter()
            .any(|b| b.session == session && b.state == SessionState::Failed)
    })
    .await;
    let row = board(&bob, &space).remove(0);
    assert_eq!(row.state_reason.as_deref(), Some(HOST_EXITED));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_the_host_closed_is_not_marked_when_the_host_exits() {
    let (_world, bob, sup, space) = rig().await;
    let session = open_idle_session(&bob, &space).await;
    call(
        &bob,
        Command::SessionClose {
            space: space.clone(),
            session: session.clone(),
        },
    )
    .await;
    *sup.0.lock().unwrap() = Some(RoleStatus::Exited(Some(0)));
    tokio::time::sleep(Duration::from_millis(400)).await;
    let mut facts = Watched::open(
        &bob,
        Query::Session {
            id: session,
            part: SessionPart::Facts,
            window: Window {
                filter: Filter {
                    include_closed: true,
                    ..Filter::default()
                },
                ..window()
            },
        },
    );
    until("the session row says closed", || {
        facts
            .rows()
            .iter()
            .any(|r| matches!(r, Row::Session(s) if s.closed))
    })
    .await;
    let Some(Row::Session(row)) = facts.rows().into_iter().next() else {
        panic!("no session row");
    };
    assert_eq!(row.state, SessionState::Done, "{row:?}");
    assert_eq!(row.state_reason, None);
}

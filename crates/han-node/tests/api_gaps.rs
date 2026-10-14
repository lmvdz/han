//! The node API gaps the terminal and the GUI specs asked for: the node's clock on every event, the plan facts
//! of an approval, the list of spaces, the `needs-me` facet, `guardedFor` on tasks and the guards a host reads,
//! and a dropped RPC server that really closes its connections.

mod common;

use std::sync::Arc;

use common::{World, call, confirm_operator, id_of, space_of, until, window};
use han_node::api::{
    ApprovalRow, Command, Filter, GuardRow, NodeApi, NodeError, Query, Row, TaskRow, ViewEvent,
    Window,
};
use han_node::rpc::{RpcClient, default_endpoint, serve};

const ALICE: &str = "@alice:a.localhost";
const CODER: &str = "@coder:a.localhost";
const EXEC: &str = "@exec:a.localhost";

fn rows(api: &dyn NodeApi, q: Query) -> Vec<Row> {
    let mut v = api.view(q);
    match v.try_next() {
        Some(ViewEvent::Snapshot { rows, .. }) => rows,
        other => panic!("no snapshot: {other:?}"),
    }
}

fn approvals(api: &dyn NodeApi, actor: &str, filter: Filter) -> Vec<ApprovalRow> {
    rows(
        api,
        Query::Approvals {
            actor: actor.into(),
            window: Window {
                filter,
                ..Window::default()
            },
        },
    )
    .into_iter()
    .filter_map(|r| match r {
        Row::Approval(a) => Some(a),
        _ => None,
    })
    .collect()
}

fn tasks(api: &dyn NodeApi, space: &str) -> Vec<TaskRow> {
    rows(
        api,
        Query::Tasks {
            space: space.into(),
            assignee: None,
            window: window(),
        },
    )
    .into_iter()
    .filter_map(|r| match r {
        Row::Task(t) => Some(t),
        _ => None,
    })
    .collect()
}

fn guard(api: &dyn NodeApi, space: &str, actor: &str) -> GuardRow {
    rows(
        api,
        Query::Guard {
            space: space.into(),
            actor: actor.into(),
            window: window(),
        },
    )
    .into_iter()
    .find_map(|r| match r {
        Row::Guard(g) => Some(g),
        _ => None,
    })
    .expect("a guard row")
}

async fn create_space(world: &World, alice: &dyn NodeApi) -> String {
    space_of(
        &call(
            alice,
            Command::SpaceCreate {
                home: world.home_ref.clone(),
                name: "team".into(),
                visibility: "members".into(),
                join: "invite".into(),
            },
        )
        .await,
    )
}

async fn join(world: &World, alice: &dyn NodeApi, space: &str, who: &str, node: &dyn NodeApi) {
    call(
        alice,
        Command::MemberSet {
            space: space.into(),
            actor: who.into(),
            role: "member".into(),
        },
    )
    .await;
    call(
        node,
        Command::SpaceFollow {
            home: world.home_ref.clone(),
            space: space.into(),
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_approval_carries_its_plan_facts_and_who_may_cancel() {
    let world = World::new(&[(ALICE, 61)]);
    world
        .home
        .register(CODER, "agent", "d1", &common::pubkey(62), Some(ALICE))
        .unwrap();
    let alice = world.node(ALICE, 61);
    let coder = world.node(CODER, 62);
    let space = create_space(&world, &alice).await;
    confirm_operator(&world, &alice, ALICE, CODER).await;
    join(&world, &alice, &space, CODER, &coder).await;
    let long = "x".repeat(500);
    let plan = format!(
        r#"{{"action":"open_pr","target":"github:o/r","args":{{"base":"main","title":"{long}","draft":true,"files":["a","b"]}}}}"#
    );
    let proposal = id_of(
        &call(
            &coder,
            Command::EffectPropose {
                space: space.clone(),
                executor: ALICE.into(),
                plan,
                task: None,
                cost: None,
                repeatable: false,
                expires_at: None,
            },
        )
        .await,
    );
    until("alice's approvals show the proposal", || {
        approvals(&alice, ALICE, Filter::default())
            .iter()
            .any(|a| a.proposal == proposal)
    })
    .await;
    let row = approvals(&alice, ALICE, Filter::default())
        .into_iter()
        .find(|a| a.proposal == proposal)
        .unwrap();
    assert_eq!(row.action, "open_pr");
    assert_eq!(row.target.as_deref(), Some("github:o/r"));
    // Plain top-level arguments only (the array is left out), long texts cut, names in order.
    let names: Vec<&str> = row.summary.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(names, ["base", "draft", "title"]);
    assert_eq!(row.summary[1].1, "true");
    assert_eq!(
        row.summary[2].1.chars().count(),
        han_node::api::SUMMARY_TEXT
    );
    assert!(row.plan.as_deref().is_some_and(|p| p.contains("open_pr")));
    // The node hashed the plan itself.
    assert!(row.digest_verified);
    let parsed = han_core::json::parse(row.plan.as_deref().unwrap().as_bytes()).unwrap();
    assert_eq!(
        han_core::ids::json_digest(&parsed).unwrap(),
        row.plan_digest
    );
    // Alice is an owner, so she may cancel; the facet keeps rows that wait on her.
    assert!(row.can_cancel);
    let mine = Filter {
        needs_me: true,
        ..Filter::default()
    };
    assert_eq!(approvals(&alice, ALICE, mine).len(), 1);
    alice.shutdown();
    coder.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_node_clock_rides_every_event_and_the_spaces_are_listed() {
    let world = World::new(&[(ALICE, 63)]);
    let alice = world.node(ALICE, 63);
    // No spaces yet: the list is empty.
    assert!(rows(&alice, Query::Spaces { window: window() }).is_empty());
    let a = create_space(&world, &alice).await;

    let mut v = alice.view(Query::Spaces { window: window() });
    let Some(ViewEvent::Snapshot { rows, .. }) = v.next().await else {
        panic!("a snapshot first");
    };
    assert!(matches!(&rows[..], [Row::SpaceStatus(s)] if s.space == a));

    // Time moves only with a tick; the node's own row and the next event carry the new reading.
    let mut status = alice.view(Query::NodeStatus { window: window() });
    let Some(ViewEvent::Snapshot { rows, now_ms, .. }) = status.next().await else {
        panic!("a snapshot first");
    };
    let Row::NodeStatus(n) = &rows[0] else {
        panic!("node status")
    };
    assert_eq!(n.now_ms, now_ms);
    let before = now_ms;
    world.clock.advance_steady(90);
    alice.tick();
    let mut latest = before;
    until("the node status shows the later time", || {
        while let Some(ev) = status.try_next() {
            if let ViewEvent::Diff { ops, now_ms, .. } = ev {
                assert!(!ops.is_empty());
                latest = now_ms;
            }
        }
        latest >= before + 90_000
    })
    .await;
    alice.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_task_records_who_it_guards_for_and_an_accepting_assignee_answers_to_them() {
    let world = World::new(&[(ALICE, 64)]);
    world
        .home
        .register(CODER, "agent", "d1", &common::pubkey(65), Some(ALICE))
        .unwrap();
    world
        .home
        .register(EXEC, "agent", "d1", &common::pubkey(66), Some(ALICE))
        .unwrap();
    let alice = world.node(ALICE, 64);
    let coder = world.node(CODER, 65);
    let exec = world.node(EXEC, 66);
    let space = create_space(&world, &alice).await;
    confirm_operator(&world, &alice, ALICE, CODER).await;
    confirm_operator(&world, &alice, ALICE, EXEC).await;
    join(&world, &alice, &space, CODER, &coder).await;
    join(&world, &alice, &space, EXEC, &exec).await;

    // The coder chose guarding; the task it writes carries that, fixed at creation.
    call(
        &coder,
        Command::SessionOpen {
            space: space.clone(),
            steer_mode: Some("guarded".into()),
            task: None,
            title: None,
        },
    )
    .await;
    let task = id_of(
        &call(
            &coder,
            Command::TaskCreate {
                space: space.clone(),
                to: vec![EXEC.into()],
                ask: "tidy up".into(),
                external: None,
            },
        )
        .await,
    );
    until("the exec node sees the task", || {
        tasks(&exec, &space).iter().any(|t| t.task == task)
    })
    .await;
    let row = tasks(&exec, &space)
        .into_iter()
        .find(|t| t.task == task)
        .unwrap();
    assert_eq!(row.guarded_for, vec![CODER.to_owned()]);
    assert!(row.accepted.is_empty());
    // Not yet accepted: the assignee carries nothing.
    let g = guard(&exec, &space, EXEC);
    assert!(!g.guarded && g.guards.is_empty());

    call(
        &exec,
        Command::TaskUpdate {
            space: space.clone(),
            task: task.clone(),
            state: "accepted".into(),
        },
    )
    .await;
    until("accepting makes the assignee guarded", || {
        guard(&exec, &space, EXEC).guarded
    })
    .await;
    let g = guard(&exec, &space, EXEC);
    assert_eq!(g.guards, vec![CODER.to_owned()]);
    assert!(!g.forced);
    assert_eq!(
        tasks(&exec, &space)
            .into_iter()
            .find(|t| t.task == task)
            .unwrap()
            .accepted,
        vec![EXEC.to_owned()]
    );
    // The assignee's own final update ends what it carries.
    call(
        &exec,
        Command::TaskUpdate {
            space: space.clone(),
            task,
            state: "done".into(),
        },
    )
    .await;
    until("finishing ends the carry", || {
        !guard(&exec, &space, EXEC).guarded
    })
    .await;
    alice.shutdown();
    coder.shutdown();
    exec.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_the_rpc_server_closes_its_connections() {
    let world = World::new(&[(ALICE, 67)]);
    let node = world.node(ALICE, 67);
    let dir = world.dir.join("rpc-drop");
    std::fs::create_dir_all(&dir).unwrap();
    let endpoint = default_endpoint(&dir);
    let server = serve(Arc::new(node.clone()), &endpoint).unwrap();
    let face = RpcClient::connect(&endpoint).await.unwrap();
    let mut view = face.view(Query::NodeStatus { window: window() });
    assert!(matches!(
        view.next().await,
        Some(ViewEvent::Snapshot { .. })
    ));
    drop(server);
    // The established connection ends: the view fails and calls are refused.
    let ended = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            match view.next().await {
                Some(ViewEvent::Failed { error }) => break Some(error),
                Some(_) => {}
                None => break None,
            }
        }
    })
    .await
    .expect("the view ends when the server is dropped");
    assert!(
        matches!(ended, Some(NodeError::Unavailable { .. })),
        "{ended:?}"
    );
    let e = face
        .call(Command::DeviceOffer)
        .await
        .expect_err("a call on a closed connection fails");
    assert!(matches!(e, NodeError::Unavailable { .. }), "{e:?}");
    node.shutdown();
}

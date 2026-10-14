//! The node's behaviour one device at a time: the closed set of errors, the outbox when the home is away, and
//! a restart that rebuilds the views from `node.db`.

mod common;

use std::time::Duration;

use common::{Watched, World, call, id_of, space_of, until, window};
use han_node::api::{CallResult, Command, NodeApi, NodeError, Query, Row, SpaceLink};

const ALICE: &str = "@alice:a.localhost";
const BOB: &str = "@bob:a.localhost";

fn create(world: &World) -> Command {
    Command::SpaceCreate {
        home: world.home_ref.clone(),
        name: "team".into(),
        visibility: "members".into(),
        join: "invite".into(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn errors_are_the_closed_set() {
    let world = World::new(&[(ALICE, 31), (BOB, 32)]);
    let alice = world.node(ALICE, 31);
    let bob = world.node(BOB, 32);
    let space = space_of(&call(&alice, create(&world)).await);

    // not_found: a space this device does not follow
    let e = alice
        .call(Command::MemberSet {
            space: "nope:a.localhost".into(),
            actor: BOB.into(),
            role: "member".into(),
        })
        .await
        .unwrap_err();
    assert!(matches!(e, NodeError::NotFound { .. }), "{e:?}");
    // invalid: nothing to assign to, a role that does not exist
    let e = alice
        .call(Command::TaskCreate {
            space: space.clone(),
            to: vec![],
            ask: "x".into(),
            external: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(e, NodeError::Invalid { .. }), "{e:?}");
    let e = alice
        .call(Command::MemberSet {
            space: space.clone(),
            actor: BOB.into(),
            role: "emperor".into(),
        })
        .await
        .unwrap_err();
    assert!(matches!(e, NodeError::Invalid { .. }), "{e:?}");
    // denied: Policy refuses a member's attempt to make someone an owner... by a non-member
    let CallResult::Ticket { text } = call(
        &alice,
        Command::Invite {
            space: space.clone(),
            ttl_secs: None,
            expires_at: None,
            auto_admit: false,
        },
    )
    .await
    else {
        panic!("a ticket")
    };
    call(
        &bob,
        Command::SpaceJoin {
            ticket: text.clone(),
        },
    )
    .await;
    let e = bob
        .call(Command::MemberSet {
            space: space.clone(),
            actor: BOB.into(),
            role: "owner".into(),
        })
        .await
        .unwrap_err();
    assert!(matches!(e, NodeError::Denied { .. }), "{e:?}");
    // conflict: the same ticket's space is already followed; a closed task stays closed
    let e = bob
        .call(Command::SpaceJoin { ticket: text })
        .await
        .unwrap_err();
    assert!(matches!(e, NodeError::Conflict { .. }), "{e:?}");
    let task = id_of(
        &call(
            &alice,
            Command::TaskCreate {
                space: space.clone(),
                to: vec![ALICE.into()],
                ask: "do it".into(),
                external: None,
            },
        )
        .await,
    );
    call(
        &alice,
        Command::TaskCancel {
            space: space.clone(),
            task: task.clone(),
        },
    )
    .await;
    let e = alice
        .call(Command::TaskUpdate {
            space,
            task,
            state: "done".into(),
        })
        .await
        .unwrap_err();
    assert!(matches!(e, NodeError::Conflict { .. }), "{e:?}");
    // unavailable: no host role on this node until M4
    let e = alice
        .call(Command::AgentStart {
            space: "x:a.localhost".into(),
            agent: "@c:a.localhost".into(),
            harness: "pi".into(),
            repo: None,
            issue: None,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(e, NodeError::NotFound { .. }),
        "the space is checked first: {e:?}"
    );
    // a view of a space nobody follows fails with not_found and ends
    let mut w = Watched::open(
        &alice,
        Query::Overlaps {
            space: "nope:a.localhost".into(),
            window: window(),
        },
    );
    w.drain();
    assert!(
        w.mirror
            .failed()
            .is_some_and(|e| matches!(e, NodeError::NotFound { .. }))
    );
    // a session nothing was mirrored for yet has an empty timeline, and no patches
    let mut t = Watched::open(
        &alice,
        Query::Timeline {
            session: "00".repeat(32),
            at: None,
            window: window(),
        },
    );
    assert!(t.rows().is_empty() && t.mirror.failed().is_none());
    let mut p = Watched::open(
        &alice,
        Query::Patches {
            session: "00".repeat(32),
            from: None,
            to: "11".repeat(32),
            window: window(),
        },
    );
    p.drain();
    assert!(
        p.mirror
            .failed()
            .is_some_and(|e| matches!(e, NodeError::NotFound { .. }))
    );
    alice.shutdown();
    bob.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_entry_signed_offline_waits_in_the_outbox_and_is_sent_later() {
    let mut world = World::offline(&[(ALICE, 31)]);
    let alice = world.node_with(ALICE, 31, |c| c.connect_wait = Duration::from_millis(200));
    let mut outbox = Watched::open(
        &alice,
        Query::Outbox {
            space: None,
            window: window(),
        },
    );
    // The space is created locally and its first entry queued: the home is not there.
    let space = space_of(&call(&alice, create(&world)).await);
    until("the entry waits in the outbox", || {
        outbox.rows().iter().any(
            |r| matches!(r, Row::Outbox(o) if o.state == "pending" && o.kind == "space.create"),
        )
    })
    .await;
    world.start_home();
    until("the queued entry reaches the home", || {
        outbox
            .rows()
            .iter()
            .any(|r| matches!(r, Row::Outbox(o) if o.state == "sent" && o.seq == Some(1)))
    })
    .await;
    let mut status = Watched::open(
        &alice,
        Query::SpaceStatus {
            space,
            window: window(),
        },
    );
    until("the space is live", || {
        status
            .rows()
            .iter()
            .any(|r| matches!(r, Row::SpaceStatus(s) if s.link == SpaceLink::Live && s.outbox_pending == 0))
    })
    .await;
    alice.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_rebuilds_the_views_from_node_db() {
    let world = World::new(&[(ALICE, 31)]);
    let data = world.dir.join("alice-data");
    let tweak = {
        let data = data.clone();
        move |c: &mut han_node::NodeConfig| c.data_dir = Some(data)
    };
    let alice = world.node_with(ALICE, 31, tweak.clone());
    let space = space_of(&call(&alice, create(&world)).await);
    let session = id_of(
        &call(
            &alice,
            Command::SessionOpen {
                steer_mode: None,
                space: space.clone(),
                task: None,
                title: None,
            },
        )
        .await,
    );
    let mut board = Watched::open(
        &alice,
        Query::Board {
            space: space.clone(),
            group: Default::default(),
            collapsed: vec![],
            window: window(),
        },
    );
    until("the session is on the board", || board.rows().len() == 1).await;
    alice.shutdown();
    drop(board);

    // A world with no home: the second run can only know what node.db holds.
    let lonely = World::offline(&[(ALICE, 31)]);
    let again = lonely.node_with(ALICE, 31, tweak);
    let ev = again
        .read(&Query::Board {
            space: space.clone(),
            group: Default::default(),
            collapsed: vec![],
            window: window(),
        })
        .unwrap();
    match ev {
        han_node::api::ViewEvent::Snapshot { rows, .. } => {
            assert!(matches!(&rows[..], [Row::Board(b)] if b.session == session));
        }
        other => panic!("{other:?}"),
    }
    let st = again
        .read(&Query::SpaceStatus {
            space,
            window: window(),
        })
        .unwrap();
    match st {
        han_node::api::ViewEvent::Snapshot { rows, .. } => {
            assert!(
                matches!(&rows[..], [Row::SpaceStatus(s)] if s.head == 2 && s.link != SpaceLink::Live)
            );
        }
        other => panic!("{other:?}"),
    }
    again.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_actor_of_the_home_without_a_profile_is_refused_at_once() {
    let world = World::new(&[(ALICE, 31)]);
    let alice = world.node(ALICE, 31);
    let space = space_of(&call(&alice, create(&world)).await);
    let err = alice
        .call(Command::MemberSet {
            space,
            actor: "@exec:a.localhost".into(),
            role: "member".into(),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, NodeError::Invalid { message } if message.contains("has no profile at this home")),
        "{err:?}"
    );
    alice.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_entry_the_home_keeps_deferring_fails_and_can_be_retried() {
    let world = World::new(&[(ALICE, 31), (BOB, 32)]);
    let alice = world.node_with(ALICE, 31, |c| {
        c.outbox_max_attempts = 3;
        c.outbox_backoff = Duration::ZERO;
        c.connect_wait = Duration::from_millis(500);
    });
    let mut outbox = Watched::open(
        &alice,
        Query::Outbox {
            space: None,
            window: window(),
        },
    );
    let space = space_of(&call(&alice, create(&world)).await);
    // A foreign actor whose profile this home can never fetch: the home answers `unavailable` for ever.
    let stuck = match call(
        &alice,
        Command::MemberSet {
            space: space.clone(),
            actor: "@carol:far.example".into(),
            role: "member".into(),
        },
    )
    .await
    {
        CallResult::Queued { outbox_id, .. } => outbox_id,
        other => panic!("expected Queued, got {other:?}"),
    };
    until("the entry fails with its last error", || {
        outbox.rows().iter().any(|r| {
            matches!(r, Row::Outbox(o) if o.outbox_id == stuck && o.state == "failed"
                && o.error.as_deref().is_some_and(|e| e.contains("gave up after 3 tries")))
        })
    })
    .await;
    // It no longer holds the line: a later entry of the same space goes through.
    let later = call(
        &alice,
        Command::MemberSet {
            space: space.clone(),
            actor: BOB.into(),
            role: "member".into(),
        },
    )
    .await;
    assert!(matches!(later, CallResult::Receipt { .. }), "{later:?}");
    // The failure is in the node's recent errors.
    let mut status = Watched::open(&alice, Query::NodeStatus { window: window() });
    until("the failure is among the recent errors", || {
        status.rows().iter().any(|r| {
            matches!(r, Row::NodeStatus(n) if n.recent_errors.iter().any(|e| e.what.contains("outbox")))
        })
    })
    .await;
    // On demand it is tried again (and, the home still deferring, fails again).
    let again = call(&alice, Command::OutboxRetry { outbox_id: stuck }).await;
    assert!(
        matches!(again, CallResult::Queued { .. }) || matches!(again, CallResult::Receipt { .. }),
        "{again:?}"
    );
    let err = alice
        .call(Command::OutboxRetry { outbox_id: 9999 })
        .await
        .unwrap_err();
    assert!(matches!(err, NodeError::NotFound { .. }), "{err:?}");
    alice.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_actor_the_home_does_not_know_gets_a_misconfigured_link_with_the_reason() {
    let world = World::new(&[(ALICE, 31)]);
    // zed has a device key but no profile at the home: hello is answered `not_found`, which is a setup fault
    let zed = world.node("@zed:a.localhost", 40);
    call(
        &zed,
        Command::SpaceFollow {
            home: world.home_ref.clone(),
            space: "x1:a.localhost".into(),
        },
    )
    .await;
    let mut status = Watched::open(
        &zed,
        Query::SpaceStatus {
            space: "x1:a.localhost".into(),
            window: window(),
        },
    );
    until("the link is misconfigured and says why", || {
        status.rows().iter().any(|r| {
            matches!(r, Row::SpaceStatus(s) if s.link == SpaceLink::Misconfigured
                && s.error.as_deref().is_some_and(|e| e.contains("not_found")))
        })
    })
    .await;
    let mut node = Watched::open(&zed, Query::NodeStatus { window: window() });
    until("the node status lists the problem", || {
        node.rows().iter().any(|r| {
            matches!(r, Row::NodeStatus(n) if n.problems.iter().any(|p| p.contains("misconfigured")))
        })
    })
    .await;
    zed.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stuck_entry_is_shown_as_blocking_its_space() {
    let world = World::new(&[(ALICE, 31)]);
    let alice = world.node_with(ALICE, 31, |c| {
        c.outbox_max_attempts = 100_000;
        c.outbox_backoff = Duration::ZERO;
        c.connect_wait = Duration::from_millis(300);
    });
    let space = space_of(&call(&alice, create(&world)).await);
    let stuck = match call(
        &alice,
        Command::MemberSet {
            space: space.clone(),
            actor: "@carol:far.example".into(),
            role: "member".into(),
        },
    )
    .await
    {
        CallResult::Queued { outbox_id, .. } => outbox_id,
        other => panic!("expected Queued, got {other:?}"),
    };
    let mut outbox = Watched::open(
        &alice,
        Query::Outbox {
            space: None,
            window: window(),
        },
    );
    until("the entry is shown as blocking", || {
        outbox
            .rows()
            .iter()
            .any(|r| matches!(r, Row::Outbox(o) if o.outbox_id == stuck && o.blocking))
    })
    .await;
    let mut node = Watched::open(&alice, Query::NodeStatus { window: window() });
    until("the node status names the blocked space", || {
        node.rows().iter().any(|r| {
            matches!(r, Row::NodeStatus(n) if n.problems.iter().any(|p|
                p.contains(&format!("space {space} blocked by outbox entry {stuck}"))))
        })
    })
    .await;
    alice.shutdown();
}

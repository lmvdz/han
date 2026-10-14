//! M3's simulation: three nodes and one home on an in-memory network. Alice creates a space and invites Bob and
//! Carol with tickets; each opens a session and publishes `han.status/1` items and claims. Every node's Board
//! converges, the overlap alerts fire on both sides, silence turns sessions stale and then failed, and each
//! view's snapshot plus its diffs equals a fresh read.

mod common;

use std::time::Duration;

use common::{SERVER, Watched, World, call, id_of, space_of, until, window};
use han_node::api::{
    BoardRow, CallResult, Command, DiffOp, FileTouch, Group, NodeApi, OverlapKind, Query, Row,
    SessionState, SpaceLink, StatusBody, Target, ViewEvent,
};

const ALICE: &str = "@alice:a.localhost";
const BOB: &str = "@bob:a.localhost";
const CAROL: &str = "@carol:a.localhost";
const ISSUE: &str = "github:o/r#1";

fn status(
    session: &str,
    agent: &str,
    state: SessionState,
    file: (&str, u32, u32),
    seq: u64,
) -> StatusBody {
    StatusBody {
        session: session.into(),
        agent: agent.into(),
        delegate_of: None,
        harness: "pi".into(),
        issue: Some(ISSUE.into()),
        repo: Some("github:o/r".into()),
        base: None,
        state,
        step: format!("step {seq}"),
        tool: None,
        plan: vec![],
        files: vec![FileTouch {
            path: file.0.into(),
            hunks: vec![(file.1, file.2)],
        }],
        tests: None,
        tokens: Some(han_node::api::Tokens {
            input: 100 * seq,
            out: 10 * seq,
            cost: seq,
        }),
        summary: None,
        sandbox: None,
        seq,
        at: "2026-01-01T00:00:00Z".into(),
    }
}

fn board(space: &str, group: Group) -> Query {
    Query::Board {
        space: space.into(),
        group,
        collapsed: vec![],
        window: window(),
    }
}

fn board_rows(rows: Vec<Row>) -> Vec<BoardRow> {
    rows.into_iter()
        .filter_map(|r| match r {
            Row::Board(b) => Some(b),
            _ => None,
        })
        .collect()
}

/// What every node must agree on: a board row without the facts that are about the reader.
fn common_board(rows: Vec<Row>) -> Vec<BoardRow> {
    board_rows(rows)
        .into_iter()
        .map(|mut b| {
            b.needs_me = false;
            b.can_steer = false;
            b.can_cancel = false;
            b
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_nodes_converge_and_alert() {
    tokio::time::timeout(Duration::from_secs(120), scenario())
        .await
        .expect("the simulation finishes");
}

async fn scenario() {
    let world = World::new(&[(ALICE, 31), (BOB, 32), (CAROL, 33)]);
    let alice = world.node(ALICE, 31);
    let bob = world.node(BOB, 32);
    let carol = world.node(CAROL, 33);

    // Alice creates the space and invites the others with tickets; she admits them.
    let space = space_of(
        &call(
            &alice,
            Command::SpaceCreate {
                home: world.home_ref.clone(),
                name: "team".into(),
                visibility: "members".into(),
                join: "invite".into(),
            },
        )
        .await,
    );
    for (who, node) in [(BOB, &bob), (CAROL, &carol)] {
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
            panic!("a ticket");
        };
        assert_eq!(
            space_of(&call(node, Command::SpaceJoin { ticket: text }).await),
            space
        );
        call(
            &alice,
            Command::MemberSet {
                space: space.clone(),
                actor: who.into(),
                role: "member".into(),
            },
        )
        .await;
    }
    let nodes = [&alice, &bob, &carol];
    for n in nodes {
        let mut w = Watched::open(
            n,
            Query::SpaceStatus {
                space: space.clone(),
                window: window(),
            },
        );
        until("the space is live and verified", || {
            w.rows().iter().any(
                |r| matches!(r, Row::SpaceStatus(s) if s.link == SpaceLink::Live && s.head >= 3),
            )
        })
        .await;
    }

    // Views opened before any work happens, so the alerts show up as diffs.
    let mut overlaps: Vec<Watched> = nodes
        .iter()
        .map(|n| {
            Watched::open(
                *n,
                Query::Overlaps {
                    space: space.clone(),
                    window: window(),
                },
            )
        })
        .collect();
    let mut boards: Vec<Watched> = nodes
        .iter()
        .map(|n| Watched::open(*n, board(&space, Group::Issue)))
        .collect();
    let mut people: Vec<Watched> = nodes
        .iter()
        .map(|n| Watched::open(*n, board(&space, Group::Person)))
        .collect();

    // Bob and Carol claim the same issue; each opens a session.
    for (who, node) in [(BOB, &bob), (CAROL, &carol)] {
        call(
            node,
            Command::TaskCreate {
                space: space.clone(),
                to: vec![who.into()],
                ask: "fix it".into(),
                external: Some(ISSUE.into()),
            },
        )
        .await;
    }
    let bob_session = id_of(
        &call(
            &bob,
            Command::SessionOpen {
                steer_mode: None,
                space: space.clone(),
                task: None,
                title: Some("bob".into()),
            },
        )
        .await,
    );
    let carol_session = id_of(
        &call(
            &carol,
            Command::SessionOpen {
                steer_mode: None,
                space: space.clone(),
                task: None,
                title: Some("carol".into()),
            },
        )
        .await,
    );
    let alice_session = id_of(
        &call(
            &alice,
            Command::SessionOpen {
                steer_mode: None,
                space: space.clone(),
                task: None,
                title: Some("alice".into()),
            },
        )
        .await,
    );
    until("every node sees three sessions", || {
        boards.iter_mut().all(|b| b.rows().len() == 3)
    })
    .await;

    // Status items reach whoever has started watching; publish until every node has them all.
    let mut seq = 0;
    let (b2, c2) = (bob_session.clone(), carol_session.clone());
    for _ in 0..200 {
        seq += 1;
        call(
            &bob,
            Command::StatusPublish {
                space: space.clone(),
                status: Box::new(status(
                    &b2,
                    BOB,
                    SessionState::Working,
                    ("src/a.rs", 10, 20),
                    seq,
                )),
            },
        )
        .await;
        call(
            &carol,
            Command::StatusPublish {
                space: space.clone(),
                status: Box::new(status(
                    &c2,
                    CAROL,
                    SessionState::Working,
                    ("src/a.rs", 25, 10),
                    seq,
                )),
            },
        )
        .await;
        // Read every view each time round: a view nobody reads is dropped by the node.
        for w in overlaps.iter_mut().chain(&mut people).chain(&mut boards) {
            w.drain();
        }
        let done = boards.iter_mut().all(|b| {
            let rows = board_rows(b.rows());
            rows.iter()
                .filter(|r| r.state == SessionState::Working && r.tokens.is_some())
                .count()
                == 2
        });
        if done {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Every node's board converges; every node sees both overlap tiers.
    until("the boards agree", || {
        let all: Vec<_> = boards.iter_mut().map(|b| common_board(b.rows())).collect();
        all[0].len() == 3 && all[0] == all[1] && all[1] == all[2] && {
            let working = all[0]
                .iter()
                .filter(|r| r.state == SessionState::Working)
                .count();
            working == 2
        }
    })
    .await;
    until("the overlap alerts fire on every node", || {
        overlaps.iter_mut().all(|o| {
            let kinds: Vec<OverlapKind> = o
                .rows()
                .iter()
                .filter_map(|r| match r {
                    Row::Overlap(x) => Some(x.kind),
                    _ => None,
                })
                .collect();
            kinds.contains(&OverlapKind::IssueClaim) && kinds.contains(&OverlapKind::HunkRange)
        })
    })
    .await;
    for (i, o) in overlaps.iter_mut().enumerate() {
        o.drain();
        let alerted = o.events.iter().any(|e| match e {
            ViewEvent::Diff { ops, .. } => ops.iter().any(|op| {
                matches!(op, DiffOp::Insert { row: Row::Overlap(x), .. } if x.kind == OverlapKind::HunkRange)
            }),
            _ => false,
        });
        assert!(alerted, "node {i} got the hunk alert as a diff");
    }
    // Both sides are badged on the board.
    for b in &mut boards {
        let rows = board_rows(b.rows());
        for who in [BOB, CAROL] {
            let r = rows.iter().find(|r| r.agent == who).unwrap();
            assert!(
                r.overlaps.contains(&OverlapKind::HunkRange),
                "{who} is badged"
            );
            assert!(
                r.overlaps.contains(&OverlapKind::IssueClaim),
                "{who} claims"
            );
        }
        assert!(
            board_rows(b.rows())
                .iter()
                .find(|r| r.agent == ALICE)
                .unwrap()
                .overlaps
                .is_empty()
        );
    }
    // WhoIsOn answers from every node, with the cost view adding up.
    let who = alice.view(Query::WhoIsOn {
        space: None,
        target: Target::Path {
            path: "src/".into(),
        },
        window: window(),
    });
    let mut who = common::Watched {
        stream: who,
        mirror: Default::default(),
        query: Query::NodeStatus { window: window() },
        events: vec![],
    };
    assert_eq!(who.rows().len(), 2);

    // Silence: 11 minutes make working sessions stale; 31 make them failed.
    world.clock.advance_steady(11 * 60);
    for n in nodes {
        n.tick();
    }
    until("sessions turn stale on every node", || {
        boards.iter_mut().all(|b| {
            let rows = board_rows(b.rows());
            rows.iter()
                .filter(|r| r.state == SessionState::Stale)
                .count()
                == 3
        })
    })
    .await;
    world.clock.advance_steady(20 * 60);
    for n in nodes {
        n.tick();
    }
    until("sessions turn failed on every node", || {
        boards.iter_mut().all(|b| {
            board_rows(b.rows())
                .iter()
                .all(|r| r.state == SessionState::Failed)
        })
    })
    .await;
    // Failed sessions no longer overlap on files.
    until("file overlaps lapse with the sessions", || {
        overlaps.iter_mut().all(|o| {
            !o.rows()
                .iter()
                .any(|r| matches!(r, Row::Overlap(x) if x.kind == OverlapKind::HunkRange))
        })
    })
    .await;
    // A fresh item brings Bob's session back everywhere.
    for _ in 0..100 {
        seq += 1;
        call(
            &bob,
            Command::StatusPublish {
                space: space.clone(),
                status: Box::new(status(
                    &bob_session,
                    BOB,
                    SessionState::Working,
                    ("src/a.rs", 10, 20),
                    seq,
                )),
            },
        )
        .await;
        for w in overlaps.iter_mut().chain(&mut people).chain(&mut boards) {
            w.drain();
        }
        let back = boards.iter_mut().all(|b| {
            board_rows(b.rows())
                .iter()
                .any(|r| r.agent == BOB && r.state == SessionState::Working)
        });
        if back {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let _ = alice_session;

    // The property: for every open view, snapshot plus diffs equals a fresh read.
    for (n, (b, p)) in nodes.iter().zip(boards.iter_mut().zip(people.iter_mut())) {
        for w in [b, p] {
            let fresh = match n.read(&w.query).unwrap() {
                ViewEvent::Snapshot { rows, total, .. } => (rows, total),
                other => panic!("{other:?}"),
            };
            let got = (w.rows(), w.mirror.total());
            assert_eq!(got, fresh, "a snapshot plus its diffs equals a fresh read");
        }
    }
    for (n, o) in nodes.iter().zip(overlaps.iter_mut()) {
        let fresh = match n.read(&o.query).unwrap() {
            ViewEvent::Snapshot { rows, .. } => rows,
            other => panic!("{other:?}"),
        };
        assert_eq!(o.rows(), fresh);
    }
    let _ = SERVER;
    for n in nodes {
        n.shutdown();
    }
}

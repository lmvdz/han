//! Live work timelines across nodes: the session's actor publishes `han.timeline/1` batches on its stream,
//! and another member's `Timeline` view shows the points (a snapshot, then diffs) and `Patches` the changes.

mod common;

use std::collections::BTreeMap;

use common::{Watched, World, call, id_of, space_of, until, window};
use han_core::base64url;
use han_node::api::{CallResult, Command, NodeApi, PatchStatus, Query, Row, ViewEvent};
use han_node::timeline::StreamTimelines;
use han_node::views::TimelineSource;
use han_timeline::{
    At, History, HistoryKind, KeySigner, MemoryStore, ObjectId, Op, OpId, Signer, Value, Written,
};

const ALICE: &str = "@alice:a.localhost";
const BOB: &str = "@bob:a.localhost";

/// A worktree history as a host writes it: genesis with the tree, main at a base, the session fork.
struct Writer {
    signer: KeySigner,
    history: History<MemoryStore>,
    tree: ObjectId,
    session: OpId,
    out: Vec<(Written, Option<String>, Vec<String>)>,
}

fn entry(obj: &OpId) -> Value {
    let mut m = BTreeMap::new();
    m.insert(
        "obj".to_string(),
        Value::object_ref(obj).as_map().unwrap()["obj"].clone(),
    );
    m.insert("mode".to_string(), Value::text("100644"));
    m.insert("eol".to_string(), Value::text("lf"));
    Value::Map(m)
}

impl Writer {
    fn new(space: &str, actor: &str, seed: u8) -> Writer {
        let signer = KeySigner::from_seed(actor, "d1", [seed; 32]);
        let replica = std::sync::Arc::new(han_timeline::ReplicaId::new(actor, "d1", 0));
        let tree_id = OpId::new(2, replica);
        let (mut history, genesis) = History::create(
            MemoryStore::default(),
            space,
            &signer,
            0,
            HistoryKind::Worktree,
            None,
            vec![
                Op::MapCreate { tree: true },
                Op::MapSet {
                    target: ObjectId::Root,
                    key: "tree".into(),
                    value: Value::object_ref(&tree_id),
                },
            ],
        )
        .unwrap();
        let main = history.main().unwrap();
        let tree = ObjectId::Op(tree_id);
        let base = history
            .write(
                &signer,
                0,
                &main,
                vec![
                    Op::TextCreate {
                        text: "a\nb\n".into(),
                        transcript: false,
                    },
                    Op::MapSet {
                        target: tree.clone(),
                        key: "README.md".into(),
                        value: entry(&OpId::new(4, history.main().unwrap().replica.clone())),
                    },
                ],
                None,
            )
            .unwrap();
        let fork = history.fork(&signer, 0, &main, None, "session").unwrap();
        let session = fork.ops[0].clone();
        Writer {
            signer,
            history,
            tree,
            session,
            out: vec![
                (genesis, None, vec![]),
                (base, None, vec!["README.md".into()]),
                (fork, None, vec![]),
            ],
        }
    }

    /// Creates a file on the session timeline.
    fn create(&mut self, path: &str, text: &str, step: &str) {
        let next = self.out.last().unwrap().0.ops.last().unwrap().counter + 1;
        let obj = OpId::new(next, self.session.replica.clone());
        let w = self
            .history
            .write(
                &self.signer,
                0,
                &self.session,
                vec![
                    Op::TextCreate {
                        text: text.into(),
                        transcript: false,
                    },
                    Op::MapSet {
                        target: self.tree.clone(),
                        key: path.into(),
                        value: entry(&obj),
                    },
                ],
                Some(step.into()),
            )
            .unwrap();
        self.out.push((w, Some(step.into()), vec![path.into()]));
    }

    fn body(&self, w: &Written, step: &Option<String>, files: &[String]) -> serde_json::Value {
        let mut b = serde_json::json!({
            "history": self.history.id().to_hex(),
            "envelope": base64url::encode(&w.envelope),
            "actor": self.signer.actor(),
            "key": self.signer.key_id(),
            "public_key": base64url::encode(&self.signer.public_key()),
            "timeline": "session",
            "files": files,
        });
        if let Some(s) = step {
            b["step"] = s.clone().into();
        }
        b
    }
}

fn hv(v: serde_json::Value) -> han_core::json::Value {
    han_core::json::parse(v.to_string().as_bytes()).unwrap()
}

#[test]
fn stream_items_become_points_and_patches() {
    let mut w = Writer::new("s1:a.localhost", BOB, 9);
    w.create("src/new.rs", "fn main() {}\n", "toolu_1");
    let t = StreamTimelines::new();
    // Out of order: the session batch first, held back until its dependencies arrive.
    let (last, rest) = w.out.split_last().unwrap();
    let session = "e".repeat(64);
    assert!(!t.receive(
        &session,
        BOB,
        "2026-01-01T00:00:05Z",
        &hv(w.body(&last.0, &last.1, &last.2))
    ));
    for (x, s, f) in rest {
        t.receive(&session, BOB, "2026-01-01T00:00:01Z", &hv(w.body(x, s, f)));
    }
    let points = t.points(&session, None).unwrap();
    assert_eq!(points.len(), 1, "{points:?}");
    assert_eq!(points[0].label, "toolu_1");
    assert_eq!(points[0].files, 1);
    let patches = t.patches(&session, None, &points[0].point).unwrap();
    assert_eq!(patches.len(), 1);
    assert_eq!(patches[0].path, "src/new.rs");
    assert_eq!(patches[0].status, PatchStatus::Added);
    assert_eq!((patches[0].added, patches[0].removed), (1, 0));
    // Time travel: nothing at or before the base items' time.
    assert!(
        t.points(&session, Some("2026-01-01T00:00:02Z"))
            .unwrap()
            .is_empty()
    );
    // Another actor's batches for this session are refused.
    let other = StreamTimelines::new();
    for (x, s, f) in &w.out {
        assert!(!other.receive(&session, ALICE, "t", &hv(w.body(x, s, f))));
    }
    assert!(other.points(&session, None).unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_member_sees_the_live_timeline() {
    let world = World::new(&[(ALICE, 31), (BOB, 32)]);
    let alice = world.node(ALICE, 31);
    let bob = world.node(BOB, 32);
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
    call(&bob, Command::SpaceJoin { ticket: text }).await;
    call(
        &alice,
        Command::MemberSet {
            space: space.clone(),
            actor: BOB.into(),
            role: "member".into(),
        },
    )
    .await;
    let session = id_of(
        &call(
            &bob,
            Command::SessionOpen {
                steer_mode: None,
                space: space.clone(),
                task: None,
                title: None,
            },
        )
        .await,
    );
    // Alice's node learns of the session and watches it: proven by a status item arriving.
    common::session_known(&alice, &space, &session).await;
    let mut seen = Watched::open(
        &alice,
        Query::Session {
            id: session.clone(),
            part: han_node::api::SessionPart::Facts,
            window: window(),
        },
    );
    let mut st = han_node::api::StatusBody {
        session: session.clone(),
        agent: BOB.into(),
        delegate_of: None,
        harness: "pi".into(),
        issue: None,
        repo: None,
        base: None,
        state: han_node::api::SessionState::Working,
        step: "start".into(),
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
    };
    st.seq = 0;
    call(
        &bob,
        Command::StatusPublish {
            space: space.clone(),
            status: Box::new(st.clone()),
        },
    )
    .await;
    until("alice receives the status item", || {
        seen.rows()
            .iter()
            .any(|r| matches!(r, Row::Session(s) if s.status.is_some()))
    })
    .await;

    let mut tl = Watched::open(
        &alice,
        Query::Timeline {
            session: session.clone(),
            at: None,
            window: window(),
        },
    );
    tl.drain();
    assert!(matches!(tl.events.first(), Some(ViewEvent::Snapshot { rows, .. }) if rows.is_empty()));

    let mut w = Writer::new(&space, BOB, 32);
    w.create("auth/login.rs", "fn login() {}\n", "toolu_a");
    let publish = |w: &Writer, i: usize| {
        let (x, s, f) = &w.out[i];
        Command::StreamPublish {
            space: space.clone(),
            session: session.clone(),
            name: "han.timeline/1".into(),
            body: w.body(x, s, f).to_string(),
        }
    };
    for i in 0..w.out.len() {
        call(&bob, publish(&w, i)).await;
    }
    until("alice's view shows the first point", || {
        tl.rows()
            .iter()
            .any(|r| matches!(r, Row::TimelinePoint(p) if p.label == "toolu_a"))
    })
    .await;
    w.create("auth/token.rs", "fn token() {}\n", "toolu_b");
    let n = w.out.len() - 1;
    call(&bob, publish(&w, n)).await;
    until("the second point arrives as a diff", || {
        tl.rows()
            .iter()
            .filter(|r| matches!(r, Row::TimelinePoint(_)))
            .count()
            == 2
    })
    .await;
    assert!(
        tl.events
            .iter()
            .any(|e| matches!(e, ViewEvent::Diff { .. }))
    );
    let point = tl
        .rows()
        .iter()
        .find_map(|r| match r {
            Row::TimelinePoint(p) if p.label == "toolu_b" => Some(p.point.clone()),
            _ => None,
        })
        .unwrap();
    let ViewEvent::Snapshot { rows, .. } = alice
        .read(&Query::Patches {
            session: session.clone(),
            from: None,
            to: point,
            window: window(),
        })
        .unwrap()
    else {
        panic!("a snapshot")
    };
    let paths: Vec<String> = rows
        .iter()
        .filter_map(|r| match r {
            Row::FilePatch(p) => Some(p.path.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(paths, vec!["auth/login.rs", "auth/token.rs"]);
    let _ = At::Timeline(w.session.clone());
    let _: &dyn NodeApi = &alice;
}

#[test]
fn the_excluded_count_and_the_masked_spans_come_with_the_changes() {
    use han_node::views::{Engine, Freshness};
    let mut w = Writer::new("s1:a.localhost", BOB, 9);
    w.create(
        "src/conf.rs",
        "const KEY: &str = \"[masked:aws_key]\";\n",
        "toolu_1",
    );
    let t = std::sync::Arc::new(StreamTimelines::new());
    let session = "e".repeat(64);
    for (i, (x, s, f)) in w.out.iter().enumerate() {
        let mut body = w.body(x, s, f);
        // The host tells how many paths it kept out of the mirror.
        if i == w.out.len() - 1 {
            body["excluded"] = 2.into();
        }
        t.receive(&session, BOB, "2026-01-01T00:00:01Z", &hv(body));
    }
    assert_eq!(t.excluded(&session), 2);
    assert_eq!(t.excluded("another session"), 0);
    let point = t.points(&session, None).unwrap()[0].point.clone();
    let mut engine = Engine::new(ALICE, Freshness::default());
    engine.set_timeline(t);
    let ViewEvent::Snapshot { rows, .. } = engine
        .read(&Query::Patches {
            session: session.clone(),
            from: None,
            to: point,
            window: window(),
        })
        .unwrap()
    else {
        panic!("a snapshot")
    };
    // The count first (it sorts first), never a name; then the files, with the masked spans marked.
    let Row::ChangeSummary(summary) = &rows[0] else {
        panic!("a summary: {:?}", rows[0])
    };
    assert_eq!(summary.excluded, 2);
    let Row::FilePatch(file) = &rows[1] else {
        panic!("a file")
    };
    assert_eq!(file.path, "src/conf.rs");
    assert_eq!(file.masked.len(), 1);
    let units: Vec<u16> = file.patch.encode_utf16().collect();
    let (start, end) = file.masked[0];
    assert_eq!(
        String::from_utf16(&units[start as usize..end as usize]).unwrap(),
        "[masked:aws_key]"
    );
    // A session with nothing excluded sends no summary.
    let t2 = std::sync::Arc::new(StreamTimelines::new());
    for (x, s, f) in &w.out {
        t2.receive(&session, BOB, "2026-01-01T00:00:01Z", &hv(w.body(x, s, f)));
    }
    let point = t2.points(&session, None).unwrap()[0].point.clone();
    engine.set_timeline(t2);
    let ViewEvent::Snapshot { rows, .. } = engine
        .read(&Query::Patches {
            session,
            from: None,
            to: point,
            window: window(),
        })
        .unwrap()
    else {
        panic!("a snapshot")
    };
    assert!(rows.iter().all(|r| matches!(r, Row::FilePatch(_))));
}

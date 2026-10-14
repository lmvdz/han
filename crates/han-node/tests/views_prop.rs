//! The views engine's property (PLAN section 5): a snapshot plus every later diff equals a fresh read. Random
//! sequences of records, status items, clock ticks, link changes and outbox changes drive an engine with
//! views open on every query; after each step every view's mirror must equal `Engine::read`.

use std::collections::BTreeMap;

use han_core::Value;
use han_core::ids::sha256_hex;
use han_core::types::{Durability, Entry, LogRecord, Receipt};
use han_node::api::{
    FileTouch, Filter, Group, Query, Row, Scope, SessionState, SpaceLink, StatusBody, Target,
    Tokens, ViewEvent, ViewMirror, ViewStream, Window,
};
use han_node::views::{Engine, Freshness, OutboxItem};
use proptest::prelude::*;

const SPACE: &str = "s1:a.localhost";
const ACTORS: [&str; 3] = [
    "@alice:a.localhost",
    "@bob:a.localhost",
    "@carol:a.localhost",
];
const PATHS: [&str; 3] = ["src/a.rs", "src/b.rs", "docs/"];
const ISSUES: [&str; 2] = ["github:o/r#1", "github:o/r#2"];

#[derive(Clone, Debug)]
enum Op {
    OpenSession {
        actor: usize,
        task: Option<usize>,
    },
    CloseSession(usize),
    Task {
        to: usize,
        issue: Option<usize>,
    },
    TaskDone(usize),
    Member {
        actor: usize,
        role: usize,
    },
    Propose {
        proposer: usize,
        executor: usize,
    },
    Approve {
        effect: usize,
        author: usize,
    },
    Lease {
        actor: usize,
        path: usize,
        ttl: u64,
    },
    Status {
        session: usize,
        state: usize,
        seq: u64,
        path: usize,
        start: u32,
        len: u32,
        issue: Option<usize>,
    },
    Tick(u64),
    Link(usize),
    Outbox(u8),
    Signal,
    Subscribe(usize),
    Unsubscribe(usize),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (0..3usize, proptest::option::of(0..4usize)).prop_map(|(actor, task)| Op::OpenSession { actor, task }),
        1 => (0..6usize).prop_map(Op::CloseSession),
        2 => (0..3usize, proptest::option::of(0..2usize)).prop_map(|(to, issue)| Op::Task { to, issue }),
        1 => (0..4usize).prop_map(Op::TaskDone),
        1 => (0..3usize, 0..4usize).prop_map(|(actor, role)| Op::Member { actor, role }),
        2 => (0..3usize, 0..3usize).prop_map(|(proposer, executor)| Op::Propose { proposer, executor }),
        2 => (0..4usize, 0..3usize).prop_map(|(effect, author)| Op::Approve { effect, author }),
        2 => (0..3usize, 0..3usize, 1..4000u64).prop_map(|(actor, path, ttl)| Op::Lease { actor, path, ttl }),
        8 => (0..6usize, 0..7usize, 1..30u64, 0..3usize, 0..40u32, 0..20u32, proptest::option::of(0..2usize))
            .prop_map(|(session, state, seq, path, start, len, issue)| Op::Status { session, state, seq, path, start, len, issue }),
        5 => (0..2500u64).prop_map(Op::Tick),
        1 => (0..4usize).prop_map(Op::Link),
        1 => (0..4u8).prop_map(Op::Outbox),
        1 => Just(Op::Signal),
        2 => (0..16usize).prop_map(Op::Subscribe),
        1 => (0..16usize).prop_map(Op::Unsubscribe),
    ]
}

fn window(limit: u32, cursor: Option<&str>, filter: Filter) -> Window {
    Window {
        filter,
        limit,
        cursor: cursor.map(str::to_owned),
    }
}

fn queries() -> Vec<Query> {
    let all = Filter {
        include_closed: true,
        ..Filter::default()
    };
    let mut q = Vec::new();
    for group in [Group::Issue, Group::Person, Group::State] {
        q.push(Query::Board {
            space: SPACE.into(),
            group,
            collapsed: vec![],
            window: window(200, None, Filter::default()),
        });
        q.push(Query::Board {
            space: SPACE.into(),
            group,
            collapsed: vec!["".into(), ACTORS[0].into(), "working".into()],
            window: window(2, None, all.clone()),
        });
    }
    q.push(Query::Board {
        space: SPACE.into(),
        group: Group::Person,
        collapsed: vec![],
        window: window(
            3,
            Some(&format!("{}|", ACTORS[0])),
            Filter {
                states: vec![SessionState::Working, SessionState::Stale],
                include_closed: true,
                ..Filter::default()
            },
        ),
    });
    q.push(Query::Board {
        space: SPACE.into(),
        group: Group::Person,
        collapsed: vec![],
        window: window(
            200,
            None,
            Filter {
                path: vec!["src/**".into()],
                overlap_min: Some(2),
                harness: vec!["PI".into()],
                ..Filter::default()
            },
        ),
    });
    q.push(Query::Board {
        space: SPACE.into(),
        group: Group::State,
        collapsed: vec![],
        window: window(
            200,
            None,
            Filter {
                mine: true,
                person: vec![ACTORS[0].into(), "@bob".into()],
                repo: vec!["r".into()],
                include_closed: true,
                ..Filter::default()
            },
        ),
    });
    q.push(Query::Overlaps {
        space: SPACE.into(),
        window: window(200, None, Filter::default()),
    });
    q.push(Query::Overlaps {
        space: SPACE.into(),
        window: window(1, None, Filter::default()),
    });
    q.push(Query::WhoIsOn {
        space: None,
        target: Target::Issue {
            issue: ISSUES[0].into(),
        },
        window: window(200, None, Filter::default()),
    });
    q.push(Query::WhoIsOn {
        space: Some(SPACE.into()),
        target: Target::Path {
            path: "src/".into(),
        },
        window: window(200, None, Filter::default()),
    });
    for a in ACTORS {
        q.push(Query::Approvals {
            actor: a.into(),
            window: window(200, None, Filter::default()),
        });
    }
    q.push(Query::SpaceStatus {
        space: SPACE.into(),
        window: window(200, None, Filter::default()),
    });
    q.push(Query::Cost {
        scope: Scope::Space {
            space: SPACE.into(),
        },
        window: window(2, None, Filter::default()),
    });
    q.push(Query::Outbox {
        space: None,
        window: window(200, None, Filter::default()),
    });
    q.push(Query::NodeStatus {
        window: window(200, None, Filter::default()),
    });
    q.push(Query::JoinProgress {
        space: SPACE.into(),
        window: window(200, None, Filter::default()),
    });
    q
}

fn obj(pairs: Vec<(&str, Value)>) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

fn s(t: &str) -> Value {
    Value::Str(t.to_owned())
}

struct Driver {
    engine: Engine,
    seq: u64,
    ms: u64,
    sessions: Vec<String>,
    tasks: Vec<String>,
    effects: Vec<String>,
    open: Vec<(usize, ViewStream, ViewMirror, u64)>,
    queries: Vec<Query>,
    status_seq: BTreeMap<String, u64>,
}

impl Driver {
    fn new() -> Driver {
        let mut engine = Engine::new(
            ACTORS[0],
            Freshness {
                stale_ms: 1_000,
                failed_ms: 3_000,
            },
        );
        engine.add_space(SPACE, "a.localhost", None);
        let mut d = Driver {
            engine,
            seq: 0,
            ms: 0,
            sessions: vec![],
            tasks: vec![],
            effects: vec![],
            open: vec![],
            queries: queries(),
            status_seq: BTreeMap::new(),
        };
        d.record(
            0,
            "space.create",
            obj(vec![(
                "policy",
                han_seq::build::space_policy("members", "invite"),
            )]),
        );
        d.record(
            0,
            "member.set",
            obj(vec![("actor", s(ACTORS[1])), ("role", s("member"))]),
        );
        d.record(
            0,
            "member.set",
            obj(vec![("actor", s(ACTORS[2])), ("role", s("member"))]),
        );
        for i in 0..d.queries.len() {
            d.subscribe(i);
        }
        d
    }

    fn record(&mut self, actor: usize, kind: &str, body: Value) -> String {
        self.seq += 1;
        let id = sha256_hex(format!("record {}", self.seq).as_bytes());
        let entry = Entry {
            v: 1,
            space: SPACE.into(),
            kind: kind.into(),
            author: ACTORS[actor].into(),
            key_id: "d1".into(),
            body,
            content_hash: String::new(),
            refs: vec![],
            seen: self.seq - 1,
            nonce: "aaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            time: "2026-01-01T00:00:00Z".into(),
            sig: String::new(),
        };
        let receipt = Receipt {
            space: SPACE.into(),
            seq: self.seq,
            id: id.clone(),
            prev: String::new(),
            home: "a.localhost".into(),
            home_key: "h1".into(),
            time: format!("2026-01-01T00:{:02}:00Z", (self.ms / 60_000).min(59)),
            durability: Durability::Local,
            profiles: BTreeMap::new(),
            home_sig: String::new(),
        };
        let rec = LogRecord {
            redacted: false,
            redacted_by: None,
            entry,
            content: None,
            receipt,
        };
        self.engine.apply_record(SPACE, &rec);
        id
    }

    fn subscribe(&mut self, i: usize) {
        let q = self.queries[i % self.queries.len()].clone();
        let (sink, stream) = ViewStream::channel();
        if let Some(id) = self.engine.subscribe(q, sink) {
            self.open
                .push((i % self.queries.len(), stream, ViewMirror::new(), id));
        }
    }

    fn step(&mut self, op: &Op) {
        match op {
            Op::OpenSession { actor, task } => {
                let mut body = vec![("steerMode", s("auto"))];
                if let Some(t) =
                    task.and_then(|t| self.tasks.get(t % self.tasks.len().max(1)).cloned())
                {
                    body.push(("task", s(&t)));
                }
                let id = self.record(*actor, "session.open", obj(body));
                self.sessions.push(id);
            }
            Op::CloseSession(i) => {
                if let Some(id) = self.sessions.get(i % self.sessions.len().max(1)).cloned() {
                    self.record(0, "session.close", obj(vec![("session", s(&id))]));
                }
            }
            Op::Task { to, issue } => {
                let mut body = vec![("to", Value::Array(vec![s(ACTORS[*to])]))];
                if let Some(i) = issue {
                    body.push(("external", s(ISSUES[*i])));
                }
                let id = self.record(*to, "task", obj(body));
                self.tasks.push(id);
            }
            Op::TaskDone(i) => {
                if let Some(id) = self.tasks.get(i % self.tasks.len().max(1)).cloned() {
                    self.record(
                        0,
                        "task.update",
                        obj(vec![("task", s(&id)), ("state", s("done"))]),
                    );
                }
            }
            Op::Member { actor, role } => {
                let role = ["owner", "member", "guest", "none"][*role];
                self.record(
                    0,
                    "member.set",
                    obj(vec![("actor", s(ACTORS[*actor])), ("role", s(role))]),
                );
            }
            Op::Propose { proposer, executor } => {
                let id = self.record(
                    *proposer,
                    "effect.propose",
                    obj(vec![
                        ("executor", s(ACTORS[*executor])),
                        ("action", s("git_push")),
                        ("planDigest", s(&sha256_hex(b"plan"))),
                        ("repeatable", Value::Bool(false)),
                    ]),
                );
                self.effects.push(id);
            }
            Op::Approve { effect, author } => {
                if let Some(id) = self
                    .effects
                    .get(effect % self.effects.len().max(1))
                    .cloned()
                {
                    self.record(
                        *author,
                        "effect.approve",
                        obj(vec![
                            ("proposal", s(&id)),
                            ("planDigest", s(&sha256_hex(b"plan"))),
                        ]),
                    );
                }
            }
            Op::Lease { actor, path, ttl } => {
                self.record(
                    *actor,
                    "han.lease",
                    obj(vec![
                        ("paths", Value::Array(vec![s(PATHS[*path])])),
                        ("ttl", Value::Int(i64::try_from(*ttl).unwrap())),
                    ]),
                );
            }
            Op::Status {
                session,
                state,
                seq,
                path,
                start,
                len,
                issue,
            } => {
                let Some(id) = self
                    .sessions
                    .get(session % self.sessions.len().max(1))
                    .cloned()
                else {
                    return;
                };
                let n = self.status_seq.entry(id.clone()).or_default();
                *n = (*n).max(*seq);
                let states = [
                    SessionState::Working,
                    SessionState::WaitingApproval,
                    SessionState::Blocked,
                    SessionState::Idle,
                    SessionState::Done,
                    SessionState::Stale,
                    SessionState::Failed,
                ];
                self.engine.apply_status(
                    SPACE,
                    StatusBody {
                        session: id,
                        agent: ACTORS[session % 3].into(),
                        delegate_of: None,
                        harness: "pi".into(),
                        issue: issue.map(|i| ISSUES[i].to_owned()),
                        repo: Some("github:o/r".into()),
                        base: None,
                        state: states[*state],
                        step: format!("step {seq}"),
                        tool: None,
                        plan: vec![],
                        files: vec![FileTouch {
                            path: PATHS[*path].into(),
                            hunks: vec![(*start, *len)],
                        }],
                        tests: None,
                        tokens: Some(Tokens {
                            input: *seq,
                            out: 1,
                            cost: 1,
                        }),
                        summary: None,
                        sandbox: None,
                        seq: *seq,
                        at: "2026-01-01T00:00:00Z".into(),
                    },
                );
            }
            Op::Tick(ms) => {
                self.ms += ms;
                self.engine.tick(
                    self.ms,
                    1_767_225_600 + i64::try_from(self.ms / 1000).unwrap(),
                );
            }
            Op::Link(i) => {
                let link = [
                    SpaceLink::Connecting,
                    SpaceLink::Live,
                    SpaceLink::Offline,
                    SpaceLink::Rejected,
                ][*i];
                self.engine.set_link(
                    SPACE,
                    link,
                    (*i == 2).then(|| "not readable yet".to_owned()),
                );
            }
            Op::Outbox(n) => {
                let items = (0..*n)
                    .map(|i| OutboxItem {
                        id: u64::from(i) + 1,
                        entry_id: sha256_hex(&[i]),
                        kind: "task".into(),
                        state: if i % 2 == 0 { "pending" } else { "sent" }.into(),
                        attempts: u32::from(i),
                        seq: None,
                        error: None,
                    })
                    .collect();
                self.engine.set_outbox(SPACE, items);
            }
            Op::Signal => self.engine.count_signal(SPACE),
            Op::Subscribe(i) => self.subscribe(*i),
            Op::Unsubscribe(i) => {
                if !self.open.is_empty() {
                    let at = i % self.open.len();
                    let (_, _, _, id) = self.open.remove(at);
                    self.engine.unsubscribe(id);
                }
            }
        }
    }

    fn check(&mut self) {
        for (qi, stream, mirror, _) in &mut self.open {
            while let Some(ev) = stream.try_next() {
                mirror.apply(ev);
            }
            let q = &self.queries[*qi];
            let ViewEvent::Snapshot {
                rows,
                total,
                offset,
                next_cursor,
                totals,
                ..
            } = self.engine.read(q).unwrap()
            else {
                panic!("a read is a snapshot");
            };
            let got: Vec<Row> = mirror.rows();
            assert_eq!(got, rows, "rows of {q:?}");
            assert!(!mirror.needs_resync(), "a gap in the seq of {q:?}");
            assert_eq!(mirror.totals().cloned(), totals, "totals of {q:?}");
            assert_eq!(
                totals.is_some(),
                matches!(q, Query::Board { .. }),
                "only a board has totals"
            );
            assert_eq!(
                (mirror.total(), mirror.offset()),
                (total, offset),
                "window of {q:?}"
            );
            assert_eq!(
                mirror.next_cursor().map(str::to_owned),
                next_cursor,
                "cursor of {q:?}"
            );
            // Rows come ordered by key, and never beyond the limit.
            let keys: Vec<&str> = rows.iter().map(Row::key).collect();
            assert!(keys.windows(2).all(|w| w[0] < w[1]));
            assert!(rows.len() <= q.window().limit as usize);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn a_snapshot_plus_its_diffs_equals_a_fresh_read(ops in proptest::collection::vec(op(), 1..80)) {
        let mut d = Driver::new();
        d.check();
        for op in &ops {
            d.step(op);
            d.check();
        }
    }
}

#[test]
fn silence_turns_sessions_stale_then_failed() {
    let mut d = Driver::new();
    d.step(&Op::OpenSession {
        actor: 1,
        task: None,
    });
    d.step(&Op::Status {
        session: 0,
        state: 0,
        seq: 1,
        path: 0,
        start: 1,
        len: 1,
        issue: None,
    });
    let state = |d: &mut Driver| {
        let q = Query::Session {
            id: d.sessions[0].clone(),
            window: Window::default(),
        };
        match d.engine.read(&q).unwrap() {
            ViewEvent::Snapshot { rows, .. } => match &rows[0] {
                Row::Session(r) => r.state,
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(state(&mut d), SessionState::Working);
    d.step(&Op::Tick(999));
    assert_eq!(state(&mut d), SessionState::Working);
    d.step(&Op::Tick(1));
    assert_eq!(state(&mut d), SessionState::Stale);
    d.step(&Op::Tick(2_000));
    assert_eq!(state(&mut d), SessionState::Failed);
    d.step(&Op::Status {
        session: 0,
        state: 0,
        seq: 2,
        path: 0,
        start: 1,
        len: 1,
        issue: None,
    });
    assert_eq!(state(&mut d), SessionState::Working);
    d.check();
}

fn open_board(d: &mut Driver, filter: Filter, collapsed: Vec<String>) -> (ViewStream, u64) {
    let (sink, stream) = ViewStream::channel();
    let id = d
        .engine
        .subscribe(
            Query::Board {
                space: SPACE.into(),
                group: Group::Issue,
                collapsed,
                window: window(200, None, filter),
            },
            sink,
        )
        .expect("a board");
    (stream, id)
}

fn status(d: &mut Driver, session: usize, state: usize, seq: u64, issue: usize) {
    d.step(&Op::Status {
        session,
        state,
        seq,
        path: 0,
        start: 1,
        len: 1,
        issue: Some(issue),
    });
}

fn totals_of(ev: ViewEvent) -> han_node::api::BoardTotals {
    match ev {
        ViewEvent::Snapshot { totals, .. } => totals.expect("a board has totals"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn board_totals_count_every_state_with_and_without_the_filter() {
    let mut d = Driver::new();
    for actor in [0, 1, 2, 0] {
        d.step(&Op::OpenSession { actor, task: None });
    }
    // alice (me): waiting; bob: working; carol: working; alice: idle on another issue.
    status(&mut d, 0, 1, 1, 0);
    status(&mut d, 1, 0, 1, 0);
    status(&mut d, 2, 0, 1, 1);
    status(&mut d, 3, 3, 1, 1);
    let filter = Filter {
        states: vec![SessionState::Working],
        ..Filter::default()
    };
    let t = totals_of(
        d.engine
            .read(&Query::Board {
                space: SPACE.into(),
                group: Group::Issue,
                collapsed: vec![],
                window: window(200, None, filter),
            })
            .unwrap(),
    );
    assert_eq!(
        (t.all.working, t.all.waiting_approval, t.all.idle),
        (2, 1, 1)
    );
    assert_eq!(t.all.sum(), 4);
    assert_eq!((t.filtered.working, t.filtered.waiting_approval), (2, 0));
    assert_eq!(t.filtered.sum(), 2);
    assert_eq!(
        t.needs_me_total, 1,
        "the filter does not hide what needs me"
    );
    let groups: Vec<(&str, u32, u32)> = t
        .groups
        .iter()
        .map(|g| (g.group.as_str(), g.count, g.needs_me))
        .collect();
    assert_eq!(groups, vec![(ISSUES[0], 1, 0), (ISSUES[1], 1, 0)]);
}

#[test]
fn a_collapsed_group_keeps_its_totals_while_its_rows_leave_the_window() {
    let mut d = Driver::new();
    for actor in [0, 1] {
        d.step(&Op::OpenSession { actor, task: None });
    }
    status(&mut d, 0, 2, 1, 0);
    status(&mut d, 1, 0, 1, 1);
    let ev = d
        .engine
        .read(&Query::Board {
            space: SPACE.into(),
            group: Group::Issue,
            collapsed: vec![ISSUES[0].into()],
            window: window(200, None, Filter::default()),
        })
        .unwrap();
    let ViewEvent::Snapshot {
        rows,
        total,
        totals,
        ..
    } = ev
    else {
        panic!("a snapshot")
    };
    assert_eq!(
        (rows.len(), total),
        (1, 1),
        "the collapsed row is out of the window"
    );
    let t = totals.unwrap();
    assert_eq!(t.filtered.sum(), 2, "the state strip still counts it");
    let g = t.groups.iter().find(|g| g.group == ISSUES[0]).unwrap();
    assert_eq!((g.count, g.needs_me), (1, 1), "and so does its header");
}

#[test]
fn totals_travel_with_the_diffs_of_a_board() {
    let mut d = Driver::new();
    d.step(&Op::OpenSession {
        actor: 0,
        task: None,
    });
    status(&mut d, 0, 0, 1, 0);
    let (mut stream, _id) = open_board(&mut d, Filter::default(), vec![]);
    let mut mirror = ViewMirror::new();
    while let Some(ev) = stream.try_next() {
        mirror.apply(ev);
    }
    assert_eq!(mirror.totals().unwrap().all.working, 1);
    status(&mut d, 0, 2, 2, 0);
    while let Some(ev) = stream.try_next() {
        assert!(matches!(
            ev,
            ViewEvent::Diff {
                totals: Some(_),
                ..
            }
        ));
        mirror.apply(ev);
    }
    let t = mirror.totals().unwrap();
    assert_eq!((t.all.working, t.all.blocked, t.needs_me_total), (0, 1, 1));
}

#[test]
fn every_event_of_a_view_is_numbered_and_a_gap_asks_for_a_new_snapshot() {
    let mut d = Driver::new();
    d.step(&Op::OpenSession {
        actor: 0,
        task: None,
    });
    let (mut stream, _id) = open_board(&mut d, Filter::default(), vec![]);
    for n in 1..=3 {
        status(&mut d, 0, n % 2, u64::try_from(n).unwrap(), 0);
    }
    let mut events = vec![];
    while let Some(ev) = stream.try_next() {
        events.push(ev);
    }
    let seqs: Vec<u64> = events.iter().map(ViewEvent::seq).collect();
    assert_eq!(
        seqs,
        vec![1, 2, 3, 4],
        "the snapshot, then one number per diff"
    );
    // A face that misses the second event does not apply the third.
    let mut mirror = ViewMirror::new();
    mirror.apply(events[0].clone());
    mirror.apply(events[1].clone());
    let before = mirror.rows();
    mirror.apply(events[3].clone());
    assert!(mirror.needs_resync());
    assert_eq!(mirror.rows(), before, "a diff out of order is not applied");
    assert_eq!(mirror.seq(), 2);
    // A new snapshot clears it.
    mirror.apply(events[0].clone());
    assert!(!mirror.needs_resync());
}

#[test]
fn a_face_that_falls_behind_is_told_to_resync_and_the_view_ends() {
    let mut d = Driver::new();
    d.step(&Op::OpenSession {
        actor: 0,
        task: None,
    });
    status(&mut d, 0, 0, 1, 0);
    let (mut stream, _id) = open_board(&mut d, Filter::default(), vec![]);
    let open = d.engine.open_views();
    // The face reads nothing while the session changes more often than the backlog holds.
    for n in 2..400u64 {
        status(&mut d, 0, 0, n, 0);
    }
    assert!(
        d.engine.open_views() < open,
        "the slow views are dropped by the engine"
    );
    let mut events = vec![];
    while let Some(ev) = stream.try_next() {
        events.push(ev);
    }
    assert_eq!(events.len(), 1, "the backlog is dropped: {events:?}");
    assert!(matches!(events[0], ViewEvent::Resync { .. }));
    let mut m = ViewMirror::new();
    m.apply(events[0].clone());
    assert!(m.needs_resync());
}

#[test]
fn a_node_that_stops_ends_its_views_without_an_error() {
    let mut d = Driver::new();
    let (mut stream, _id) = open_board(&mut d, Filter::default(), vec![]);
    d.engine.end_all();
    assert!(matches!(
        stream.try_next(),
        Some(ViewEvent::Snapshot { seq: 1, .. })
    ));
    assert!(matches!(stream.try_next(), Some(ViewEvent::End { seq: 2 })));
    assert!(stream.try_next().is_none());
    assert_eq!(d.engine.open_views(), 0);
    let mut m = ViewMirror::new();
    m.apply(ViewEvent::End { seq: 2 });
    assert!(m.ended() && m.failed().is_none());
}

#[test]
fn windows_are_bounded_per_view() {
    let timeline = Query::Timeline {
        session: "s".into(),
        at: None,
        window: Window::default(),
    };
    assert_eq!(timeline.max_limit(), 2_000);
    assert_eq!(
        Query::Session {
            id: "s".into(),
            window: Window::default()
        }
        .max_limit(),
        200
    );
    assert_eq!(
        Query::NodeStatus {
            window: Window::default()
        }
        .max_limit(),
        10_000
    );
    // A limit of 0 still shows one row, and says where the next one is.
    let mut d = Driver::new();
    for actor in [0, 1] {
        d.step(&Op::OpenSession { actor, task: None });
    }
    let ev = d
        .engine
        .read(&Query::Board {
            space: SPACE.into(),
            group: Group::Issue,
            collapsed: vec![],
            window: window(0, None, Filter::default()),
        })
        .unwrap();
    let ViewEvent::Snapshot {
        rows, next_cursor, ..
    } = ev
    else {
        panic!("a snapshot")
    };
    assert_eq!(rows.len(), 1);
    assert!(next_cursor.is_some());
}

#[test]
fn the_new_events_cross_the_pipe_and_old_ones_still_decode() {
    for ev in [
        ViewEvent::Resync { seq: 9 },
        ViewEvent::End { seq: 4 },
        ViewEvent::Failed {
            seq: 2,
            error: han_node::api::NodeError::unavailable("gone"),
        },
    ] {
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["seq"], ev.seq());
        assert_eq!(serde_json::from_value::<ViewEvent>(json).unwrap(), ev);
    }
    // An event from a node that does not number them reads as number 0 with no totals.
    let old = serde_json::json!({"type": "snapshot", "rows": [], "total": 0, "offset": 0});
    let ev: ViewEvent = serde_json::from_value(old).unwrap();
    assert!(matches!(
        ev,
        ViewEvent::Snapshot {
            seq: 0,
            totals: None,
            ..
        }
    ));
}

// ---- the filter grammar, row reasons and overlap detail (PR B) -------------------------------------------

fn put_status(d: &mut Driver, session: usize, f: impl FnOnce(&mut StatusBody)) {
    let id = d.sessions[session].clone();
    let n = d.status_seq.entry(id.clone()).or_default();
    *n += 1;
    let mut body = StatusBody {
        session: id,
        agent: ACTORS[0].into(),
        delegate_of: None,
        harness: "pi".into(),
        issue: None,
        repo: Some("github:o/r".into()),
        base: None,
        state: SessionState::Working,
        step: "working".into(),
        tool: None,
        plan: vec![],
        files: vec![],
        tests: None,
        tokens: None,
        summary: None,
        sandbox: None,
        seq: *n,
        at: "2026-01-01T00:00:00Z".into(),
    };
    f(&mut body);
    assert!(d.engine.apply_status(SPACE, body));
}

fn touch(path: &str, hunks: &[(u32, u32)]) -> FileTouch {
    FileTouch {
        path: path.into(),
        hunks: hunks.to_vec(),
    }
}

fn board(d: &mut Driver, filter: Filter) -> Vec<han_node::api::BoardRow> {
    let ev = d
        .engine
        .read(&Query::Board {
            space: SPACE.into(),
            group: Group::Issue,
            collapsed: vec![],
            window: window(200, None, filter),
        })
        .unwrap();
    let ViewEvent::Snapshot { rows, .. } = ev else {
        panic!("a snapshot")
    };
    rows.into_iter()
        .map(|r| match r {
            Row::Board(b) => b,
            other => panic!("{other:?}"),
        })
        .collect()
}

/// Four sessions: alice and a second one of hers meet in src/a.rs; bob shares alice's issue; carol is apart.
fn filter_fixture() -> Driver {
    let mut d = Driver::new();
    for actor in [0, 1, 2, 0] {
        d.step(&Op::OpenSession { actor, task: None });
    }
    put_status(&mut d, 0, |b| {
        b.issue = Some(ISSUES[0].into());
        b.files = vec![touch("src/a.rs", &[(1, 5)])];
    });
    put_status(&mut d, 1, |b| {
        b.harness = "claude".into();
        b.repo = Some("github:o/other".into());
        b.issue = Some(ISSUES[0].into());
        b.files = vec![touch("docs/guide.md", &[])];
    });
    put_status(&mut d, 2, |b| {
        b.issue = Some(ISSUES[1].into());
        b.files = vec![touch("src/deep/x.rs", &[(100, 5)])];
    });
    put_status(&mut d, 3, |b| {
        b.files = vec![touch("src/a.rs", &[(2, 3)])];
    });
    d
}

fn which(d: &mut Driver, rows: Vec<han_node::api::BoardRow>) -> Vec<usize> {
    let mut v: Vec<usize> = rows
        .iter()
        .map(|r| d.sessions.iter().position(|s| *s == r.session).unwrap())
        .collect();
    v.sort_unstable();
    v
}

#[test]
fn every_filter_key_narrows_the_board() {
    let mut d = filter_fixture();
    let mut run = |f: Filter| {
        let rows = board(&mut d, f);
        which(&mut d, rows)
    };
    let all = vec![0, 1, 2, 3];
    assert_eq!(run(Filter::default()), all);
    // person and agent: the full id or its @local part, never a loose prefix.
    let f = |text: &str| Filter::parse(text).unwrap();
    assert_eq!(run(f("person:@bob")), vec![1]);
    assert_eq!(run(f("person:@bob:a.localhost")), vec![1]);
    assert_eq!(run(f("person:@bo")), Vec::<usize>::new());
    assert_eq!(run(f("person:@bob person:@carol")), vec![1, 2]);
    assert_eq!(run(f("agent:@carol")), vec![2]);
    assert_eq!(run(f("actor:@alice")), vec![0, 3]);
    // harness and repo: case-insensitive; a repository by its full name, `owner/name` or `name`.
    assert_eq!(run(f("harness:CLAUDE")), vec![1]);
    assert_eq!(run(f("repo:o/other")), vec![1]);
    assert_eq!(run(f("repo:github:o/r")), vec![0, 2, 3]);
    assert_eq!(run(f("repo:r")), vec![0, 2, 3]);
    // labels: the node has no source of them yet, so a label matches nothing.
    assert_eq!(run(f("label:bug")), Vec::<usize>::new());
    // path globs.
    assert_eq!(run(f("path:src/**")), vec![0, 2, 3]);
    assert_eq!(run(f("path:src/*.rs")), vec![0, 3]);
    assert_eq!(run(f("path:docs/")), vec![1]);
    assert_eq!(run(f("path:**/x.rs")), vec![2]);
    assert_eq!(run(f("path:*.rs")), Vec::<usize>::new());
    assert_eq!(run(f("path:docs/ path:**/x.rs")), vec![1, 2]);
    // overlap tiers: a shared issue is tier 1, a shared file tier 2.
    assert_eq!(run(f("overlap:1")), vec![0, 1, 3]);
    assert_eq!(run(f("overlapMin:2")), vec![0, 3]);
    assert_eq!(run(f("overlap:3")), Vec::<usize>::new());
    // mine: this device is alice.
    assert_eq!(run(f("mine:true")), vec![0, 3]);
    assert_eq!(run(f("is:mine harness:pi")), vec![0, 3]);
    // Keys add up: all of them must hold.
    assert_eq!(
        run(f("person:@alice path:src/*.rs repo:r overlap:2")),
        vec![0, 3]
    );
    assert_eq!(run(f("person:@alice person:@bob harness:claude")), vec![1]);
}

#[test]
fn the_text_form_is_strict_about_keys_and_quotes() {
    let f = Filter::parse(
        r#"person:@ada state:working state:idle "two words" auth overlapMin:2 closed:true"#,
    )
    .unwrap();
    assert_eq!(f.person, vec!["@ada".to_owned()]);
    assert_eq!(f.states, vec![SessionState::Working, SessionState::Idle]);
    assert_eq!(f.text.as_deref(), Some("two words auth"));
    assert_eq!(f.overlap_min, Some(2));
    assert!(f.include_closed);
    // A quoted value keeps its spaces; a quoted word keeps its colon.
    let f = Filter::parse(r#"path:"docs/my file.md" "a:b""#).unwrap();
    assert_eq!(f.path, vec!["docs/my file.md".to_owned()]);
    assert_eq!(f.text.as_deref(), Some("a:b"));
    // Errors: an unknown key, a missing value, a bad tier or state, a quote left open.
    for bad in [
        "colour:red",
        "person:",
        "overlap:9",
        "overlap:high",
        "state:sleepy",
        "mine:maybe",
        "is:happy",
        "path:\"open",
        "http://x",
    ] {
        let e = Filter::parse(bad).unwrap_err();
        assert_eq!(e.code(), "invalid", "{bad}: {e}");
    }
    assert!(
        Filter::parse("colour:red")
            .unwrap_err()
            .to_string()
            .contains("colour")
    );
    assert_eq!(Filter::parse("").unwrap(), Filter::default());
}

#[test]
fn an_unknown_member_or_a_bad_value_in_a_filter_is_an_error_not_ignored() {
    let json = serde_json::json!({"person": ["@ada"], "colour": "red"});
    let e = serde_json::from_value::<Filter>(json).unwrap_err();
    assert!(e.to_string().contains("colour"), "{e}");
    // A bad value passes serde and is refused when the view opens.
    let mut d = Driver::new();
    for bad in [
        Filter {
            overlap_min: Some(0),
            ..Filter::default()
        },
        Filter {
            path: vec!["*".repeat(9)],
            ..Filter::default()
        },
        Filter {
            harness: vec![String::new()],
            ..Filter::default()
        },
    ] {
        let err = d
            .engine
            .read(&Query::Board {
                space: SPACE.into(),
                group: Group::Issue,
                collapsed: vec![],
                window: window(10, None, bad),
            })
            .unwrap_err();
        assert_eq!(err.code(), "invalid");
    }
    // A view that cannot open says why and opens nothing.
    let (sink, mut stream) = ViewStream::channel();
    let bad = Filter {
        overlap_min: Some(7),
        ..Filter::default()
    };
    assert!(
        d.engine
            .subscribe(
                Query::Overlaps {
                    space: SPACE.into(),
                    window: window(10, None, bad),
                },
                sink
            )
            .is_none()
    );
    assert!(matches!(
        stream.try_next(),
        Some(ViewEvent::Failed {
            error: han_node::api::NodeError::Invalid { .. },
            ..
        })
    ));
}

#[test]
fn globs_follow_the_documented_rules() {
    use han_node::api::glob_match as g;
    assert!(g("src/*.rs", "src/a.rs"));
    assert!(!g("src/*.rs", "src/deep/a.rs"));
    assert!(g("src/**/a.rs", "src/a.rs"));
    assert!(g("src/**/a.rs", "src/x/y/a.rs"));
    assert!(g("**", "any/thing/at/all"));
    assert!(g("a?c", "abc"));
    assert!(!g("a?c", "a/c"));
    assert!(
        g("src/", "src/a/b.rs"),
        "a trailing slash covers what is under it"
    );
    assert!(!g("src/", "srcs/a.rs"));
    assert!(g("src/a.rs", "src/a.rs"));
    assert!(!g("src/a.rs", "SRC/a.rs"), "paths are case-sensitive");
    assert!(g("ünï/*", "ünï/ä"));
}

#[test]
fn failed_and_blocked_rows_say_why() {
    let mut d = Driver::new();
    for actor in [0, 1, 2, 0, 1] {
        d.step(&Op::OpenSession { actor, task: None });
    }
    put_status(&mut d, 0, |b| {
        b.state = SessionState::Failed;
        b.summary = Some("the build is broken".into());
    });
    put_status(&mut d, 1, |b| {
        b.state = SessionState::Blocked;
        b.step = "waiting for review".into();
    });
    put_status(&mut d, 2, |b| {
        b.state = SessionState::Blocked;
        b.step = "waiting for review".into();
        b.summary = Some("needs a decision on the schema".into());
    });
    put_status(&mut d, 3, |_| {});
    put_status(&mut d, 4, |b| b.state = SessionState::Idle);
    let reasons = |d: &mut Driver| -> Vec<Option<String>> {
        let rows = board(
            d,
            Filter {
                include_closed: true,
                ..Filter::default()
            },
        );
        let mut by: Vec<(usize, Option<String>)> = rows
            .into_iter()
            .map(|r| {
                (
                    d.sessions.iter().position(|s| *s == r.session).unwrap(),
                    r.state_reason,
                )
            })
            .collect();
        by.sort();
        by.into_iter().map(|(_, r)| r).collect()
    };
    assert_eq!(
        reasons(&mut d),
        vec![
            Some("the build is broken".into()),
            Some("waiting for review".into()),
            Some("needs a decision on the schema".into()),
            None,
            None
        ]
    );
    // Silence: stale has no reason; failed by silence names the threshold, and stays the same text as time
    // passes, so the row does not change with every tick.
    d.step(&Op::Tick(1_000));
    let stale = reasons(&mut d);
    assert_eq!(stale[3], None, "stale needs no reason");
    d.step(&Op::Tick(2_000));
    let failed = reasons(&mut d);
    assert_eq!(failed[3].as_deref(), Some("no status for 3 seconds"));
    assert_eq!(failed[4].as_deref(), Some("no status for 3 seconds"));
    d.step(&Op::Tick(500));
    assert_eq!(reasons(&mut d), failed);
    // The session row says the same.
    let ev = d
        .engine
        .read(&Query::Session {
            id: d.sessions[3].clone(),
            window: Window::default(),
        })
        .unwrap();
    let ViewEvent::Snapshot { rows, .. } = ev else {
        panic!("a snapshot")
    };
    assert!(matches!(&rows[0], Row::Session(s)
        if s.state_reason.as_deref() == Some("no status for 3 seconds")));
}

#[test]
fn an_overlap_row_carries_its_files_ranges_and_claim() {
    let mut d = filter_fixture();
    let ev = d
        .engine
        .read(&Query::Overlaps {
            space: SPACE.into(),
            window: window(200, None, Filter::default()),
        })
        .unwrap();
    let ViewEvent::Snapshot { rows, .. } = ev else {
        panic!("a snapshot")
    };
    let rows: Vec<_> = rows
        .into_iter()
        .map(|r| match r {
            Row::Overlap(o) => o,
            other => panic!("{other:?}"),
        })
        .collect();
    // The file overlap: both sides' ranges, in the order of the parties.
    let hunk = rows
        .iter()
        .find(|o| o.kind == han_node::api::OverlapKind::HunkRange)
        .expect("a hunk overlap");
    assert_eq!(hunk.files.len(), 1);
    let file = &hunk.files[0];
    assert_eq!(file.path, "src/a.rs");
    let first_is_s0 = hunk.parties[0].session.as_deref() == Some(d.sessions[0].as_str());
    let (a, b) = if first_is_s0 {
        (vec![(1, 5)], vec![(2, 3)])
    } else {
        (vec![(2, 3)], vec![(1, 5)])
    };
    assert_eq!((&file.ranges_a, &file.ranges_b), (&a, &b));
    assert!(hunk.claim.is_none());
    assert!(hunk.conflict.is_none(), "the node simulates no merges yet");
    // The issue overlap: a claim, no files.
    let claim = rows
        .iter()
        .find(|o| o.kind == han_node::api::OverlapKind::IssueClaim)
        .expect("an issue overlap");
    let c = claim.claim.as_ref().unwrap();
    assert_eq!(c.kind, han_node::api::ClaimKind::Issue);
    assert_eq!(c.reference, ISSUES[0]);
    assert_eq!(c.expires_at, None);
    assert!(claim.files.is_empty());
    // The wire name of the reference is `ref`.
    let json = serde_json::to_value(&claim.claim).unwrap();
    assert_eq!(json["ref"], ISSUES[0]);
}

#[test]
fn leases_overlap_with_a_claim_that_ends() {
    let mut d = Driver::new();
    d.step(&Op::Lease {
        actor: 0,
        path: 2, // docs/
        ttl: 1000,
    });
    d.step(&Op::Lease {
        actor: 1,
        path: 2,
        ttl: 500,
    });
    d.step(&Op::Tick(1));
    let ev = d
        .engine
        .read(&Query::Overlaps {
            space: SPACE.into(),
            window: window(200, None, Filter::default()),
        })
        .unwrap();
    let ViewEvent::Snapshot { rows, .. } = ev else {
        panic!("a snapshot")
    };
    let Row::Overlap(o) = &rows[0] else {
        panic!("an overlap")
    };
    let c = o.claim.as_ref().expect("a claim");
    assert_eq!(c.kind, han_node::api::ClaimKind::Lease);
    assert_eq!(c.reference, "docs/");
    assert_eq!(
        c.expires_at.as_deref(),
        Some("2026-01-01T00:08:20Z"),
        "the earlier lease ends the overlap"
    );
}

#[test]
fn board_rows_summarise_who_they_overlap_with() {
    let mut d = filter_fixture();
    let rows = board(&mut d, Filter::default());
    let row = |d: &Driver, i: usize| {
        rows.iter()
            .find(|r| r.session == d.sessions[i])
            .unwrap()
            .clone()
    };
    let s0 = row(&d, 0);
    assert_eq!(s0.overlap_with.len(), 2, "{:?}", s0.overlap_with);
    let hunk = &s0.overlap_with[0];
    assert_eq!(hunk.session.as_deref(), Some(d.sessions[3].as_str()));
    assert_eq!(
        (hunk.tier, hunk.kind),
        (2, han_node::api::OverlapKind::HunkRange)
    );
    assert_eq!(hunk.files, vec!["src/a.rs".to_owned()]);
    assert_eq!(hunk.agent, ACTORS[0]);
    let claim = &s0.overlap_with[1];
    assert_eq!(claim.agent, ACTORS[1]);
    assert_eq!(claim.person, ACTORS[1]);
    assert_eq!(claim.issue.as_deref(), Some(ISSUES[0]));
    assert_eq!(claim.tier, 1);
    assert!(claim.files.is_empty());
    // Bob is only on the issue; carol is on nothing.
    assert_eq!(row(&d, 1).overlap_with.len(), 1);
    assert_eq!(row(&d, 1).overlap_with[0].agent, ACTORS[0]);
    assert!(row(&d, 2).overlap_with.is_empty());
    // The kinds the badge shows agree with the detail.
    assert_eq!(
        s0.overlaps,
        vec![
            han_node::api::OverlapKind::IssueClaim,
            han_node::api::OverlapKind::HunkRange
        ]
    );
}

//! A fake team shared by the render and key-flow tests.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use han_node::api::{
    ApprovalRow, BoardRow, Group, OverlapKind, Query, Row, SessionState, SpaceLink, SpaceStatusRow,
    Tests,
};
use han_tui::testing::{
    FakeApi, Harness, approval, board_row, node_status, page_of, session_row, snapshot,
};

pub const ME: &str = "@lars";
/// The node's steady clock, in ms, as the freshest row saw it.
const NOW: u64 = 100_000_000;

/// What the fake node serves; tests change it and push diffs.
#[derive(Default)]
pub struct Data {
    pub board: Vec<BoardRow>,
    pub approvals: Vec<ApprovalRow>,
    pub timeline: bool,
}

pub type Shared = Arc<Mutex<Data>>;

/// Seven sessions in four groups: a hunk overlap on #41, an approval waiting on me on #52, a stale and a
/// failed row, and a finished one.
pub fn team() -> Vec<BoardRow> {
    let mut lars = board_row(
        "s1",
        "lars-coder",
        "@lars",
        "#41 Board freshness",
        "running tests",
    );
    lars.issue = Some("#41".into());
    lars.tool = Some("cargo test".into());
    lars.tests = Some(Tests { pass: 12, fail: 0 });
    lars.overlaps = vec![OverlapKind::HunkRange];
    lars.last_active_ms = NOW;
    lars.files = (0..4)
        .map(|i| han_node::api::FileTouch {
            path: format!("src/f{i}.rs"),
            hunks: vec![(1, 5)],
        })
        .collect();

    let mut ada = board_row(
        "s2",
        "ada-bot",
        "@ada",
        "#41 Board freshness",
        "editing src/board.rs",
    );
    ada.issue = Some("#41".into());
    ada.overlaps = vec![OverlapKind::HunkRange, OverlapKind::IssueClaim];
    ada.last_active_ms = NOW - 20_000;
    ada.can_steer = false;
    ada.sandbox = Some(han_node::api::Sandbox {
        backend: han_node::api::SandboxBackend::None,
        fidelity: han_node::api::Fidelity::Advisory,
    });

    let mut rev = board_row(
        "s3",
        "lars-reviewer",
        "@lars",
        "#52 Login flow",
        "push han/52-login",
    );
    rev.issue = Some("#52".into());
    rev.state = SessionState::WaitingApproval;
    rev.needs_me = true;
    rev.tests = Some(Tests { pass: 30, fail: 2 });
    rev.last_active_ms = NOW - 90_000;

    let mut bo = board_row("s4", "bo-runner", "@bo", "#52 Login flow", "waiting for CI");
    bo.issue = Some("#52".into());
    bo.state = SessionState::Stale;
    bo.last_active_ms = NOW - 14 * 60_000;
    bo.can_steer = false;

    let mut docs = board_row("s5", "ada-docs", "@ada", "#60 Docs", "wrote the guide");
    docs.issue = Some("#60".into());
    docs.state = SessionState::Done;
    docs.last_active_ms = NOW - 3 * 3_600_000;
    docs.can_steer = false;

    let mut scratch = board_row("s6", "bo-scratch", "@bo", "", "host unreachable");
    scratch.issue = None;
    scratch.state = SessionState::Failed;
    scratch.last_active_ms = NOW - 40 * 60_000;
    scratch.can_steer = false;
    scratch.tokens = None;
    scratch.files = vec![];

    let mut idle = board_row("s7", "lars-notes", "@lars", "#60 Docs", "nothing to do");
    idle.issue = Some("#60".into());
    idle.state = SessionState::Idle;
    idle.last_active_ms = NOW - 120_000;
    idle.tokens = None;
    vec![lars, ada, rev, bo, docs, idle, scratch]
}

/// Rows with distinct keys in an order that matches a group mode (the node does the ordering; the fake
/// keeps the order it is given, so keys are made to sort the same way).
pub fn keyed(mut rows: Vec<BoardRow>) -> Vec<BoardRow> {
    for (i, r) in rows.iter_mut().enumerate() {
        r.key = format!("{i:05}");
    }
    rows
}

/// The fake node over `data`: it answers every view the TUI opens.
pub fn node(data: &Shared) -> Arc<FakeApi> {
    let api = FakeApi::new();
    let d = Arc::clone(data);
    api.serve(move |q| {
        let d = d.lock().unwrap();
        Some(match q {
            Query::NodeStatus { .. } => snapshot(vec![node_status(ME)], 1, 0, None),
            Query::SpaceStatus { space, .. } => snapshot(
                vec![Row::SpaceStatus(SpaceStatusRow {
                    key: "s".into(),
                    space: space.clone(),
                    home: "h".into(),
                    link: SpaceLink::Live,
                    head: 42,
                    sessions: d.board.len() as u32,
                    members: 3,
                    outbox_pending: 0,
                    signals: 0,
                    error: None,
                })],
                1,
                0,
                None,
            ),
            Query::Board {
                window,
                group,
                collapsed,
                ..
            } => {
                let rows: Vec<Row> = reorder(&d.board, *group)
                    .into_iter()
                    .filter(|b| !collapsed.contains(&b.group))
                    .map(Row::Board)
                    .collect();
                page_of(&rows, window)
            }
            Query::Approvals { .. } => {
                let rows: Vec<Row> = d.approvals.iter().cloned().map(Row::Approval).collect();
                let n = rows.len() as u32;
                snapshot(rows, n, 0, None)
            }
            Query::Session { id, .. } => {
                let b = d.board.iter().find(|b| &b.session == id)?;
                let mut row = session_row(id, &b.agent, b.state, &b.step);
                let st = row.status.as_mut().unwrap();
                st.tool = b.tool.clone();
                st.files = b.files.clone();
                st.tests = b.tests;
                st.tokens = b.tokens;
                st.plan = vec![
                    han_node::api::PlanItem {
                        text: "read the code".into(),
                        done: true,
                    },
                    han_node::api::PlanItem {
                        text: "fix the freshness bug".into(),
                        done: false,
                    },
                ];
                snapshot(vec![Row::Session(row)], 1, 0, None)
            }
            Query::Inputs { .. } => snapshot(vec![], 0, 0, None),
            Query::Timeline { .. } | Query::Patches { .. } if !d.timeline => return None,
            Query::Timeline { session, .. } => {
                let point = |n: u32, label: &str| {
                    Row::TimelinePoint(han_node::api::TimelinePointRow {
                        key: format!("pt{n}"),
                        session: session.clone(),
                        point: format!("pt{n}"),
                        label: label.into(),
                        at: "2026-10-13T10:00:00Z".into(),
                        files: n,
                    })
                };
                snapshot(
                    vec![point(1, "first edit"), point(2, "tests added")],
                    2,
                    0,
                    None,
                )
            }
            Query::Patches { .. } => snapshot(
                vec![Row::FilePatch(han_node::api::FilePatchRow {
                    key: "src/lib.rs".into(),
                    path: "src/lib.rs".into(),
                    status: han_node::api::PatchStatus::Modified,
                    patch: "@@ -1,2 +1,3 @@
 fn a() {}
-let old = 1;
+let fresh = 2;
"
                    .into(),
                    added: 1,
                    removed: 1,
                })],
                1,
                0,
                None,
            ),
            _ => snapshot(vec![], 0, 0, None),
        })
    });
    api
}

/// The board in the order of a grouping: groups together, the fixture's own order inside them. Keys are
/// renumbered so the fake's paging sees the order.
pub fn reorder(rows: &[BoardRow], group: Group) -> Vec<BoardRow> {
    let mut rows: Vec<BoardRow> = rows.to_vec();
    rows.iter_mut().for_each(|r| {
        r.group = match group {
            Group::Issue => r.group.clone(),
            Group::Person => r.person.clone(),
            Group::State => r.state.name().to_owned(),
        };
    });
    let rank = |r: &BoardRow| match group {
        Group::State => [
            "waiting-approval",
            "blocked",
            "failed",
            "stale",
            "working",
            "idle",
            "done",
        ]
        .iter()
        .position(|s| *s == r.group)
        .unwrap_or(99),
        _ => usize::from(r.group.is_empty()),
    };
    rows.sort_by(|a, b| (rank(a), &a.group).cmp(&(rank(b), &b.group)));
    keyed(rows)
}

pub fn snapshot_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
        .join(format!("{name}.txt"))
}

/// Compares a screen with its golden file; `HAN_TUI_BLESS=1` writes the file instead.
pub fn assert_screen(name: &str, screen: &str) {
    let path = snapshot_path(name);
    let screen = format!("{}\n", screen.trim_end());
    if std::env::var_os("HAN_TUI_BLESS").is_some() {
        std::fs::write(&path, &screen).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| {
            panic!(
                "{}: {e} (run with HAN_TUI_BLESS=1 to create it)",
                path.display()
            )
        })
        .replace("\r\n", "\n");
    assert_eq!(
        screen, want,
        "the screen differs from {name}.txt; run with HAN_TUI_BLESS=1 to accept it"
    );
}

/// A harness on a fake team, started and settled.
pub async fn started(size: (u16, u16)) -> (Harness, Arc<FakeApi>, Shared) {
    let data: Shared = Arc::new(Mutex::new(Data {
        board: team(),
        ..Data::default()
    }));
    let api = node(&data);
    let mut h = Harness::new(api.clone(), Some("team"), size);
    h.settle().await;
    (h, api, data)
}

pub fn pending_approval(data: &Shared) -> ApprovalRow {
    let a = approval("p1", "lars-reviewer", true);
    data.lock().unwrap().approvals = vec![a.clone()];
    a
}

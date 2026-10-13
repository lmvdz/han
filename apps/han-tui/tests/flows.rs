//! Key-flow tests: what pressing keys does, seen from the node's side (the queries it is asked and the commands
//! it gets) and from the screen.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Data, ME, Shared, assert_screen, node, pending_approval, started, team};
use han_node::api::{
    BoardRow, CallResult, Command, DiffOp, Group, InputRow, NodeError, Query, Row, SessionState,
    ViewEvent,
};
use han_tui::config::Config;
use han_tui::testing::{FakeApi, FakeConnector, Harness, approval};

fn is_board(q: &Query) -> bool {
    matches!(q, Query::Board { .. })
}

fn boards(api: &FakeApi) -> Vec<Query> {
    api.opened().into_iter().filter(is_board).collect()
}

fn last_board(api: &FakeApi) -> (Group, Vec<String>, han_node::api::Window) {
    match boards(api).pop() {
        Some(Query::Board {
            group,
            collapsed,
            window,
            ..
        }) => (group, collapsed, window),
        other => panic!("no board query: {other:?}"),
    }
}

fn diff(ops: Vec<DiffOp>, total: u32) -> ViewEvent {
    ViewEvent::Diff {
        ops,
        total,
        offset: 0,
        next_cursor: None,
    }
}

/// The row of a session as the app holds it, changed.
fn changed(h: &Harness, session: &str, f: impl FnOnce(&mut BoardRow)) -> Row {
    let mut row =
        h.rt.app
            .board
            .rows()
            .iter()
            .find(|r| r.session == session)
            .cloned()
            .expect("the session is on the board");
    f(&mut row);
    Row::Board(row)
}

// ---- approving: two deliberate steps -------------------------------------------------------------------

#[tokio::test]
async fn approving_takes_two_deliberate_steps_and_no_single_key_signs() {
    let (mut h, api, data) = started((130, 26)).await;
    pending_approval(&data);
    h.keys("r").await;

    // Step 0: the pane. `a` on the board opens it on the effect; nothing is sent.
    h.keys("a").await;
    assert!(api.calls().is_empty());
    // Step 1: open the full card. Neither Enter nor `a` signs, however often it is pressed.
    h.keys("<enter>").await;
    assert!(h.screen().contains("WHY GATED"));
    h.keys("<enter><enter><enter>").await;
    assert!(api.calls().is_empty(), "Enter on the card never signs");
    // Step 2: `a` opens the sign panel, where Enter signs. Escape goes back without signing.
    h.keys("a").await;
    assert!(h.screen().contains("You are signing an approval of:"));
    h.keys("<esc>").await;
    assert!(api.calls().is_empty());
    assert!(h.screen().contains("WHY GATED"));
    h.keys("a<enter>").await;

    assert_eq!(
        api.calls(),
        vec![Command::EffectApprove {
            space: "team".into(),
            proposal: "p1".into(),
            note: None,
        }],
        "exactly one approval, naming the proposal"
    );
    // The receipt closes the card and says so.
    h.settle().await;
    let s = h.screen();
    assert!(!s.contains("You are signing"), "{s}");
    assert!(s.contains("Approved"), "{s}");
}

#[tokio::test]
async fn a_key_pressed_twice_quickly_does_not_sign_twice() {
    let (mut h, api, data) = started((130, 26)).await;
    pending_approval(&data);
    h.keys("r").await;
    h.keys("a<enter>a").await;
    // Both Enters arrive before the node has answered.
    h.rt.handle(han_tui::msg::Msg::Key(han_tui::testing::key_named("enter")));
    h.rt.handle(han_tui::msg::Msg::Key(han_tui::testing::key_named("enter")));
    h.settle().await;
    assert_eq!(api.calls().len(), 1);
}

#[tokio::test]
async fn a_refused_approval_says_why_and_is_not_retried() {
    let (mut h, api, data) = started((130, 26)).await;
    api.answer(|_| Err(NodeError::denied("role changed")));
    pending_approval(&data);
    h.keys("r").await;
    h.keys("a<enter>a<enter>").await;
    let s = h.screen();
    assert!(
        s.contains("The space refused this approval: role changed."),
        "{s}"
    );
    // A refusal that retrying cannot fix: Enter does not send it again.
    h.keys("<enter><enter>").await;
    assert_eq!(api.calls().len(), 1);
}

#[tokio::test]
async fn an_unavailable_node_can_be_retried_and_a_queued_approval_says_so() {
    let (mut h, api, data) = started((130, 26)).await;
    api.answer(|_| Err(NodeError::unavailable("no route to the home")));
    pending_approval(&data);
    h.keys("r").await;
    h.keys("a<enter>a<enter>").await;
    assert!(
        h.screen()
            .contains("Couldn't sign right now (no route to the home).")
    );
    api.answer(|_| {
        Ok(CallResult::Queued {
            outbox_id: 1,
            space: "team".into(),
            id: "e".into(),
        })
    });
    h.keys("<enter>").await;
    assert_eq!(api.calls().len(), 2);
    assert!(h.screen().contains("signed and queued"));
}

#[tokio::test]
async fn a_card_closes_when_its_effect_leaves_and_refuses_a_changed_digest() {
    let (mut h, api, data) = started((130, 26)).await;
    let a = pending_approval(&data);
    h.keys("r").await;
    h.keys("a<enter>").await;

    // The digest changes under the open card: signing is off until it is reviewed again.
    let mut changed_digest = a.clone();
    changed_digest.plan_digest = "sha256:ffff".into();
    api.push(
        |q| matches!(q, Query::Approvals { .. }),
        diff(
            vec![DiffOp::Update {
                row: Row::Approval(changed_digest),
            }],
            1,
        ),
    );
    h.settle().await;
    h.keys("a").await;
    let s = h.screen();
    assert!(s.contains("This proposal changed. Review it again."), "{s}");
    assert!(!s.contains("You are signing"), "{s}");

    // The effect is claimed or cancelled by someone else: the card closes with a line saying so.
    api.push(
        |q| matches!(q, Query::Approvals { .. }),
        diff(vec![DiffOp::Remove { key: a.key.clone() }], 0),
    );
    h.settle().await;
    let s = h.screen();
    assert!(!s.contains("WHY GATED"), "{s}");
    assert!(s.contains("no longer waiting"), "{s}");
    assert!(api.calls().is_empty());
}

#[tokio::test]
async fn cancelling_asks_first_and_starts_on_keep() {
    let (mut h, api, data) = started((130, 26)).await;
    pending_approval(&data);
    h.keys("r").await;
    h.keys("a<enter>c").await;
    let s = h.screen();
    assert!(s.contains("It will not run."), "{s}");
    // Enter on the initial choice keeps the effect.
    h.keys("<enter>").await;
    assert!(api.calls().is_empty());
    // Choosing "Cancel effect" and confirming sends one EffectCancel.
    h.keys("a<enter>c<right><enter>").await;
    assert_eq!(
        api.calls(),
        vec![Command::EffectCancel {
            space: "team".into(),
            proposal: "p1".into()
        }]
    );
}

#[tokio::test]
async fn an_effect_that_arrives_while_the_pane_is_closed_is_announced() {
    let (mut h, api, _data) = started((130, 26)).await;
    assert_eq!(h.rt.app.approvals.count(), 0);
    api.push(
        |q| matches!(q, Query::Approvals { .. }),
        diff(
            vec![DiffOp::Insert {
                index: 0,
                row: Row::Approval(approval("p9", "ada-bot", true)),
            }],
            1,
        ),
    );
    h.settle().await;
    let s = h.screen();
    assert!(s.contains("1 effect waits for your approval"), "{s}");
    assert!(s.contains("⚑ 1 awaiting you"), "{s}");
}

// ---- the board: grouping, filter, collapse --------------------------------------------------------------

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("han-tui-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[tokio::test]
async fn t_cycles_the_grouping_and_the_choice_is_remembered() {
    let data: Shared = Arc::new(std::sync::Mutex::new(Data {
        board: team(),
        ..Data::default()
    }));
    let api = node(&data);
    let dir = temp_dir("group");
    let path = dir.join("tui.json");
    let mut h = Harness::build(
        Some(api.clone()),
        None,
        Some("team"),
        (130, 26),
        Config::default(),
        Some(path.clone()),
    );
    h.settle().await;
    assert_eq!(last_board(&api).0, Group::Issue);

    h.keys("t").await;
    assert_eq!(last_board(&api).0, Group::Person);
    assert!(h.screen().contains("by person"));
    h.keys("t").await;
    assert_eq!(last_board(&api).0, Group::State);
    h.keys("t").await;
    assert_eq!(last_board(&api).0, Group::Issue);
    h.keys("t").await;
    assert_eq!(Config::load(&path).group_of("team"), Group::Person);

    // A new run starts with what was chosen, without anyone asking.
    let api2 = node(&data);
    let mut h2 = Harness::build(
        Some(api2.clone()),
        None,
        Some("team"),
        (130, 26),
        Config::load(&path),
        Some(path.clone()),
    );
    h2.settle().await;
    assert_eq!(last_board(&api2).0, Group::Person);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn the_filter_box_is_parsed_and_sent_to_the_node() {
    let (mut h, api, _data) = started((130, 26)).await;
    h.keys("/person:@ada state:working auth<enter>").await;
    let (_, _, window) = last_board(&api);
    assert_eq!(window.filter.actor.as_deref(), Some("@ada"));
    assert_eq!(window.filter.states, vec![SessionState::Working]);
    assert_eq!(window.filter.text.as_deref(), Some("auth"));
    assert!(
        h.screen()
            .contains("filter: person:@ada state:working auth")
    );

    // An unknown key is marked, and the rest of the filter still applies.
    let before = boards(&api).len();
    h.keys("/<c-u>colour:red state:idle<enter>").await;
    let s = h.screen();
    assert!(s.contains("! Unknown filter key 'colour'"), "{s}");
    let (_, _, window) = last_board(&api);
    assert_eq!(window.filter.states, vec![SessionState::Idle]);
    assert!(boards(&api).len() > before);

    // x clears it; so does Esc.
    h.keys("x").await;
    assert_eq!(last_board(&api).2.filter, han_node::api::Filter::default());
    h.keys("c").await;
    assert!(last_board(&api).2.filter.include_closed);
    h.keys("<esc>").await;
    assert!(!last_board(&api).2.filter.include_closed);
}

#[tokio::test]
async fn collapsing_a_group_asks_the_node_to_leave_its_rows_out() {
    let (mut h, api, data) = started((130, 26)).await;
    // On the first header: Enter collapses.
    h.keys("<enter>").await;
    let (_, collapsed, _) = last_board(&api);
    assert_eq!(collapsed, vec!["#41 Board freshness".to_owned()]);
    let s = h.screen();
    assert!(s.contains("▸ #41 Board freshness (2)"), "{s}");
    assert!(!s.contains("lars-coder"), "{s}");
    assert!(s.contains("▾ #52 Login flow"), "{s}");
    // Enter again opens it; the rows are back.
    h.keys("<enter>").await;
    assert!(last_board(&api).1.is_empty());
    assert!(h.screen().contains("lars-coder"));
    let _ = data;
}

#[tokio::test]
async fn live_diffs_update_rows_and_focus_stays_on_its_session() {
    let (mut h, api, _data) = started((130, 26)).await;
    h.keys("jj").await; // ada-bot (s2)
    let row = changed(&h, "s2", |r| {
        r.state = SessionState::Failed;
        r.step = "turn failed".into();
    });
    api.push(is_board, diff(vec![DiffOp::Update { row }], 7));
    h.settle().await;
    let s = h.screen();
    assert!(s.contains("failed: turn failed"), "{s}");
    assert_eq!(
        h.rt.app.board.selected_row().map(|r| r.session.as_str()),
        Some("s2")
    );

    // The focused session leaves the board: focus moves to the entry now at its place.
    api.push(
        is_board,
        diff(
            vec![DiffOp::Remove {
                key: "00001".into(),
            }],
            6,
        ),
    );
    h.settle().await;
    assert!(h.rt.app.board.selected_row().is_some() || h.rt.app.board.selected().is_some());
    assert!(!h.screen().contains("ada-bot"));
}

#[tokio::test]
async fn windowed_scrolling_follows_total_and_offset() {
    let data: Shared = Arc::new(std::sync::Mutex::new(Data::default()));
    {
        let mut d = data.lock().unwrap();
        d.board = (0..300)
            .map(|i| {
                let mut r = han_tui::testing::board_row(
                    &format!("s{i}"),
                    &format!("agent{i:03}"),
                    "@lars",
                    &format!("#{:02} Issue", i / 10),
                    "working",
                );
                r.last_active_ms = 1_000;
                r
            })
            .collect();
    }
    let api = node(&data);
    let mut h = Harness::new(api.clone(), Some("team"), (100, 24));
    h.settle().await;
    let first = last_board(&api).2;
    assert_eq!(first.limit, 72, "three screens of rows");
    assert!(first.cursor.is_none());
    let s = h.screen();
    assert!(s.contains("1-72 of 300 ▼"), "{s}");

    // Walking down past the edge of the window moves it, and the selection stays on its row.
    for _ in 0..4 {
        h.keys("<pgdn><pgdn><pgdn>").await;
    }
    assert!(
        boards(&api)
            .iter()
            .any(|q| matches!(q, Query::Board { window, .. } if window.cursor.is_some())),
        "the window moved"
    );
    let at =
        h.rt.app
            .board
            .selected_row()
            .expect("a row is selected")
            .session
            .clone();
    let n: usize = at[1..].parse().unwrap();
    assert!(n > 60, "{at}");
    let s = h.screen();
    assert!(s.contains(&format!("agent{n:03}")), "{s}");

    // G goes to the end, whatever it takes; gg comes back.
    h.keys("G").await;
    let s = h.screen();
    assert!(s.contains("of 300"), "{s}");
    assert!(s.contains("agent299"), "{s}");
    assert_eq!(
        h.rt.app.board.selected_row().map(|r| r.session.as_str()),
        Some("s299")
    );
    h.keys("gg").await;
    let s = h.screen();
    assert!(s.contains("1-72 of 300"), "{s}");
    assert!(last_board(&api).2.cursor.is_none());
}

// ---- the session pane: steering and rights --------------------------------------------------------------

#[tokio::test]
async fn steering_sends_a_session_input_and_shows_its_delivery() {
    let (mut h, api, _data) = started((130, 26)).await;
    api.answer(|_| {
        Ok(CallResult::Receipt {
            space: "team".into(),
            seq: 9,
            id: "e9".into(),
            time: "t".into(),
        })
    });
    h.keys("j<enter>").await; // lars-coder: mine, may steer
    h.keys("iuse the cache<enter>").await;
    assert_eq!(
        api.calls(),
        vec![Command::SessionInput {
            space: "team".into(),
            session: "s1".into(),
            text: "use the cache".into()
        }]
    );
    let s = h.screen();
    assert!(s.contains("sent · seq 9"), "{s}");

    // The input comes back through the Inputs view as a line of the stream.
    api.push(
        |q| matches!(q, Query::Inputs { .. }),
        diff(
            vec![DiffOp::Insert {
                index: 0,
                row: Row::Input(InputRow {
                    key: "i1".into(),
                    space: "team".into(),
                    session: "s1".into(),
                    id: "e9".into(),
                    seq: 9,
                    author: ME.into(),
                    text: Some("use the cache".into()),
                }),
            }],
            1,
        ),
    );
    h.settle().await;
    assert!(h.screen().contains("» @lars -> session: use the cache"));
}

#[tokio::test]
async fn steering_is_labelled_by_whose_agent_it_is_and_cancels_with_escape() {
    let (mut h, api, _data) = started((130, 26)).await;
    h.keys("j<enter>i").await;
    let s = h.screen();
    assert!(s.contains("steer (instruction)>"), "{s}");
    h.keys("never mind<esc>").await;
    assert!(api.calls().is_empty());
    assert!(!h.screen().contains("steer (instruction)>"));
}

#[tokio::test]
async fn without_the_steer_right_the_pane_is_watch_only_and_sends_nothing() {
    let (mut h, api, _data) = started((130, 26)).await;
    h.keys("jj<enter>").await; // ada-bot: can_steer is false
    h.keys("ihello<enter>").await;
    let s = h.screen();
    assert!(s.contains("watch only"), "{s}");
    assert!(!s.contains("steer ("), "{s}");
    assert!(api.calls().is_empty());
}

#[tokio::test]
async fn a_refused_steering_input_says_so() {
    let (mut h, api, _data) = started((130, 26)).await;
    api.answer(|_| Err(NodeError::denied("not your agent")));
    h.keys("j<enter>ihello<enter>").await;
    let s = h.screen();
    assert!(s.contains("not sent: denied: not your agent"), "{s}");
}

#[tokio::test]
async fn escape_closes_the_session_and_its_views() {
    let (mut h, api, _data) = started((130, 26)).await;
    h.keys("j<enter>").await;
    assert!(
        api.opened()
            .iter()
            .any(|q| matches!(q, Query::Session { id, .. } if id == "s1"))
    );
    assert!(
        api.opened()
            .iter()
            .any(|q| matches!(q, Query::Inputs { .. }))
    );
    h.keys("<esc>").await;
    assert!(h.rt.app.session.is_none());
    assert!(!h.screen().contains("Session ·"));
}

#[tokio::test]
async fn the_timeline_and_changes_fill_in_once_the_node_serves_them() {
    let (mut h, api, data) = started((130, 26)).await;
    data.lock().unwrap().timeline = true;
    h.keys("j<enter>2").await;
    let s = h.screen();
    assert!(s.contains("tests added"), "{s}");
    assert!(!s.contains("pending M4"), "{s}");
    h.keys("3").await;
    let s = h.screen();
    assert!(s.contains("src/lib.rs +1 -1"), "{s}");
    assert!(s.contains("+let fresh = 2;"), "{s}");
    // The patches are asked for the latest point.
    assert!(
        api.opened()
            .iter()
            .any(|q| matches!(q, Query::Patches { to, .. } if to == "pt2"))
    );
    assert_screen("session_changes", &s);
}

#[tokio::test]
async fn the_session_stream_logs_steps_and_tool_calls_as_they_arrive() {
    let (mut h, api, _data) = started((130, 26)).await;
    h.keys("j<enter>").await;
    let mut row =
        han_tui::testing::session_row("s1", "lars-coder", SessionState::Working, "writing the fix");
    row.status.as_mut().unwrap().tool = Some("edit src/board.rs".into());
    api.push(
        |q| matches!(q, Query::Session { .. }),
        diff(
            vec![DiffOp::Update {
                row: Row::Session(row),
            }],
            1,
        ),
    );
    h.settle().await;
    let s = h.screen();
    assert!(s.contains("▸ writing the fix"), "{s}");
    assert!(s.contains("$ edit src/board.rs"), "{s}");
    // Scrolling up pauses the stream; G follows it again.
    h.keys("k").await;
    h.keys("G").await;
    assert!(h.rt.app.session.as_ref().unwrap().follow);
}

// ---- the shell ------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_palette_runs_commands() {
    let data: Shared = Arc::new(std::sync::Mutex::new(Data {
        board: team(),
        ..Data::default()
    }));
    let api = node(&data);
    let dir = temp_dir("palette");
    let path = dir.join("tui.json");
    let mut h = Harness::build(
        Some(api.clone()),
        None,
        Some("team"),
        (130, 26),
        Config::default(),
        Some(path.clone()),
    );
    h.settle().await;
    h.keys(":group state<enter>").await;
    assert_eq!(last_board(&api).0, Group::State);
    h.keys(":filter state:idle<enter>").await;
    assert_eq!(last_board(&api).2.filter.states, vec![SessionState::Idle]);
    h.keys(":clear<enter>").await;
    h.keys(":space other<enter>").await;
    assert!(
        api.opened()
            .iter()
            .any(|q| matches!(q, Query::Board { space, .. } if space == "other"))
    );
    assert_eq!(Config::load(&path).last_space.as_deref(), Some("other"));
    h.keys(":bogus<enter>").await;
    assert!(h.screen().contains("unknown command 'bogus'"));
    h.keys(":q<enter>").await;
    assert!(h.rt.app.quit);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn the_palette_escapes_and_edits() {
    let (mut h, _api, _data) = started((130, 26)).await;
    h.keys(":gro").await;
    assert!(h.screen().lines().last().unwrap().starts_with(":gro"));
    h.keys("<esc>").await;
    assert!(!h.screen().lines().last().unwrap().starts_with(':'));
    // Backspace on an empty palette leaves it.
    h.keys(":<bs>").await;
    assert!(h.rt.app.board.selected().is_some());
    h.keys("j").await; // keys are commands again
}

#[tokio::test]
async fn no_space_is_said_plainly() {
    let api = FakeApi::new();
    let mut h = Harness::new(api, None, (100, 14));
    h.settle().await;
    let s = h.screen();
    assert!(s.contains("No space selected."), "{s}");
    assert!(s.contains(":space <space id>"), "{s}");
}

#[tokio::test]
async fn resizing_redraws_and_asks_for_more_rows_when_the_screen_grows() {
    let (mut h, api, _data) = started((130, 26)).await;
    assert_eq!(last_board(&api).2.limit, 78);
    h.resize(60, 14);
    let s = h.screen();
    assert!(s.contains("lars-coder"), "{s}");
    assert_eq!(s.lines().count(), 14);
    // Shrinking does not churn the subscription; growing asks for a bigger window.
    let n = boards(&api).len();
    h.settle().await;
    assert_eq!(boards(&api).len(), n);
    h.resize(200, 60);
    h.settle().await;
    assert_eq!(last_board(&api).2.limit, 180);
    // Absurdly small terminals do not crash.
    h.resize(12, 3);
    assert!(h.screen().contains("too small"));
}

// ---- the connection ------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn when_the_daemon_restarts_the_tui_reconnects_and_reopens_its_views() {
    let data: Shared = Arc::new(std::sync::Mutex::new(Data {
        board: team(),
        ..Data::default()
    }));
    let d = Arc::clone(&data);
    let connector = FakeConnector::new(move || node(&d));
    let mut h = Harness::dialing(Arc::clone(&connector), Some("team"), (130, 26));
    h.settle().await;
    let s = h.screen();
    assert!(s.contains("lars-coder"), "{s}");
    assert!(s.contains("● node"), "{s}");
    assert_eq!(*connector.dials.lock().unwrap(), 1);

    // The daemon goes away, and stays away for a while.
    *connector.up.lock().unwrap() = false;
    connector.nodes.lock().unwrap()[0].kill();
    h.settle().await;
    let s = h.screen();
    assert!(s.contains("reconnecting"), "{s}");
    // What was on screen stays, as of the last snapshot.
    assert!(s.contains("lars-coder"), "{s}");
    for _ in 0..3 {
        tokio::time::advance(Duration::from_secs(5)).await;
        h.settle().await;
    }
    assert!(*connector.dials.lock().unwrap() >= 3, "it keeps trying");
    assert!(h.screen().contains("reconnecting (try"), "{}", h.screen());

    // The daemon is back: the views are opened afresh on the new connection.
    *connector.up.lock().unwrap() = true;
    tokio::time::advance(Duration::from_secs(5)).await;
    h.settle().await;
    let s = h.screen();
    assert!(s.contains("● node"), "{s}");
    let nodes = connector.nodes.lock().unwrap();
    assert_eq!(nodes.len(), 2);
    assert!(nodes[1].opened().iter().any(is_board));
    assert!(
        nodes[1]
            .opened()
            .iter()
            .any(|q| matches!(q, Query::Approvals { actor, .. } if actor == ME))
    );
}

#[tokio::test(start_paused = true)]
async fn the_tui_may_start_before_the_daemon() {
    let data: Shared = Arc::new(std::sync::Mutex::new(Data {
        board: team(),
        ..Data::default()
    }));
    let d = Arc::clone(&data);
    let connector = FakeConnector::new(move || node(&d));
    *connector.up.lock().unwrap() = false;
    let mut h = Harness::dialing(Arc::clone(&connector), Some("team"), (100, 14));
    h.settle().await;
    assert!(
        h.screen().contains("Waiting for `han node`"),
        "{}",
        h.screen()
    );
    *connector.up.lock().unwrap() = true;
    tokio::time::advance(Duration::from_secs(5)).await;
    h.settle().await;
    assert!(h.screen().contains("lars-coder"));
}

#[tokio::test(start_paused = true)]
async fn a_call_in_flight_when_the_daemon_dies_is_reported_not_lost() {
    let (mut h, api, _data) = started((130, 26)).await;
    h.keys("j<enter>").await;
    api.kill();
    h.settle().await;
    h.keys("ihello<enter>").await;
    h.settle().await;
    // With no connection the input is refused at once, in words.
    let s = h.screen();
    assert!(s.contains("not sent"), "{s}");
}

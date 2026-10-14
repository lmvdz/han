//! Render tests: the screens of the board, the session pane and the approvals pane, as text, drawn on a
//! `TestBackend` from a fake node. The golden files are in `tests/snapshots/`; `HAN_TUI_BLESS=1` rewrites them.

mod common;

use common::{assert_screen, pending_approval, started};
use han_node::api::{Fidelity, NodeError, Sandbox, SandboxBackend, SessionState};

const WIDE: (u16, u16) = (130, 26);

#[tokio::test]
async fn the_board_grouped_by_issue() {
    let (mut h, _api, _data) = started(WIDE).await;
    let screen = h.screen();
    assert_screen("board_by_issue", &screen);
    // Stale and failed are told apart from working by glyph and by words, not by colour alone.
    assert!(screen.contains("◌ @bo"), "{screen}");
    assert!(screen.contains("stale · last heard 14m ago"), "{screen}");
    assert!(screen.contains("✗ @bo"), "{screen}");
    assert!(screen.contains("failed: host unreachable"), "{screen}");
    // The overlap badge shows the highest tier and the count.
    assert!(screen.contains("⚑ hunk ×2"), "{screen}");
    // The row that waits on me says so.
    assert!(screen.contains("waiting-approval (awaits you)"), "{screen}");
    // The advisory sandbox is marked in words.
    assert!(screen.contains("(adv)"), "{screen}");
}

#[tokio::test]
async fn the_board_grouped_by_person_and_by_state() {
    let (mut h, _api, _data) = started(WIDE).await;
    h.keys("t").await;
    assert_screen("board_by_person", &h.screen());
    h.keys("t").await;
    let by_state = h.screen();
    assert_screen("board_by_state", &by_state);
    // State groups come in the node's fixed order.
    let order: Vec<usize> = [
        "waiting-approval (",
        "failed (",
        "stale (",
        "working (",
        "idle (",
        "done (",
    ]
    .iter()
    .map(|g| {
        by_state
            .find(&format!("▾ {g}"))
            .unwrap_or_else(|| panic!("{g} in\n{by_state}"))
    })
    .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{by_state}");
}

#[tokio::test]
async fn the_board_on_a_narrow_terminal_drops_columns_not_rows() {
    let (mut h, _api, _data) = started((72, 20)).await;
    let screen = h.screen();
    assert_screen("board_narrow", &screen);
    assert!(screen.contains("lars-coder"), "{screen}");
}

#[tokio::test]
async fn empty_and_failed_boards_say_why() {
    let (mut h, api, data) = started((100, 14)).await;
    data.lock().unwrap().board.clear();
    h.keys("r").await;
    assert!(h.screen().contains("No sessions in this space yet."));
    h.keys("/nothing<enter>").await;
    assert!(h.screen().contains("No session matches the filter."));

    // A denied board: read access is gone, and there is no retry advice.
    api.push(
        |q| matches!(q, han_node::api::Query::Board { .. }),
        han_node::api::ViewEvent::Failed {
            seq: 1,
            error: NodeError::denied("not a member"),
        },
    );
    h.settle().await;
    let s = h.screen();
    assert!(s.contains("Read access to team is gone"), "{s}");
    assert!(!s.contains("r tries again"), "{s}");
}

#[tokio::test]
async fn text_from_agents_is_shown_as_text() {
    let (mut h, _api, data) = started(WIDE).await;
    data.lock().unwrap().board[0].step = "<b>x</b> \u{202E}evil \u{1b}[31m".into();
    h.keys("r").await;
    let s = h.screen();
    assert!(s.contains("<b>x</b> <U+202E>evil <U+001B>[31m"), "{s}");
}

#[tokio::test]
async fn the_session_pane() {
    let (mut h, _api, data) = started(WIDE).await;
    {
        let mut d = data.lock().unwrap();
        d.board[0].sandbox = Some(Sandbox {
            backend: SandboxBackend::Bwrap,
            fidelity: Fidelity::Enforced,
        });
    }
    h.keys("j<enter>").await;
    let screen = h.screen();
    assert_screen("session_pane", &screen);
    assert!(screen.contains("Session ·"), "{screen}");
    // The timeline and the changes are pending until the node serves them.
    h.keys("2").await;
    let timeline = h.screen();
    assert_screen("session_timeline_pending", &timeline);
    assert!(timeline.contains("pending M4"), "{timeline}");
    h.keys("3").await;
    assert!(h.screen().contains("pending M4"));
}

#[tokio::test]
async fn the_session_pane_of_someone_elses_agent_is_watch_only() {
    let (mut h, _api, _data) = started(WIDE).await;
    // s2 (ada-bot) is the second row of #41; the first entry is the group header.
    h.keys("jj<enter>").await;
    let s = h.screen();
    assert!(s.contains("ada-bot") && s.contains("watch only"), "{s}");
}

#[tokio::test]
async fn the_approvals_pane_and_its_cards() {
    let (mut h, _api, data) = started(WIDE).await;
    pending_approval(&data);
    h.keys("r").await;
    h.keys("a").await;
    assert_screen("approvals_pane", &h.screen());
    h.keys("<enter>").await;
    let card = h.screen();
    assert_screen("approval_card", &card);
    assert!(card.contains("9f3c 41aa 07be"), "{card}");
    h.keys("a").await;
    let sign = h.screen();
    assert_screen("approval_sign_panel", &sign);
    assert!(sign.contains("You are signing an approval of:"), "{sign}");
}

#[tokio::test]
async fn a_card_that_cannot_be_signed_says_why() {
    let (mut h, _api, data) = started(WIDE).await;
    let mut a = han_tui::testing::approval("p2", "ada-bot", false);
    a.approvers = vec!["owner".into()];
    data.lock().unwrap().approvals = vec![a];
    h.keys("r").await;
    h.keys("a<enter>a").await;
    let s = h.screen();
    assert!(
        s.contains("You cannot approve this effect; it needs owner."),
        "{s}"
    );
    assert!(!s.contains("You are signing"), "{s}");
}

#[tokio::test]
async fn the_help_overlay_lists_the_keys() {
    let (mut h, _api, _data) = started(WIDE).await;
    h.keys("?").await;
    let s = h.screen();
    assert_screen("help", &s);
    assert!(s.contains("a, then Enter"), "{s}");
    h.keys("x").await;
    assert!(!h.screen().contains("Keys (any key closes)"));
    let _ = SessionState::Working;
}

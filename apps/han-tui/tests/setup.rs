//! Setup previews, durable recovery and failure-before-send behavior.

use std::time::Instant;

use han_node::api::{CallResult, Command, NodeError};
use han_tui::{
    app::{App, Conn},
    config::Config,
    input::LineEdit,
    msg::{Action, Msg},
    setup::Step,
    testing::{FakeApi, Harness},
};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn enter(app: &mut App) -> Vec<Action> {
    app.update(Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)))
}

fn draft() -> App {
    let mut app = App::new(Config::default(), None, Instant::now());
    app.conn = Conn::Up;
    app.run_command("setup");
    let setup = app.setup.as_mut().unwrap();
    setup.step = Step::Trusted;
    setup.fields = [
        LineEdit::with("Example team"),
        LineEdit::with("github:o/r,github:o/s"),
        LineEdit::default(),
    ];
    app
}

#[test]
fn launcher_handoff_requires_a_confirmed_space_and_explicit_continue() {
    let mut app = draft();
    app.guided_setup = true;
    enter(&mut app);
    let actions = enter(&mut app);
    let Action::SetupCall { id, .. } = actions[0] else {
        panic!("call")
    };
    assert!(app.setup_continue.is_none());
    assert!(!app.quit);
    app.update(Msg::Called {
        id,
        result: Ok(CallResult::Space {
            space: "new:a.localhost".into(),
        }),
    });
    assert_eq!(app.setup.as_ref().unwrap().step, Step::Ready);
    assert!(app.setup_continue.is_none());
    assert!(!app.quit);
    enter(&mut app);
    assert_eq!(app.setup_continue.as_deref(), Some("new:a.localhost"));
    assert!(app.quit);
}

#[test]
fn launcher_never_hands_off_a_queued_or_lost_creation() {
    for result in [
        Err(NodeError::unavailable("lost response")),
        Ok(CallResult::Queued {
            space: "new:a.localhost".into(),
            outbox_id: 1,
            id: "entry".into(),
        }),
    ] {
        let mut app = draft();
        app.guided_setup = true;
        enter(&mut app);
        let actions = enter(&mut app);
        let Action::SetupCall { id, .. } = actions[0] else {
            panic!("call")
        };
        app.update(Msg::Called { id, result });
        enter(&mut app);
        assert!(app.setup_continue.is_none());
        assert!(!app.quit);
    }
}

#[test]
fn preview_precedes_creation_and_uncertain_results_survive_restart() {
    let mut app = draft();
    assert!(enter(&mut app).is_empty());
    assert_eq!(app.setup.as_ref().unwrap().step, Step::Preview);
    let actions = enter(&mut app);
    let Action::SetupCall {
        id,
        command,
        config,
    } = &actions[0]
    else {
        panic!("persist then call")
    };
    assert!(
        matches!(command, Command::SpaceCreateTrusted { repos, .. } if repos == &["github:o/r", "github:o/s"])
    );
    assert!(config.setup_recovery.is_some());
    assert!(enter(&mut app).is_empty());
    app.update(Msg::Called {
        id: *id,
        result: Err(NodeError::unavailable("reply lost")),
    });
    assert_eq!(app.setup.as_ref().unwrap().step, Step::Recovery);
    assert!(enter(&mut app).is_empty());
    let mut restored = App::new(app.config.clone(), None, Instant::now());
    assert_eq!(restored.setup.as_ref().unwrap().step, Step::Recovery);
    assert!(enter(&mut restored).is_empty());
}

#[test]
fn queued_creation_keeps_its_space_and_never_reports_ready() {
    let mut app = draft();
    enter(&mut app);
    let actions = enter(&mut app);
    let Action::SetupCall { id, .. } = actions[0] else {
        panic!("call")
    };
    app.update(Msg::Called {
        id,
        result: Ok(CallResult::Queued {
            space: "new:a.localhost".into(),
            outbox_id: 7,
            id: "entry".into(),
        }),
    });
    assert!(app.space.is_none());
    assert!(
        app.config
            .setup_recovery
            .as_ref()
            .unwrap()
            .contains("new:a.localhost")
    );
    assert!(enter(&mut app).is_empty());
}

#[tokio::test]
async fn host_saves_recovery_before_call_and_then_remembers_success() {
    let path = std::env::temp_dir().join(format!("han-setup-{}.json", std::process::id()));
    let api = FakeApi::new();
    let inspect_path = path.clone();
    api.answer(move |_| {
        assert!(Config::load(&inspect_path).setup_recovery.is_some());
        Ok(CallResult::Space {
            space: "new:a.localhost".into(),
        })
    });
    let mut h = Harness::build(
        Some(api.clone()),
        None,
        None,
        (90, 30),
        Config::default(),
        Some(path.clone()),
    );
    h.settle().await;
    h.keys(":setup<enter>1Example<tab>github:o/r<enter>").await;
    assert!(api.calls().is_empty());
    let screen = h.screen();
    assert!(screen.contains("github:o/r"), "{screen}");
    assert!(screen.contains("owner approval"), "{screen}");
    h.keys("<enter>").await;
    assert_eq!(api.calls().len(), 1);
    assert!(h.rt.app.setup.is_none());
    assert_eq!(
        Config::load(&path).last_space.as_deref(),
        Some("new:a.localhost")
    );
    assert!(Config::load(&path).setup_recovery.is_none());
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn a_failed_recovery_save_does_not_send_the_operation() {
    let api = FakeApi::new();
    let mut h = Harness::new(api.clone(), None, (90, 30));
    h.settle().await;
    h.keys(":setup<enter>1Example<tab>github:o/r<enter><enter>")
        .await;
    assert!(api.calls().is_empty());
    assert!(
        h.rt.app
            .setup
            .as_ref()
            .unwrap()
            .notice
            .contains("cannot save")
    );
}

#[tokio::test]
async fn join_ticket_is_previewed_but_never_saved_in_preferences() {
    let api = FakeApi::new();
    let mut h = Harness::new(api.clone(), None, (90, 30));
    h.settle().await;
    h.keys(":setup<enter>2han1.secret-ticket<enter>").await;
    assert!(api.calls().is_empty());
    assert!(
        !serde_json::to_string(&h.rt.app.config)
            .unwrap()
            .contains("secret-ticket")
    );
    assert!(matches!(
        h.rt.app.setup.as_ref().unwrap().command,
        Some(Command::SpaceJoin { .. })
    ));
    assert!(h.screen().contains("team's existing policy"));
}

fn existing_team() -> App {
    use han_node::api::{Row, SpaceLink, SpaceStatusRow, TrustedSetup};
    let mut app = App::new(
        Config {
            setup_recovery: Some("inspect the previous creation".into()),
            ..Config::default()
        },
        Some("team:a.localhost".into()),
        Instant::now(),
    );
    app.guided_setup = true;
    app.conn = Conn::Up;
    let Row::NodeStatus(node) = han_tui::testing::node_status("@alice:a.localhost") else {
        unreachable!()
    };
    app.node = Some(node);
    app.space_status = Some(SpaceStatusRow {
        key: "team:a.localhost".into(),
        space: "team:a.localhost".into(),
        home: "a.localhost".into(),
        link: SpaceLink::Live,
        head: 1,
        sessions: 0,
        members: 1,
        outbox_pending: 0,
        signals: 0,
        error: None,
        trusted_setup: Some(TrustedSetup {
            can_manage: true,
            repos: vec!["github:o/r".into()],
        }),
    });
    app
}

#[test]
fn existing_team_handoff_requires_selection_preview_and_enter_without_resubmission() {
    let mut app = existing_team();
    assert!(
        app.run_command("setup-existing").is_empty(),
        "selection sends no command"
    );
    let setup = app.setup.as_ref().unwrap();
    assert_eq!(setup.step, Step::Ready);
    assert_eq!(setup.ready_space.as_deref(), Some("team:a.localhost"));
    assert!(setup.notice.contains("a.localhost") && setup.notice.contains("github:o/r"));
    assert!(!app.quit && app.setup_continue.is_none());
    assert!(
        enter(&mut app).is_empty(),
        "handoff sends no space creation"
    );
    assert_eq!(app.setup_continue.as_deref(), Some("team:a.localhost"));
    assert!(app.quit);
    assert!(
        app.config.setup_recovery.is_some(),
        "only the launcher clears recovery after live validation"
    );
}

#[test]
fn existing_team_refuses_ineligible_or_mismatched_views_and_preserves_recovery() {
    use han_node::api::SpaceLink;
    let invalid: &[fn(&mut App)] = &[
        |a| a.guided_setup = false,
        |a| a.conn = Conn::Connecting { attempt: 1 },
        |a| a.node = None,
        |a| a.node.as_mut().unwrap().outbox_pending = 1,
        |a| a.node.as_mut().unwrap().problems.push("blocked".into()),
        |a| a.space = None,
        |a| a.space_status = None,
        |a| a.space_status.as_mut().unwrap().space = "other:a.localhost".into(),
        |a| a.space_status.as_mut().unwrap().link = SpaceLink::Offline,
        |a| a.space_status.as_mut().unwrap().outbox_pending = 1,
        |a| a.space_status.as_mut().unwrap().trusted_setup = None,
        |a| {
            a.space_status
                .as_mut()
                .unwrap()
                .trusted_setup
                .as_mut()
                .unwrap()
                .can_manage = false
        },
    ];
    for change in invalid {
        let mut app = existing_team();
        change(&mut app);
        assert!(app.run_command("setup-existing").is_empty());
        assert_eq!(app.setup.as_ref().unwrap().step, Step::Recovery);
        assert!(!app.quit && app.setup_continue.is_none());
        assert!(app.config.setup_recovery.is_some());
    }
}

#[test]
fn existing_confirmation_rechecks_eligibility_and_keeps_the_frozen_selection() {
    for change in [
        (|a: &mut App| a.space = Some("other:a.localhost".into())) as fn(&mut App),
        |a| {
            a.space_status
                .as_mut()
                .unwrap()
                .trusted_setup
                .as_mut()
                .unwrap()
                .can_manage = false
        },
        |a| a.conn = Conn::Connecting { attempt: 1 },
    ] {
        let mut app = existing_team();
        app.run_command("setup-existing");
        change(&mut app);
        assert!(enter(&mut app).is_empty());
        assert!(!app.quit && app.setup_continue.is_none());
        assert_eq!(
            app.setup.as_ref().unwrap().ready_space.as_deref(),
            Some("team:a.localhost")
        );
        assert!(app.config.setup_recovery.is_some());
        app.update(Msg::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(app.setup.is_none());
        assert!(app.config.setup_recovery.is_some());
    }
}

#[tokio::test]
async fn existing_preview_shows_the_exact_team_home_and_repository() {
    let mut h = Harness::new(FakeApi::new(), None, (100, 30));
    h.settle().await;
    h.rt.app = existing_team();
    h.rt.app.run_command("setup-existing");
    let screen = h.screen();
    assert!(screen.contains("Team: team:a.localhost"), "{screen}");
    assert!(screen.contains("Existing team on a.localhost"), "{screen}");
    assert!(screen.contains("github:o/r"), "{screen}");
    assert!(screen.contains("Enter continues setup"), "{screen}");
}

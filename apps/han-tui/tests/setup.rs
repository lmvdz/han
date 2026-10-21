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

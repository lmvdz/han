//! Task intake preserves accepted work and does not repeat an uncertain create/start.
use std::time::Instant;

use han_node::api::{CallResult, Command, NodeError, Row, TaskRow};
use han_tui::app::{App, Conn};
use han_tui::config::Config;
use han_tui::input::LineEdit;
use han_tui::msg::{Action, Msg, ViewKind};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn enter(app: &mut App) -> Vec<Action> {
    app.update(Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)))
}

fn ready() -> App {
    let mut app = App::new(Config::default(), Some("team".into()), Instant::now());
    app.conn = Conn::Up;
    app.run_command("task");
    app.task_draft.as_mut().unwrap().fields = [
        LineEdit::with("@coder:example.com"),
        LineEdit::with("github:team/project"),
        LineEdit::with("Fix the README links"),
    ];
    app
}

fn call(actions: Vec<Action>) -> (u64, Command) {
    assert_eq!(actions.len(), 1);
    match actions.into_iter().next().unwrap() {
        Action::Call { id, command } => (id, command),
        _ => panic!("expected one call"),
    }
}

fn receipt(id: &str) -> CallResult {
    CallResult::Receipt {
        space: "team".into(),
        seq: 8,
        id: id.into(),
        time: "2026-10-20T21:00:00Z".into(),
    }
}

#[test]
fn accepted_task_is_started_once_and_a_refused_start_reuses_it() {
    let mut app = ready();
    let (create, cmd) = call(enter(&mut app));
    assert!(matches!(cmd, Command::TaskCreate { .. }));
    assert!(
        enter(&mut app).is_empty(),
        "repeated Enter while pending is inert"
    );
    let (start, cmd) = call(app.update(Msg::Called {
        id: create,
        result: Ok(receipt("task1")),
    }));
    assert!(matches!(cmd, Command::AgentStart { task: Some(t), .. } if t == "task1"));
    app.update(Msg::Called {
        id: start,
        result: Err(NodeError::denied("confirm the agent first")),
    });
    assert_eq!(
        app.task_draft.as_ref().unwrap().task.as_deref(),
        Some("task1")
    );
    let (retry, cmd) = call(enter(&mut app));
    assert!(matches!(cmd, Command::AgentStart { task: Some(t), .. } if t == "task1"));
    app.update(Msg::Called {
        id: retry,
        result: Ok(CallResult::Done),
    });
    assert!(app.task_draft.is_none());
    assert!(
        app.toast
            .as_ref()
            .unwrap()
            .text
            .contains("Started task task1")
    );
}

#[test]
fn queued_task_is_not_started_until_explicitly_requested() {
    let mut app = ready();
    let (id, _) = call(enter(&mut app));
    let actions = app.update(Msg::Called {
        id,
        result: Ok(CallResult::Queued {
            outbox_id: 12,
            space: "team".into(),
            id: "queued-task".into(),
        }),
    });
    assert!(actions.is_empty());
    assert!(app.task_draft.as_ref().unwrap().notice.contains("queued"));
    let (_, cmd) = call(enter(&mut app));
    assert!(matches!(cmd, Command::AgentStart { task: Some(t), .. } if t == "queued-task"));
}

#[test]
fn an_unknown_creation_result_never_resubmits_on_enter_or_reconnect() {
    let mut app = ready();
    let (id, _) = call(enter(&mut app));
    app.update(Msg::Called {
        id,
        result: Err(NodeError::unavailable("lost reply")),
    });
    assert!(app.task_draft.as_ref().unwrap().uncertain);
    assert!(enter(&mut app).is_empty());
    let actions = app.update(Msg::Connected);
    assert!(!actions.iter().any(|a| matches!(a, Action::Call { .. })));
    assert!(enter(&mut app).is_empty());
}

#[test]
fn an_unknown_start_result_keeps_the_task_but_does_not_retry() {
    let mut app = ready();
    let (id, _) = call(enter(&mut app));
    let (start, _) = call(app.update(Msg::Called {
        id,
        result: Ok(receipt("task1")),
    }));
    app.update(Msg::Called {
        id: start,
        result: Err(NodeError::unavailable("lost reply")),
    });
    assert_eq!(
        app.task_draft.as_ref().unwrap().task.as_deref(),
        Some("task1")
    );
    assert!(enter(&mut app).is_empty());
}

#[test]
fn selecting_an_existing_task_never_creates_a_new_one() {
    let mut app = ready();
    app.task_draft = None;
    let actions = app.run_command("tasks");
    let epoch = match &actions[0] {
        Action::Subscribe { epoch, .. } => *epoch,
        _ => panic!("task subscription"),
    };
    let row = TaskRow {
        key: "team:8".into(),
        space: "team".into(),
        task: "existing".into(),
        seq: 8,
        author: "@person:example.com".into(),
        to: vec!["@coder:example.com".into()],
        external: Some("github:team/project#8".into()),
        ask: Some("Fix links".into()),
        open: true,
        closed_as: None,
        guarded_for: vec![],
        accepted: vec![],
        budget: None,
    };
    app.update(Msg::View {
        view: ViewKind::Tasks,
        epoch,
        event: han_tui::testing::snapshot(vec![Row::Task(row)], 1, 0, None).with_seq(1),
    });
    assert!(
        enter(&mut app).is_empty(),
        "selection opens a form, not a session"
    );
    assert_eq!(
        app.task_draft.as_ref().unwrap().task.as_deref(),
        Some("existing")
    );
    let (_, cmd) = call(enter(&mut app));
    assert!(
        matches!(cmd, Command::AgentStart { task: Some(t), repo: Some(r), .. }
        if t == "existing" && r == "github:team/project")
    );
}

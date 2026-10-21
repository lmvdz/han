//! The TUI as a pure state machine: [`App::update`] takes a [`Msg`] (a key, a view event, a call result, the
//! clock) and returns the [`Action`]s to carry out. Nothing here touches the terminal or the network, so the
//! whole thing is tested with a fake node and a `TestBackend`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use han_node::api::{
    BoardRow, CallResult, Command, Group, NodeError, NodeStatusRow, Query, Row, SessionPart,
    SessionState, SpaceStatusRow, TaskRow, ViewEvent, Window,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::approvals::{ApprovalsState, Card, Tone};
use crate::board::{BoardState, Entry, EntryId, Nav};
use crate::config::Config;
use crate::input::LineEdit;
use crate::msg::{Action, Msg, ViewKind};
use crate::session::{Delivery, SessionPane, Tab};

/// How long a transient message stays.
const TOAST: Duration = Duration::from_secs(4);

/// Which pane has the keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    /// The board.
    Board,
    /// The session pane.
    Session,
    /// The approvals pane.
    Approvals,
}

/// A line being typed at the bottom of the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Keys are commands.
    Normal,
    /// The `:` palette.
    Palette(LineEdit),
    /// The filter box.
    Filter(LineEdit),
}

/// The state of the link to the node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conn {
    /// Dialing the node; `attempt` is 0 before the first try.
    Connecting {
        /// Which attempt.
        attempt: u32,
    },
    /// Connected, views open.
    Up,
}

/// A transient message.
#[derive(Clone, Debug)]
pub struct Toast {
    /// The text.
    pub text: String,
    /// How it looks.
    pub tone: Tone,
    until: Instant,
}

#[derive(Clone, Debug)]
enum CallKind {
    Setup,
    Approve,
    Cancel,
    Steer,
    Admit,
    CreateTask,
    StartTask,
}

/// A task being composed. A known task id is retained when its start fails, so retrying never creates
/// another task. An uncertain create is never silently repeated after a disconnect.
#[derive(Clone, Debug)]
pub struct TaskDraft {
    /// The space selected when the form opened.
    pub space: String,
    /// Agent, repository and request fields.
    pub fields: [LineEdit; 3],
    /// Active field.
    pub field: usize,
    /// A call is awaiting its response.
    pub pending: bool,
    /// The accepted or queued task, if creation returned its id.
    pub task: Option<String>,
    /// A lost reply means the user must inspect tasks before creating again.
    pub uncertain: bool,
    /// Persistent result or recovery guidance.
    pub notice: String,
}

/// The whole state of the TUI.
pub struct App {
    /// Guided create/join flow.
    pub setup: Option<crate::setup::Setup>,
    /// Whether this TUI was opened by the guided setup launcher.
    pub guided_setup: bool,
    /// Explicitly accepted handoff to the launcher after space confirmation.
    pub setup_continue: Option<String>,
    /// What is remembered between runs.
    pub config: Config,
    /// The space on the board.
    pub space: Option<String>,
    /// The local actor, learned from the node.
    pub me: Option<String>,
    /// The node's own status.
    pub node: Option<NodeStatusRow>,
    /// The space's status on this device.
    pub space_status: Option<SpaceStatusRow>,
    /// The spaces this device follows, as the node last said.
    pub spaces: Vec<SpaceStatusRow>,
    /// The link to the node.
    pub conn: Conn,
    /// The board.
    pub board: BoardState,
    /// The open session, if any.
    pub session: Option<SessionPane>,
    /// The approvals pane.
    pub approvals: ApprovalsState,
    /// Whether the approvals side pane is shown.
    pub approvals_open: bool,
    /// Which pane has the keys.
    pub focus: Focus,
    /// What the bottom line is doing.
    pub mode: Mode,
    /// Whether the help overlay is up.
    pub help: bool,
    /// Whether the overlap detail of the selected session is up.
    pub overlap_detail: bool,
    /// The transient message.
    pub toast: Option<Toast>,
    /// The task creation form, if open.
    pub task_draft: Option<TaskDraft>,
    /// Whether the task list is open.
    pub tasks_open: bool,
    /// Selected row in the task list.
    pub task_selection: usize,
    /// The task list's persistent status.
    pub tasks_notice: String,
    /// Terminal size, columns and rows.
    pub size: (u16, u16),
    /// The clock, as of the last tick.
    pub now: Instant,
    /// Whether the program should end.
    pub quit: bool,
    spaces_mirror: han_node::api::ViewMirror,
    tasks_mirror: han_node::api::ViewMirror,
    pending_g: bool,
    epochs: HashMap<ViewKind, u64>,
    /// The `seq` of the last event of each open view: a gap means a missed event.
    seqs: HashMap<ViewKind, u64>,
    next_epoch: u64,
    next_call: u64,
    calls: HashMap<u64, CallKind>,
}

fn ctrl(k: &KeyEvent, c: char) -> bool {
    k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char(c)
}

impl App {
    /// A known local signing blocker; the home still decides policy eligibility.
    pub fn approval_block_reason(&self) -> Option<String> {
        if self.node.as_ref().is_some_and(|node| node.headless) {
            return Some(
                "This node is headless and cannot approve. Use your personal node.".into(),
            );
        }
        self.approvals.block_reason(self.me.as_deref())
    }

    /// A new app. `space` is the space to show (else the one last shown, else none).
    pub fn new(config: Config, space: Option<String>, now: Instant) -> App {
        let space = space.or_else(|| config.last_space.clone());
        let group = space
            .as_deref()
            .map_or(Group::Issue, |s| config.group_of(s));
        App {
            guided_setup: false,
            setup_continue: None,
            setup: config
                .setup_recovery
                .as_deref()
                .map(|s| crate::setup::Setup::new(Some(s))),
            config,
            space,
            me: None,
            node: None,
            space_status: None,
            spaces: Vec::new(),
            conn: Conn::Connecting { attempt: 0 },
            board: BoardState::new(group),
            session: None,
            approvals: ApprovalsState::new(),
            approvals_open: false,
            focus: Focus::Board,
            mode: Mode::Normal,
            task_draft: None,
            tasks_open: false,
            task_selection: 0,
            tasks_notice: String::new(),
            tasks_mirror: han_node::api::ViewMirror::new(),
            help: false,
            overlap_detail: false,
            toast: None,
            size: (100, 30),
            now,
            quit: false,
            spaces_mirror: han_node::api::ViewMirror::new(),
            pending_g: false,
            epochs: HashMap::new(),
            seqs: HashMap::new(),
            next_epoch: 1,
            next_call: 1,
            calls: HashMap::new(),
        }
    }

    // ---- subscriptions --------------------------------------------------------------------------------

    fn sub(&mut self, view: ViewKind, query: Query) -> Action {
        let epoch = self.next_epoch;
        self.next_epoch += 1;
        self.epochs.insert(view, epoch);
        self.seqs.remove(&view);
        Action::Subscribe {
            view,
            epoch,
            query: Box::new(query),
        }
    }

    fn unsub(&mut self, view: ViewKind) -> Action {
        self.epochs.remove(&view);
        Action::Unsubscribe(view)
    }

    fn board_sub(&mut self) -> Vec<Action> {
        match self.space.clone() {
            Some(space) => {
                let q = self.board.query(&space);
                vec![self.sub(ViewKind::Board, q)]
            }
            None => Vec::new(),
        }
    }

    fn session_subs(&mut self) -> Vec<Action> {
        let Some(p) = &self.session else {
            return Vec::new();
        };
        let (id, latest) = (p.id.clone(), p.latest.clone());
        let mut out = vec![
            self.sub(
                ViewKind::Session,
                Query::Session {
                    id: id.clone(),
                    part: SessionPart::Facts,
                    window: Window::default(),
                },
            ),
            self.sub(
                ViewKind::Inputs,
                Query::Inputs {
                    session: id.clone(),
                    window: Window::default(),
                },
            ),
            // The latest page of the typed stream; the node leaves out what this actor may not read.
            self.sub(
                ViewKind::Items,
                Query::Session {
                    id: id.clone(),
                    part: SessionPart::Items,
                    window: Window {
                        limit: 200,
                        from_end: true,
                        ..Window::default()
                    },
                },
            ),
            self.sub(
                ViewKind::Timeline,
                Query::Timeline {
                    session: id.clone(),
                    at: None,
                    window: Window {
                        limit: 2000,
                        ..Window::default()
                    },
                },
            ),
        ];
        if let Some(to) = latest {
            out.push(self.sub(
                ViewKind::Patches,
                Query::Patches {
                    session: id,
                    from: None,
                    to,
                    window: Window::default(),
                },
            ));
        }
        out
    }

    /// Every view the screen needs, opened afresh (at start, and after a reconnect).
    fn resubscribe_all(&mut self) -> Vec<Action> {
        self.epochs.clear();
        self.seqs.clear();
        let mut out = vec![
            self.sub(
                ViewKind::Node,
                Query::NodeStatus {
                    window: Window::default(),
                },
            ),
            self.sub(
                ViewKind::Spaces,
                Query::Spaces {
                    window: Window::default(),
                },
            ),
        ];
        if let Some(space) = self.space.clone() {
            out.extend(self.board_sub());
            out.push(self.sub(
                ViewKind::Space,
                Query::SpaceStatus {
                    space,
                    window: Window::default(),
                },
            ));
        }
        if let Some(me) = self.me.clone() {
            out.push(self.sub(
                ViewKind::Approvals,
                Query::Approvals {
                    actor: me,
                    window: Window::default(),
                },
            ));
        }
        out.extend(self.session_subs());
        if self.tasks_open {
            out.extend(self.open_tasks());
        }
        out
    }

    fn call(&mut self, kind: CallKind, command: Command) -> Action {
        let id = self.next_call;
        self.next_call += 1;
        self.calls.insert(id, kind);
        Action::Call { id, command }
    }

    fn say(&mut self, tone: Tone, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            tone,
            until: self.now + TOAST,
        });
    }

    // ---- messages -------------------------------------------------------------------------------------

    /// Handles one message and returns what to do about it.
    pub fn update(&mut self, msg: Msg) -> Vec<Action> {
        match msg {
            Msg::Key(k) => self.on_key(k),
            Msg::Paste(text) => {
                if let Some(setup) = &mut self.setup {
                    setup.paste(&text);
                    return Vec::new();
                }
                if let Some(draft) = &mut self.task_draft {
                    if !draft.pending
                        && (draft.task.is_none() || draft.field < 2)
                        && !draft.uncertain
                    {
                        draft.fields[draft.field].insert(&text.replace(['\n', '\r'], " "));
                    }
                    return Vec::new();
                }
                match &mut self.mode {
                    Mode::Palette(e) | Mode::Filter(e) => e.insert(&text),
                    Mode::Normal => {
                        if let Some(e) = self.session.as_mut().and_then(|s| s.steer.as_mut()) {
                            e.insert(&text);
                        }
                    }
                }
                Vec::new()
            }
            Msg::Resize(w, h) => {
                self.size = (w, h);
                let limit = (u32::from(h) * 3).clamp(50, 500);
                if self.board.set_limit(limit) && self.board.synced {
                    return self.board_sub();
                }
                Vec::new()
            }
            Msg::Tick(now) => {
                self.now = now;
                if self.toast.as_ref().is_some_and(|t| t.until <= now) {
                    self.toast = None;
                }
                Vec::new()
            }
            Msg::View { view, epoch, event } => {
                if self.epochs.get(&view) != Some(&epoch) {
                    return Vec::new();
                }
                // Each event is numbered; a number that does not follow the last one, or the node saying it
                // dropped events, means the face missed something: take new snapshots.
                match &event {
                    ViewEvent::Resync { .. } => return self.resubscribe_all(),
                    ViewEvent::End { .. } | ViewEvent::Failed { .. } => {}
                    ViewEvent::Snapshot { seq, .. } => {
                        self.seqs.insert(view, *seq);
                    }
                    ViewEvent::Diff { seq, .. } | ViewEvent::Resnapshot { seq, .. } => {
                        if self.seqs.get(&view).is_none_or(|last| *seq != last + 1) {
                            return self.resubscribe_all();
                        }
                        self.seqs.insert(view, *seq);
                    }
                }
                self.on_view(view, event)
            }
            Msg::ViewEnded { view, epoch } => {
                if self.epochs.get(&view) != Some(&epoch) || view != ViewKind::Node {
                    return Vec::new();
                }
                self.lost()
            }
            Msg::Called { id, result } => self.on_called(id, result),
            Msg::Connecting { attempt } => {
                self.conn = Conn::Connecting { attempt };
                Vec::new()
            }
            Msg::Connected => {
                self.conn = Conn::Up;
                self.resubscribe_all()
            }
        }
    }

    /// The node went away: the views are void and the host dials again. What is on screen stays until the new
    /// snapshots replace it.
    fn lost(&mut self) -> Vec<Action> {
        if self.conn != Conn::Up {
            return Vec::new();
        }
        self.conn = Conn::Connecting { attempt: 0 };
        self.epochs.clear();
        self.calls.clear();
        if let Some(setup) = &mut self.setup
            && setup.step == crate::setup::Step::Waiting
        {
            setup.step = crate::setup::Step::Recovery;
            setup.notice = self.config.setup_recovery.clone().unwrap_or_default();
        }
        if let Some(draft) = &mut self.task_draft
            && draft.pending
        {
            draft.pending = false;
            draft.uncertain = true;
            draft.notice = "Connection lost; the result is unknown. Inspect tasks and the board before retrying. Nothing will be repeated automatically.".into();
        }
        if let Card::Signing {
            key,
            digest,
            busy: true,
        } = self.approvals.card.clone()
        {
            self.approvals.card = Card::Signing {
                key,
                digest,
                busy: false,
            };
            self.approvals.notice = Some((Tone::Bad, "Lost contact with the node.".to_owned()));
            self.approvals.retry = true;
        }
        if let Some(p) = &mut self.session
            && p.delivery == Some(Delivery::Sending)
        {
            p.delivery = Some(Delivery::Refused("lost contact with the node".to_owned()));
        }
        vec![Action::Reconnect]
    }

    fn on_view(&mut self, view: ViewKind, event: ViewEvent) -> Vec<Action> {
        match view {
            ViewKind::Tasks => {
                let selected = self
                    .task_rows()
                    .get(self.task_selection)
                    .map(|r| r.task.clone());
                self.tasks_notice = match &event {
                    ViewEvent::Failed { error, .. } => format!("Tasks unavailable: {error}"),
                    ViewEvent::End { .. } => "Task view disconnected; refresh to reconnect".into(),
                    _ => String::new(),
                };
                self.tasks_mirror.apply(event);
                let rows = self.task_rows();
                self.task_selection = selected
                    .and_then(|id| rows.iter().position(|r| r.task == id))
                    .unwrap_or(self.task_selection.min(rows.len().saturating_sub(1)));
                Vec::new()
            }
            ViewKind::Node => {
                if let ViewEvent::Failed { error, .. } = &event {
                    return if matches!(error, NodeError::Unavailable { .. }) {
                        self.lost()
                    } else {
                        self.say(Tone::Bad, format!("node status: {error}"));
                        Vec::new()
                    };
                }
                let mut mirror = han_node::api::ViewMirror::new();
                mirror.apply(event);
                let row = mirror.rows().into_iter().find_map(|r| match r {
                    Row::NodeStatus(n) => Some(n),
                    _ => None,
                });
                let Some(row) = row else { return Vec::new() };
                let first = self.me.is_none();
                let headless = row.headless;
                self.me = Some(row.actor.clone());
                self.node = Some(row);
                if first && headless {
                    self.say(
                        Tone::Info,
                        "This node is headless (a service node): it asks for no person to govern or steer and it never approves.",
                    );
                }
                if first {
                    let me = self.me.clone().unwrap_or_default();
                    return vec![self.sub(
                        ViewKind::Approvals,
                        Query::Approvals {
                            actor: me,
                            window: Window::default(),
                        },
                    )];
                }
                Vec::new()
            }
            ViewKind::Spaces => {
                if matches!(event, ViewEvent::Failed { .. }) {
                    return Vec::new();
                }
                self.spaces_mirror.apply(event);
                self.spaces = self
                    .spaces_mirror
                    .rows()
                    .into_iter()
                    .filter_map(|r| match r {
                        Row::SpaceStatus(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                Vec::new()
            }
            ViewKind::Space => {
                if let ViewEvent::Snapshot { rows, .. } | ViewEvent::Resnapshot { rows, .. } =
                    &event
                {
                    self.space_status = rows.iter().find_map(|r| match r {
                        Row::SpaceStatus(s) => Some(s.clone()),
                        _ => None,
                    });
                } else if let ViewEvent::Diff { ops, .. } = event {
                    for op in ops {
                        match op {
                            han_node::api::DiffOp::Insert {
                                row: Row::SpaceStatus(s),
                                ..
                            }
                            | han_node::api::DiffOp::Update {
                                row: Row::SpaceStatus(s),
                            } => {
                                self.space_status = Some(s);
                            }
                            _ => {}
                        }
                    }
                }
                Vec::new()
            }
            ViewKind::Board => {
                let more = self.board.apply(event, self.now);
                if let Some(p) = &mut self.session
                    && let Some(r) = self.board.row_of(&p.id)
                {
                    p.hint = Some(r.clone());
                }
                if more { self.board_sub() } else { Vec::new() }
            }
            ViewKind::Approvals => {
                let (before, was_synced) = (self.approvals.count(), self.approvals.synced);
                self.approvals.apply(event);
                let after = self.approvals.count();
                // The first snapshot does not shout; a new effect does, unless the pane is in front.
                if was_synced && after > before && self.focus != Focus::Approvals {
                    self.say(
                        Tone::Info,
                        if after == 1 {
                            "1 effect waits for your approval (a)".to_owned()
                        } else {
                            format!("{after} effects wait for your approval (a)")
                        },
                    );
                }
                Vec::new()
            }
            ViewKind::Session => {
                if let Some(p) = &mut self.session {
                    p.apply_session(event);
                }
                Vec::new()
            }
            ViewKind::Inputs => {
                if let Some(p) = &mut self.session {
                    p.apply_inputs(event);
                }
                Vec::new()
            }
            ViewKind::Items => {
                if let Some(p) = &mut self.session {
                    p.apply_items(event);
                }
                Vec::new()
            }
            ViewKind::Timeline => {
                let Some(p) = &mut self.session else {
                    return Vec::new();
                };
                let latest = p.apply_timeline(event);
                if latest.is_some() && latest != p.latest {
                    p.latest = latest;
                    return match self.session.as_ref().map(|p| p.id.clone()) {
                        Some(id) => {
                            let to = self
                                .session
                                .as_ref()
                                .and_then(|p| p.latest.clone())
                                .unwrap_or_default();
                            vec![self.sub(
                                ViewKind::Patches,
                                Query::Patches {
                                    session: id,
                                    from: None,
                                    to,
                                    window: Window::default(),
                                },
                            )]
                        }
                        None => Vec::new(),
                    };
                }
                Vec::new()
            }
            ViewKind::Patches => {
                if let Some(p) = &mut self.session {
                    p.apply_patches(event);
                }
                Vec::new()
            }
        }
    }

    fn on_called(&mut self, id: u64, result: Result<CallResult, NodeError>) -> Vec<Action> {
        let Some(kind) = self.calls.remove(&id) else {
            return Vec::new();
        };
        match kind {
            CallKind::Setup => {
                let Some(setup) = &mut self.setup else { return Vec::new() };
                match result {
                    Ok(CallResult::Space { space }) => {
                        self.config.setup_recovery = None;
                        if self.guided_setup {
                            setup.step = crate::setup::Step::Ready;
                            setup.ready_space = Some(space.clone());
                            setup.notice.clear();
                            return self.set_space(space);
                        }
                        self.setup = None;
                        self.say(Tone::Good, "Space ready. Use :task to start work after configuring your agent and runner.");
                        return self.set_space(space);
                    }
                    Ok(CallResult::Queued { space, outbox_id, .. }) => {
                        setup.step = crate::setup::Step::Recovery;
                        setup.notice = format!("Creation queued for {space} (outbox {outbox_id}). Do not create again. Close setup and inspect :space and the outbox until it reaches the home.");
                    }
                    Err(error) if !matches!(error, NodeError::Unavailable { .. }) => {
                        self.config.setup_recovery = None;
                        setup.step = crate::setup::Step::Preview;
                        setup.notice = format!("Setup was refused: {error}. Backspace edits; Enter retries.");
                    }
                    other => {
                        setup.step = crate::setup::Step::Recovery;
                        setup.notice = format!("Setup result is unknown ({other:?}). Inspect followed spaces and the outbox before creating again. No automatic retry.");
                    }
                }
                if setup.step == crate::setup::Step::Recovery {
                    self.config.setup_recovery = Some(setup.notice.clone());
                }
                return vec![Action::SaveConfig(self.config.clone())];
            }
            CallKind::CreateTask => {
                let Some(draft) = &mut self.task_draft else { return Vec::new() };
                draft.pending = false;
                match result {
                    Ok(CallResult::Receipt { id, .. }) => {
                        draft.task = Some(id);
                        return self.submit_task();
                    }
                    Ok(CallResult::Queued { id, .. }) => {
                        draft.task = Some(id);
                        draft.notice = "Task is queued in the outbox. Once it reaches the home, Enter starts this same task.".into();
                    }
                    Err(error) => {
                        draft.uncertain = matches!(error, NodeError::Unavailable { .. });
                        draft.notice = if draft.uncertain {
                            format!("Creation result unknown: {error}. Inspect tasks before creating again.")
                        } else { format!("Task was not created: {error}") };
                    }
                    Ok(_) => {
                        draft.uncertain = true;
                        draft.notice = "Unexpected response. Inspect tasks before creating again.".into();
                    }
                }
            }
            CallKind::StartTask => {
                let Some(draft) = &mut self.task_draft else { return Vec::new() };
                draft.pending = false;
                match result {
                    Ok(_) => {
                        let id = draft.task.clone().unwrap_or_default();
                        self.task_draft = None;
                        self.tasks_open = false;
                        self.focus = Focus::Board;
                        self.say(Tone::Good, format!("Started task {id}"));
                        let mut actions = self.board_sub();
                        actions.push(self.unsub(ViewKind::Tasks));
                        return actions;
                    }
                    Err(error) => {
                        draft.uncertain = matches!(error, NodeError::Unavailable { .. });
                        draft.notice = if draft.uncertain {
                            format!("Task exists; start result unknown: {error}. Inspect the board before starting this task again. No automatic retry.")
                        } else {
                            format!("Task exists; start was refused: {error}. Enter retries only start, without creating another task.")
                        };
                    }
                }
            }
            CallKind::Approve => match result {
                Ok(CallResult::Queued { .. }) => {
                    self.approvals.card = Card::Closed;
                    self.say(
                        Tone::Good,
                        "Approval signed and queued; it will be sent when the space's home is reachable",
                    );
                }
                Ok(r) => {
                    self.approvals.card = Card::Closed;
                    let seq = match r {
                        CallResult::Receipt { seq, .. } => format!(" · seq {seq}"),
                        _ => String::new(),
                    };
                    self.say(Tone::Good, format!("Approved{seq}"));
                }
                Err(e) => self.card_failed(&e),
            },
            CallKind::Cancel => match result {
                Ok(_) => {
                    self.approvals.card = Card::Closed;
                    self.say(Tone::Info, "Effect cancelled");
                }
                Err(e) => self.card_failed(&e),
            },
            CallKind::Admit => match result {
                Ok(CallResult::Queued { .. }) => self.say(
                    Tone::Good,
                    "Admission signed and queued; it will be sent when the space's home is reachable",
                ),
                Ok(_) => self.say(Tone::Good, "Admitted as a member"),
                Err(e) => self.say(Tone::Bad, format!("Not admitted: {e}")),
            },
            CallKind::Steer => {
                if let Some(p) = &mut self.session {
                    p.delivery = Some(match result {
                        Ok(CallResult::Receipt { seq, .. }) => Delivery::Sent(seq),
                        Ok(CallResult::Queued { .. }) => Delivery::Queued,
                        Ok(_) => Delivery::Sent(0),
                        Err(e) => Delivery::Refused(e.to_string()),
                    });
                }
            }
        }
        Vec::new()
    }

    fn new_task(&mut self) -> Vec<Action> {
        let Some(space) = self.space.clone() else {
            self.say(
                Tone::Info,
                "Select a space with :space before creating a task",
            );
            return Vec::new();
        };
        if self.task_draft.is_none() {
            let row = self.board.selected_row();
            self.task_draft = Some(TaskDraft {
                space,
                fields: [
                    LineEdit::with(row.map_or("", |r| r.agent.as_str())),
                    LineEdit::with(row.and_then(|r| r.repo.as_deref()).unwrap_or("")),
                    LineEdit::default(),
                ],
                field: 0,
                pending: false,
                task: None,
                uncertain: false,
                notice: String::new(),
            });
        }
        Vec::new()
    }

    /// Prefill the ordinary task form after the launcher verified its agent and runner.
    /// This sends nothing; submission remains the user's explicit action.
    pub fn prepare_task(&mut self, agent: &str, repo: &str) {
        self.new_task();
        if let Some(draft) = &mut self.task_draft {
            draft.fields[0] = LineEdit::with(agent);
            draft.fields[1] = LineEdit::with(repo);
            draft.field = 2;
        }
    }

    /// Open tasks known to this space. The query window is bounded to 1,000 entries.
    pub fn task_rows(&self) -> Vec<TaskRow> {
        self.tasks_mirror
            .rows()
            .into_iter()
            .filter_map(|r| match r {
                Row::Task(t) if t.open => Some(t),
                _ => None,
            })
            .collect()
    }

    fn open_tasks(&mut self) -> Vec<Action> {
        let Some(space) = self.space.clone() else {
            self.say(Tone::Info, "Select a space with :space first");
            return Vec::new();
        };
        self.tasks_open = true;
        self.tasks_notice = "Loading tasks…".into();
        vec![self.sub(
            ViewKind::Tasks,
            Query::Tasks {
                space,
                assignee: None,
                window: Window {
                    limit: 1000,
                    ..Window::default()
                },
            },
        )]
    }

    fn tasks_key(&mut self, k: KeyEvent) -> Vec<Action> {
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.tasks_open = false;
                return vec![self.unsub(ViewKind::Tasks)];
            }
            KeyCode::Char('n') => return self.new_task(),
            KeyCode::Char('r') => return self.open_tasks(),
            KeyCode::Down | KeyCode::Char('j') => {
                self.task_selection =
                    (self.task_selection + 1).min(self.task_rows().len().saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.task_selection = self.task_selection.saturating_sub(1)
            }
            KeyCode::Enter => {
                if let Some(task) = self.task_rows().get(self.task_selection) {
                    let repo = task
                        .external
                        .as_deref()
                        .and_then(|s| s.split_once('#'))
                        .map_or("", |(r, _)| r);
                    self.task_draft = Some(TaskDraft {
                        space: task.space.clone(),
                        fields: [LineEdit::with(task.to.first().map_or("", String::as_str)),
                            LineEdit::with(repo), LineEdit::with(task.ask.as_deref().unwrap_or("Existing task"))],
                        field: 1, pending: false, task: Some(task.task.clone()), uncertain: false,
                        notice: "Choose an assigned agent and repository, then Enter starts this existing task.".into(),
                    });
                }
            }
            _ => {}
        }
        Vec::new()
    }

    fn submit_task(&mut self) -> Vec<Action> {
        let Some(draft) = &mut self.task_draft else {
            return Vec::new();
        };
        if draft.pending || draft.uncertain {
            return Vec::new();
        }
        if self.conn != Conn::Up {
            draft.notice =
                "Reconnect to the node before submitting; your request is kept here.".into();
            return Vec::new();
        }
        if draft.fields.iter().any(|f| f.text().trim().is_empty()) {
            draft.notice = "Fill in the agent, repository and request.".into();
            return Vec::new();
        }
        let agent = draft.fields[0].text().trim().to_owned();
        let repo = draft.fields[1].text().trim().to_owned();
        let ask = draft.fields[2].text().to_owned();
        let space = draft.space.clone();
        let task = draft.task.clone();
        draft.pending = true;
        draft.notice = if task.is_some() {
            "Starting agent; confirm any system authentication prompt…"
        } else {
            "Creating task…"
        }
        .into();
        let (kind, command) = match task {
            Some(task) => (
                CallKind::StartTask,
                Command::AgentStart {
                    space,
                    agent,
                    harness: None,
                    repo: Some(repo),
                    issue: None,
                    task: Some(task),
                    home: None,
                },
            ),
            None => (
                CallKind::CreateTask,
                Command::TaskCreate {
                    space,
                    to: vec![agent],
                    ask,
                    external: None,
                },
            ),
        };
        vec![self.call(kind, command)]
    }

    fn task_key(&mut self, k: KeyEvent) -> Vec<Action> {
        let draft = self.task_draft.as_mut().expect("open task form");
        if draft.pending {
            return Vec::new();
        }
        match k.code {
            KeyCode::Esc => self.task_draft = None,
            KeyCode::Enter => return self.submit_task(),
            KeyCode::Tab => draft.field = (draft.field + 1) % 3,
            KeyCode::BackTab => draft.field = (draft.field + 2) % 3,
            _ if (draft.task.is_none() || draft.field < 2) && !draft.uncertain => {
                draft.fields[draft.field].key(k);
            }
            _ => {}
        }
        Vec::new()
    }

    /// `:admit <actor>`: makes an actor that waits to join a member. Only a listed admission is admitted, and an
    /// agent whose operator has not confirmed it is not.
    fn admit(&mut self, actor: &str) -> Vec<Action> {
        let found = self
            .approvals
            .admissions()
            .iter()
            .find(|a| a.actor == actor)
            .cloned();
        let Some(a) = found else {
            self.say(Tone::Info, format!("{actor} is not waiting to join"));
            return Vec::new();
        };
        if !a.operator_confirmed {
            self.say(
                Tone::Bad,
                "Its operator has not confirmed it, so it cannot be admitted",
            );
            return Vec::new();
        }
        vec![self.call(
            CallKind::Admit,
            Command::MemberSet {
                space: a.space,
                actor: a.actor,
                role: "member".into(),
            },
        )]
    }

    fn card_failed(&mut self, e: &NodeError) {
        let (text, retry) = ApprovalsState::describe_error(e);
        match self.approvals.card.clone() {
            Card::Signing { key, digest, .. } => {
                self.approvals.card = Card::Signing {
                    key,
                    digest,
                    busy: false,
                };
            }
            Card::Cancelling { key, .. } => {
                self.approvals.card = Card::Cancelling {
                    key,
                    keep: true,
                    busy: false,
                };
            }
            _ => {}
        }
        self.approvals.notice = Some((Tone::Bad, text));
        self.approvals.retry = retry;
    }

    // ---- keys -----------------------------------------------------------------------------------------

    fn on_key(&mut self, k: KeyEvent) -> Vec<Action> {
        if k.kind == KeyEventKind::Release {
            return Vec::new();
        }
        if ctrl(&k, 'c') {
            self.quit = true;
            return vec![Action::Quit];
        }
        if k.code == KeyCode::Enter
            && let Some(space) = self
                .setup
                .as_ref()
                .filter(|s| s.existing && s.step == crate::setup::Step::Ready)
                .and_then(|s| s.ready_space.as_deref())
            && let Some(why) = self.existing_setup_error(space)
        {
            if let Some(setup) = &mut self.setup {
                setup.notice = why.into();
            }
            return Vec::new();
        }
        if let Some(setup) = &mut self.setup {
            if self.guided_setup
                && setup.step == crate::setup::Step::Ready
                && k.code == KeyCode::Enter
                && let Some(space) = setup.ready_space.clone()
            {
                self.setup_continue = Some(space);
                self.quit = true;
                return Vec::new();
            }
            if k.code == KeyCode::Esc && setup.step != crate::setup::Step::Waiting {
                self.setup = None;
                return Vec::new();
            }
            if let Some(command) = setup.key(k) {
                if self.conn != Conn::Up {
                    setup.step = crate::setup::Step::Preview;
                    setup.notice =
                        "Waiting for the node connection. Enter retries when connected.".into();
                    return Vec::new();
                }
                self.config.setup_recovery = Some("A previous setup operation may have reached the home. Inspect :space and the outbox before starting another. After inspection, :setup-reset clears this notice.".into());
                let Action::Call { id, command } = self.call(CallKind::Setup, command) else {
                    unreachable!()
                };
                return vec![Action::SetupCall {
                    id,
                    command,
                    config: self.config.clone(),
                }];
            }
            return Vec::new();
        }
        if self.task_draft.is_some() {
            return self.task_key(k);
        }
        if self.tasks_open {
            return self.tasks_key(k);
        }
        if self.help || self.overlap_detail {
            self.help = false;
            self.overlap_detail = false;
            return Vec::new();
        }
        match self.mode.clone() {
            Mode::Palette(e) => return self.palette_key(e, k),
            Mode::Filter(e) => return self.filter_key(e, k),
            Mode::Normal => {}
        }
        if self.focus == Focus::Session && self.session.as_ref().is_some_and(|s| s.steer.is_some())
        {
            return self.steer_key(k);
        }
        // A card is modal: no palette or pane switching while it is open.
        let card_open = self.focus == Focus::Approvals && self.approvals.card != Card::Closed;
        if !card_open {
            match k.code {
                KeyCode::Char(':') => {
                    self.mode = Mode::Palette(LineEdit::default());
                    return Vec::new();
                }
                KeyCode::Char('p') if ctrl(&k, 'p') => {
                    self.mode = Mode::Palette(LineEdit::default());
                    return Vec::new();
                }
                KeyCode::Char('?') => {
                    self.help = true;
                    return Vec::new();
                }
                KeyCode::Tab => {
                    self.cycle_focus(true);
                    return Vec::new();
                }
                KeyCode::BackTab => {
                    self.cycle_focus(false);
                    return Vec::new();
                }
                _ => {}
            }
        }
        let g = std::mem::take(&mut self.pending_g);
        match self.focus {
            Focus::Board => self.board_key(k, g),
            Focus::Session => self.session_key(k, g),
            Focus::Approvals => self.approvals_key(k, g),
        }
    }

    fn cycle_focus(&mut self, forward: bool) {
        let mut order = vec![Focus::Board];
        if self.session.is_some() {
            order.push(Focus::Session);
        }
        if self.approvals_open {
            order.push(Focus::Approvals);
        }
        let at = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        let n = order.len();
        self.focus = order[if forward {
            (at + 1) % n
        } else {
            (at + n - 1) % n
        }];
    }

    fn page(&self) -> isize {
        self.board.page.get().saturating_sub(1).max(1) as isize
    }

    fn board_key(&mut self, k: KeyEvent, g: bool) -> Vec<Action> {
        let moved = |this: &mut Self, nav: Nav| -> Vec<Action> {
            if this.board.nav(nav) {
                this.board_sub()
            } else {
                Vec::new()
            }
        };
        match k.code {
            KeyCode::Char('j') | KeyCode::Down => moved(self, Nav::Delta(1)),
            KeyCode::Char('k') | KeyCode::Up => moved(self, Nav::Delta(-1)),
            KeyCode::Char('g') if g => moved(self, Nav::First),
            KeyCode::Char('g') => {
                self.pending_g = true;
                Vec::new()
            }
            KeyCode::Home => moved(self, Nav::First),
            KeyCode::Char('G') | KeyCode::End => moved(self, Nav::Last),
            KeyCode::PageDown => {
                let p = self.page();
                moved(self, Nav::Delta(p))
            }
            KeyCode::Char('d') if ctrl(&k, 'd') => {
                let p = self.page() / 2;
                moved(self, Nav::Delta(p))
            }
            KeyCode::PageUp => {
                let p = self.page();
                moved(self, Nav::Delta(-p))
            }
            KeyCode::Char('u') if ctrl(&k, 'u') => {
                let p = self.page() / 2;
                moved(self, Nav::Delta(-p))
            }
            KeyCode::Enter => self.board_enter(),
            KeyCode::Char('n') => self.new_task(),
            KeyCode::Char('T') => self.open_tasks(),
            KeyCode::Char(' ') => {
                if self.board.toggle_group() {
                    self.board_sub()
                } else {
                    Vec::new()
                }
            }
            KeyCode::Char('h') | KeyCode::Left => self.board_left(),
            KeyCode::Char('l') | KeyCode::Right => self.board_right(),
            KeyCode::Char('o') => {
                match self.board.selected_row() {
                    Some(r) if !r.overlap_with.is_empty() => self.overlap_detail = true,
                    Some(r) if !r.overlaps.is_empty() => {
                        let words = BoardState::overlap_words(&r.overlaps);
                        let text = format!("{} overlaps: {}", r.agent, words);
                        self.say(Tone::Info, text);
                    }
                    _ => self.say(Tone::Info, "no overlap on this session"),
                }
                Vec::new()
            }
            KeyCode::Char('a') => {
                self.open_approvals_for_selection();
                Vec::new()
            }
            KeyCode::Char('t') => self.cycle_group(),
            KeyCode::Char('/') => {
                self.mode = Mode::Filter(LineEdit::with(&self.board.filter_text));
                Vec::new()
            }
            KeyCode::Char('x') => {
                if self.board.is_filtered() {
                    self.apply_filter("")
                } else {
                    Vec::new()
                }
            }
            KeyCode::Char('c') => {
                let on = !self.board.include_closed();
                self.board.set_include_closed(on);
                self.say(
                    Tone::Info,
                    if on {
                        "showing closed sessions"
                    } else {
                        "hiding closed sessions"
                    },
                );
                self.board_sub()
            }
            KeyCode::Char('r') => self.resubscribe_all(),
            KeyCode::Char('q') => {
                self.quit = true;
                vec![Action::Quit]
            }
            KeyCode::Esc if self.board.is_filtered() => self.apply_filter(""),
            _ => Vec::new(),
        }
    }

    fn board_enter(&mut self) -> Vec<Action> {
        match self.board.selected().cloned() {
            Some(EntryId::Session(_)) => {
                let Some(row) = self.board.selected_row().cloned() else {
                    return Vec::new();
                };
                self.open_session(&row)
            }
            Some(EntryId::Group(_)) => {
                if self.board.toggle_group() {
                    self.board_sub()
                } else {
                    Vec::new()
                }
            }
            None => Vec::new(),
        }
    }

    fn board_left(&mut self) -> Vec<Action> {
        match self.board.selected().cloned() {
            Some(EntryId::Session(s)) => {
                let Some(group) = self.board.row_of(&s).map(|r| r.group.clone()) else {
                    return Vec::new();
                };
                self.board.select_header(&group);
                Vec::new()
            }
            Some(EntryId::Group(g)) => {
                let collapsed = self.board.entries().iter().any(
                    |e| matches!(e, Entry::Group { group, collapsed: true, .. } if *group == g),
                );
                if collapsed || !self.board.toggle_group() {
                    Vec::new()
                } else {
                    self.board_sub()
                }
            }
            None => Vec::new(),
        }
    }

    fn board_right(&mut self) -> Vec<Action> {
        let Some(EntryId::Group(g)) = self.board.selected().cloned() else {
            return Vec::new();
        };
        let collapsed = self
            .board
            .entries()
            .iter()
            .any(|e| matches!(e, Entry::Group { group, collapsed: true, .. } if *group == g));
        if collapsed {
            if self.board.toggle_group() {
                return self.board_sub();
            }
        } else {
            self.board.nav(Nav::Delta(1));
        }
        Vec::new()
    }

    fn cycle_group(&mut self) -> Vec<Action> {
        let next = match self.board.group {
            Group::Issue => Group::Person,
            Group::Person => Group::State,
            Group::State => Group::Issue,
        };
        self.set_group(next)
    }

    fn set_group(&mut self, group: Group) -> Vec<Action> {
        self.board.set_group(group);
        let mut out = Vec::new();
        if let Some(space) = self.space.clone() {
            self.config.groups.insert(space, group);
            out.push(Action::SaveConfig(self.config.clone()));
        }
        self.say(Tone::Info, format!("grouped by {}", group_name(group)));
        out.extend(self.board_sub());
        out
    }

    fn apply_filter(&mut self, text: &str) -> Vec<Action> {
        if self.board.set_filter(text) {
            self.board_sub()
        } else {
            Vec::new()
        }
    }

    fn open_approvals_for_selection(&mut self) {
        self.approvals_open = true;
        self.focus = Focus::Approvals;
        if let Some(agent) = self.board.selected_row().map(|r| r.agent.clone()) {
            self.approvals.select_by_proposer(&agent);
        }
    }

    fn open_session(&mut self, row: &BoardRow) -> Vec<Action> {
        let mut out = Vec::new();
        if self.session.is_some() {
            out.extend(self.close_session_actions());
        }
        self.session = Some(SessionPane::open(row));
        self.focus = Focus::Session;
        out.extend(self.session_subs());
        out
    }

    fn close_session_actions(&mut self) -> Vec<Action> {
        self.session = None;
        [
            ViewKind::Session,
            ViewKind::Inputs,
            ViewKind::Items,
            ViewKind::Timeline,
            ViewKind::Patches,
        ]
        .into_iter()
        .map(|v| self.unsub(v))
        .collect()
    }

    fn session_key(&mut self, k: KeyEvent, g: bool) -> Vec<Action> {
        let Some(p) = self.session.as_mut() else {
            self.focus = Focus::Board;
            return Vec::new();
        };
        let lines = p.log.len();
        let page = self.size.1.saturating_sub(10).max(3) as usize;
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.focus = Focus::Board;
                self.close_session_actions()
            }
            KeyCode::Char('j') | KeyCode::Down => {
                p.scroll(1, lines, page);
                Vec::new()
            }
            KeyCode::Char('k') | KeyCode::Up => {
                p.scroll(-1, lines, page);
                Vec::new()
            }
            KeyCode::PageDown => {
                p.scroll(page as isize, lines, page);
                Vec::new()
            }
            KeyCode::PageUp => {
                p.scroll(-(page as isize), lines, page);
                Vec::new()
            }
            KeyCode::Char('g') if g => {
                p.scroll(-(lines as isize), lines, page);
                Vec::new()
            }
            KeyCode::Char('g') => {
                self.pending_g = true;
                Vec::new()
            }
            KeyCode::Char('G') | KeyCode::End => {
                p.follow = true;
                p.top = 0;
                Vec::new()
            }
            KeyCode::Char('1') => {
                p.tab = Tab::Stream;
                Vec::new()
            }
            KeyCode::Char('2') => {
                p.tab = Tab::Timeline;
                p.top = 0;
                Vec::new()
            }
            KeyCode::Char('3') => {
                p.tab = Tab::Changes;
                p.top = 0;
                Vec::new()
            }
            KeyCode::Char(']') => {
                p.tab = p.tab.next();
                p.top = 0;
                Vec::new()
            }
            KeyCode::Char('[') => {
                p.tab = p.tab.next().next();
                p.top = 0;
                Vec::new()
            }
            KeyCode::Char('n') | KeyCode::Char('N') if p.tab == Tab::Changes => {
                let count = match &p.patches {
                    crate::session::Panel::Ready(f) => f.len(),
                    _ => 0,
                };
                if count > 0 {
                    p.file = if k.code == KeyCode::Char('n') {
                        (p.file + 1) % count
                    } else {
                        (p.file + count - 1) % count
                    };
                    p.top = 0;
                }
                Vec::new()
            }
            KeyCode::Char('i') | KeyCode::Enter => {
                if p.can_steer() {
                    p.steer = Some(LineEdit::default());
                    p.delivery = None;
                } else {
                    self.say(
                        Tone::Info,
                        "watch only: you do not have the right to steer this session",
                    );
                }
                Vec::new()
            }
            KeyCode::Char('a') => {
                if let Some(agent) = p.hint.as_ref().map(|h| h.agent.clone()) {
                    self.approvals_open = true;
                    self.focus = Focus::Approvals;
                    self.approvals.select_by_proposer(&agent);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn steer_key(&mut self, k: KeyEvent) -> Vec<Action> {
        let Some(p) = self.session.as_mut() else {
            return Vec::new();
        };
        match k.code {
            KeyCode::Esc => {
                p.steer = None;
                Vec::new()
            }
            KeyCode::Enter => {
                let text = p
                    .steer
                    .as_ref()
                    .map(|e| e.text().trim().to_owned())
                    .unwrap_or_default();
                if text.is_empty() {
                    return Vec::new();
                }
                if !p.can_steer() {
                    p.steer = None;
                    return Vec::new();
                }
                p.steer = None;
                p.delivery = Some(Delivery::Sending);
                p.follow = true;
                let (space, session) = (p.space.clone(), p.id.clone());
                vec![self.call(
                    CallKind::Steer,
                    Command::SessionInput {
                        space,
                        session,
                        text,
                    },
                )]
            }
            _ => {
                if let Some(e) = p.steer.as_mut() {
                    e.key(k);
                }
                Vec::new()
            }
        }
    }

    fn approvals_key(&mut self, k: KeyEvent, g: bool) -> Vec<Action> {
        match self.approvals.card.clone() {
            Card::Closed => match k.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    self.approvals.nav(1);
                    Vec::new()
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.approvals.nav(-1);
                    Vec::new()
                }
                KeyCode::Char('g') if g => {
                    self.approvals.nav_to(false);
                    Vec::new()
                }
                KeyCode::Char('g') => {
                    self.pending_g = true;
                    Vec::new()
                }
                KeyCode::Char('G') => {
                    self.approvals.nav_to(true);
                    Vec::new()
                }
                // `a` and Enter only open the full card; neither signs.
                KeyCode::Enter | KeyCode::Char('a' | ' ') => {
                    self.approvals.open_card();
                    Vec::new()
                }
                KeyCode::Char('s') => {
                    let agent = self.approvals.selected().map(|r| r.proposer.clone());
                    let row = agent
                        .and_then(|a| self.board.rows().iter().find(|r| r.agent == a).cloned());
                    match row {
                        Some(r) => {
                            self.board.select_session(&r.session);
                            self.open_session(&r)
                        }
                        None => {
                            self.say(Tone::Info, "that session is not in the board's window");
                            Vec::new()
                        }
                    }
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.approvals_open = false;
                    self.focus = if self.session.is_some() {
                        Focus::Session
                    } else {
                        Focus::Board
                    };
                    Vec::new()
                }
                _ => Vec::new(),
            },
            Card::Open { key, digest } => match k.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.approvals.card = Card::Closed;
                    self.approvals.notice = None;
                    Vec::new()
                }
                // The second step: `a` on the full card opens the sign panel.
                KeyCode::Char('a') => {
                    match self.approval_block_reason() {
                        Some(reason) => self.approvals.notice = Some((Tone::Bad, reason)),
                        None => {
                            self.approvals.card = Card::Signing {
                                key,
                                digest,
                                busy: false,
                            };
                            self.approvals.notice = None;
                        }
                    }
                    Vec::new()
                }
                KeyCode::Char('c') => {
                    if self.approvals.can_cancel() {
                        self.approvals.card = Card::Cancelling {
                            key,
                            keep: true,
                            busy: false,
                        };
                        self.approvals.notice = None;
                    } else {
                        self.approvals.notice = Some((
                            Tone::Bad,
                            "Only the proposer or an owner can cancel this effect.".to_owned(),
                        ));
                    }
                    Vec::new()
                }
                // Enter on the card never signs (APR-5).
                _ => Vec::new(),
            },
            Card::Signing { key, digest, busy } => match k.code {
                _ if busy => Vec::new(),
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.approvals.card = Card::Open { key, digest };
                    self.approvals.notice = None;
                    Vec::new()
                }
                KeyCode::Enter => {
                    if let Some(reason) = self.approval_block_reason() {
                        self.approvals.notice = Some((Tone::Bad, reason));
                        return Vec::new();
                    }
                    if self.approvals.notice.is_some() && !self.approvals.retry {
                        return Vec::new();
                    }
                    let Some(row) = self.approvals.card_row().cloned() else {
                        return Vec::new();
                    };
                    self.approvals.card = Card::Signing {
                        key,
                        digest,
                        busy: true,
                    };
                    self.approvals.notice = None;
                    vec![self.call(
                        CallKind::Approve,
                        Command::EffectApprove {
                            space: row.space,
                            proposal: row.proposal,
                            expires_at: None,
                            note: None,
                            evidence: None,
                        },
                    )]
                }
                _ => Vec::new(),
            },
            Card::Cancelling { key, keep, busy } => match k.code {
                _ if busy => Vec::new(),
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.approvals.card = Card::Closed;
                    self.approvals.notice = None;
                    Vec::new()
                }
                KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::Char('h' | 'l') => {
                    self.approvals.card = Card::Cancelling {
                        key,
                        keep: !keep,
                        busy,
                    };
                    Vec::new()
                }
                KeyCode::Enter if keep => {
                    self.approvals.card = Card::Closed;
                    Vec::new()
                }
                KeyCode::Enter => {
                    let Some(row) = self.approvals.card_row().cloned() else {
                        return Vec::new();
                    };
                    self.approvals.card = Card::Cancelling {
                        key,
                        keep,
                        busy: true,
                    };
                    self.approvals.notice = None;
                    vec![self.call(
                        CallKind::Cancel,
                        Command::EffectCancel {
                            space: row.space,
                            proposal: row.proposal,
                            reason: None,
                            evidence: None,
                        },
                    )]
                }
                _ => Vec::new(),
            },
        }
    }

    fn filter_key(&mut self, mut e: LineEdit, k: KeyEvent) -> Vec<Action> {
        match k.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                Vec::new()
            }
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                let text = e.text().to_owned();
                self.apply_filter(&text)
            }
            _ => {
                e.key(k);
                self.mode = Mode::Filter(e);
                Vec::new()
            }
        }
    }

    fn palette_key(&mut self, mut e: LineEdit, k: KeyEvent) -> Vec<Action> {
        match k.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                Vec::new()
            }
            KeyCode::Backspace if e.is_empty() => {
                self.mode = Mode::Normal;
                Vec::new()
            }
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                let line = e.text().to_owned();
                self.run_command(&line)
            }
            _ => {
                e.key(k);
                self.mode = Mode::Palette(e);
                Vec::new()
            }
        }
    }

    fn existing_setup_error(&self, space: &str) -> Option<&'static str> {
        if !self.guided_setup {
            return Some(
                "Open han setup with this data folder to continue agent and runner setup.",
            );
        }
        if self.conn != Conn::Up || self.space.as_deref() != Some(space) {
            return Some(
                "Reconnect and explicitly select the intended team with :space before continuing.",
            );
        }
        if !self
            .node
            .as_ref()
            .is_some_and(|n| n.outbox_pending == 0 && n.problems.is_empty())
        {
            return Some(
                "Wait for pending writes and resolve node problems before continuing setup.",
            );
        }
        let Some(status) = self.space_status.as_ref().filter(|s| s.space == space) else {
            return Some("Wait for the selected team's current status before continuing.");
        };
        if status.link != han_node::api::SpaceLink::Live || status.outbox_pending != 0 {
            return Some(
                "The selected team must be live with no pending writes before continuing.",
            );
        }
        if !status.trusted_setup.as_ref().is_some_and(|t| t.can_manage) {
            return Some(
                "Guided runner setup needs current ownership and the unchanged Trusted team policy.",
            );
        }
        None
    }

    fn existing_setup(&mut self) -> Vec<Action> {
        let Some(space) = self.space.clone() else {
            self.say(
                Tone::Bad,
                "Select the existing team with :space <space id> first.",
            );
            return Vec::new();
        };
        if let Some(why) = self.existing_setup_error(&space) {
            self.say(Tone::Bad, why);
            return Vec::new();
        }
        let status = self.space_status.as_ref().expect("eligibility checked");
        let mut setup = crate::setup::Setup::new(self.config.setup_recovery.as_deref());
        setup.step = crate::setup::Step::Ready;
        setup.ready_space = Some(space);
        setup.existing = true;
        setup.notice = format!(
            "Existing team on {}. Repositories: {}. The launcher rechecks eligibility before configuring anything.",
            status.home,
            status
                .trusted_setup
                .as_ref()
                .expect("eligibility checked")
                .repos
                .join(", ")
        );
        self.setup = Some(setup);
        Vec::new()
    }

    /// Runs a line of the `:` palette.
    pub fn run_command(&mut self, line: &str) -> Vec<Action> {
        let line = line.trim().trim_start_matches(':');
        let (cmd, arg) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let arg = arg.trim();
        match cmd {
            "setup" => {
                self.setup = Some(crate::setup::Setup::new(
                    self.config.setup_recovery.as_deref(),
                ));
                Vec::new()
            }
            "setup-existing" => self.existing_setup(),
            "setup-reset" => {
                self.config.setup_recovery = None;
                self.setup = Some(crate::setup::Setup::new(None));
                vec![Action::SaveConfig(self.config.clone())]
            }
            "task" | "new" => self.new_task(),
            "tasks" => self.open_tasks(),
            "" => Vec::new(),
            "q" | "quit" | "exit" => {
                self.quit = true;
                vec![Action::Quit]
            }
            "help" | "h" | "?" => {
                self.help = true;
                Vec::new()
            }
            "group" => {
                let next = match arg {
                    "issue" => Group::Issue,
                    "person" => Group::Person,
                    "state" => Group::State,
                    "" | "next" => return self.cycle_group(),
                    other => {
                        self.say(
                            Tone::Bad,
                            format!("unknown grouping '{other}' (issue, person, state)"),
                        );
                        return Vec::new();
                    }
                };
                self.set_group(next)
            }
            "filter" | "f" => self.apply_filter(arg),
            "clear" => self.apply_filter(""),
            "closed" => {
                let on = match arg {
                    "off" | "no" | "false" => false,
                    "" => !self.board.include_closed(),
                    _ => true,
                };
                self.board.set_include_closed(on);
                self.board_sub()
            }
            "space" | "s" => {
                if arg.is_empty() {
                    let mut text = format!("space: {}", self.space.as_deref().unwrap_or("none"));
                    if !self.spaces.is_empty() {
                        let names: Vec<&str> =
                            self.spaces.iter().map(|s| s.space.as_str()).collect();
                        text = format!("{text}. Followed: {}", names.join(", "));
                    }
                    self.say(Tone::Info, text);
                    return Vec::new();
                }
                // A part of a followed space's id is enough when it names only one.
                let mut found = self.spaces.iter().filter(|s| s.space.contains(arg));
                let pick = match (found.next(), found.next()) {
                    (Some(only), None) => only.space.clone(),
                    _ => arg.to_owned(),
                };
                self.set_space(pick)
            }
            "approvals" | "inbox" => {
                self.approvals_open = true;
                self.focus = Focus::Approvals;
                Vec::new()
            }
            "admit" => self.admit(arg),
            "board" => {
                self.focus = Focus::Board;
                Vec::new()
            }
            "collapse" | "expand" | "toggle" => {
                if self.board.toggle_group() {
                    self.board_sub()
                } else {
                    Vec::new()
                }
            }
            "reconnect" | "refresh" | "reload" => self.resubscribe_all(),
            other => {
                self.say(
                    Tone::Bad,
                    format!("unknown command '{other}' (:help lists the keys)"),
                );
                Vec::new()
            }
        }
    }

    fn set_space(&mut self, space: String) -> Vec<Action> {
        let mut out = Vec::new();
        self.tasks_open = false;
        self.tasks_mirror = han_node::api::ViewMirror::new();
        self.task_selection = 0;
        out.push(self.unsub(ViewKind::Tasks));
        if self.session.is_some() {
            out.extend(self.close_session_actions());
            self.focus = Focus::Board;
        }
        self.board = BoardState::new(self.config.group_of(&space));
        self.board
            .set_limit((u32::from(self.size.1) * 3).clamp(50, 500));
        self.space_status = None;
        self.space = Some(space.clone());
        self.config.last_space = Some(space.clone());
        out.push(Action::SaveConfig(self.config.clone()));
        out.extend(self.board_sub());
        out.push(self.sub(
            ViewKind::Space,
            Query::SpaceStatus {
                space,
                window: Window::default(),
            },
        ));
        out
    }

    /// The count of sessions that need the local user, from the board's window.
    pub fn needs_me(&self) -> usize {
        self.board.rows().iter().filter(|r| r.needs_me).count()
    }

    /// Whether the open session's state is waiting for an approval.
    pub fn session_waits(&self) -> bool {
        self.session
            .as_ref()
            .and_then(|s| s.state())
            .is_some_and(|s| s == SessionState::WaitingApproval)
    }
}

/// A grouping's name.
pub fn group_name(g: Group) -> &'static str {
    match g {
        Group::Issue => "issue",
        Group::Person => "person",
        Group::State => "state",
    }
}

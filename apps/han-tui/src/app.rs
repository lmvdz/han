//! The TUI as a pure state machine: [`App::update`] takes a [`Msg`] (a key, a view event, a call result, the
//! clock) and returns the [`Action`]s to carry out. Nothing here touches the terminal or the network, so the
//! whole thing is tested with a fake node and a `TestBackend`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use han_node::api::{
    BoardRow, CallResult, Command, Group, NodeError, NodeStatusRow, Query, Row, SessionState,
    SpaceStatusRow, ViewEvent, Window,
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
    Approve,
    Cancel,
    Steer,
}

/// The whole state of the TUI.
pub struct App {
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
    /// The transient message.
    pub toast: Option<Toast>,
    /// Terminal size, columns and rows.
    pub size: (u16, u16),
    /// The clock, as of the last tick.
    pub now: Instant,
    /// Whether the program should end.
    pub quit: bool,
    spaces_mirror: han_node::api::ViewMirror,
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
    /// A new app. `space` is the space to show (else the one last shown, else none).
    pub fn new(config: Config, space: Option<String>, now: Instant) -> App {
        let space = space.or_else(|| config.last_space.clone());
        let group = space
            .as_deref()
            .map_or(Group::Issue, |s| config.group_of(s));
        App {
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
            help: false,
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
        Action::Subscribe { view, epoch, query }
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
            Msg::Called { id, result } => {
                self.on_called(id, result);
                Vec::new()
            }
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
                self.me = Some(row.actor.clone());
                self.node = Some(row);
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

    fn on_called(&mut self, id: u64, result: Result<CallResult, NodeError>) {
        let Some(kind) = self.calls.remove(&id) else {
            return;
        };
        match kind {
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
        if self.help {
            self.help = false;
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
        let me = self.me.clone();
        self.board.set_filter(text, me.as_deref());
        self.board_sub()
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
        let me = self.me.clone();
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
                    match self.approvals.block_reason(me.as_deref()) {
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
                    if let Some(reason) = self.approvals.block_reason(me.as_deref()) {
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
                            note: None,
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

    /// Runs a line of the `:` palette.
    pub fn run_command(&mut self, line: &str) -> Vec<Action> {
        let line = line.trim().trim_start_matches(':');
        let (cmd, arg) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let arg = arg.trim();
        match cmd {
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

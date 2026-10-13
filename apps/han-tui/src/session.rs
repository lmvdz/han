//! The session pane: one session's live stream (what its status says it is doing, and the steering inputs sent
//! to it), its work timeline and its changes, and the steering field.
//!
//! The stream is built from what the node gives for a session: the status of its host and the `session.input`
//! entries. Each change of the status becomes one line of the stream, so the pane reads like a log of steps
//! and tool calls. The timeline and the changes come from `Timeline` and `Patches`, which the node answers
//! `unavailable` until the timeline engine backs them: the pane then says "pending M4".

use std::collections::HashSet;

use han_node::api::{
    BoardRow, FilePatchRow, InputRow, NodeError, Row, SessionRow, SessionState, StatusBody,
    TimelinePointRow, ViewEvent, ViewMirror,
};

use crate::input::LineEdit;

/// A tab of the pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    /// Steps, tool calls and steering.
    Stream,
    /// The points of the work timeline.
    Timeline,
    /// The patches of the latest point.
    Changes,
}

impl Tab {
    /// The next tab.
    pub fn next(self) -> Tab {
        match self {
            Tab::Stream => Tab::Timeline,
            Tab::Timeline => Tab::Changes,
            Tab::Changes => Tab::Stream,
        }
    }

    /// The tab's name.
    pub fn name(self) -> &'static str {
        match self {
            Tab::Stream => "Stream",
            Tab::Timeline => "Timeline",
            Tab::Changes => "Changes",
        }
    }
}

/// What a line of the stream is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogKind {
    /// The pane was opened on the session.
    Opened,
    /// The state changed.
    State,
    /// The step changed.
    Step,
    /// A tool call began.
    Tool,
    /// A plan entry was ticked off.
    Plan,
    /// The tests changed.
    Tests,
    /// The agent's summary changed.
    Summary,
    /// A steering input.
    Steer,
}

/// A line of the stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogLine {
    /// What kind of line.
    pub kind: LogKind,
    /// The text (still to be cleaned for the terminal).
    pub text: String,
}

/// A panel fed by a view that may not be served yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Panel<T> {
    /// Waiting for the first snapshot.
    Loading,
    /// The rows.
    Ready(T),
    /// The node cannot serve it yet (`unavailable`): shown as "pending M4".
    Pending(String),
    /// The view failed for another reason.
    Failed(String),
}

impl<T> Panel<T> {
    fn from_error(e: &NodeError) -> Self {
        match e {
            NodeError::Unavailable { message, .. } => Panel::Pending(message.clone()),
            other => Panel::Failed(other.to_string()),
        }
    }
}

/// How a steering input went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// The call is in flight.
    Sending,
    /// The home recorded it, at this sequence number.
    Sent(u64),
    /// The node kept it in its outbox for the home to take later.
    Queued,
    /// The node or the space refused it.
    Refused(String),
}

/// The open session.
#[derive(Debug)]
pub struct SessionPane {
    /// The session's entry id.
    pub id: String,
    /// The space.
    pub space: String,
    /// The session as the node has it.
    pub row: Option<SessionRow>,
    /// The board row the pane was opened from, kept current while the session stays in the board's window. It
    /// carries the rights (`can_steer`).
    pub hint: Option<BoardRow>,
    /// The stream.
    pub log: Vec<LogLine>,
    /// The timeline.
    pub timeline: Panel<Vec<TimelinePointRow>>,
    /// The changes.
    pub patches: Panel<Vec<FilePatchRow>>,
    /// The tab.
    pub tab: Tab,
    /// The first line shown when not following (stream) or the scroll offset (other tabs).
    pub top: usize,
    /// Whether the stream sticks to its live end.
    pub follow: bool,
    /// Which file of the changes is selected.
    pub file: usize,
    /// The steering field, while it is being edited.
    pub steer: Option<LineEdit>,
    /// How the last steering input went.
    pub delivery: Option<Delivery>,
    /// Why the session view failed.
    pub error: Option<NodeError>,
    /// Whether the first snapshot of the session has arrived.
    pub synced: bool,
    /// The key of the latest timeline point, once the timeline has any.
    pub latest: Option<String>,
    inputs: HashSet<String>,
    inputs_seeded: bool,
    timeline_mirror: ViewMirror,
    patches_mirror: ViewMirror,
    session_mirror: ViewMirror,
    inputs_mirror: ViewMirror,
}

impl SessionPane {
    /// A pane on a session, opened from a board row.
    pub fn open(hint: &BoardRow) -> Self {
        SessionPane {
            id: hint.session.clone(),
            space: hint.space.clone(),
            row: None,
            hint: Some(hint.clone()),
            log: Vec::new(),
            timeline: Panel::Loading,
            patches: Panel::Loading,
            tab: Tab::Stream,
            top: 0,
            follow: true,
            file: 0,
            steer: None,
            delivery: None,
            error: None,
            synced: false,
            latest: None,
            inputs: HashSet::new(),
            inputs_seeded: false,
            timeline_mirror: ViewMirror::new(),
            patches_mirror: ViewMirror::new(),
            session_mirror: ViewMirror::new(),
            inputs_mirror: ViewMirror::new(),
        }
    }

    /// Whether the local user may steer this session.
    pub fn can_steer(&self) -> bool {
        self.hint.as_ref().is_some_and(|h| h.can_steer)
    }

    /// The session's state as the node derives it.
    pub fn state(&self) -> Option<SessionState> {
        self.row
            .as_ref()
            .map(|r| r.state)
            .or(self.hint.as_ref().map(|h| h.state))
    }

    fn push(&mut self, kind: LogKind, text: impl Into<String>) {
        self.log.push(LogLine {
            kind,
            text: text.into(),
        });
        // A long session does not grow without bound.
        if self.log.len() > 2000 {
            self.log.drain(..500);
        }
    }

    /// Applies an event of the `Session` view.
    pub fn apply_session(&mut self, event: ViewEvent) {
        if let ViewEvent::Failed { error } = event {
            self.error = Some(error);
            return;
        }
        self.session_mirror.apply(event);
        self.error = None;
        self.synced = true;
        let row = self
            .session_mirror
            .rows()
            .into_iter()
            .find_map(|r| match r {
                Row::Session(s) => Some(s),
                _ => None,
            });
        if let Some(new) = row {
            self.log_changes(self.row.clone().as_ref(), &new);
            self.row = Some(new);
        }
    }

    fn log_changes(&mut self, old: Option<&SessionRow>, new: &SessionRow) {
        let prev: Option<&StatusBody> = old.and_then(|o| o.status.as_ref());
        let Some(st) = new.status.as_ref() else {
            if old.is_none() {
                self.push(LogKind::Opened, format!("opened by {}", new.actor));
            }
            return;
        };
        if old.is_none() {
            self.push(
                LogKind::Opened,
                format!("{} on {} ({})", st.agent, st.harness, new.state.name()),
            );
        } else if old.is_some_and(|o| o.state != new.state) {
            self.push(
                LogKind::State,
                format!(
                    "state: {} -> {}",
                    old.map_or("?", |o| o.state.name()),
                    new.state.name()
                ),
            );
        }
        if prev.map(|p| p.step.as_str()) != Some(st.step.as_str()) {
            self.push(LogKind::Step, st.step.clone());
        }
        if let Some(tool) = &st.tool
            && prev.and_then(|p| p.tool.as_ref()) != Some(tool)
        {
            self.push(LogKind::Tool, tool.clone());
        }
        for item in st.plan.iter().filter(|i| i.done) {
            let was_done =
                prev.is_some_and(|p| p.plan.iter().any(|q| q.done && q.text == item.text));
            if !was_done && prev.is_some() {
                self.push(LogKind::Plan, format!("done: {}", item.text));
            }
        }
        if let Some(t) = st.tests
            && prev.and_then(|p| p.tests) != st.tests
        {
            self.push(
                LogKind::Tests,
                format!("tests: {} passed, {} failed", t.pass, t.fail),
            );
        }
        if let Some(s) = &st.summary
            && prev.and_then(|p| p.summary.as_ref()) != Some(s)
        {
            self.push(LogKind::Summary, s.clone());
        }
    }

    /// Applies an event of the `Inputs` view: steering inputs become lines of the stream.
    pub fn apply_inputs(&mut self, event: ViewEvent) {
        if matches!(event, ViewEvent::Failed { .. }) {
            return;
        }
        self.inputs_mirror.apply(event);
        let rows: Vec<InputRow> = self
            .inputs_mirror
            .rows()
            .into_iter()
            .filter_map(|r| match r {
                Row::Input(i) => Some(i),
                _ => None,
            })
            .collect();
        for i in rows {
            if self.inputs.insert(i.key.clone()) {
                let text = i.text.as_deref().unwrap_or("(text not available)");
                let line = format!("{} -> session: {}", i.author, text);
                self.push(LogKind::Steer, line);
            }
        }
        self.inputs_seeded = true;
    }

    /// Applies an event of the `Timeline` view. Returns the latest point, once there is one, so the changes can
    /// be asked for.
    pub fn apply_timeline(&mut self, event: ViewEvent) -> Option<String> {
        if let ViewEvent::Failed { error } = &event {
            self.timeline = Panel::from_error(error);
            return None;
        }
        self.timeline_mirror.apply(event);
        let points: Vec<TimelinePointRow> = self
            .timeline_mirror
            .rows()
            .into_iter()
            .filter_map(|r| match r {
                Row::TimelinePoint(p) => Some(p),
                _ => None,
            })
            .collect();
        let latest = points.last().map(|p| p.point.clone());
        self.timeline = Panel::Ready(points);
        latest
    }

    /// Applies an event of the `Patches` view.
    pub fn apply_patches(&mut self, event: ViewEvent) {
        if let ViewEvent::Failed { error } = &event {
            self.patches = Panel::from_error(error);
            return;
        }
        self.patches_mirror.apply(event);
        let files: Vec<FilePatchRow> = self
            .patches_mirror
            .rows()
            .into_iter()
            .filter_map(|r| match r {
                Row::FilePatch(p) => Some(p),
                _ => None,
            })
            .collect();
        self.file = self.file.min(files.len().saturating_sub(1));
        self.patches = Panel::Ready(files);
    }

    /// Scrolls the active tab by `delta` lines (negative: up). `lines` is the number of lines the stream has.
    pub fn scroll(&mut self, delta: isize, lines: usize, page: usize) {
        match self.tab {
            Tab::Stream => {
                let bottom = lines.saturating_sub(page);
                let from = if self.follow {
                    bottom
                } else {
                    self.top.min(bottom)
                };
                let to = (from as isize + delta).clamp(0, bottom as isize) as usize;
                self.top = to;
                self.follow = to >= bottom;
            }
            Tab::Timeline | Tab::Changes => {
                self.top = (self.top as isize + delta).max(0) as usize;
            }
        }
    }
}

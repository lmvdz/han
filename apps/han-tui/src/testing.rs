//! Tools for testing the TUI without a terminal or a node: a scriptable [`FakeApi`], fixtures for rows, and a
//! [`Harness`] that runs the real app and host against a `TestBackend` and renders the screen as text.
//!
//! This is public so that `han-node`'s own tests can drive the TUI against a real in-process node.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use han_node::NodeApi;
use han_node::api::{
    ApprovalRow, BoardRow, BoxFuture, CallResult, Command, Filter, NodeError, NodeStatusRow, Query,
    Row, SessionRow, SessionState, StatusBody, Tokens, ViewEvent, ViewSink, ViewStream, Window,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::config::Config;
use crate::host::{Connector, Runtime};
use crate::msg::Msg;
use crate::ui;

type Provider = Box<dyn Fn(&Query) -> Option<ViewEvent> + Send>;
type Replier = Box<dyn Fn(&Command) -> Result<CallResult, NodeError> + Send>;

struct FakeState {
    views: Vec<(Query, ViewSink, u64)>,
    opened: Vec<Query>,
    calls: Vec<Command>,
    dead: bool,
    provider: Provider,
    replier: Replier,
}

/// A node that serves what the test says. Every view starts with whatever the provider returns for its query
/// (an empty snapshot by default); `push` sends later events; `kill` is the daemon going away.
pub struct FakeApi {
    state: Mutex<FakeState>,
}

impl FakeApi {
    /// A node with empty views that accepts every command.
    pub fn new() -> Arc<FakeApi> {
        Arc::new(FakeApi {
            state: Mutex::new(FakeState {
                views: Vec::new(),
                opened: Vec::new(),
                calls: Vec::new(),
                dead: false,
                provider: Box::new(|_| Some(snapshot(vec![], 0, 0, None))),
                replier: Box::new(|_| Ok(CallResult::Done)),
            }),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Sets what a newly opened view starts with (`None`: it fails as unavailable).
    pub fn serve(&self, f: impl Fn(&Query) -> Option<ViewEvent> + Send + 'static) {
        self.lock().provider = Box::new(f);
    }

    /// Sets how calls end.
    pub fn answer(&self, f: impl Fn(&Command) -> Result<CallResult, NodeError> + Send + 'static) {
        self.lock().replier = Box::new(f);
    }

    /// The queries opened so far, oldest first.
    pub fn opened(&self) -> Vec<Query> {
        self.lock().opened.clone()
    }

    /// The commands received so far.
    pub fn calls(&self) -> Vec<Command> {
        self.lock().calls.clone()
    }

    /// Sends an event to every open view whose query matches.
    pub fn push(&self, matches: impl Fn(&Query) -> bool, event: ViewEvent) {
        let mut st = self.lock();
        st.views.retain(|(_, sink, _)| !sink.is_closed());
        for (q, sink, seq) in &mut st.views {
            if matches(q) {
                *seq += 1;
                sink.send(event.clone().with_seq(*seq));
            }
        }
    }

    /// Sends an event to every open view whose query matches, with the number it already has.
    pub fn push_raw(&self, matches: impl Fn(&Query) -> bool, event: ViewEvent) {
        let mut st = self.lock();
        st.views.retain(|(_, sink, _)| !sink.is_closed());
        for (q, sink, _) in &st.views {
            if matches(q) {
                sink.send(event.clone());
            }
        }
    }

    /// The daemon goes away: every view fails and later ones fail at once.
    pub fn kill(&self) {
        let mut st = self.lock();
        st.dead = true;
        for (_, sink, _) in st.views.drain(..) {
            sink.send(ViewEvent::Failed {
                seq: 1,
                error: gone(),
            });
        }
    }
}

fn gone() -> NodeError {
    NodeError::unavailable("the node closed the connection")
}

impl NodeApi for FakeApi {
    fn call(&self, cmd: Command) -> BoxFuture<'_, Result<CallResult, NodeError>> {
        Box::pin(async move {
            let mut st = self.lock();
            if st.dead {
                return Err(gone());
            }
            let result = (st.replier)(&cmd);
            st.calls.push(cmd);
            result
        })
    }

    fn view(&self, query: Query) -> ViewStream {
        let mut st = self.lock();
        if st.dead {
            return ViewStream::failed(gone());
        }
        st.opened.push(query.clone());
        let (sink, stream) = ViewStream::channel();
        match (st.provider)(&query) {
            Some(event) => {
                sink.send(event);
                st.views.push((query, sink, 1));
            }
            None => {
                sink.send(ViewEvent::Failed {
                    seq: 1,
                    error: NodeError::unavailable(
                        "work timelines are not available on this node yet",
                    ),
                });
            }
        }
        stream
    }
}

/// Hands out a fresh fake each time it is dialed (or fails while `up` is false).
pub struct FakeConnector {
    /// Whether dialing works.
    pub up: Mutex<bool>,
    /// Every node handed out, oldest first.
    pub nodes: Mutex<Vec<Arc<FakeApi>>>,
    /// How many dials there were.
    pub dials: Mutex<u32>,
    make: Box<dyn Fn() -> Arc<FakeApi> + Send + Sync>,
}

impl FakeConnector {
    /// A connector whose nodes come from `make`.
    pub fn new(make: impl Fn() -> Arc<FakeApi> + Send + Sync + 'static) -> Arc<FakeConnector> {
        Arc::new(FakeConnector {
            up: Mutex::new(true),
            nodes: Mutex::new(Vec::new()),
            dials: Mutex::new(0),
            make: Box::new(make),
        })
    }
}

impl Connector for Arc<FakeConnector> {
    fn connect(&self) -> BoxFuture<'static, std::io::Result<Arc<dyn NodeApi>>> {
        let this = Arc::clone(self);
        Box::pin(async move {
            *this.dials.lock().unwrap_or_else(|e| e.into_inner()) += 1;
            if !*this.up.lock().unwrap_or_else(|e| e.into_inner()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "down",
                ));
            }
            let node = (this.make)();
            this.nodes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(Arc::clone(&node));
            Ok(node as Arc<dyn NodeApi>)
        })
    }
}

// ---- fixtures ------------------------------------------------------------------------------------------

/// The node clock the fixtures use: the moment their rows were last active.
pub const NOW_MS: u64 = 100_000_000;

/// A snapshot event.
pub fn snapshot(rows: Vec<Row>, total: u32, offset: u32, next_cursor: Option<String>) -> ViewEvent {
    ViewEvent::Snapshot {
        seq: 1,
        rows,
        total,
        offset,
        next_cursor,
        now_ms: NOW_MS,
        totals: None,
    }
}

/// What a node does with a window over all of `rows` (ordered by key): the rows after the cursor, at most the
/// limit, with the total, the offset and the next cursor.
pub fn page_of(rows: &[Row], window: &Window) -> ViewEvent {
    let start = match &window.cursor {
        Some(c) => rows.iter().position(|r| r.key() == c).map_or(0, |i| i + 1),
        None => 0,
    };
    let end = (start + window.limit as usize).min(rows.len());
    let next = (end < rows.len()).then(|| rows[end - 1].key().to_owned());
    snapshot(
        rows[start..end].to_vec(),
        rows.len() as u32,
        start as u32,
        next,
    )
}

/// A board row with plausible defaults: working, in group `group`, one file, no overlap.
pub fn board_row(session: &str, agent: &str, person: &str, group: &str, step: &str) -> BoardRow {
    BoardRow {
        key: format!("{group}/{session}"),
        space: "team".into(),
        group: group.into(),
        session: session.into(),
        agent: agent.into(),
        person: person.into(),
        harness: Some("pi".into()),
        issue: Some(group.into()),
        repo: None,
        state: SessionState::Working,
        step: step.into(),
        tool: None,
        files: vec![han_node::api::FileTouch {
            path: "src/lib.rs".into(),
            hunks: vec![(1, 5)],
        }],
        overlaps: vec![],
        overlap_with: vec![],
        state_reason: None,
        tests: None,
        tokens: Some(Tokens {
            input: 30_000,
            out: 11_000,
            cost: 0,
        }),
        summary: None,
        sandbox: None,
        budget: None,
        publish_errors: 0,
        last_active_ms: NOW_MS,
        closed: false,
        needs_me: false,
        can_steer: true,
        can_cancel: true,
    }
}

/// A node-status row for `actor`.
pub fn node_status(actor: &str) -> Row {
    Row::NodeStatus(NodeStatusRow {
        key: "node".into(),
        actor: actor.into(),
        node_id: "n".into(),
        version: "0.1.0".into(),
        spaces: 1,
        spaces_live: 1,
        outbox_pending: 0,
        roles: vec![],
        now_ms: NOW_MS,
        subscriptions: 0,
        resyncs: 0,
        recent_errors: vec![],
        problems: vec![],
    })
}

/// A session row with a status.
pub fn session_row(id: &str, agent: &str, state: SessionState, step: &str) -> SessionRow {
    SessionRow {
        key: id.into(),
        space: "team".into(),
        actor: agent.into(),
        opened: "2026-10-13T10:00:00Z".into(),
        task: None,
        external: None,
        closed: false,
        state,
        state_reason: None,
        status: Some(StatusBody {
            session: id.into(),
            agent: agent.into(),
            delegate_of: None,
            harness: "pi".into(),
            issue: Some("#41".into()),
            repo: None,
            base: None,
            state,
            step: step.into(),
            tool: None,
            plan: vec![],
            files: vec![],
            tests: None,
            tokens: None,
            summary: None,
            sandbox: None,
            budget: None,
            publish_errors: 0,
            seq: 1,
            at: "2026-10-13T10:00:01Z".into(),
        }),
        last_active_ms: NOW_MS,
        rights: han_node::api::SessionRights {
            watch: true,
            read: true,
        },
        is_mine: true,
    }
}

/// An approval row: `git.push` by `proposer`, which `can_approve` may or may not be signed by the local actor.
pub fn approval(proposal: &str, proposer: &str, can_approve: bool) -> ApprovalRow {
    ApprovalRow {
        key: format!("team/{proposal}"),
        space: "team".into(),
        proposal: proposal.into(),
        proposer: proposer.into(),
        executor: "@git:han.example".into(),
        action: "git.push".into(),
        target: Some("han".into()),
        summary: vec![("branch".into(), "han/auth".into())],
        plan: None,
        plan_digest: "sha256:9f3c41aa07bed2e1aabbccddeeff00112233445566778899aabbccddeeff0011"
            .into(),
        digest_verified: true,
        approvals: vec![],
        cost: None,
        expires_at: None,
        seq: 7,
        can_approve,
        can_cancel: true,
        approvers: vec!["@lars:han.example".into()],
        why_gated: "push of a han/* branch (space rule)".into(),
        needed: han_node::api::Needed {
            rule: 1,
            guards: vec![],
        },
        agent_text: vec![],
    }
}

/// A filter that keeps everything.
pub fn no_filter() -> Filter {
    Filter::default()
}

// ---- the harness ---------------------------------------------------------------------------------------

/// A key event from a short name: `j`, `G`, `<enter>`, `<esc>`, `<tab>`, `<c-d>` and so on.
pub fn key_named(name: &str) -> KeyEvent {
    let none = KeyModifiers::NONE;
    let code = match name {
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "pgdn" => KeyCode::PageDown,
        "pgup" => KeyCode::PageUp,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "bs" => KeyCode::Backspace,
        "space" => KeyCode::Char(' '),
        n if n.starts_with("c-") && n.len() == 3 => {
            let c = n.chars().nth(2).unwrap_or('c');
            return KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        }
        other => panic!("unknown key <{other}>"),
    };
    KeyEvent::new(code, none)
}

/// The app and its host, run against a `TestBackend`.
pub struct Harness {
    /// The runtime (the app is `rt.app`).
    pub rt: Runtime,
    term: Terminal<TestBackend>,
    clock: Instant,
}

impl Harness {
    /// An app on `api`, `size` columns by rows, showing `space`.
    pub fn new(api: Arc<dyn NodeApi>, space: Option<&str>, size: (u16, u16)) -> Harness {
        Self::build(Some(api), None, space, size, Config::default(), None)
    }

    /// An app that dials `connector` (and redials it when the node goes away).
    pub fn dialing(
        connector: Arc<FakeConnector>,
        space: Option<&str>,
        size: (u16, u16),
    ) -> Harness {
        Self::build(
            None,
            Some(Arc::new(connector)),
            space,
            size,
            Config::default(),
            None,
        )
    }

    /// The general form: a config and the file it is saved to.
    pub fn build(
        api: Option<Arc<dyn NodeApi>>,
        connector: Option<Arc<dyn Connector>>,
        space: Option<&str>,
        size: (u16, u16),
        config: Config,
        config_path: Option<std::path::PathBuf>,
    ) -> Harness {
        let clock = Instant::now();
        let app = App::new(config, space.map(str::to_owned), clock);
        let mut rt = Runtime::new(app, api, connector, config_path);
        rt.handle(Msg::Resize(size.0, size.1));
        let term = Terminal::new(TestBackend::new(size.0, size.1)).expect("a test terminal");
        Harness { rt, term, clock }
    }

    /// Lets everything that can happen without waiting for time happen: tasks run, messages are handled.
    pub async fn settle(&mut self) {
        for _ in 0..60 {
            tokio::task::yield_now().await;
            self.rt.drain();
        }
    }

    /// Presses keys: each character is a key, `<name>` is a named one.
    pub async fn keys(&mut self, seq: &str) {
        let mut chars = seq.chars().peekable();
        while let Some(c) = chars.next() {
            let key = if c == '<' {
                let mut name = String::new();
                for n in chars.by_ref() {
                    if n == '>' {
                        break;
                    }
                    name.push(n);
                }
                key_named(&name)
            } else {
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
            };
            self.rt.handle(Msg::Key(key));
            self.settle().await;
        }
    }

    /// Moves the clock on and ticks.
    pub async fn advance(&mut self, by: Duration) {
        self.clock += by;
        self.rt.handle(Msg::Tick(self.clock));
        self.settle().await;
    }

    /// Resizes the terminal.
    pub fn resize(&mut self, w: u16, h: u16) {
        self.term.backend_mut().resize(w, h);
        self.rt.handle(Msg::Resize(w, h));
    }

    /// The screen as text, one line per row, trailing spaces trimmed.
    pub fn screen(&mut self) -> String {
        self.term
            .draw(|f| ui::draw(f, &self.rt.app))
            .expect("draws");
        buffer_text(self.term.backend().buffer())
    }

    /// Waits (in real time) until the screen shows `needle`; panics with the screen if it never does.
    pub async fn wait_for(&mut self, needle: &str) -> String {
        for _ in 0..500 {
            self.rt.drain();
            let s = self.screen();
            if s.contains(needle) {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the screen never showed {needle:?}:\n{}", self.screen());
    }
}

/// A buffer as text.
pub fn buffer_text(buf: &Buffer) -> String {
    let area = buf.area;
    let mut out = Vec::new();
    for y in 0..area.height {
        let mut line = String::new();
        let mut skip = 0usize;
        for x in 0..area.width {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let sym = buf[(x, y)].symbol();
            skip = UnicodeWidthStr::width(sym).saturating_sub(1);
            line.push_str(sym);
        }
        out.push(line.trim_end().to_owned());
    }
    out.join("\n")
}

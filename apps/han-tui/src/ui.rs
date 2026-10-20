//! Drawing: the board, the session pane, the approvals pane, the status line and the overlays. Pure functions
//! of the [`App`]; colour is never the only signal (every state has a glyph and a word).

use han_node::api::{
    ActorKind, BoardRow, Fidelity, OverlapKind, PatchStatus, SandboxBackend, SessionState,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::app::{App, Conn, Focus, Mode, group_name};
use crate::approvals::{Card, Tone};
use crate::board::{BoardState, Entry};
use crate::session::{Delivery, LogKind, Panel, SessionPane, Tab};
use crate::text::{age, clean, count, digest_groups, fit, pad, wrap};

/// Terminals narrower than this show one pane at a time.
const WIDE: u16 = 110;
/// The width of the approvals side pane.
const SIDE: u16 = 40;

fn tone(t: Tone) -> Style {
    Style::new().fg(match t {
        Tone::Good => Color::Green,
        Tone::Info => Color::Cyan,
        Tone::Bad => Color::Red,
    })
}

fn quiet() -> Style {
    Style::new().fg(Color::DarkGray)
}

/// The glyph, the colour and the word of a state (board spec 4.1).
pub fn state_look(s: SessionState) -> (&'static str, Color) {
    match s {
        SessionState::Working => ("●", Color::Green),
        SessionState::WaitingApproval => ("◐", Color::Yellow),
        SessionState::Blocked => ("⊘", Color::Yellow),
        SessionState::Idle => ("○", Color::Gray),
        SessionState::Done => ("✓", Color::Green),
        SessionState::Stale => ("◌", Color::DarkGray),
        SessionState::Failed => ("✗", Color::Red),
    }
}

/// Draws the whole screen.
pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    if area.width < 20 || area.height < 5 {
        f.render_widget(Paragraph::new("too small"), area);
        return;
    }
    let [head, body, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);
    draw_header(f, app, head);
    draw_body(f, app, body);
    draw_footer(f, app, foot);
    if app.approvals.card != Card::Closed {
        draw_card(f, app, body);
    }
    if app.help {
        draw_help(f, area);
    }
    if app.overlap_detail {
        draw_overlaps(f, app, area);
    }
    if app.tasks_open {
        draw_tasks(f, app, area);
    }
    if app.task_draft.is_some() {
        draw_task(f, app, area);
    }
}

fn draw_task(f: &mut Frame, app: &App, area: Rect) {
    let Some(draft) = &app.task_draft else { return };
    let width = area.width.saturating_sub(2).min(100);
    let height = area.height.saturating_sub(2).min(20);
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    f.render_widget(Clear, rect);
    let title = if draft.task.is_some() {
        " Existing task · start "
    } else {
        " New task · create and start "
    };
    let block = Block::bordered()
        .title(title)
        .border_style(Style::new().fg(Color::Cyan));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let w = inner.width as usize;
    let mut lines = vec![
        Line::raw(fit(&format!("Space: {}", clean(&draft.space)), w)),
        Line::raw(""),
    ];
    for (i, (label, edit)) in ["Agent", "Repository", "Request"]
        .iter()
        .zip(&draft.fields)
        .enumerate()
    {
        let active = i == draft.field
            && !draft.pending
            && (draft.task.is_none() || i < 2)
            && !draft.uncertain;
        let marker = if active { ">" } else { " " };
        lines.push(Line::styled(
            format!("{marker} {label}"),
            if active {
                Style::new().fg(Color::Cyan)
            } else {
                quiet()
            },
        ));
        // Keep the cursor's neighborhood visible in a long request instead of clipping its tail.
        let start = edit.cursor().saturating_sub(w.saturating_sub(2));
        let shown: String = edit.text().chars().skip(start).take(w).collect();
        let y = inner.y + lines.len() as u16;
        lines.push(Line::raw(clean(&shown)));
        if active && y < inner.bottom() {
            f.set_cursor_position((
                inner.x + edit.cursor().saturating_sub(start).min(w.saturating_sub(1)) as u16,
                y,
            ));
        }
    }
    lines.push(Line::raw(""));
    if let Some(id) = &draft.task {
        lines.extend(wrap(&format!("Task: {id}"), w).into_iter().map(Line::raw));
    }
    let help = if draft.pending {
        "Waiting for the node…"
    } else if draft.uncertain {
        "Esc closes · inspect tasks/board before retrying"
    } else if draft.task.is_some() {
        "Enter starts the existing task · Esc closes"
    } else {
        "Tab changes field · Enter creates and starts · Esc cancels"
    };
    lines.extend(wrap(help, w).into_iter().map(|s| Line::styled(s, quiet())));
    if !draft.notice.is_empty() {
        lines.extend(
            wrap(&clean(&draft.notice), w)
                .into_iter()
                .map(|s| Line::styled(s, Style::new().fg(Color::Yellow))),
        );
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_tasks(f: &mut Frame, app: &App, area: Rect) {
    let rect = Rect::new(
        area.x + 1,
        area.y + 1,
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );
    f.render_widget(Clear, rect);
    let block = Block::bordered()
        .title(" Tasks · current space ")
        .border_style(Style::new().fg(Color::Cyan));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let w = inner.width as usize;
    let rows = app.task_rows();
    let capacity = inner.height.saturating_sub(7).max(1) as usize;
    let start = app
        .task_selection
        .saturating_sub(capacity.saturating_sub(1));
    let mut lines = vec![Line::styled(
        fit(
            "↑ ↓ select · Enter start · n new task · r refresh · Esc board",
            w,
        ),
        quiet(),
    )];
    if rows.is_empty() {
        lines.push(Line::raw(
            "No open tasks in this window. Press n to create one.",
        ));
    }
    for (i, task) in rows.iter().enumerate().skip(start).take(capacity) {
        let selected = i == app.task_selection;
        let state = if task.accepted.is_empty() {
            "open"
        } else {
            "accepted"
        };
        let ask = task.ask.as_deref().unwrap_or("(request not received yet)");
        let text = format!(
            "{} {state:8} {} · {}",
            if selected { ">" } else { " " },
            task.to.join(", "),
            clean(ask)
        );
        lines.push(Line::styled(
            fit(&text, w),
            if selected {
                Style::new().fg(Color::Cyan)
            } else {
                Style::new()
            },
        ));
    }
    lines.push(Line::raw(""));
    if let Some(task) = rows.get(app.task_selection) {
        lines.extend(
            wrap(&format!("Task {}", task.task), w)
                .into_iter()
                .map(Line::raw),
        );
        lines.push(Line::raw(fit(
            &format!(
                "Request: {}",
                clean(task.ask.as_deref().unwrap_or("pending"))
            ),
            w,
        )));
    }
    lines.push(Line::styled(
        format!(
            "{} open tasks shown (window: up to 1,000 entries)",
            rows.len()
        ),
        quiet(),
    ));
    lines.extend(
        wrap(&app.tasks_notice, w)
            .into_iter()
            .map(|s| Line::styled(s, Style::new().fg(Color::Yellow))),
    );
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let space = app.space.as_deref().unwrap_or("(no space)");
    let filter = if app.board.filter_text.is_empty() {
        "-".to_owned()
    } else {
        clean(&app.board.filter_text)
    };
    let mut spans = vec![
        Span::styled(
            " HAN ",
            Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        ),
        Span::raw(format!(
            " {} · by {} · filter: {filter}",
            clean(space),
            group_name(app.board.group)
        )),
    ];
    if let Some(e) = app.board.filter_errors.first() {
        spans.push(Span::styled(
            format!("  ! {e}"),
            Style::new()
                .fg(Color::Red)
                .add_modifier(Modifier::UNDERLINED),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_body(f: &mut Frame, app: &App, area: Rect) {
    let wide = area.width >= WIDE;
    let session = app.session.is_some();
    if wide {
        let mut cols = Vec::new();
        let side = app.approvals_open;
        let rest = area.width.saturating_sub(if side { SIDE } else { 0 });
        if session {
            cols.push(Constraint::Length(rest * 2 / 5));
            cols.push(Constraint::Length(rest - rest * 2 / 5));
        } else {
            cols.push(Constraint::Length(rest));
        }
        if side {
            cols.push(Constraint::Length(SIDE));
        }
        let rects = Layout::horizontal(cols).split(area);
        let mut i = 0;
        draw_board(f, app, rects[i]);
        i += 1;
        if session {
            draw_session(f, app, rects[i]);
            i += 1;
        }
        if side {
            draw_approvals(f, app, rects[i]);
        }
    } else {
        match app.focus {
            Focus::Board => draw_board(f, app, area),
            Focus::Session => draw_session(f, app, area),
            Focus::Approvals => draw_approvals(f, app, area),
        }
    }
}

fn pane(title: String, focused: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(if focused {
            BorderType::Thick
        } else {
            BorderType::Plain
        })
        .border_style(if focused {
            Style::new().fg(Color::Cyan)
        } else {
            quiet()
        })
        .title(title)
}

// ---- the board -----------------------------------------------------------------------------------------

struct Cols {
    person: usize,
    agent: usize,
    harness: usize,
    issue: usize,
    files: usize,
    overlap: usize,
    tests: usize,
    tokens: usize,
    age: usize,
}

impl Cols {
    /// Drops columns, least important first, until a flexible state column of at least 22 fits.
    fn for_width(w: usize, by_issue: bool) -> Cols {
        let mut c = Cols {
            person: 10,
            agent: 12,
            harness: 10,
            issue: if by_issue { 0 } else { 12 },
            files: 8,
            overlap: 10,
            tests: 6,
            tokens: 5,
            age: 4,
        };
        let used = |c: &Cols| {
            // marker, glyph, and one space after every column
            2 + [
                c.person, c.agent, c.harness, c.issue, c.files, c.overlap, c.tests, c.tokens, c.age,
            ]
            .iter()
            .filter(|w| **w > 0)
            .map(|w| w + 1)
            .sum::<usize>()
        };
        let drops: [fn(&mut Cols); 7] = [
            |c| c.tests = 0,
            |c| c.harness = 0,
            |c| c.issue = 0,
            |c| c.tokens = 0,
            |c| c.files = 0,
            |c| c.person = 0,
            |c| c.overlap = 0,
        ];
        for d in drops {
            if used(&c) + 22 <= w {
                break;
            }
            d(&mut c);
        }
        c
    }

    fn state_width(&self, w: usize) -> usize {
        let used = 2 + [
            self.person,
            self.agent,
            self.harness,
            self.issue,
            self.files,
            self.overlap,
            self.tests,
            self.tokens,
            self.age,
        ]
        .iter()
        .filter(|x| **x > 0)
        .map(|x| x + 1)
        .sum::<usize>();
        w.saturating_sub(used).max(8)
    }
}

fn draw_board(f: &mut Frame, app: &App, area: Rect) {
    let b = &app.board;
    let focused = app.focus == Focus::Board;
    let mut block = pane(" Board ".to_owned(), focused);
    if b.synced && b.total() > 0 {
        let first = b.offset() + 1;
        let last = b.offset() + b.rows().len() as u32;
        let more_up = if b.offset() > 0 { "▲ " } else { "" };
        let more_down = if last < b.total() { " ▼" } else { "" };
        // With a filter on, the node's unfiltered count says how much the filter hides.
        let hidden = b
            .totals()
            .filter(|t| t.all.sum() > t.filtered.sum())
            .map(|t| format!(" · {} in the space", t.all.sum()))
            .unwrap_or_default();
        block = block.title_bottom(format!(
            " {more_up}{first}-{last} of {}{hidden}{more_down} ",
            b.total()
        ));
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    let msg = board_message(app);
    if let Some((text, style)) = msg {
        f.render_widget(Paragraph::new(text).style(style), inner);
        return;
    }
    let w = inner.width as usize;
    let cols = Cols::for_width(w, b.group == han_node::api::Group::Issue);
    let sw = cols.state_width(w);
    let mut lines: Vec<Line> = Vec::new();
    let mut head = String::from("  ");
    for (name, width) in [
        ("PERSON", cols.person),
        ("AGENT", cols.agent),
        ("HARNESS", cols.harness),
        ("ISSUE", cols.issue),
    ] {
        if width > 0 {
            head.push_str(&pad(name, width + 1));
        }
    }
    head.push_str(&pad("STATE · STEP", sw + 1));
    for (name, width) in [
        ("CHANGES", cols.files),
        ("OVERLAP", cols.overlap),
        ("TESTS", cols.tests),
        ("TOK", cols.tokens),
        ("AGE", cols.age),
    ] {
        if width > 0 {
            head.push_str(&pad(name, width + 1));
        }
    }
    lines.push(Line::styled(fit(head.trim_end(), w), quiet()));

    let entries = b.entries();
    let page = (inner.height as usize).saturating_sub(1).max(1);
    b.page.set(page);
    let selected = entries.iter().position(|e| match (e, b.selected()) {
        (Entry::Group { group, .. }, Some(crate::board::EntryId::Group(g))) => group == g,
        (Entry::Row(i), Some(crate::board::EntryId::Session(s))) => b.rows()[*i].session == *s,
        _ => false,
    });
    let mut top = b.top.get().min(entries.len().saturating_sub(1));
    if let Some(sel) = selected {
        if sel < top {
            top = sel;
        } else if sel >= top + page {
            top = sel + 1 - page;
        }
    }
    b.top.set(top);
    for (i, e) in entries.iter().enumerate().skip(top).take(page) {
        let is_sel = selected == Some(i);
        let line = match e {
            Entry::Group {
                group,
                rows,
                collapsed,
                tier,
                needs_me,
            } => group_line(app, group, *rows, *collapsed, *tier, *needs_me, w, is_sel),
            Entry::Row(r) => row_line(app, &b.rows()[*r], &cols, sw, w, is_sel),
        };
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn board_message(app: &App) -> Option<(String, Style)> {
    let b = &app.board;
    let Some(space) = &app.space else {
        return Some((
            "No space selected.\n\nType :space <space id> to look at one.".to_owned(),
            quiet(),
        ));
    };
    if let Conn::Connecting { attempt } = app.conn
        && !b.synced
    {
        let text = if attempt == 0 {
            "Connecting to the node…".to_owned()
        } else {
            format!("Waiting for `han node` (attempt {attempt})…")
        };
        return Some((text, Style::new().fg(Color::Yellow)));
    }
    if let Some(e) = &b.error
        && b.rows().is_empty()
    {
        let text = match e {
            han_node::api::NodeError::Denied { .. } => {
                format!("Read access to {space} is gone: {e}")
            }
            _ => format!("Cannot read the board of {space}: {e}\n\nr tries again."),
        };
        return Some((text, Style::new().fg(Color::Red)));
    }
    if !b.synced {
        return Some(("Loading the board…".to_owned(), quiet()));
    }
    if b.rows().is_empty() && b.entries().is_empty() {
        let text = if b.is_filtered() {
            "No session matches the filter.\n\nx clears it.".to_owned()
        } else {
            "No sessions in this space yet.".to_owned()
        };
        return Some((text, quiet()));
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn group_line(
    app: &App,
    group: &str,
    rows: u32,
    collapsed: bool,
    tier: u8,
    needs_me: u32,
    w: usize,
    selected: bool,
) -> Line<'static> {
    let label = if group.is_empty() {
        match app.board.group {
            han_node::api::Group::Issue => "No issue".to_owned(),
            han_node::api::Group::Person => "Unassigned".to_owned(),
            han_node::api::Group::State => "No state".to_owned(),
        }
    } else {
        clean(group)
    };
    let mut text = format!("{} {} ({rows})", if collapsed { "▸" } else { "▾" }, label);
    if tier > 0 {
        text.push_str(if tier >= 2 {
            "  ⚑ hunk"
        } else {
            "  ⚑ claim"
        });
    }
    if needs_me > 0 {
        text.push_str(&format!("  {needs_me} await you"));
    }
    let style = Style::new().add_modifier(Modifier::BOLD);
    let style = if selected {
        style.add_modifier(Modifier::REVERSED)
    } else {
        style
    };
    Line::styled(
        pad(&format!("{}{}", if selected { ">" } else { " " }, text), w),
        style,
    )
}

fn row_line(
    app: &App,
    r: &BoardRow,
    c: &Cols,
    sw: usize,
    w: usize,
    selected: bool,
) -> Line<'static> {
    let b = &app.board;
    let (glyph, color) = state_look(r.state);
    let quieter = matches!(
        r.state,
        SessionState::Stale | SessionState::Failed | SessionState::Done
    );
    let base = if quieter { quiet() } else { Style::new() };
    let age_ms = b.age_ms(r, app.now);
    let mut state = r.state.name().to_owned();
    if r.needs_me {
        state.push_str(" (awaits you)");
    }
    let step = clean(&r.step);
    // The node says why a session failed or is blocked; the step is what it was doing.
    let said = r
        .state_reason
        .as_deref()
        .map_or_else(|| step.clone(), clean);
    let state_text = match r.state {
        SessionState::Stale => format!("{state} · last heard {} ago · {step}", age(age_ms)),
        SessionState::Failed => format!("failed: {said}"),
        SessionState::Blocked => format!("{state}: {said}"),
        _ => {
            let tool = r
                .tool
                .as_deref()
                .map(|t| format!(" [{}]", clean(t)))
                .unwrap_or_default();
            format!("{state}: {step}{tool}")
        }
    };
    let harness = {
        let h = clean(r.harness.as_deref().unwrap_or("-"));
        let advisory = r.sandbox.is_some_and(|s| s.fidelity == Fidelity::Advisory);
        if advisory {
            format!("{}(adv)", fit(&h, c.harness.saturating_sub(5)))
        } else {
            h
        }
    };
    let files = match r.files.len() {
        1 => "1 file".to_owned(),
        n => format!("{n} files"),
    };
    let overlap = BoardState::badge(r)
        .map(|(word, _, n)| {
            if n > 1 {
                format!("⚑ {word} ×{n}")
            } else {
                format!("⚑ {word}")
            }
        })
        .unwrap_or_default();
    let tests = r
        .tests
        .map(|t| {
            format!(
                "{}/{}{}",
                t.pass,
                t.fail,
                if t.fail == 0 { "✓" } else { "✗" }
            )
        })
        .unwrap_or_else(|| "–".to_owned());
    let tokens = r
        .tokens
        .map(|t| count(t.input + t.out))
        .unwrap_or_else(|| "–".to_owned());
    let mut spans: Vec<Span> = Vec::new();
    let edge = if selected {
        Span::styled(">", Style::new().add_modifier(Modifier::BOLD))
    } else if r.needs_me {
        Span::styled("▌", Style::new().fg(Color::Yellow))
    } else {
        Span::raw(" ")
    };
    spans.push(edge);
    spans.push(Span::styled(glyph, Style::new().fg(color)));
    let mut cell = |text: String, width: usize, style: Style| {
        if width > 0 {
            spans.push(Span::styled(format!(" {}", pad(&text, width)), style));
        }
    };
    cell(clean(&r.person), c.person, base);
    cell(clean(&r.agent), c.agent, base);
    cell(harness, c.harness, quiet());
    cell(
        r.issue.as_deref().map(clean).unwrap_or_default(),
        c.issue,
        base,
    );
    cell(
        state_text,
        sw,
        if matches!(r.state, SessionState::Failed) {
            Style::new().fg(Color::Red)
        } else {
            base
        },
    );
    cell(files, c.files, quiet());
    let tier2 = r.overlaps.iter().any(|k| k.tier() >= 2);
    cell(
        overlap,
        c.overlap,
        Style::new().fg(if tier2 { Color::Yellow } else { Color::Gray }),
    );
    cell(
        tests,
        c.tests,
        if r.tests.is_some_and(|t| t.fail > 0) {
            Style::new().fg(Color::Red)
        } else {
            quiet()
        },
    );
    cell(tokens, c.tokens, quiet());
    cell(
        age(age_ms),
        c.age,
        if r.state == SessionState::Stale {
            Style::new().fg(Color::Yellow)
        } else {
            quiet()
        },
    );
    let mut line = Line::from(spans);
    let used: usize = line
        .spans
        .iter()
        .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
        .sum();
    if used < w {
        line.spans.push(Span::raw(" ".repeat(w - used)));
    }
    if selected {
        line = line.style(Style::new().add_modifier(Modifier::REVERSED));
    }
    line
}

// ---- the session pane ----------------------------------------------------------------------------------

fn draw_session(f: &mut Frame, app: &App, area: Rect) {
    let Some(p) = &app.session else { return };
    let focused = app.focus == Focus::Session;
    let who = p
        .hint
        .as_ref()
        .map(|h| format!("{} ({})", clean(&h.agent), clean(&h.person)))
        .unwrap_or_else(|| clean(&p.id));
    let block = pane(format!(" Session · {who} "), focused);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = inner.width as usize;
    let mut head: Vec<Line> = Vec::new();
    let st = p.row.as_ref().and_then(|r| r.status.as_ref());
    if let Some(state) = p.state() {
        let (glyph, color) = state_look(state);
        let mut spans = vec![Span::styled(
            format!("{glyph} {}", state.name()),
            Style::new().fg(color),
        )];
        let issue = p
            .hint
            .as_ref()
            .and_then(|h| h.issue.clone())
            .or_else(|| st.and_then(|s| s.issue.clone()));
        if let Some(i) = issue {
            spans.push(Span::raw(format!("  {}", clean(&i))));
        }
        if let Some(s) = st {
            spans.push(Span::styled(format!("  {}", clean(&s.harness)), quiet()));
        }
        head.push(Line::from(spans));
    } else if !p.synced {
        head.push(Line::styled("loading the session…", quiet()));
    }
    if let Some(e) = &p.error {
        head.push(Line::styled(
            fit(&format!("session view: {e}"), w),
            tone(Tone::Bad),
        ));
    }
    if let Some(s) = st {
        let tool = s
            .tool
            .as_deref()
            .map(|t| format!("  tool: {}", clean(t)))
            .unwrap_or_default();
        head.push(Line::raw(fit(
            &format!("step: {}{tool}", clean(&s.step)),
            w,
        )));
        let mut facts = Vec::new();
        if !s.plan.is_empty() {
            facts.push(format!(
                "plan {}/{}",
                s.plan.iter().filter(|i| i.done).count(),
                s.plan.len()
            ));
        }
        facts.push(format!("{} file(s)", s.files.len()));
        if let Some(t) = s.tests {
            facts.push(format!("tests {}/{}", t.pass, t.fail));
        }
        if let Some(t) = s.tokens {
            facts.push(format!("{} tokens", count(t.input + t.out)));
        }
        if let Some(sb) = s.sandbox {
            let backend = match sb.backend {
                SandboxBackend::Mxc => "mxc",
                SandboxBackend::Bwrap => "bubblewrap",
                SandboxBackend::None => "none",
            };
            let fidelity = if sb.fidelity == Fidelity::Advisory {
                "advisory"
            } else {
                "enforced"
            };
            facts.push(format!("sandbox: {backend} ({fidelity})"));
        }
        if let Some(b) = &s.budget {
            facts.push(b.text());
        }
        if s.publish_errors > 0 {
            facts.push(format!("{} publish error(s)", s.publish_errors));
        }
        head.push(Line::styled(fit(&facts.join(" · "), w), quiet()));
    }
    if let Some(row) = &p.row
        && !row.rights.read
    {
        head.push(Line::styled(
            fit("reasoning and tool output are withheld (no read right)", w),
            quiet(),
        ));
    }
    let mut tabs: Vec<Span> = Vec::new();
    for (i, t) in [Tab::Stream, Tab::Timeline, Tab::Changes]
        .into_iter()
        .enumerate()
    {
        let label = format!(" {} {} ", i + 1, t.name());
        tabs.push(if t == p.tab {
            Span::styled(
                label,
                Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD),
            )
        } else {
            Span::styled(label, quiet())
        });
    }
    head.push(Line::from(tabs));
    let head_h = head.len() as u16;
    let bar = decision_bar(app, p);
    let foot_h = 1 + u16::from(bar.is_some());
    if inner.height <= head_h + foot_h {
        f.render_widget(Paragraph::new(head), inner);
        return;
    }
    let [a_head, a_body, a_foot] = Layout::vertical([
        Constraint::Length(head_h),
        Constraint::Fill(1),
        Constraint::Length(foot_h),
    ])
    .areas(inner);
    f.render_widget(Paragraph::new(head), a_head);
    match p.tab {
        Tab::Stream => draw_stream(f, p, a_body),
        Tab::Timeline => draw_timeline(f, p, a_body),
        Tab::Changes => draw_changes(f, p, a_body),
    }
    let mut foot: Vec<Line> = Vec::new();
    if let Some(bar) = bar {
        foot.push(bar);
    }
    foot.push(steer_line(app, p, w));
    f.render_widget(Paragraph::new(foot), a_foot);
    if let (true, Some(e)) = (focused, &p.steer) {
        let x = a_foot.x
            + 2
            + e.cursor().min(w.saturating_sub(3)) as u16
            + steer_prefix_len(app, p) as u16;
        f.set_cursor_position((
            x.min(a_foot.right().saturating_sub(1)),
            a_foot.bottom().saturating_sub(1),
        ));
    }
}

fn decision_bar(app: &App, p: &SessionPane) -> Option<Line<'static>> {
    if p.state() != Some(SessionState::WaitingApproval) {
        return None;
    }
    let agent = p.hint.as_ref().map(|h| h.agent.as_str())?;
    let pending = app
        .approvals
        .rows()
        .iter()
        .filter(|r| r.proposer == agent)
        .count();
    let text = if pending > 0 {
        format!("◐ waiting for approval: {pending} effect(s) pending · a reviews")
    } else {
        "◐ waiting for approval".to_owned()
    };
    Some(Line::styled(text, Style::new().fg(Color::Yellow)))
}

fn steer_prefix_len(app: &App, p: &SessionPane) -> usize {
    steer_prefix(app, p).chars().count()
}

fn steer_prefix(app: &App, p: &SessionPane) -> String {
    let mine = p.hint.as_ref().is_some_and(|h| {
        app.me
            .as_deref()
            .is_some_and(|m| h.person == m || h.agent == m)
    });
    if mine {
        "steer (instruction)> ".to_owned()
    } else {
        "steer (data)> ".to_owned()
    }
}

fn steer_line(app: &App, p: &SessionPane, w: usize) -> Line<'static> {
    if let Some(e) = &p.steer {
        return Line::raw(fit(&format!("{}{}", steer_prefix(app, p), e.text()), w));
    }
    let status = match &p.delivery {
        Some(Delivery::Sending) => Some(("sending…".to_owned(), Tone::Info)),
        Some(Delivery::Sent(0)) => Some(("sent".to_owned(), Tone::Good)),
        Some(Delivery::Sent(n)) => Some((format!("sent · seq {n}"), Tone::Good)),
        Some(Delivery::Queued) => Some((
            "signed and queued; it goes out when the home is reachable".to_owned(),
            Tone::Info,
        )),
        Some(Delivery::Refused(e)) => Some((format!("not sent: {e}"), Tone::Bad)),
        None => None,
    };
    if let Some((text, t)) = status {
        return Line::styled(fit(&text, w), tone(t));
    }
    if p.can_steer() {
        let mine = steer_prefix(app, p).contains("instruction");
        let what = if mine {
            "to your agent: an instruction"
        } else {
            "to another's agent: data, not an instruction"
        };
        Line::styled(fit(&format!("i steers ({what})"), w), quiet())
    } else {
        Line::styled("watch only: you may not steer this session", quiet())
    }
}

fn draw_stream(f: &mut Frame, p: &SessionPane, area: Rect) {
    let h = area.height as usize;
    let w = area.width as usize;
    if p.log.is_empty() {
        let text = if p.synced {
            "Nothing yet."
        } else {
            "Waiting for the stream…"
        };
        f.render_widget(Paragraph::new(text).style(quiet()), area);
        return;
    }
    let bottom = p.log.len().saturating_sub(h);
    let start = if p.follow { bottom } else { p.top.min(bottom) };
    let lines: Vec<Line> = p
        .log
        .iter()
        .skip(start)
        .take(h)
        .map(|l| {
            let (mark, style) = match l.kind {
                LogKind::Opened => ("• ", quiet()),
                LogKind::State => ("● ", Style::new().fg(Color::Yellow)),
                LogKind::Step => ("▸ ", Style::new()),
                LogKind::Tool => ("$ ", Style::new().fg(Color::Cyan)),
                LogKind::Plan => ("✓ ", Style::new().fg(Color::Green)),
                LogKind::Tests => ("T ", Style::new().fg(Color::Magenta)),
                LogKind::Summary => ("Σ ", quiet()),
                LogKind::Steer => ("» ", Style::new().fg(Color::Yellow)),
                LogKind::Message => ("“ ", Style::new()),
                LogKind::Reasoning => ("~ ", quiet()),
                LogKind::Call => ("$ ", Style::new().fg(Color::Cyan)),
                LogKind::File => ("± ", Style::new().fg(Color::Green)),
                LogKind::Turn => ("— ", quiet()),
                LogKind::Decision => ("⚑ ", Style::new().fg(Color::Yellow)),
                LogKind::Unsupported => ("? ", quiet()),
            };
            Line::styled(fit(&format!("{mark}{}", clean(&l.text)), w), style)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
    if !p.follow {
        let tag = " paused · G follows ";
        let r = Rect::new(
            area.right().saturating_sub(tag.chars().count() as u16),
            area.y,
            tag.chars().count() as u16,
            1,
        );
        f.render_widget(
            Paragraph::new(tag).style(Style::new().fg(Color::Black).bg(Color::Yellow)),
            r,
        );
    }
}

fn pending_text(what: &str, message: &str) -> String {
    format!(
        "pending M4: {what} are not available on this node yet.\n\n({})",
        clean(message)
    )
}

fn draw_timeline(f: &mut Frame, p: &SessionPane, area: Rect) {
    let w = area.width as usize;
    match &p.timeline {
        Panel::Loading => {
            f.render_widget(Paragraph::new("Loading the timeline…").style(quiet()), area)
        }
        Panel::Pending(m) => f.render_widget(
            Paragraph::new(pending_text("work timelines", m)).style(Style::new().fg(Color::Yellow)),
            area,
        ),
        Panel::Failed(e) => f.render_widget(
            Paragraph::new(format!("timeline: {}", clean(e))).style(tone(Tone::Bad)),
            area,
        ),
        Panel::Ready(points) if points.is_empty() => {
            f.render_widget(
                Paragraph::new("No points yet: the session has changed nothing.").style(quiet()),
                area,
            );
        }
        Panel::Ready(points) => {
            let lines: Vec<Line> = points
                .iter()
                .rev()
                .skip(p.top)
                .take(area.height as usize)
                .map(|t| {
                    Line::raw(fit(
                        &format!(
                            "{} · {} file(s) · {}",
                            clean(&t.label),
                            t.files,
                            clean(&t.at)
                        ),
                        w,
                    ))
                })
                .collect();
            f.render_widget(Paragraph::new(lines), area);
        }
    }
}

fn draw_changes(f: &mut Frame, p: &SessionPane, area: Rect) {
    let w = area.width as usize;
    // The patches come from the timeline: while it is pending, so are they.
    if let Panel::Pending(m) = &p.timeline {
        f.render_widget(
            Paragraph::new(pending_text("patches", m)).style(Style::new().fg(Color::Yellow)),
            area,
        );
        return;
    }
    match &p.patches {
        Panel::Loading => {
            let text = if matches!(p.timeline, Panel::Ready(ref t) if t.is_empty()) {
                "No changes yet."
            } else {
                "Loading the changes…"
            };
            f.render_widget(Paragraph::new(text).style(quiet()), area);
        }
        Panel::Pending(m) => f.render_widget(
            Paragraph::new(pending_text("patches", m)).style(Style::new().fg(Color::Yellow)),
            area,
        ),
        Panel::Failed(e) => f.render_widget(
            Paragraph::new(format!("changes: {}", clean(e))).style(tone(Tone::Bad)),
            area,
        ),
        Panel::Ready(files) if files.is_empty() => {
            f.render_widget(Paragraph::new("No changes yet.").style(quiet()), area);
        }
        Panel::Ready(files) => {
            let list_h = files.len().min(5).min(area.height as usize / 2).max(1);
            let [a_list, a_patch] =
                Layout::vertical([Constraint::Length(list_h as u16), Constraint::Fill(1)])
                    .areas(area);
            let first = p.file.saturating_sub(list_h.saturating_sub(1));
            let list: Vec<Line> = files
                .iter()
                .enumerate()
                .skip(first)
                .take(list_h)
                .map(|(i, fp)| {
                    let mark = match fp.status {
                        PatchStatus::Added => 'A',
                        PatchStatus::Modified => 'M',
                        PatchStatus::Deleted => 'D',
                    };
                    let text = format!(
                        "{}{} {} +{} -{}",
                        if i == p.file { ">" } else { " " },
                        mark,
                        clean(&fp.path),
                        fp.added,
                        fp.removed
                    );
                    let style = if i == p.file {
                        Style::new().add_modifier(Modifier::REVERSED)
                    } else {
                        Style::new()
                    };
                    Line::styled(pad(&text, w), style)
                })
                .collect();
            f.render_widget(Paragraph::new(list), a_list);
            if let Some(fp) = files.get(p.file) {
                let lines: Vec<Line> = fp
                    .patch
                    .lines()
                    .skip(p.top)
                    .take(a_patch.height as usize)
                    .map(|l| {
                        let style = match l.chars().next() {
                            Some('+') => Style::new().fg(Color::Green),
                            Some('-') => Style::new().fg(Color::Red),
                            Some('@') => Style::new().fg(Color::Cyan),
                            _ => quiet(),
                        };
                        Line::styled(fit(&clean(l), w), style)
                    })
                    .collect();
                f.render_widget(Paragraph::new(lines), a_patch);
            }
        }
    }
}

// ---- the approvals pane --------------------------------------------------------------------------------

fn draw_approvals(f: &mut Frame, app: &App, area: Rect) {
    let a = &app.approvals;
    let focused = app.focus == Focus::Approvals;
    let block = pane(format!(" Approvals ({}) ", a.count()), focused).title_bottom(if focused {
        " j/k · Enter opens · Esc closes "
    } else {
        " a opens "
    });
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = inner.width as usize;
    if let Some(e) = &a.error {
        f.render_widget(
            Paragraph::new(format!("Cannot read approvals: {e}")).style(tone(Tone::Bad)),
            inner,
        );
        return;
    }
    if app.me.is_none() {
        f.render_widget(
            Paragraph::new("Waiting for the node to say who you are…").style(quiet()),
            inner,
        );
        return;
    }
    let mut lines: Vec<Line> = Vec::new();
    if let Some((t, text)) = &a.notice
        && a.card == Card::Closed
    {
        for part in wrap(text, w) {
            lines.push(Line::styled(part, tone(*t)));
        }
        lines.push(Line::raw(""));
    }
    if a.rows().is_empty() {
        lines.push(Line::styled("Nothing is waiting for you.", quiet()));
    }
    let sel = a.selected().map(|r| r.key.clone());
    for r in a.rows() {
        let is_sel = sel.as_deref() == Some(r.key.as_str());
        let mark = if is_sel { ">" } else { " " };
        let style = if is_sel {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new()
        };
        let you = if r.can_approve {
            "you can approve".to_owned()
        } else if r.approvers.is_empty() {
            "you cannot approve".to_owned()
        } else {
            format!("needs {}", r.approvers.join(" or "))
        };
        lines.push(Line::styled(
            pad(
                &format!("{mark}{} · {}", clean(&r.action), clean(&r.space)),
                w,
            ),
            style,
        ));
        lines.push(Line::raw(fit(&format!("  by {}", clean(&r.proposer)), w)));
        lines.push(Line::styled(fit(&format!("  {you}"), w), quiet()));
        lines.push(Line::raw(""));
    }
    if !a.admissions().is_empty() {
        lines.push(Line::styled(
            format!("Waiting to join ({})", a.admissions().len()),
            Style::new().add_modifier(Modifier::BOLD),
        ));
        for m in a.admissions() {
            let what = match m.kind {
                ActorKind::Human => "person".to_owned(),
                ActorKind::Agent | ActorKind::Service => {
                    let kind = if m.kind == ActorKind::Agent {
                        "agent"
                    } else {
                        "service"
                    };
                    match (&m.operator, m.operator_confirmed) {
                        (Some(op), true) => format!("{kind} of {}", clean(op)),
                        (Some(op), false) => {
                            format!("{kind} of {}, unconfirmed", clean(op))
                        }
                        (None, _) => kind.to_owned(),
                    }
                }
                ActorKind::Unknown => "not known yet".to_owned(),
            };
            lines.push(Line::raw(fit(&format!("  {}", clean(&m.actor)), w)));
            lines.push(Line::styled(
                fit(&format!("    {what} · {}", clean(&m.space)), w),
                quiet(),
            ));
        }
        lines.push(Line::styled("  admit with :admit <actor>", quiet()));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_card(f: &mut Frame, app: &App, body: Rect) {
    let a = &app.approvals;
    let Some(r) = a.card_row() else { return };
    let width = body.width.saturating_sub(4).min(92);
    let w = width.saturating_sub(4) as usize;
    let mut lines: Vec<Line> = Vec::new();
    let label = |s: &str| Span::styled(pad(s, 11), quiet());
    let wrapped = |label_text: &str, text: &str, lines: &mut Vec<Line>| {
        let parts = wrap(&clean(text), w.saturating_sub(11).max(8));
        for (i, part) in parts.into_iter().enumerate() {
            lines.push(Line::from(vec![
                Span::styled(pad(if i == 0 { label_text } else { "" }, 11), quiet()),
                Span::raw(part),
            ]));
        }
    };
    wrapped("WHY GATED", &r.why_gated, &mut lines);
    let action = match &r.target {
        Some(target) => format!("{} on {}", r.action, target),
        None => r.action.clone(),
    };
    wrapped("ACTION", &action, &mut lines);
    // The plan's values: some are text an agent wrote, so each is quoted and never taken for the card's own words.
    // Text the node sends in full is shown below, as the author's.
    let said = |name: &str| {
        r.agent_text
            .iter()
            .any(|t| t.field == name && t.author == r.proposer)
    };
    let mut first = true;
    for (name, value) in r.summary.iter().filter(|(n, _)| !said(n)) {
        wrapped(
            if first { "CHANGES" } else { "" },
            &format!("{}: \"{}\"", clean(name), clean(value)),
            &mut lines,
        );
        first = false;
    }
    for (i, t) in r.agent_text.iter().enumerate() {
        wrapped(
            if i == 0 { "WRITTEN" } else { "" },
            &format!(
                "{} wrote {}: \"{}\"",
                clean(&t.author),
                clean(&t.field),
                clean(&t.text)
            ),
            &mut lines,
        );
    }
    let checked = if r.digest_verified {
        "  (plan matches digest)"
    } else {
        ""
    };
    wrapped(
        "DIGEST",
        &format!("{}{checked}", digest_groups(&r.plan_digest)),
        &mut lines,
    );
    if !r.digest_verified {
        lines.push(Line::from(vec![
            label(""),
            Span::styled("plan NOT verified against its digest", tone(Tone::Bad)),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        label("WHO"),
        Span::raw(fit(
            &format!(
                "proposed by {} · executor {}",
                clean(&r.proposer),
                clean(&r.executor)
            ),
            w.saturating_sub(11),
        )),
    ]));
    if let Some((unit, n)) = &r.cost {
        lines.push(Line::from(vec![
            label("COST"),
            Span::raw(format!("{n} {}", clean(unit))),
        ]));
    }
    lines.push(Line::from(vec![
        label("EXPIRES"),
        Span::raw(
            r.expires_at
                .as_deref()
                .map(clean)
                .unwrap_or_else(|| "no expiry".to_owned()),
        ),
    ]));
    lines.push(Line::raw(""));
    let have = if r.approvals.is_empty() {
        "none yet".to_owned()
    } else {
        r.approvals
            .iter()
            .map(|x| clean(x))
            .collect::<Vec<_>>()
            .join(", ")
    };
    wrapped("APPROVALS", &format!("have: {have}"), &mut lines);
    if !r.approvers.is_empty() {
        wrapped(
            "",
            &format!("needed from: {}", r.approvers.join(" or ")),
            &mut lines,
        );
    }
    for (i, g) in r.needed.guards.iter().enumerate() {
        let who = match &g.guard {
            Some(guard) => format!(
                "{} guards {}: {}",
                clean(guard),
                clean(&g.agent),
                if g.approved {
                    "approved"
                } else {
                    "has not approved"
                }
            ),
            None => format!(
                "{} has no confirmed operator: nobody can approve for it",
                clean(&g.agent)
            ),
        };
        wrapped(if i == 0 { "GUARDS" } else { "" }, &who, &mut lines);
    }
    let you = a.block_reason(app.me.as_deref());
    match &you {
        None => wrapped("", "you can approve", &mut lines),
        Some(reason) => {
            for part in wrap(reason, w.saturating_sub(11).max(8)) {
                lines.push(Line::from(vec![
                    label(""),
                    Span::styled(part, tone(Tone::Bad)),
                ]));
            }
        }
    }
    lines.push(Line::raw(""));
    match &a.card {
        Card::Open { .. } => {
            let sign = if you.is_none() {
                "[a] Review & sign approval"
            } else {
                "(signing unavailable)"
            };
            lines.push(Line::styled(
                if r.can_cancel {
                    format!("{sign}    [c] Cancel effect    [Esc] Close")
                } else {
                    format!("{sign}    [Esc] Close")
                },
                Style::new().add_modifier(Modifier::BOLD),
            ));
        }
        Card::Signing { busy, .. } => {
            lines.push(Line::styled(
                "─ sign ".to_owned() + &"─".repeat(w.saturating_sub(7)),
                quiet(),
            ));
            lines.push(Line::raw("You are signing an approval of:"));
            lines.push(Line::raw(format!("  {}", clean(&r.action))));
            for part in wrap(&digest_groups(&r.plan_digest), w.saturating_sub(2).max(8)) {
                lines.push(Line::raw(format!("  {part}")));
            }
            lines.push(Line::raw(format!(
                "as {} on this device",
                clean(app.me.as_deref().unwrap_or("?"))
            )));
            lines.push(Line::styled(
                if *busy {
                    presence_wait().to_owned()
                } else {
                    "[Enter] Sign approval    [Esc] Back".to_owned()
                },
                Style::new().add_modifier(Modifier::BOLD),
            ));
        }
        Card::Cancelling { keep, busy, .. } => {
            lines.push(Line::styled(
                "─ cancel ".to_owned() + &"─".repeat(w.saturating_sub(9)),
                quiet(),
            ));
            for part in wrap(
                &format!(
                    "Cancel this {}? It will not run. The agent has to propose again to retry.",
                    clean(&r.action)
                ),
                w,
            ) {
                lines.push(Line::raw(part));
            }
            let pick = |on: bool, s: &str| {
                if on {
                    Span::styled(
                        format!("({s})"),
                        Style::new().add_modifier(Modifier::REVERSED),
                    )
                } else {
                    Span::raw(format!(" {s} "))
                }
            };
            lines.push(Line::from(vec![
                pick(*keep, "Keep it"),
                Span::raw("   "),
                pick(!*keep, "Cancel effect"),
            ]));
            if *busy {
                lines.push(Line::styled("Cancelling…", quiet()));
            } else {
                lines.push(Line::styled(
                    "←/→ choose · Enter confirms · Esc closes",
                    quiet(),
                ));
            }
        }
        Card::Closed => {}
    }
    if let Some((t, text)) = &a.notice {
        for part in wrap(text, w) {
            lines.push(Line::styled(part, tone(*t)));
        }
    }
    let height = (lines.len() as u16 + 2).min(body.height);
    let x = body.x + (body.width.saturating_sub(width)) / 2;
    let y = body.y + (body.height.saturating_sub(height)) / 2;
    let rect = Rect::new(x, y, width.min(body.width), height);
    f.render_widget(Clear, rect);
    let title = format!(" {} · {} ", clean(&r.action), clean(&r.space));
    let block = Block::bordered()
        .border_type(BorderType::Double)
        .title(title)
        .border_style(Style::new().fg(Color::Yellow));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    f.render_widget(Paragraph::new(lines), inner);
}

// ---- the status line and the overlays ------------------------------------------------------------------

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    match &app.mode {
        Mode::Palette(e) => {
            f.render_widget(Paragraph::new(format!(":{}", e.text())), area);
            f.set_cursor_position((area.x + 1 + e.cursor() as u16, area.y));
            return;
        }
        Mode::Filter(e) => {
            f.render_widget(Paragraph::new(format!("filter> {}", e.text())), area);
            f.set_cursor_position((area.x + 8 + e.cursor() as u16, area.y));
            return;
        }
        Mode::Normal => {}
    }
    let mut left: Vec<Span> = Vec::new();
    match app.conn {
        Conn::Up => left.push(Span::styled(" ● node ", Style::new().fg(Color::Green))),
        Conn::Connecting { attempt: 0 } => left.push(Span::styled(
            " ○ connecting ",
            Style::new().fg(Color::Yellow),
        )),
        Conn::Connecting { attempt } => left.push(Span::styled(
            format!(" ○ node down, reconnecting (try {attempt}) "),
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )),
    }
    if let Some(me) = &app.me {
        left.push(Span::raw(format!("{} ", clean(me))));
    }
    if let Some(s) = &app.space_status {
        let link = match s.link {
            han_node::api::SpaceLink::Connecting => "connecting",
            han_node::api::SpaceLink::Live => "live",
            han_node::api::SpaceLink::Offline => "offline",
            han_node::api::SpaceLink::Rejected => "rejected",
            han_node::api::SpaceLink::Misconfigured => "misconfigured",
        };
        let style = match s.link {
            han_node::api::SpaceLink::Live => Style::new().fg(Color::Green),
            han_node::api::SpaceLink::Connecting => Style::new().fg(Color::Yellow),
            _ => Style::new().fg(Color::Red),
        };
        left.push(Span::styled(format!("· space {link} "), style));
        left.push(Span::styled(format!("head {} ", s.head), quiet()));
        if s.outbox_pending > 0 {
            left.push(Span::styled(
                format!("outbox {} ", s.outbox_pending),
                Style::new().fg(Color::Yellow),
            ));
        }
    }
    let n = app.approvals.count();
    if n > 0 {
        left.push(Span::styled(
            format!("⚑ {n} awaiting you "),
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
    }
    let right: Span = match &app.toast {
        Some(t) => Span::styled(format!(" {} ", clean(&t.text)), tone(t.tone)),
        None => Span::styled(" ? help · : command ", quiet()),
    };
    let right_w = unicode_width::UnicodeWidthStr::width(right.content.as_ref()) as u16;
    let [l, r] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(right_w.min(area.width / 2 + 10)),
    ])
    .areas(area);
    f.render_widget(Paragraph::new(Line::from(left)), l);
    f.render_widget(Paragraph::new(Line::from(right)), r);
}

const HELP: &[(&str, &str)] = &[
    ("Board", ""),
    (
        "j k / ↓ ↑",
        "move; g g first, G last, Ctrl-d / Ctrl-u half a page",
    ),
    (
        "Enter",
        "open the session pane (on a group header: collapse or open it)",
    ),
    ("h l / ← →", "to the group header / collapse and open"),
    ("t", "group by issue, person, state (remembered per space)"),
    ("n / :task", "create and start a task in the current space"),
    ("T / :tasks", "browse open tasks and start an existing task"),
    (
        "/  x  c",
        "filter box (person: agent: harness: repo: path: label: state: overlap: mine: ...), clear, closed",
    ),
    (
        "o  a",
        "overlap detail (who, tier, files); approvals for this session",
    ),
    ("Session", ""),
    ("1 2 3 / [ ]", "stream, timeline, changes"),
    ("j k  G", "scroll; G follows the live end"),
    (
        "i / Enter",
        "steer (Enter sends, Esc cancels); allowed only with the steer right",
    ),
    ("Esc", "close the pane"),
    ("Approvals", ""),
    ("Enter / a", "open the full card (never signs)"),
    (
        "a, then Enter",
        "on the card: a opens the sign panel; Enter there signs",
    ),
    ("c  Esc", "cancel the effect (asks first); close"),
    ("Everywhere", ""),
    (
        ": or Ctrl-p",
        "command palette (:task :group :filter :space :closed :approvals :admit :quit)",
    ),
    ("Tab", "next pane"),
    ("?  q  Ctrl-c", "this help; quit (or close the pane); quit"),
];

/// The overlaps of the selected session: who, how high, and which files.
fn draw_overlaps(f: &mut Frame, app: &App, area: Rect) {
    let Some(r) = app.board.selected_row() else {
        return;
    };
    let width = area.width.saturating_sub(6).min(110);
    let height = (r.overlap_with.len() as u16 * 2 + 4).min(area.height);
    let rect = Rect::new(
        area.x + (area.width.saturating_sub(width)) / 2,
        area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    f.render_widget(Clear, rect);
    let block = Block::bordered()
        .title(format!(
            " Overlaps of {} (any key closes) ",
            clean(&r.agent)
        ))
        .border_style(Style::new().fg(Color::Yellow));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let w = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    for o in &r.overlap_with {
        let word = match o.kind {
            OverlapKind::IssueClaim => "same issue",
            OverlapKind::PathLease => "path lease",
            OverlapKind::SameFile => "same file",
            OverlapKind::HunkRange => "same lines",
        };
        let on = o
            .issue
            .as_deref()
            .map(|i| format!(" on {}", clean(i)))
            .unwrap_or_default();
        lines.push(Line::from(vec![
            Span::styled(
                pad(&format!("⚑ tier {} {word}", o.tier), 24),
                Style::new().fg(Color::Yellow),
            ),
            Span::raw(fit(
                &format!("{} ({}){on}", clean(&o.person), clean(&o.agent)),
                w.saturating_sub(24),
            )),
        ]));
        if !o.files.is_empty() {
            let files: Vec<String> = o.files.iter().map(|p| clean(p)).collect();
            lines.push(Line::styled(
                format!(
                    "{}{}",
                    " ".repeat(24),
                    fit(&files.join(", "), w.saturating_sub(24))
                ),
                quiet(),
            ));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_help(f: &mut Frame, area: Rect) {
    let width = area.width.saturating_sub(6).min(96);
    let height = (HELP.len() as u16 + 4).min(area.height);
    let rect = Rect::new(
        area.x + (area.width.saturating_sub(width)) / 2,
        area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    f.render_widget(Clear, rect);
    let block = Block::bordered()
        .title(" Keys (any key closes) ")
        .border_style(Style::new().fg(Color::Cyan));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let w = inner.width as usize;
    let lines: Vec<Line> = HELP
        .iter()
        .map(|(k, d)| {
            if d.is_empty() {
                Line::styled(
                    (*k).to_owned(),
                    Style::new().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                )
            } else {
                Line::from(vec![
                    Span::styled(pad(k, 16), Style::new().fg(Color::Cyan)),
                    Span::raw(fit(d, w.saturating_sub(16))),
                ])
            }
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

/// What the sign panel says while the node waits for the person to confirm on the device. The node runs on
/// this machine, so the platform names the check it runs.
fn presence_wait() -> &'static str {
    if cfg!(windows) {
        "Waiting for Windows Hello… confirm on this device"
    } else if cfg!(target_os = "macos") {
        "Waiting for Touch ID… confirm on this device"
    } else {
        "Waiting for the system's authentication prompt… confirm on this device"
    }
}

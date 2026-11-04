//! Guided space setup, with an explicit preview before granting automatic landing.

use han_node::api::{Command, HomeRef, trusted_team_policy};
use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::input::LineEdit;

/// Which step is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Pick a setup path.
    Choose,
    /// Enter a name, repositories and optional home.
    Trusted,
    /// Paste an invite ticket.
    Join,
    /// Review the exact operation before sending it.
    Preview,
    /// Await the node's response.
    Waiting,
    /// An uncertain or queued operation must be inspected before another setup.
    Recovery,
    /// Explain the existing custom policy tools.
    Advanced,
    /// The space is confirmed; the launcher can continue agent and runner setup.
    Ready,
}

/// The in-memory setup draft. Tickets are never persisted in TUI preferences.
#[derive(Clone, Debug)]
pub struct Setup {
    /// Current step.
    pub step: Step,
    /// Editable values (name, repositories, home; or invite in the first field).
    pub fields: [LineEdit; 3],
    /// Focused field.
    pub field: usize,
    /// Validation or recovery guidance.
    pub notice: String,
    /// Concrete command being reviewed.
    pub command: Option<Command>,
    /// Preview scroll offset.
    pub scroll: u16,
    /// A confirmed space, never inferred from an uncertain or queued response.
    pub ready_space: Option<String>,
    /// An explicit existing-space handoff whose eligibility is checked again on Enter.
    pub existing: bool,
}

impl Setup {
    /// Opens a fresh draft or a persisted recovery notice.
    pub fn new(recovery: Option<&str>) -> Self {
        Self {
            step: if recovery.is_some() {
                Step::Recovery
            } else {
                Step::Choose
            },
            fields: std::array::from_fn(|_| LineEdit::default()),
            field: 0,
            notice: recovery.unwrap_or_default().into(),
            command: None,
            scroll: 0,
            ready_space: None,
            existing: false,
        }
    }

    /// Insert pasted text only in editable fields.
    pub fn paste(&mut self, text: &str) {
        if matches!(self.step, Step::Trusted | Step::Join) {
            self.fields[self.field].insert(&text.replace(['\r', '\n'], " "));
        }
    }

    /// Handles a key; returns a command only after the preview's Enter.
    pub fn key(&mut self, key: KeyEvent) -> Option<Command> {
        if matches!(self.step, Step::Preview | Step::Recovery | Step::Advanced) {
            match key.code {
                KeyCode::PageDown => {
                    self.scroll = self.scroll.saturating_add(8);
                    return None;
                }
                KeyCode::PageUp => {
                    self.scroll = self.scroll.saturating_sub(8);
                    return None;
                }
                _ => {}
            }
        }
        match self.step {
            Step::Choose => match key.code {
                KeyCode::Char('1') => self.step = Step::Trusted,
                KeyCode::Char('2') => self.step = Step::Join,
                KeyCode::Char('3') => self.step = Step::Advanced,
                _ => {}
            },
            Step::Trusted | Step::Join => match key.code {
                KeyCode::Tab => {
                    self.field = (self.field + 1) % if self.step == Step::Join { 1 } else { 3 }
                }
                KeyCode::BackTab => {
                    self.field = (self.field + 2) % if self.step == Step::Join { 1 } else { 3 }
                }
                KeyCode::Enter => match self.build() {
                    Ok(command) => {
                        self.command = Some(command);
                        self.step = Step::Preview;
                        self.notice.clear();
                    }
                    Err(message) => self.notice = message,
                },
                _ => {
                    self.fields[self.field].key(key);
                }
            },
            Step::Preview if key.code == KeyCode::Enter => {
                self.step = Step::Waiting;
                return self.command.clone();
            }
            Step::Preview if key.code == KeyCode::Backspace => {
                self.step = if matches!(self.command, Some(Command::SpaceJoin { .. })) {
                    Step::Join
                } else {
                    Step::Trusted
                };
            }
            _ => {}
        }
        None
    }

    fn build(&self) -> Result<Command, String> {
        if self.step == Step::Join {
            let ticket = self.fields[0].text().trim();
            if ticket.is_empty() {
                return Err("Paste an invite ticket from a team owner.".into());
            }
            return Ok(Command::SpaceJoin {
                ticket: ticket.into(),
            });
        }
        let name = self.fields[0].text().trim().to_owned();
        if name.is_empty() {
            return Err("Enter a team name.".into());
        }
        let repos = self.fields[1]
            .text()
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        trusted_team_policy(&repos).map_err(|e| e.to_string())?;
        let home = match self.fields[2].text().trim() {
            "" => None,
            s => Some(s.parse::<HomeRef>().map_err(|e| e.to_string())?),
        };
        Ok(Command::SpaceCreateTrusted { home, name, repos })
    }
}

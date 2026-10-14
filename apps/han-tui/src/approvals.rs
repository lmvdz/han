//! The approvals pane: effects waiting for the local actor, and the card that signs one.
//!
//! Signing takes two deliberate steps, as in `spec/ui/approvals.md` (APR-5): the full card is opened, and only
//! then does `a` open the sign panel, where Enter on the one button signs. Enter on the card itself never
//! signs. That is an accident guard: the node then asks the person to confirm on the device (Windows Hello,
//! Touch ID, polkit), and signs only if they do (PLAN section 6.2, SR-106). The node says whether the actor may approve (`can_approve`) and why the effect is gated
//! (`why_gated`); this module does not decide either.

use han_node::api::{
    APPROVAL_NOT_SIGNED, AdmissionRow, ApprovalRow, NodeError, Row, ViewEvent, ViewMirror,
};

/// What the card is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Card {
    /// No card is open; the pane shows the compact list.
    Closed,
    /// The full card of one effect.
    Open {
        /// The effect's row key.
        key: String,
        /// The plan digest when the card opened; a change disables signing (APR-3).
        digest: String,
    },
    /// The sign panel of the card: the digest again, and the one button.
    Signing {
        /// The effect's row key.
        key: String,
        /// The plan digest being signed.
        digest: String,
        /// Whether the call is in flight (the panel is inert).
        busy: bool,
    },
    /// The confirm panel of "cancel effect"; the initial choice is to keep it.
    Cancelling {
        /// The effect's row key.
        key: String,
        /// Whether "Keep it" is the choice.
        keep: bool,
        /// Whether the call is in flight.
        busy: bool,
    },
}

/// The approvals view and the card.
#[derive(Debug)]
pub struct ApprovalsState {
    mirror: ViewMirror,
    rows: Vec<ApprovalRow>,
    admissions: Vec<AdmissionRow>,
    selected: Option<String>,
    sel_index: usize,
    /// The card.
    pub card: Card,
    /// A line shown in the card or the pane: the last outcome or the reason something changed.
    pub notice: Option<(Tone, String)>,
    /// Whether the last error allows trying again.
    pub retry: bool,
    /// Whether the first snapshot has arrived.
    pub synced: bool,
    /// Why the view failed.
    pub error: Option<NodeError>,
}

/// How a notice looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Good news.
    Good,
    /// Plain information.
    Info,
    /// An error.
    Bad,
}

impl Default for ApprovalsState {
    fn default() -> Self {
        Self::new()
    }
}

impl ApprovalsState {
    /// An empty pane.
    pub fn new() -> Self {
        ApprovalsState {
            mirror: ViewMirror::new(),
            rows: Vec::new(),
            admissions: Vec::new(),
            selected: None,
            sel_index: 0,
            card: Card::Closed,
            notice: None,
            retry: false,
            synced: false,
            error: None,
        }
    }

    /// The effects, in the node's order.
    pub fn rows(&self) -> &[ApprovalRow] {
        &self.rows
    }

    /// The actors waiting to be admitted (owners only).
    pub fn admissions(&self) -> &[AdmissionRow] {
        &self.admissions
    }

    /// How many effects wait.
    pub fn count(&self) -> usize {
        self.rows.len()
    }

    /// The selected effect.
    pub fn selected(&self) -> Option<&ApprovalRow> {
        let key = self.selected.as_ref()?;
        self.rows.iter().find(|r| &r.key == key)
    }

    /// The effect the card is on.
    pub fn card_row(&self) -> Option<&ApprovalRow> {
        let key = match &self.card {
            Card::Closed => return None,
            Card::Open { key, .. } | Card::Signing { key, .. } | Card::Cancelling { key, .. } => {
                key
            }
        };
        self.rows.iter().find(|r| &r.key == key)
    }

    /// Selects the effect proposed by this agent, if one waits. True when it did.
    pub fn select_by_proposer(&mut self, agent: &str) -> bool {
        match self.rows.iter().position(|r| r.proposer == agent) {
            Some(i) => {
                self.selected = Some(self.rows[i].key.clone());
                self.sel_index = i;
                true
            }
            None => false,
        }
    }

    /// Applies an event of the view. Returns a notice when the effect under an open card has left the list.
    pub fn apply(&mut self, event: ViewEvent) {
        if let ViewEvent::Failed { error, .. } = event {
            self.error = Some(error);
            return;
        }
        self.mirror.apply(event);
        self.error = None;
        self.synced = true;
        self.rows = self
            .mirror
            .rows()
            .into_iter()
            .filter_map(|r| match r {
                Row::Approval(a) => Some(a),
                _ => None,
            })
            .collect();
        self.admissions = self
            .mirror
            .rows()
            .into_iter()
            .filter_map(|r| match r {
                Row::Admission(a) => Some(a),
                _ => None,
            })
            .collect();
        // Cards do not reorder under the user: the selection follows its key, and falls back to the same
        // position when its effect has gone.
        let at = self
            .selected
            .as_ref()
            .and_then(|k| self.rows.iter().position(|r| &r.key == k));
        match at {
            Some(i) => self.sel_index = i,
            None if !self.rows.is_empty() => {
                self.sel_index = self.sel_index.min(self.rows.len() - 1);
                self.selected = Some(self.rows[self.sel_index].key.clone());
            }
            None => {
                self.selected = None;
                self.sel_index = 0;
            }
        }
        self.check_card();
    }

    /// An open card whose effect has left the list closes with a line saying so (APR-10); one whose digest
    /// changed keeps its place and refuses to sign.
    fn check_card(&mut self) {
        let in_flight = matches!(
            self.card,
            Card::Signing { busy: true, .. } | Card::Cancelling { busy: true, .. }
        );
        if self.card != Card::Closed && !in_flight && self.card_row().is_none() {
            self.card = Card::Closed;
            self.notice = Some((
                Tone::Info,
                "That effect is no longer waiting for you (approved, claimed, cancelled or expired).".to_owned(),
            ));
        }
    }

    /// Moves the selection by `delta`.
    pub fn nav(&mut self, delta: isize) {
        if self.rows.is_empty() || self.card != Card::Closed {
            return;
        }
        let i = (self.sel_index as isize + delta).clamp(0, self.rows.len() as isize - 1) as usize;
        self.sel_index = i;
        self.selected = Some(self.rows[i].key.clone());
    }

    /// Selects the first or last effect.
    pub fn nav_to(&mut self, last: bool) {
        if self.rows.is_empty() || self.card != Card::Closed {
            return;
        }
        let i = if last { self.rows.len() - 1 } else { 0 };
        self.sel_index = i;
        self.selected = Some(self.rows[i].key.clone());
    }

    /// Opens the full card of the selected effect.
    pub fn open_card(&mut self) {
        if let Some(r) = self.selected() {
            self.card = Card::Open {
                key: r.key.clone(),
                digest: r.plan_digest.clone(),
            };
            self.notice = None;
        }
    }

    /// Why signing is not possible now, or `None` when it is. The first reason that fails is given.
    pub fn block_reason(&self, me: Option<&str>) -> Option<String> {
        let row = self.card_row()?;
        let digest = match &self.card {
            Card::Open { digest, .. } | Card::Signing { digest, .. } => Some(digest),
            _ => None,
        };
        if digest.is_some_and(|d| *d != row.plan_digest) {
            return Some("This proposal changed. Review it again.".to_owned());
        }
        if me.is_some_and(|m| row.approvals.iter().any(|a| a == m)) {
            return Some("You have already approved this effect.".to_owned());
        }
        if !row.digest_verified {
            return Some(
                "The node could not check this plan against its digest, so it cannot be signed."
                    .to_owned(),
            );
        }
        if !row.can_approve {
            return Some(if row.approvers.is_empty() {
                "You cannot approve this effect.".to_owned()
            } else {
                format!(
                    "You cannot approve this effect; it needs {}.",
                    row.approvers.join(" or ")
                )
            });
        }
        None
    }

    /// Whether the actor may cancel the effect under the card (the node says: its proposer, or an owner).
    pub fn can_cancel(&self) -> bool {
        self.card_row().is_some_and(|r| r.can_cancel)
    }

    /// Re-reads the card after a digest change: the open card is kept, so the change is visible.
    pub fn card_digest_changed(&self) -> bool {
        match (&self.card, self.card_row()) {
            (Card::Open { digest, .. } | Card::Signing { digest, .. }, Some(r)) => {
                *digest != r.plan_digest
            }
            _ => false,
        }
    }

    /// The message a failed approval shows, and whether trying again may work (APR section 4.4).
    pub fn describe_error(e: &NodeError) -> (String, bool) {
        match e {
            // The node's presence check did not pass: nothing was signed, and the person may confirm again.
            NodeError::Denied { message } if message.starts_with(APPROVAL_NOT_SIGNED) => {
                (capitalize(message), true)
            }
            NodeError::Denied { message } => (
                format!("The space refused this approval: {message}."),
                false,
            ),
            NodeError::Conflict { message } => (
                format!("This effect changed while you were approving ({message})."),
                false,
            ),
            NodeError::Invalid { message } => (
                format!("The approval was malformed: {message}. This is a bug."),
                false,
            ),
            NodeError::NotFound { .. } => (
                "This effect no longer exists in the space.".to_owned(),
                false,
            ),
            NodeError::Unavailable { message, .. } => {
                (format!("Couldn't sign right now ({message})."), true)
            }
        }
    }
}

/// "approval not signed: presence check cancelled" as a sentence.
fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str() + ".",
        None => String::new(),
    }
}

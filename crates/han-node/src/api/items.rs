//! What the `Session` view yields besides the session's facts: its typed items (`part: items`) and its signed
//! record (`part: record`), and the other rows of the same family: admissions (Approvals view) and devices
//! (Devices view).
//!
//! The items are modelled on ACP's session updates. The node folds them from four places: the host's
//! `han.item/1` stream items (message, reasoning, plan, tool call, file change, turn start and end), the
//! session's `han.status/1` items (status), its signed `session.input` entries (input) and the effects its
//! actor proposes (decision). An item the node does not know is kept as [`ItemBody::Unknown`], never dropped
//! and never a decode error (DAT-5).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::rows::{PlanItem, StatusBody, Tokens};

/// A span `[start, end)` of a text, in UTF-16 code units (what a JavaScript `slice` takes). The host masks
/// secrets before text leaves the device and puts `[masked:<rule>]` where one was; the node marks those spans
/// so a face can draw them as masked (SES-3).
pub type Span = (u32, u32);

/// The spans of `[masked:<rule>]` markers in `text`.
pub fn masked_spans(text: &str) -> Vec<Span> {
    const OPEN: &str = "[masked:";
    let mut out = Vec::new();
    let mut at16 = 0u32;
    let mut rest = text;
    while let Some(i) = rest.find(OPEN) {
        at16 += u32::try_from(rest[..i].encode_utf16().count()).unwrap_or(u32::MAX);
        let tail = &rest[i..];
        let rule = &tail[OPEN.len()..];
        let ok = rule.find(']').filter(|n| {
            *n > 0
                && rule[..*n]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
        });
        match ok {
            Some(n) => {
                let len = u32::try_from(tail[..OPEN.len() + n + 1].encode_utf16().count())
                    .unwrap_or(u32::MAX);
                out.push((at16, at16.saturating_add(len)));
                at16 = at16.saturating_add(len);
                rest = &tail[OPEN.len() + n + 1..];
            }
            None => {
                // Not a marker: its first character is text.
                let c = tail.chars().next().map_or(1, char::len_utf8);
                at16 += u32::try_from(tail[..c].encode_utf16().count()).unwrap_or(1);
                rest = &tail[c..];
            }
        }
    }
    out
}

/// Who wrote a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    /// The person (or whoever steered).
    User,
    /// The agent.
    Agent,
}

/// What a tool call does, as ACP names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    /// Reads files.
    Read,
    /// Edits files.
    Edit,
    /// Deletes files.
    Delete,
    /// Moves files.
    Move,
    /// Searches.
    Search,
    /// Runs a command.
    Execute,
    /// Thinks.
    Think,
    /// Fetches from the network.
    Fetch,
    /// Anything else.
    Other,
}

/// How far a tool call is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    /// Not started.
    Pending,
    /// Running.
    InProgress,
    /// Finished.
    Completed,
    /// Failed.
    Failed,
}

/// A place a tool call works on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ToolLocation {
    /// The path.
    pub path: String,
    /// The line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub line: Option<u32>,
}

/// Text a tool call produced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ToolText {
    /// The text.
    pub text: String,
    /// The masked spans of `text` (set by the node).
    #[serde(default)]
    pub masked: Vec<Span>,
}

/// What started a turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum TurnDriver {
    /// A task.
    Task,
    /// A steering input.
    Input,
    /// An effect's outcome.
    Effect,
    /// The agent itself.
    Own,
}

/// How far a steering input got.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum InputState {
    /// Signed and in the log.
    Recorded,
    /// A turn started on it (the agent saw it).
    Seen,
}

/// The body of an item. The kinds are ACP's session updates plus the node's own (`status`, `decision`,
/// `input`); `unknown` carries what this version cannot read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ItemBody {
    /// A message; streamed text updates the same item until `done`.
    Message {
        /// Who wrote it.
        role: MessageRole,
        /// The text so far.
        text: String,
        /// Whether it is complete.
        done: bool,
        /// The masked spans of `text` (set by the node).
        #[serde(default)]
        masked: Vec<Span>,
    },
    /// The agent's reasoning (needs the read right).
    Reasoning {
        /// The text.
        text: String,
        /// How long it took.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        duration_ms: Option<u64>,
        /// The masked spans of `text` (set by the node).
        #[serde(default)]
        masked: Vec<Span>,
    },
    /// The agent's plan.
    Plan {
        /// Its steps.
        entries: Vec<PlanItem>,
    },
    /// A tool call; later items with the same id update it.
    ToolCall {
        /// ACP's id of the call.
        tool_call_id: String,
        /// What it does.
        tool_kind: ToolKind,
        /// A title.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        title: Option<String>,
        /// How far it is.
        status: ToolStatus,
        /// What it works on.
        #[serde(default)]
        locations: Vec<ToolLocation>,
        /// What it produced (needs the read right).
        #[serde(default)]
        content: Vec<ToolText>,
        /// When it started.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        started_at: Option<String>,
        /// When it ended.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        ended_at: Option<String>,
    },
    /// A file a tool call changed.
    FileChange {
        /// The tool call.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        tool_call_id: Option<String>,
        /// The path.
        path: String,
        /// Lines added.
        added: u32,
        /// Lines removed.
        removed: u32,
    },
    /// An effect the session's actor proposed (made by the node from the log).
    Decision {
        /// The proposal's entry id.
        effect: String,
        /// The action.
        action: String,
    },
    /// A steering input (made by the node from the log).
    Input {
        /// The `session.input` entry.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        entry: Option<String>,
        /// Who signed it.
        author: String,
        /// The text (absent when redacted).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        text: Option<String>,
        /// How far it got.
        state: InputState,
        /// The masked spans of `text` (set by the node).
        #[serde(default)]
        masked: Vec<Span>,
    },
    /// A turn begins.
    TurnStart {
        /// The turn.
        turn: u32,
        /// What started it.
        driver: TurnDriver,
        /// The entry that did (a task, an input or an outcome).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        driver_entry: Option<String>,
        /// The harness runs the turn in its stricter mode (a guarded agent).
        #[serde(default)]
        strict: bool,
    },
    /// A turn ends.
    TurnEnd {
        /// The turn.
        turn: u32,
        /// How long it took.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        duration_ms: Option<u64>,
        /// What it used.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        tokens: Option<Tokens>,
    },
    /// A `han.status/1` item as the host sent it (made by the node).
    Status {
        /// The body.
        body: Box<StatusBody>,
    },
    /// An item this version cannot read.
    Unknown {
        /// Its `kind`.
        raw_kind: String,
        /// The item as JSON text.
        raw: String,
    },
}

/// One item of a session's stream (`Session` view, `part: items`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ItemRow {
    /// Orders and identifies the row: the number in the session, zero-padded.
    pub key: String,
    /// The session.
    pub session: String,
    /// The item's stable id; a later item with the same id replaces its body.
    pub id: String,
    /// The number the node gave the item when it first saw it.
    pub seq_in_session: u64,
    /// The turn it belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub turn: Option<u32>,
    /// This node's steady clock (ms) when the item last changed.
    pub updated_ms: u64,
    /// What it is.
    pub item: ItemBody,
}

/// One entry of a session's signed record (`Session` view, `part: record`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RecordRow {
    /// Orders and identifies the row: the seq, zero-padded.
    pub key: String,
    /// The session.
    pub session: String,
    /// The entry's position in the space's log.
    pub seq: u64,
    /// The entry's id.
    pub entry_id: String,
    /// The entry's kind.
    pub kind: String,
    /// Who signed it.
    pub author: String,
    /// The receipt's time.
    pub at: String,
    /// One line on what it did. Text an actor wrote is cut and marked by quotes.
    pub summary: String,
    /// The node checked the receipt before it took the entry.
    pub verified: bool,
    /// The content was erased by a `redact`.
    #[serde(default)]
    pub redacted: bool,
    /// Why, when the redaction says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub redact_reason: Option<String>,
    /// The effect it is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub effect: Option<String>,
}

/// What kind of actor asks to join.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    /// A person.
    Human,
    /// An agent.
    Agent,
    /// A service.
    Service,
    /// The node could not read its profile.
    Unknown,
}

/// An actor that asked to join and waits for an owner (Approvals view; owners only).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AdmissionRow {
    /// Orders and identifies the row: the space and the actor.
    pub key: String,
    /// The space.
    pub space: String,
    /// The actor that asks.
    pub actor: String,
    /// What it is, from its profile.
    pub kind: ActorKind,
    /// Its operator, for an agent or a service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub operator: Option<String>,
    /// The operator confirmed the agent in its profile space (P-03); only then may it be admitted.
    pub operator_confirmed: bool,
    /// This node's steady clock (ms) when the join arrived.
    pub at_ms: u64,
    /// The invite was this device's. The home's join signal does not say which ticket was used, so this is
    /// false until it does.
    pub via_my_ticket: bool,
}

/// Whether a key of the identity works.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DeviceState {
    /// In use.
    Active,
    /// Revoked.
    Revoked,
}

/// One key of the identity (`Devices` view).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DeviceRow {
    /// Orders and identifies the row: the key id.
    pub key: String,
    /// The key id.
    pub key_id: String,
    /// Active or revoked.
    pub state: DeviceState,
    /// When the key was added to the profile (the receipt's time).
    pub added_at: String,
    /// When it was revoked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub revoked_at: Option<String>,
    /// This device holds the key.
    pub this_device: bool,
}

/// The totals of a change set (`Patches` and `ChangeSet` views): the row with key `` (it sorts first).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ChangeSummaryRow {
    /// Always empty.
    pub key: String,
    /// How many paths the host kept out of the change set (secrets, per the exclusion list). Their names are
    /// never sent.
    pub excluded: u32,
}

#[cfg(test)]
mod tests {
    use super::masked_spans;

    #[test]
    fn markers_become_utf16_spans() {
        assert_eq!(masked_spans("no secret"), vec![]);
        assert_eq!(masked_spans("key=[masked:aws_key]!"), vec![(4, 20)]);
        // A character outside the BMP counts as two code units, like in JavaScript.
        assert_eq!(
            masked_spans("😀[masked:x]a[masked:y-1]"),
            vec![(2, 12), (13, 25)]
        );
        // Not markers.
        assert_eq!(masked_spans("[masked:] [masked:a b] [masked:x"), vec![]);
    }
}

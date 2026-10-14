//! The session's typed items (`han.item/1`, spec/ui/data.md §4.2, §12.1): what the agent says and does, as the
//! node's `Session` view shows it.
//!
//! The feed folds ACP `session/update`s (as [`AgentEvent`]s) into items: the agent's message chunks coalesce
//! into one `message` item per message, thought chunks into one `reasoning` item, and a tool call with its
//! updates into one `tool_call` item. An item with the same id replaces the node's earlier body, so the feed
//! sends each changed item whole. Every text is masked here (SR-52) and bounded, so an item fits the stream
//! (64 KiB with its envelope); the node marks the masked spans and withholds reasoning and tool output from
//! whoever lacks the read right.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::acp::AgentEvent;
use crate::mask::Masker;

/// The profile name of the items.
pub const PROFILE: &str = "han.item/1";

/// The most bytes of a message's or a thought's raw text kept (masking may lengthen it a little).
const MAX_TEXT: usize = 16 * 1024;
/// The most bytes of a masked text sent in one item.
const MAX_SENT: usize = 24 * 1024;
/// The most bytes of a tool's title.
const MAX_TITLE: usize = 300;
/// The most bytes of a path.
const MAX_PATH: usize = 512;
/// The most locations a tool call shows.
const MAX_LOCATIONS: usize = 20;
/// The most tool calls tracked (older ones are forgotten; the node keeps what it was sent).
const MAX_TOOLS: usize = 500;

/// A message or a thought, growing chunk by chunk.
struct Run {
    reasoning: bool,
    text: String,
    cut: bool,
    done: bool,
    turn: u32,
}

impl Run {
    fn push(&mut self, chunk: &str) {
        let room = MAX_TEXT.saturating_sub(self.text.len());
        if chunk.len() <= room {
            self.text.push_str(chunk);
        } else {
            self.cut = true;
            let mut end = room;
            while !chunk.is_char_boundary(end) {
                end -= 1;
            }
            self.text.push_str(&chunk[..end]);
        }
    }
}

struct Tool {
    kind: String,
    title: String,
    status: String,
    locations: Vec<String>,
    output: Option<String>,
    started_at: String,
    ended_at: Option<String>,
    turn: u32,
}

/// Folds agent events into `han.item/1` bodies.
#[derive(Default)]
pub struct ItemFeed {
    turn: u32,
    /// Counter for ids of messages and thoughts.
    runs_seen: u32,
    runs: HashMap<String, Run>,
    /// The ids of the open message and the open thought.
    open_message: Option<String>,
    open_thought: Option<String>,
    tools: HashMap<String, Tool>,
    tool_order: Vec<String>,
    /// Ids of items that changed since [`ItemFeed::take`], in the order they first changed.
    dirty: Vec<String>,
    /// The raw text of the agent's latest message this turn.
    last_message: Option<String>,
}

impl ItemFeed {
    /// A feed with no items.
    pub fn new() -> Self {
        Self::default()
    }

    /// A new turn begins: what follows belongs to it.
    pub fn begin_turn(&mut self) {
        self.close_runs();
        self.turn += 1;
        self.last_message = None;
    }

    /// The raw text of the agent's latest message of this turn, if it said one.
    pub fn last_message(&self) -> Option<&str> {
        self.last_message
            .as_deref()
            .filter(|m| !m.trim().is_empty())
    }

    fn mark(&mut self, id: &str) {
        if !self.dirty.iter().any(|d| d == id) {
            self.dirty.push(id.to_owned());
        }
    }

    /// Closes the open message and thought: the next chunk starts a new one.
    fn close_runs(&mut self) {
        for id in [self.open_message.take(), self.open_thought.take()]
            .into_iter()
            .flatten()
        {
            if let Some(r) = self.runs.get_mut(&id) {
                r.done = true;
            }
            self.mark(&id);
        }
    }

    fn chunk(&mut self, reasoning: bool, text: &str) {
        // A message after a thought (or the reverse) is a new item; so is anything after a tool call.
        if reasoning && self.open_message.is_some() || !reasoning && self.open_thought.is_some() {
            self.close_runs();
        }
        let open = if reasoning {
            &mut self.open_thought
        } else {
            &mut self.open_message
        };
        let id = match open {
            Some(id) => id.clone(),
            None => {
                self.runs_seen += 1;
                let id = format!(
                    "{}:{}:{}",
                    if reasoning { "thought" } else { "msg" },
                    self.turn,
                    self.runs_seen
                );
                *open = Some(id.clone());
                self.runs.insert(
                    id.clone(),
                    Run {
                        reasoning,
                        text: String::new(),
                        cut: false,
                        done: false,
                        turn: self.turn,
                    },
                );
                id
            }
        };
        if let Some(r) = self.runs.get_mut(&id) {
            r.push(text);
            if !reasoning {
                self.last_message = Some(r.text.clone());
            }
        }
        self.mark(&id);
    }

    fn tool(&mut self, id: &str, now: &str) -> &mut Tool {
        if !self.tools.contains_key(id) {
            self.tool_order.push(id.to_owned());
            while self.tool_order.len() > MAX_TOOLS {
                let old = self.tool_order.remove(0);
                self.tools.remove(&old);
            }
        }
        let turn = self.turn;
        self.tools.entry(id.to_owned()).or_insert_with(|| Tool {
            kind: String::new(),
            title: String::new(),
            status: "pending".into(),
            locations: Vec::new(),
            output: None,
            started_at: now.to_owned(),
            ended_at: None,
            turn,
        })
    }

    /// Folds one event.
    pub fn apply(&mut self, ev: &AgentEvent, now: &str) {
        match ev {
            AgentEvent::Output { text } => self.chunk(false, text),
            AgentEvent::Thought { text } => self.chunk(true, text),
            AgentEvent::ToolCall {
                id,
                title,
                kind,
                status,
                locations,
                ..
            } if !id.is_empty() => {
                self.close_runs();
                let t = self.tool(id, now);
                t.kind = kind.clone().unwrap_or_default();
                t.title = title.clone();
                t.status = status.clone().unwrap_or_else(|| "pending".into());
                t.locations = locations.clone();
                self.mark(&format!("tool:{id}"));
            }
            AgentEvent::ToolCallUpdate {
                id,
                status,
                title,
                output,
                locations,
                ..
            } if !id.is_empty() => {
                self.close_runs();
                let t = self.tool(id, now);
                if let Some(s) = status {
                    t.status = s.clone();
                }
                if let Some(s) = title {
                    t.title = s.clone();
                }
                if let Some(o) = output {
                    t.output = Some(o.clone());
                }
                if !locations.is_empty() {
                    t.locations = locations.clone();
                }
                if matches!(t.status.as_str(), "completed" | "failed") && t.ended_at.is_none() {
                    t.ended_at = Some(now.to_owned());
                }
                self.mark(&format!("tool:{id}"));
            }
            _ => {}
        }
    }

    /// Closes what is open (the turn ended) so the last message goes out with `done`.
    pub fn end_turn(&mut self) {
        self.close_runs();
    }

    /// The items that changed since the last call, as `han.item/1` bodies, masked and bounded.
    pub fn take(&mut self, masker: &Masker) -> Vec<Value> {
        let ids = std::mem::take(&mut self.dirty);
        let mut out = Vec::new();
        for id in ids {
            if let Some(tool_id) = id.strip_prefix("tool:") {
                if let Some(t) = self.tools.get(tool_id) {
                    out.push(tool_body(&id, tool_id, t, masker));
                }
            } else if let Some(r) = self.runs.get(&id) {
                out.push(run_body(&id, r, masker));
                if r.done {
                    self.runs.remove(&id);
                }
            }
        }
        out
    }
}

fn run_body(id: &str, r: &Run, masker: &Masker) -> Value {
    let mut text = masked(masker, &r.text, MAX_SENT);
    if r.cut {
        text.push_str("\n[truncated]");
    }
    if r.reasoning {
        json!({"id": id, "turn": r.turn, "kind": "reasoning", "text": text})
    } else {
        json!({"id": id, "turn": r.turn, "kind": "message", "role": "agent", "text": text, "done": r.done})
    }
}

fn tool_body(id: &str, tool_id: &str, t: &Tool, masker: &Masker) -> Value {
    let mut v = json!({
        "id": id,
        "turn": t.turn,
        "kind": "tool_call",
        "tool_call_id": bounded(tool_id, 128),
        "tool_kind": tool_kind(&t.kind),
        "status": tool_status(&t.status),
        "locations": t
            .locations
            .iter()
            .take(MAX_LOCATIONS)
            .map(|p| json!({"path": masked(masker, p, MAX_PATH)}))
            .collect::<Vec<_>>(),
        "content": t
            .output
            .iter()
            .map(|o| json!({"text": masked(masker, o, MAX_SENT)}))
            .collect::<Vec<_>>(),
        "started_at": t.started_at,
    });
    if !t.title.is_empty() {
        v["title"] = Value::String(masked(masker, &t.title, MAX_TITLE));
    }
    if let Some(e) = &t.ended_at {
        v["ended_at"] = Value::String(e.clone());
    }
    v
}

fn masked(masker: &Masker, s: &str, max: usize) -> String {
    bounded(&masker.mask(s), max)
}

/// `s` cut to at most `max` bytes on a character boundary.
fn bounded(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

fn tool_kind(k: &str) -> &'static str {
    match k {
        "read" => "read",
        "edit" => "edit",
        "delete" => "delete",
        "move" => "move",
        "search" => "search",
        "execute" => "execute",
        "think" => "think",
        "fetch" => "fetch",
        _ => "other",
    }
}

fn tool_status(s: &str) -> &'static str {
    match s {
        "in_progress" => "in_progress",
        "completed" => "completed",
        "failed" => "failed",
        _ => "pending",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use han_node::api::{ItemBody, MessageRole, ToolStatus};

    fn masker() -> Masker {
        Masker::new(vec!["hunter2-secret".to_owned()])
    }

    fn out(t: &str) -> AgentEvent {
        AgentEvent::Output { text: t.into() }
    }

    fn body(v: &Value) -> ItemBody {
        serde_json::from_value(v.clone()).expect("the node reads it")
    }

    #[test]
    fn chunks_coalesce_per_message_and_a_tool_call_ends_the_message() {
        let m = masker();
        let mut f = ItemFeed::new();
        f.begin_turn();
        f.apply(&out("Hel"), "t0");
        f.apply(&out("lo, my key is hunter2-"), "t0");
        f.apply(&out("secret"), "t0");
        let first = f.take(&m);
        assert_eq!(first.len(), 1);
        let ItemBody::Message {
            role, text, done, ..
        } = body(&first[0])
        else {
            panic!("{first:?}")
        };
        assert_eq!(role, MessageRole::Agent);
        assert_eq!(text, "Hello, my key is [masked:known]");
        assert!(!done);
        let id = first[0]["id"].as_str().unwrap().to_owned();

        f.apply(
            &AgentEvent::ToolCall {
                id: "c1".into(),
                title: "cargo test".into(),
                kind: Some("execute".into()),
                status: Some("in_progress".into()),
                raw_input: None,
                locations: vec![],
            },
            "t1",
        );
        f.apply(
            &AgentEvent::ToolCallUpdate {
                id: "c1".into(),
                status: Some("completed".into()),
                title: None,
                output: Some("ok hunter2-secret".into()),
                raw_output: None,
                locations: vec![],
            },
            "t2",
        );
        f.apply(&out("Done."), "t3");
        let next = f.take(&m);
        // The first message is closed (same id, done), the tool call is one item, the new message is another.
        assert_eq!(next.len(), 3, "{next:?}");
        assert_eq!(next[0]["id"], id);
        assert_eq!(next[0]["done"], true);
        let ItemBody::ToolCall {
            status, content, ..
        } = body(&next[1])
        else {
            panic!("{next:?}")
        };
        assert_eq!(status, ToolStatus::Completed);
        assert_eq!(content[0].text, "ok [masked:known]");
        assert_ne!(next[2]["id"], id);
        assert_eq!(f.last_message(), Some("Done."));
        f.end_turn();
        let last = f.take(&m);
        assert_eq!(last.len(), 1);
        assert_eq!(last[0]["done"], true);
        assert!(f.take(&m).is_empty());
    }

    #[test]
    fn thoughts_are_reasoning_items_and_long_texts_are_bounded() {
        let m = masker();
        let mut f = ItemFeed::new();
        f.begin_turn();
        f.apply(&AgentEvent::Thought { text: "hm".into() }, "t");
        f.apply(&AgentEvent::Thought { text: "m".into() }, "t");
        f.apply(&out(&"é".repeat(40_000)), "t");
        let items = f.take(&m);
        assert_eq!(items.len(), 2);
        assert!(matches!(body(&items[0]), ItemBody::Reasoning { text, .. } if text == "hmm"));
        let ItemBody::Message { text, .. } = body(&items[1]) else {
            panic!()
        };
        assert!(text.len() <= MAX_SENT + 16, "{}", text.len());
        assert!(text.ends_with("[truncated]"));
    }
}

//! Token counts from provider responses (PLAN §6.4 Cost): JSON bodies and SSE streams, Anthropic Messages and
//! OpenAI-style `usage` objects. The proxy is the reliable source of counts in API-key mode.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Token counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Input tokens (not counting cache reads).
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Input tokens read from the prompt cache.
    pub cache_read: u64,
    /// Input tokens written to the prompt cache.
    pub cache_creation: u64,
    /// Model requests forwarded.
    pub requests: u64,
    /// Responses with complete input/output usage and a clean end of the response.
    #[serde(default)]
    pub measured_requests: u64,
}

impl Usage {
    /// Adds another count.
    pub fn add(&mut self, o: &Usage) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_creation += o.cache_creation;
        self.requests += o.requests;
        self.measured_requests += o.measured_requests;
    }

    fn max_with(&mut self, o: &Usage) {
        self.input = self.input.max(o.input);
        self.output = self.output.max(o.output);
        self.cache_read = self.cache_read.max(o.cache_read);
        self.cache_creation = self.cache_creation.max(o.cache_creation);
    }
}

fn from_usage_obj(u: &Value) -> Usage {
    let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    Usage {
        input: n("input_tokens").max(n("prompt_tokens")),
        output: n("output_tokens").max(n("completion_tokens")),
        cache_read: n("cache_read_input_tokens"),
        cache_creation: n("cache_creation_input_tokens"),
        requests: 0,
        measured_requests: 0,
    }
}

fn from_event(v: &Value) -> Usage {
    let mut u = Usage::default();
    if let Some(x) = v.get("usage") {
        u.max_with(&from_usage_obj(x));
    }
    if let Some(x) = v.get("message").and_then(|m| m.get("usage")) {
        u.max_with(&from_usage_obj(x));
    }
    u
}

/// Scans a response body as it streams. Within one response the counts are cumulative (Anthropic sends
/// input in `message_start` and the running output in `message_delta`), so the scanner keeps the maximum of
/// each field.
#[derive(Default)]
pub struct Scanner {
    sse: bool,
    line: Vec<u8>,
    body: Vec<u8>,
    overflow: bool,
    usage: Usage,
    input_seen: bool,
    output_seen: bool,
    ended: bool,
    malformed: bool,
    anthropic: bool,
    final_output_seen: bool,
}

const MAX_JSON_BODY: usize = 8 * 1024 * 1024;
const MAX_LINE: usize = 1024 * 1024;

impl Scanner {
    /// A scanner for a body of this content type.
    pub fn new(content_type: &str) -> Self {
        Scanner {
            sse: content_type.contains("text/event-stream"),
            ..Scanner::default()
        }
    }

    /// Feeds a chunk.
    pub fn feed(&mut self, chunk: &[u8]) {
        if self.sse {
            for &b in chunk {
                if b == b'\n' {
                    self.line_done();
                } else if self.line.len() < MAX_LINE {
                    self.line.push(b);
                } else {
                    self.overflow = true;
                }
            }
        } else if self.body.len() + chunk.len() <= MAX_JSON_BODY {
            self.body.extend_from_slice(chunk);
        } else {
            self.overflow = true;
        }
    }

    fn line_done(&mut self) {
        let line = std::mem::take(&mut self.line);
        let text = String::from_utf8_lossy(&line);
        if let Some(data) = text.trim_end_matches('\r').strip_prefix("data:") {
            if data.trim() == "[DONE]" {
                self.ended = true;
            } else if let Ok(v) = serde_json::from_str::<Value>(data.trim()) {
                self.event(&v);
            } else {
                self.malformed = true;
            }
        }
    }

    fn event(&mut self, v: &Value) {
        match v.get("type").and_then(Value::as_str) {
            Some("message_start") => self.anthropic = true,
            Some("message_delta") => {
                self.final_output_seen = v
                    .get("usage")
                    .and_then(|u| u.get("output_tokens"))
                    .and_then(Value::as_u64)
                    .is_some();
            }
            _ => {}
        }
        self.usage.max_with(&from_event(v));
        for u in [
            v.get("usage"),
            v.get("message").and_then(|m| m.get("usage")),
        ]
        .into_iter()
        .flatten()
        {
            self.input_seen |= ["input_tokens", "prompt_tokens"]
                .iter()
                .any(|k| u.get(k).and_then(Value::as_u64).is_some());
            self.output_seen |= ["output_tokens", "completion_tokens"]
                .iter()
                .any(|k| u.get(k).and_then(Value::as_u64).is_some());
        }
        self.ended |= v.get("type").and_then(Value::as_str) == Some("message_stop");
    }

    /// The counts of the whole response.
    pub fn finish(self) -> Usage {
        self.finish_with_completion(true)
    }

    /// Counts remain useful on a truncated response, but its cost is not fully measured.
    pub fn finish_with_completion(mut self, complete: bool) -> Usage {
        if self.sse {
            if !self.line.is_empty() {
                self.line_done();
            }
        } else if !self.overflow
            && let Ok(v) = serde_json::from_slice::<Value>(&self.body)
        {
            self.event(&v);
        }
        self.usage.requests = 1;
        self.usage.measured_requests = u64::from(
            complete
                && !self.overflow
                && !self.malformed
                && self.input_seen
                && self.output_seen
                && (!self.sse || (self.ended && (!self.anthropic || self.final_output_seen))),
        );
        self.usage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_partial_or_truncated_usage_is_not_measured_zero() {
        for body in [br#"{}"#.as_slice(), br#"{"usage":{"input_tokens":0}}"#] {
            let mut s = Scanner::new("application/json");
            s.feed(body);
            assert_eq!(s.finish().measured_requests, 0);
        }
        let mut s = Scanner::new("application/json");
        s.feed(br#"{"usage":{"input_tokens":0,"output_tokens":0}}"#);
        assert_eq!(s.finish().measured_requests, 1);
        let mut s = Scanner::new("application/json");
        s.feed(br#"{"usage":{"input_tokens":3,"output_tokens":5}}"#);
        let partial = s.finish_with_completion(false);
        assert_eq!(
            (partial.input, partial.output, partial.measured_requests),
            (3, 5, 0)
        );

        let event = b"data: {\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":5}}\n";
        let mut s = Scanner::new("text/event-stream");
        s.feed(event);
        assert_eq!(s.finish().measured_requests, 0);
        let mut s = Scanner::new("text/event-stream");
        s.feed(event);
        s.feed(b"data: [DONE]\n");
        assert_eq!(s.finish().measured_requests, 1);
    }

    #[test]
    fn a_terminal_marker_does_not_repair_missing_or_malformed_final_usage() {
        let start = b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":3,\"output_tokens\":0}}}\n";
        let stop = b"data: {\"type\":\"message_stop\"}\n";
        for delta in [
            b"data: {broken message_delta}\n".as_slice(),
            b"",
            b"data: {\"type\":\"message_delta\"}\n",
        ] {
            let mut s = Scanner::new("text/event-stream");
            s.feed(start);
            s.feed(delta);
            s.feed(stop);
            assert_eq!(s.finish().measured_requests, 0);
        }
        let mut s = Scanner::new("text/event-stream");
        s.feed(start);
        s.feed(b"data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":0}}\n");
        s.feed(stop);
        assert_eq!(s.finish().measured_requests, 1);
    }

    #[test]
    fn sse_and_json() {
        let mut s = Scanner::new("text/event-stream; charset=utf-8");
        let body = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12,\"cache_read_input_tokens\":1500,\"cache_creation_input_tokens\":3,\"output_tokens\":1}}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":42}}\n\n";
        for c in body.as_bytes().chunks(7) {
            s.feed(c);
        }
        let u = s.finish();
        assert_eq!(
            (
                u.input,
                u.output,
                u.cache_read,
                u.cache_creation,
                u.requests
            ),
            (12, 42, 1500, 3, 1)
        );
        let mut s = Scanner::new("application/json");
        s.feed(br#"{"usage":{"input_tokens":5,"output_tokens":7}}"#);
        let u = s.finish();
        assert_eq!((u.input, u.output), (5, 7));
    }
}

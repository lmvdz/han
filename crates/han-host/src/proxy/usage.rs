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
}

impl Usage {
    /// Adds another count.
    pub fn add(&mut self, o: &Usage) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_creation += o.cache_creation;
        self.requests += o.requests;
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
        if let Some(data) = text.trim_end_matches('\r').strip_prefix("data:")
            && let Ok(v) = serde_json::from_str::<Value>(data.trim())
        {
            let u = from_event(&v);
            self.usage.max_with(&u);
        }
    }

    /// The counts of the whole response.
    pub fn finish(mut self) -> Usage {
        if self.sse {
            if !self.line.is_empty() {
                self.line_done();
            }
        } else if !self.overflow
            && let Ok(v) = serde_json::from_slice::<Value>(&self.body)
        {
            self.usage.max_with(&from_event(&v));
        }
        self.usage.requests = 1;
        self.usage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

//! The model seam: one request in, one answer out. [`FakeBackend`] answers from a script (CI never calls a
//! real model); [`OpenAiCompat`] calls an OpenAI-compatible chat endpoint such as OmniRoute's.

use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::record::Usage;

/// What the reviewer asks of a model.
#[derive(Clone, Debug)]
pub struct ModelRequest {
    /// The model, as configured.
    pub model: String,
    /// The system prompt.
    pub system: String,
    /// The user message: the changeset.
    pub user: String,
    /// The output ceiling, always set explicitly (never the backend's default).
    pub max_tokens: u32,
    /// The JSON schema the answer is held to.
    pub schema: Value,
}

/// What a model answered.
#[derive(Clone, Debug)]
pub struct ModelReply {
    /// The answer text.
    pub text: String,
    /// The model as the backend resolved it, when it said.
    pub resolved_model: Option<String>,
    /// Tokens used.
    pub usage: Usage,
    /// Whether the output stopped at `max_tokens` (the answer is cut off).
    pub truncated: bool,
}

/// Why a model call failed.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    /// The call could not be made or the backend said no. Never carries a credential.
    #[error("model backend: {0}")]
    Call(String),
}

/// A model backend.
#[async_trait]
pub trait ModelBackend: Send + Sync {
    /// The backend's name, recorded in the review record.
    fn name(&self) -> &str;
    /// Asks the model.
    async fn complete(&self, req: &ModelRequest) -> Result<ModelReply, BackendError>;
}

/// A backend that answers from a script, for tests.
pub struct FakeBackend {
    replies: Mutex<Vec<Result<ModelReply, String>>>,
    seen: Mutex<Vec<ModelRequest>>,
}

impl FakeBackend {
    /// A fake that answers each call with the next reply, in order.
    pub fn new(replies: Vec<Result<ModelReply, String>>) -> FakeBackend {
        FakeBackend {
            replies: Mutex::new(replies),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// A fake that answers once with `text`, as model `resolved`.
    pub fn answering(text: &str, resolved: Option<&str>) -> FakeBackend {
        FakeBackend::new(vec![Ok(ModelReply {
            text: text.to_owned(),
            resolved_model: resolved.map(str::to_owned),
            usage: Usage {
                input_tokens: 100,
                output_tokens: 20,
            },
            truncated: false,
        })])
    }

    /// The requests the fake has seen.
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.seen.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

#[async_trait]
impl ModelBackend for FakeBackend {
    fn name(&self) -> &str {
        "fake"
    }

    async fn complete(&self, req: &ModelRequest) -> Result<ModelReply, BackendError> {
        self.seen
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(req.clone());
        let mut q = self.replies.lock().unwrap_or_else(|p| p.into_inner());
        if q.is_empty() {
            return Err(BackendError::Call("the fake has no reply left".into()));
        }
        q.remove(0).map_err(BackendError::Call)
    }
}

/// How the key goes on the request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyStyle {
    /// `Authorization: Bearer <key>`.
    Bearer,
    /// `x-api-key: <key>`.
    XApiKey,
}

/// An OpenAI-compatible chat-completions backend: OmniRoute, or a han-host session proxy's model route in
/// front of one (then the key held here is a dummy and the proxy injects the real one).
///
/// The same fields as han-host's `ModelRoute`: a base URL, a path, how the key is sent, the key. The key is
/// read from the environment variable named in the config; it is never printed or logged, and `Debug` shows
/// nothing of it.
pub struct OpenAiCompat {
    name: String,
    url: String,
    style: KeyStyle,
    key: String,
    client: reqwest::Client,
}

impl std::fmt::Debug for OpenAiCompat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompat")
            .field("name", &self.name)
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

/// Whether `base` is `http://` to a loopback host.
fn is_loopback_http(base: &str) -> bool {
    ["http://127.0.0.1", "http://localhost", "http://[::1]"]
        .iter()
        .any(|p| {
            base.strip_prefix(p)
                .is_some_and(|r| r.is_empty() || r.starts_with(':') || r.starts_with('/'))
        })
}

impl OpenAiCompat {
    /// A backend for `base` + `path` (for example `https://host` and `/v1/chat/completions`) with the key in
    /// the environment variable `key_env`. TLS only, except to a loopback host (a local OmniRoute).
    pub fn new(
        name: &str,
        base: &str,
        path: &str,
        style: KeyStyle,
        key_env: &str,
        timeout: Duration,
    ) -> Result<OpenAiCompat, BackendError> {
        if !base.starts_with("https://") && !is_loopback_http(base) {
            return Err(BackendError::Call(
                "the model route is TLS only (http only to loopback)".into(),
            ));
        }
        if !path.starts_with('/') {
            return Err(BackendError::Call("the path starts with `/`".into()));
        }
        let key = std::env::var(key_env).map_err(|_| {
            BackendError::Call(format!("environment variable {key_env} is not set"))
        })?;
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|e| BackendError::Call(e.to_string()))?;
        Ok(OpenAiCompat {
            name: name.to_owned(),
            url: format!("{}{}", base.trim_end_matches('/'), path),
            style,
            key,
            client,
        })
    }
}

#[async_trait]
impl ModelBackend for OpenAiCompat {
    fn name(&self) -> &str {
        &self.name
    }

    async fn complete(&self, req: &ModelRequest) -> Result<ModelReply, BackendError> {
        let body = json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "temperature": 0,
            "stream": false,
            "messages": [
                {"role": "system", "content": req.system},
                {"role": "user", "content": req.user},
            ],
            "response_format": {
                "type": "json_schema",
                "json_schema": {"name": "review", "strict": true, "schema": req.schema},
            },
        });
        let mut r = self.client.post(&self.url).json(&body);
        r = match self.style {
            KeyStyle::Bearer => r.bearer_auth(&self.key),
            KeyStyle::XApiKey => r.header("x-api-key", &self.key),
        };
        let resp = r
            .send()
            .await
            .map_err(|e| BackendError::Call(scrub(&e.to_string(), &self.key)))?;
        let status = resp.status();
        let v: Value = resp
            .json()
            .await
            .map_err(|_| BackendError::Call(format!("HTTP {status}: the answer is not JSON")))?;
        if !status.is_success() {
            let msg = v["error"]["message"].as_str().unwrap_or("no message");
            return Err(BackendError::Call(scrub(
                &format!("HTTP {status}: {msg}"),
                &self.key,
            )));
        }
        let choice = &v["choices"][0];
        let text = choice["message"]["content"]
            .as_str()
            .ok_or_else(|| BackendError::Call("the answer has no content".into()))?
            .to_owned();
        Ok(ModelReply {
            text,
            resolved_model: v["model"].as_str().map(str::to_owned),
            usage: Usage {
                input_tokens: v["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
                output_tokens: v["usage"]["completion_tokens"].as_u64().unwrap_or(0),
            },
            truncated: choice["finish_reason"].as_str() == Some("length"),
        })
    }
}

fn scrub(s: &str, key: &str) -> String {
    if key.is_empty() {
        s.to_owned()
    } else {
        s.replace(key, "<redacted>")
    }
}

/// Whether the route at `base` answers: one unauthenticated `GET` of the base URL. Any HTTP status counts (the
/// route is there; the key is not sent, so it can't be refused for it). Returns the status. For `han doctor`,
/// on request only.
pub async fn probe_route(base: &str, timeout: Duration) -> Result<u16, String> {
    if !base.starts_with("https://") && !is_loopback_http(base) {
        return Err("the model route is TLS only (http only to loopback)".into());
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .map_err(|e| e.to_string())?;
    client
        .get(base)
        .send()
        .await
        .map(|r| r.status().as_u16())
        .map_err(|e| e.to_string())
}

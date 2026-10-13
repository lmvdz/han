//! Reading GitHub: installation tokens (read-only, one repository), conditional requests, pagination.
//!
//! **Reads only.** Every token asked for here has `issues: read` and nothing else. Writes (a comment, a label,
//! a PR) are han-exec actions, approved in the log, minted by han-exec (SR-49); nothing in this process can
//! write to GitHub, and nothing here logs a token or the key path.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use han_exec::GithubApp;
use han_exec::github::GithubMinter;
use octocrab::Octocrab;
use serde::Serialize;
use serde_json::Value;

use crate::config::App;

/// Installation tokens last an hour; take a new one after this.
const TOKEN_TTL: Duration = Duration::from_secs(45 * 60);

/// Why a read failed. No variant carries a URL, a token or a key path.
#[derive(Debug, thiserror::Error)]
pub enum GhError {
    /// No installation token (the App, the key or the installation is wrong).
    #[error("minting a read token: {0}")]
    Token(String),
    /// The request did not complete.
    #[error("request failed: {0}")]
    Transport(String),
    /// GitHub answered with another status than 200 or 304.
    #[error("GitHub answered HTTP {0}")]
    Status(u16),
    /// The body is not an array of issues.
    #[error("the answer is not a list of issues")]
    Shape,
    /// A `Link` header points outside the API base.
    #[error("a pagination link leaves the API base")]
    Link,
}

/// The answer to one conditional page request.
#[derive(Debug)]
pub enum Fetched {
    /// 304: nothing changed since the ETag.
    NotModified,
    /// 200.
    Page {
        /// The page's ETag.
        etag: Option<String>,
        /// The issues (pull requests are left out).
        issues: Vec<Issue>,
        /// The path (with query) of the next page, when there is one.
        next: Option<String>,
    },
}

/// An issue, as much of it as the space needs. `title` and `labels` are untrusted text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Issue {
    /// The number.
    pub number: u64,
    /// The title.
    pub title: String,
    /// `open` or `closed`.
    pub state: String,
    /// Label names.
    pub labels: Vec<String>,
    /// Assignee logins (GitHub's, for display; HAN assignment is a task).
    pub assignees: Vec<String>,
    /// The web URL.
    pub url: String,
    /// GitHub's `updated_at`, kept as the text GitHub sent (ISO 8601 UTC sorts as text).
    pub updated_at: String,
}

impl Issue {
    /// Reads an issue from GitHub's JSON; `None` for a pull request or a malformed item.
    pub fn from_json(v: &Value) -> Option<Issue> {
        if v.get("pull_request").is_some() {
            return None;
        }
        let strings = |key: &str, field: &str| -> Vec<String> {
            v.get(key)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.get(field).and_then(Value::as_str))
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        Some(Issue {
            number: v.get("number")?.as_u64()?,
            title: v.get("title")?.as_str()?.to_owned(),
            state: v.get("state")?.as_str()?.to_owned(),
            labels: strings("labels", "name"),
            assignees: strings("assignees", "login"),
            url: v
                .get("html_url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            updated_at: v.get("updated_at")?.as_str()?.to_owned(),
        })
    }
}

/// The read side of the App.
pub struct Reader {
    minter: GithubMinter,
    api_base: String,
    tokens: Mutex<HashMap<String, (Octocrab, Instant)>>,
}

impl Reader {
    /// A reader that signs with the App's key file.
    pub fn from_config(app: &App) -> Result<Reader, String> {
        let exec_app = GithubApp {
            app_id: app.app_id,
            installation_id: app.installation_id,
            api_base: app.api_base.clone(),
            private_key: app.private_key.clone(),
        };
        Ok(Reader::new(GithubMinter::from_app(&exec_app)?))
    }

    /// A reader over a given minter (tests give it a fixed JWT).
    pub fn new(minter: GithubMinter) -> Reader {
        let api_base = minter.api_base().to_owned();
        Reader {
            minter,
            api_base,
            tokens: Mutex::default(),
        }
    }

    async fn client(&self, repo: &str) -> Result<Octocrab, GhError> {
        if let Some((c, at)) = self
            .tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(repo)
            && at.elapsed() < TOKEN_TTL
        {
            return Ok(c.clone());
        }
        let token = self
            .minter
            .token(repo, &[("issues", "read")])
            .await
            .map_err(GhError::Token)?;
        let client = Octocrab::builder()
            .base_uri(self.api_base.as_str())
            .map_err(|_| GhError::Transport("bad API base".into()))?
            .personal_token(token.expose().to_owned())
            .build()
            .map_err(|_| GhError::Transport("building the client".into()))?;
        self.tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(repo.to_owned(), (client.clone(), Instant::now()));
        Ok(client)
    }

    /// The first page of `repo`'s issues, every state, most recently updated first.
    pub fn first_page(repo: &str) -> String {
        format!("/repos/{repo}/issues?state=all&sort=updated&direction=desc&per_page=100")
    }

    /// One conditional GET of `path` (a path with query under the API base) for `repo`.
    pub async fn get(
        &self,
        repo: &str,
        path: &str,
        etag: Option<&str>,
    ) -> Result<Fetched, GhError> {
        let client = self.client(repo).await?;
        let mut headers = http::HeaderMap::new();
        if let Some(e) = etag
            && let Ok(v) = http::HeaderValue::from_str(e)
        {
            headers.insert(http::header::IF_NONE_MATCH, v);
        }
        let resp = client
            ._get_with_headers(path, Some(headers))
            .await
            .map_err(|_| GhError::Transport("sending the request".into()))?;
        let status = resp.status().as_u16();
        match status {
            304 => Ok(Fetched::NotModified),
            200 => {
                let etag = header(&resp, "etag");
                let next = match header(&resp, "link").and_then(|l| next_link(&l)) {
                    Some(url) => Some(self.relative(&url)?),
                    None => None,
                };
                let body = client
                    .body_to_string(resp)
                    .await
                    .map_err(|_| GhError::Transport("reading the body".into()))?;
                let v: Value = serde_json::from_str(&body).map_err(|_| GhError::Shape)?;
                let items = v.as_array().ok_or(GhError::Shape)?;
                Ok(Fetched::Page {
                    etag,
                    issues: items.iter().filter_map(Issue::from_json).collect(),
                    next,
                })
            }
            401 => {
                self.tokens
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(repo);
                Err(GhError::Status(status))
            }
            other => Err(GhError::Status(other)),
        }
    }

    /// A `Link` target as a path under the API base; a link elsewhere is refused (the token must not follow it).
    fn relative(&self, url: &str) -> Result<String, GhError> {
        url.strip_prefix(self.api_base.as_str())
            .filter(|p| p.starts_with('/'))
            .map(str::to_owned)
            .ok_or(GhError::Link)
    }
}

fn header<B>(resp: &http::Response<B>, name: &str) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// The `rel="next"` target of a `Link` header.
pub fn next_link(link: &str) -> Option<String> {
    link.split(',').find_map(|part| {
        let (url, params) = part.split_once(';')?;
        let is_next = params
            .split(';')
            .any(|p| p.trim().replace(' ', "") == "rel=\"next\"");
        let url = url.trim().strip_prefix('<')?.strip_suffix('>')?;
        is_next.then(|| url.to_owned())
    })
}

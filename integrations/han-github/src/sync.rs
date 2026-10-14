//! The sync loop: poll a repository, surface what changed as `github.issue/1` items, and suggest a task for
//! an issue that carries the trigger label.
//!
//! The process is an actor over the node API ([`NodeApi`], the same trait the JSON-RPC client implements). It
//! opens one session per repository (titled `github:owner/repo`) and publishes its items on that session's
//! stream, which only the session's actor may do (T-07). It creates a task only for an open issue with the
//! trigger label (or on a human's request) and only when no task with that `external` exists; it never
//! assigns a session to anything. An agent that takes the issue makes its own signed task. It cancels a task it
//! made when the issue is closed or loses the label before anyone accepted the task.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use han_node::api::{
    BoxFuture, CallResult, Command, NodeApi, NodeError, Query, Row, TaskRow, ViewEvent, Window,
};
use serde_json::{Value, json};

use crate::config::Config;
use crate::github::{Fetched, GhError, Issue, Reader};

/// The profile name of the items.
pub const PROFILE: &str = "github.issue/1";

/// The most issues in one item (an item is at most 64 KiB with its envelope).
const CHUNK: usize = 50;

/// Longest title, label and count of labels in an item.
const MAX_TITLE: usize = 256;
const MAX_LABEL: usize = 64;
const MAX_LABELS: usize = 20;

/// How long to wait for a view's first snapshot.
const VIEW_TIMEOUT: Duration = Duration::from_secs(5);

/// Why a poll or a request failed.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// Reading GitHub failed.
    #[error(transparent)]
    GitHub(#[from] GhError),
    /// The node refused or could not be reached.
    #[error("node: {0}")]
    Node(String),
}

impl From<NodeError> for SyncError {
    fn from(e: NodeError) -> Self {
        SyncError::Node(e.to_string())
    }
}

/// What the reader needs to be: something that does conditional GETs. `Reader` is the real one.
pub trait Source: Send + Sync {
    /// One conditional GET (see [`Reader::get`]).
    fn get<'a>(
        &'a self,
        repo: &'a str,
        path: &'a str,
        etag: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Fetched, GhError>>;
}

impl Source for Reader {
    fn get<'a>(
        &'a self,
        repo: &'a str,
        path: &'a str,
        etag: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Fetched, GhError>> {
        Box::pin(Reader::get(self, repo, path, etag))
    }
}

#[derive(Default)]
struct RepoState {
    session: Option<String>,
    etag: Option<String>,
    /// The greatest `updated_at` seen: later pages are only read while they reach back to it.
    watermark: Option<String>,
    known: HashMap<u64, Issue>,
    seq: u64,
}

/// What one poll did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Tick {
    /// The page-1 request was answered 304.
    pub not_modified: bool,
    /// Issues published (new or changed).
    pub published: usize,
    /// Tasks created.
    pub tasks: Vec<String>,
    /// Tasks cancelled because their issue was closed or lost the trigger label before anyone accepted them.
    pub cancelled: Vec<String>,
}

/// The state of every repository and the means to poll them.
pub struct Syncer<S: Source> {
    api: std::sync::Arc<dyn NodeApi>,
    source: S,
    space: String,
    label: String,
    to: Vec<String>,
    max_pages: u32,
    repos: HashMap<String, RepoState>,
    /// External references this process already made a task for (the Tasks view may lag behind).
    requested: HashSet<String>,
    /// The references a human asked for with [`Syncer::request`]: the label does not govern those.
    asked: HashSet<String>,
    /// The actor of this process's node (the author of the tasks it made), once known.
    me: Option<String>,
}

impl<S: Source> Syncer<S> {
    /// A syncer for the repositories of `cfg`.
    pub fn new(cfg: &Config, api: std::sync::Arc<dyn NodeApi>, source: S) -> Self {
        Syncer {
            api,
            source,
            space: cfg.space.clone(),
            label: cfg.tasks.label.clone(),
            to: cfg.tasks.addressees(),
            max_pages: cfg.max_pages,
            repos: cfg
                .repos
                .iter()
                .map(|r| (r.clone(), RepoState::default()))
                .collect(),
            requested: HashSet::new(),
            asked: HashSet::new(),
            me: None,
        }
    }

    /// The configured repositories, in no particular order.
    pub fn repos(&self) -> Vec<String> {
        self.repos.keys().cloned().collect()
    }

    /// Polls `repo` once. State moves forward only after everything was published, so a failure repeats the
    /// work on the next poll (publishing the same issue twice is harmless: a face keeps the item with the
    /// highest `seq`).
    pub async fn poll(&mut self, repo: &str) -> Result<Tick, SyncError> {
        let Some(state) = self.repos.get(repo) else {
            return Err(SyncError::Node(format!("not a configured repo: {repo}")));
        };
        let (etag, watermark) = (state.etag.clone(), state.watermark.clone());

        // Page 1 is conditional; the next pages are read only if page 1 changed. Sorted by `updated_at`
        // descending, any change to an issue moves it to page 1, so an unchanged page 1 means no change.
        let mut path = Reader::first_page(repo);
        let mut new_etag = None;
        let mut seen: Vec<Issue> = Vec::new();
        for page_no in 0..self.max_pages {
            let cond = if page_no == 0 { etag.as_deref() } else { None };
            match self.source.get(repo, &path, cond).await? {
                Fetched::NotModified => {
                    return Ok(Tick {
                        not_modified: true,
                        ..Tick::default()
                    });
                }
                Fetched::Page { etag, issues, next } => {
                    if page_no == 0 {
                        new_etag = etag;
                    }
                    let reaches_back = match (&watermark, issues.last()) {
                        (Some(w), Some(last)) => last.updated_at.as_str() >= w.as_str(),
                        (None, Some(_)) => true,
                        (_, None) => false,
                    };
                    seen.extend(issues);
                    match next {
                        Some(n) if reaches_back => path = n,
                        _ => break,
                    }
                }
            }
        }

        let state = &self.repos[repo];
        let changed: Vec<Issue> = seen
            .into_iter()
            .filter(|i| state.known.get(&i.number) != Some(i))
            .collect();
        let session = self.session(repo).await?;
        let mut seq = self.repos[repo].seq;
        for chunk in changed.chunks(CHUNK) {
            seq += 1;
            let body = item_body(repo, chunk, seq, &self.label);
            self.api
                .call(Command::StreamPublish {
                    space: self.space.clone(),
                    session: session.clone(),
                    name: PROFILE.into(),
                    body: body.to_string(),
                })
                .await?;
            // Commit per chunk, so a later failure does not republish what is out.
            let state = self.repos.get_mut(repo).expect("configured");
            state.seq = seq;
            for i in chunk {
                state.known.insert(i.number, i.clone());
            }
        }

        let candidates: Vec<&Issue> = changed
            .iter()
            .filter(|i| i.state == "open" && i.labels.contains(&self.label))
            .collect();
        let tasks = self.suggest(repo, &candidates).await?;
        let stale: Vec<String> = changed
            .iter()
            .filter(|i| i.state != "open" || !i.labels.contains(&self.label))
            .map(|i| format!("github:{repo}#{}", i.number))
            .collect();
        let cancelled = self.cancel_stale(&stale).await?;

        let state = self.repos.get_mut(repo).expect("configured");
        state.etag = new_etag;
        if let Some(max) = changed.iter().map(|i| i.updated_at.clone()).max()
            && state.watermark.as_ref().is_none_or(|w| *w < max)
        {
            state.watermark = Some(max);
        }
        Ok(Tick {
            not_modified: false,
            published: changed.len(),
            tasks,
            cancelled,
        })
    }

    /// Asks for a task on `reference` (`github:owner/repo#N`) because a human did. Returns the task's id, or
    /// `None` when the reference already has one.
    pub async fn request(
        &mut self,
        reference: &str,
        title: &str,
    ) -> Result<Option<String>, SyncError> {
        let existing = self.task_externals().await?;
        if existing.contains(reference) || self.requested.contains(reference) {
            return Ok(None);
        }
        let id = self.create_task(reference, title).await?;
        self.asked.insert(reference.to_owned());
        Ok(Some(id))
    }

    /// Cancels the open tasks this process's node made for `references` (issues that were closed or lost the
    /// trigger label) that nobody accepted yet. A task an agent accepted is left to its assignee, and a task
    /// of another author is not ours to cancel (only the author may).
    async fn cancel_stale(&mut self, references: &[String]) -> Result<Vec<String>, SyncError> {
        if references.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self.task_rows().await?;
        let ours: Vec<&TaskRow> = rows
            .iter()
            .filter(|t| {
                t.open
                    && t.accepted.is_empty()
                    && t.external
                        .as_ref()
                        .is_some_and(|e| references.contains(e) && !self.asked.contains(e))
            })
            .collect();
        if ours.is_empty() {
            return Ok(Vec::new());
        }
        let me = self.actor().await?;
        let mut done = Vec::new();
        for t in ours.into_iter().filter(|t| t.author == me) {
            self.api
                .call(Command::TaskCancel {
                    space: self.space.clone(),
                    task: t.task.clone(),
                })
                .await?;
            done.push(t.task.clone());
        }
        Ok(done)
    }

    /// The actor of the node this process talks to.
    async fn actor(&mut self) -> Result<String, SyncError> {
        if let Some(me) = &self.me {
            return Ok(me.clone());
        }
        let mut stream = self.api.view(Query::NodeStatus {
            window: Window::default(),
        });
        let first = tokio::time::timeout(VIEW_TIMEOUT, stream.next())
            .await
            .map_err(|_| SyncError::Node("the NodeStatus view did not answer".into()))?;
        let me = match first {
            Some(ViewEvent::Snapshot { rows, .. }) => rows.into_iter().find_map(|r| match r {
                Row::NodeStatus(n) => Some(n.actor),
                _ => None,
            }),
            Some(ViewEvent::Failed { error, .. }) => return Err(error.into()),
            _ => None,
        }
        .ok_or_else(|| SyncError::Node("the node did not say who it acts as".into()))?;
        self.me = Some(me.clone());
        Ok(me)
    }

    async fn suggest(
        &mut self,
        repo: &str,
        candidates: &[&Issue],
    ) -> Result<Vec<String>, SyncError> {
        if candidates.is_empty() || self.to.is_empty() {
            return Ok(Vec::new());
        }
        let existing = self.task_externals().await?;
        let mut made = Vec::new();
        for issue in candidates {
            let reference = format!("github:{repo}#{}", issue.number);
            if existing.contains(&reference) || self.requested.contains(&reference) {
                continue;
            }
            self.create_task(&reference, &issue.title).await?;
            made.push(reference);
        }
        Ok(made)
    }

    async fn create_task(&mut self, reference: &str, title: &str) -> Result<String, SyncError> {
        // The title is an issue author's text: it travels in `ask` as data, like any task text (SR-54).
        let ask = format!("Work on {reference}: {}", clean(title, 200));
        let res = self
            .api
            .call(Command::TaskCreate {
                space: self.space.clone(),
                to: self.to.clone(),
                ask,
                external: Some(reference.to_owned()),
            })
            .await?;
        self.requested.insert(reference.to_owned());
        Ok(match res {
            CallResult::Receipt { id, .. } | CallResult::Queued { id, .. } => id,
            _ => String::new(),
        })
    }

    async fn session(&mut self, repo: &str) -> Result<String, SyncError> {
        if let Some(s) = &self.repos[repo].session {
            return Ok(s.clone());
        }
        let res = self
            .api
            .call(Command::SessionOpen {
                space: self.space.clone(),
                steer_mode: None,
                task: None,
                title: Some(format!("github:{repo}")),
            })
            .await?;
        let id = match res {
            CallResult::Receipt { id, .. } => id,
            _ => return Err(SyncError::Node("session.open was not ordered".into())),
        };
        self.repos.get_mut(repo).expect("configured").session = Some(id.clone());
        Ok(id)
    }

    /// Closes the sessions this process opened (best effort, at shutdown).
    pub async fn close(&mut self) {
        for state in self.repos.values_mut() {
            if let Some(session) = state.session.take() {
                let _ = self
                    .api
                    .call(Command::SessionClose {
                        space: self.space.clone(),
                        session,
                    })
                    .await;
            }
        }
    }

    /// Every `external` of the space's tasks, open or not.
    async fn task_externals(&self) -> Result<HashSet<String>, SyncError> {
        Ok(self
            .task_rows()
            .await?
            .into_iter()
            .filter_map(|t| t.external)
            .collect())
    }

    /// The space's tasks, open or not.
    async fn task_rows(&self) -> Result<Vec<TaskRow>, SyncError> {
        let mut out = Vec::new();
        let mut cursor = None;
        for _ in 0..50 {
            let mut stream = self.api.view(Query::Tasks {
                space: self.space.clone(),
                assignee: None,
                window: Window {
                    limit: 1000,
                    cursor: cursor.clone(),
                    ..Window::default()
                },
            });
            let first = tokio::time::timeout(VIEW_TIMEOUT, stream.next())
                .await
                .map_err(|_| SyncError::Node("the Tasks view did not answer".into()))?;
            let (rows, next) = match first {
                Some(ViewEvent::Snapshot {
                    rows, next_cursor, ..
                }) => (rows, next_cursor),
                Some(ViewEvent::Failed { error, .. }) => return Err(error.into()),
                _ => return Err(SyncError::Node("the Tasks view ended".into())),
            };
            out.extend(rows.into_iter().filter_map(|r| match r {
                Row::Task(t) => Some(t),
                _ => None,
            }));
            match next {
                Some(n) => cursor = Some(n),
                None => break,
            }
        }
        Ok(out)
    }
}

/// The body of a `github.issue/1` item (see the README).
fn item_body(repo: &str, issues: &[Issue], seq: u64, trigger: &str) -> Value {
    let issues: Vec<Value> = issues
        .iter()
        .map(|i| {
            json!({
                "ref": format!("github:{repo}#{}", i.number),
                "number": i.number,
                "title": clean(&i.title, MAX_TITLE),
                "state": i.state,
                "labels": i.labels.iter().take(MAX_LABELS).map(|l| clean(l, MAX_LABEL)).collect::<Vec<_>>(),
                "assignees": i.assignees.iter().take(MAX_LABELS).map(|a| clean(a, MAX_LABEL)).collect::<Vec<_>>(),
                "url": i.url,
                "updated_at": i.updated_at,
                "agent_ready": i.state == "open" && i.labels.iter().any(|l| l == trigger),
            })
        })
        .collect();
    json!({"repo": format!("github:{repo}"), "seq": seq, "issues": issues})
}

/// Text without control characters, at most `max` characters.
fn clean(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect()
}

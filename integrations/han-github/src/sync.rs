//! The sync loop: poll a repository, surface what changed as `github.issue/1` items, and suggest a task for
//! an issue that carries the trigger label.
//!
//! The process is an actor over the node API ([`NodeApi`], the same trait the JSON-RPC client implements). It
//! opens one session per repository (titled `github:owner/repo`) and publishes its items on that session's
//! stream, which only the session's actor may do (T-07). It creates a task only for an open issue with the
//! trigger label (or on a human's request) and only when it has no open task (see [`needs_task`] for which
//! closed tasks do not count); it never assigns a session to anything. An agent that takes the issue makes its
//! own signed task. It cancels a task it made when the issue is closed or loses the label before anyone
//! accepted the task.

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

/// Longest issue body carried in a task's ask, in bytes (the ask may be 32 KiB; the body is untrusted text and
/// the agent needs the gist, not a novel).
pub const MAX_BODY_BYTES: usize = 4096;

/// The most tasks that may end without landing (failed, declined, cancelled) before an issue waits for a person
/// instead of getting yet another one.
pub const MAX_ATTEMPTS: usize = 3;

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
                    // Nothing changed on GitHub, but a task may have closed since the last poll.
                    let tasks = self.reconcile(repo).await?;
                    return Ok(Tick {
                        not_modified: true,
                        tasks,
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

        // Every known agent-ready issue, not only the changed ones: a task that closed since the last poll
        // leaves its issue unchanged.
        let tasks = self.reconcile(repo).await?;
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
    /// `None` when the reference already has an open task. A person overrides the wait that a landed task or
    /// [`MAX_ATTEMPTS`] failures put on the poll.
    pub async fn request(
        &mut self,
        reference: &str,
        title: &str,
    ) -> Result<Option<String>, SyncError> {
        let rows = self.task_rows().await?;
        self.forget_seen(&rows);
        if rows
            .iter()
            .any(|t| t.open && t.external.as_deref() == Some(reference))
            || self.requested.contains(reference)
        {
            return Ok(None);
        }
        let id = self.create_task(reference, title, None).await?;
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

    /// Makes a task for each known issue that is open, has the trigger label and needs one.
    async fn reconcile(&mut self, repo: &str) -> Result<Vec<String>, SyncError> {
        if self.to.is_empty() {
            return Ok(Vec::new());
        }
        let mut candidates: Vec<Issue> = self.repos[repo]
            .known
            .values()
            .filter(|i| i.state == "open" && i.labels.contains(&self.label))
            .cloned()
            .collect();
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        // Most recently updated first, as GitHub lists them.
        candidates.sort_by(|a, b| (&b.updated_at, b.number).cmp(&(&a.updated_at, a.number)));
        let rows = self.task_rows().await?;
        self.forget_seen(&rows);
        let mut made = Vec::new();
        for issue in &candidates {
            let reference = format!("github:{repo}#{}", issue.number);
            if self.requested.contains(&reference) || !needs_task(&rows, &reference) {
                continue;
            }
            self.create_task(&reference, &issue.title, Some(&issue.body))
                .await?;
            made.push(reference);
        }
        Ok(made)
    }

    /// Drops the references the Tasks view now shows from `requested`: that set only covers the lag of the
    /// view, and a reference that stays in it would never get a second task after its first one closed.
    fn forget_seen(&mut self, rows: &[TaskRow]) {
        self.requested.retain(|r| {
            !rows
                .iter()
                .any(|t| t.external.as_deref() == Some(r.as_str()))
        });
    }

    async fn create_task(
        &mut self,
        reference: &str,
        title: &str,
        body: Option<&str>,
    ) -> Result<String, SyncError> {
        // The title and body are an issue author's text: they travel in `ask` as data, like any task text
        // (SR-54). The ask is fixed when the task is made; a later edit of the issue does not change it.
        let ask = task_ask(reference, title, body);
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

/// Whether an open, agent-ready issue with the tasks `rows` needs a new task. The rule:
///
/// * a task for the reference is open: no (somebody has it);
/// * a task for it closed as `done` while the issue is still open: no, that waits for a person (the work was
///   reported finished, so whether the issue is really settled is theirs to say; `han-github request` makes a
///   new task on purpose);
/// * [`MAX_ATTEMPTS`] tasks ended without landing: no, a person looks at why before another try;
/// * otherwise yes. This is the case of a task that failed, was declined or cancelled, or was closed by an
///   owner whose assignee's session died, and it also covers a closure the node reports without a state.
pub fn needs_task(rows: &[TaskRow], reference: &str) -> bool {
    let mine: Vec<&TaskRow> = rows
        .iter()
        .filter(|t| t.external.as_deref() == Some(reference))
        .collect();
    if mine.iter().any(|t| t.open) {
        return false;
    }
    if mine.iter().any(|t| t.closed_as.as_deref() == Some("done")) {
        return false;
    }
    mine.len() < MAX_ATTEMPTS
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

/// The ask of a task made for an issue: the title line, then the body (if any) between fixed delimiters.
/// Control characters (but newlines and tabs in the body) are dropped, the body is cut at [`MAX_BODY_BYTES`]
/// on a character boundary, and the delimiters' words are defused inside it so a body cannot close the block
/// early.
pub fn task_ask(reference: &str, title: &str, body: Option<&str>) -> String {
    const BEGIN: &str = "----- BEGIN ISSUE TEXT (untrusted, from the issue's author) -----";
    const END: &str = "----- END ISSUE TEXT -----";
    let mut ask = format!("Work on {reference}: {}", clean(title, 200));
    let text = body.map(clean_body).unwrap_or_default();
    let text = text.trim();
    if !text.is_empty() {
        ask.push_str(&format!("\n\n{BEGIN}\n{text}\n{END}"));
    }
    ask
}

/// An issue body without control characters (newlines and tabs stay; a carriage return goes), without
/// anything that looks like the delimiters, cut at [`MAX_BODY_BYTES`] bytes (with a marker when cut).
fn clean_body(s: &str) -> String {
    let s = s.replace("ISSUE TEXT", "ISSUE_TEXT");
    let mut out = String::new();
    let mut cut = false;
    for c in s
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
    {
        if out.len() + c.len_utf8() > MAX_BODY_BYTES {
            cut = true;
            break;
        }
        out.push(c);
    }
    if cut {
        out.push_str("\n[truncated]");
    }
    out
}

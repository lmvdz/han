//! `view(Query)`: what a face wants to watch. Every query takes a window.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::DEFAULT_LIMIT;
use super::error::NodeError;
use super::rows::SessionState;

/// The largest window of most views.
pub const MAX_LIMIT: u32 = 10_000;

/// The largest window of a `Timeline` view (points).
pub const MAX_TIMELINE_LIMIT: u32 = 2_000;

/// The largest window of a `Session` view (items).
pub const MAX_SESSION_LIMIT: u32 = 200;

fn default_limit() -> u32 {
    DEFAULT_LIMIT
}

/// Which rows a window keeps. Every row type answers the facets it has; a facet a row lacks does not match.
///
/// **Grammar (Q-DAT-11).** Within one key, any value matches; across keys, all must. Unknown members are an
/// error, not ignored: a filter that silently drops a key shows rows the user asked to hide.
///
/// * `person`, `agent`, `actor`: an actor id, or just its `@local` part (`@ada` matches `@ada:a.example`).
///   `person` is who the work is for, `agent` the actor running it, `actor` either.
/// * `harness`: the agent program, case-insensitively.
/// * `repo`: `github:owner/name`, `owner/name` or `name`, case-insensitively.
/// * `label`: a label of the row, case-insensitively (the node has no label source yet, so none matches).
/// * `path`: globs on the files a session changed. `*` is any run of characters but `/`, `**` any run
///   including `/` (`**/` may also be empty), `?` one character but `/`; a pattern ending in `/` covers
///   everything under it. Case-sensitive.
/// * `overlap_min`: 1 to 3, the lowest overlap tier the row is in.
/// * `mine`: only the device's own work (it is the person or the agent).
/// * `text`: every word must occur, case-insensitively, in the row's searchable text.
///
/// The text form (`person:@ada state:working "two words"`) is [`Filter::parse`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    /// Free text: every word must occur, case-insensitively, in the row's searchable text.
    #[serde(default)]
    #[ts(optional)]
    pub text: Option<String>,
    /// Only these states (empty: any).
    #[serde(default)]
    pub states: Vec<SessionState>,
    /// Only rows that involve this actor (a person, an agent or a delegate).
    #[serde(default)]
    #[ts(optional)]
    pub actor: Option<String>,
    /// Only rows about this issue.
    #[serde(default)]
    #[ts(optional)]
    pub issue: Option<String>,
    /// Include closed sessions (a board shows live work by default).
    #[serde(default)]
    pub include_closed: bool,
    /// Only rows that wait on this device's actor (`needs_me`): a blocked or approval-waiting session of its
    /// own work, an effect it may approve.
    #[serde(default)]
    pub needs_me: bool,
    /// Only work for these people (empty: any).
    #[serde(default)]
    pub person: Vec<String>,
    /// Only sessions run by these agents (empty: any).
    #[serde(default)]
    pub agent: Vec<String>,
    /// Only these harnesses (empty: any).
    #[serde(default)]
    pub harness: Vec<String>,
    /// Only these repositories (empty: any).
    #[serde(default)]
    pub repo: Vec<String>,
    /// Only rows with one of these labels (empty: any).
    #[serde(default)]
    pub label: Vec<String>,
    /// Only sessions that changed a file matching one of these globs (empty: any).
    #[serde(default)]
    pub path: Vec<String>,
    /// Only rows in an overlap of at least this tier (1 to 3).
    #[serde(default)]
    #[ts(optional)]
    pub overlap_min: Option<u8>,
    /// Only this device's own work.
    #[serde(default)]
    pub mine: bool,
}

/// The facets of a row that a [`Filter`] reads.
#[derive(Clone, Debug, Default)]
pub struct Facets {
    /// The row's state, if it has one.
    pub state: Option<SessionState>,
    /// The actors involved.
    pub actors: Vec<String>,
    /// The issue.
    pub issue: Option<String>,
    /// Whether the row is about a closed session.
    pub closed: bool,
    /// Whether the row waits on this device's actor.
    pub needs_me: bool,
    /// Searchable text.
    pub text: String,
    /// The person the work is for.
    pub person: Option<String>,
    /// The actor running the session.
    pub agent: Option<String>,
    /// The harness.
    pub harness: Option<String>,
    /// The repository.
    pub repo: Option<String>,
    /// Labels.
    pub labels: Vec<String>,
    /// The files the session changed.
    pub paths: Vec<String>,
    /// The highest tier of the overlaps the row is in (0: none).
    pub overlap_tier: u8,
    /// Whether the row is this device's own work.
    pub mine: bool,
}

/// The longest glob a filter may hold.
const MAX_GLOB: usize = 256;

/// The most `*` a glob may hold (matching is recursive).
const MAX_STARS: usize = 8;

/// The most values a key may hold.
const MAX_VALUES: usize = 64;

/// Whether `value` names `actor`: the full id, or its `@local` part.
fn actor_is(value: &str, actor: &str) -> bool {
    actor == value
        || actor
            .strip_prefix(value)
            .is_some_and(|r| r.starts_with(':'))
}

/// The repository without its scheme (`github:`).
fn bare_repo(r: &str) -> &str {
    r.split_once(':').map_or(r, |(_, rest)| rest)
}

fn repo_is(value: &str, repo: &str) -> bool {
    let (v, r) = (bare_repo(value), bare_repo(repo));
    v.eq_ignore_ascii_case(r)
        || r.rsplit_once('/')
            .is_some_and(|(_, name)| name.eq_ignore_ascii_case(v))
}

/// Glob matching for `path` (see [`Filter`]).
pub fn glob_match(pattern: &str, path: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') if p.get(1) == Some(&'*') => {
                let mut rest = &p[2..];
                while rest.first() == Some(&'*') {
                    rest = &rest[1..];
                }
                // `**/` may also match no directory at all.
                if rest.first() == Some(&'/') && go(&rest[1..], t) {
                    return true;
                }
                (0..=t.len()).any(|i| go(rest, &t[i..]))
            }
            Some('*') => {
                let rest = &p[1..];
                let mut i = 0;
                loop {
                    if go(rest, &t[i..]) {
                        return true;
                    }
                    if i < t.len() && t[i] != '/' {
                        i += 1;
                    } else {
                        return false;
                    }
                }
            }
            Some('?') => t.first().is_some_and(|c| *c != '/') && go(&p[1..], &t[1..]),
            Some(c) => t.first() == Some(c) && go(&p[1..], &t[1..]),
        }
    }
    let mut pat = pattern.to_owned();
    if pat.ends_with('/') {
        pat.push_str("**");
    }
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = path.chars().collect();
    go(&p, &t)
}

fn bool_value(key: &str, v: &str) -> Result<bool, NodeError> {
    match v.to_ascii_lowercase().as_str() {
        "" | "true" | "yes" | "1" => Ok(true),
        "false" | "no" | "0" => Ok(false),
        _ => Err(NodeError::invalid(format!(
            "filter key '{key}' takes true or false, not '{v}'"
        ))),
    }
}

fn state_value(v: &str) -> Result<SessionState, NodeError> {
    serde_json::from_value(serde_json::Value::String(v.to_owned()))
        .map_err(|_| NodeError::invalid(format!("unknown state '{v}' in the filter")))
}

/// Splits a filter box into terms on spaces; `"..."` keeps spaces (and a `key:` before it keeps its key).
fn terms(text: &str) -> Result<Vec<(Option<String>, String, bool)>, NodeError> {
    // (key, value, quoted)
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            return Ok(out);
        }
        let mut word = String::new();
        let mut key: Option<String> = None;
        let mut quoted = false;
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() {
                break;
            }
            chars.next();
            match c {
                '"' => {
                    quoted = true;
                    let mut closed = false;
                    for q in chars.by_ref() {
                        if q == '"' {
                            closed = true;
                            break;
                        }
                        word.push(q);
                    }
                    if !closed {
                        return Err(NodeError::invalid("a quote in the filter is not closed"));
                    }
                }
                ':' if key.is_none() && !quoted && !word.is_empty() => {
                    key = Some(std::mem::take(&mut word));
                }
                c => word.push(c),
            }
        }
        out.push((key, word, quoted));
    }
}

impl Filter {
    /// Parses the text form of a filter: space-separated `key:value` terms (`"..."` keeps spaces), repeated
    /// keys add values, and any other word is free text.
    ///
    /// Keys: `person`, `agent`, `actor`, `harness`, `repo`, `issue`, `label`, `path`, `state`, `overlap` (or
    /// `overlap_min`), `mine`, `needs_me`, `closed` (booleans take `true` or `false`, and `is:mine`,
    /// `is:closed`, `is:needs_me` are the same), `text`. A word before a `:` that is none of these is an
    /// `invalid` error, so a mistyped key is never taken for text; quote a word to search for one with a colon.
    pub fn parse(text: &str) -> Result<Filter, NodeError> {
        let mut f = Filter::default();
        let mut words: Vec<String> = Vec::new();
        for (key, value, _quoted) in terms(text)? {
            let Some(key) = key else {
                words.push(value);
                continue;
            };
            let k = key.to_ascii_lowercase().replace(['-', '_'], "");
            let need = |v: &str| {
                if v.is_empty() {
                    Err(NodeError::invalid(format!(
                        "filter key '{key}' has no value"
                    )))
                } else {
                    Ok(v.to_owned())
                }
            };
            match k.as_str() {
                "person" => f.person.push(need(&value)?),
                "agent" => f.agent.push(need(&value)?),
                "actor" => f.actor = Some(need(&value)?),
                "harness" => f.harness.push(need(&value)?),
                "repo" => f.repo.push(need(&value)?),
                "issue" => f.issue = Some(need(&value)?),
                "label" => f.label.push(need(&value)?),
                "path" => f.path.push(need(&value)?),
                "state" => {
                    let s = state_value(&need(&value)?)?;
                    if !f.states.contains(&s) {
                        f.states.push(s);
                    }
                }
                "overlap" | "overlapmin" => {
                    let n: u8 = need(&value)?.parse().map_err(|_| {
                        NodeError::invalid(format!(
                            "overlap takes a tier from 1 to 3, not '{value}'"
                        ))
                    })?;
                    f.overlap_min = Some(n);
                }
                "mine" => f.mine = bool_value(&key, &value)?,
                "needsme" => f.needs_me = bool_value(&key, &value)?,
                "closed" => f.include_closed = bool_value(&key, &value)?,
                "is" => match need(&value)?
                    .to_ascii_lowercase()
                    .replace(['-', '_'], "")
                    .as_str()
                {
                    "mine" => f.mine = true,
                    "closed" => f.include_closed = true,
                    "needsme" => f.needs_me = true,
                    other => {
                        return Err(NodeError::invalid(format!(
                            "unknown filter 'is:{other}' (mine, closed or needs_me)"
                        )));
                    }
                },
                "text" => words.push(need(&value)?),
                _ => {
                    return Err(NodeError::invalid(format!(
                        "unknown filter key '{key}' (person, agent, actor, harness, repo, issue, label, path, state, overlap, mine, needs_me, closed, text)"
                    )));
                }
            }
        }
        if !words.is_empty() {
            f.text = Some(words.join(" "));
        }
        f.validate()?;
        Ok(f)
    }

    /// Checks what serde cannot: the tier of `overlap_min`, empty values, and the size and shape of globs.
    pub fn validate(&self) -> Result<(), NodeError> {
        if let Some(n) = self.overlap_min
            && !(1..=3).contains(&n)
        {
            return Err(NodeError::invalid(format!(
                "overlap_min is a tier from 1 to 3, not {n}"
            )));
        }
        for (key, values) in [
            ("person", &self.person),
            ("agent", &self.agent),
            ("harness", &self.harness),
            ("repo", &self.repo),
            ("label", &self.label),
            ("path", &self.path),
        ] {
            if values.len() > MAX_VALUES {
                return Err(NodeError::invalid(format!(
                    "filter key '{key}' holds more than {MAX_VALUES} values"
                )));
            }
            if values.iter().any(|v| v.is_empty()) {
                return Err(NodeError::invalid(format!(
                    "filter key '{key}' has an empty value"
                )));
            }
        }
        for g in &self.path {
            if g.chars().count() > MAX_GLOB || g.matches('*').count() > MAX_STARS {
                return Err(NodeError::invalid(format!(
                    "the glob '{g}' is longer than {MAX_GLOB} characters or has more than {MAX_STARS} stars"
                )));
            }
        }
        Ok(())
    }

    /// Whether a row with these facets is in the window.
    pub fn matches(&self, f: &Facets) -> bool {
        if f.closed && !self.include_closed {
            return false;
        }
        if !self.states.is_empty() && !f.state.is_some_and(|s| self.states.contains(&s)) {
            return false;
        }
        if self.needs_me && !f.needs_me {
            return false;
        }
        if self.mine && !f.mine {
            return false;
        }
        if let Some(a) = &self.actor
            && !f.actors.iter().any(|x| actor_is(a, x))
        {
            return false;
        }
        if let Some(i) = &self.issue
            && f.issue.as_deref() != Some(i.as_str())
        {
            return false;
        }
        let any = |values: &[String], has: &dyn Fn(&str) -> bool| {
            values.is_empty() || values.iter().any(|v| has(v))
        };
        if !any(&self.person, &|v| {
            f.person.as_deref().is_some_and(|p| actor_is(v, p))
        }) || !any(&self.agent, &|v| {
            f.agent.as_deref().is_some_and(|a| actor_is(v, a))
        }) || !any(&self.harness, &|v| {
            f.harness
                .as_deref()
                .is_some_and(|h| h.eq_ignore_ascii_case(v))
        }) || !any(&self.repo, &|v| {
            f.repo.as_deref().is_some_and(|r| repo_is(v, r))
        }) || !any(&self.label, &|v| {
            f.labels.iter().any(|l| l.eq_ignore_ascii_case(v))
        }) || !any(&self.path, &|v| f.paths.iter().any(|p| glob_match(v, p)))
        {
            return false;
        }
        if let Some(n) = self.overlap_min
            && f.overlap_tier < n
        {
            return false;
        }
        if let Some(t) = &self.text {
            let hay = f.text.to_lowercase();
            if !t
                .split_whitespace()
                .all(|w| hay.contains(&w.to_lowercase()))
            {
                return false;
            }
        }
        true
    }
}

/// The part of a view a face sees: a filter, a size and a place. Rows are ordered by key; `cursor` is the key
/// of the last row of the page before, so scrolling is moving the cursor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Window {
    /// Which rows.
    #[serde(default)]
    pub filter: Filter,
    /// At most this many rows (1 to 10,000).
    #[serde(default = "default_limit")]
    pub limit: u32,
    /// Rows after this key.
    #[serde(default)]
    #[ts(optional)]
    pub cursor: Option<String>,
}

impl Default for Window {
    fn default() -> Self {
        Window {
            filter: Filter::default(),
            limit: DEFAULT_LIMIT,
            cursor: None,
        }
    }
}

/// How the board groups its rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    /// By issue.
    #[default]
    Issue,
    /// By person.
    Person,
    /// By state.
    State,
}

/// What `WhoIsOn` asks about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Target {
    /// An issue, `github:owner/repo#123`.
    Issue {
        /// The reference.
        issue: String,
    },
    /// A path; a prefix ending in `/` matches what is under it.
    Path {
        /// The path.
        path: String,
    },
}

/// What `Cost` adds up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scope {
    /// Every session of a space, with a total.
    Space {
        /// The space.
        space: String,
    },
    /// One session.
    Session {
        /// The session's entry id.
        session: String,
    },
}

/// Something a face wants to watch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Query {
    /// Person by agent by issue, one row per session.
    Board {
        /// The space.
        space: String,
        /// How rows are grouped.
        #[serde(default)]
        group: Group,
        /// Groups (the `group` label of their rows) the face has collapsed: their rows are left out of the
        /// window and the total.
        #[serde(default)]
        collapsed: Vec<String>,
        /// The window.
        #[serde(default)]
        window: Window,
    },
    /// One session in full.
    Session {
        /// The session's entry id.
        id: String,
        /// The window.
        #[serde(default)]
        window: Window,
    },
    /// Who is working on an issue or a path.
    WhoIsOn {
        /// The space; every followed space when absent.
        #[serde(default)]
        #[ts(optional)]
        space: Option<String>,
        /// The issue or path.
        target: Target,
        /// The window.
        #[serde(default)]
        window: Window,
    },
    /// Where two pieces of work meet: claims and leases (tier 1), touched hunks (tier 2).
    Overlaps {
        /// The space.
        space: String,
        /// The window.
        #[serde(default)]
        window: Window,
    },
    /// Effects waiting for an approval that this actor may give.
    Approvals {
        /// The approving actor.
        actor: String,
        /// The window.
        #[serde(default)]
        window: Window,
    },
    /// The spaces this device follows, one status row each: what `:space` offers to switch to.
    Spaces {
        /// The window.
        #[serde(default)]
        window: Window,
    },
    /// How a space is doing on this device: link, head, outbox.
    SpaceStatus {
        /// The space.
        space: String,
        /// The window.
        #[serde(default)]
        window: Window,
    },
    /// Tokens and cost.
    Cost {
        /// What to add up.
        scope: Scope,
        /// The window.
        #[serde(default)]
        window: Window,
    },
    /// Entries signed on this device and their way to the home: where a `Queued` result's receipt or refusal
    /// shows up later.
    Outbox {
        /// Only this space; every space when absent.
        #[serde(default)]
        #[ts(optional)]
        space: Option<String>,
        /// The window.
        #[serde(default)]
        window: Window,
    },
    /// How joining a space stands (connecting, waiting for an owner to admit this actor, syncing, ready).
    JoinProgress {
        /// The space.
        space: String,
        /// The window.
        #[serde(default)]
        window: Window,
    },
    /// The node itself: actor, node id, version, links, outbox, supervised roles.
    NodeStatus {
        /// The window.
        #[serde(default)]
        window: Window,
    },
    /// The points of a session's work timeline (backed by han-timeline; unavailable before M4).
    Timeline {
        /// The session.
        session: String,
        /// Only points at or before this time (time travel).
        #[serde(default)]
        #[ts(optional)]
        at: Option<String>,
        /// The window (pages of up to 2,000 points).
        #[serde(default)]
        window: Window,
    },
    /// Per-file patches of a session between two timeline points (unavailable before M4).
    Patches {
        /// The session.
        session: String,
        /// The earlier point; the session's base when absent.
        #[serde(default)]
        #[ts(optional)]
        from: Option<String>,
        /// The later point.
        to: String,
        /// The window.
        #[serde(default)]
        window: Window,
    },
    /// The steering inputs of a session (`session.input` with their receipts), oldest first: what its host
    /// turns into user turns (R-24). Only the session's host should act on them.
    Inputs {
        /// The session.
        session: String,
        /// The window (its cursor is the key of the last input handled).
        #[serde(default)]
        window: Window,
    },
    /// Tasks, optionally only those assigned to one actor.
    Tasks {
        /// The space.
        space: String,
        /// Only tasks with this assignee.
        #[serde(default)]
        #[ts(optional)]
        assignee: Option<String>,
        /// The window.
        #[serde(default)]
        window: Window,
    },
    /// Whether an actor is guarded in a space, as this device folds the log (R-24): it chose guarding (a
    /// session opened `guarded`, until its guard's `agent.unguard`) or the owners force non-humans.
    Guard {
        /// The space.
        space: String,
        /// The actor.
        actor: String,
        /// The window.
        #[serde(default)]
        window: Window,
    },
    /// Effects with their plans, as an executor reads them: the proposals it may run, their approvals and
    /// claims, and the space's launch window (X-03).
    Effects {
        /// Only this space; every followed space when absent.
        #[serde(default)]
        #[ts(optional)]
        space: Option<String>,
        /// Only effects whose executor is this actor.
        #[serde(default)]
        #[ts(optional)]
        executor: Option<String>,
        /// Include effects in a final phase.
        #[serde(default)]
        include_done: bool,
        /// The window.
        #[serde(default)]
        window: Window,
    },
    /// The full change set of an effect: a push plan or a merge preview (unavailable before M4).
    ChangeSet {
        /// The space.
        space: String,
        /// The proposal's entry id.
        proposal: String,
        /// The window.
        #[serde(default)]
        window: Window,
    },
}

impl Query {
    /// The most rows a window of this query may ask for. A larger `limit` is cut to it (Q-DAT-7): pages of
    /// up to 2,000 timeline points, 200 session items and 10,000 rows of any other view.
    pub fn max_limit(&self) -> u32 {
        match self {
            Query::Timeline { .. } => MAX_TIMELINE_LIMIT,
            Query::Session { .. } => MAX_SESSION_LIMIT,
            _ => MAX_LIMIT,
        }
    }

    /// The window of the query.
    pub fn window(&self) -> &Window {
        match self {
            Query::Board { window, .. }
            | Query::Session { window, .. }
            | Query::WhoIsOn { window, .. }
            | Query::Overlaps { window, .. }
            | Query::Approvals { window, .. }
            | Query::Spaces { window }
            | Query::SpaceStatus { window, .. }
            | Query::Cost { window, .. }
            | Query::Outbox { window, .. }
            | Query::JoinProgress { window, .. }
            | Query::NodeStatus { window }
            | Query::Timeline { window, .. }
            | Query::Patches { window, .. }
            | Query::Effects { window, .. }
            | Query::Guard { window, .. }
            | Query::Inputs { window, .. }
            | Query::Tasks { window, .. }
            | Query::ChangeSet { window, .. } => window,
        }
    }
}

//! `view(Query)`: what a face wants to watch. Every query takes a window.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::DEFAULT_LIMIT;
use super::rows::SessionState;

fn default_limit() -> u32 {
    DEFAULT_LIMIT
}

/// Which rows a window keeps. Every row type answers the facets it has; a facet a row lacks does not match.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
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
    /// Searchable text.
    pub text: String,
}

impl Filter {
    /// Whether a row with these facets is in the window.
    pub fn matches(&self, f: &Facets) -> bool {
        if f.closed && !self.include_closed {
            return false;
        }
        if !self.states.is_empty() && !f.state.is_some_and(|s| self.states.contains(&s)) {
            return false;
        }
        if let Some(a) = &self.actor
            && !f.actors.iter().any(|x| x == a)
        {
            return false;
        }
        if let Some(i) = &self.issue
            && f.issue.as_deref() != Some(i.as_str())
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
    /// The window of the query.
    pub fn window(&self) -> &Window {
        match self {
            Query::Board { window, .. }
            | Query::Session { window, .. }
            | Query::WhoIsOn { window, .. }
            | Query::Overlaps { window, .. }
            | Query::Approvals { window, .. }
            | Query::SpaceStatus { window, .. }
            | Query::Cost { window, .. }
            | Query::Outbox { window, .. }
            | Query::JoinProgress { window, .. }
            | Query::NodeStatus { window }
            | Query::Timeline { window, .. }
            | Query::Patches { window, .. }
            | Query::ChangeSet { window, .. } => window,
        }
    }
}

//! The state of the board: the window the node sends (a snapshot, then diffs), the grouping, the filter, the
//! collapsed groups, the selection and the moving of the window as the selection nears its edge.
//!
//! The node orders and groups the rows and says how many there are in all; this module never computes a
//! display state, an overlap tier or an order (board spec BRD-24). It only lays out what it is given.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::time::Instant;

use han_node::api::{
    BoardRow, Filter, Group, NodeError, OverlapKind, Query, Row, ViewEvent, ViewMirror, Window,
};

use crate::filter;

/// How near the edge of the window the selection may come before the window moves.
const MARGIN: usize = 5;

/// What a selection points at. Selection is tracked by id, not by position, so a row that moves keeps focus
/// (BRD-32).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryId {
    /// A group header, by the group's label.
    Group(String),
    /// A session row, by the session's id.
    Session(String),
}

/// One line of the board: a group header or a session row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// A group header.
    Group {
        /// The group as the node names it.
        group: String,
        /// How many sessions of the group are in the window (the last count seen when collapsed).
        rows: u32,
        /// Whether the group is collapsed.
        collapsed: bool,
        /// The highest overlap tier among its rows (0: none).
        tier: u8,
        /// How many of its rows need the local user.
        needs_me: u32,
    },
    /// A session row, by its index in [`BoardState::rows`].
    Row(usize),
}

/// Where the selection goes.
#[derive(Clone, Copy, Debug)]
pub enum Nav {
    /// By so many entries.
    Delta(isize),
    /// The first entry.
    First,
    /// The last entry (the window moves there first).
    Last,
}

/// Collapsing remembers where the header sat among the groups, because the node leaves a collapsed group's
/// rows out and the header has nothing else to be placed by.
#[derive(Clone, Copy, Debug)]
struct Collapsed {
    position: usize,
    rows: u32,
}

/// The node's steady clock is not the face's, and the Board view carries no "now". The face estimates it as the
/// latest activity it has seen, advanced by its own clock since it saw it. Ages are therefore lower bounds
/// right after the first snapshot and exact once any session moves.
#[derive(Clone, Copy, Debug, Default)]
struct AgeClock {
    node_ms: u64,
    seen: Option<Instant>,
}

impl AgeClock {
    fn estimate(&self, now: Instant) -> u64 {
        match self.seen {
            Some(at) => self.node_ms + now.saturating_duration_since(at).as_millis() as u64,
            None => 0,
        }
    }

    fn observe(&mut self, latest: u64, now: Instant) {
        if self.seen.is_none() || latest > self.estimate(now) {
            self.node_ms = latest;
            self.seen = Some(now);
        }
    }
}

/// The board of one space.
#[derive(Debug)]
pub struct BoardState {
    mirror: ViewMirror,
    rows: Vec<BoardRow>,
    /// How rows are grouped.
    pub group: Group,
    /// The filter box as typed.
    pub filter_text: String,
    /// The filter as sent to the node.
    pub filter: Filter,
    /// What was wrong with the filter box.
    pub filter_errors: Vec<String>,
    collapsed: BTreeMap<String, Collapsed>,
    cursor: Option<String>,
    /// Cursors of window starts the face has seen: absolute index to the key before it.
    known: BTreeMap<u32, String>,
    /// The size of the window asked for.
    pub limit: u32,
    selected: Option<EntryId>,
    sel_index: usize,
    loading: bool,
    want_last: bool,
    /// Whether a first snapshot has arrived.
    pub synced: bool,
    /// Why the view failed, if it did.
    pub error: Option<NodeError>,
    age: AgeClock,
    /// The rows that fit on screen; set by the drawing code, read by the page keys.
    pub page: Cell<usize>,
    /// The first entry on screen; kept by the drawing code so the view does not jump.
    pub top: Cell<usize>,
}

impl BoardState {
    /// An empty board with this grouping.
    pub fn new(group: Group) -> Self {
        BoardState {
            mirror: ViewMirror::new(),
            rows: Vec::new(),
            group,
            filter_text: String::new(),
            filter: Filter::default(),
            filter_errors: Vec::new(),
            collapsed: BTreeMap::new(),
            cursor: None,
            known: BTreeMap::new(),
            limit: 100,
            selected: None,
            sel_index: 0,
            loading: false,
            want_last: false,
            synced: false,
            error: None,
            age: AgeClock::default(),
            page: Cell::new(10),
            top: Cell::new(0),
        }
    }

    /// The query for this board.
    pub fn query(&self, space: &str) -> Query {
        Query::Board {
            space: space.to_owned(),
            group: self.group,
            collapsed: self.collapsed.keys().cloned().collect(),
            window: Window {
                filter: self.filter.clone(),
                limit: self.limit,
                cursor: self.cursor.clone(),
            },
        }
    }

    /// The rows in the window, in the node's order.
    pub fn rows(&self) -> &[BoardRow] {
        &self.rows
    }

    /// How many sessions match in all.
    pub fn total(&self) -> u32 {
        self.mirror.total()
    }

    /// How many matching sessions come before the window.
    pub fn offset(&self) -> u32 {
        self.mirror.offset()
    }

    /// The row of a session, if it is in the window.
    pub fn row_of(&self, session: &str) -> Option<&BoardRow> {
        self.rows.iter().find(|r| r.session == session)
    }

    /// The age of a row, in milliseconds.
    pub fn age_ms(&self, row: &BoardRow, now: Instant) -> u64 {
        self.age.estimate(now).saturating_sub(row.last_active_ms)
    }

    /// Whether a window request is in flight.
    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// Forgets everything the node sent (the grouping, the filter or the space changed).
    fn reset_window(&mut self) {
        self.cursor = None;
        self.known.clear();
        self.want_last = false;
        self.loading = true;
    }

    /// Applies one event of the board view. True when the face wants to move the window (it was asked to go to
    /// the end and the end is not here yet).
    pub fn apply(&mut self, event: ViewEvent, now: Instant) -> bool {
        if let ViewEvent::Failed { error } = event {
            self.error = Some(error);
            self.loading = false;
            return false;
        }
        self.mirror.apply(event);
        self.error = None;
        self.synced = true;
        self.loading = false;
        self.rows = self
            .mirror
            .rows()
            .into_iter()
            .filter_map(|r| match r {
                Row::Board(b) => Some(b),
                _ => None,
            })
            .collect();
        if let Some(latest) = self.rows.iter().map(|r| r.last_active_ms).max() {
            self.age.observe(latest, now);
        }
        // Remember where each row of this window sits, so the window can come back to it.
        let offset = self.mirror.offset();
        if let Some(c) = &self.cursor {
            self.known.insert(offset, c.clone());
        }
        for i in 1..self.rows.len() {
            self.known
                .insert(offset + i as u32, self.rows[i - 1].key.clone());
        }
        self.keep_selection();
        if self.want_last {
            if let Some(next) = self.mirror.next_cursor() {
                self.cursor = Some(next.to_owned());
                self.loading = true;
                return true;
            }
            self.want_last = false;
            if let Some(last) = self.entries().len().checked_sub(1) {
                self.select_index(last);
            }
        }
        false
    }

    /// Puts the selection back on its entry, or on the entry now at its old position.
    fn keep_selection(&mut self) {
        let entries = self.entries();
        if entries.is_empty() {
            self.selected = None;
            self.sel_index = 0;
            return;
        }
        let at = self
            .selected
            .as_ref()
            .and_then(|id| entries.iter().position(|e| self.id_of(e) == *id));
        match at {
            Some(i) => self.sel_index = i,
            None => {
                let i = self.sel_index.min(entries.len() - 1);
                self.select_index(i);
            }
        }
    }

    fn id_of(&self, e: &Entry) -> EntryId {
        match e {
            Entry::Group { group, .. } => EntryId::Group(group.clone()),
            Entry::Row(i) => EntryId::Session(self.rows[*i].session.clone()),
        }
    }

    fn select_index(&mut self, i: usize) {
        let entries = self.entries();
        if let Some(e) = entries.get(i) {
            self.selected = Some(self.id_of(e));
            self.sel_index = i;
        }
    }

    /// The lines of the board: a header before the first row of each group, collapsed groups as a header alone.
    pub fn entries(&self) -> Vec<Entry> {
        // The groups present, in order.
        let mut groups: Vec<(&str, Vec<usize>)> = Vec::new();
        for (i, r) in self.rows.iter().enumerate() {
            match groups.last_mut() {
                Some((g, v)) if *g == r.group => v.push(i),
                _ => groups.push((r.group.as_str(), vec![i])),
            }
        }
        let mut headers: Vec<Entry> = Vec::new();
        let mut rows_of: Vec<Vec<usize>> = Vec::new();
        for (g, idx) in &groups {
            let tier = idx
                .iter()
                .flat_map(|i| self.rows[*i].overlaps.iter().map(|k| k.tier()))
                .max()
                .unwrap_or(0);
            headers.push(Entry::Group {
                group: (*g).to_owned(),
                rows: idx.len() as u32,
                collapsed: false,
                tier,
                needs_me: idx.iter().filter(|i| self.rows[**i].needs_me).count() as u32,
            });
            rows_of.push(idx.clone());
        }
        // Collapsed groups have no rows; put each header back where it sat.
        let mut collapsed: Vec<(&String, &Collapsed)> = self.collapsed.iter().collect();
        collapsed.sort_by_key(|(_, c)| c.position);
        for (g, c) in collapsed {
            let at = c.position.min(headers.len());
            headers.insert(
                at,
                Entry::Group {
                    group: g.clone(),
                    rows: c.rows,
                    collapsed: true,
                    tier: 0,
                    needs_me: 0,
                },
            );
            rows_of.insert(at, Vec::new());
        }
        let mut out = Vec::new();
        for (h, idx) in headers.into_iter().zip(rows_of) {
            out.push(h);
            out.extend(idx.into_iter().map(Entry::Row));
        }
        out
    }

    /// The selected entry's id.
    pub fn selected(&self) -> Option<&EntryId> {
        self.selected.as_ref()
    }

    /// The selected session row, if a row is selected.
    pub fn selected_row(&self) -> Option<&BoardRow> {
        match &self.selected {
            Some(EntryId::Session(s)) => self.row_of(s),
            _ => None,
        }
    }

    /// The group the selection is in (a header is in its own group).
    fn selected_group(&self) -> Option<String> {
        match &self.selected {
            Some(EntryId::Group(g)) => Some(g.clone()),
            Some(EntryId::Session(s)) => self.row_of(s).map(|r| r.group.clone()),
            None => None,
        }
    }

    /// Selects a session by id, if it is in the window.
    pub fn select_session(&mut self, session: &str) -> bool {
        let entries = self.entries();
        match entries
            .iter()
            .position(|e| matches!(e, Entry::Row(i) if self.rows[*i].session == session))
        {
            Some(i) => {
                self.select_index(i);
                true
            }
            None => false,
        }
    }

    /// Selects a group's header.
    pub fn select_header(&mut self, group: &str) {
        let entries = self.entries();
        if let Some(i) = entries
            .iter()
            .position(|e| matches!(e, Entry::Group { group: g, .. } if g == group))
        {
            self.select_index(i);
        }
    }

    /// Moves the selection. True when the window has to move to follow it.
    pub fn nav(&mut self, nav: Nav) -> bool {
        let entries = self.entries();
        if entries.is_empty() {
            return false;
        }
        let last = entries.len() - 1;
        let target = match nav {
            Nav::Delta(d) => (self.sel_index as isize + d).clamp(0, last as isize) as usize,
            Nav::First => {
                if self.mirror.offset() > 0 {
                    self.reset_window();
                    self.selected = None;
                    self.sel_index = 0;
                    return true;
                }
                0
            }
            Nav::Last => {
                if let Some(next) = self.mirror.next_cursor() {
                    self.want_last = true;
                    self.cursor = Some(next.to_owned());
                    self.loading = true;
                    return true;
                }
                last
            }
        };
        self.select_index(target);
        self.slide_if_near_edge()
    }

    /// Moves the window by half its size when the selection is within [`MARGIN`] rows of an edge.
    fn slide_if_near_edge(&mut self) -> bool {
        if self.loading {
            return false;
        }
        let n = self.rows.len();
        let Some(EntryId::Session(s)) = self.selected.clone() else {
            return false;
        };
        let Some(at) = self.rows.iter().position(|r| r.session == s) else {
            return false;
        };
        let offset = self.mirror.offset();
        if at + MARGIN >= n && self.mirror.next_cursor().is_some() && n >= 2 {
            let half = n / 2;
            self.cursor = Some(self.rows[half - 1].key.clone());
            self.loading = true;
            return true;
        }
        if at < MARGIN && offset > 0 {
            let start = offset.saturating_sub(self.limit / 2);
            self.cursor = if start == 0 {
                None
            } else {
                self.known.get(&start).cloned()
            };
            self.loading = true;
            return true;
        }
        false
    }

    /// Collapses or opens the selected group. The window is asked again, because the node leaves a collapsed
    /// group's rows out of it.
    pub fn toggle_group(&mut self) -> bool {
        let Some(group) = self.selected_group() else {
            return false;
        };
        if self.collapsed.remove(&group).is_none() {
            let entries = self.entries();
            let position = entries
                .iter()
                .filter_map(|e| match e {
                    Entry::Group { group: g, .. } => Some(g),
                    Entry::Row(_) => None,
                })
                .position(|g| *g == group)
                .unwrap_or(0);
            let rows = self.rows.iter().filter(|r| r.group == group).count() as u32;
            self.collapsed
                .insert(group.clone(), Collapsed { position, rows });
        }
        self.selected = Some(EntryId::Group(group));
        self.cursor = None;
        self.known.clear();
        self.loading = true;
        true
    }

    /// Changes the grouping.
    pub fn set_group(&mut self, group: Group) {
        self.group = group;
        self.collapsed.clear();
        self.selected = None;
        self.sel_index = 0;
        self.reset_window();
    }

    /// Parses and applies the filter box.
    pub fn set_filter(&mut self, text: &str, me: Option<&str>) {
        let parsed = filter::parse(text, me);
        self.filter_text = text.trim().to_owned();
        self.filter = parsed.filter;
        self.filter_errors = parsed.errors;
        self.selected = None;
        self.sel_index = 0;
        self.reset_window();
    }

    /// Whether closed sessions are included.
    pub fn include_closed(&self) -> bool {
        self.filter.include_closed
    }

    /// Asks for closed sessions (or not) without touching the rest of the filter.
    pub fn set_include_closed(&mut self, on: bool) {
        self.filter.include_closed = on;
        self.selected = None;
        self.sel_index = 0;
        self.reset_window();
    }

    /// Whether the filter keeps any session out.
    pub fn is_filtered(&self) -> bool {
        self.filter != Filter::default()
    }

    /// Sets the number of rows asked for. True when the window has to be asked again (it grew).
    pub fn set_limit(&mut self, limit: u32) -> bool {
        let grew = limit > self.limit;
        self.limit = limit;
        grew
    }

    /// The word and the tier of the overlap badge: the highest tier of the row (claim, hunk), and how many
    /// overlaps there are (BRD-6). The node knows no "conflict" tier yet.
    pub fn badge(row: &BoardRow) -> Option<(&'static str, u8, usize)> {
        let tier = row.overlaps.iter().map(|k| k.tier()).max()?;
        let word = if tier >= 2 { "hunk" } else { "claim" };
        Some((word, tier, row.overlaps.len()))
    }

    /// The kinds of overlap of a row in words, for the detail line.
    pub fn overlap_words(kinds: &[OverlapKind]) -> String {
        let mut words: Vec<&str> = kinds
            .iter()
            .map(|k| match k {
                OverlapKind::IssueClaim => "same issue",
                OverlapKind::PathLease => "path lease",
                OverlapKind::SameFile => "same file",
                OverlapKind::HunkRange => "same lines",
            })
            .collect();
        words.dedup();
        words.join(", ")
    }
}

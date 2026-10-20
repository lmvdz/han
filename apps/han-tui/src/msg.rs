//! What flows between the three parts of the TUI: the terminal and the node produce [`Msg`]s, the [`App`]
//! (pure state) answers each with [`Action`]s, and the host (`host.rs`) carries the actions out.
//!
//! [`App`]: crate::app::App

use std::time::Instant;

use han_node::api::{CallResult, Command, NodeError, Query, ViewEvent};
use ratatui::crossterm::event::KeyEvent;

/// The views the TUI keeps open. Each has at most one subscription at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ViewKind {
    /// The board of the current space.
    Board,
    /// Tasks in the current space.
    Tasks,
    /// The node itself: the actor, the roles, the links. Doubles as the connection heartbeat.
    Node,
    /// How the current space is doing on this device.
    Space,
    /// The spaces this device follows (what `:space` switches between).
    Spaces,
    /// Effects waiting for the local actor.
    Approvals,
    /// The open session.
    Session,
    /// The steering inputs of the open session.
    Inputs,
    /// The typed items of the open session's stream (messages, tool calls, plans).
    Items,
    /// The work timeline of the open session.
    Timeline,
    /// The patches of the open session's latest point.
    Patches,
}

/// Something that happened.
#[derive(Debug)]
pub enum Msg {
    /// A key press.
    Key(KeyEvent),
    /// Pasted text.
    Paste(String),
    /// The terminal changed size.
    Resize(u16, u16),
    /// Time passes (about once a second); carries the clock so tests can drive it.
    Tick(Instant),
    /// A view produced an event. `epoch` says which subscription it came from, so late events of a replaced
    /// subscription are ignored.
    View {
        /// The view.
        view: ViewKind,
        /// The subscription's generation.
        epoch: u64,
        /// The event.
        event: ViewEvent,
    },
    /// A view's stream ended without a failure (the connection went away).
    ViewEnded {
        /// The view.
        view: ViewKind,
        /// The subscription's generation.
        epoch: u64,
    },
    /// A call finished.
    Called {
        /// The id the app gave the call.
        id: u64,
        /// How it ended.
        result: Result<CallResult, NodeError>,
    },
    /// The host is trying to reach the node again.
    Connecting {
        /// Which attempt this is (1 for the first).
        attempt: u32,
    },
    /// The host holds a working connection; the app opens its views.
    Connected,
}

/// Something the app wants done.
#[derive(Debug, PartialEq)]
pub enum Action {
    /// Opens (or replaces) the subscription of a view.
    Subscribe {
        /// The view.
        view: ViewKind,
        /// The generation events must carry.
        epoch: u64,
        /// The query.
        query: Box<Query>,
    },
    /// Closes the subscription of a view.
    Unsubscribe(ViewKind),
    /// Sends a command.
    Call {
        /// An id for matching the result.
        id: u64,
        /// The command.
        command: Command,
    },
    /// Writes the config file.
    SaveConfig(crate::config::Config),
    /// Drops the connection and dials the node again until it answers.
    Reconnect,
    /// Leaves the program.
    Quit,
}

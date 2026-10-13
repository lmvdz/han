//! Fan-out (T-01): the live side of `read` with follow, `subscribe` and `watch`.
//!
//! The sequencer pushes items into a bounded queue per subscription while it holds its lock, so a push never
//! waits. A consumer that falls behind is cut off (`unavailable`, section 3) and re-reads.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use han_core::json::Value;
use tokio::sync::mpsc;

/// Items a subscription may hold before it is cut off.
const QUEUE: usize = 10_000;

/// What a live call yields next.
#[derive(Debug, PartialEq)]
pub enum Next {
    /// A `ReadItem` or `StreamItem`.
    Item(Value),
    /// The subscriber fell too far behind: answer `unavailable` and end the call.
    Overflow,
    /// The call ended (cancelled, or the connection closed).
    Ended,
}

/// The receiving end of a live call.
pub struct Live {
    rx: mpsc::Receiver<Value>,
    overflow: Arc<AtomicBool>,
}

impl Live {
    /// The next item, waiting for it.
    pub async fn next(&mut self) -> Next {
        match self.rx.recv().await {
            Some(item) => Next::Item(item),
            None if self.overflow.load(Ordering::SeqCst) => Next::Overflow,
            None => Next::Ended,
        }
    }
}

/// A registered subscription.
pub(crate) struct Sub {
    pub id: u64,
    pub actor: String,
    /// Records at or below this seq were part of the backlog.
    pub cutoff: u64,
    /// Whether signals are delivered (`subscribe` only, G-09).
    pub signals: bool,
    /// The connection that made it: a signal is not echoed to the sender's own connection.
    pub conn: u64,
    tx: mpsc::Sender<Value>,
    overflow: Arc<AtomicBool>,
}

impl Sub {
    /// Another handle on the same subscription (same id, same queue).
    pub fn duplicate(&self) -> Sub {
        Sub {
            id: self.id,
            actor: self.actor.clone(),
            cutoff: self.cutoff,
            signals: self.signals,
            conn: self.conn,
            tx: self.tx.clone(),
            overflow: Arc::clone(&self.overflow),
        }
    }

    /// Queues an item; false when the subscription is dead and should be dropped.
    pub fn push(&self, item: Value) -> bool {
        match self.tx.try_send(item) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.overflow.store(true, Ordering::SeqCst);
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }
}

/// The registry of live subscriptions of one sequencer.
#[derive(Default)]
pub(crate) struct Subs {
    next_id: u64,
    /// Records (and signals) of a space.
    pub by_space: HashMap<String, Vec<Sub>>,
    /// An actor's subscriptions to its own profile space: where notices go (G-03).
    pub notices: HashMap<String, Vec<Sub>>,
    /// `watch`ers of a session.
    pub watchers: HashMap<(String, String), Vec<Sub>>,
}

impl Subs {
    /// A new subscription and its receiving end.
    pub fn make(&mut self, actor: &str, cutoff: u64, signals: bool, conn: u64) -> (Sub, Live) {
        self.next_id += 1;
        let (tx, rx) = mpsc::channel(QUEUE);
        let overflow = Arc::new(AtomicBool::new(false));
        (
            Sub {
                id: self.next_id,
                actor: actor.to_owned(),
                cutoff,
                signals,
                conn,
                tx,
                overflow: Arc::clone(&overflow),
            },
            Live { rx, overflow },
        )
    }

    /// Ends a subscription.
    pub fn remove(&mut self, id: u64) {
        for list in self.by_space.values_mut() {
            list.retain(|s| s.id != id);
        }
        for list in self.notices.values_mut() {
            list.retain(|s| s.id != id);
        }
        for list in self.watchers.values_mut() {
            list.retain(|s| s.id != id);
        }
    }
}

/// A handle on a subscription that a connection keeps so that it can end it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SubId(pub(crate) u64);

/// Receives what a home would hand to other servers: hooks for federation and for views (M3). All methods
/// default to doing nothing, and are called with the home's lock held, so they must not block.
pub trait Hooks: Send + Sync + 'static {
    /// After a record is ordered and durable (not on a dedupe hit).
    fn ordered(&self, _record: &han_core::LogRecord, _state: &han_core::policy::SpaceState) {}

    /// A first-contact notice (G-03) for `actor` on another server `server`: it signals
    /// `profile:<actor>` there with `{type: "notice", space, seq}` from `@:<this server>`. Notices for actors
    /// on this server are delivered by the home itself.
    fn notice(&self, _server: &str, _actor: &str, _space: &str, _seq: u64) {}
}

/// Hooks that do nothing.
pub struct NoHooks;

impl Hooks for NoHooks {}

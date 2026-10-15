//! The two methods as a trait, and the stream a view yields.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use futures_core::Stream;

use super::{CallResult, Command, ConnectionClass, NodeError, Query, ViewEvent};

/// A boxed future, so [`NodeApi`] can be used as a trait object.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The node API (PLAN section 5): the whole surface a face sees. The runtime implements it in process; the
/// JSON-RPC client implements it over the local pipe, so a face does not care which it holds.
pub trait NodeApi: Send + Sync + 'static {
    /// Does something. It ends as a receipt, a verdict or an error.
    fn call(&self, cmd: Command) -> BoxFuture<'_, Result<CallResult, NodeError>>;

    /// Watches something: a snapshot of the window, then diffs, until the stream is dropped.
    fn view(&self, query: Query) -> ViewStream;

    /// Whether a face connection must confirm a person before it governs or steers
    /// ([`Command::needs_presence`]). The RPC server asks at each such command; the node answers from its
    /// presence setting. An implementation with no device behind it (a test double) needs nobody.
    fn presence_required(&self) -> Result<bool, NodeError> {
        Ok(false)
    }

    /// Asks the person, on this device, to unlock a face connection, and returns how long the unlock lasts.
    /// The RPC server calls it for one connection and keeps the window on that connection alone.
    fn confirm_unlock(&self) -> BoxFuture<'_, Result<Duration, NodeError>> {
        Box::pin(async { Err(NodeError::unavailable("this node has no presence check")) })
    }
}

/// A role process's own node, as the role may use it: a host or an executor that runs its own node in process
/// (as its own actor) holds this and never the node itself, so the class bounds what it can send however it
/// asks (SR-06, SR-106). The same bound the role endpoints put on a connection, plus the one Face-class
/// command a role needs in process: a host uploads its `git_push` changeset (`BlobPut`), an executor fetches
/// it (`BlobFetch`). Views are not bounded.
pub struct ClassApi {
    inner: Arc<dyn NodeApi>,
    class: ConnectionClass,
}

impl ClassApi {
    /// `inner` as a connection of `class` sees it.
    pub fn new(inner: Arc<dyn NodeApi>, class: ConnectionClass) -> Self {
        Self { inner, class }
    }

    fn permits(&self, cmd: &Command) -> bool {
        self.class.permits(cmd.class())
            || matches!(
                (self.class, cmd),
                (ConnectionClass::Host, Command::BlobPut { .. })
                    | (ConnectionClass::Executor, Command::BlobFetch { .. })
            )
    }
}

impl NodeApi for ClassApi {
    fn call(&self, cmd: Command) -> BoxFuture<'_, Result<CallResult, NodeError>> {
        if !self.permits(&cmd) {
            let e = NodeError::denied(format!(
                "a {:?} connection may not send a {:?} command",
                self.class,
                cmd.class()
            ));
            return Box::pin(async move { Err(e) });
        }
        self.inner.call(cmd)
    }

    fn view(&self, query: Query) -> ViewStream {
        self.inner.view(query)
    }
}

/// A view's backlog may hold this many events. A reader further behind than this is not served the backlog:
/// the view drops it, says [`ViewEvent::Resync`] and ends, so a slow face takes a new snapshot instead of the
/// node's memory growing without bound (DAT-9). The bound counts events, not bytes.
pub const MAX_BACKLOG: usize = 256;

struct State {
    queue: VecDeque<ViewEvent>,
    waker: Option<Waker>,
    sinks: usize,
    reader_gone: bool,
    resynced: bool,
}

struct Shared(Mutex<State>);

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// The sending end of a view stream.
pub struct ViewSink(Arc<Shared>);

impl Clone for ViewSink {
    fn clone(&self) -> Self {
        self.0.lock().sinks += 1;
        ViewSink(Arc::clone(&self.0))
    }
}

impl Drop for ViewSink {
    fn drop(&mut self) {
        let mut st = self.0.lock();
        st.sinks -= 1;
        if st.sinks == 0
            && let Some(w) = st.waker.take()
        {
            w.wake();
        }
    }
}

impl ViewSink {
    /// Sends an event; false when the reader is gone, or has fallen so far behind that the view ended with a
    /// [`ViewEvent::Resync`] (the sender drops the view then).
    pub fn send(&self, event: ViewEvent) -> bool {
        let mut st = self.0.lock();
        if st.reader_gone || st.resynced {
            return false;
        }
        let terminal = matches!(event, ViewEvent::Failed { .. } | ViewEvent::End { .. });
        if st.queue.len() >= MAX_BACKLOG && !terminal {
            let seq = event.seq();
            st.queue.clear();
            st.queue.push_back(ViewEvent::Resync { seq });
            st.resynced = true;
            if let Some(w) = st.waker.take() {
                w.wake();
            }
            return false;
        }
        st.queue.push_back(event);
        if let Some(w) = st.waker.take() {
            w.wake();
        }
        true
    }

    /// Whether the view ended because its reader fell behind.
    pub fn was_resynced(&self) -> bool {
        self.0.lock().resynced
    }

    /// Whether the reader is gone.
    pub fn is_closed(&self) -> bool {
        self.0.lock().reader_gone
    }
}

/// What a view yields: [`ViewEvent::Snapshot`] first, then [`ViewEvent::Diff`]s. Dropping it ends the view.
pub struct ViewStream {
    shared: Arc<Shared>,
    _guard: Option<Box<dyn Send + Sync>>,
}

impl Drop for ViewStream {
    fn drop(&mut self) {
        let mut st = self.shared.lock();
        st.reader_gone = true;
        st.queue.clear();
    }
}

impl ViewStream {
    /// A stream and the sink that feeds it.
    pub fn channel() -> (ViewSink, ViewStream) {
        let shared = Arc::new(Shared(Mutex::new(State {
            queue: VecDeque::new(),
            waker: None,
            sinks: 1,
            reader_gone: false,
            resynced: false,
        })));
        (
            ViewSink(Arc::clone(&shared)),
            ViewStream {
                shared,
                _guard: None,
            },
        )
    }

    /// A stream that only says why the view failed.
    pub fn failed(error: NodeError) -> ViewStream {
        let (sink, stream) = Self::channel();
        sink.send(ViewEvent::Failed { seq: 1, error });
        stream
    }

    /// Runs `guard`'s destructor when the stream is dropped (to unsubscribe).
    #[must_use]
    pub fn guarded(mut self, guard: impl Send + Sync + 'static) -> ViewStream {
        self._guard = Some(Box::new(guard));
        self
    }

    /// The next event; `None` when the view has ended.
    pub async fn next(&mut self) -> Option<ViewEvent> {
        std::future::poll_fn(|cx| self.poll_recv(cx)).await
    }

    /// The next event if one is already there.
    pub fn try_next(&mut self) -> Option<ViewEvent> {
        self.shared.lock().queue.pop_front()
    }

    fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<ViewEvent>> {
        let mut st = self.shared.lock();
        if let Some(e) = st.queue.pop_front() {
            return Poll::Ready(Some(e));
        }
        if st.sinks == 0 || st.resynced {
            return Poll::Ready(None);
        }
        st.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

impl Stream for ViewStream {
    type Item = ViewEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<ViewEvent>> {
        self.poll_recv(cx)
    }
}

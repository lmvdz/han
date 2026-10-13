//! The two methods as a trait, and the stream a view yields.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;
use tokio::sync::mpsc;

use super::{CallResult, Command, NodeError, Query, ViewEvent};

/// A boxed future, so [`NodeApi`] can be used as a trait object.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The node API (PLAN section 5): the whole surface a face sees. The runtime implements it in process; the
/// JSON-RPC client implements it over the local pipe, so a face does not care which it holds.
pub trait NodeApi: Send + Sync + 'static {
    /// Does something. It ends as a receipt, a verdict or an error.
    fn call(&self, cmd: Command) -> BoxFuture<'_, Result<CallResult, NodeError>>;

    /// Watches something: a snapshot of the window, then diffs, until the stream is dropped.
    fn view(&self, query: Query) -> ViewStream;
}

/// The sending end of a view stream.
#[derive(Clone)]
pub struct ViewSink(mpsc::UnboundedSender<ViewEvent>);

impl ViewSink {
    /// Sends an event; false when the reader is gone.
    pub fn send(&self, event: ViewEvent) -> bool {
        self.0.send(event).is_ok()
    }

    /// Whether the reader is gone.
    pub fn is_closed(&self) -> bool {
        self.0.is_closed()
    }
}

/// What a view yields: [`ViewEvent::Snapshot`] first, then [`ViewEvent::Diff`]s. Dropping it ends the view.
pub struct ViewStream {
    rx: mpsc::UnboundedReceiver<ViewEvent>,
    _guard: Option<Box<dyn Send + Sync>>,
}

impl ViewStream {
    /// A stream and the sink that feeds it.
    pub fn channel() -> (ViewSink, ViewStream) {
        let (tx, rx) = mpsc::unbounded_channel();
        (ViewSink(tx), ViewStream { rx, _guard: None })
    }

    /// A stream that only says why the view failed.
    pub fn failed(error: NodeError) -> ViewStream {
        let (sink, stream) = Self::channel();
        sink.send(ViewEvent::Failed { error });
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
        self.rx.recv().await
    }

    /// The next event if one is already there.
    pub fn try_next(&mut self) -> Option<ViewEvent> {
        self.rx.try_recv().ok()
    }
}

impl Stream for ViewStream {
    type Item = ViewEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<ViewEvent>> {
        self.rx.poll_recv(cx)
    }
}

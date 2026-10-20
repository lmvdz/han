//! Process-wide graceful shutdown for roles composed by the local setup launcher.

use std::sync::atomic::{AtomicBool, Ordering};

static STOP: AtomicBool = AtomicBool::new(false);
static CHANGED: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Ask every role in this process to shut down normally.
pub fn request() {
    STOP.store(true, Ordering::SeqCst);
    CHANGED.notify_waiters();
}

/// Wait for Ctrl-C or the local setup control endpoint's stop request.
pub async fn requested() -> std::io::Result<()> {
    let notified = CHANGED.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    if STOP.load(Ordering::SeqCst) {
        return Ok(());
    }
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = notified => Ok(()),
    }
}

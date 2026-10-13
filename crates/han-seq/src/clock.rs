//! The injected clock (R-03a, R-16): receipt times come from a clock that does not jump, and elapsed time
//! from a steady one. A sequencer never reads the system clock itself, so tests and simulations can move it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// What a sequencer reads of time.
pub trait Clock: Send + Sync + 'static {
    /// Milliseconds since the epoch for receipt times, `hello` and the Challenge. It must not jump: a
    /// production clock reads the wall time once at startup and then follows the steady clock (R-03a).
    fn now_ms(&self) -> u64;

    /// Steady milliseconds from an arbitrary origin; never goes back. R-16 and challenge expiry count here.
    fn steady_ms(&self) -> u64;
}

/// The production clock: wall time read once, then slewed by the steady clock.
#[derive(Debug)]
pub struct SystemClock {
    anchor_wall_ms: u64,
    anchor: Instant,
}

impl SystemClock {
    /// Anchors the clock at the system time now.
    pub fn new() -> Self {
        let wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0));
        SystemClock {
            anchor_wall_ms: wall,
            anchor: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        self.anchor_wall_ms + self.steady_ms()
    }

    fn steady_ms(&self) -> u64 {
        u64::try_from(self.anchor.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// The clock of test mode (G-11): neither reading moves except by [`TestClock::set_wall`] and
/// [`TestClock::advance_steady`], which `test.clock` calls.
#[derive(Debug)]
pub struct TestClock {
    wall_ms: AtomicU64,
    steady_ms: AtomicU64,
}

impl TestClock {
    /// A test clock at `wall_secs` seconds since the epoch and steady time 0.
    pub fn new(wall_secs: u64) -> Self {
        TestClock {
            wall_ms: AtomicU64::new(wall_secs * 1000),
            steady_ms: AtomicU64::new(0),
        }
    }

    /// Sets the wall reading, forward or back.
    pub fn set_wall(&self, wall_secs: u64) {
        self.wall_ms.store(wall_secs * 1000, Ordering::SeqCst);
    }

    /// Advances the steady reading.
    pub fn advance_steady(&self, secs: u64) {
        self.steady_ms.fetch_add(secs * 1000, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.wall_ms.load(Ordering::SeqCst)
    }

    fn steady_ms(&self) -> u64 {
        self.steady_ms.load(Ordering::SeqCst)
    }
}

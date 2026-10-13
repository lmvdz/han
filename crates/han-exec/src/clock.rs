//! The elapsed-time clock of X-02 and X-04, injected so tests can stall it.
//!
//! X-02 asks for a clock that keeps counting through suspend, sleep and VM pause. The workspace forbids
//! `unsafe`, so the platform calls (`QueryInterruptTime`, `mach_continuous_time`) are not used directly:
//! * **Linux:** `/proc/uptime`, which the kernel derives from `CLOCK_BOOTTIME` (counts suspend), at 10 ms
//!   resolution; if it cannot be read, `std::time::Instant` (`CLOCK_MONOTONIC`, which does **not** count
//!   suspend) and the clock's name says so.
//! * **Windows:** `std::time::Instant`, which is `QueryPerformanceCounter`; it keeps counting across sleep and
//!   hibernation on current Windows, so it serves as the suspend-aware clock.
//! * **macOS and others:** `std::time::Instant` (`mach_absolute_time` on macOS, which does **not** count
//!   sleep): a known gap against X-02, named in every journal entry.
//!
//! Readings are only compared within one process life: after a restart the executor never launches (X-06).

use std::time::Instant;

/// An elapsed-time clock.
pub trait Elapsed: Send + Sync + 'static {
    /// Milliseconds since an arbitrary origin that does not move during the process's life.
    fn now_ms(&self) -> u64;

    /// The clock's name, journalled beside every reading (X-02).
    fn name(&self) -> &'static str;
}

/// The platform clock described in the module documentation.
pub struct SystemElapsed {
    origin: Instant,
    boottime: bool,
}

impl Default for SystemElapsed {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemElapsed {
    /// The clock; logs which one it is (X-02: an executor logs its clock at start-up).
    pub fn new() -> Self {
        let clock = SystemElapsed {
            origin: Instant::now(),
            boottime: cfg!(target_os = "linux") && read_uptime_ms().is_some(),
        };
        tracing::info!(clock = clock.name(), "han-exec elapsed-time clock");
        clock
    }
}

fn read_uptime_ms() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/uptime").ok()?;
    let first = text.split_whitespace().next()?;
    let (secs, frac) = first.split_once('.').unwrap_or((first, "0"));
    let secs: u64 = secs.parse().ok()?;
    let frac: u64 = format!("{frac:0<3}")[..3].parse().ok()?;
    Some(secs * 1000 + frac)
}

impl Elapsed for SystemElapsed {
    fn now_ms(&self) -> u64 {
        if self.boottime
            && let Some(ms) = read_uptime_ms()
        {
            return ms;
        }
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn name(&self) -> &'static str {
        if self.boottime {
            "linux-boottime(/proc/uptime)"
        } else if cfg!(windows) {
            "windows-qpc(Instant)"
        } else if cfg!(target_os = "linux") {
            "linux-monotonic(Instant, no suspend)"
        } else {
            "monotonic(Instant, no sleep)"
        }
    }
}

//! `han-proc`: spawn flags for HAN's child processes.
//!
//! On Windows a console-subsystem program started by a process that has no console (every HAN background
//! service: the node, seq, exec, host and runner are started `DETACHED_PROCESS`, as is the scheduled task that
//! starts the node) gets a brand-new visible console window. Each `git`, `cargo`, `rustup`, `han shim` and `gh`
//! such a service starts would pop one. [`NoWindow::no_window`] passes `CREATE_NO_WINDOW`, which gives the child
//! a hidden console instead; the child's own children inherit that hidden console and show nothing either.
//!
//! The two flags must not be combined: `CREATE_NO_WINDOW` is ignored when `DETACHED_PROCESS` is set.
//! Use `no_window` for a short-lived or piped helper child (git, cargo, a shim, `taskkill`). Use
//! [`NoWindow::detached_service`] for a long-lived service that must outlive its launcher and own no console.
//!
//! `creation_flags` replaces the previous value (std has no getter), so these set the whole value; a caller that
//! needs extra flags passes them to [`NoWindow::no_window_with`].
//!
//! Everything here is a no-op off Windows.

/// `CREATE_NO_WINDOW`: the child gets a console, but not a visible one.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// `DETACHED_PROCESS`: the child has no console at all.
pub const DETACHED_PROCESS: u32 = 0x0000_0008;
/// `CREATE_NEW_PROCESS_GROUP`: the child is the root of its own process group, so Ctrl-C does not reach it.
pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// Spawn flags for a child that must not show a console window.
pub trait NoWindow {
    /// Start the child without a visible console window (`CREATE_NO_WINDOW`).
    fn no_window(&mut self) -> &mut Self {
        self.no_window_with(0)
    }

    /// `CREATE_NO_WINDOW` plus `extra` creation flags. Do not pass `DETACHED_PROCESS` here.
    fn no_window_with(&mut self, extra: u32) -> &mut Self;

    /// Start a long-lived service: no console at all, in its own process group, so it outlives its launcher
    /// and its console. Its own children are then started with [`NoWindow::no_window`].
    fn detached_service(&mut self) -> &mut Self;
}

#[cfg(windows)]
fn set_flags(cmd: &mut std::process::Command, value: u32) {
    use std::os::windows::process::CommandExt as _;
    cmd.creation_flags(value);
}

impl NoWindow for std::process::Command {
    fn no_window_with(&mut self, extra: u32) -> &mut Self {
        debug_assert_eq!(
            extra & DETACHED_PROCESS,
            0,
            "DETACHED_PROCESS defeats CREATE_NO_WINDOW"
        );
        #[cfg(windows)]
        set_flags(self, CREATE_NO_WINDOW | extra);
        #[cfg(not(windows))]
        let _ = extra;
        self
    }

    fn detached_service(&mut self) -> &mut Self {
        #[cfg(windows)]
        set_flags(self, DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        self
    }
}

#[cfg(feature = "tokio")]
impl NoWindow for tokio::process::Command {
    fn no_window_with(&mut self, extra: u32) -> &mut Self {
        self.as_std_mut().no_window_with(extra);
        self
    }

    fn detached_service(&mut self) -> &mut Self {
        self.as_std_mut().detached_service();
        self
    }
}

//! A child started through the helper, under a parent that has no console (as every HAN background service),
//! gets no console window. The check is the OS's: `GetConsoleWindow()` in the child is null.
//!
//! The test binary plays all three roles. The test starts itself as the parent with `DETACHED_PROCESS`, the
//! parent starts itself as the child through `no_window`, and the child reports whether it owns a console
//! window. No control case (a child started without the helper) runs here, since it would open the very window
//! this guards against.
#![allow(unsafe_code)]
#![cfg(windows)]

use std::ffi::c_void;
use std::process::{Command, Stdio};

use han_proc::NoWindow as _;

unsafe extern "system" {
    safe fn GetConsoleWindow() -> *mut c_void;
}

const ROLE: &str = "HAN_PROC_ROLE";

fn run_role(role: &str, test: &str) -> Command {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(ROLE, role)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Parent role: no console of its own; starts the child through the helper and relays what it says.
#[test]
fn role_parent() {
    if std::env::var(ROLE).as_deref() != Ok("parent") {
        return;
    }
    assert!(
        GetConsoleWindow().is_null(),
        "the detached parent should have no console window"
    );
    let out = run_role("child", "role_child")
        .no_window()
        .output()
        .unwrap();
    print!("{}", String::from_utf8_lossy(&out.stdout));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Child role: prints whether it has a console window.
#[test]
fn role_child() {
    if std::env::var(ROLE).as_deref() != Ok("child") {
        return;
    }
    println!("CONSOLE_WINDOW={}", !GetConsoleWindow().is_null());
}

#[test]
fn a_child_of_a_detached_parent_opens_no_console_window() {
    if std::env::var(ROLE).is_ok() {
        return;
    }
    let out = run_role("parent", "role_parent")
        .detached_service()
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "parent failed: {stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("CONSOLE_WINDOW=false"),
        "the child opened a console window: {stdout}"
    );
}

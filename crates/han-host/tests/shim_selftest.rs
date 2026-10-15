//! `han shim selftest` never outlives its launcher: it ends by itself at its deadline, and the sandbox probe
//! that runs it leaves no copy of it running from this target directory.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use han_host::sandbox;
use han_host::shim::SELFTEST_DEADLINE;

mod production_shim;

/// Kills and reaps the child when dropped, so a failing assertion leaves nothing behind.
struct Reaped(Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The command lines of every process running, one per entry.
#[cfg(windows)]
fn command_lines() -> Vec<String> {
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_Process | ForEach-Object { $_.CommandLine }",
        ])
        .output()
        .expect("powershell runs");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

#[cfg(target_os = "linux")]
fn command_lines() -> Vec<String> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.flatten()
        .filter_map(|e| std::fs::read(e.path().join("cmdline")).ok())
        .map(|raw| {
            raw.split(|b| *b == 0)
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

#[cfg(not(any(windows, target_os = "linux")))]
fn command_lines() -> Vec<String> {
    Vec::new()
}

fn norm(s: &str) -> String {
    s.replace('\\', "/").to_lowercase()
}

/// The `shim selftest` processes whose program sits under `target`.
fn selftests_under(target: &Path) -> Vec<String> {
    let target = norm(&target.to_string_lossy());
    command_lines()
        .into_iter()
        .filter(|l| l.contains("shim selftest") && norm(l).contains(&target))
        .collect()
}

fn target_dir() -> PathBuf {
    production_shim::han()
        .parent()
        .expect("han sits in target/<profile>")
        .to_path_buf()
}

#[test]
fn an_orphaned_selftest_exits_by_itself() {
    let han = production_shim::han();
    let started = Instant::now();
    let mut child = Reaped(
        Command::new(&han)
            .args([
                "shim",
                "selftest",
                "--port",
                "9",
                "--stall",
                "--deadline-ms",
                "700",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("han starts"),
    );
    let code = loop {
        if let Some(status) = child.0.try_wait().expect("wait") {
            break status.code();
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the selftest did not end at its deadline"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(code, Some(SELFTEST_DEADLINE));
    assert!(started.elapsed() >= Duration::from_millis(600));
}

#[test]
fn the_probe_leaves_no_selftest_running() {
    let target = target_dir();
    let _ = sandbox::probe(&production_shim::shim());
    // Another test binary of this target may be probing right now; wait for its selftest to end too.
    let until = Instant::now() + Duration::from_secs(40);
    loop {
        let left = selftests_under(&target);
        if left.is_empty() {
            return;
        }
        assert!(Instant::now() < until, "selftest left running: {left:?}");
        std::thread::sleep(Duration::from_millis(500));
    }
}

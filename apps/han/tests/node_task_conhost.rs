//! Task Scheduler starts a console program with a console it sometimes never finishes setting up, and the
//! program stalls before `main`. `han node install-service` therefore registers the node through
//! `conhost.exe --headless`. This starts a scratch task (never `HAN Node`) several times, each way, and counts
//! how often the node opens its pipe within 10 s. Windows only, and only with `HAN_E2E_TASK=1`
//! (`HAN_E2E_TASK_RUNS` sets the runs per way, 3 by default).

#![cfg(windows)]

use std::process::Command;
use std::time::{Duration, Instant};

const HAN: &str = env!("CARGO_BIN_EXE_han");

fn pipe_exists(endpoint: &str) -> bool {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(format!(r"\\.\pipe\{endpoint}"))
        .is_ok()
}

fn powershell(script: &str) -> bool {
    Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .status()
        .is_ok_and(|s| s.success())
}

/// Ends the nodes of a scratch task: `Stop-ScheduledTask` ends `conhost.exe` but not always the node under it.
/// Only processes whose command line holds the scratch name, which is unique to this test, are ended.
fn end_nodes(name: &str) {
    powershell(&format!(
        "Get-CimInstance Win32_Process | Where-Object {{ $_.CommandLine -like '*--endpoint {name}*' -and $_.Name -ne 'powershell.exe' }}         | ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }}"
    ));
}

/// Removes the scratch task, whatever happens.
struct Scratch(String);

impl Drop for Scratch {
    fn drop(&mut self) {
        powershell(&format!(
            "Stop-ScheduledTask -TaskName '{0}' -ErrorAction SilentlyContinue; \
             Unregister-ScheduledTask -TaskName '{0}' -Confirm:$false -ErrorAction SilentlyContinue",
            self.0
        ));
        end_nodes(&self.0);
    }
}

/// A registered scratch task.
struct Task {
    name: String,
    endpoint: String,
    dir: std::path::PathBuf,
    _scratch: Scratch,
}

impl Drop for Task {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Registers a scratch task with the action `install-service` would give.
fn register(headless: bool, tag: &str) -> Task {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let name = format!("han-test-{tag}-{}-{nanos}", std::process::id());
    let endpoint = name.clone();
    // The task runs as the user, outside any sandbox of the test run: `HAN_E2E_TASK_DIR` names a directory
    // it can write when the temp directory is not one.
    let base = std::env::var_os("HAN_E2E_TASK_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(&name);
    std::fs::create_dir_all(&dir).unwrap();
    let mut cmd = Command::new(HAN);
    cmd.args(["node", "install-service", "--actor", "@t:x.test", "--data"])
        .arg(&dir)
        .args(["--file-keys", "--relay", "none", "--bind", "127.0.0.1:0"])
        .args(["--endpoint", &endpoint, "--no-start", "--dry-run"]);
    if !headless {
        cmd.arg("--no-headless");
    }
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    // the dry run prints the script; keep the part from its first PowerShell line
    let script = text
        .lines()
        .skip_while(|l| !l.starts_with("$ErrorActionPreference"))
        .collect::<Vec<_>>()
        .join(
            "
",
        )
        .replace("'HAN Node'", &format!("'{name}'"));
    assert_eq!(
        script.contains("conhost.exe"),
        headless,
        "the action is not as asked: {script}"
    );
    let scratch = Scratch(name.clone());
    assert!(powershell(&script), "registering the scratch task failed");
    Task {
        name,
        endpoint,
        dir,
        _scratch: scratch,
    }
}

/// Starts a scratch task `runs` times. Returns how many runs opened the pipe within 10 s.
fn measure(headless: bool, runs: usize, tag: &str) -> usize {
    let task = register(headless, tag);
    let (name, endpoint) = (task.name.as_str(), task.endpoint.as_str());
    let mut up = 0;
    for _ in 0..runs {
        assert!(powershell(&format!(
            "Start-ScheduledTask -TaskName '{name}'"
        )));
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut opened = false;
        while Instant::now() < deadline {
            if pipe_exists(endpoint) {
                opened = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if opened {
            up += 1;
        }
        powershell(&format!(
            "Stop-ScheduledTask -TaskName '{name}' -ErrorAction SilentlyContinue"
        ));
        end_nodes(name);
        let gone = Instant::now() + Duration::from_secs(15);
        while pipe_exists(endpoint) && Instant::now() < gone {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    up
}

#[test]
fn a_node_started_by_task_scheduler_through_conhost_headless_opens_its_pipe() {
    if std::env::var_os("HAN_E2E_TASK").is_none() {
        eprintln!("skipped: set HAN_E2E_TASK=1 to start scratch scheduled tasks");
        return;
    }
    let runs: usize = std::env::var("HAN_E2E_TASK_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let plain = measure(false, runs, "plain");
    let wrapped = measure(true, runs, "wrap");
    eprintln!("pipe within 10 s: without the wrapper {plain}/{runs}, with it {wrapped}/{runs}");
    assert_eq!(
        wrapped, runs,
        "the wrapped task stalled ({wrapped}/{runs}; unwrapped {plain}/{runs})"
    );
}

/// The processes whose command line names the scratch endpoint.
fn nodes_of(endpoint: &str) -> bool {
    Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(
                "if (Get-CimInstance Win32_Process | Where-Object {{ $_.CommandLine -like '*--endpoint {endpoint}*'                  -and $_.Name -ne 'powershell.exe' }}) {{ exit 1 }}"
            ),
        ])
        .status()
        .is_ok_and(|s| !s.success())
}

#[test]
fn stopping_the_task_ends_the_node_too() {
    if std::env::var_os("HAN_E2E_TASK").is_none() {
        eprintln!("skipped: set HAN_E2E_TASK=1 to start scratch scheduled tasks");
        return;
    }
    let task = register(true, "stop");
    let (name, endpoint) = (task.name.as_str(), task.endpoint.as_str());
    assert!(powershell(&format!(
        "Start-ScheduledTask -TaskName '{name}'"
    )));
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut up = false;
    while !up && Instant::now() < deadline {
        up = pipe_exists(endpoint);
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(up, "the node did not open its pipe");
    assert!(nodes_of(endpoint));
    powershell(&format!("Stop-ScheduledTask -TaskName '{name}'"));
    let gone = Instant::now() + Duration::from_secs(20);
    while nodes_of(endpoint) && Instant::now() < gone {
        std::thread::sleep(Duration::from_millis(250));
    }
    let left = nodes_of(endpoint);
    end_nodes(name);
    assert!(!left, "a node outlived the task that started it");
}

//! The node starts where nothing can answer it: no console and no stdin.
//! The scheduled task that starts it at logon is such a place, and a node that waited on a console or on a
//! child program there never opened its pipe.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const HAN: &str = env!("CARGO_BIN_EXE_han");

#[cfg(windows)]
fn pipe_exists(endpoint: &str) -> bool {
    // A pipe has no file metadata to look at: connecting is the check.
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(format!(r"\\.\pipe\{endpoint}"))
        .is_ok()
}

#[cfg(not(windows))]
fn pipe_exists(endpoint: &str) -> bool {
    std::path::Path::new(endpoint).exists()
}

#[test]
fn the_node_opens_its_pipe_with_no_console_and_no_stdin() {
    let dir = std::env::temp_dir().join(format!("han-noconsole-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let endpoint = if cfg!(windows) {
        format!("han-noconsole-{}", std::process::id())
    } else {
        dir.join("node.sock").to_string_lossy().into_owned()
    };
    let mut cmd = Command::new(HAN);
    cmd.args(["node", "--actor", "@t:x.test", "--data"])
        .arg(&dir)
        .args(["--file-keys", "--relay", "none", "--bind", "127.0.0.1:0"])
        .args(["--endpoint", &endpoint])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // DETACHED_PROCESS: the process has no console at all, as under the scheduled task.
        cmd.creation_flags(0x0000_0008);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // No controlling terminal.
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut up = false;
    while Instant::now() < deadline {
        if pipe_exists(&endpoint) {
            up = true;
            break;
        }
        if let Some(st) = child.try_wait().unwrap() {
            panic!("the node ended before it opened its pipe: {st}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(up, "the node did not open its pipe in 60 s");
}

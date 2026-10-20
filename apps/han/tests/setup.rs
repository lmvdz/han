//! Production bootstrap without a model, fake home, or weakened presence settings.

use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use han_proc::NoWindow as _;

fn han(data: &Path, args: &[&str]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_han"))
        .no_window()
        .args(args)
        .arg("--data")
        .arg(data)
        .output()
        .unwrap();
    if !output.status.success()
        && let Ok(log) = std::fs::read_to_string(data.join("setup.log"))
    {
        eprintln!("setup log:\n{log}");
    }
    output
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

struct Stop<'a>(&'a Path);
impl Drop for Stop<'_> {
    fn drop(&mut self) {
        let _ = han(self.0, &["setup", "stop"]);
    }
}

#[test]
fn fresh_setup_runs_real_roles_resumes_and_stops_without_replacing_identity() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let _stop = Stop(data);
    let started = success(han(
        data,
        &[
            "setup",
            "--person",
            "alice",
            "--file-keys",
            "--relay",
            "none",
            "--no-tui",
        ],
    ));
    assert!(started.contains("Home and person node ready"), "{started}");
    let record = std::fs::read(data.join("setup.json")).unwrap();
    let key = std::fs::read(data.join("person/keys/device.key")).unwrap();
    assert!(success(han(data, &["setup", "status"])).contains("ready"));
    success(han(data, &["setup", "--no-tui"]));
    assert_eq!(std::fs::read(data.join("setup.json")).unwrap(), record);
    assert_eq!(
        std::fs::read(data.join("person/keys/device.key")).unwrap(),
        key
    );
    let presence = success(han(
        &data.join("person"),
        &["node", "presence", "--file-keys"],
    ));
    assert!(
        presence.contains("presence.require all"),
        "bootstrap must preserve device presence: {presence}"
    );
    success(han(data, &["setup", "stop"]));
    let deadline = Instant::now() + Duration::from_secs(15);
    while !success(han(data, &["setup", "status"])).contains("stopped") {
        assert!(Instant::now() < deadline, "setup did not stop");
        std::thread::sleep(Duration::from_millis(100));
    }
    success(han(data, &["setup", "--no-tui"]));
    assert_eq!(std::fs::read(data.join("setup.json")).unwrap(), record);
    assert_eq!(
        std::fs::read(data.join("person/keys/device.key")).unwrap(),
        key
    );
}

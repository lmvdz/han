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

#[test]
fn failed_managed_executor_keeps_person_and_home_available_without_automatic_retry() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let _stop = Stop(data);
    success(han(
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
    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(data.join("setup.json")).unwrap()).unwrap();
    let server = state["server"].as_str().unwrap();
    // A retained journal is not authority to serve a space. This valid-looking journal names
    // a space the person has never joined; startup must fail before opening an executor.
    let mut flow = serde_json::json!({"version":1, "space":format!("missing:{server}"),
    "executor_enabled":true, "generation":1, "configuration":{}, "plan":{
        "agent_name":"coder", "harness":"codex", "repo":"github:o/r",
        "target_ref":"refs/heads/main", "checks":["ci"], "app_id":1,
        "installation_id":2, "key_source":data.join("absent.pem"), "read_app":null
    }});
    std::fs::write(
        data.join("workflow.json"),
        serde_json::to_vec(&flow).unwrap(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(35);
    loop {
        let status = success(han(data, &["setup", "status"]));
        if status.contains("executor: failed:") {
            assert!(
                status.contains(": ready;"),
                "person remains ready: {status}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no executor failure reported: {status}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let first = std::fs::read(data.join("executor.log")).unwrap();
    assert!(!first.is_empty());
    std::thread::sleep(Duration::from_millis(750));
    assert_eq!(
        std::fs::read(data.join("executor.log")).unwrap(),
        first,
        "no automatic retry after failure"
    );
    assert!(
        !data.join("executor/state").exists(),
        "unvalidated role was never opened"
    );
    flow["generation"] = serde_json::json!(2);
    let temp = data.join("workflow.json.tmp");
    std::fs::write(&temp, serde_json::to_vec(&flow).unwrap()).unwrap();
    std::fs::rename(temp, data.join("workflow.json")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(35);
    while std::fs::read(data.join("executor.log")).unwrap().len() == first.len() {
        assert!(Instant::now() < deadline, "explicit retry did not run");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(success(han(data, &["setup", "status"])).contains(": ready;"));
    success(han(data, &["setup", "stop"]));
    assert!(success(han(data, &["setup", "status"])).contains("stopped"));
}

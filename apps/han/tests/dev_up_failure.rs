//! `han dev up` failing must say why and exit non-zero, never kill itself (its own command line names the dir).

use std::process::Command;

const HAN: &str = env!("CARGO_BIN_EXE_han");

fn up_with(dir: &std::path::Path, extra: &[&str]) -> std::process::Output {
    Command::new(HAN)
        .args(["dev", "up", "--dir"])
        .arg(dir)
        .args(extra)
        .env_remove("HAN_DATA")
        .output()
        .expect("han runs")
}

#[test]
fn failed_up_prints_the_error_and_exits_with_a_failure_status() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("stack");
    // a bad value after the dir is set: the run must fail on its own terms
    let out = up_with(&dir, &["--budget", "not-a-budget"]);
    let err = String::from_utf8_lossy(&out.stderr);
    // old code: killed by its own `pkill -f <dir>`, so no code and nothing on stderr
    assert_eq!(out.status.code(), Some(1), "stderr: {err}");
    assert!(err.contains("error:"), "the real error is printed: {err}");
}

#[test]
fn missing_binary_error_names_the_binary_and_the_build_command() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("stack");
    let missing = d.path().join("no-such-han-github");
    let out = up_with(
        &dir,
        &[
            "--fake-agent",
            HAN,
            "--github-bin",
            &missing.to_string_lossy(),
        ],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr: {err}");
    assert!(err.contains("han-github"), "{err}");
    assert!(err.contains("cargo build --workspace --bins"), "{err}");
}

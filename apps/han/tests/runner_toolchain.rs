//! `han runner setup --toolchain rust` (docs/runner.md 7.2): Linux, and for the real run root and the network.
//!
//! The dry run changes nothing and always runs. The real run installs rustup into the shared `/opt/rust`, so it
//! is skipped unless `HAN_E2E_RUNNER_TOOLCHAIN=1`; run it as root in a throwaway container (podman, as the
//! os-user end-to-end test does).

#![cfg(target_os = "linux")]

use han_proc::NoWindow as _;
use std::process::Command;

const HAN: &str = env!("CARGO_BIN_EXE_han");

fn setup(data: &std::path::Path, repo: &std::path::Path, extra: &[&str]) -> (bool, String) {
    let out = Command::new(HAN)
        .no_window()
        .args(["runner", "setup", "--toolchain", "rust", "--toolchain-only"])
        .arg("--repo")
        .arg(repo)
        .arg("--data")
        .arg(data)
        .args(extra)
        .output()
        .expect("han runs");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn repo_pinning(dir: &std::path::Path, channel: &str) -> std::path::PathBuf {
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(
        repo.join("rust-toolchain.toml"),
        format!("[toolchain]\nchannel = \"{channel}\"\n"),
    )
    .unwrap();
    repo
}

#[test]
fn a_dry_run_says_what_it_would_do_and_writes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let repo = repo_pinning(d.path(), "1.90.0");
    let data = d.path().join("node");
    let (ok, text) = setup(&data, &repo, &["--dry-run"]);
    assert!(ok, "{text}");
    assert!(text.contains("pinned:    1.90.0 by"), "{text}");
    assert!(text.contains("dry run: nothing changed"), "{text}");
    assert!(!data.exists(), "{text}");
}

#[test]
fn the_real_run_installs_what_the_repositories_pin_and_is_idempotent() {
    if std::env::var_os("HAN_E2E_RUNNER_TOOLCHAIN").is_none() {
        eprintln!("skipped: needs HAN_E2E_RUNNER_TOOLCHAIN=1, root and the network");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let repo = repo_pinning(d.path(), "1.90.0");
    let data = d.path().join("node");
    let (ok, first) = setup(&data, &repo, &["--rust-toolchain", "1.97.1"]);
    assert!(ok, "{first}");
    assert!(
        first.contains(
            "
  changed:"
        ),
        "{first}"
    );
    let toolchains: Vec<String> = std::fs::read_dir("/opt/rust/rustup/toolchains")
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        toolchains.iter().any(|t| t.starts_with("1.90.0-")),
        "{toolchains:?}"
    );
    assert!(
        toolchains.iter().any(|t| t.starts_with("1.97.1-")),
        "{toolchains:?}"
    );
    let file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(data.join("agents.json")).unwrap()).unwrap();
    assert_eq!(file["toolchains"][0], "rust");
    assert_eq!(
        file["toolchainPaths"]["rust"]["rustupHome"],
        "/opt/rust/rustup"
    );

    let (ok, again) = setup(&data, &repo, &["--rust-toolchain", "1.97.1"]);
    assert!(ok, "{again}");
    assert!(
        !again.contains(
            "
  changed:"
        ),
        "{again}"
    );
    assert!(again.contains("done: nothing to change"), "{again}");
}

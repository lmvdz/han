//! Replays every Policy vector of `conformance/vectors` (166 files) through the `han-conform`
//! binary, which checks each verdict and the folded state after every step as
//! `ref/run_vectors.py` does.

use std::process::Command;

#[test]
fn every_policy_vector_passes() {
    let out = Command::new(env!("CARGO_BIN_EXE_han-conform"))
        .output()
        .expect("han-conform runs");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "han-conform failed:\n{text}");
    assert!(
        text.lines()
            .last()
            .is_some_and(|l| l.ends_with("vectors pass 166 fail 0")),
        "unexpected report:\n{text}"
    );
}

#[test]
fn a_failing_vector_is_reported_in_the_fuzz_protocol() {
    let dir = std::env::temp_dir().join(format!("han-conform-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let vector = r#"{"name":"x","profiles":{},"steps":[{"entry":{"id":"","v":2,"space":"s:a.example","kind":"message","author":"@a:a.example","keyId":"k","body":{},"contentHash":"","refs":[],"seen":0,"nonce":"n","time":"2026-10-10T12:00:00Z"},"time":"2026-10-10T12:00:00Z","expect":"allow"}]}"#;
    std::fs::write(dir.join("seed1.json"), vector).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_han-conform"))
        .arg(&dir)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(!out.status.success());
    assert!(text.starts_with("seed1.json#0: "), "{text}");
    assert!(
        text.contains("steps pass 0 fail 1; vectors pass 0 fail 1"),
        "{text}"
    );
}

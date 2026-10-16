//! Replays every Policy vector of `conformance/vectors` (every file there) through the `han-conform`
//! binary, which checks each verdict and the folded state after every step as
//! `ref/run_vectors.py` does.

use han_proc::NoWindow as _;
use std::process::Command;

#[test]
fn every_policy_vector_passes() {
    let out = Command::new(env!("CARGO_BIN_EXE_han-conform"))
        .no_window()
        .output()
        .expect("han-conform runs");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "han-conform failed:\n{text}");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/vectors");
    let files = std::fs::read_dir(dir)
        .expect("the vector directory")
        .filter(|e| {
            e.as_ref()
                .is_ok_and(|e| e.path().extension().is_some_and(|x| x == "json"))
        })
        .count();
    let want = format!("vectors pass {files} fail 0");
    assert!(
        text.lines().last().is_some_and(|l| l.ends_with(&want)),
        "unexpected report (want {want}):\n{text}"
    );
}

#[test]
fn a_failing_vector_is_reported_in_the_fuzz_protocol() {
    let dir = std::env::temp_dir().join(format!("han-conform-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let vector = r#"{"name":"x","profiles":{},"steps":[{"entry":{"id":"","v":2,"space":"s:a.example","kind":"message","author":"@a:a.example","keyId":"k","body":{},"contentHash":"","refs":[],"seen":0,"nonce":"n","time":"2026-10-10T12:00:00Z"},"time":"2026-10-10T12:00:00Z","expect":"allow"}]}"#;
    std::fs::write(dir.join("seed1.json"), vector).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_han-conform"))
        .no_window()
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

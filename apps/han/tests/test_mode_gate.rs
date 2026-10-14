//! R9: test mode (`han seq --test-mode`, `han conform`) exists only in a build with the `conformance` feature.

use std::process::Command;

fn help(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_han"))
        .args(args)
        .output()
        .expect("han runs");
    assert!(out.status.success(), "han {args:?} failed");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn test_mode_is_listed_only_with_the_conformance_feature() {
    let on = cfg!(feature = "conformance");
    assert_eq!(help(&["seq", "--help"]).contains("--test-mode"), on);
    assert_eq!(help(&["--help"]).contains(" conform "), on);
}

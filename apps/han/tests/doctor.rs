//! `han doctor` and `han agent set`, run as the real binary on a data directory with no node running.

use std::process::Command;

const HAN: &str = env!("CARGO_BIN_EXE_han");

fn han(args: &[&str]) -> std::process::Output {
    Command::new(HAN)
        .args(args)
        .env_remove("HAN_DATA")
        .output()
        .expect("han runs")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn agent_set_prints_changed_then_unchanged_and_doctor_reads_what_it_wrote() {
    let d = tempfile::tempdir().unwrap();
    let data = d.path().to_string_lossy().into_owned();
    let set = |extra: &[&str]| {
        let mut a = vec!["agent", "set", "@coder:han.example", "--data", &data];
        a.extend(extra);
        han(&a)
    };

    let first = set(&[
        "--sandbox",
        "bwrap",
        "--toolchains",
        "rust",
        "--harness",
        "pi",
    ]);
    assert!(first.status.success(), "{}", text(&first.stderr));
    assert!(text(&first.stdout).starts_with("changed: @coder:han.example"));
    let again = set(&[
        "--sandbox",
        "bwrap",
        "--toolchains",
        "rust",
        "--harness",
        "pi",
    ]);
    assert!(text(&again.stdout).starts_with("unchanged: @coder:han.example"));
    let file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(d.path().join("agents.json")).unwrap()).unwrap();
    assert_eq!(file["agents"]["@coder:han.example"]["sandbox"], "bwrap");
    assert_eq!(
        file["agents"]["@coder:han.example"]["toolchains"][0],
        "rust"
    );

    // a value no launch could use, and a call that sets nothing, are refused and write nothing
    let before = std::fs::read(d.path().join("agents.json")).unwrap();
    assert!(!set(&["--sandbox", "chroot"]).status.success());
    assert!(!set(&[]).status.success());
    assert_eq!(std::fs::read(d.path().join("agents.json")).unwrap(), before);

    // doctor: no node is running, the pi harness is not set up
    let out = han(&["doctor", "--json", "--data", &data]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stdout));
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["schema"], "han-doctor/1");
    assert_eq!(report["healthy"], false);
    let ids: Vec<&str> = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"node.down"), "{ids:?}");
    assert!(ids.contains(&"harness.missing"), "{ids:?}");
    let fix = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == "harness.missing")
        .unwrap()["fix"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        fix.starts_with("han agent setup-harness pi --data "),
        "{fix}"
    );

    // the human form says the same, with a cause and a fix per problem
    let human = han(&["doctor", "--data", &data]);
    assert_eq!(human.status.code(), Some(1));
    let t = text(&human.stdout);
    assert!(t.contains("cause:") && t.contains("fix:"), "{t}");
}

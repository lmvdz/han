//! `han doctor` and `han agent set`, run as the real binary on a data directory with no node running.

use han_proc::NoWindow as _;
use std::process::Command;

const HAN: &str = env!("CARGO_BIN_EXE_han");

fn han(args: &[&str]) -> std::process::Output {
    Command::new(HAN)
        .no_window()
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

#[test]
fn the_land_setup_is_made_by_commands_and_doctor_finds_what_is_missing() {
    let d = tempfile::tempdir().unwrap();
    let data = d.path().join("node").to_string_lossy().into_owned();
    let exec = d.path().join("exec");
    let exec_dir = exec.to_string_lossy().into_owned();
    let ids = |extra: &[&str]| {
        let mut a = vec!["doctor", "--json", "--data", &data, "--exec-dir", &exec_dir];
        a.extend(extra);
        let out = han(&a);
        let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["id"].as_str().unwrap().to_owned())
            .filter(|i| i.starts_with("land."))
            .collect::<Vec<_>>()
    };

    // the executor has the repository and nothing to land on; the flow is off: nothing to say
    let add = han(&[
        "executor",
        "set",
        "github:o/r",
        "--dir",
        &exec_dir,
        "--remote",
        "https://github.com/o/r.git",
        "--required",
        "quick",
    ]);
    assert!(add.status.success(), "{}", text(&add.stderr));
    assert!(ids(&[]).is_empty());

    // turn the flow on for the repository: the executor does not list the ref yet
    let on = han(&[
        "agent",
        "finish",
        "s1:han.example",
        "--repo",
        "github:o/r",
        "--data",
        &data,
        "land",
        "--target-ref",
        "refs/heads/main",
        "--tier-rule",
        "docs/**=0",
    ]);
    assert!(on.status.success(), "{}", text(&on.stderr));
    assert!(text(&on.stdout).starts_with("changed: land on"));
    assert_eq!(ids(&[]), ["land.executor.landing-ref"]);

    // the fix doctor prints is a command; run it and the finding is gone
    let fix = han(&[
        "executor",
        "set",
        "github:o/r",
        "--dir",
        &exec_dir,
        "--landing-refs",
        "refs/heads/main",
    ]);
    assert!(fix.status.success(), "{}", text(&fix.stderr));
    assert!(text(&fix.stdout).starts_with("changed: github:o/r"));
    assert!(ids(&[]).is_empty());

    // the node's file has it, in the shape the launch reads
    let file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(d.path().join("node/agents.json")).unwrap()).unwrap();
    let layer = &file["finish"]["s1:han.example"]["repos"]["github:o/r"];
    assert_eq!(layer["enabled"], true);
    assert_eq!(layer["targetRef"], "refs/heads/main");
    assert_eq!(layer["tierRules"][0]["path"], "docs/**");

    // off, then the default: nothing is checked again
    let off = han(&[
        "agent",
        "finish",
        "s1:han.example",
        "--repo",
        "github:o/r",
        "--data",
        &data,
        "off",
    ]);
    assert!(text(&off.stdout).starts_with("changed: land off"));
}

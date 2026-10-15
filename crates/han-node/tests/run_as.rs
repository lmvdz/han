//! A host as another OS user (SR-83): the supervisor leaves the spec for the unit and starts it with
//! `systemctl`, with no privilege of its own. A fake `systemctl` stands in for the real one.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;

use han_node::supervisor::{
    ProcessSpawner, RoleSpec, RoleStatus, Supervisor, SupervisorError, unit_of,
};
use han_registry::{
    HostAdmission, HostSpec, RefusalKind, RunAs, unit_admission_file, unit_spec_file,
};

fn fake_systemctl(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("systemctl");
    // `is-active` answers from a marker file the fake `start` creates and `stop` removes.
    let script = format!(
        "#!/bin/sh\necho \"$@\" >> {log}\ncase \"$1\" in\n  start) touch {mark} ;;\n  stop) rm -f {mark} ;;\n  is-active) if [ -f {mark} ]; then echo active; else echo inactive; exit 3; fi ;;\nesac\n",
        log = dir.join("calls.log").display(),
        mark = dir.join("active").display()
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn host_spec(state: &std::path::Path) -> HostSpec {
    serde_json::from_value(serde_json::json!({
        "space": "s1:a",
        "agent": "@coder:a",
        "harness": {"name": "pi"},
        "home": {"ticket": "han1home.xyz"},
        "work": {"issue": "1"},
        "dirs": {"state": state, "keys": state, "login": state.join("claude-config")}
    }))
    .unwrap()
}

fn spec(state: &std::path::Path, user: &str) -> RoleSpec {
    RoleSpec {
        name: "host-@coder:a#1".into(),
        spec: host_spec(state),
        run_as: Some(RunAs {
            os_user: user.into(),
            state_dir: state.to_path_buf(),
        }),
    }
}

#[test]
fn a_host_as_another_user_goes_through_its_unit() {
    let dir = std::env::temp_dir().join(format!("han-run-as-{}", std::process::id()));
    let state = dir.join("state");
    std::fs::create_dir_all(&dir).unwrap();
    let spawner = ProcessSpawner::new("/nonexistent/han")
        .with_systemctl(fake_systemctl(&dir))
        .with_admission_wait(std::time::Duration::ZERO);

    spawner.start(spec(&state, "han-agent1")).unwrap();
    // the unit reads the spec the node left for it (`han host --spec <that file>`)
    assert_eq!(
        HostSpec::read(&unit_spec_file(&state).to_string_lossy()).unwrap(),
        host_spec(&state)
    );
    let log = std::fs::read_to_string(dir.join("calls.log")).unwrap();
    assert!(
        log.contains(&format!("start --no-block {}", unit_of("han-agent1"))),
        "{log}"
    );
    assert_eq!(unit_of("han-agent1"), "han-agent@han-agent1.service");
    assert_eq!(
        spawner.status(),
        vec![("host-@coder:a#1".to_owned(), RoleStatus::Running)]
    );
    // one role per OS user at a time
    assert!(matches!(
        spawner.start(spec(&state, "han-agent1")),
        Err(SupervisorError::Running(_))
    ));
    spawner.stop("host-@coder:a#1").unwrap();
    assert!(
        std::fs::read_to_string(dir.join("calls.log"))
            .unwrap()
            .contains("stop han-agent@han-agent1.service")
    );
    assert!(matches!(
        spawner.stop("host-@coder:a#1"),
        Err(SupervisorError::NotRunning(_))
    ));

    // no unit for a name that is not a user name
    for bad in ["Root", "a b", "-x", "../x", ""] {
        assert!(matches!(
            spawner.start(spec(&state, bad)),
            Err(SupervisorError::Spawn(_))
        ));
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// The unit's host checks its sandbox where it runs and leaves the answer; a refusal is the start's error, shows
/// in the problems, and a later start does not inherit it.
#[test]
fn a_host_that_refuses_its_sandbox_is_the_starts_error_and_a_problem() {
    let dir = std::env::temp_dir().join(format!("han-run-as-refuse-{}", std::process::id()));
    let state = dir.join("state");
    std::fs::create_dir_all(&dir).unwrap();
    let spawner = ProcessSpawner::new("/nonexistent/han")
        .with_systemctl(fake_systemctl(&dir))
        .with_admission_wait(std::time::Duration::from_secs(5));
    // the fake unit "runs" the host at once: it leaves its word where the node looks
    std::fs::create_dir_all(state.join("spawn")).unwrap();
    let refused = HostAdmission::Refused {
        kind: RefusalKind::SandboxUnavailable,
        message: "sandbox backend unavailable: Bwrap: no namespaces".into(),
    };
    // the node clears an old answer before it starts: write it after, from a thread, as the host would
    let file = unit_admission_file(&state);
    let writer = std::thread::spawn({
        let refused = refused.clone();
        move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            refused.write(&file).unwrap();
        }
    });
    let err = spawner.start(spec(&state, "han-agent1")).unwrap_err();
    writer.join().unwrap();
    assert!(
        matches!(&err, SupervisorError::SandboxUnavailable(m) if m.contains("no namespaces")),
        "{err:?}"
    );
    let problems = spawner.problems();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("host-@coder:a#1") && problems[0].contains("no namespaces"),
        "{problems:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// Bug: after the node restarted (`han runner setup --restart-stale`) it knew nothing of the units it had
/// started, so `agent stop` said "is not running" while `han-agent@han-agent1` went on. The new node takes the
/// running units back at start, and stop, status and the problems see them again.
#[test]
fn a_restarted_node_takes_back_the_unit_that_is_still_running() {
    let dir = std::env::temp_dir().join(format!("han-run-as-adopt-{}", std::process::id()));
    let (state, state2) = (dir.join("state"), dir.join("state2"));
    std::fs::create_dir_all(&dir).unwrap();
    let new_spawner = || {
        ProcessSpawner::new("/nonexistent/han")
            .with_systemctl(fake_systemctl(&dir))
            .with_admission_wait(std::time::Duration::ZERO)
    };
    let first = new_spawner();
    first.start(spec(&state, "han-agent1")).unwrap();
    // the node restarts: its spawner is new, the unit is not
    drop(first);
    let second = new_spawner();
    assert!(second.status().is_empty(), "a new spawner knows nothing");
    assert!(matches!(
        second.stop("host-@coder:a#1"),
        Err(SupervisorError::NotRunning(_))
    ));

    let run_as = |user: &str, state: &std::path::Path| RunAs {
        os_user: user.into(),
        state_dir: state.to_path_buf(),
    };
    // han-agent2 has a unit that is not running: not taken
    let adopted = second.adopt(&[
        ("@coder:a".into(), run_as("han-agent1", &state)),
        ("@other:a".into(), run_as("han-agent2", &state2)),
    ]);
    assert_eq!(adopted, ["host-@coder:a#1"]);
    assert_eq!(
        second.status(),
        vec![("host-@coder:a#1".to_owned(), RoleStatus::Running)]
    );
    // adopting twice changes nothing
    assert!(
        second
            .adopt(&[("@coder:a".into(), run_as("han-agent1", &state))])
            .is_empty()
    );
    // it is a running host to start (one per OS user) and to stop
    assert!(matches!(
        second.start(spec(&state, "han-agent1")),
        Err(SupervisorError::Running(_))
    ));
    second.stop("host-@coder:a#1").unwrap();
    assert!(
        std::fs::read_to_string(dir.join("calls.log"))
            .unwrap()
            .contains("stop han-agent@han-agent1.service")
    );
    assert!(second.status().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

/// A unit that runs with no spawn file the node can read is still taken, under the agent's name.
#[test]
fn a_unit_with_no_readable_spec_is_taken_under_the_agents_name() {
    let dir = std::env::temp_dir().join(format!("han-run-as-adopt2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("active-han-agent@han-agent1.service"), "").unwrap();
    let spawner = ProcessSpawner::new("/nonexistent/han").with_systemctl(fake_systemctl(&dir));
    let adopted = spawner.adopt(&[(
        "@coder:a".into(),
        RunAs {
            os_user: "han-agent1".into(),
            state_dir: dir.join("nothing-here"),
        },
    )]);
    assert_eq!(adopted, ["host-@coder:a"]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn role_names_round_trip_and_the_exit_watch_waits_out_its_grace() {
    use han_node::supervisor::{ExitWatch, host_name, parse_host_name};
    for (agent, issue, task) in [
        ("@a:h", Some("github:o/r#1"), None),
        ("@a:h", None, Some("abc")),
        ("@a:h", None, None),
    ] {
        let name = host_name(agent, issue, task);
        assert_eq!(
            parse_host_name(&name),
            Some((agent, issue.or(task))),
            "{name}"
        );
    }
    assert_eq!(parse_host_name("nope"), None);

    let run = |s| vec![("host-@a:h".to_owned(), s)];
    let mut w = ExitWatch::default();
    // a role that was never seen running is not an exit
    assert!(w.observe(&run(RoleStatus::Exited(None)), 0, 0).is_empty());
    assert!(w.observe(&run(RoleStatus::Running), 10, 100).is_empty());
    // the exit stands for 100 ms before it is reported, once
    assert!(
        w.observe(&run(RoleStatus::Exited(None)), 20, 100)
            .is_empty()
    );
    assert!(
        w.observe(&run(RoleStatus::Exited(None)), 119, 100)
            .is_empty()
    );
    assert_eq!(
        w.observe(&run(RoleStatus::Exited(None)), 120, 100),
        ["host-@a:h"]
    );
    assert!(
        w.observe(&run(RoleStatus::Exited(None)), 500, 100)
            .is_empty()
    );
    // started again under the same name, then stopped on purpose (no longer listed)
    assert!(w.observe(&run(RoleStatus::Running), 600, 100).is_empty());
    w.stopped("host-@a:h", 700);
    assert!(w.observe(&[], 750, 100).is_empty());
    assert_eq!(w.observe(&[], 800, 100), ["host-@a:h"]);
}

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

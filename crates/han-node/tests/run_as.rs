//! A host as another OS user (SR-83): the supervisor leaves the arguments for the unit and starts it with
//! `systemctl`, with no privilege of its own. A fake `systemctl` stands in for the real one.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;

use han_node::supervisor::{
    ProcessSpawner, Role, RoleSpec, RoleStatus, RunAs, Supervisor, SupervisorError, spawn_file,
    unit_of,
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

fn spec(state: &std::path::Path, user: &str, env: Vec<(String, String)>) -> RoleSpec {
    RoleSpec {
        role: Role::Host,
        name: "host-@coder:a#1".into(),
        args: vec!["--agent".into(), "@coder:a".into()],
        env,
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
    let spawner = ProcessSpawner::new("/nonexistent/han").with_systemctl(fake_systemctl(&dir));
    let ticket = ("HAN_HOME_TICKET".to_owned(), "han1home.xyz".to_owned());

    spawner
        .start(spec(&state, "han-agent1", vec![ticket.clone()]))
        .unwrap();
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(spawn_file(&state)).unwrap()).unwrap();
    assert_eq!(
        doc["args"],
        serde_json::json!(["--agent", "@coder:a", "--home-ticket", "han1home.xyz"])
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
        spawner.start(spec(&state, "han-agent1", vec![])),
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

    // no unit for a name that is not a user name, and no environment the unit cannot carry
    for bad in ["Root", "a b", "-x", "../x", ""] {
        assert!(matches!(
            spawner.start(spec(&state, bad, vec![])),
            Err(SupervisorError::Spawn(_))
        ));
    }
    assert!(matches!(
        spawner.start(spec(
            &state,
            "han-agent1",
            vec![("ANTHROPIC_API_KEY".into(), "k".into())]
        )),
        Err(SupervisorError::Spawn(_))
    ));
    let _ = std::fs::remove_dir_all(dir);
}

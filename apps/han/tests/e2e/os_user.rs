//! R15: an agent that runs as its own OS user (`han-agentN`, made by `han runner setup`) proposes a `git_push`,
//! and the executor runs as `han-exec`, which shares no group with the agent. The executor must land the
//! approved changeset without reading the agent's workspace: its state directory is mode 2770, group
//! `han-agentN`, so `han-exec` can't open it.
//!
//! Linux only, as root, on a machine with systemd (the agent's host is the `han-agent@` unit) and the
//! users `han runner setup` makes. It is skipped unless `HAN_E2E_OS_USERS=1`: the nightly workflow runs it in
//! a systemd container (`.github/workflows/full.yml`, job `e2e-os-users`). `TMPDIR` must not be under `/tmp`
//! or `/home`: the agent's unit has `PrivateTmp` and `ProtectHome`, so it would not see the dev stack.

use han_proc::NoWindow as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use han_node::NodeApi;
use han_node::api::{Command as Call, EffectRow, Query, Row, Window};

use crate::stack::{HAN, Stack, han, rows, run, until};

const AGENT_USER: &str = "han-agent1";
const EXEC_USER: &str = "han-exec";
const EXEC_DIR: &str = "/var/lib/han-exec";

fn sh(program: &str, args: &[&str]) {
    let st = Command::new(program)
        .no_window()
        .args(args)
        .status()
        .unwrap_or_else(|e| panic!("{program}: {e}"));
    assert!(st.success(), "{program} {args:?} failed");
}

/// A bare copy of `from` at `to`, owned by `user` (a local remote must belong to whoever uses it: git refuses
/// a repository of another owner).
fn bare_copy(from: &Path, to: &Path, user: &str) {
    sh(
        "git",
        &[
            "clone",
            "--quiet",
            "--bare",
            &from.to_string_lossy(),
            &to.to_string_lossy(),
        ],
    );
    sh(
        "chown",
        &["-R", &format!("{user}:{user}"), &to.to_string_lossy()],
    );
}

/// A task for the stack's issue addressed to `agent`, which `han agent start --issue` needs (the issue text is
/// the task's): the stack's own task is addressed to its own agent, not to the one these tests add.
async fn task_for_issue(s: &Stack, agent: &str) {
    let c = s.client().await;
    c.call(Call::TaskCreate {
        space: s.space(),
        to: vec![agent.to_owned()],
        ask: "Reject short tokens".into(),
        external: Some(s.get("issue")),
    })
    .await
    .expect("the task for the issue is created");
}

/// The executor as `han-exec`, with the dev stack's executor key and a config whose remote it owns.
fn executor_as_exec_user(s: &Stack, source: &Path, landing: &Path) -> Child {
    let exec_dir = s.dir.join("exec");
    let _ = std::fs::remove_dir_all(format!("{EXEC_DIR}/state"));
    let _ = std::fs::remove_dir_all(format!("{EXEC_DIR}/keys"));
    std::fs::create_dir_all(format!("{EXEC_DIR}/keys")).unwrap();
    for e in std::fs::read_dir(exec_dir.join("keys")).unwrap() {
        let e = e.unwrap();
        std::fs::copy(
            e.path(),
            Path::new(EXEC_DIR).join("keys").join(e.file_name()),
        )
        .unwrap();
    }
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(exec_dir.join("exec.json")).unwrap()).unwrap();
    config["repos"] = serde_json::json!([{
        "name": source.to_string_lossy(),
        "remote": landing.to_string_lossy(),
        "github": null
    }]);
    std::fs::write(
        format!("{EXEC_DIR}/exec.json"),
        serde_json::to_vec_pretty(&config).unwrap(),
    )
    .unwrap();
    sh(
        "chown",
        &["-R", &format!("{EXEC_USER}:{EXEC_USER}"), EXEC_DIR],
    );
    let log = std::fs::File::create(s.dir.join("logs").join("exec-os-user.log")).unwrap();
    Command::new("runuser")
        .no_window()
        .args([
            "-u",
            EXEC_USER,
            "--",
            "env",
            "HAN_RELAY=none",
            "RUST_LOG=info",
            HAN,
        ])
        .args([
            "exec",
            "--actor",
            &s.executor(),
            "--space",
            &s.space(),
            "--home-ticket",
            &s.ticket(),
            "--keys",
            EXEC_DIR,
            "--data",
            &format!("{EXEC_DIR}/state"),
            "--config",
            &format!("{EXEC_DIR}/exec.json"),
            "--relay",
            "none",
        ])
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .expect("han exec runs as han-exec")
}

/// R15: with the plan's host path as the only way to the changeset, the executor (another OS user) could not
/// read it and the approved push never landed. The host now hands the changeset over as a bundle blob.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "end to end, as root in a systemd container: run with --ignored and HAN_E2E_OS_USERS=1"]
async fn an_os_user_agents_push_lands_through_an_executor_that_cannot_read_its_workspace() {
    if std::env::var_os("HAN_E2E_OS_USERS").is_none() || !cfg!(target_os = "linux") {
        eprintln!("skipped: needs HAN_E2E_OS_USERS=1, Linux, root and systemd");
        return;
    }
    let tmp = std::env::temp_dir();
    assert!(
        !tmp.starts_with("/tmp") && !tmp.starts_with("/home"),
        "set TMPDIR outside /tmp and /home: the agent's unit can't see them"
    );
    han(&["runner", "setup", "--agents", "1"]);

    let s = Stack::up("propose", &[]);
    // the agent clones from a source it owns; the executor pushes to a remote it owns
    let source = s.dir.join("source.git");
    let landing = s.dir.join("landing.git");
    bare_copy(&s.remote(), &source, AGENT_USER);
    bare_copy(&s.remote(), &landing, EXEC_USER);
    s.dev(&["stop", "exec"]);
    let mut exec = executor_as_exec_user(&s, &source, &landing);

    let ticket = s.ticket();
    let space = s.space();
    let person = s.person();
    let data = s.person_data().to_string_lossy().into_owned();
    let home_data = s.home_data().to_string_lossy().into_owned();
    han(&[
        "agent",
        "add",
        "coder2",
        "--operator",
        &person,
        "--os-user",
        AGENT_USER,
        "--space",
        &space,
        "--home",
        &ticket,
        "--home-data",
        &home_data,
        "--data",
        &data,
        "--harness",
        "fake",
        "--sandbox",
        "none",
        "--allow-advisory",
    ]);
    let agent = format!("@coder2:{}", s.get("server"));
    task_for_issue(&s, &agent).await;
    han(&[
        "agent",
        "start",
        "--space",
        &space,
        "--agent",
        &agent,
        "--repo",
        &source.to_string_lossy(),
        "--issue",
        &s.get("issue"),
        "--home",
        &ticket,
        "--data",
        &data,
    ]);

    let c = s.client().await;
    let effects = || async {
        rows(
            &c,
            Query::Effects {
                space: Some(space.clone()),
                executor: Some(s.executor()),
                include_done: true,
                window: Window::default(),
            },
        )
        .await
        .into_iter()
        .filter_map(|r| match r {
            Row::Effect(e) if e.proposer == agent => Some(e),
            _ => None,
        })
        .collect::<Vec<EffectRow>>()
    };
    until("the OS-user agent's proposal", 180, || async {
        !effects().await.is_empty()
    })
    .await;
    let e = effects().await.remove(0);
    let plan: serde_json::Value = serde_json::from_str(e.plan.as_deref().unwrap()).unwrap();
    // the executor really can't read the agent's workspace
    let ws_root = format!("/var/lib/han-agents/{AGENT_USER}");
    let peek = Command::new("runuser")
        .no_window()
        .args(["-u", EXEC_USER, "--", "ls", &ws_root])
        .output()
        .unwrap();
    assert!(!peek.status.success(), "han-exec can list {ws_root}");

    c.call(Call::EffectApprove {
        space: space.clone(),
        proposal: e.proposal.clone(),
        expires_at: None,
        note: None,
        evidence: None,
    })
    .await
    .expect("the approval is accepted");
    let done = tokio::time::timeout(Duration::from_secs(120), async {
        until("the effect to be done", 120, || async {
            effects()
                .await
                .iter()
                .any(|x| x.proposal == e.proposal && x.phase == "done")
        })
        .await;
    })
    .await;
    let exec_log =
        std::fs::read_to_string(s.dir.join("logs").join("exec-os-user.log")).unwrap_or_default();
    let _ = exec.kill();
    let _ = exec.wait();
    let _ = Command::new("systemctl")
        .no_window()
        .args(["stop", &format!("han-agent@{AGENT_USER}")])
        .status();
    assert!(
        done.is_ok(),
        "the push never landed; the executor said:\n{exec_log}"
    );
    let to = plan["args"]["to"].as_str().unwrap();
    let branch = plan["args"]["branch"].as_str().unwrap();
    // the landing remote belongs to han-exec: read it as root past git's ownership check
    let landed = Command::new("git")
        .no_window()
        .args(["-c", "safe.directory=*", "-C", &landing.to_string_lossy()])
        .args(["rev-parse", &format!("refs/heads/{branch}")])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&landed.stdout).trim(), to);
    assert!(
        plan["argsBlob"]["hash"].is_string(),
        "the plan carries the changeset as a bundle blob: {plan}"
    );
    assert!(
        plan["args"].get("source").is_none(),
        "and no host path: {plan}"
    );
}

/// Bug (#99): the node probed bwrap in its own process before it started any host. On the runner the node's
/// unit forbids namespaces (`RestrictNamespaces=yes`), so every OS-user agent was refused ("bwrap: Can't read
/// /proc/sys/kernel/overflowuid"), though the agent's own unit, `han-agent@`, can run bwrap. The node here
/// cannot use bwrap at all (`HAN_BWRAP_FORCE_UNAVAILABLE`, standing in for its hardened unit). The start must
/// not be refused by that: the host checks in its own unit and refuses in its own words. This container has no
/// bubblewrap, so the host's refusal is "bwrap is not installed", which the node's hook text never says; it
/// reaches `han agent start`'s error and the node's problems, and no agent starts.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "end to end, as root in a systemd container: run with --ignored and HAN_E2E_OS_USERS=1"]
async fn an_os_user_agents_sandbox_is_checked_by_its_host_not_by_the_node() {
    if std::env::var_os("HAN_E2E_OS_USERS").is_none() || !cfg!(target_os = "linux") {
        eprintln!("skipped: needs HAN_E2E_OS_USERS=1, Linux, root and systemd");
        return;
    }
    han(&["runner", "setup", "--agents", "1"]);
    let s = Stack::up_confined("propose", &[], &[("HAN_BWRAP_FORCE_UNAVAILABLE", "1")]);
    let space = s.space();
    let ticket = s.ticket();
    let data = s.person_data().to_string_lossy().into_owned();
    han(&[
        "agent",
        "add",
        "coder3",
        "--operator",
        &s.person(),
        "--os-user",
        AGENT_USER,
        "--space",
        &space,
        "--home",
        &ticket,
        "--home-data",
        &s.home_data().to_string_lossy(),
        "--data",
        &data,
        "--harness",
        "fake",
        "--sandbox",
        "bwrap",
    ]);
    let agent = format!("@coder3:{}", s.get("server"));
    task_for_issue(&s, &agent).await;
    let (ok, out, err) = run(&[
        "agent",
        "start",
        "--space",
        &space,
        "--agent",
        &agent,
        "--repo",
        &s.remote().to_string_lossy(),
        "--issue",
        &s.get("issue"),
        "--home",
        &ticket,
        "--data",
        &data,
    ]);
    let _ = Command::new("systemctl")
        .no_window()
        .args(["stop", &format!("han-agent@{AGENT_USER}")])
        .status();
    assert!(
        !ok,
        "the host's own refusal must fail the start:\n{out}\n{err}"
    );
    assert!(
        err.contains("bwrap is not installed"),
        "the refusal is the host's, from its own unit: {err}"
    );
    assert!(
        !err.contains("HAN_BWRAP_FORCE_UNAVAILABLE"),
        "the node must not have probed its own process: {err}"
    );
    let c = s.client().await;
    until("the node's problems carry the host's refusal", 60, || async {
        rows(
            &c,
            Query::NodeStatus {
                window: Window::default(),
            },
        )
        .await
        .into_iter()
        .any(|r| matches!(r, Row::NodeStatus(n) if n.problems.iter().any(|p| p.contains("refused to start") && p.contains("bwrap is not installed"))))
    })
    .await;
}

/// The `-p` properties of the installed `han-agent@` unit's [Service] section that confine a process (not the
/// command, user or paths), with `%i` filled in.
fn confinement_properties(unit: &str, instance: &str) -> Vec<String> {
    unit.lines()
        .map(str::trim)
        .skip_while(|l| *l != "[Service]")
        .skip(1)
        .filter(|l| !l.starts_with('#') && l.contains('='))
        .filter(|l| {
            ![
                "ExecStart=",
                "Environment=",
                "WorkingDirectory=",
                "User=",
                "Group=",
                "KillSignal=",
                "Restart=",
            ]
            .iter()
            .any(|p| l.starts_with(p))
        })
        .map(|l| l.replace("%i", instance))
        .collect()
}

/// The host's admission probe runs bubblewrap with the launch's own argv, so under the unit's real directives
/// (taken from the installed `han-agent@.service`) it must admit. With `ProtectKernelTunables=yes` in the unit
/// the probe, like the launch, failed with "bwrap: Can't mount proc on /newroot/proc: Operation not permitted".
/// Needs bubblewrap in the container.
#[test]
#[ignore = "end to end, as root in a systemd container: run with --ignored and HAN_E2E_OS_USERS=1"]
fn the_host_admits_its_sandbox_under_the_agent_units_directives() {
    if std::env::var_os("HAN_E2E_OS_USERS").is_none() || !cfg!(target_os = "linux") {
        eprintln!("skipped: needs HAN_E2E_OS_USERS=1, Linux, root and systemd");
        return;
    }
    han(&["runner", "setup", "--agents", "1"]);
    let unit = std::fs::read_to_string("/etc/systemd/system/han-agent@.service").unwrap();
    let state = format!("/var/lib/han-agents/{AGENT_USER}");
    let spawn = format!("{state}/spawn");
    std::fs::create_dir_all(&spawn).unwrap();
    let spec = serde_json::json!({
        "space": "s1:han.example",
        "agent": "@coder:han.example",
        "harness": {"name": "fake"},
        "home": {"server": "han.example"},
        "sandbox": {"backend": "bwrap"},
        "dirs": {"state": state, "keys": format!("{state}/keys"), "login": format!("{state}/login")},
    });
    std::fs::write(format!("{spawn}/host.json"), spec.to_string()).unwrap();
    let _ = std::fs::remove_file(format!("{spawn}/admission.json"));
    sh(
        "chown",
        &["-R", &format!("{AGENT_USER}:{AGENT_USER}"), &state],
    );
    let mut args: Vec<String> = ["--wait", "--collect", "--pipe", "-p", "RuntimeMaxSec=30"]
        .map(String::from)
        .into();
    for p in [format!("User={AGENT_USER}"), format!("Group={AGENT_USER}")]
        .into_iter()
        .chain(confinement_properties(&unit, AGENT_USER))
    {
        args.extend(["-p".into(), p]);
    }
    args.extend([
        "-p".into(),
        format!("WorkingDirectory={state}"),
        "/usr/local/bin/han".into(),
        "host".into(),
        "--spec".into(),
        format!("{spawn}/host.json"),
    ]);
    // The host goes on to fail later (no node to talk to); only what it wrote at its start matters.
    let out = Command::new("systemd-run")
        .no_window()
        .args(&args)
        .output()
        .unwrap();
    let admission = std::fs::read_to_string(format!("{spawn}/admission.json")).unwrap_or_default();
    assert!(
        admission.contains("Admitted"),
        "admission: {admission}\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

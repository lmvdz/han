//! The first dogfood's blockers, on the real binaries: a profile registered on a running home with no restart
//! (`han seq register`), `han agent start` naming the operator from the profile and asking for the harness
//! setup, `han agent login --check`, `han node status` and `han task list`.

use han_proc::NoWindow as _;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const SERVER: &str = "runner.han.home.arpa";
const OPERATOR: &str = "@lars:runner.han.home.arpa";
const AGENT: &str = "@coder:runner.han.home.arpa";
const HAN: &str = env!("CARGO_BIN_EXE_han");

struct Daemon {
    child: Child,
    lines: Receiver<String>,
}

impl Daemon {
    fn spawn(args: &[&str]) -> Daemon {
        let mut child = Command::new(HAN)
            .no_window()
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        Daemon { child, lines }
    }

    fn value(&self, key: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("no `{key}` line"));
            if let Some(v) = line.strip_prefix(&format!("{key} ")) {
                return v.to_owned();
            }
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Runs `han <args>` to the end and returns (success, stdout, stderr).
fn run(args: &[&str]) -> (bool, String, String) {
    let mut child = Command::new(HAN)
        .no_window()
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    while child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "han {args:?} did not finish");
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output().unwrap();
    (
        out.status.success(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

fn han(args: &[&str]) -> String {
    let (ok, out, err) = run(args);
    assert!(ok, "han {args:?}: {err}");
    out
}

fn node(data: &Path, home: &[&str]) -> Daemon {
    // Nobody sits at the machine that runs the tests: the node does not ask for a presence check before it
    // lets this connection govern (it reads the setting at each use).
    std::fs::create_dir_all(data).unwrap();
    han_node::db::NodeDb::open(&data.join("node.db"))
        .unwrap()
        .set_config(han_node::presence::REQUIRE_KEY, "")
        .unwrap();
    let data = data.to_str().unwrap();
    let mut args = vec![
        "node",
        "--actor",
        OPERATOR,
        "--data",
        data,
        "--file-keys",
        "--relay",
        "none",
        "--bind",
        "127.0.0.1:0",
    ];
    args.extend_from_slice(home);
    Daemon::spawn(&args)
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("han-bin-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_profile_is_registered_on_a_running_home_and_start_reads_its_operator() {
    let dir = temp("dogfood");
    let (device, home) = (dir.join("device"), dir.join("home"));
    let (device_s, home_s) = (device.to_str().unwrap(), home.to_str().unwrap());

    let first = node(&device, &[]);
    let device_id = first.value("node");
    first.value("endpoint");
    let created = han(&[
        "agent",
        "create",
        "--name",
        "coder",
        "--server",
        SERVER,
        "--operator",
        OPERATOR,
        "--data",
        device_s,
    ]);
    let agent_key = created
        .lines()
        .find_map(|l| l.strip_prefix("key k1 "))
        .expect("the agent's key")
        .to_owned();
    // what it tells the home's admin to run needs no restart
    assert!(created.contains("han home register"), "{created}");
    drop(first);

    // the home knows only the person at first
    let seq = Daemon::spawn(&[
        "seq",
        "--server",
        SERVER,
        "--data",
        home_s,
        "--relay",
        "none",
        "--bind",
        "127.0.0.1:0",
        "--http",
        "127.0.0.1:0",
        "--register",
        &format!("{OPERATOR}:human:d1:{device_id}"),
    ]);
    seq.value("ticket-file");
    let ticket = han(&["ticket", "--data", home_s]).trim().to_owned();

    // the agent's profile is registered over the admin socket while the home runs
    let registered = han(&[
        "seq",
        "register",
        "--data",
        home_s,
        "--actor",
        AGENT,
        "--kind",
        "agent",
        "--key-id",
        "k1",
        "--public-key",
        &agent_key,
        "--operator",
        OPERATOR,
    ]);
    assert!(registered.contains("registered"), "{registered}");
    // again: idempotent
    han(&[
        "seq",
        "register",
        "--data",
        home_s,
        "--actor",
        AGENT,
        "--public-key",
        &agent_key,
        "--operator",
        OPERATOR,
    ]);
    // an agent needs an operator; another home's actor is refused
    let (ok, _, err) = run(&[
        "seq",
        "register",
        "--data",
        home_s,
        "--actor",
        "@x:runner.han.home.arpa",
        "--public-key",
        &agent_key,
    ]);
    assert!(!ok && err.contains("operator"), "{err}");
    let (ok, _, err) = run(&[
        "seq",
        "register",
        "--data",
        home_s,
        "--actor",
        "@x:elsewhere.example",
        "--public-key",
        &agent_key,
        "--operator",
        OPERATOR,
    ]);
    assert!(!ok && err.contains("elsewhere.example"), "{err}");

    // the operator confirms it: the profile exists, with no restart of the home
    let joined = node(&device, &["--home", &ticket]);
    joined.value("endpoint");
    let done = han(&[
        "agent",
        "confirm",
        AGENT,
        "--operator",
        OPERATOR,
        "--home",
        &ticket,
        "--data",
        device_s,
    ]);
    assert!(done.contains("confirmed"), "{done}");

    // a space to start in
    let created = han(&[
        "space", "create", "--name", "team", "--home", &ticket, "--data", device_s,
    ]);
    let space = created.lines().last().unwrap().trim().to_owned();
    assert!(space.contains(':'), "{created}");

    // start finds the operator in the profile and then asks for the harness setup (it is not set up here)
    let (ok, _, err) = run(&[
        "agent",
        "start",
        "--space",
        &space,
        "--agent",
        AGENT,
        "--harness",
        "claude",
        "--home",
        &ticket,
        "--data",
        device_s,
    ]);
    assert!(
        !ok && err.contains("han agent setup-harness claude"),
        "{err}"
    );

    // the node's status and the task list, through the running node
    let status = han(&["node", "status", "--data", device_s]);
    assert!(status.contains(&format!("running: {OPERATOR}")), "{status}");
    let tasks = han(&["task", "list", &space, "--data", device_s]);
    assert!(tasks.contains("0 task(s)"), "{tasks}");

    drop(joined);
    let (ok, _, err) = run(&["node", "status", "--data", device_s]);
    assert!(!ok && err.contains("not running"), "{err}");
    drop(seq);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn login_check_says_only_whether_a_login_exists() {
    let dir = temp("login");
    let d = dir.to_str().unwrap();
    let (ok, _, err) = run(&["agent", "login", AGENT, "--check", "--data", d]);
    assert!(
        !ok && err.contains("no login") && err.contains("han agent login"),
        "{err}"
    );
    let login = dir
        .join("agents")
        .join("_coder_runner_han_home_arpa")
        .join("claude-config");
    std::fs::create_dir_all(&login).unwrap();
    std::fs::write(
        login.join(".credentials.json"),
        "han-fake-token-not-a-secret",
    )
    .unwrap();
    let (ok, out, err) = run(&["agent", "login", AGENT, "--check", "--data", d]);
    assert!(ok, "{err}");
    assert!(out.contains("logged in"));
    assert!(
        !out.contains("han-fake-token") && !err.contains("han-fake-token"),
        "the contents are never shown"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn install_service_dry_run_shows_a_per_user_service_and_changes_nothing() {
    let out = han(&[
        "node",
        "install-service",
        "--dry-run",
        "--actor",
        OPERATOR,
        "--data",
        "han-data",
        "--file-keys",
    ]);
    if cfg!(windows) {
        assert!(
            out.contains("New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME"),
            "{out}"
        );
        assert!(out.contains("--actor @lars:runner.han.home.arpa"), "{out}");
    } else {
        assert!(out.contains("ExecStart="), "{out}");
        assert!(out.contains("WantedBy=default.target"), "{out}");
    }
    assert!(out.contains("--file-keys"), "{out}");
}

/// A base64url key starts with `-` one time in 64, and clap read it as a flag. Every command that takes a key
/// must hand it to the command, whether written `--public-key -abc…` or `--public-key=-abc…`.
#[test]
fn a_public_key_that_starts_with_a_dash_is_a_value_not_a_flag() {
    let key = "-uCTmeS1VJmdjq8WyNoBVUo1_lcrMUswERD_CHaA0x0";
    let dir = temp("dash");
    let d = dir.to_str().unwrap();
    for cmd in [
        vec![
            "home",
            "register",
            "@x:runner.han.home.arpa",
            "--public-key",
            key,
        ],
        vec![
            "home",
            "register",
            "@x:runner.han.home.arpa",
            &format!("--public-key={key}"),
        ],
        vec![
            "seq",
            "register",
            "--actor",
            "@x:runner.han.home.arpa",
            "--public-key",
            key,
        ],
    ] {
        let mut args = cmd.clone();
        args.extend(["--operator", OPERATOR, "--data", d]);
        let (ok, _, err) = run(&args);
        // there is no home here, so it fails to reach one: the point is that it got that far
        assert!(!ok, "{cmd:?}");
        assert!(
            !err.contains("unexpected argument") && err.contains("is `han seq` running"),
            "{cmd:?}: {err}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

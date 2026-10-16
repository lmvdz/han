//! The real binaries, end to end: a home with a placeholder ServerName and no HTTPS (`han seq`), a device that
//! joins it by the home ticket (`han node --home`), and an agent made and confirmed through that node
//! (`han agent create`, `han agent confirm --home`). No `/.well-known/han` is fetched anywhere.

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

/// A daemon, killed when dropped, with the lines it prints.
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

    /// The value of the next line that starts with `<key> `.
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

/// Runs `han <args>` to the end (within a minute) and returns its stdout.
fn han(args: &[&str]) -> String {
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
    assert!(
        out.status.success(),
        "han {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
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

fn temp() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("han-bin-join-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_device_joins_a_placeholder_home_by_ticket_and_confirms_an_agent() {
    let dir = temp();
    let (device, home) = (dir.join("device"), dir.join("home"));
    let (device_s, home_s) = (device.to_str().unwrap(), home.to_str().unwrap());

    // the device makes its key, and the agent's key is made through it
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
    drop(first);

    // the home registers both, and serves its well-known on loopback HTTP only (nobody fetches it)
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
        "--register",
        &format!("{AGENT}:agent:k1:{agent_key}:{OPERATOR}"),
    ]);
    // `han seq` names the ticket's file and never prints the ticket (stdout may be a journal).
    let file = seq.value("ticket-file");
    assert!(file.ends_with("home.ticket"), "{file}");
    let ticket = han(&["ticket", "--data", home_s]).trim().to_owned();
    assert!(ticket.starts_with("han1home."), "{ticket}");

    // the device joins by the ticket; the operator confirms the agent
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

    // the binding is kept: a restart with no flag dials the home again
    drop(joined);
    let again = node(&device, &[]);
    again.value("endpoint");
    drop(again);
    drop(seq);
    let _ = std::fs::remove_dir_all(&dir);
}

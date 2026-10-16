//! The harness of the end-to-end tests: `han dev up` in a temp directory, the real `han` binaries, and a node
//! client to look at what a person sees. Everything a test does to the stack goes through the same commands
//! a user runs.

#![allow(dead_code)]

use han_proc::NoWindow as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use han_node::NodeApi;
use han_node::api::{BoardRow, Query, Row, ViewEvent, Window};
use han_node::rpc::{RpcClient, default_endpoint};
use serde_json::Value;

pub const HAN: &str = env!("CARGO_BIN_EXE_han");

/// One stack at a time: each is six processes, a sandbox and several QUIC endpoints.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A running dev stack. Dropping it runs `han dev down`.
pub struct Stack {
    pub dir: PathBuf,
    pub state: Value,
    _guard: MutexGuard<'static, ()>,
}

/// Whether an OS sandbox is available here.
pub fn os_sandbox() -> bool {
    let shim = han_host::sandbox::ShimCommand::new(HAN, ["shim"]);
    let found = han_host::sandbox::probe(&shim);
    for b in found.iter().filter(|b| !b.available && b.loopback_blocked) {
        eprintln!(
            "SKIP the OS sandbox: {:?} unusable here: {}",
            b.backend, b.detail
        );
    }
    found
        .iter()
        .any(|b| b.available && b.backend != han_node::api::SandboxBackend::None)
}

/// Whether the tests may fall back to the advisory sandbox: only where none exists and the environment says
/// so. CI sets `HAN_E2E_REQUIRE_SANDBOX=1`, which turns a missing sandbox into a failure.
pub fn advisory() -> bool {
    if os_sandbox() {
        return false;
    }
    assert!(
        std::env::var_os("HAN_E2E_REQUIRE_SANDBOX").is_none(),
        "no OS sandbox here, and HAN_E2E_REQUIRE_SANDBOX is set"
    );
    eprintln!("no OS sandbox here: the agent runs advisory");
    true
}

pub fn run(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(HAN)
        .no_window()
        .env("HAN_PRESENCE_LIVE", "1")
        .args(args)
        .env("HAN_RELAY", "none")
        .stdin(Stdio::null())
        .output()
        .expect("han runs");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

pub fn han(args: &[&str]) -> String {
    let (ok, out, err) = run(args);
    assert!(ok, "han {args:?} failed: {err}\n{out}");
    out
}

impl Stack {
    /// `han dev up --script <script>` plus `extra` arguments.
    pub fn up(script: &str, extra: &[&str]) -> Stack {
        Stack::up_as(script, extra, advisory(), &[])
    }

    /// Like [`Stack::up`], but the agent is never advisory, whatever the machine has: for a test that sets
    /// the agent's sandbox itself (`extra` has `--sandbox`).
    pub fn up_confined(script: &str, extra: &[&str], env: &[(&str, &str)]) -> Stack {
        Stack::up_as(script, extra, false, env)
    }

    fn up_as(script: &str, extra: &[&str], advisory: bool, env: &[(&str, &str)]) -> Stack {
        let guard = ONE_AT_A_TIME.lock().unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "han-e2e-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let log = std::fs::File::create(std::env::temp_dir().join(format!(
            "han-e2e-up-{}.log",
            dir.file_name().unwrap().to_string_lossy()
        )))
        .unwrap();
        let mut args = vec!["dev", "up", "--script", script];
        if advisory {
            args.push("--advisory");
        }
        args.extend_from_slice(extra);
        let dir_s = dir.to_string_lossy().into_owned();
        args.extend(["--dir", &dir_s]);
        // Output goes to a file, not a pipe: the stack's services outlive `dev up` and must not hold a pipe
        // that the test would wait on.
        let status = Command::new(HAN)
            .no_window()
            .env("HAN_PRESENCE_LIVE", "1")
            .args(&args)
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .status()
            .expect("han dev up runs");
        let log_path = std::env::temp_dir().join(format!(
            "han-e2e-up-{}.log",
            dir.file_name().unwrap().to_string_lossy()
        ));
        let text = std::fs::read_to_string(&log_path).unwrap_or_default();
        assert!(status.success(), "han dev up failed:\n{text}");
        let state: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("dev.json")).unwrap()).unwrap();
        let _ = std::fs::remove_file(&log_path);
        let stack = Stack {
            dir,
            state,
            _guard: guard,
        };
        // Nobody sits at the machine that runs the tests: the person's node does not ask for a presence check
        // (it reads the setting at each approval). Tests of the check itself set it back with `presence`.
        stack.presence("", "auto");
        stack
    }

    /// Sets the person's node's presence check in its `node.db`, the way `han node presence` does, but without
    /// the confirmation that command asks for (nobody is here to give it). `require` is `all` or a list.
    pub fn presence(&self, require: &str, backend: &str) {
        let db = han_node::db::NodeDb::open(&self.person_data().join("node.db")).unwrap();
        db.set_config(han_node::presence::REQUIRE_KEY, require)
            .unwrap();
        db.set_config(han_node::presence::BACKEND_KEY, backend)
            .unwrap();
    }

    pub fn get(&self, key: &str) -> String {
        self.state[key].as_str().unwrap_or_default().to_owned()
    }

    pub fn space(&self) -> String {
        self.get("space")
    }
    pub fn agent(&self) -> String {
        self.get("agent")
    }
    pub fn person(&self) -> String {
        self.get("person")
    }
    pub fn executor(&self) -> String {
        self.get("executor")
    }
    pub fn ticket(&self) -> String {
        self.get("ticket")
    }
    pub fn remote(&self) -> PathBuf {
        PathBuf::from(self.get("remote"))
    }
    pub fn person_data(&self) -> PathBuf {
        PathBuf::from(self.get("person_data"))
    }
    pub fn home_data(&self) -> PathBuf {
        PathBuf::from(self.get("home_data"))
    }

    /// `han dev <args>` on this stack. Its output goes to a file, not a pipe: a service that `dev start` or
    /// `dev restart` launches would inherit the pipe on Windows and the read would never see its end.
    pub fn dev(&self, args: &[&str]) -> String {
        let dir = self.dir.to_string_lossy().into_owned();
        let mut a = vec!["dev"];
        a.extend_from_slice(args);
        a.extend(["--dir", &dir]);
        let out = self.dir.join(format!(
            "dev-{}.out",
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let file = std::fs::File::create(&out).unwrap();
        let status = Command::new(HAN)
            .no_window()
            .env("HAN_PRESENCE_LIVE", "1")
            .args(&a)
            .stdin(Stdio::null())
            .stdout(file.try_clone().unwrap())
            .stderr(file)
            .status()
            .expect("han dev runs");
        let text = std::fs::read_to_string(&out).unwrap_or_default();
        assert!(status.success(), "han {a:?} failed: {text}");
        text
    }

    /// Starts the agent's session on the stack's issue, as the person.
    pub fn start_agent(&self) {
        let issue = self.get("issue");
        let (ok, out, err) = self.start_agent_with(&["--issue", &issue]);
        assert!(
            ok,
            "han agent start failed: {err}
{out}"
        );
    }

    /// `han agent start` with `work` (`--issue`, `--task`) as the person; returns how it ended.
    pub fn start_agent_with(&self, work: &[&str]) -> (bool, String, String) {
        let (space, agent) = (self.space(), self.agent());
        let (remote, ticket) = (self.remote().to_string_lossy().into_owned(), self.ticket());
        let data = self.person_data().to_string_lossy().into_owned();
        let mut args = vec![
            "agent", "start", "--space", &space, "--agent", &agent, "--repo", &remote, "--home",
            &ticket, "--data", &data,
        ];
        args.extend_from_slice(work);
        run(&args)
    }

    /// Kills the agent's host outright (SIGKILL: no chance to close its session), as a stop timeout or the OOM
    /// killer would.
    #[cfg(unix)]
    pub fn kill_host(&self) {
        let pattern = format!("host --spec {}", self.dir.display());
        let status = Command::new("pkill")
            .no_window()
            .args(["-KILL", "-f", &pattern])
            .status()
            .expect("pkill runs");
        assert!(status.success(), "no host process matched {pattern:?}");
    }

    /// The id of the open task for the stack's issue.
    pub async fn issue_task(&self, client: &RpcClient) -> String {
        let issue = self.get("issue");
        rows(
            client,
            Query::Tasks {
                space: self.space(),
                assignee: None,
                window: Window::default(),
            },
        )
        .await
        .into_iter()
        .find_map(|r| match r {
            Row::Task(t) if t.open && t.external.as_deref() == Some(issue.as_str()) => Some(t.task),
            _ => None,
        })
        .expect("the stack has a task for its issue")
    }

    /// A client of the person's node.
    pub async fn client(&self) -> RpcClient {
        let endpoint = default_endpoint(&self.person_data());
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match RpcClient::connect(&endpoint).await {
                Ok(c) => return c,
                Err(e) => {
                    assert!(Instant::now() < deadline, "the node does not answer: {e}");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    }

    /// The agent's row on the board, if there is one.
    pub async fn board_row(&self, client: &RpcClient) -> Option<BoardRow> {
        rows(
            client,
            Query::Board {
                space: self.space(),
                group: Default::default(),
                collapsed: vec![],
                window: Window::default(),
            },
        )
        .await
        .into_iter()
        .find_map(|r| match r {
            Row::Board(b) if b.agent == self.agent() => Some(b),
            _ => None,
        })
    }

    /// Waits for the agent's board row to satisfy `f`.
    pub async fn board_until(
        &self,
        client: &RpcClient,
        what: &str,
        secs: u64,
        f: impl Fn(&BoardRow) -> bool,
    ) -> BoardRow {
        let deadline = Instant::now() + Duration::from_secs(secs);
        let mut last = None;
        loop {
            if let Some(b) = self.board_row(client).await {
                if f(&b) {
                    return b;
                }
                last = Some(format!("{:?} / {}", b.state, b.step));
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; the board says {last:?}\n{}",
                self.logs()
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// The tail of every service log, for a failure message.
    pub fn logs(&self) -> String {
        let mut out = String::new();
        if let Ok(rd) = std::fs::read_dir(self.dir.join("logs")) {
            for e in rd.flatten() {
                let text = std::fs::read_to_string(e.path()).unwrap_or_default();
                let tail: Vec<&str> = text.lines().rev().take(12).collect();
                out.push_str(&format!(
                    "--- {}\n{}\n",
                    e.file_name().to_string_lossy(),
                    tail.into_iter().rev().collect::<Vec<_>>().join("\n")
                ));
            }
        }
        out
    }

    /// The fake agent's log of the requests it received (the files under the agent's session directory).
    pub fn agent_log(&self) -> String {
        let mut out = String::new();
        let mut stack = vec![self.person_data().join("agents")];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.file_name().is_some_and(|n| n == "fake-agent.jsonl") {
                    out.push_str(&std::fs::read_to_string(&p).unwrap_or_default());
                }
            }
        }
        out
    }

    /// The kinds of request the fake agent received, in order.
    pub fn agent_events(&self) -> Vec<String> {
        self.agent_log()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| v["event"].as_str().map(str::to_owned))
            .collect()
    }

    /// The refs of the bare remote.
    pub fn remote_refs(&self) -> String {
        git(
            &self.remote(),
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        )
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        let dir = self.dir.to_string_lossy().into_owned();
        let _ = Command::new(HAN)
            .no_window()
            .env("HAN_PRESENCE_LIVE", "1")
            .args(["dev", "down", "--dir", &dir])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .no_window()
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// The first snapshot of a view.
pub async fn rows(client: &RpcClient, q: Query) -> Vec<Row> {
    let mut v = client.view(q);
    match tokio::time::timeout(Duration::from_secs(10), v.next()).await {
        Ok(Some(ViewEvent::Snapshot { rows, .. })) => rows,
        _ => Vec::new(),
    }
}

/// Waits until `f` is true.
pub async fn until<F, Fut>(what: &str, secs: u64, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if f().await {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

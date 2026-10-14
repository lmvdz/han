//! `han dev`: a complete local HAN on loopback, made with the same commands a real setup uses.
//!
//! `han dev up --dir D` makes, in `D`: a home (`han home init`, `han seq`), a person's node (`han node`), a space
//! with the dogfood policy (pushes gated), an agent (`han agent add`), an executor (`han executor add`,
//! `han exec`), a fake GitHub with one `agent-ready` issue (`han-github`) and a local bare git remote. Every
//! process is the real `han` binary; the only fakes are GitHub and, with `--harness fake`, the model: the
//! agent is `han-fake-agent`, reached over the real ACP path, the real shim and the real OS sandbox.
//!
//! `han dev down --dir D` stops everything and removes `D`. `han dev restart <service>` kills one service as a
//! crash would and starts it again with the same command line.
//!
//! The live-Claude variant (`--harness claude`) installs the pinned Claude adapter and needs a login
//! (`han agent login`); it is for people, not for CI.

mod github;

use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clap::{Args, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// The placeholder ServerName of a dev home.
pub const SERVER: &str = "dev.han.home.arpa";
/// The base64 body of a throwaway RSA key the fake GitHub's "App" signs with. It opens nothing: the fake
/// GitHub accepts any JWT.
const FAKE_APP_KEY: &str = include_str!("dev/fake_app_key.b64");
/// The issue the fake GitHub serves.
pub const ISSUE_TEXT: &str = "The login check accepts empty tokens. Make `login` reject a token shorter than 8 characters. DEV-ISSUE-MARKER";

/// Options of `han dev`.
#[derive(Args)]
pub struct DevArgs {
    #[command(subcommand)]
    cmd: DevCmd,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Harness {
    /// `han-fake-agent` replaying a script over the real ACP path.
    Fake,
    /// The pinned Claude Code adapter (needs `han agent login`; never for CI).
    Claude,
}

#[derive(Subcommand)]
enum DevCmd {
    /// Starts the stack and prints how to use it.
    Up(UpArgs),
    /// Stops the stack and removes its directory.
    Down {
        /// The directory `up` made.
        #[arg(long, default_value = "han-dev")]
        dir: PathBuf,
        /// Stop everything but keep the directory.
        #[arg(long)]
        keep: bool,
    },
    /// Kills one service as a crash would and starts it again with the same command line.
    Restart {
        /// `home`, `node`, `exec` or `github`.
        service: String,
        /// The directory `up` made.
        #[arg(long, default_value = "han-dev")]
        dir: PathBuf,
    },
    /// Kills one service as a crash would and leaves it down (`start` brings it back).
    Stop {
        /// `home`, `node`, `exec` or `github`.
        service: String,
        /// The directory `up` made.
        #[arg(long, default_value = "han-dev")]
        dir: PathBuf,
    },
    /// Starts a service that `stop` killed, with the same command line.
    Start {
        /// `home`, `node`, `exec` or `github`.
        service: String,
        /// The directory `up` made.
        #[arg(long, default_value = "han-dev")]
        dir: PathBuf,
    },
    /// Says what runs.
    Status {
        /// The directory `up` made.
        #[arg(long, default_value = "han-dev")]
        dir: PathBuf,
    },
    /// The fake GitHub, served until stopped (`up` runs it).
    #[command(hide = true)]
    FakeGithub {
        /// Where to listen.
        #[arg(long)]
        addr: SocketAddr,
        /// The JSON file of issues, read at every request.
        #[arg(long)]
        issues: PathBuf,
    },
}

#[derive(Args)]
struct UpArgs {
    /// The directory to build the stack in.
    #[arg(long, default_value = "han-dev")]
    dir: PathBuf,
    /// The agent's harness.
    #[arg(long, value_enum, default_value_t = Harness::Fake)]
    harness: Harness,
    /// The agent's sandbox: `auto` (MXC on Windows, bubblewrap on Linux), `mxc`, `bwrap`.
    #[arg(long, default_value = "auto")]
    sandbox: String,
    /// Run the agent with no OS sandbox (advisory; it shows in status). For machines that have none.
    #[arg(long)]
    advisory: bool,
    /// What the fake agent does: `simple` (messages, a thought and a tool call, then waits for input),
    /// `propose` (also writes a file and proposes a `git_push`), `propose-late` (the same after a pause), or the path of a script
    /// (`han-fake-agent replay`).
    #[arg(long, default_value = "simple")]
    script: String,
    /// Give the space a budget, `UNIT=AMOUNT` (for example `USD-cent=100`): the space policy's `budget`.
    #[arg(long)]
    budget: Option<String>,
    /// Run the fake agent in API-key mode against the model provider at this `https://` URL: its model calls
    /// go through the session proxy, which holds the key and counts the tokens. With `--budget` the node
    /// gets a price table for that unit: the model `fake-model` at one unit per token, in and out.
    #[arg(long)]
    model_url: Option<String>,
    /// A PEM file of the certificate authority that signed the provider's certificate (a test provider has
    /// its own).
    #[arg(long, requires = "model_url")]
    model_ca: Option<PathBuf>,
    /// Do not register the executor (to see how a proposal with no executor is refused).
    #[arg(long)]
    no_executor: bool,
    /// Start the agent's session on the issue when everything is up.
    #[arg(long)]
    start: bool,
    /// The `han-fake-agent` program, if it is not next to `han`.
    #[arg(long, env = "HAN_FAKE_AGENT")]
    fake_agent: Option<PathBuf>,
    /// The `han-github` program, if it is not next to `han`.
    #[arg(long, env = "HAN_GITHUB")]
    github_bin: Option<PathBuf>,
}

/// One running service, as `dev.json` keeps it.
#[derive(Clone, Serialize, Deserialize)]
struct Service {
    pid: u32,
    program: PathBuf,
    args: Vec<String>,
    log: PathBuf,
}

/// What `up` made, kept in `<dir>/dev.json` for `down`, `restart`, `status` and the tests.
#[derive(Serialize, Deserialize)]
struct State {
    dir: PathBuf,
    server: String,
    person: String,
    agent: String,
    executor: Option<String>,
    space: String,
    ticket: String,
    home_data: PathBuf,
    person_data: PathBuf,
    exec_dir: PathBuf,
    remote: PathBuf,
    issue: String,
    github_url: String,
    harness: Harness,
    services: BTreeMap<String, Service>,
}

pub async fn run(args: DevArgs) -> Res<()> {
    match args.cmd {
        DevCmd::Up(a) => {
            let dir = abs(&a.dir);
            let r = up(a).await;
            if r.is_err() {
                // a half-made stack leaves nothing running
                kill_strays(&dir);
            }
            r
        }
        DevCmd::Down { dir, keep } => down(&abs(&dir), keep),
        DevCmd::Restart { service, dir } => {
            let dir = abs(&dir);
            stop(&dir, &service).await?;
            start(&dir, &service).await
        }
        DevCmd::Stop { service, dir } => stop(&abs(&dir), &service).await,
        DevCmd::Start { service, dir } => start(&abs(&dir), &service).await,
        DevCmd::Status { dir } => status(&abs(&dir)),
        DevCmd::FakeGithub { addr, issues } => github::serve(addr, issues).await,
    }
}

fn abs(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn free_udp() -> Res<u16> {
    Ok(UdpSocket::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn free_tcp() -> Res<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// A program that sits next to `han` (cargo puts every binary of the workspace in one directory).
fn sibling(name: &str, given: Option<&PathBuf>) -> Res<PathBuf> {
    if let Some(p) = given {
        return Ok(p.clone());
    }
    let exe = std::env::current_exe()?;
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let mut dir = exe.parent();
    // test binaries live in `deps/`, one level below the binaries
    for _ in 0..2 {
        let Some(d) = dir else { break };
        let p = d.join(&file);
        if p.is_file() {
            return Ok(p);
        }
        dir = d.parent();
    }
    Err(format!(
        "{name} is not next to han: build it (`cargo build -p {}`) or point at it",
        if name == "han-github" {
            "han-github"
        } else {
            "han-host --bin han-fake-agent"
        }
    )
    .into())
}

fn spawn(name: &str, state_dir: &Path, program: &Path, args: &[String]) -> Res<Service> {
    let logs = state_dir.join("logs");
    std::fs::create_dir_all(&logs)?;
    let log = logs.join(format!("{name}.log"));
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)?;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .env("HAN_RELAY", "none")
        .stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS: the service outlives `han dev up`
        cmd.creation_flags(0x0000_0200 | 0x0000_0008);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    let child = cmd.spawn()?;
    Ok(Service {
        pid: child.id(),
        program: program.to_path_buf(),
        args: args.to_vec(),
        log,
    })
}

fn kill(pid: u32, tree: bool) {
    #[cfg(windows)]
    {
        let mut c = Command::new("taskkill");
        c.args(["/PID", &pid.to_string(), "/F"]);
        if tree {
            c.arg("/T");
        }
        let _ = c.stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    #[cfg(unix)]
    {
        let target = if tree {
            format!("-{pid}")
        } else {
            pid.to_string()
        };
        let _ = Command::new("kill")
            .args(["-KILL", "--", &target])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Kills every process whose command line mentions `dir`: hosts and agents that outlived a killed node.
fn kill_strays(dir: &Path) {
    #[cfg(windows)]
    {
        let script = format!(
            "Get-CimInstance Win32_Process | Where-Object {{ $_.CommandLine -like '*{}*' -and $_.ProcessId -ne $PID }} | ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }}",
            dir.to_string_lossy().replace('\'', "''")
        );
        let _ = Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(unix)]
    {
        let _ = Command::new("pkill")
            .args(["-KILL", "-f", &dir.to_string_lossy()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

async fn wait_until<F, Fut>(what: &str, secs: u64, mut f: F) -> Res<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if f().await {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!("timed out waiting for {what}").into());
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

/// Runs `han <args>` (this binary), shows what it printed indented, and returns its stdout.
fn han(args: &[String]) -> Res<String> {
    println!("$ han {}", args.join(" "));
    let out = Command::new(std::env::current_exe()?)
        .args(args)
        .env("HAN_RELAY", "none")
        .stdin(Stdio::null())
        .output()?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    for l in stdout.lines() {
        println!("    {l}");
    }
    if !out.status.success() {
        return Err(format!(
            "han {} failed: {}",
            args.first().cloned().unwrap_or_default(),
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(stdout)
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_owned()).collect()
}

/// The space policy of the dogfood: members create work, and every effect that leaves the machine waits for
/// an owner's approval.
/// The model the dev stack's price table knows.
const FAKE_MODEL: &str = "fake-model";

/// `UNIT=AMOUNT` of `--budget`.
fn parse_budget(text: &str) -> Res<(String, u64)> {
    let (unit, amount) = text
        .split_once('=')
        .ok_or("--budget is UNIT=AMOUNT, for example USD-cent=100")?;
    let ok = !unit.is_empty()
        && unit.len() <= 32
        && unit
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !ok {
        return Err(format!("{unit:?} is not a budget unit ([A-Za-z0-9_-]{{1,32}})").into());
    }
    Ok((unit.to_owned(), amount.parse()?))
}

fn dogfood_policy(budget: Option<&(String, u64)>) -> Res<String> {
    let mut p = han_seq::build::space_policy("members", "invite");
    if let han_core::Value::Object(m) = &mut p {
        if let Some((unit, amount)) = budget {
            m.insert(
                "budget".into(),
                han_core::json::parse(
                    format!(r#"{{"unit":"{unit}","amount":{amount}}}"#).as_bytes(),
                )?,
            );
        }
        m.insert(
            "approvals".into(),
            han_core::json::parse(
                br#"[{"action":"git_push","approvers":["owner"]},{"action":"open_pr","approvers":["owner"]},{"action":"http_request","approvers":["owner"]}]"#,
            )?,
        );
    }
    Ok(String::from_utf8(han_core::json::canonicalize(&p)?)?)
}

const SIMPLE_SCRIPT: &str = r#"{
  "agent": {"name": "han-fake-agent", "version": "0.0.34"},
  "modes": {"current": "off", "available": ["off"]},
  "turns": [
    {"steps": [
      {"type": "thought", "text": "Reading the task before touching anything."},
      {"type": "tool_call", "id": "t1", "title": "read README.md", "kind": "read", "locations": ["README.md"], "rawInput": {"path": "README.md"}},
      {"type": "tool_update", "id": "t1", "status": "completed", "output": "a demo repository"},
      {"type": "message", "text": "I read the task. Which branch should I target?"},
      {"type": "end", "stopReason": "end_turn"}
    ]},
    {"steps": [{"type": "message", "text": "Noted."}, {"type": "end", "stopReason": "end_turn"}]}
  ]
}"#;

/// Two model calls through the session proxy: the first is paid for, the second finds the budget spent.
const MODEL_CALLS_SCRIPT: &str = r#"{
  "agent": {"name": "han-fake-agent", "version": "0.0.34"},
  "modes": {"current": "off", "available": ["off"]},
  "turns": [
    {"steps": [
      {"type": "model_call", "model": "fake-model", "max_tokens": 20},
      {"type": "model_call", "model": "fake-model", "max_tokens": 20},
      {"type": "message", "text": "Both calls are made."},
      {"type": "end", "stopReason": "end_turn"}
    ]},
    {"steps": [{"type": "message", "text": "Noted."}, {"type": "end", "stopReason": "end_turn"}]}
  ]
}"#;

/// Like `propose`, but waits first, so a test can take the home away before the proposal is made.
const PROPOSE_LATE_SCRIPT: &str = r#"{
  "agent": {"name": "han-fake-agent", "version": "0.0.34"},
  "modes": {"current": "off", "available": ["off"]},
  "turns": [
    {"steps": [
      {"type": "message", "text": "Working on it."},
      {"type": "write_file", "path": "src/auth/login.rs", "content": "pub fn login(user: &str, token: &str) -> bool {\n    !user.is_empty() && token.len() >= 8\n}\n"},
      {"type": "sleep", "ms": 9000},
      {"type": "mcp_call", "id": "p1", "server": "han", "tool": "propose", "args": {"summary": "Push the login check on the session branch", "kind": "git_push", "effect": {}}},
      {"type": "message", "text": "The push is proposed."},
      {"type": "end", "stopReason": "end_turn"}
    ]},
    {"steps": [{"type": "message", "text": "Noted."}, {"type": "end", "stopReason": "end_turn"}]}
  ]
}"#;

const PROPOSE_SCRIPT: &str = r#"{
  "agent": {"name": "han-fake-agent", "version": "0.0.34"},
  "modes": {"current": "off", "available": ["off"]},
  "turns": [
    {"steps": [
      {"type": "thought", "text": "Plan: reject short tokens, then push."},
      {"type": "tool_call", "id": "w1", "title": "write src/auth/login.rs", "kind": "edit", "locations": ["src/auth/login.rs"], "rawInput": {"path": "src/auth/login.rs"}},
      {"type": "write_file", "path": "src/auth/login.rs", "content": "pub fn login(user: &str, token: &str) -> bool {\n    !user.is_empty() && token.len() >= 8\n}\n"},
      {"type": "tool_update", "id": "w1", "status": "completed", "output": "Wrote src/auth/login.rs"},
      {"type": "message", "text": "The check is written."},
      {"type": "mcp_call", "id": "p1", "server": "han", "tool": "propose", "args": {"summary": "Push the login check on the session branch", "kind": "git_push", "effect": {}}},
      {"type": "message", "text": "The push is proposed and waits for approval; it has not run."},
      {"type": "end", "stopReason": "end_turn"}
    ]},
    {"steps": [{"type": "message", "text": "Noted."}, {"type": "end", "stopReason": "end_turn"}]}
  ]
}"#;

fn git(dir: &Path, args: &[&str]) -> Res<()> {
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=dev",
            "-c",
            "user.email=dev@example.invalid",
        ])
        .args(["-c", "init.defaultBranch=main", "-c", "core.autocrlf=false"])
        .args(args)
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(())
}

/// A bare remote with one commit on `main`.
fn make_remote(dir: &Path) -> Res<PathBuf> {
    let seed = dir.join("seed");
    std::fs::create_dir_all(seed.join("src"))?;
    std::fs::write(seed.join("README.md"), "a demo repository\n")?;
    std::fs::write(seed.join("src/lib.rs"), "pub mod auth;\n")?;
    git(&seed, &["init", "-q"])?;
    git(&seed, &["add", "-A"])?;
    git(&seed, &["commit", "-q", "-m", "init"])?;
    let bare = dir.join("remote.git");
    git(dir, &["clone", "-q", "--bare", &s(&seed), &s(&bare)])?;
    Ok(bare)
}

fn pem() -> String {
    let body = FAKE_APP_KEY.trim();
    let mut out = String::from("-----BEGIN RSA PRIVATE KEY-----\n");
    for chunk in body.as_bytes().chunks(64) {
        out.push_str(&String::from_utf8_lossy(chunk));
        out.push('\n');
    }
    out.push_str("-----END RSA PRIVATE KEY-----\n");
    out
}

async fn up(a: UpArgs) -> Res<()> {
    let dir = abs(&a.dir);
    if dir.join("dev.json").exists() {
        return Err(format!(
            "{} already holds a stack: `han dev down --dir {}` first",
            dir.display(),
            dir.display()
        )
        .into());
    }
    std::fs::create_dir_all(&dir)?;
    let exe = std::env::current_exe()?;
    let (home_data, person_data, exec_dir) =
        (dir.join("home"), dir.join("person"), dir.join("exec"));
    let person = format!("@you:{SERVER}");
    let agent = format!("@coder:{SERVER}");
    let exec_actor = format!("@exec:{SERVER}");
    let mut services = BTreeMap::new();
    let mut state = State {
        dir: dir.clone(),
        server: SERVER.into(),
        person: person.clone(),
        agent: agent.clone(),
        executor: (!a.no_executor).then(|| exec_actor.clone()),
        space: String::new(),
        ticket: String::new(),
        home_data: home_data.clone(),
        person_data: person_data.clone(),
        exec_dir: exec_dir.clone(),
        remote: PathBuf::new(),
        issue: "github:dev/demo#1".into(),
        github_url: String::new(),
        harness: a.harness,
        services: BTreeMap::new(),
    };
    let budget = a.budget.as_deref().map(parse_budget).transpose()?;
    if a.model_url.is_some() && a.harness != Harness::Fake {
        return Err("--model-url is for the fake harness".into());
    }
    // Programs first, so a missing one fails before anything runs.
    let fake_agent = match a.harness {
        Harness::Fake => Some(sibling("han-fake-agent", a.fake_agent.as_ref())?),
        Harness::Claude => None,
    };
    let github_bin = sibling("han-github", a.github_bin.as_ref())?;

    println!("1. the home");
    let (udp, http) = (free_udp()?, free_tcp()?);
    han(&[
        "home".into(),
        "init".into(),
        "--server".into(),
        SERVER.into(),
        "--data".into(),
        s(&home_data),
        "--bind".into(),
        format!("127.0.0.1:{udp}"),
        "--http".into(),
        format!("127.0.0.1:{http}"),
        "--relay".into(),
        "none".into(),
    ])?;
    services.insert(
        "home".to_owned(),
        spawn(
            "home",
            &dir,
            &exe,
            &strs(&["seq", "--data", &s(&home_data)]),
        )?,
    );
    let admin = crate::seqadmin::endpoint(&home_data);
    wait_until("the home", 60, || {
        let admin = admin.clone();
        async move { han_node::rpc::connect_local(&admin).await.is_ok() }
    })
    .await?;
    let ticket = han(&["ticket".into(), "--data".into(), s(&home_data)])?
        .trim()
        .to_owned();
    state.ticket = ticket.clone();

    println!("2. the person and their node");
    let keys = han_node::keys::FileKeyStore::new(person_data.join("keys"));
    let seed = han_node::keys::KeyStore::load_or_create(&keys, han_node::keys::DEVICE_KEY)?;
    let person_key = han_core::SigningKey::from_seed(seed)
        .public_key()
        .to_base64url();
    han(&[
        "home".into(),
        "register".into(),
        person.clone(),
        "--kind".into(),
        "human".into(),
        "--key-id".into(),
        "d1".into(),
        "--public-key".into(),
        person_key,
        "--data".into(),
        s(&home_data),
    ])?;
    services.insert(
        "node".to_owned(),
        spawn(
            "node",
            &dir,
            &exe,
            &strs(&[
                "node",
                "--actor",
                &person,
                "--data",
                &s(&person_data),
                "--file-keys",
                "--relay",
                "none",
                "--bind",
                "127.0.0.1:0",
                "--home",
                &ticket,
            ]),
        )?,
    );
    wait_until("the node", 60, || {
        let d = person_data.clone();
        async move {
            Command::new(std::env::current_exe().unwrap_or_default())
                .args(["node", "status", "--data", &s(&d)])
                .output()
                .is_ok_and(|o| o.status.success())
        }
    })
    .await?;

    println!("3. a space with the dogfood policy");
    let out = han(&[
        "space".into(),
        "create".into(),
        "--name".into(),
        "dev".into(),
        "--home".into(),
        ticket.clone(),
        "--data".into(),
        s(&person_data),
    ])?;
    let space = out
        .lines()
        .last()
        .map(|l| l.trim().to_owned())
        .filter(|l| l.contains(':'))
        .ok_or("no space id")?;
    state.space = space.clone();
    let policy = dir.join("policy.json");
    std::fs::write(&policy, dogfood_policy(budget.as_ref())?)?;
    han(&[
        "space".into(),
        "policy".into(),
        space.clone(),
        s(&policy),
        "--data".into(),
        s(&person_data),
    ])?;

    println!("4. the remote and the fake GitHub");
    let remote = make_remote(&dir)?;
    state.remote = remote.clone();
    let gh_dir = dir.join("github");
    std::fs::create_dir_all(&gh_dir)?;
    let issues = gh_dir.join("issues.json");
    std::fs::write(
        &issues,
        serde_json::to_string_pretty(&serde_json::json!([{
            "number": 1, "title": "Reject short tokens", "state": "open",
            "html_url": "https://github.com/dev/demo/issues/1",
            "labels": [{"name": "agent-ready"}], "assignees": [],
            "body": ISSUE_TEXT, "updated_at": "2026-01-01T00:00:00Z"
        }]))?,
    )?;
    let gh_port = free_tcp()?;
    let gh_url = format!("http://127.0.0.1:{gh_port}");
    state.github_url = gh_url.clone();
    services.insert(
        "github".to_owned(),
        spawn(
            "github",
            &dir,
            &exe,
            &strs(&[
                "dev",
                "fake-github",
                "--addr",
                &format!("127.0.0.1:{gh_port}"),
                "--issues",
                &s(&issues),
            ]),
        )?,
    );
    let pem_path = gh_dir.join("app.pem");
    std::fs::write(&pem_path, pem())?;
    let gh_toml = gh_dir.join("han-github.toml");
    std::fs::write(
        &gh_toml,
        format!(
            "space = {space:?}\ndata_dir = {data:?}\nrepos = [\"dev/demo\"]\npoll_secs = 10\n\n[app]\napp_id = 1\ninstallation_id = 7\nprivate_key = {key:?}\napi_base = {gh_url:?}\n\n[tasks]\npool = [{agent:?}]\n",
            data = s(&person_data),
            key = s(&pem_path),
        ),
    )?;

    // The model provider the session proxy forwards to, and the node's prices for it.
    if let (Some(_), Some((unit, _))) = (&a.model_url, &budget) {
        // one unit per token, in and out (prices are per million tokens)
        std::fs::write(
            person_data.join(han_registry::PRICES_FILE),
            serde_json::to_string_pretty(&serde_json::json!({
                unit: {FAKE_MODEL: {"input": 1_000_000, "output": 1_000_000}}
            }))?,
        )?;
    }

    println!("5. the agent");
    let mut add = vec![
        "agent".into(),
        "add".into(),
        "coder".into(),
        "--operator".into(),
        person.clone(),
        "--space".into(),
        space.clone(),
        "--home".into(),
        ticket.clone(),
        "--home-data".into(),
        s(&home_data),
        "--data".into(),
        s(&person_data),
    ];
    match a.harness {
        Harness::Fake => {
            let fake = dir.join("fake");
            std::fs::create_dir_all(&fake)?;
            let script_path = match a.script.as_str() {
                "simple" | "propose" | "propose-late" | "model-calls" => {
                    let p = fake.join("script.json");
                    std::fs::write(
                        &p,
                        match a.script.as_str() {
                            "simple" => SIMPLE_SCRIPT,
                            "propose" => PROPOSE_SCRIPT,
                            "model-calls" => MODEL_CALLS_SCRIPT,
                            _ => PROPOSE_LATE_SCRIPT,
                        },
                    )?;
                    p
                }
                other => {
                    let p = fake.join("script.json");
                    std::fs::copy(other, &p)?;
                    p
                }
            };
            let mut driver = fake_driver(&fake_agent.clone().unwrap_or_default(), &script_path);
            if let Some(url) = &a.model_url {
                // API-key mode: the proxy holds the key and forwards to the fake model; the agent finds
                // the proxy and its session token in its environment
                let key = fake.join("model.key");
                std::fs::write(&key, "sk-fake-dev-model-key")?;
                driver.credentials = han_host::driver::CredentialMode::ApiKey {
                    provider_base: url.clone(),
                    header: han_host::driver::KeyHeader::XApiKey,
                    dummy_key: "han-dummy-key-not-a-secret".into(),
                    paths: vec!["/v1/messages".into()],
                    provider_id: None,
                    api: None,
                    models: vec![FAKE_MODEL.into()],
                    ca_file: a.model_ca.as_deref().map(abs),
                };
                driver
                    .env
                    .push(("HAN_MODEL_URL".into(), "{proxy_url}".into()));
                driver
                    .env
                    .push(("HAN_MODEL_TOKEN".into(), "{proxy_token}".into()));
                add.extend(strs(&["--model-key-file", &s(&key)]));
            }
            let driver_path = fake.join("driver.json");
            std::fs::write(&driver_path, serde_json::to_string_pretty(&driver)?)?;
            let record = han_registry::HarnessInstall {
                harness: "fake".into(),
                node: PathBuf::from("node"),
                adapter_dir: fake.clone(),
                package: "han-fake-agent".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                integrity: String::new(),
                driver: Some(driver_path),
            };
            record.save(&person_data)?;
            add.extend(strs(&["--harness", "fake"]));
        }
        Harness::Claude => add.extend(strs(&["--harness", "claude"])),
    }
    if a.advisory {
        add.extend(strs(&["--sandbox", "none", "--allow-advisory"]));
    } else {
        add.extend(["--sandbox".into(), a.sandbox.clone()]);
    }
    han(&add)?;

    if !a.no_executor {
        println!("6. the executor");
        han(&[
            "executor".into(),
            "add".into(),
            "exec".into(),
            "--operator".into(),
            person.clone(),
            "--dir".into(),
            s(&exec_dir),
            "--repo".into(),
            format!("{}={}", s(&remote), s(&remote)),
            "--space".into(),
            space.clone(),
            "--home".into(),
            ticket.clone(),
            "--home-data".into(),
            s(&home_data),
            "--data".into(),
            s(&person_data),
            "--relay".into(),
            "none".into(),
        ])?;
        services.insert(
            "exec".to_owned(),
            spawn(
                "exec",
                &dir,
                &exe,
                &strs(&[
                    "exec",
                    "--actor",
                    &exec_actor,
                    "--space",
                    &space,
                    "--home",
                    &ticket,
                    "--keys",
                    &s(&exec_dir),
                    "--data",
                    &s(&exec_dir.join("state")),
                    "--config",
                    &s(&exec_dir.join("exec.json")),
                    "--relay",
                    "none",
                ]),
            )?,
        );
    }

    println!("7. the issue becomes a task");
    // `up` of the tool itself is not a Tokio-less process: the poller is its own program.
    services.insert(
        "poller".to_owned(),
        spawn(
            "poller",
            &dir,
            &github_bin,
            &strs(&["run", "--config", &s(&gh_toml)]),
        )?,
    );
    let task_ready = wait_until("the issue's task", 90, || {
        let d = person_data.clone();
        let sp = space.clone();
        async move {
            Command::new(std::env::current_exe().unwrap_or_default())
                .args(["task", "list", &sp, "--data", &s(&d)])
                .output()
                .is_ok_and(|o| {
                    o.status.success()
                        && String::from_utf8_lossy(&o.stdout).contains("github:dev/demo#1")
                })
        }
    })
    .await;
    state.services = services;
    std::fs::write(dir.join("dev.json"), serde_json::to_string_pretty(&state)?)?;
    task_ready?;

    if a.start {
        println!("8. the session");
        han(&start_args(&state))?;
    }
    print_usage(&state);
    Ok(())
}

fn start_args(st: &State) -> Vec<String> {
    vec![
        "agent".into(),
        "start".into(),
        "--space".into(),
        st.space.clone(),
        "--agent".into(),
        st.agent.clone(),
        "--repo".into(),
        s(&st.remote),
        "--issue".into(),
        st.issue.clone(),
        "--home".into(),
        st.ticket.clone(),
        "--data".into(),
        s(&st.person_data),
    ]
}

fn fake_driver(program: &Path, script: &Path) -> han_host::driver::DriverConfig {
    let _ = script;
    han_host::driver::DriverConfig {
        name: "fake".into(),
        harness: "fake-pi".into(),
        command: vec![
            s(program),
            "replay".into(),
            "{adapter_dir}/script.json".into(),
        ],
        env: vec![(
            "HAN_FAKE_AGENT_LOG".into(),
            "{agent_dir}/fake-agent.jsonl".into(),
        )],
        mcp: han_host::driver::McpMode::AcpMcpServers,
        credentials: han_host::driver::CredentialMode::Subscription {
            connect_hosts: Vec::new(),
        },
        connect_hosts: Vec::new(),
        tested_version: "0.0.34".into(),
        adapter: Default::default(),
        gate_fidelity: han_host::acp::GateFidelity::PromptOnly,
        isolation: Default::default(),
        node_flags: Vec::new(),
        strict_mode: None,
        auto_mode: None,
        session_log_dir: None,
    }
}

fn print_usage(st: &State) {
    let d = s(&st.person_data);
    println!();
    println!("A local HAN is up in {}", st.dir.display());
    println!("  home     {} (data {})", st.server, st.home_data.display());
    println!(
        "  person   {}   agent {}   executor {}",
        st.person,
        st.agent,
        st.executor.as_deref().unwrap_or("(none)")
    );
    println!("  space    {}", st.space);
    println!(
        "  remote   {}   fake GitHub {}",
        st.remote.display(),
        st.github_url
    );
    println!("  logs     {}", st.dir.join("logs").display());
    println!();
    println!("Start the agent on the issue:");
    println!("  han {}", start_args(st).join(" "));
    println!("Watch it:");
    println!("  han board {} --data {d}", st.space);
    println!("  han tui --data {d}");
    println!("Stop everything and clean up:");
    println!("  han dev down --dir {}", st.dir.display());
}

fn load(dir: &Path) -> Res<State> {
    let text = std::fs::read_to_string(dir.join("dev.json")).map_err(|e| {
        format!(
            "{}: {e} (is there a stack? `han dev up --dir …`)",
            dir.join("dev.json").display()
        )
    })?;
    Ok(serde_json::from_str(&text)?)
}

fn down(dir: &Path, keep: bool) -> Res<()> {
    if let Ok(st) = load(dir) {
        // the node last: its children (hosts) go with it
        for name in ["poller", "exec", "github", "node", "home"] {
            if let Some(svc) = st.services.get(name) {
                kill(svc.pid, true);
                println!("stopped {name} (pid {})", svc.pid);
            }
        }
    }
    kill_strays(dir);
    if keep {
        println!("kept {}", dir.display());
        return Ok(());
    }
    let mut last = None;
    for _ in 0..40 {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => {
                println!("removed {}", dir.display());
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                last = Some(e);
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    }
    Err(format!(
        "could not remove {}: {}",
        dir.display(),
        last.map(|e| e.to_string()).unwrap_or_default()
    )
    .into())
}

fn service_name(service: &str) -> &str {
    match service {
        "github" => "poller", // the fake server keeps running; the poller is what `github` means
        other => other,
    }
}

async fn stop(dir: &Path, service: &str) -> Res<()> {
    let st = load(dir)?;
    let name = service_name(service);
    let svc = st
        .services
        .get(name)
        .ok_or_else(|| format!("no service {service}: home, node, exec or github"))?;
    kill(svc.pid, false);
    println!("stopped {name} (pid {})", svc.pid);
    tokio::time::sleep(Duration::from_millis(500)).await;
    Ok(())
}

async fn start(dir: &Path, service: &str) -> Res<()> {
    let mut st = load(dir)?;
    let name = service_name(service);
    let old = st
        .services
        .get(name)
        .cloned()
        .ok_or_else(|| format!("no service {service}: home, node, exec or github"))?;
    let svc = spawn(name, dir, &old.program, &old.args)?;
    println!("started {name}: pid {} -> {}", old.pid, svc.pid);
    st.services.insert(name.to_owned(), svc);
    std::fs::write(dir.join("dev.json"), serde_json::to_string_pretty(&st)?)?;
    match name {
        "home" => {
            let admin = crate::seqadmin::endpoint(&st.home_data);
            wait_until("the home", 60, || {
                let admin = admin.clone();
                async move { han_node::rpc::connect_local(&admin).await.is_ok() }
            })
            .await?;
        }
        "node" => {
            let d = st.person_data.clone();
            wait_until("the node", 60, || {
                let d = d.clone();
                async move {
                    Command::new(std::env::current_exe().unwrap_or_default())
                        .args(["node", "status", "--data", &s(&d)])
                        .output()
                        .is_ok_and(|o| o.status.success())
                }
            })
            .await?;
        }
        _ => {}
    }
    Ok(())
}

fn status(dir: &Path) -> Res<()> {
    let st = load(dir)?;
    for (name, svc) in &st.services {
        println!("{name}: pid {} log {}", svc.pid, svc.log.display());
    }
    println!("space {}", st.space);
    Ok(())
}

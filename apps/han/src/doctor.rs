//! `han doctor [--json]`: what is wrong on this machine, one line of cause and the exact command that fixes it.
//!
//! It reads, and changes nothing: the running node's status (its problems, the links of its spaces, the outbox
//! entries that hold a space back), `agents.json` and the harness records, the agents' logins (whether one
//! exists, never what is in it), the device's presence setting, the Rust toolchain the sandboxes get and the
//! repositories pin, and the `han` processes that run a binary which was replaced on disk.
//!
//! The work is split in two so it can be tested without a machine in a given state: [`gather`] reads the
//! machine into [`Facts`], and [`analyze`] turns facts into [`Finding`]s. Exit code 0 means healthy, 1 means at
//! least one finding. `--json` prints [`Report`]; the schema is documented in docs/runner.md.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::Args;
use han_node::NodeApi;
use han_node::api::{Query, Row, SpaceLink, ViewEvent, Window};
use han_node::rpc::{RpcClient, default_endpoint};
use han_registry::{AgentsFile, HarnessInstall, valid_os_user};
use serde::Serialize;

/// The `schema` of `--json`; it changes only when a field is removed or renamed.
pub const SCHEMA: &str = "han-doctor/1";

/// Options of `han doctor`.
#[derive(Args)]
pub struct DoctorArgs {
    /// Print the report as JSON (schema `han-doctor/1`, see docs/runner.md).
    #[arg(long)]
    json: bool,
    /// The data directory of the node.
    #[arg(long, env = "HAN_DATA", default_value = "han-data")]
    data: PathBuf,
    /// The node's endpoint, if it is not the default of `--data`.
    #[arg(long)]
    endpoint: Option<String>,
    /// A repository whose `rust-toolchain.toml` the sandboxes' Rust must have installed (repeatable).
    #[arg(long = "repo")]
    repos: Vec<PathBuf>,
    /// The executor's directory, if it is not `/var/lib/han-exec` (Linux; checked when `exec.json` is there).
    #[arg(long)]
    exec_dir: Option<PathBuf>,
}

/// One thing that is wrong.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// A stable name of the kind of problem, such as `agent.login.missing`.
    pub id: String,
    /// What it is about: an agent, a space, a path, a process.
    pub subject: String,
    /// One line: why it is a problem.
    pub cause: String,
    /// The command that fixes it.
    pub fix: String,
}

/// What `--json` prints.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Report {
    /// Always [`SCHEMA`].
    pub schema: &'static str,
    /// No finding.
    pub healthy: bool,
    /// The data directory that was checked.
    pub data: String,
    /// The findings, in the order the checks run.
    pub findings: Vec<Finding>,
}

// ───────────────────────── facts ─────────────────────────

/// What the running node says.
#[derive(Clone, Debug, Default)]
pub struct NodeFacts {
    /// The actor the node acts as.
    pub actor: String,
    /// The node's id (its public key).
    pub node_id: String,
    /// `NodeStatus.problems`.
    pub problems: Vec<String>,
    /// One per followed space.
    pub links: Vec<LinkFact>,
    /// The outbox entries that hold a space back.
    pub blocking: Vec<BlockingFact>,
}

/// The link to one space.
#[derive(Clone, Debug)]
pub struct LinkFact {
    pub space: String,
    pub home: String,
    pub link: SpaceLink,
    pub error: Option<String>,
}

/// An outbox entry that holds its space back.
#[derive(Clone, Debug)]
pub struct BlockingFact {
    pub id: u64,
    pub space: String,
    pub state: String,
    pub error: Option<String>,
}

/// A harness record, as the registry has it.
#[derive(Clone, Debug)]
pub enum HarnessFact {
    /// No record.
    Missing,
    /// The record cannot be read.
    Broken(String),
    /// The record names files; whether they exist.
    Recorded {
        node: PathBuf,
        node_ok: bool,
        adapter: PathBuf,
        adapter_ok: bool,
    },
}

/// The device's presence setting.
#[derive(Clone, Debug)]
pub struct PresenceFacts {
    /// `presence.require` covers approvals.
    pub required: bool,
    /// `presence.backend`.
    pub choice: String,
    /// The backend that choice builds on this platform (`none` when there is no check).
    pub effective: String,
    /// Linux: `pkcheck` is installed.
    pub pkcheck: bool,
}

/// The Rust toolchain the sandboxes get.
#[derive(Clone, Debug, Default)]
pub struct ToolchainFacts {
    /// Some agent gets `rust`.
    pub enabled: bool,
    /// `RUSTUP_HOME` the launch would use.
    pub rustup_home: Option<PathBuf>,
    /// The toolchains installed there.
    pub installed: Vec<String>,
    /// `cargo` exists in the bin directory the launch would use.
    pub cargo_ok: bool,
    /// `(repository, channel)` of the repositories given with `--repo`.
    pub pins: Vec<(PathBuf, String)>,
}

/// A `han` process that runs a replaced binary.
#[derive(Clone, Debug)]
pub struct StaleFact {
    pub pid: u32,
    /// `seq`, `node` or `exec`.
    pub role: String,
    pub data: String,
}

/// The executor's GitHub App setup.
#[derive(Clone, Debug)]
pub struct ExecutorFacts {
    pub dir: PathBuf,
    /// `exec.json` names repositories.
    pub has_repos: bool,
    /// The `github` block: `(app id, installation id, key path)`.
    pub github: Option<(u64, u64, PathBuf)>,
    /// The key file exists.
    pub key_exists: bool,
    /// The key file's permission bits (Unix).
    pub key_mode: Option<u32>,
}

/// Everything [`analyze`] looks at.
#[derive(Clone, Debug)]
pub struct Facts {
    pub data: PathBuf,
    pub linux: bool,
    /// `Err(endpoint)` when nothing answers there.
    pub node: Result<NodeFacts, String>,
    /// `Err` is the reason `agents.json` cannot be read.
    pub agents: Result<AgentsFile, String>,
    /// By harness name, for the harnesses the agents use.
    pub harnesses: BTreeMap<String, HarnessFact>,
    /// By actor: whether the agent's login exists, for the agents that use Claude Code.
    pub logins: BTreeMap<String, bool>,
    /// By OS user: whether the user exists (Linux).
    pub os_users: BTreeMap<String, bool>,
    pub presence: Option<Result<PresenceFacts, String>>,
    pub toolchain: ToolchainFacts,
    /// Why MXC cannot give a container a loopback port on this machine (the sandbox self-test's finding), when
    /// it cannot and some agent would use MXC.
    pub mxc_loopback_blocked: Option<String>,
    /// By actor: why `han agent start` would refuse the agent's sandbox, from the same decision the node makes
    /// (`sandbox::admit_for`). An OS-user agent is never here for a backend that does not work in this process:
    /// its host checks where it runs, and a refusal there is in the node's problems.
    pub sandbox_refusals: BTreeMap<String, String>,
    pub stale: Vec<StaleFact>,
    /// `None` when there is no executor on this machine.
    pub executor: Option<ExecutorFacts>,
}

// ───────────────────────── analysis ─────────────────────────

/// A path or a name as one shell word.
fn q(s: impl AsRef<str>) -> String {
    let s = s.as_ref();
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/\\._-:@=,#".contains(c))
    {
        s.to_owned()
    } else {
        format!("\"{}\"", s.replace('"', "\\\""))
    }
}

fn is_claude(harness: &str) -> bool {
    matches!(harness, "claude" | "claude-code")
}

/// `harness/<name>.json` names a claude harness `claude`.
fn record_name(harness: &str) -> &str {
    if is_claude(harness) {
        "claude"
    } else {
        harness
    }
}

fn finding(
    id: &str,
    subject: impl Into<String>,
    cause: impl Into<String>,
    fix: impl Into<String>,
) -> Finding {
    Finding {
        id: id.to_owned(),
        subject: subject.into(),
        cause: cause.into(),
        fix: fix.into(),
    }
}

/// The findings in `facts`.
pub fn analyze(f: &Facts) -> Vec<Finding> {
    let data = q(f.data.to_string_lossy());
    let mut out = Vec::new();
    node_findings(f, &data, &mut out);
    agent_findings(f, &data, &mut out);
    toolchain_findings(f, &data, &mut out);
    sandbox_findings(f, &data, &mut out);
    refusal_findings(f, &data, &mut out);
    if let Some(p) = &f.presence {
        presence_findings(p, &data, f.linux, &mut out);
    }
    if let Some(e) = &f.executor {
        executor_findings(e, &mut out);
    }
    for s in &f.stale {
        out.push(finding(
            "process.stale-binary",
            format!("{} (pid {})", s.role, s.pid),
            format!(
                "the {} process runs a binary that was replaced on disk{}; it keeps the old code until restarted",
                s.role,
                if s.data.is_empty() {
                    String::new()
                } else {
                    format!(" (data {})", s.data)
                }
            ),
            format!("sudo systemctl restart han-{}", s.role),
        ));
    }
    out
}

fn executor_findings(e: &ExecutorFacts, out: &mut Vec<Finding>) {
    let subject = e.dir.to_string_lossy().into_owned();
    let (app, inst, key) = match &e.github {
        Some((a, i, k)) => (
            a.to_string(),
            i.to_string(),
            k.to_string_lossy().into_owned(),
        ),
        None => ("<app-id>".into(), "<installation-id>".into(), String::new()),
    };
    let fix = format!(
        "han executor add <name> --operator <operator> --dir {} --github-app {app} --github-installation {inst} --github-key <path-to-pem>",
        q(&subject)
    );
    let Some(_) = &e.github else {
        if e.has_repos {
            out.push(finding(
                "executor.github.missing",
                subject,
                "exec.json names repositories but has no `github` block, so the executor has no write GitHub App and cannot push or open pull requests",
                fix,
            ));
        }
        return;
    };
    if !e.key_exists {
        out.push(finding(
            "executor.github.key-missing",
            subject,
            format!("the write App's private key {key} does not exist"),
            fix,
        ));
    } else if e.key_mode.is_some_and(|m| m != 0o600) {
        out.push(finding(
            "executor.github.key-mode",
            subject,
            format!(
                "the write App's private key {key} has mode {:03o}, not 600",
                e.key_mode.unwrap_or_default()
            ),
            fix,
        ));
    }
}

/// Reads the executor's directory, when there is one.
fn executor_facts(dir: Option<&Path>) -> Option<ExecutorFacts> {
    let dir = match dir {
        Some(d) => d.to_path_buf(),
        None if cfg!(target_os = "linux") => PathBuf::from(crate::executor::RUNNER_DIR),
        None => return None,
    };
    let cfg: han_exec::ExecConfig =
        serde_json::from_slice(&std::fs::read(dir.join("exec.json")).ok()?).ok()?;
    let github = cfg
        .github
        .map(|g| (g.app_id, g.installation_id, PathBuf::from(g.private_key)));
    let meta = github
        .as_ref()
        .and_then(|(_, _, k)| std::fs::metadata(k).ok());
    #[cfg(unix)]
    let key_mode = {
        use std::os::unix::fs::PermissionsExt;
        meta.as_ref().map(|m| m.permissions().mode() & 0o777)
    };
    #[cfg(not(unix))]
    let key_mode = None;
    Some(ExecutorFacts {
        dir,
        has_repos: !cfg.repos.is_empty(),
        key_exists: meta.is_some(),
        key_mode,
        github,
    })
}

fn node_findings(f: &Facts, data: &str, out: &mut Vec<Finding>) {
    let n = match &f.node {
        Ok(n) => n,
        Err(endpoint) => {
            out.push(finding(
                "node.down",
                endpoint.clone(),
                format!("nothing answers at {endpoint}: the node is not running"),
                format!("han node install-service --actor <your actor> --data {data}"),
            ));
            return;
        }
    };
    for l in &n.links {
        if l.link != SpaceLink::Misconfigured {
            continue;
        }
        let reason = l.error.as_deref().unwrap_or("no reason given");
        let lower = reason.to_lowercase();
        let fix = if lower.contains("does not know")
            || lower.contains("unknown actor")
            || lower.contains("profile")
        {
            format!(
                "han home register {} --data <the home's data directory> --kind human --key-id d1 --public-key {}",
                q(&n.actor),
                q(&n.node_id)
            )
        } else if lower.contains("pin") || lower.contains("key") {
            format!(
                "han node install-service --actor {} --data {data} --home <ticket from `han ticket` at the home>",
                q(&n.actor)
            )
        } else {
            format!(
                "han space follow {} --home {} --data {data}",
                q(&l.space),
                q(&l.home)
            )
        };
        out.push(finding(
            "link.misconfigured",
            l.space.clone(),
            format!(
                "the link to {} does not fit this device's setup: {reason}",
                l.home
            ),
            fix,
        ));
    }
    for b in &n.blocking {
        let reason = b.error.as_deref().unwrap_or("not sent");
        let fix = if b.state == "failed" {
            format!("han outbox retry {} --data {data}", b.id)
        } else {
            format!("han outbox list {} --data {data}", q(&b.space))
        };
        out.push(finding(
            "outbox.blocking",
            format!("{} entry {}", b.space, b.id),
            format!(
                "outbox entry {} ({}) holds back every entry behind it in {}: {reason}",
                b.id, b.state, b.space
            ),
            fix,
        ));
    }
    // What the node lists that the rows above did not already say.
    for p in &n.problems {
        if p.contains("misconfigured:") || p.contains("blocked by outbox entry") {
            continue;
        }
        // said by the sandbox finding, with the fix
        if f.mxc_loopback_blocked.is_some() && p.starts_with("sandbox mxc unavailable") {
            continue;
        }
        let actor = p
            .split_once("profile of ")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .filter(|a| a.starts_with('@'))
            .map(|a| a.trim_end_matches([':', ',', '.']));
        if let Some(actor) = actor {
            let name = actor
                .trim_start_matches('@')
                .split(':')
                .next()
                .unwrap_or(actor);
            out.push(finding(
                "profile.missing",
                actor.to_owned(),
                p.clone(),
                format!(
                    "han agent add {} --operator {} --data {data}",
                    q(name),
                    q(&n.actor)
                ),
            ));
        } else {
            out.push(finding(
                "node.problem",
                n.actor.clone(),
                p.clone(),
                format!("han node status --data {data}"),
            ));
        }
    }
}

fn agent_findings(f: &Facts, data: &str, out: &mut Vec<Finding>) {
    let file = match &f.agents {
        Ok(file) => file,
        Err(e) => {
            let path = q(f.data.join("agents.json").to_string_lossy());
            out.push(finding(
                "registry.agents-json",
                path.clone(),
                format!("agents.json cannot be read: {e}"),
                format!(
                    "mv {path} {path}.bad   # then `han agent set <agent> …` per agent writes a new one"
                ),
            ));
            return;
        }
    };
    let mut used: Vec<&String> = Vec::new();
    for (actor, a) in &file.agents {
        let set = |what: &str| format!("han agent set {} {what} --data {data}", q(actor));
        if let Some(u) = &a.os_user {
            if !valid_os_user(u) {
                out.push(finding(
                    "registry.os-user",
                    actor.clone(),
                    format!("{u:?} is not an OS user name"),
                    set("--os-user han-agent1"),
                ));
            } else if f.os_users.get(u) == Some(&false) {
                let n = u
                    .strip_prefix("han-agent")
                    .and_then(|n| n.parse::<u32>().ok());
                out.push(finding(
                    "registry.os-user-missing",
                    actor.clone(),
                    format!("{actor} runs as OS user {u}, which does not exist on this machine"),
                    match n {
                        Some(n) => format!("sudo han runner setup --agents {n}"),
                        None => format!(
                            "sudo useradd --system --create-home --home-dir /var/lib/han-agents/{u} --shell /usr/sbin/nologin {u}"
                        ),
                    },
                ));
            }
        }
        if let Some(s) = &a.sandbox
            && !han_registry::agents::SANDBOXES.contains(&s.as_str())
        {
            out.push(finding(
                "registry.sandbox",
                actor.clone(),
                format!("unknown sandbox {s:?}: auto, mxc, bwrap or none"),
                set("--sandbox auto"),
            ));
        }
        if let Some(bad) = a
            .toolchains
            .iter()
            .flatten()
            .find(|t| !han_registry::agents::TOOLCHAINS.contains(&t.as_str()))
        {
            out.push(finding(
                "registry.toolchain",
                actor.clone(),
                format!("unknown toolchain {bad:?}: rust"),
                set("--toolchains rust"),
            ));
        }
        if let Some(h) = &a.harness {
            if !used.contains(&h) {
                used.push(h);
            }
            if is_claude(h) && f.logins.get(actor) == Some(&false) {
                let dir = file.login_dir(&f.data, actor);
                out.push(finding(
                    "agent.login.missing",
                    actor.clone(),
                    format!("{actor} has no Claude Code login in {}", dir.display()),
                    format!("han agent login {} --data {data}", q(actor)),
                ));
            }
        }
    }
    for h in used {
        let name = record_name(h);
        let builtin = matches!(name, "claude" | "pi");
        let fix = format!("han agent setup-harness {name} --data {data}");
        match f.harnesses.get(h) {
            Some(HarnessFact::Missing) if builtin => out.push(finding(
                "harness.missing",
                h.clone(),
                format!("the {name} harness is not set up on this node"),
                fix,
            )),
            Some(HarnessFact::Broken(e)) => out.push(finding(
                "harness.broken",
                h.clone(),
                format!("the record of the {name} harness cannot be read: {e}"),
                fix,
            )),
            Some(HarnessFact::Recorded {
                node,
                node_ok,
                adapter,
                adapter_ok,
            }) if !node_ok || !adapter_ok => {
                let gone = if *node_ok { adapter } else { node };
                out.push(finding(
                    "harness.files-missing",
                    h.clone(),
                    format!(
                        "the {name} harness record names {}, which is gone",
                        gone.display()
                    ),
                    fix,
                ));
            }
            _ => {}
        }
    }
}

/// MXC refuses a container its own loopback port on some Windows builds; an agent set to MXC then refuses to
/// start, and one set to `auto` needs `--allow-advisory`.
fn sandbox_findings(f: &Facts, data: &str, out: &mut Vec<Finding>) {
    let Some(why) = &f.mxc_loopback_blocked else {
        return;
    };
    let affected: Vec<String> = match &f.agents {
        Ok(file) => file
            .agents
            .keys()
            .filter(|a| matches!(file.sandbox_of(a).as_str(), "mxc" | "auto"))
            .cloned()
            .collect(),
        Err(_) => Vec::new(),
    };
    let who = if affected.is_empty() {
        "no agent is set to it yet".to_owned()
    } else {
        format!("these agents will not start: {}", affected.join(", "))
    };
    out.push(finding(
        "sandbox.mxc.loopback",
        "mxc",
        format!("{why}; {who}"),
        format!(
            "{}   # e.g. han agent set <agent> --sandbox bwrap --data {data}",
            han_host::sandbox::LOOPBACK_FIX
        ),
    ));
}

/// An agent whose sandbox the node would refuse at `han agent start`.
fn refusal_findings(f: &Facts, data: &str, out: &mut Vec<Finding>) {
    for (actor, why) in &f.sandbox_refusals {
        out.push(finding(
            "sandbox.unavailable",
            actor.as_str(),
            format!("`han agent start` refuses this agent: {why}"),
            format!(
                "install what the sandbox needs here, or `han agent set {actor} --sandbox <auto|bwrap|mxc|none> --data {data}` (`--allow-advisory` runs with no confinement; not recommended)"
            ),
        ));
    }
}

fn toolchain_findings(f: &Facts, data: &str, out: &mut Vec<Finding>) {
    let t = &f.toolchain;
    if !t.enabled {
        return;
    }
    let setup = |repos: &[&PathBuf]| {
        let repos: String = repos
            .iter()
            .map(|r| format!(" --repo {}", q(r.to_string_lossy())))
            .collect();
        format!("sudo han runner setup --toolchain rust{repos} --data {data}")
    };
    let Some(home) = &t.rustup_home else {
        out.push(finding(
            "toolchain.rust.location",
            "rust",
            "the rust toolchain is enabled but nothing says where rustup is",
            format!("sudo han runner setup --toolchain rust --data {data}"),
        ));
        return;
    };
    if t.installed.is_empty() || !t.cargo_ok {
        out.push(finding(
            "toolchain.rust.missing",
            home.to_string_lossy(),
            format!(
                "the rust toolchain is enabled but {} has no installed toolchain or no cargo",
                home.display()
            ),
            if f.linux {
                setup(&t.pins.iter().map(|(r, _)| r).collect::<Vec<_>>())
            } else {
                "rustup toolchain install stable --profile minimal -c clippy -c rustfmt".to_owned()
            },
        ));
        return;
    }
    for (repo, channel) in &t.pins {
        if !crate::rusttool::channel_installed(channel, &t.installed) {
            out.push(finding(
                "toolchain.rust.pin-missing",
                repo.to_string_lossy(),
                format!(
                    "{} pins Rust {channel} (rust-toolchain.toml) but {} has: {}",
                    repo.display(),
                    home.display(),
                    t.installed.join(", ")
                ),
                if f.linux {
                    setup(&[repo])
                } else {
                    format!(
                        "rustup toolchain install {channel} --profile minimal -c clippy -c rustfmt"
                    )
                },
            ));
        }
    }
}

fn presence_findings(
    p: &Result<PresenceFacts, String>,
    data: &str,
    linux: bool,
    out: &mut Vec<Finding>,
) {
    let p = match p {
        Ok(p) => p,
        Err(e) => {
            out.push(finding(
                "presence.settings",
                "presence",
                format!("the presence settings cannot be read: {e}"),
                format!("han node presence --backend auto --data {data}"),
            ));
            return;
        }
    };
    if !p.required {
        return;
    }
    if p.effective == "none" {
        out.push(finding(
            "presence.backend",
            "presence",
            format!(
                "approvals need a person present but the backend {:?} has no check on this device",
                p.choice
            ),
            format!("han node presence --backend auto --data {data}"),
        ));
    } else if linux && p.effective == "polkit" && !p.pkcheck {
        out.push(finding(
            "presence.polkit",
            "presence",
            "approvals need a person present, through polkit, but `pkcheck` is not installed",
            "sudo apt-get install polkitd   # or your distribution's polkit package",
        ));
    }
}

// ───────────────────────── gathering ─────────────────────────

async fn rows(client: &RpcClient, query: Query) -> Vec<Row> {
    match client.view(query).next().await {
        Some(ViewEvent::Snapshot { rows, .. }) => rows,
        _ => Vec::new(),
    }
}

async fn node_facts(endpoint: &str) -> Result<NodeFacts, String> {
    let Ok(client) = RpcClient::connect(endpoint).await else {
        return Err(endpoint.to_owned());
    };
    let mut n = NodeFacts::default();
    for row in rows(
        &client,
        Query::NodeStatus {
            window: Window::default(),
        },
    )
    .await
    {
        if let Row::NodeStatus(s) = row {
            n.actor = s.actor;
            n.node_id = s.node_id;
            n.problems = s.problems;
        }
    }
    for row in rows(
        &client,
        Query::Spaces {
            window: Window::default(),
        },
    )
    .await
    {
        if let Row::SpaceStatus(s) = row {
            n.links.push(LinkFact {
                space: s.space,
                home: s.home,
                link: s.link,
                error: s.error,
            });
        }
    }
    for row in rows(
        &client,
        Query::Outbox {
            space: None,
            window: Window::default(),
        },
    )
    .await
    {
        if let Row::Outbox(o) = row
            && o.blocking
        {
            n.blocking.push(BlockingFact {
                id: o.outbox_id,
                space: o.space,
                state: o.state,
                error: o.error,
            });
        }
    }
    Ok(n)
}

fn user_exists(user: &str) -> bool {
    std::process::Command::new("id")
        .arg(user)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
}

fn presence_facts(data: &Path) -> Option<Result<PresenceFacts, String>> {
    let path = data.join("node.db");
    if !path.is_file() {
        return None;
    }
    let read = || -> Result<PresenceFacts, String> {
        let db = han_node::db::NodeDb::open(&path).map_err(|e| e.to_string())?;
        let (require, backend) = han_node::presence::settings(&db).map_err(|e| e.to_string())?;
        Ok(PresenceFacts {
            required: require.covers(han_node::presence::EFFECT_APPROVE),
            choice: backend.to_string(),
            effective: backend.build().name().to_owned(),
            pkcheck: on_path("pkcheck"),
        })
    };
    Some(read())
}

/// The toolchain as a launch would find it: the file's overrides, then the environment, then the user's home.
fn toolchain_facts(data: &Path, file: &AgentsFile, repos: &[PathBuf]) -> ToolchainFacts {
    let on = |tools: &[String]| tools.iter().any(|t| t == "rust");
    let enabled = on(&file.toolchains)
        || file
            .agents
            .keys()
            .any(|actor| on(&file.toolchains_of(actor)));
    if !enabled {
        return ToolchainFacts::default();
    }
    let spec = file.toolchain_spec(data, "");
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let rustup_home = spec
        .path("rust.rustupHome")
        .or_else(|| std::env::var_os("RUSTUP_HOME").map(PathBuf::from))
        .or_else(|| home.as_ref().map(|h| h.join(".rustup")));
    let cargo_bin = spec
        .path("rust.cargoBin")
        .or_else(|| std::env::var_os("CARGO_HOME").map(|c| PathBuf::from(c).join("bin")))
        .or_else(|| home.as_ref().map(|h| h.join(".cargo").join("bin")));
    let cargo = if cfg!(windows) { "cargo.exe" } else { "cargo" };
    ToolchainFacts {
        enabled,
        installed: rustup_home
            .as_deref()
            .map(crate::rusttool::installed)
            .unwrap_or_default(),
        rustup_home,
        cargo_ok: cargo_bin.is_some_and(|b| b.join(cargo).is_file()),
        pins: crate::rusttool::pins(repos),
    }
}

/// `han` processes of the roles that run for long and whose binary was replaced (Linux: `/proc/<pid>/exe`
/// reads `… (deleted)`).
#[cfg(target_os = "linux")]
fn stale_processes() -> Vec<StaleFact> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in dir.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(cmd) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let args: Vec<String> = cmd
            .split(|b| *b == 0)
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        let is_han = args
            .first()
            .and_then(|a| Path::new(a).file_name())
            .is_some_and(|n| n == "han");
        let role = args.get(1).map(String::as_str).unwrap_or_default();
        if !is_han || !matches!(role, "seq" | "node" | "exec") {
            continue;
        }
        let stale = std::fs::read_link(entry.path().join("exe"))
            .is_ok_and(|e| e.to_string_lossy().ends_with(" (deleted)"));
        if stale {
            let data = args
                .iter()
                .position(|a| a == "--data")
                .and_then(|i| args.get(i + 1))
                .cloned()
                .unwrap_or_default();
            out.push(StaleFact {
                pid,
                role: role.to_owned(),
                data,
            });
        }
    }
    out
}

#[cfg(not(target_os = "linux"))]
fn stale_processes() -> Vec<StaleFact> {
    Vec::new()
}

/// Reads the machine.
pub async fn gather(
    data: &Path,
    endpoint: &str,
    repos: &[PathBuf],
    exec_dir: Option<&Path>,
) -> Facts {
    let linux = cfg!(target_os = "linux");
    let node = node_facts(endpoint).await;
    let agents = AgentsFile::load(data);
    let mut harnesses = BTreeMap::new();
    let mut logins = BTreeMap::new();
    let mut os_users = BTreeMap::new();
    let mut toolchain = ToolchainFacts::default();
    let mut mxc_loopback_blocked = None;
    let mut sandbox_refusals = BTreeMap::new();
    if let Ok(file) = &agents {
        if cfg!(windows)
            && file
                .agents
                .keys()
                .any(|a| matches!(file.sandbox_of(a).as_str(), "mxc" | "auto"))
            && let Ok(shim) = han_host::sandbox::ShimCommand::of_current_exe()
        {
            mxc_loopback_blocked = han_host::sandbox::probe(&shim)
                .into_iter()
                .find(|b| b.loopback_blocked)
                .map(|b| b.detail);
        }
        if let Ok(shim) = han_host::sandbox::ShimCommand::of_current_exe() {
            for (actor, a) in &file.agents {
                // The node's own decision: an OS-user agent's host checks in its own unit, not here.
                let own_unit = linux && a.os_user.is_some();
                match han_host::sandbox::admit_for(
                    &file.sandbox_of(actor),
                    a.allow_advisory,
                    own_unit,
                    &shim,
                ) {
                    // said by `sandbox.mxc.loopback`, with its own fix
                    Err(han_host::sandbox::SandboxError::LoopbackBlocked(_)) | Ok(_) => {}
                    // a name no launch could use is `registry.sandbox`
                    Err(han_host::sandbox::SandboxError::Invalid(_)) => {}
                    Err(e) => {
                        sandbox_refusals.insert(actor.clone(), e.to_string());
                    }
                }
            }
        }
        for (actor, a) in &file.agents {
            if let Some(u) = &a.os_user
                && linux
                && valid_os_user(u)
            {
                os_users.insert(u.clone(), user_exists(u));
            }
            let Some(h) = &a.harness else { continue };
            if is_claude(h) {
                logins.insert(
                    actor.clone(),
                    han_host::driver::login_present(&file.login_dir(data, actor)),
                );
            }
            harnesses.entry(h.clone()).or_insert_with(|| {
                match HarnessInstall::load(data, record_name(h)) {
                    Ok(None) => HarnessFact::Missing,
                    Err(e) => HarnessFact::Broken(e),
                    Ok(Some(r)) => HarnessFact::Recorded {
                        node_ok: r.node.exists(),
                        adapter_ok: r.adapter_dir.exists(),
                        node: r.node,
                        adapter: r.adapter_dir,
                    },
                }
            });
        }
        toolchain = toolchain_facts(data, file, repos);
    }
    Facts {
        data: data.to_path_buf(),
        linux,
        node,
        agents,
        harnesses,
        logins,
        os_users,
        presence: presence_facts(data),
        toolchain,
        mxc_loopback_blocked,
        sandbox_refusals,
        stale: stale_processes(),
        executor: executor_facts(exec_dir),
    }
}

// ───────────────────────── the command ─────────────────────────

/// The report of `findings`.
pub fn report(data: &Path, findings: Vec<Finding>) -> Report {
    Report {
        schema: SCHEMA,
        healthy: findings.is_empty(),
        data: data.to_string_lossy().into_owned(),
        findings,
    }
}

/// Text for a person: one line of cause, one of fix, per finding.
pub fn render(r: &Report) -> String {
    if r.healthy {
        return "healthy: no problems found".to_owned();
    }
    let mut s = String::new();
    for f in &r.findings {
        s.push_str(&format!(
            "problem: {} [{}]\n  cause: {}\n  fix:   {}\n",
            f.subject, f.id, f.cause, f.fix
        ));
    }
    s.push_str(&format!("{} problem(s)", r.findings.len()));
    s
}

pub async fn run(args: DoctorArgs) -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = args
        .endpoint
        .clone()
        .unwrap_or_else(|| default_endpoint(&args.data));
    let facts = gather(&args.data, &endpoint, &args.repos, args.exec_dir.as_deref()).await;
    let r = report(&args.data, analyze(&facts));
    if args.json {
        println!("{}", serde_json::to_string_pretty(&r)?);
    } else {
        println!("{}", render(&r));
    }
    if !r.healthy {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy() -> Facts {
        Facts {
            data: PathBuf::from("/d"),
            linux: true,
            node: Ok(NodeFacts {
                actor: "@lars:han.example".into(),
                node_id: "NODEKEY".into(),
                ..NodeFacts::default()
            }),
            agents: Ok(AgentsFile::default()),
            harnesses: BTreeMap::new(),
            logins: BTreeMap::new(),
            os_users: BTreeMap::new(),
            presence: None,
            toolchain: ToolchainFacts::default(),
            mxc_loopback_blocked: None,
            sandbox_refusals: BTreeMap::new(),
            stale: Vec::new(),
            executor: None,
        }
    }

    fn agents(json: &str) -> Result<AgentsFile, String> {
        serde_json::from_str(json).map_err(|e| e.to_string())
    }

    fn ids(f: &Facts) -> Vec<String> {
        analyze(f).into_iter().map(|f| f.id).collect()
    }

    fn only(f: &Facts) -> Finding {
        let mut all = analyze(f);
        assert_eq!(all.len(), 1, "{all:#?}");
        all.remove(0)
    }

    #[test]
    fn a_healthy_machine_has_no_findings_and_says_so() {
        let f = healthy();
        assert!(analyze(&f).is_empty());
        let r = report(&f.data, analyze(&f));
        assert!(r.healthy);
        assert!(render(&r).starts_with("healthy"));
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["schema"], "han-doctor/1");
        assert_eq!(j["healthy"], true);
        assert_eq!(j["findings"], serde_json::json!([]));
    }

    #[test]
    fn a_node_that_is_down_is_a_finding_with_the_command_to_start_it() {
        let mut f = healthy();
        f.node = Err("/run/han.sock".into());
        let x = only(&f);
        assert_eq!(x.id, "node.down");
        assert!(
            x.fix.starts_with("han node install-service --actor"),
            "{}",
            x.fix
        );
        assert!(x.fix.contains("--data /d"));
    }

    #[test]
    fn a_misconfigured_link_names_the_reason_and_the_registration_to_make() {
        let mut f = healthy();
        let n = f.node.as_mut().unwrap();
        n.links.push(LinkFact {
            space: "s1:han.example".into(),
            home: "han.example".into(),
            link: SpaceLink::Misconfigured,
            error: Some("the home does not know @lars:han.example".into()),
        });
        // a live link is not a finding
        n.links.push(LinkFact {
            space: "s2:han.example".into(),
            home: "han.example".into(),
            link: SpaceLink::Live,
            error: None,
        });
        let x = only(&f);
        assert_eq!(x.id, "link.misconfigured");
        assert!(x.cause.contains("does not know @lars"), "{}", x.cause);
        assert!(
            x.fix.starts_with("han home register @lars:han.example"),
            "{}",
            x.fix
        );
        assert!(x.fix.contains("--public-key NODEKEY"));
    }

    #[test]
    fn a_blocking_outbox_entry_is_retried_when_it_failed_and_listed_while_it_is_pending() {
        let mut f = healthy();
        let n = f.node.as_mut().unwrap();
        n.blocking.push(BlockingFact {
            id: 7,
            space: "s1:x".into(),
            state: "failed".into(),
            error: Some("denied".into()),
        });
        n.blocking.push(BlockingFact {
            id: 9,
            space: "s2:x".into(),
            state: "pending".into(),
            error: None,
        });
        // the sentences the node itself lists for these are not said twice
        n.problems
            .push("space s1:x blocked by outbox entry 7: denied".into());
        let all = analyze(&f);
        assert_eq!(all.len(), 2, "{all:#?}");
        assert_eq!(all[0].fix, "han outbox retry 7 --data /d");
        assert_eq!(all[1].fix, "han outbox list s2:x --data /d");
    }

    #[test]
    fn an_agent_start_would_refuse_is_a_finding_and_an_os_user_agent_is_left_to_its_host() {
        let mut f = healthy();
        f.sandbox_refusals.insert(
            "@a:x".into(),
            "sandbox backend unavailable: Bwrap: no".into(),
        );
        let all = analyze(&f);
        assert_eq!(all.len(), 1, "{all:#?}");
        assert_eq!(all[0].id, "sandbox.unavailable");
        assert!(all[0].cause.contains("Bwrap: no"), "{}", all[0].cause);
        // a refusal the OS user's own host left is the node's problem, said once by the generic row
        f.sandbox_refusals.clear();
        f.node.as_mut().unwrap().problems.push(
            "host-@b:x refused to start (SandboxUnavailable, checked by its own host): no".into(),
        );
        let all = analyze(&f);
        assert_eq!(all.len(), 1, "{all:#?}");
        assert!(
            all[0].cause.contains("refused to start"),
            "{}",
            all[0].cause
        );
    }

    #[test]
    fn a_blocked_mxc_loopback_is_one_finding_with_the_fix_and_the_agents_it_stops() {
        let mut f = healthy();
        f.agents =
            agents(r#"{"agents":{"@a:x":{"sandbox":"mxc"},"@b:x":{"sandbox":"none"},"@c:x":{}}}"#);
        f.mxc_loopback_blocked = Some("MXC blocks the in-sandbox loopback port (10013)".into());
        f.node
            .as_mut()
            .unwrap()
            .problems
            .push("sandbox mxc unavailable: x; fix: y".into());
        let all = analyze(&f);
        assert_eq!(all.len(), 1, "{all:#?}");
        assert_eq!(all[0].id, "sandbox.mxc.loopback");
        assert!(all[0].cause.contains("10013"), "{}", all[0].cause);
        assert!(all[0].cause.contains("@a:x, @c:x"), "{}", all[0].cause);
        assert!(!all[0].cause.contains("@b:x"), "{}", all[0].cause);
        assert!(all[0].fix.starts_with("use WSL2+bubblewrap when available, or run the agent on the runner, or `--allow-advisory` (not recommended)"));
    }

    #[test]
    fn a_node_problem_nothing_else_covers_is_passed_on() {
        let mut f = healthy();
        let n = f.node.as_mut().unwrap();
        n.problems
            .push("profile of @coder:han.example has not arrived".into());
        n.problems.push("something else".into());
        let all = analyze(&f);
        assert_eq!(all[0].id, "profile.missing");
        assert_eq!(all[0].subject, "@coder:han.example");
        assert_eq!(
            all[0].fix,
            "han agent add coder --operator @lars:han.example --data /d"
        );
        assert_eq!(all[1].id, "node.problem");
        assert_eq!(all[1].fix, "han node status --data /d");
    }

    #[test]
    fn a_missing_harness_or_login_names_the_command_that_makes_it() {
        let mut f = healthy();
        f.agents =
            agents(r#"{"agents":{"@coder:x":{"harness":"claude"},"@pi:x":{"harness":"pi"}}}"#);
        f.harnesses.insert("claude".into(), HarnessFact::Missing);
        f.harnesses.insert("pi".into(), HarnessFact::Missing);
        f.logins.insert("@coder:x".into(), false);
        let all = analyze(&f);
        let got: Vec<(&str, &str)> = all
            .iter()
            .map(|x| (x.id.as_str(), x.fix.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("agent.login.missing", "han agent login @coder:x --data /d"),
                (
                    "harness.missing",
                    "han agent setup-harness claude --data /d"
                ),
                ("harness.missing", "han agent setup-harness pi --data /d"),
            ]
        );
        // with the login and the harness there, only the other harness is said
        f.logins.insert("@coder:x".into(), true);
        f.harnesses.insert(
            "claude".into(),
            HarnessFact::Recorded {
                node: "/n".into(),
                node_ok: true,
                adapter: "/a".into(),
                adapter_ok: true,
            },
        );
        assert_eq!(ids(&f), ["harness.missing"]);
    }

    #[test]
    fn a_harness_record_whose_files_are_gone_is_installed_again() {
        let mut f = healthy();
        f.agents = agents(r#"{"agents":{"@coder:x":{"harness":"pi"}}}"#);
        f.harnesses.insert(
            "pi".into(),
            HarnessFact::Recorded {
                node: "/n".into(),
                node_ok: true,
                adapter: "/gone".into(),
                adapter_ok: false,
            },
        );
        let x = only(&f);
        assert_eq!(x.id, "harness.files-missing");
        assert!(x.cause.contains("/gone"));
        assert_eq!(x.fix, "han agent setup-harness pi --data /d");
    }

    #[test]
    fn registry_values_no_launch_could_use_are_fixed_with_han_agent_set() {
        let mut f = healthy();
        f.agents = agents(
            r#"{"agents":{"@a:x":{"osUser":"Bad User","sandbox":"chroot","toolchains":["go"]},"@b:x":{"osUser":"han-agent3"}}}"#,
        );
        f.os_users.insert("han-agent3".into(), false);
        let all = analyze(&f);
        let got: Vec<(&str, &str)> = all
            .iter()
            .map(|x| (x.id.as_str(), x.fix.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                (
                    "registry.os-user",
                    "han agent set @a:x --os-user han-agent1 --data /d"
                ),
                (
                    "registry.sandbox",
                    "han agent set @a:x --sandbox auto --data /d"
                ),
                (
                    "registry.toolchain",
                    "han agent set @a:x --toolchains rust --data /d"
                ),
                (
                    "registry.os-user-missing",
                    "sudo han runner setup --agents 3"
                ),
            ]
        );
    }

    #[test]
    fn an_unreadable_agents_file_is_one_finding_and_stops_the_agent_checks() {
        let mut f = healthy();
        f.agents = Err("/d/agents.json: expected value".into());
        let x = only(&f);
        assert_eq!(x.id, "registry.agents-json");
        assert!(
            x.fix.starts_with("mv ") && x.fix.contains("agents.json.bad"),
            "{}",
            x.fix
        );
    }

    #[test]
    fn a_pinned_toolchain_that_is_not_installed_is_installed_through_runner_setup() {
        let mut f = healthy();
        f.toolchain = ToolchainFacts {
            enabled: true,
            rustup_home: Some("/opt/rust/rustup".into()),
            installed: vec!["1.97.1-x86_64-unknown-linux-gnu".into()],
            cargo_ok: true,
            pins: vec![
                ("/srv/a".into(), "1.97.1".into()),
                ("/srv/b".into(), "1.90.0".into()),
            ],
        };
        let x = only(&f);
        assert_eq!(x.id, "toolchain.rust.pin-missing");
        assert!(x.cause.contains("1.90.0"));
        assert_eq!(
            x.fix,
            "sudo han runner setup --toolchain rust --repo /srv/b --data /d"
        );
        // an enabled toolchain with nothing installed
        f.toolchain.installed.clear();
        let x = only(&f);
        assert_eq!(x.id, "toolchain.rust.missing");
        assert_eq!(
            x.fix,
            "sudo han runner setup --toolchain rust --repo /srv/a --repo /srv/b --data /d"
        );
        // not enabled: not looked at
        f.toolchain.enabled = false;
        assert!(analyze(&f).is_empty());
    }

    #[test]
    fn approvals_that_need_a_person_need_a_backend_that_can_check() {
        let facts = |required, effective: &str, pkcheck| {
            Some(Ok(PresenceFacts {
                required,
                choice: "auto".into(),
                effective: effective.into(),
                pkcheck,
            }))
        };
        let mut f = healthy();
        f.presence = facts(true, "none", true);
        let x = only(&f);
        assert_eq!(x.id, "presence.backend");
        assert_eq!(x.fix, "han node presence --backend auto --data /d");
        f.presence = facts(true, "polkit", false);
        assert_eq!(only(&f).id, "presence.polkit");
        // nothing required, or a backend that works: nothing to say
        f.presence = facts(false, "none", false);
        assert!(analyze(&f).is_empty());
        f.presence = facts(true, "polkit", true);
        assert!(analyze(&f).is_empty());
        f.presence = Some(Err("node.db: bad".into()));
        assert_eq!(only(&f).id, "presence.settings");
    }

    #[test]
    fn a_replaced_binary_is_restarted_through_its_unit() {
        let mut f = healthy();
        f.stale.push(StaleFact {
            pid: 42,
            role: "seq".into(),
            data: "/srv/home".into(),
        });
        let x = only(&f);
        assert_eq!(x.id, "process.stale-binary");
        assert_eq!(x.fix, "sudo systemctl restart han-seq");
        assert!(x.cause.contains("/srv/home"));
    }

    fn exec_facts() -> ExecutorFacts {
        ExecutorFacts {
            dir: PathBuf::from("/x"),
            has_repos: true,
            github: Some((7, 8, PathBuf::from("/x/github-app.pem"))),
            key_exists: true,
            key_mode: Some(0o600),
        }
    }

    #[test]
    fn an_executor_with_repos_and_no_github_block_is_reported_with_the_flags() {
        let mut f = healthy();
        let mut e = exec_facts();
        e.github = None;
        f.executor = Some(e);
        let x = only(&f);
        assert_eq!(x.id, "executor.github.missing");
        for flag in ["--github-app", "--github-installation", "--github-key"] {
            assert!(x.fix.contains(flag), "{}", x.fix);
        }
        // no repos, nothing to write to
        let mut e = exec_facts();
        e.github = None;
        e.has_repos = false;
        f.executor = Some(e);
        assert!(analyze(&f).is_empty());
    }

    #[test]
    fn a_missing_or_loose_executor_key_is_reported() {
        let mut f = healthy();
        f.executor = Some(exec_facts());
        assert!(analyze(&f).is_empty());
        let mut e = exec_facts();
        e.key_exists = false;
        e.key_mode = None;
        f.executor = Some(e);
        let x = only(&f);
        assert_eq!(x.id, "executor.github.key-missing");
        assert!(x.fix.contains("--github-key") && x.fix.contains("--github-app 7"));
        let mut e = exec_facts();
        e.key_mode = Some(0o644);
        f.executor = Some(e);
        let x = only(&f);
        assert_eq!(x.id, "executor.github.key-mode");
        assert!(x.cause.contains("644"));
    }

    #[test]
    fn the_json_schema_is_stable() {
        let mut f = healthy();
        f.node = Err("e".into());
        let j = serde_json::to_value(report(&f.data, analyze(&f))).unwrap();
        assert_eq!(j["healthy"], false);
        let first = &j["findings"][0];
        let mut keys: Vec<&str> = first
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["cause", "fix", "id", "subject"]);
        let mut top: Vec<&str> = j.as_object().unwrap().keys().map(String::as_str).collect();
        top.sort_unstable();
        assert_eq!(top, ["data", "findings", "healthy", "schema"]);
    }
}

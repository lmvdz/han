//! `han runner setup`: the root-level part of a shared runner (docs/runner.md), Linux only, and the only `han`
//! command that needs root. It makes the OS users, the agents directory, installs the `han-agent@` template
//! unit, the executor and `han-github` units (left disabled), the polkit rule and the node's drop-in. Everything else
//! (`han home`, `han agent add`, `han executor add`) runs without root.
//!
//! `han runner setup --toolchain rust` also provisions the shared Rust toolchain of docs/runner.md 7.2 (`/opt/rust`,
//! read-only to the agents' sandboxes) and records where it is in `agents.json`.
//!
//! Safe to repeat: every step looks at what is there first and the output says what changed.

use std::path::PathBuf;

use clap::{Args, Subcommand};

/// Options of `han runner`.
#[derive(Args)]
pub struct RunnerArgs {
    #[command(subcommand)]
    cmd: RunnerCmd,
}

#[derive(Subcommand)]
enum RunnerCmd {
    /// Prepares this Linux machine to run agents and an executor for a team. Run as root.
    Setup {
        /// The OS user that runs `han node` and `han seq`.
        #[arg(long, default_value = "han")]
        node_user: String,
        /// The OS user that runs the executor.
        #[arg(long, default_value = "han-exec")]
        exec_user: String,
        /// How many agent users to make (`han-agent1` … `han-agentN`, one per agent that runs as its own
        /// user; more can be added later by running this again with a bigger number).
        #[arg(long, default_value_t = 1)]
        agents: u32,
        /// Restart a running `han-seq` or `han-node` unit whose binary has been replaced.
        #[arg(long)]
        restart_stale: bool,
        /// Also provision a shared toolchain for the agents' sandboxes (`rust`): rustup in /opt/rust with the
        /// minimal profile, clippy and rustfmt, every toolchain the repositories pin, readable by all and
        /// writable by root only; `toolchains` and `toolchainPaths` go into `agents.json`.
        #[arg(long, value_parser = ["rust"])]
        toolchain: Option<String>,
        /// A repository whose `rust-toolchain.toml` names a toolchain to install (repeatable).
        #[arg(long = "repo", requires = "toolchain")]
        repos: Vec<PathBuf>,
        /// A Rust toolchain to install, whether or not a repository pins it (repeatable).
        #[arg(long, requires = "toolchain")]
        rust_toolchain: Vec<String>,
        /// The node's data directory, where `agents.json` is.
        #[arg(long, default_value = "/var/lib/han/node", requires = "toolchain")]
        data: PathBuf,
        /// Do only the `--toolchain` step: no users, units or polkit rule (for a machine without systemd, such
        /// as a container).
        #[arg(long, requires = "toolchain")]
        toolchain_only: bool,
        /// Print what would change and change nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

#[cfg(not(target_os = "linux"))]
pub async fn run(args: RunnerArgs) -> Result<(), Box<dyn std::error::Error>> {
    let RunnerCmd::Setup { .. } = args.cmd;
    Err("han runner setup is for Linux runners (systemd); nothing to do on this OS".into())
}

#[cfg(target_os = "linux")]
pub async fn run(args: RunnerArgs) -> Result<(), Box<dyn std::error::Error>> {
    let RunnerCmd::Setup {
        node_user,
        exec_user,
        agents,
        restart_stale,
        toolchain,
        repos,
        rust_toolchain,
        data,
        toolchain_only,
        dry_run,
    } = args.cmd;
    let rust = toolchain.map(|_| linux::Rust {
        repos,
        channels: rust_toolchain,
        data,
        only: toolchain_only,
    });
    linux::setup(
        &node_user,
        &exec_user,
        agents,
        restart_stale,
        rust.as_ref(),
        dry_run,
    )
    .await
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::BTreeMap;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use han_proc::NoWindow as _;
    use han_registry::AgentsFile;

    type Res<T> = Result<T, Box<dyn std::error::Error>>;

    const AGENT_UNIT: &str = include_str!("../../../integrations/runner/han-agent@.service");
    const EXEC_UNIT: &str = include_str!("../../../integrations/runner/han-exec.service");
    const GITHUB_UNIT: &str = include_str!("../../../integrations/runner/han-github.service");
    const POLKIT_RULE: &str = include_str!("../../../integrations/runner/50-han-agent.rules");
    const AGENTS_ROOT: &str = "/var/lib/han-agents";
    const NODE_DATA: &str = "/var/lib/han";
    const EXEC_DIR: &str = "/var/lib/han-exec";
    /// Where `han-github` finds its config and the read-only App's key (docs/runner.md 10).
    const GITHUB_DIR: &str = "/var/lib/han/github";

    /// The shared Rust toolchain asked for.
    pub struct Rust {
        pub repos: Vec<PathBuf>,
        pub channels: Vec<String>,
        pub data: PathBuf,
        /// Nothing but the toolchain.
        pub only: bool,
    }

    const RUST_HOME: &str = "/opt/rust";
    const RUST_COMPONENTS: [&str; 4] = ["-c", "clippy", "-c", "rustfmt"];

    struct Ctx {
        dry: bool,
        changed: bool,
        reload: bool,
    }

    impl Ctx {
        fn say(&self, status: &str, what: &str) {
            println!("  {status:<10} {what}");
        }

        /// Runs a program; in a dry run only says so.
        fn cmd(&mut self, program: &str, args: &[&str], what: &str) -> Res<()> {
            self.cmd_env(program, args, &[], what)
        }

        /// Like [`Ctx::cmd`], with environment variables.
        fn cmd_env(
            &mut self,
            program: &str,
            args: &[&str],
            env: &[(&str, &str)],
            what: &str,
        ) -> Res<()> {
            if self.dry {
                self.say("would:", what);
                return Ok(());
            }
            let out = Command::new(program)
                .no_window()
                .args(args)
                .envs(env.iter().copied())
                .output()?;
            if !out.status.success() {
                return Err(format!(
                    "{program} {}: {}",
                    args.join(" "),
                    String::from_utf8_lossy(&out.stderr).trim()
                )
                .into());
            }
            self.changed = true;
            self.say("changed:", what);
            Ok(())
        }

        /// Makes a directory with an owner and a mode.
        fn dir(&mut self, path: &str, owner: Option<&str>, mode: u32) -> Res<()> {
            let have = std::fs::metadata(path).ok();
            let owner_ok = |p: &str| {
                owner.is_none_or(|o| {
                    Command::new("stat")
                        .no_window()
                        .args(["-c", "%U", p])
                        .output()
                        .is_ok_and(|s| String::from_utf8_lossy(&s.stdout).trim() == o)
                })
            };
            if let Some(m) = &have
                && m.permissions().mode() & 0o7777 == mode
                && owner_ok(path)
            {
                self.say("unchanged:", &format!("{path} ({mode:o})"));
                return Ok(());
            }
            let mode_s = format!("{mode:o}");
            if have.is_none() {
                self.cmd("install", &["-d", path], &format!("made {path}"))?;
            }
            // owner first: chown may clear a setgid bit, so the mode comes last
            if let Some(o) = owner
                && !owner_ok(path)
            {
                self.cmd(
                    "chown",
                    &[&format!("{o}:{o}"), path],
                    &format!("{path} is owned by {o}"),
                )?;
            }
            self.cmd(
                "chmod",
                &[&mode_s, path],
                &format!("{path} is mode {mode_s}"),
            )
        }

        /// A directory that `owner` (root) owns and nobody else may write, made if it is missing. A symlink, a
        /// file, or a directory of another user or open to writes is refused: whoever planted it could choose
        /// what root's marker inside it says.
        fn root_dir(&mut self, path: &str, owner: u32) -> Res<()> {
            use std::os::unix::fs::MetadataExt as _;
            match std::fs::symlink_metadata(path) {
                Ok(m) => {
                    if m.file_type().is_symlink()
                        || !m.is_dir()
                        || m.uid() != owner
                        || m.mode() & 0o022 != 0
                    {
                        return Err(format!(
                            "{path} is not a plain directory owned by root that nobody else can write (a symlink, a file, or another user's): remove it and run this again"
                        )
                        .into());
                    }
                    self.say("unchanged:", path);
                    Ok(())
                }
                Err(_) => self.cmd(
                    "install",
                    &["-d", "-o", "root", "-m", "755", path],
                    &format!("made {path}"),
                ),
            }
        }

        /// Writes `dir/name` as `owner`'s alone: refuses a marker that is a symlink, not a plain file, or
        /// another user's, and writes a new file (never through a link) in `dir`, which `root_dir` vouched for,
        /// then renames it over the old one.
        fn root_marker(&mut self, dir: &Path, name: &str, content: &str, owner: u32) -> Res<()> {
            use std::io::Write as _;
            use std::os::unix::fs::MetadataExt as _;
            let path = dir.join(name);
            if let Ok(m) = std::fs::symlink_metadata(&path) {
                if m.file_type().is_symlink() || !m.is_file() || m.uid() != owner {
                    return Err(format!(
                        "{} is not a plain file owned by root (a symlink or another user's): remove it and run this again",
                        path.display()
                    )
                    .into());
                }
                if std::fs::read_to_string(&path).is_ok_and(|c| c == content)
                    && m.mode() & 0o7777 == 0o644
                {
                    self.say("unchanged:", &path.display().to_string());
                    return Ok(());
                }
            }
            if self.dry {
                self.say("would:", &format!("write {}", path.display()));
                return Ok(());
            }
            let tmp = dir.join(format!(".{name}.tmp"));
            let _ = std::fs::remove_file(&tmp);
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o644)
                .open(&tmp)?;
            f.write_all(content.as_bytes())?;
            drop(f);
            std::fs::rename(&tmp, &path)?;
            self.changed = true;
            self.say("changed:", &format!("wrote {}", path.display()));
            Ok(())
        }

        /// Writes a file when its content or mode differs.
        fn file(&mut self, path: &str, content: &str, mode: u32) -> Res<bool> {
            let same = std::fs::read_to_string(path).is_ok_and(|c| c == content)
                && std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o7777 == mode);
            if same {
                self.say("unchanged:", path);
                return Ok(false);
            }
            if self.dry {
                self.say("would:", &format!("write {path}"));
                return Ok(true);
            }
            if let Some(d) = Path::new(path).parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::write(path, content)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
            self.changed = true;
            self.say("changed:", &format!("wrote {path}"));
            Ok(true)
        }
    }

    fn user_exists(user: &str) -> bool {
        Command::new("id")
            .no_window()
            .arg(user)
            .output()
            .is_ok_and(|o| o.status.success())
    }

    fn is_root() -> bool {
        Command::new("id")
            .no_window()
            .arg("-u")
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
    }

    fn in_group(user: &str, group: &str) -> bool {
        Command::new("id")
            .no_window()
            .args(["-nG", user])
            .output()
            .is_ok_and(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .any(|g| g == group)
            })
    }

    pub async fn setup(
        node_user: &str,
        exec_user: &str,
        agents: u32,
        restart_stale: bool,
        rust: Option<&Rust>,
        dry: bool,
    ) -> Res<()> {
        if !han_registry::valid_os_user(node_user) || !han_registry::valid_os_user(exec_user) {
            return Err("an OS user name starts with a lowercase letter or _, then lowercase letters, digits, - and _".into());
        }
        if !dry && !is_root() {
            return Err(
                "han runner setup changes users, units and polkit rules: run it as root (`sudo han runner setup`), or with --dry-run to see what it would do"
                    .into(),
            );
        }
        let mut c = Ctx {
            dry,
            changed: false,
            reload: false,
        };
        if let Some(r) = rust.filter(|r| r.only) {
            rust_toolchain(&mut c, r, node_user)?;
            return finish(&c, false);
        }

        println!("users");
        if user_exists(node_user) {
            c.say("unchanged:", &format!("user {node_user}"));
        } else {
            c.cmd(
                "useradd",
                &[
                    "--create-home",
                    "--shell",
                    "/bin/bash",
                    "--comment",
                    "HAN runner",
                    node_user,
                ],
                &format!("made user {node_user}"),
            )?;
            c.cmd(
                "passwd",
                &["-l", node_user],
                &format!("locked the password of {node_user}"),
            )?;
        }
        c.dir(NODE_DATA, Some(node_user), 0o700)?;
        // Nobody sits at the runner's node to answer a presence prompt, so root says it is headless: no unlock
        // for governing and steering, no approvals. The marker is root's alone (a process of the node's user
        // cannot write it), which is what makes it the authority (SR-108).
        c.root_dir("/etc/han", 0)?;
        c.root_dir(han_node::presence::HEADLESS_DIR, 0)?;
        c.root_marker(
            Path::new(han_node::presence::HEADLESS_DIR),
            &han_node::presence::seal_name(Path::new(&format!("{NODE_DATA}/node"))),
            &format!(
                "# han runner setup: the node with data in {NODE_DATA}/node is headless (no person sits at it).
                 # It asks for no presence check to govern or steer, and it never approves.
                 # Only root can write this file; remove it to make the node a person's again.
"
            ),
            0,
        )?;

        if user_exists(exec_user) {
            c.say("unchanged:", &format!("user {exec_user}"));
        } else {
            c.cmd(
                "useradd",
                &[
                    "--system",
                    "--create-home",
                    "--home-dir",
                    EXEC_DIR,
                    "--shell",
                    "/usr/sbin/nologin",
                    exec_user,
                ],
                &format!("made user {exec_user}"),
            )?;
        }
        for sub in ["", "/keys", "/state"] {
            c.dir(&format!("{EXEC_DIR}{sub}"), Some(exec_user), 0o700)?;
        }

        // han-github reaches the node through its owner-only socket, so it runs as the node's user; its config and
        // the read-only App's key sit in a directory only that user can read.
        c.dir(GITHUB_DIR, Some(node_user), 0o700)?;

        println!("agents");
        c.dir(AGENTS_ROOT, None, 0o755)?;
        let mut group_added = false;
        for n in 1..=agents {
            let user = format!("han-agent{n}");
            let home = format!("{AGENTS_ROOT}/{user}");
            if user_exists(&user) {
                c.say("unchanged:", &format!("user {user}"));
            } else {
                c.cmd(
                    "useradd",
                    &[
                        "--system",
                        "--create-home",
                        "--home-dir",
                        &home,
                        "--shell",
                        "/usr/sbin/nologin",
                        &user,
                    ],
                    &format!("made user {user}"),
                )?;
                c.cmd(
                    "passwd",
                    &["-l", &user],
                    &format!("locked the password of {user}"),
                )?;
            }
            // the node's user shares only this agent's group; the directory is setgid
            if user_exists(node_user) && in_group(node_user, &user) {
                c.say("unchanged:", &format!("{node_user} is in group {user}"));
            } else {
                c.cmd(
                    "usermod",
                    &["-aG", &user, node_user],
                    &format!("{node_user} joined group {user}"),
                )?;
                group_added = true;
            }
            c.dir(&home, Some(&user), 0o2770)?;
        }

        println!("units");
        c.reload |= c.file("/etc/systemd/system/han-agent@.service", AGENT_UNIT, 0o644)?;
        c.reload |= c.file("/etc/systemd/system/han-exec.service", EXEC_UNIT, 0o644)?;
        c.reload |= c.file("/etc/systemd/system/han-github.service", GITHUB_UNIT, 0o644)?;
        c.file(
            "/etc/polkit-1/rules.d/50-han-agent.rules",
            POLKIT_RULE,
            0o644,
        )?;
        let dropin = format!(
            "# han runner setup: the node writes each OS-user agent's spawn file (and, for a new agent, its key)\n\
             # in {AGENTS_ROOT}/<user>, shared with that user's group; systemctl reaches the system bus over AF_UNIX.\n\
             [Service]\nUMask=0007\nReadWritePaths=\nReadWritePaths={NODE_DATA}/node {AGENTS_ROOT}\n"
        );
        c.reload |= c.file(
            "/etc/systemd/system/han-node.service.d/10-agents.conf",
            &dropin,
            0o644,
        )?;
        if c.reload && !dry {
            c.cmd("systemctl", &["daemon-reload"], "systemctl daemon-reload")?;
        }
        let enabled = Command::new("systemctl")
            .no_window()
            .args(["is-enabled", "han-exec"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default();
        c.say(
            "note:",
            &format!(
                "han-exec.service is {}; enable it when its key, profile and config are ready: systemctl enable --now han-exec",
                if enabled.is_empty() { "installed, disabled" } else { &enabled }
            ),
        );

        let configured = Path::new(GITHUB_DIR).join("han-github.toml").exists();
        c.say(
            "note:",
            &format!(
                "han-github.service is installed, disabled, and {}; configure it as in docs/runner.md 10, then: systemctl enable --now han-github",
                if configured {
                    "has a config"
                } else {
                    "has no config yet"
                }
            ),
        );

        if let Some(r) = rust {
            rust_toolchain(&mut c, r, node_user)?;
        }

        println!("running services");
        let mut warnings = false;
        for (pid, data) in crate::homecmd::stale_homes() {
            warnings = true;
            c.say(
                "stale:",
                &format!(
                    "the home (pid {pid}, data {data}) runs a binary that was replaced; it keeps the old code until restarted"
                ),
            );
        }
        let mut units: Vec<String> = ["han-seq", "han-node", "han-exec", "han-github"]
            .map(str::to_owned)
            .into();
        units.extend(active_agent_units());
        for unit in &units {
            let unit = unit.as_str();
            let agent = unit.starts_with("han-agent@");
            let active = Command::new("systemctl")
                .no_window()
                .args(["is-active", unit])
                .output()
                .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "active");
            if !active {
                continue;
            }
            let stale = unit_binary_replaced(unit);
            let needs = stale || (unit == "han-node" && group_added);
            // An agent unit's cgroup holds its sandboxed agent while a session runs: restarting then would
            // kill it mid-session, so such a unit is reported, and restarted once idle.
            let busy = agent && needs && unit_is_busy(unit);
            if !needs {
                c.say("unchanged:", &format!("{unit} is running current code"));
            } else if busy {
                warnings = true;
                c.say(
                    "busy:",
                    &format!(
                        "{unit} runs a replaced binary and has a session running; not restarted (restart it when the session ends: systemctl restart {unit})"
                    ),
                );
            } else if restart_stale && !dry {
                let since = std::time::SystemTime::now();
                c.cmd(
                    "systemctl",
                    &["restart", unit],
                    &format!(
                        "restarting {unit} ({})",
                        if stale {
                            "replaced binary"
                        } else {
                            "new group"
                        }
                    ),
                )?;
                // systemd has started the unit, which is not the same as it serving: the next command of a
                // script would find no socket yet.
                wait_ready(unit, since).await?;
                c.say("restarted:", &format!("{unit} (ready)"));
            } else {
                warnings = true;
                c.say(
                    "restart:",
                    &format!(
                        "{unit} needs a restart ({}): systemctl restart {unit}   (or run this again with --restart-stale)",
                        if stale { "its binary was replaced" } else { "it must pick up its new group" }
                    ),
                );
            }
        }
        finish(&c, warnings)
    }

    /// Waits until the restarted `unit` serves: han-seq on its admin socket, han-node on a status call.
    async fn wait_ready(unit: &str, since: std::time::SystemTime) -> Res<()> {
        let timeout = crate::seqadmin::READY_TIMEOUT;
        if let Some(name) = unit.strip_prefix("han-agent@") {
            // The host rewrites its admission file when it starts; a file older than the restart is the last run's.
            let file = han_registry::unit_admission_file(&Path::new(AGENTS_ROOT).join(name));
            return Ok(crate::seqadmin::wait_until(
                &format!(
                    "{unit}: its host did not write {} after the restart, so it",
                    file.display()
                ),
                timeout,
                || admitted_since(&file, since),
            )
            .await?);
        }
        if unit == "han-exec" || unit == "han-github" {
            return Ok(crate::seqadmin::wait_until(
                &format!("{unit}: systemd does not report it active, so it"),
                timeout,
                || async {
                    if unit_active(unit) {
                        Ok(())
                    } else {
                        Err("not active")
                    }
                },
            )
            .await?);
        }
        if unit == "han-seq" {
            let data = Path::new(NODE_DATA).join("seq");
            return Ok(crate::seqadmin::wait_ready(&data, timeout).await?);
        }
        let endpoint = han_node::rpc::default_endpoint(&Path::new(NODE_DATA).join("node"));
        Ok(crate::seqadmin::wait_until(
            &format!("{unit}: nothing answered a status call at {endpoint}, so it"),
            timeout,
            || node_answers(&endpoint),
        )
        .await?)
    }

    /// Whether the host's admission file at `file` was written after `since` and admits the agent.
    async fn admitted_since(file: &Path, since: std::time::SystemTime) -> Result<(), String> {
        let fresh = std::fs::metadata(file)
            .and_then(|m| m.modified())
            .map_err(|e| e.to_string())?
            >= since;
        if !fresh {
            return Err("the admission file is from before the restart".into());
        }
        let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
        if text.contains("\"Admitted\"") {
            Ok(())
        } else {
            Err(format!("the host did not admit the agent: {}", text.trim()))
        }
    }

    fn unit_active(unit: &str) -> bool {
        Command::new("systemctl")
            .no_window()
            .args(["is-active", unit])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "active")
    }

    /// The `han-agent@<user>.service` units that are active.
    fn active_agent_units() -> Vec<String> {
        let out = Command::new("systemctl")
            .no_window()
            .args([
                "list-units",
                "han-agent@*",
                "--type=service",
                "--state=active",
                "--plain",
                "--no-legend",
            ])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        parse_agent_units(&out)
    }

    /// The unit names in `systemctl list-units --plain --no-legend` output that are agent units.
    fn parse_agent_units(listing: &str) -> Vec<String> {
        listing
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .filter(|u| u.starts_with("han-agent@") && u.ends_with(".service"))
            .map(str::to_owned)
            .collect()
    }

    /// Whether the unit has a session running: processes in its cgroup besides its main process (the host).
    fn unit_is_busy(unit: &str) -> bool {
        let show = |prop: &str| {
            Command::new("systemctl")
                .no_window()
                .args(["show", "-p", prop, "--value", unit])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .unwrap_or_default()
        };
        let (main, cgroup) = (show("MainPID"), show("ControlGroup"));
        let procs = std::fs::read_to_string(format!("/sys/fs/cgroup{cgroup}/cgroup.procs"))
            .unwrap_or_default();
        // Cannot tell: treat as busy rather than kill a session.
        cgroup.is_empty() || has_other_processes(&procs, &main)
    }

    /// Whether `cgroup_procs` lists a pid other than `main`.
    fn has_other_processes(cgroup_procs: &str, main: &str) -> bool {
        cgroup_procs
            .lines()
            .map(str::trim)
            .any(|p| !p.is_empty() && p != main)
    }

    /// Whether the node at `endpoint` answers a status call.
    async fn node_answers(endpoint: &str) -> Result<(), String> {
        use han_node::NodeApi as _;
        use han_node::api::{Query, Row, ViewEvent, Window};
        let client = han_node::rpc::RpcClient::connect(endpoint)
            .await
            .map_err(|e| e.to_string())?;
        let mut view = client.view(Query::NodeStatus {
            window: Window::default(),
        });
        let first = tokio::time::timeout(std::time::Duration::from_secs(5), view.next())
            .await
            .map_err(|_| "no answer to the status call".to_owned())?;
        match first {
            Some(ViewEvent::Snapshot { rows, .. })
                if rows.iter().any(|r| matches!(r, Row::NodeStatus(_))) =>
            {
                Ok(())
            }
            _ => Err("the status call returned no node status".to_owned()),
        }
    }

    /// The last line of the output.
    fn finish(c: &Ctx, warnings: bool) -> Res<()> {
        let pending = if warnings {
            ", with restarts pending"
        } else {
            ""
        };
        if c.dry {
            println!("dry run: nothing changed");
        } else if c.changed {
            println!("done: changes made{pending}");
        } else {
            println!("done: nothing to change{pending}");
        }
        Ok(())
    }

    /// Whether the main process of `unit` runs a binary that has been replaced on disk.
    fn unit_binary_replaced(unit: &str) -> bool {
        let Ok(out) = Command::new("systemctl")
            .no_window()
            .args(["show", "-p", "MainPID", "--value", unit])
            .output()
        else {
            return false;
        };
        let pid = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if pid.is_empty() || pid == "0" {
            return false;
        }
        std::fs::read_link(format!("/proc/{pid}/exe"))
            .is_ok_and(|e| e.to_string_lossy().ends_with(" (deleted)"))
    }

    /// Whether anything under `dir` is not readable by all (or a directory not searchable by all).
    fn unreadable_by_all(dir: &str) -> bool {
        Command::new("find")
            .no_window()
            .args([
                dir, "(", "-not", "-perm", "-o+r", "-o", "(", "-type", "d", "-not", "-perm",
                "-o+x", ")", ")", "-print", "-quit",
            ])
            .output()
            .is_ok_and(|o| !o.stdout.is_empty())
    }

    /// Installs rustup under [`RUST_HOME`] with a first toolchain; the installer is downloaded to a file only
    /// this run made, then run.
    fn install_rustup(c: &mut Ctx, env: &[(&str, &str)], first: &str) -> Res<()> {
        let what = "installed rustup in /opt/rust";
        if c.dry {
            c.say("would:", what);
            return Ok(());
        }
        let script = std::env::temp_dir().join(format!("han-rustup-{}.sh", std::process::id()));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&script)?;
        let path = script.to_string_lossy().into_owned();
        let got = c.cmd(
            "curl",
            &[
                "--proto",
                "=https",
                "--tlsv1.2",
                "-sSf",
                "-o",
                &path,
                "https://sh.rustup.rs",
            ],
            "downloaded the rustup installer",
        );
        let result = got.and_then(|()| {
            let mut args = vec![
                path.as_str(),
                "-y",
                "--no-modify-path",
                "--profile",
                "minimal",
                "--default-toolchain",
                first,
            ];
            args.extend(RUST_COMPONENTS);
            c.cmd_env("sh", &args, env, what)
        });
        let _ = std::fs::remove_file(&script);
        result
    }

    /// `--toolchain rust`: the shared toolchain of docs/runner.md 7.2.
    fn rust_toolchain(c: &mut Ctx, r: &Rust, node_user: &str) -> Res<()> {
        println!("rust toolchain ({RUST_HOME})");
        let rustup_home = format!("{RUST_HOME}/rustup");
        let cargo_home = format!("{RUST_HOME}/cargo");
        let cargo_bin = format!("{cargo_home}/bin");
        let rustup = format!("{cargo_bin}/rustup");
        let env = [
            ("RUSTUP_HOME", rustup_home.as_str()),
            ("CARGO_HOME", cargo_home.as_str()),
        ];

        // what was asked for, and what the repositories pin
        let mut want: Vec<String> = Vec::new();
        for ch in &r.channels {
            if !want.contains(ch) {
                want.push(ch.clone());
            }
        }
        let pins = crate::rusttool::pins(&r.repos);
        for repo in &r.repos {
            if !pins.iter().any(|(p, _)| p == repo) {
                c.say("note:", &format!("{} pins no toolchain", repo.display()));
            }
        }
        for (repo, ch) in pins {
            c.say("pinned:", &format!("{ch} by {}", repo.display()));
            if !want.contains(&ch) {
                want.push(ch);
            }
        }

        // the toolchain rustup installs with itself
        let mut first = None;
        if Path::new(&rustup).is_file() {
            c.say("unchanged:", &format!("rustup in {RUST_HOME}"));
        } else {
            c.dir(RUST_HOME, None, 0o755)?;
            let name = want.first().map_or("stable", String::as_str).to_owned();
            install_rustup(c, &env, &name)?;
            first = Some(name);
        }
        let have = crate::rusttool::installed(Path::new(&rustup_home));
        for ch in &want {
            if first.as_ref() == Some(ch) {
                c.say("note:", &format!("toolchain {ch} came with rustup"));
            } else if crate::rusttool::channel_installed(ch, &have) {
                c.say("unchanged:", &format!("toolchain {ch}"));
            } else if Path::new(&rustup).is_file() || c.dry {
                let mut args = vec!["toolchain", "install", ch.as_str(), "--profile", "minimal"];
                args.extend(RUST_COMPONENTS);
                c.cmd_env(&rustup, &args, &env, &format!("installed toolchain {ch}"))?;
            }
        }

        // readable by every agent user, writable by root only
        if Path::new(RUST_HOME).exists() {
            if unreadable_by_all(RUST_HOME) {
                c.cmd(
                    "chmod",
                    &["-R", "a+rX", RUST_HOME],
                    &format!("{RUST_HOME} is readable by all"),
                )?;
            } else {
                c.say("unchanged:", &format!("{RUST_HOME} is readable by all"));
            }
        }

        // where the sandboxes find it
        let mut file = AgentsFile::load(&r.data)?;
        let paths: BTreeMap<String, PathBuf> = [
            ("rustupHome".to_owned(), PathBuf::from(&rustup_home)),
            ("cargoBin".to_owned(), PathBuf::from(&cargo_bin)),
        ]
        .into();
        let shown = r.data.join("agents.json").display().to_string();
        if !file.set_toolchain("rust", paths)? {
            c.say(
                "unchanged:",
                &format!("toolchains and toolchainPaths in {shown}"),
            );
        } else if c.dry {
            c.say(
                "would:",
                &format!("write toolchains and toolchainPaths to {shown}"),
            );
        } else {
            if !r.data.exists() {
                let owner = user_exists(node_user).then_some(node_user);
                c.dir(&r.data.to_string_lossy(), owner, 0o700)?;
            }
            file.save(&r.data)?;
            // the node's user owns its data: `han agent set` runs as that user
            c.cmd(
                "chown",
                &[&format!("--reference={}", r.data.display()), &shown],
                &format!("wrote toolchains and toolchainPaths to {shown}"),
            )?;
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn ctx() -> Ctx {
            Ctx {
                dry: false,
                changed: false,
                reload: false,
            }
        }

        #[test]
        fn a_planted_symlink_is_refused_for_root_s_headless_marker() {
            use std::os::unix::fs::MetadataExt as _;
            let base = std::env::temp_dir().join(format!("han-marker-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            let dir = base.join("headless.d");
            std::fs::create_dir_all(&dir).unwrap();
            // the user running the test stands in for root
            let me = std::fs::metadata(&dir).unwrap().uid();
            let victim = base.join("victim");
            std::fs::write(&victim, "keep").unwrap();
            // a marker that is a symlink to a file is refused, and the file is not written through
            std::os::unix::fs::symlink(&victim, dir.join("node")).unwrap();
            let e = ctx()
                .root_marker(
                    &dir,
                    "node",
                    "headless
",
                    me,
                )
                .unwrap_err();
            assert!(e.to_string().contains("not a plain file"), "{e}");
            assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
            // so is a symlink where the directory should be
            let link = base.join("link.d");
            std::os::unix::fs::symlink(&dir, &link).unwrap();
            let e = ctx().root_dir(link.to_str().unwrap(), me).unwrap_err();
            assert!(e.to_string().contains("not a plain directory"), "{e}");
            // and a directory another user owns
            let e = ctx()
                .root_dir(dir.to_str().unwrap(), me.wrapping_add(1))
                .unwrap_err();
            assert!(e.to_string().contains("not a plain directory"), "{e}");
            // a planted temp name does not redirect the write, and a plain marker is written and kept
            std::fs::remove_file(dir.join("node")).unwrap();
            std::os::unix::fs::symlink(&victim, dir.join(".node.tmp")).unwrap();
            ctx()
                .root_marker(
                    &dir,
                    "node",
                    "headless
",
                    me,
                )
                .unwrap();
            assert_eq!(
                std::fs::read_to_string(dir.join("node")).unwrap(),
                "headless
"
            );
            assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
            let _ = std::fs::remove_dir_all(&base);
        }

        #[test]
        fn agent_units_are_found_in_a_listing() {
            let listing =
                "han-agent@han-agent1.service loaded active running HAN agent host as han-agent1
                           han-exec.service loaded active running x
                           han-agent@a.service loaded active running y
";
            assert_eq!(
                parse_agent_units(listing),
                ["han-agent@han-agent1.service", "han-agent@a.service"]
            );
        }

        #[test]
        fn a_unit_with_more_than_its_host_runs_a_session() {
            assert!(!has_other_processes(
                "42
", "42"
            ));
            assert!(!has_other_processes("", "42"));
            assert!(has_other_processes(
                "42
77
", "42"
            ));
        }

        #[tokio::test]
        async fn an_admission_file_from_before_the_restart_is_not_ready() {
            let dir = std::env::temp_dir().join(format!("han-adm-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let file = dir.join("admission.json");
            std::fs::write(&file, r#"{"state":"Admitted"}"#).unwrap();
            let before = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
            let after = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
            assert!(admitted_since(&file, before).await.is_ok());
            assert!(admitted_since(&file, after).await.is_err());
            std::fs::write(&file, r#"{"state":"Refused"}"#).unwrap();
            assert!(admitted_since(&file, before).await.is_err());
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

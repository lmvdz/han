//! `han runner setup`: the root-level part of a shared runner (docs/runner.md), Linux only, and the only `han`
//! command that needs root. It makes the OS users, the agents directory, installs the `han-agent@` template
//! unit, the executor unit (left disabled), the polkit rule and the node's drop-in. Everything else
//! (`han home`, `han agent add`, `han executor add`) runs without root.
//!
//! Safe to repeat: every step looks at what is there first and the output says what changed.

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
        dry_run,
    } = args.cmd;
    linux::setup(&node_user, &exec_user, agents, restart_stale, dry_run)
}

#[cfg(target_os = "linux")]
mod linux {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;
    use std::process::Command;

    type Res<T> = Result<T, Box<dyn std::error::Error>>;

    const AGENT_UNIT: &str = include_str!("../../../integrations/runner/han-agent@.service");
    const EXEC_UNIT: &str = include_str!("../../../integrations/runner/han-exec.service");
    const POLKIT_RULE: &str = include_str!("../../../integrations/runner/50-han-agent.rules");
    const AGENTS_ROOT: &str = "/var/lib/han-agents";
    const NODE_DATA: &str = "/var/lib/han";
    const EXEC_DIR: &str = "/var/lib/han-exec";

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
            if self.dry {
                self.say("would:", what);
                return Ok(());
            }
            let out = Command::new(program).args(args).output()?;
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
            .arg(user)
            .output()
            .is_ok_and(|o| o.status.success())
    }

    fn is_root() -> bool {
        Command::new("id")
            .arg("-u")
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
    }

    fn in_group(user: &str, group: &str) -> bool {
        Command::new("id")
            .args(["-nG", user])
            .output()
            .is_ok_and(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .any(|g| g == group)
            })
    }

    pub fn setup(
        node_user: &str,
        exec_user: &str,
        agents: u32,
        restart_stale: bool,
        dry: bool,
    ) -> Res<()> {
        if !han_node::agents::valid_os_user(node_user)
            || !han_node::agents::valid_os_user(exec_user)
        {
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
        for unit in ["han-seq", "han-node"] {
            let active = Command::new("systemctl")
                .args(["is-active", unit])
                .output()
                .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "active");
            if !active {
                continue;
            }
            let stale = unit_binary_replaced(unit);
            let needs = stale || (unit == "han-node" && group_added);
            if !needs {
                c.say("unchanged:", &format!("{unit} is running current code"));
            } else if restart_stale && !dry {
                c.cmd(
                    "systemctl",
                    &["restart", unit],
                    &format!(
                        "restarted {unit} ({})",
                        if stale {
                            "replaced binary"
                        } else {
                            "new group"
                        }
                    ),
                )?;
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
        if dry {
            println!("dry run: nothing changed");
        } else if c.changed {
            println!(
                "done: changes made{}",
                if warnings {
                    ", with restarts pending"
                } else {
                    ""
                }
            );
        } else {
            println!(
                "done: nothing to change{}",
                if warnings {
                    ", with restarts pending"
                } else {
                    ""
                }
            );
        }
        Ok(())
    }

    /// Whether the main process of `unit` runs a binary that has been replaced on disk.
    fn unit_binary_replaced(unit: &str) -> bool {
        let Ok(out) = Command::new("systemctl")
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
}

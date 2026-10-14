//! `han executor add`: sets up an executor (the `han exec` role): its key where `han exec` looks for it, its
//! configuration and environment file, and (where this device may) its profile, the operator's confirmation and
//! its membership of a space. It needs no root: the unit and the OS user are `han runner setup`'s, and the unit
//! stays disabled until an admin enables it.
//!
//! Safe to repeat: each step looks at what is there and the output says what changed.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use han_exec::{ExecConfig, RepoTarget};
use han_node::keys::{FileKeyStore, KeyStore};
use han_node::rpc::{RpcClient, default_endpoint};

use crate::home::HomeOpts;
use crate::rolenode::{key_dir, key_name};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// Where the executor of an OS user keeps everything (the unit of `han runner setup` reads here).
pub const RUNNER_DIR: &str = "/var/lib/han-exec";

/// Options of `han executor`.
#[derive(Args)]
pub struct ExecutorArgs {
    #[command(subcommand)]
    cmd: ExecutorCmd,
}

#[derive(Subcommand)]
enum ExecutorCmd {
    /// Sets an executor up. Prints what another party (the home's admin, the operator) has to run.
    Add {
        /// The executor's name, without `@` and server (`exec` makes `@exec:<server>`).
        name: String,
        /// Its operator (a person, `@name:server`). The executor's server defaults to the operator's.
        #[arg(long)]
        operator: String,
        /// The executor's server, if it is not the operator's.
        #[arg(long)]
        server: Option<String>,
        /// Linux: the OS user the executor runs as; its files default to `/var/lib/han-exec`.
        #[arg(long)]
        os_user: Option<String>,
        /// The executor's directory: `keys/`, `state/`, `exec.json`, `exec.env`.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// A repository the executor may push to: `<name>=<remote>` (the name is what proposals call it; for
        /// a local repository it is the repository's path). Repeatable.
        #[arg(long)]
        repo: Vec<String>,
        /// The space the executor serves; it is made a member of it.
        #[arg(long)]
        space: Option<String>,
        /// The data directory of the home, when this machine runs it.
        #[arg(long)]
        home_data: Option<PathBuf>,
        /// The data directory of the operator's running `han node`, if this device is the operator's.
        #[arg(long, env = "HAN_DATA", default_value = "han-data")]
        data: PathBuf,
        /// Relays for the executor: `n0`, `none` or URLs.
        #[arg(long, default_value = "n0")]
        relay: String,
        #[command(flatten)]
        home: HomeOpts,
    },
}

pub async fn run(args: ExecutorArgs) -> Res<()> {
    let ExecutorCmd::Add {
        name,
        operator,
        server,
        os_user,
        dir,
        repo,
        space,
        home_data,
        data,
        relay,
        home,
    } = args.cmd;
    let server = match (&server, operator.rsplit_once(':')) {
        (Some(s), _) => s.clone(),
        (None, Some((_, s))) => s.to_owned(),
        (None, None) => return Err("--operator is @name:server".into()),
    };
    let actor = format!("@{name}:{server}");
    let dir = dir.unwrap_or_else(|| {
        if os_user.is_some() {
            PathBuf::from(RUNNER_DIR)
        } else {
            PathBuf::from("han-exec")
        }
    });
    println!(
        "executor {actor} (operator {operator}) in {}",
        dir.display()
    );

    // 1. the key, where `han exec --keys <dir>` looks for it
    let store = FileKeyStore::new(key_dir(&dir));
    let kname = key_name(&actor);
    let key_file = key_dir(&dir).join(format!("{kname}.key"));
    let existed = store.load(&kname)?.is_some();
    let seed = store.load_or_create(&kname)?;
    let public_key = han_core::SigningKey::from_seed(seed)
        .public_key()
        .to_base64url();
    if existed {
        println!("  unchanged: key {}", key_file.display());
    } else {
        println!("  changed:   made key {}", key_file.display());
    }
    println!("  key k1 {public_key}");
    std::fs::create_dir_all(dir.join("state"))?;

    // 2. exec.json
    let cfg_path = dir.join("exec.json");
    let mut cfg_changed = !cfg_path.exists();
    let mut cfg: ExecConfig = match std::fs::read(&cfg_path) {
        Ok(b) => serde_json::from_slice(&b)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            serde_json::from_str(r#"{"repos": [], "branch_prefixes": ["han/"]}"#)?
        }
        Err(e) => return Err(format!("{}: {e}", cfg_path.display()).into()),
    };
    for spec in &repo {
        let (rname, remote) = spec.split_once('=').ok_or("--repo is <name>=<remote>")?;
        match cfg.repos.iter_mut().find(|r| r.name == rname) {
            Some(r) if r.remote == remote => {}
            Some(r) => {
                r.remote = remote.to_owned();
                cfg_changed = true;
            }
            None => {
                cfg.repos.push(RepoTarget {
                    name: rname.to_owned(),
                    remote: remote.to_owned(),
                    github: None,
                });
                cfg_changed = true;
            }
        }
    }
    if cfg_changed {
        write_text(&cfg_path, &serde_json::to_string_pretty(&cfg)?)?;
        println!("  changed:   {}", cfg_path.display());
    } else {
        println!("  unchanged: {}", cfg_path.display());
    }

    // 3. exec.env (what the unit reads)
    let env_path = dir.join("exec.env");
    let ticket = home.home.clone().unwrap_or_default();
    let env = format!(
        "HAN_SERVER={server}\nHAN_SPACE={}\nHAN_RELAY={relay}\nHAN_HOME_TICKET={ticket}\n",
        space.clone().unwrap_or_default()
    );
    if std::fs::read_to_string(&env_path).ok().as_deref() == Some(env.as_str()) {
        println!("  unchanged: {}", env_path.display());
    } else if space.is_none() && ticket.is_empty() && env_path.exists() {
        println!(
            "  unchanged: {} (no --space or --home given)",
            env_path.display()
        );
    } else {
        write_text(&env_path, &env)?;
        println!("  changed:   {}", env_path.display());
    }

    // 4. ownership (root only; otherwise the files are the caller's and the note says so)
    if let Some(user) = &os_user {
        chown(&dir, user);
    }

    // 5. the unit, for an executor that runs as an OS user
    if os_user.is_some() {
        unit_state();
    }

    // 6. profile, confirmation, membership
    let client = RpcClient::connect(&default_endpoint(&data)).await.ok();
    if client.is_none() {
        println!(
            "  (no node running with --data {}: the steps that need the operator's node are listed below)",
            data.display()
        );
    }
    let outcome = crate::enroll::run(&crate::enroll::Enroll {
        actor: &actor,
        kind: "service",
        key_id: "k1",
        public_key: &public_key,
        operator: &operator,
        home: &home,
        home_data: home_data.as_deref(),
        space: space.as_deref(),
        client: client.as_ref(),
    })
    .await?;
    println!(
        "by hand: han exec --actor {actor} --space {} --home-ticket <ticket> --keys {} --data {} --config {}",
        space.as_deref().unwrap_or("<space>"),
        dir.display(),
        dir.join("state").display(),
        cfg_path.display()
    );
    if outcome.todo.is_empty() {
        println!("ready: {actor}");
    } else {
        println!(
            "not ready: {} step(s) wait for somebody else; run this command again afterwards",
            outcome.todo.len()
        );
    }
    Ok(())
}

fn write_text(path: &Path, text: &str) -> Res<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(path, text)?;
    Ok(())
}

/// Hands the executor's directory to its OS user, when that is allowed (root).
fn chown(dir: &Path, user: &str) {
    if !cfg!(unix) {
        return;
    }
    let root = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false);
    if root {
        let ok = std::process::Command::new("chown")
            .args(["-R", &format!("{user}:{user}")])
            .arg(dir)
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            println!("  changed:   {} is owned by {user}", dir.display());
        } else {
            println!("  to do:     could not chown {} to {user}", dir.display());
        }
    } else {
        println!(
            "  note:      {} stays owned by you; `sudo chown -R {user}: {}` hands it to {user}",
            dir.display(),
            dir.display()
        );
    }
}

/// Says where the unit stands. Never installs or enables it: that is `han runner setup` and an admin.
fn unit_state() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let unit = Path::new("/etc/systemd/system/han-exec.service");
    if !unit.exists() {
        println!(
            "  to do:     the unit is not installed: an admin runs `sudo han runner setup` (installs it, disabled)"
        );
        return;
    }
    let enabled = std::process::Command::new("systemctl")
        .args(["is-enabled", "han-exec"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default();
    println!(
        "  unchanged: han-exec.service is installed ({}); an admin enables it with `sudo systemctl enable --now han-exec`",
        if enabled.is_empty() {
            "state unknown"
        } else {
            &enabled
        }
    );
}

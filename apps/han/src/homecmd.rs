//! `han home`: set up a home and register profiles on it.
//!
//! * `han home init --server S --data D` makes a home's directory: its settings (`home.json`, which `han seq
//!   --data D` then needs no flags for), its receipt key and node key, and its ticket. Safe to repeat: what is
//!   there stays, and it says what it changed.
//! * `han home register <actor> --kind agent|service --public-key K --operator O` creates a profile on the
//!   running home (no restart). `han seq register` is the same command under its old name.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Args, Subcommand};
use han_net::{IrohOptions, IrohTransport, Transport};
use han_seq::SqliteStore;
use serde::{Deserialize, Serialize};

use crate::seq::{open_home, relay_choice, write_ticket};
use crate::seqadmin::{self, Register};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// Options of `han home`.
#[derive(Args)]
pub struct HomeArgs {
    #[command(subcommand)]
    cmd: HomeCmd,
}

#[derive(Subcommand)]
enum HomeCmd {
    /// Creates a home: its directory, keys, settings and ticket. Run `han seq --data <dir>` to serve it.
    Init {
        /// The ServerName of the home (lowercase DNS name; a placeholder such as `team.han.home.arpa` does
        /// until it has DNS).
        #[arg(long)]
        server: Option<String>,
        /// The home's data directory.
        #[arg(long, env = "HAN_HOME_DATA", default_value = "han-home")]
        data: PathBuf,
        /// UDP address of the home's QUIC endpoint, kept for `han seq`.
        #[arg(long)]
        bind: Option<SocketAddr>,
        /// TCP address of the well-known HTTP server, kept for `han seq`.
        #[arg(long)]
        http: Option<SocketAddr>,
        /// Relays: `n0`, `none`, or URLs separated by commas, kept for `han seq`.
        #[arg(long)]
        relay: Option<String>,
        /// Who may create spaces here: `known`, `open` or `allow:<actor-or-server>,...`, kept for `han seq`.
        #[arg(long)]
        hosting: Option<String>,
    },
    /// Creates a profile on the running home at `--data`, with no restart. Admin only: it goes over the home's
    /// local admin socket. Registering an actor again changes nothing.
    Register {
        /// The actor, `@name:server`.
        actor: String,
        /// The home's data directory.
        #[arg(long, env = "HAN_HOME_DATA", default_value = "han-home")]
        data: PathBuf,
        /// `human`, `agent` or `service`.
        #[arg(long, default_value = "agent")]
        kind: String,
        /// The key's id.
        #[arg(long, default_value = "k1")]
        key_id: String,
        /// The key, base64url (it may start with `-`).
        #[arg(long, allow_hyphen_values = true)]
        public_key: String,
        /// The operator, for an agent or a service.
        #[arg(long)]
        operator: Option<String>,
    },
}

/// What `han home init` keeps in `home.json` for `han seq`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeSettings {
    /// The ServerName.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// The QUIC endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind: Option<SocketAddr>,
    /// The well-known HTTP server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<SocketAddr>,
    /// The relays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,
    /// The hosting policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosting: Option<String>,
}

const SETTINGS_FILE: &str = "home.json";

impl HomeSettings {
    /// The settings in `data`, or none if the home was never initialised.
    pub fn load(data: &Path) -> Res<HomeSettings> {
        let path = data.join(SETTINGS_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()).into())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HomeSettings::default()),
            Err(e) => Err(format!("{}: {e}", path.display()).into()),
        }
    }

    fn save(&self, data: &Path) -> Res<()> {
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(data.join(SETTINGS_FILE), format!("{text}\n"))?;
        Ok(())
    }
}

pub async fn run(args: HomeArgs) -> Res<()> {
    match args.cmd {
        HomeCmd::Init {
            server,
            data,
            bind,
            http,
            relay,
            hosting,
        } => {
            let ticket = init(&data, server, bind, http, relay, hosting).await?;
            println!("ticket-file {}", ticket.display());
            if let Ok(text) = std::fs::read_to_string(&ticket) {
                println!("ticket {}", text.trim());
            }
            Ok(())
        }
        HomeCmd::Register {
            actor,
            data,
            kind,
            key_id,
            public_key,
            operator,
        } => {
            register(
                &data,
                Register {
                    actor,
                    kind,
                    key_id,
                    public_key,
                    operator,
                },
            )
            .await
        }
    }
}

/// Makes (or finds) the home in `data`; returns the path of its ticket file.
pub async fn init(
    data: &Path,
    server: Option<String>,
    bind: Option<SocketAddr>,
    http: Option<SocketAddr>,
    relay: Option<String>,
    hosting: Option<String>,
) -> Res<PathBuf> {
    let existed = data.join(SETTINGS_FILE).exists();
    std::fs::create_dir_all(data)?;
    let had = HomeSettings::load(data)?;
    if let (Some(old), Some(new)) = (&had.server, &server)
        && old != new
    {
        return Err(format!(
            "{} is the home of {old}: a home's name does not change (use another --data for {new})",
            data.display()
        )
        .into());
    }
    if let Some(h) = &hosting {
        // checked now, so a typo is not found when the home starts
        h.parse::<HostingSpec>()?;
    }
    let want = HomeSettings {
        server: server.or(had.server.clone()),
        bind: bind.or(had.bind),
        http: http.or(had.http),
        relay: relay.or(had.relay.clone()),
        hosting: hosting.or(had.hosting.clone()),
    };
    if want.server.is_none() {
        return Err("han home init needs --server".into());
    }
    if want == had && existed {
        println!("unchanged: {}", data.join(SETTINGS_FILE).display());
    } else {
        want.save(data)?;
        println!(
            "{}: {}",
            if existed { "updated" } else { "wrote" },
            data.join(SETTINGS_FILE).display()
        );
    }
    // A running home owns the ports and the database: leave it be.
    if seqadmin_reachable(data).await {
        println!(
            "running: the home at {} is up; nothing else to do",
            data.display()
        );
        return Ok(data.join(crate::seq::TICKET_FILE));
    }
    let server_name = want.server.as_deref().unwrap_or_default();
    let new_keys = ["server.key", "node.key"]
        .iter()
        .filter(|f| !data.join(f).exists())
        .count();
    let store = Arc::new(SqliteStore::open(data)?);
    let home = open_home(data, server_name, store)?;
    let seed = crate::keys::load_or_create(&data.join("node.key"))?;
    let mut opts = IrohOptions::new(seed);
    opts.bind = want.bind;
    opts.relay = relay_choice(want.relay.as_deref().unwrap_or("n0"));
    let transport = IrohTransport::bind(opts).await?;
    transport.online(std::time::Duration::from_secs(5)).await;
    let ticket = write_ticket(data, &home, &transport)?;
    println!("server {server_name}");
    println!("node {}", transport.node_id());
    println!(
        "keys: {}",
        if new_keys == 0 {
            "unchanged"
        } else {
            "created (server.key, node.key)"
        }
    );
    transport.shutdown().await;
    println!(
        "serve it with: han seq --data {} (the ticket is rewritten with the address it binds)",
        data.display()
    );
    Ok(ticket)
}

/// `hosting` as the settings keep it.
struct HostingSpec;

impl std::str::FromStr for HostingSpec {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "known" | "open" => Ok(HostingSpec),
            l if l.strip_prefix("allow:").is_some_and(|l| !l.is_empty()) => Ok(HostingSpec),
            _ => Err("hosting: want known, open or allow:<actor-or-server>,...".into()),
        }
    }
}

async fn seqadmin_reachable(data: &Path) -> bool {
    han_node::rpc::connect_local(&seqadmin::endpoint(data))
        .await
        .is_ok()
}

/// Registers a profile on the running home at `data` and says what happened.
pub async fn register(data: &Path, r: Register) -> Res<()> {
    let actor = r.actor.clone();
    let (seq, already) = seqadmin::ask(data, &r).await?;
    if already {
        println!("already registered: {actor} (profile:{actor} at seq {seq}); nothing changed");
    } else {
        println!("registered {actor} (profile:{actor} at seq {seq})");
    }
    Ok(())
}

/// The pid of a `han seq` serving `data` that runs a binary which has since been replaced (Linux: its
/// `/proc/<pid>/exe` reads "(deleted)"). Such a home predates the current binary, so it may lack features
/// such as the admin socket.
pub fn stale_home(data: &Path) -> Option<u32> {
    #[cfg(target_os = "linux")]
    {
        let want = std::fs::canonicalize(data).ok()?;
        for entry in std::fs::read_dir("/proc").ok()?.flatten() {
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
            if args.get(1).map(String::as_str) != Some("seq") {
                continue;
            }
            let Some(dir) = args
                .iter()
                .position(|a| a == "--data")
                .and_then(|i| args.get(i + 1))
            else {
                continue;
            };
            if std::fs::canonicalize(dir).ok().as_ref() != Some(&want) {
                continue;
            }
            let Ok(exe) = std::fs::read_link(entry.path().join("exe")) else {
                continue;
            };
            if exe.to_string_lossy().ends_with(" (deleted)") {
                return Some(pid);
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = data;
        None
    }
}

/// Every `han seq` on this machine that runs a replaced binary, as `(pid, data dir)`.
#[cfg(target_os = "linux")]
pub fn stale_homes() -> Vec<(u32, String)> {
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
        if args.get(1).map(String::as_str) != Some("seq") {
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
            out.push((pid, data));
        }
    }
    out
}

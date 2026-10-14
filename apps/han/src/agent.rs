//! `han agent`: set up, start and stop agents through the running node (a face over its pipe).
//!
//! * `han agent add coder --operator @lars:han.example` makes the agent's key and launch settings, then
//!   registers, confirms and admits it where this device may, and prints what other parties run where it may
//!   not. Safe to repeat.
//! * `han agent create --name coder --server han.example --operator @lars:han.example` makes only the agent's
//!   key in the node's key store and prints what its home must register (the profile with its operator).
//! * `han agent start --space S --agent @coder:han.example --harness pi|claude --repo github:o/r --issue
//!   github:o/r#12` asks the node's supervisor to start a `han host` for the session.
//! * `han agent stop @coder:han.example` stops its sessions.
//! * `han agent setup-harness claude|pi` installs the pinned ACP adapter into the data directory, so `start`
//!   needs no flags; `han agent login @coder:han.example` logs the agent in to Claude Code once, in its own
//!   login directory (`--check` only says whether a login exists).

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use han_node::NodeApi;
use han_node::api::{CallResult, Command};
use han_node::rpc::{RpcClient, default_endpoint};
use han_registry::{AgentEdit, HomeRef};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// Options of `han agent`.
#[derive(Args)]
pub struct AgentArgs {
    #[command(subcommand)]
    cmd: AgentCmd,
    /// The data directory of the running `han node` (names its pipe).
    #[arg(long, env = "HAN_DATA", default_value = "han-data", global = true)]
    data: PathBuf,
    /// The node's endpoint, if it is not the default of `--data`.
    #[arg(long, global = true)]
    endpoint: Option<String>,
}

#[derive(Subcommand)]
enum AgentCmd {
    /// Sets an agent up: its key, its launch settings and, where this device may, its profile at the home, the
    /// operator's confirmation and its membership of a space. Safe to repeat. Whatever another party has to do
    /// is printed as the exact command to run.
    Add {
        /// The agent's name, without `@` and server (`coder` makes `@coder:<server>`).
        name: String,
        /// Its operator (a person, `@name:server`). The agent's server defaults to the operator's.
        #[arg(long)]
        operator: String,
        /// The agent's server, if it is not the operator's.
        #[arg(long)]
        server: Option<String>,
        /// Linux: run the agent's host as this OS user (SR-83; `sudo han runner setup` makes the user).
        #[arg(long)]
        os_user: Option<String>,
        /// The sandbox for this agent: `auto`, `mxc`, `bwrap` or `none`.
        #[arg(long)]
        sandbox: Option<String>,
        /// Let this agent run with no OS sandbox when none is available (advisory; it shows in status).
        #[arg(long)]
        allow_advisory: bool,
        /// The harness this agent uses: `claude` or `pi` (installed now if it is not), or one recorded in the
        /// node's `harness/` directory.
        #[arg(long)]
        harness: Option<String>,
        /// Make the agent a member of this space.
        #[arg(long)]
        space: Option<String>,
        /// The data directory of the home, when this machine runs it: the profile is then registered here,
        /// over the home's admin socket.
        #[arg(long)]
        home_data: Option<PathBuf>,
        /// The agent's home: a home ticket (`han1home.…`) or a ServerName. Default: the agent's server.
        #[arg(long)]
        home: Option<HomeRef>,
    },
    /// Makes an agent's key and prints what its home registers.
    Create {
        /// The agent's name, without `@` and server.
        #[arg(long)]
        name: String,
        /// Its server.
        #[arg(long)]
        server: String,
        /// Its operator (a person).
        #[arg(long)]
        operator: String,
    },
    /// The operator confirms an agent whose profile exists: `member.set {actor: <operator>, role: owner}` in
    /// the agent's profile space (§4.3). The same as `han actor confirm`.
    Confirm {
        /// The agent.
        agent: String,
        /// The agent's home: a home ticket (`han1home.…`) or a ServerName. Default: the agent's server.
        #[arg(long)]
        home: Option<HomeRef>,
        /// The operator (this node's actor).
        #[arg(long)]
        operator: String,
    },
    /// Starts a session of an agent on this node.
    Start {
        /// The space.
        #[arg(long)]
        space: String,
        /// The agent actor.
        #[arg(long)]
        agent: String,
        /// The harness: `pi`, `claude`, or a driver configured by name. Default: the one `han agent add`
        /// recorded for the agent.
        #[arg(long)]
        harness: Option<String>,
        /// The repository, `github:owner/repo` (or a local path for tests).
        #[arg(long)]
        repo: Option<String>,
        /// The issue, `github:owner/repo#123`.
        #[arg(long)]
        issue: Option<String>,
        /// The agent's home: a home ticket (`han1home.…`) or a ServerName. The node follows the agent's profile
        /// there to find its operator.
        #[arg(long)]
        home: Option<HomeRef>,
    },
    /// Installs a harness's pinned ACP adapter (npm, with Node 22) into the data directory and records where.
    SetupHarness {
        /// `claude` or `pi`.
        harness: String,
        /// Install here instead of `<data>/harness/<harness>` (for example a directory an OS-user agent can
        /// read).
        #[arg(long)]
        dir: Option<PathBuf>,
        /// A Node 22 to use, instead of the one on the PATH or the pinned build `han` downloads.
        #[arg(long)]
        node: Option<PathBuf>,
    },
    /// Logs an agent in to Claude Code, once, in the agent's own login directory (`CLAUDE_CONFIG_DIR`). Runs
    /// `claude` interactively outside the sandbox; the login is the only thing of yours the agent's sandbox
    /// will see. With `--check` it only says whether a login exists, and never shows its contents.
    Login {
        /// The agent actor.
        agent: String,
        /// Only report whether a login exists (exit code 1 if not).
        #[arg(long)]
        check: bool,
        /// The login directory, if it is not the agent's default.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// The `claude` program, if it is not on the PATH.
        #[arg(long)]
        claude: Option<PathBuf>,
    },
    /// Stops an agent's sessions, or one `host-<agent>#<issue>`.
    Stop {
        /// The agent or session name.
        target: String,
    },
}

pub async fn run(args: AgentArgs) -> Result<(), Box<dyn std::error::Error>> {
    // These two work on the data directory and need no running node.
    match &args.cmd {
        AgentCmd::Login {
            agent,
            check,
            dir,
            claude,
        } => {
            return crate::harness::login(
                &args.data,
                agent,
                *check,
                dir.as_deref(),
                claude.as_deref(),
            );
        }
        AgentCmd::SetupHarness { harness, dir, node } => {
            let r = crate::harness::setup(&args.data, harness, dir.as_deref(), node.as_deref())?;
            println!(
                "{} {} installed: node {}, adapter {}",
                r.package,
                r.version,
                r.node.display(),
                r.adapter_dir.display()
            );
            return Ok(());
        }
        _ => {}
    }
    let endpoint = args
        .endpoint
        .clone()
        .unwrap_or_else(|| default_endpoint(&args.data));
    let client = RpcClient::connect(&endpoint).await.map_err(|e| {
        format!(
            "cannot reach the node at {endpoint}: {e} (is `han node` running with --data {}?)",
            args.data.display()
        )
    })?;
    match args.cmd {
        AgentCmd::Add {
            name,
            operator,
            server,
            os_user,
            sandbox,
            allow_advisory,
            harness,
            space,
            home_data,
            home,
        } => {
            let opts = Add {
                allow_advisory,
                name,
                operator,
                server,
                os_user,
                sandbox,
                harness,
                space,
                home_data,
                home,
            };
            add(&client, &args.data, opts).await?;
        }
        AgentCmd::Create {
            name,
            server,
            operator,
        } => {
            let r = client.call(Command::AgentCreate { name, server }).await?;
            if let CallResult::Agent {
                actor,
                key_id,
                public_key,
            } = r
            {
                println!("actor {actor}");
                println!("key {key_id} {public_key}");
                println!(
                    "register at its home (admin, no restart): han home register {actor} --data <home data> --kind agent --key-id {key_id} --public-key {public_key} --operator {operator}"
                );
                println!("then confirm: han actor confirm {actor} --operator {operator} …");
            }
        }
        AgentCmd::Confirm {
            agent,
            home,
            operator,
        } => confirm_cmd(&client, &agent, home.as_ref(), &operator).await?,
        AgentCmd::Start {
            space,
            agent,
            harness,
            repo,
            issue,
            home,
        } => {
            let mut tries = 0;
            loop {
                let r = client
                    .call(Command::AgentStart {
                        space: space.clone(),
                        agent: agent.clone(),
                        harness: harness.clone(),
                        repo: repo.clone(),
                        issue: issue.clone(),
                        home: home.clone(),
                    })
                    .await;
                if tries < 600 && profile_pending(&r) {
                    // The profile is still being fetched.
                    tries += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
                r?;
                break;
            }
            println!("started a session of {agent}");
        }
        AgentCmd::Login { .. } | AgentCmd::SetupHarness { .. } => {}
        AgentCmd::Stop { target } => {
            client.call(Command::AgentStop { target }).await?;
            println!("stopped");
        }
    }
    Ok(())
}

/// Whether a failed start only waits for the agent's profile to arrive.
fn profile_pending(r: &Result<CallResult, han_node::api::NodeError>) -> bool {
    match r {
        Err(e) => {
            let m = e.to_string();
            m.contains("profile of")
                && (m.contains("has not arrived") || m.contains("is not followed"))
        }
        Ok(_) => false,
    }
}

/// `han agent confirm` and `han actor confirm`.
pub async fn confirm_cmd(
    client: &RpcClient,
    actor: &str,
    home: Option<&HomeRef>,
    operator: &str,
) -> Res<()> {
    match crate::enroll::confirm(client, actor, home, operator, 600).await {
        Ok(true) => println!("changed: {operator} confirmed {actor}"),
        Ok(false) => println!("unchanged: {actor} is already confirmed by {operator}"),
        Err(crate::enroll::Pending::Profile) => {
            return Err(format!(
                "the home has no profile for {actor} yet: its admin runs `han home register {actor} …`"
            )
            .into());
        }
        Err(crate::enroll::Pending::Other(m)) => return Err(m.into()),
    }
    Ok(())
}

/// The options of `han agent add`.
struct Add {
    name: String,
    operator: String,
    server: Option<String>,
    os_user: Option<String>,
    sandbox: Option<String>,
    allow_advisory: bool,
    harness: Option<String>,
    space: Option<String>,
    home_data: Option<PathBuf>,
    home: Option<HomeRef>,
}

/// `han agent add`.
async fn add(client: &RpcClient, data: &Path, a: Add) -> Res<()> {
    let server = match (&a.server, a.operator.rsplit_once(':')) {
        (Some(s), _) => s.clone(),
        (None, Some((_, s))) => s.to_owned(),
        (None, None) => return Err("--operator is @name:server".into()),
    };
    let actor = format!("@{}:{server}", a.name);
    println!("agent {actor} (operator {})", a.operator);

    // 1. launch settings, before the key: an OS-user agent's key lives in that user's state directory
    let changed = crate::harness::record_settings(
        data,
        &actor,
        &AgentEdit {
            os_user: a.os_user.clone(),
            sandbox: a.sandbox.clone(),
            allow_advisory: a.allow_advisory.then_some(true),
            harness: a.harness.clone(),
            toolchains: None,
        },
    )?;
    if changed {
        println!(
            "  changed:   launch settings in {}",
            data.join("agents.json").display()
        );
    } else {
        println!("  unchanged: launch settings");
    }

    // 2. the harness, when it is one `han` installs
    if let Some(h) = a
        .harness
        .as_deref()
        .filter(|h| matches!(*h, "claude" | "pi"))
    {
        if crate::harness::is_set_up(data, h)? {
            println!("  unchanged: the {h} harness is set up");
        } else {
            let r = crate::harness::setup(data, h, None, None)?;
            println!("  changed:   installed {} {} for {h}", r.package, r.version);
        }
    }

    // 3. the key
    let CallResult::Agent {
        key_id, public_key, ..
    } = client
        .call(Command::AgentCreate {
            name: a.name.clone(),
            server: server.clone(),
        })
        .await?
    else {
        return Err("the node did not make a key".into());
    };
    println!("  key {key_id} {public_key}");

    // 4. profile, confirmation, membership
    let outcome = crate::enroll::run(&crate::enroll::Enroll {
        actor: &actor,
        kind: "agent",
        key_id: &key_id,
        public_key: &public_key,
        operator: &a.operator,
        home: a.home.as_ref(),
        home_data: a.home_data.as_deref(),
        space: a.space.as_deref(),
        client: Some(client),
    })
    .await?;
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

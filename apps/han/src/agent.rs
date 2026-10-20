//! `han agent`: set up, start and stop agents through the running node (a face over its pipe).
//!
//! * `han agent add coder --operator @lars:han.example` makes the agent's key and launch settings, then
//!   registers, confirms and admits it where this device may, and prints what other parties run where it may
//!   not. Safe to repeat.
//! * `han agent create --name coder --server han.example --operator @lars:han.example` makes only the agent's
//!   key in the node's key store and prints what its home must register (the profile with its operator).
//! * `han agent start --space S --agent @coder:han.example --harness pi|claude --repo github:o/r --issue
//!   github:o/r#12` asks the node's supervisor to start a `han host` for the session.
//! * `han agent set @coder:han.example --os-user han-agent1 --sandbox bwrap --toolchains rust --harness claude`
//!   changes how an agent is launched (`agents.json`) and nothing else; it needs no running node, never hand-edit
//!   the file. Prints `changed:` or `unchanged:`.
//! * `han agent finish s1:han.example --repo github:o/r land --target-ref refs/heads/main --tier-rule 'docs/**=0'`
//!   turns on the `land` finish flow for a space (or, with `--repo`, one repository of it); `off` turns it off,
//!   `reset` removes the setting, `show` prints what a session would get. It is written to `agents.json`, needs
//!   no running node and takes effect for the sessions started after it. Default: off.
//! * `han agent stop @coder:han.example` stops its sessions.
//! * `han agent setup-harness claude|pi` installs the pinned ACP adapter into the data directory, so `start`
//!   needs no flags; `han agent login @coder:han.example` logs the agent in to Claude Code once, in its own
//!   login directory (`--check` only says whether a login exists).

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use han_node::NodeApi;
use han_node::api::{CallResult, Command};
use han_node::rpc::{RpcClient, default_endpoint};
use han_registry::{AgentEdit, AgentsFile, HomeRef, LandEdit, TierRule};

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
        /// API-key mode: a file holding the model key (readable only by you). The host's proxy keeps the key;
        /// the agent never sees it.
        #[arg(long)]
        model_key_file: Option<PathBuf>,
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
        /// The task to work. `--issue` alone finds the open task for the issue addressed to the agent (the
        /// task carries the issue's text; with none, the start is refused).
        #[arg(long)]
        task: Option<String>,
        /// The agent's home: a home ticket (`han1home.…`) or a ServerName. The node follows the agent's profile
        /// there to find its operator.
        #[arg(long)]
        home: Option<HomeRef>,
    },
    /// Installs a harness's pinned ACP adapter (npm, with Node 22) into the data directory and records where.
    SetupHarness {
        /// A bundled driver (`claude`, `pi`), an installed name, or a TOML/JSON driver file.
        harness: String,
        /// Install here instead of `<data>/harness/<harness>` (for example a directory an OS-user agent can
        /// read).
        #[arg(long)]
        dir: Option<PathBuf>,
        /// A Node 22 to use, instead of the one on the PATH or the pinned build `han` downloads.
        #[arg(long)]
        node: Option<PathBuf>,
    },
    /// Runs the harness's login command in the agent's own login directory, outside the sandbox.
    /// With `--check`, only checks whether its credential file exists; never shows its contents.
    Login {
        /// The agent actor.
        agent: String,
        /// Only report whether a login exists (exit code 1 if not).
        #[arg(long)]
        check: bool,
        /// The login directory, if it is not the agent's default.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Override the first executable in the adapter's login command. `--claude` is a legacy alias.
        #[arg(long, alias = "claude")]
        program: Option<PathBuf>,
        /// Use this harness instead of the agent's recorded one.
        #[arg(long)]
        harness: Option<String>,
    },
    /// Changes how an agent is launched, in `<data>/agents.json`; only what is given changes. Needs no running
    /// node. Prints `changed:` or `unchanged:`.
    Set {
        /// The agent actor, `@name:server`.
        agent: String,
        /// Linux: run the agent's host as this OS user (SR-83).
        #[arg(long)]
        os_user: Option<String>,
        /// The sandbox: `auto`, `mxc`, `bwrap` or `none`.
        #[arg(long)]
        sandbox: Option<String>,
        /// Whether the agent may run with no OS sandbox when none is available (`--allow-advisory`, or
        /// `--allow-advisory=false`).
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        allow_advisory: Option<bool>,
        /// The toolchains inside its sandbox, comma separated (`rust`); `""` for none.
        #[arg(long)]
        toolchains: Option<String>,
        /// The harness `han agent start` uses by default.
        #[arg(long)]
        harness: Option<String>,
        /// API-key mode: a file holding the model key (readable only by you). The host's proxy keeps the
        /// key; the agent never sees it.
        #[arg(long)]
        model_key_file: Option<PathBuf>,
    },
    /// Sets how finished work leaves the sessions of a space (or one of its repositories): through the landing
    /// lane (`land`), or as before. Written to `<data>/agents.json`; needs no running node; sessions started
    /// afterwards use it. A repository's setting is laid over its space's.
    Finish {
        /// The space.
        space: String,
        /// Only this repository of the space, `github:owner/repo` as the session names it.
        #[arg(long)]
        repo: Option<String>,
        #[command(subcommand)]
        action: FinishAction,
    },
    /// Stops an agent's sessions, or one `host-<agent>#<issue>`.
    Stop {
        /// The agent or session name.
        target: String,
    },
}

/// What `han agent finish` does.
#[derive(Subcommand)]
enum FinishAction {
    /// Turns the `land` flow on: the agent proposes `land` and the host proposes one `vcs.land` to the
    /// executor. Only what is given changes.
    Land {
        /// The full branch ref to land on. Default: the work's default branch (`refs/heads/main`). The
        /// executor must list it in the repository's `landing_refs` (`han executor set --landing-refs`).
        #[arg(long)]
        target_ref: Option<String>,
        /// A path rule of the informative tier, `<glob>=<0-3>` (`'docs/**=0'`). Repeatable; given at all, they
        /// replace this layer's. Policy derives the real tier.
        #[arg(long = "tier-rule")]
        tier_rules: Vec<String>,
        /// The repository has no post-merge (`full`) checks, so `landed` is final and the host prunes on it
        /// (`--no-full-stage`, or `--no-full-stage=false`).
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        no_full_stage: Option<bool>,
        /// After the session ends, keep following its proposals for this many seconds, to prune on the final
        /// one.
        #[arg(long)]
        settle_secs: Option<u64>,
    },
    /// Turns the `land` flow off here (a repository can turn it off in a space that has it on).
    Off,
    /// Removes the setting of this layer, so the space's (or the default, off) applies again.
    Reset,
    /// Prints what a session on the space (and repository) would get.
    Show,
}

pub async fn run(args: AgentArgs) -> Result<(), Box<dyn std::error::Error>> {
    // These two work on the data directory and need no running node.
    match &args.cmd {
        AgentCmd::Login {
            agent,
            check,
            dir,
            program,
            harness,
        } => {
            return crate::harness::login(
                &args.data,
                agent,
                *check,
                dir.as_deref(),
                program.as_deref(),
                harness.as_deref(),
            );
        }
        AgentCmd::Set {
            agent,
            os_user,
            sandbox,
            allow_advisory,
            toolchains,
            harness,
            model_key_file,
        } => {
            let edit = AgentEdit {
                model_key_file: model_key_file.clone(),
                os_user: os_user.clone(),
                sandbox: sandbox.clone(),
                allow_advisory: *allow_advisory,
                harness: harness.clone(),
                toolchains: toolchains.as_deref().map(split_list),
            };
            println!("{}", set(&args.data, agent, &edit)?);
            return Ok(());
        }
        AgentCmd::Finish {
            space,
            repo,
            action,
        } => {
            println!("{}", finish(&args.data, space, repo.as_deref(), action)?);
            return Ok(());
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
            model_key_file,
            space,
            home_data,
            home,
        } => {
            let opts = Add {
                model_key_file,
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
            task,
            home,
        } => {
            // The agent's profile names its operator: have the node follow it and wait for it to sync.
            let profile = format!("profile:{agent}");
            let _ = client
                .call(Command::SpaceFollow {
                    home: home.clone(),
                    space: profile.clone(),
                })
                .await;
            client
                .call(Command::SpaceAwait {
                    space: profile,
                    timeout_ms: Some(60_000),
                })
                .await?;
            client
                .call(Command::AgentStart {
                    space,
                    agent: agent.clone(),
                    harness,
                    repo,
                    issue,
                    task,
                    home,
                })
                .await?;
            println!("started a session of {agent}");
        }
        AgentCmd::Login { .. }
        | AgentCmd::SetupHarness { .. }
        | AgentCmd::Set { .. }
        | AgentCmd::Finish { .. } => {}
        AgentCmd::Stop { target } => {
            client.call(Command::AgentStop { target }).await?;
            println!("stopped");
        }
    }
    Ok(())
}

/// `a,b` as names; the empty string is no names.
fn split_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `han agent set`: applies `edit` to `agent` in `<data>/agents.json` and says what happened.
fn set(data: &Path, agent: &str, edit: &AgentEdit) -> Res<String> {
    if !agent
        .strip_prefix('@')
        .and_then(|a| a.split_once(':'))
        .is_some_and(|(n, s)| !n.is_empty() && !s.is_empty())
    {
        return Err(format!("{agent:?} is not an agent actor (@name:server)").into());
    }
    if *edit == AgentEdit::default() {
        return Err(
            "nothing to set: give --os-user, --sandbox, --allow-advisory, --toolchains or --harness".into(),
        );
    }
    let path = data.join("agents.json");
    Ok(if crate::harness::record_settings(data, agent, edit)? {
        format!("changed: {agent} in {}", path.display())
    } else {
        format!("unchanged: {agent} in {}", path.display())
    })
}

/// `<glob>=<tier>` as a path rule.
fn tier_rule(text: &str) -> Res<TierRule> {
    let bad = || format!("--tier-rule {text:?}: give <glob>=<tier>, for example 'docs/**=0'");
    let (path, tier) = text.rsplit_once('=').ok_or_else(bad)?;
    let tier: u8 = tier.trim().parse().map_err(|_| bad())?;
    if path.trim().is_empty() || tier > 3 {
        return Err(bad().into());
    }
    Ok(TierRule {
        path: path.trim().to_owned(),
        tier,
    })
}

/// `han agent finish`: applies `action` to the `land` setting of `space` (or of its `repo`) in
/// `<data>/agents.json` and says what happened.
fn finish(data: &Path, space: &str, repo: Option<&str>, action: &FinishAction) -> Res<String> {
    let mut file = AgentsFile::load(data)?;
    let what = match repo {
        Some(r) => format!("{space} {r}"),
        None => space.to_owned(),
    };
    let path = data.join("agents.json");
    let edit = match action {
        FinishAction::Show => return Ok(show_land(&file, space, repo)),
        FinishAction::Reset => {
            let changed = file.clear_land(space, repo);
            if changed {
                file.save(data)?;
            }
            return Ok(format!(
                "{} the land setting of {what} in {}",
                if changed {
                    "changed: removed"
                } else {
                    "unchanged: no"
                },
                path.display()
            ));
        }
        FinishAction::Off => LandEdit {
            enabled: Some(false),
            ..LandEdit::default()
        },
        FinishAction::Land {
            target_ref,
            tier_rules,
            no_full_stage,
            settle_secs,
        } => LandEdit {
            enabled: Some(true),
            target_ref: target_ref.clone(),
            tier_rules: if tier_rules.is_empty() {
                None
            } else {
                Some(
                    tier_rules
                        .iter()
                        .map(|t| tier_rule(t))
                        .collect::<Res<Vec<_>>>()?,
                )
            },
            no_full_stage: *no_full_stage,
            settle_secs: *settle_secs,
        },
    };
    let changed = file.set_land(space, repo, &edit)?;
    if changed {
        file.save(data)?;
    }
    let mut out = format!(
        "{}: land {} for {what} in {}",
        if changed { "changed" } else { "unchanged" },
        if edit.enabled == Some(true) {
            "on"
        } else {
            "off"
        },
        path.display()
    );
    if edit.enabled == Some(true) {
        out.push('\n');
        out.push_str(&show_land(&file, space, repo));
        out.push_str(
            "\nsessions started from now on use it; a running one keeps its settings.\nthe executor must allow the ref: han executor set <repo> --landing-refs <ref> (han doctor checks the whole setup)",
        );
    }
    Ok(out)
}

/// What `land` resolves to for `space` and `repo`.
fn show_land(file: &AgentsFile, space: &str, repo: Option<&str>) -> String {
    let at = repo.map(|r| format!(" {r}")).unwrap_or_default();
    match file.land_for(space, repo) {
        None => format!("land: off for {space}{at}"),
        Some(l) => {
            let rules: Vec<String> = l
                .tier_rules
                .iter()
                .map(|t| format!("{}={}", t.path, t.tier))
                .collect();
            format!(
                "land: on for {space}{at}: target {}, tier rules [{}], {}, settle {}s",
                l.target_ref.as_deref().unwrap_or("the default branch"),
                rules.join(", "),
                if l.no_full_stage {
                    "no full stage"
                } else {
                    "full stage as the executor has it"
                },
                l.settle_secs
            )
        }
    }
}

/// `han agent confirm` and `han actor confirm`.
pub async fn confirm_cmd(
    client: &RpcClient,
    actor: &str,
    home: Option<&HomeRef>,
    operator: &str,
) -> Res<()> {
    match crate::enroll::confirm(
        client,
        actor,
        home,
        operator,
        std::time::Duration::from_secs(60),
    )
    .await
    {
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
    model_key_file: Option<PathBuf>,
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
            model_key_file: a.model_key_file.clone(),
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

#[cfg(test)]
mod tests {
    use han_registry::AgentsFile;

    use super::*;

    #[test]
    fn set_is_idempotent_and_changes_only_what_is_given() {
        let d = tempfile::tempdir().unwrap();
        let edit = AgentEdit {
            os_user: Some("han-agent1".into()),
            toolchains: Some(split_list("rust")),
            ..AgentEdit::default()
        };
        let first = set(d.path(), "@coder:han.example", &edit).unwrap();
        assert!(first.starts_with("changed: @coder:han.example"), "{first}");
        let again = set(d.path(), "@coder:han.example", &edit).unwrap();
        assert!(again.starts_with("unchanged: "), "{again}");
        let sandbox = AgentEdit {
            sandbox: Some("bwrap".into()),
            ..AgentEdit::default()
        };
        assert!(
            set(d.path(), "@coder:han.example", &sandbox)
                .unwrap()
                .starts_with("changed: ")
        );
        let file = AgentsFile::load(d.path()).unwrap();
        let a = file.agent("@coder:han.example");
        assert_eq!(a.os_user.as_deref(), Some("han-agent1"));
        assert_eq!(a.sandbox.as_deref(), Some("bwrap"));
        assert_eq!(a.toolchains, Some(vec!["rust".to_owned()]));
        // an empty list turns the toolchains off for this agent; it is a change, then not
        let none = AgentEdit {
            toolchains: Some(split_list("")),
            ..AgentEdit::default()
        };
        assert!(
            set(d.path(), "@coder:han.example", &none)
                .unwrap()
                .starts_with("changed: ")
        );
        assert!(
            set(d.path(), "@coder:han.example", &none)
                .unwrap()
                .starts_with("unchanged: ")
        );
    }

    #[test]
    fn finish_land_is_off_until_set_and_repeats_as_unchanged() {
        let d = tempfile::tempdir().unwrap();
        let land = FinishAction::Land {
            target_ref: Some("refs/heads/main".into()),
            tier_rules: vec!["docs/**=0".into(), "crates/core/**=3".into()],
            no_full_stage: None,
            settle_secs: Some(120),
        };
        let show = |d: &Path| finish(d, "s1:x", Some("github:o/r"), &FinishAction::Show).unwrap();
        assert!(show(d.path()).starts_with("land: off"));
        let first = finish(d.path(), "s1:x", Some("github:o/r"), &land).unwrap();
        assert!(
            first.starts_with("changed: land on for s1:x github:o/r"),
            "{first}"
        );
        assert!(
            first.contains("refs/heads/main") && first.contains("docs/**=0"),
            "{first}"
        );
        let again = finish(d.path(), "s1:x", Some("github:o/r"), &land).unwrap();
        assert!(again.starts_with("unchanged: "), "{again}");
        // the repository's own setting is in effect; the space has none
        let file = AgentsFile::load(d.path()).unwrap();
        assert!(file.land_for("s1:x", Some("github:o/r")).is_some());
        assert_eq!(file.land_for("s1:x", None), None);
        // off, then reset
        let off = finish(d.path(), "s1:x", Some("github:o/r"), &FinishAction::Off).unwrap();
        assert!(off.starts_with("changed: land off"), "{off}");
        assert!(show(d.path()).starts_with("land: off"));
        let reset = finish(d.path(), "s1:x", Some("github:o/r"), &FinishAction::Reset).unwrap();
        assert!(reset.starts_with("changed: removed"), "{reset}");
        let reset = finish(d.path(), "s1:x", Some("github:o/r"), &FinishAction::Reset).unwrap();
        assert!(reset.starts_with("unchanged: no"), "{reset}");
        assert!(AgentsFile::load(d.path()).unwrap().finish.is_empty());
    }

    #[test]
    fn finish_refuses_what_no_host_could_use() {
        let d = tempfile::tempdir().unwrap();
        let land = |target: Option<&str>, rules: &[&str]| FinishAction::Land {
            target_ref: target.map(Into::into),
            tier_rules: rules.iter().map(|r| (*r).to_owned()).collect(),
            no_full_stage: None,
            settle_secs: None,
        };
        assert!(finish(d.path(), "s1:x", None, &land(Some("main"), &[])).is_err());
        assert!(finish(d.path(), "s1:x", None, &land(None, &["docs/**"])).is_err());
        assert!(finish(d.path(), "s1:x", None, &land(None, &["docs/**=9"])).is_err());
        assert!(finish(d.path(), "s1:x", None, &land(None, &["=1"])).is_err());
        assert!(!d.path().join("agents.json").exists());
        assert_eq!(tier_rule("a=b/c=2").unwrap().path, "a=b/c");
    }

    #[test]
    fn set_refuses_what_no_launch_could_use() {
        let d = tempfile::tempdir().unwrap();
        let bad = AgentEdit {
            sandbox: Some("chroot".into()),
            ..AgentEdit::default()
        };
        assert!(set(d.path(), "@coder:han.example", &bad).is_err());
        assert!(set(d.path(), "coder", &bad).is_err());
        assert!(set(d.path(), "@coder:han.example", &AgentEdit::default()).is_err());
        assert!(!d.path().join("agents.json").exists());
    }
}

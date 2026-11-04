//! Guided local runner setup. The journal contains choices and confirmed progress, never credentials.

use std::fs::File;
use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use clap::Parser as _;
use han_node::api::{Command, NodeApi, Query, Row, SpaceLink, ViewEvent, Window};
use han_node::rpc::{RpcClient, default_endpoint};
use han_proc::NoWindow as _;
use serde::{Deserialize, Serialize};

use super::{Res, State};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    agent_name: String,
    harness: String,
    repo: String,
    target_ref: String,
    checks: Vec<String>,
    app_id: u64,
    installation_id: u64,
    key_source: PathBuf,
    read_app: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Flow {
    version: u32,
    space: String,
    plan: Option<Plan>,
    executor_enabled: bool,
    #[serde(default)]
    generation: u64,
    #[serde(default)]
    configuration: Option<serde_json::Value>,
}

fn load(data: &Path) -> Res<Option<Flow>> {
    let bytes = match std::fs::read(data.join("workflow.json")) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let flow: Flow = serde_json::from_slice(&bytes)?;
    if flow.version != 1
        || !han_core::grammar::is_space_id(&flow.space)
        || (flow.executor_enabled && (flow.plan.is_none() || flow.configuration.is_none()))
    {
        return Err(
            "invalid setup progress; preserve workflow.json and inspect it before resuming".into(),
        );
    }
    if let Some(plan) = &flow.plan {
        validate_plan(plan)?;
    }
    Ok(Some(flow))
}

fn validate_plan(plan: &Plan) -> Res<()> {
    if !han_core::grammar::is_actor_id(&format!("@{}:setup.example", plan.agent_name))
        || !han_registry::agents::valid_harness_name(&plan.harness)
        || !matches!(plan.repo.strip_prefix("github:").and_then(|r| r.split_once('/')), Some((o, r)) if !o.is_empty() && !r.is_empty() && !r.contains('/') && o.chars().chain(r.chars()).all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)))
        || !plan.target_ref.starts_with("refs/heads/")
        || plan.target_ref.contains(char::is_whitespace)
        || plan.checks.is_empty()
        || plan
            .checks
            .iter()
            .any(|c| c.is_empty() || c.contains(char::is_whitespace))
        || plan.app_id == 0
        || plan.installation_id == 0
        || plan.read_app == Some(plan.app_id)
        || !plan.key_source.is_absolute()
    {
        return Err("invalid saved runner choices; inspect workflow.json before resuming".into());
    }
    let land = han_registry::LandSetting {
        target_ref: Some(plan.target_ref.clone()),
        ..Default::default()
    };
    land.check()?;
    Ok(())
}

fn configuration(data: &Path, state: &State, flow: &Flow) -> Res<serde_json::Value> {
    let plan = flow.plan.as_ref().ok_or("missing runner choices")?;
    validate_plan(plan)?;
    let agents = han_registry::AgentsFile::load(&data.join("person"))?;
    let actor = format!("@{}:{}", plan.agent_name, state.server);
    let agent = agents.agent(&actor);
    if agent.os_user.is_some()
        || agent.state_dir.is_some()
        || agent.model_key_file.is_some()
        || agent.allow_advisory
        || agents.sandbox_of(&actor) != "bwrap"
        || agent.harness.as_deref() != Some(plan.harness.as_str())
    {
        return Err(
            "agent settings differ from this guided local runner; inspect them before resuming"
                .into(),
        );
    }
    let exec: han_exec::ExecConfig =
        serde_json::from_slice(&std::fs::read(data.join("executor/exec.json"))?)?;
    Ok(serde_json::json!({"executor": exec, "agent": agent,
        "land": agents.land_for(&flow.space, Some(&plan.repo)),
        "toolchains": agents.toolchains_of(&actor)}))
}

fn verify_configuration(data: &Path, state: &State, flow: &Flow) -> Res<()> {
    let expected = flow
        .configuration
        .as_ref()
        .ok_or("runner configuration was not confirmed")?;
    if &configuration(data, state, flow)? != expected {
        return Err("runner configuration changed after setup; inspect it before resuming (setup will not overwrite it)".into());
    }
    Ok(())
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct Runtime {
    pub generation: u64,
    pub ready: bool,
    pub detail: String,
}

fn save(data: &Path, flow: &Flow) -> Res<()> {
    let temp = data.join("workflow.json.tmp");
    let mut file = File::create(&temp)?;
    file.write_all(&serde_json::to_vec_pretty(flow)?)?;
    file.sync_all()?;
    std::fs::rename(temp, data.join("workflow.json"))?;
    Ok(())
}

fn prompt(label: &str, default: &str) -> Res<String> {
    if !std::io::stdin().is_terminal() {
        return Err(
            "runner setup needs an interactive terminal; resume with the same han setup command"
                .into(),
        );
    }
    print!(
        "{label}{}: ",
        if default.is_empty() {
            String::new()
        } else {
            format!(" [{default}]")
        }
    );
    std::io::stdout().flush()?;
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line)? == 0 {
        return Err("setup input ended; confirmed progress is retained".into());
    }
    Ok(if line.trim().is_empty() {
        default.to_owned()
    } else {
        line.trim().to_owned()
    })
}

async fn snapshot(client: &dyn NodeApi, query: Query) -> Res<Vec<Row>> {
    tokio::time::timeout(Duration::from_secs(10), async {
        match client.view(query).next().await {
            Some(ViewEvent::Snapshot { rows, .. }) => Ok(rows),
            Some(ViewEvent::Failed { error, .. }) => Err(error.into()),
            _ => Err("the node did not return a confirmed view; resume after it reconnects".into()),
        }
    })
    .await
    .map_err(|_| "the node did not answer within 10 seconds")?
}

async fn verify_space(client: &dyn NodeApi, state: &State, flow: &Flow) -> Res<Vec<String>> {
    client
        .call(Command::SpaceAwait {
            space: flow.space.clone(),
            timeout_ms: Some(15_000),
        })
        .await?;
    let node = snapshot(
        client,
        Query::NodeStatus {
            window: Window::default(),
        },
    )
    .await?;
    if !node.iter().any(|row| matches!(row, Row::NodeStatus(n) if n.actor == state.actor && n.outbox_pending == 0 && n.problems.is_empty())) {
        return Err("this setup's person node is not ready or has pending writes; inspect han doctor and the outbox before resuming".into());
    }
    let rows = snapshot(
        client,
        Query::SpaceStatus {
            space: flow.space.clone(),
            window: Window::default(),
        },
    )
    .await?;
    let space = rows
        .into_iter()
        .find_map(|row| match row {
            Row::SpaceStatus(s) => Some(s),
            _ => None,
        })
        .ok_or("the confirmed space is not followed; inspect it before resuming")?;
    if space.home != state.server
        || han_core::grammar::server_of(&flow.space) != Some(state.server.as_str())
    {
        return Err("this space belongs to another home. Remote runner pairing is not available yet; use its prepared runner or continue with the advanced setup commands".into());
    }
    if space.link != SpaceLink::Live || space.outbox_pending != 0 {
        return Err("the space is not live or still has pending writes; wait for delivery before resuming setup".into());
    }
    let access = space.trusted_setup.filter(|s| s.can_manage)
        .ok_or("guided runner setup needs current ownership and the unchanged Trusted team preset; inspect membership and policy or use advanced setup")?;
    if flow
        .plan
        .as_ref()
        .is_some_and(|plan| !access.repos.contains(&plan.repo))
    {
        return Err("the saved repository is no longer selected by the team's policy; setup will not restore that grant".into());
    }
    Ok(access.repos)
}

fn canonical_harness(person: &Path, name: &str) -> Res<String> {
    let driver = crate::harness::driver(person, name)?;
    if driver.login().is_none() {
        return Err(
            "this harness uses API-key configuration; use the advanced agent setup path for it"
                .into(),
        );
    }
    Ok(driver.name)
}

fn choose_plan(data: &Path, state: &State, repos: &[String]) -> Res<Plan> {
    println!("Run an agent on this Linux machine, inside the enforced bubblewrap sandbox.");
    let agent_name = prompt("Agent name", "coder")?;
    if !han_core::grammar::is_actor_id(&format!("@{agent_name}:{}", state.server)) {
        return Err("invalid short agent name".into());
    }
    let harness = canonical_harness(&data.join("person"), &prompt("Harness name", "codex")?)?;
    println!("Repositories selected by this team's policy:");
    for (i, repo) in repos.iter().enumerate() {
        println!("  {}. {repo}", i + 1);
    }
    let index: usize = prompt("Repository number", "1")?.parse()?;
    let repo = repos
        .get(
            index
                .checked_sub(1)
                .ok_or("repository numbers start at 1")?,
        )
        .ok_or("choose a listed repository")?
        .clone();
    let slug = repo.strip_prefix("github:").ok_or(
        "guided checks currently use GitHub repositories; use advanced setup for another backend",
    )?;
    if !matches!(slug.split_once('/'), Some((owner, name)) if !owner.is_empty() && !name.is_empty() && !name.contains('/'))
    {
        return Err("repository must be github:owner/name".into());
    }
    let target_ref = prompt("Landing branch (full ref)", "refs/heads/main")?;
    let checks: Vec<String> = prompt("Required GitHub check names (comma separated)", "")?
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    if checks.is_empty() || checks.iter().any(|s| s.contains(char::is_whitespace)) {
        return Err("choose at least one required check name without spaces; missing or red checks will block landing".into());
    }
    println!(
        "Use a GitHub App installed on this repository with the executor's write permissions. Keep it separate from any read-only HAN App."
    );
    let app_id = prompt("Write App ID", "")?.parse()?;
    let installation_id = prompt("Write App installation ID", "")?.parse()?;
    let key_source = std::path::absolute(prompt(
        "Private PEM key file (contents are never displayed)",
        "",
    )?)?;
    let read = prompt("Read-only App ID, if you use one (blank for none)", "")?;
    let read_app = if read.is_empty() {
        None
    } else {
        Some(read.parse()?)
    };
    if read_app == Some(app_id) {
        return Err("the executor must use a different App from the read-only integration".into());
    }
    Ok(Plan {
        agent_name,
        harness,
        repo,
        target_ref,
        checks,
        app_id,
        installation_id,
        key_source,
        read_app,
    })
}

async fn configure_executor(data: &Path, state: &State, flow: &Flow, plan: &Plan) -> Res<()> {
    let dir = data.join("executor");
    let installed_key = dir.join(crate::executor::GITHUB_KEY_FILE);
    let key = if installed_key.is_file() {
        installed_key
    } else {
        plan.key_source.clone()
    };
    let ticket = std::fs::read_to_string(data.join("home").join(crate::seq::TICKET_FILE))?;
    let slug = plan
        .repo
        .strip_prefix("github:")
        .ok_or("expected a GitHub repository")?;
    let mut argv = vec![
        "han".into(),
        "executor".into(),
        "add".into(),
        "setup-exec".into(),
        "--operator".into(),
        state.actor.clone(),
        "--dir".into(),
        dir.to_string_lossy().into_owned(),
        "--space".into(),
        flow.space.clone(),
        "--home-data".into(),
        data.join("home").to_string_lossy().into_owned(),
        "--data".into(),
        data.join("person").to_string_lossy().into_owned(),
        "--home".into(),
        ticket.trim().into(),
        "--relay".into(),
        state.relay.clone(),
        "--repo".into(),
        format!("{}=https://github.com/{slug}.git", plan.repo),
        "--github-app".into(),
        plan.app_id.to_string(),
        "--github-installation".into(),
        plan.installation_id.to_string(),
        "--github-key".into(),
        key.to_string_lossy().into_owned(),
    ];
    if let Some(id) = plan.read_app {
        argv.extend(["--read-app".into(), id.to_string()]);
    }
    let crate::Cmd::Executor(args) = crate::Cli::try_parse_from(argv)?.command else {
        unreachable!()
    };
    let outcome = crate::executor::provision(args).await?;
    if !outcome.todo.is_empty() {
        return Err(format!(
            "executor enrollment is incomplete: {}",
            outcome.todo.join("; ")
        )
        .into());
    }
    let crate::Cmd::Executor(args) = crate::Cli::try_parse_from([
        "han",
        "executor",
        "set",
        &plan.repo,
        "--dir",
        &dir.to_string_lossy(),
        "--landing-refs",
        &plan.target_ref,
        "--required",
        &plan.checks.join(","),
        "--full",
        &plan.checks.join(","),
    ])?
    .command
    else {
        unreachable!()
    };
    crate::executor::provision(args).await?;
    let crate::Cmd::Agent(args) = crate::Cli::try_parse_from([
        "han",
        "agent",
        "--data",
        &data.join("person").to_string_lossy(),
        "finish",
        &flow.space,
        "--repo",
        &plan.repo,
        "land",
        "--target-ref",
        &plan.target_ref,
    ])?
    .command
    else {
        unreachable!()
    };
    crate::agent::run(args).await?;
    let facts = crate::doctor::gather(
        &data.join("person"),
        &default_endpoint(&data.join("person")),
        &[],
        Some(&dir),
        None,
        true,
        true,
        None,
        false,
    )
    .await;
    let problems = crate::doctor::analyze(&facts);
    if !problems.is_empty() {
        for p in &problems {
            eprintln!("{}: {}\n  {}", p.subject, p.cause, p.fix);
        }
        return Err(
            "executor checks are not ready; correct the listed problems and resume setup".into(),
        );
    }
    Ok(())
}

async fn accept_space(
    client: &dyn NodeApi,
    data: &Path,
    state: &State,
    flow: &Flow,
    new_flow: bool,
) -> Res<Vec<String>> {
    let repos = verify_space(client, state, flow).await?;
    // A rejected existing-team selection must not strand subsequent resumes in this journal.
    if new_flow {
        save(data, flow)?;
    }
    let config_path = data.join("person/tui.json");
    let mut config = han_tui::config::Config::load(&config_path);
    if config.setup_recovery.take().is_some() {
        config.save(&config_path)?;
    }
    Ok(repos)
}

pub(super) async fn continue_setup(data: &Path, state: &State) -> Res<()> {
    let _lock = super::lock_file(&data.join("workflow.lock"))?;
    let saved = load(data)?;
    let new_flow = saved.is_none();
    let mut flow = match saved {
        Some(flow) => flow,
        None => {
            let han_tui::SetupExit::Continue { space } = han_tui::run_setup(han_tui::Options {
                space: None,
                data: data.join("person"),
                endpoint: None,
            })
            .await?
            else {
                return Ok(());
            };
            Flow {
                version: 1,
                space,
                plan: None,
                executor_enabled: false,
                generation: 0,
                configuration: None,
            }
        }
    };
    if !cfg!(target_os = "linux") {
        return Err("guided local agent setup currently requires Linux and bubblewrap. Your team is saved; remote runner pairing is not available yet. Use a prepared Linux runner with the advanced commands".into());
    }
    let client = RpcClient::connect(&default_endpoint(&data.join("person"))).await?;
    let repos = accept_space(&client, data, state, &flow, new_flow).await?;
    han_host::sandbox::admit_for(
        "bwrap",
        false,
        false,
        &han_host::sandbox::ShimCommand::of_current_exe()?,
    )?;
    if flow.plan.is_none() {
        let plan = choose_plan(data, state, &repos)?;
        validate_plan(&plan)?;
        flow.plan = Some(plan);
        save(data, &flow)?;
    }
    let plan = flow
        .plan
        .as_ref()
        .ok_or("setup has no saved choices")?
        .clone();
    let actor = format!("@{}:{}", plan.agent_name, state.server);
    let person = data.join("person");
    if flow.executor_enabled {
        verify_configuration(data, state, &flow)?;
    } else {
        let existing = han_registry::AgentsFile::load(&person)?.agent(&actor);
        if existing.os_user.is_some()
            || existing.state_dir.is_some()
            || existing.model_key_file.is_some()
        {
            return Err(
                "this agent already has advanced settings; choose a separate local agent".into(),
            );
        }
        crate::harness::record_settings(
            &person,
            &actor,
            &han_registry::AgentEdit {
                allow_advisory: Some(false),
                ..Default::default()
            },
        )?;
        let outcome = crate::agent::add(
            &client,
            &person,
            crate::agent::Add {
                name: plan.agent_name.clone(),
                operator: state.actor.clone(),
                server: None,
                os_user: None,
                sandbox: Some("bwrap".into()),
                allow_advisory: false,
                harness: Some(plan.harness.clone()),
                model_key_file: None,
                space: Some(flow.space.clone()),
                home_data: Some(data.join("home")),
                home: None,
            },
        )
        .await?;
        if !outcome.todo.is_empty() {
            return Err(format!(
                "agent enrollment is incomplete: {}",
                outcome.todo.join("; ")
            )
            .into());
        }
        if crate::harness::login(&person, &actor, true, None, None, None).is_err() {
            crate::harness::login(&person, &actor, false, None, None, None)?;
        }
        configure_executor(data, state, &flow, &plan).await?;
        flow.configuration = Some(configuration(data, state, &flow)?);
    }
    verify_space(&client, state, &flow).await?;
    flow.executor_enabled = true;
    flow.generation = flow
        .generation
        .checked_add(1)
        .ok_or("runner retry count exhausted")?;
    save(data, &flow)?;
    let until = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let runtime = super::request(data, "status").await?.executor;
        if runtime.generation == flow.generation {
            if runtime.ready {
                break;
            }
            if runtime.detail.starts_with("failed:") {
                return Err(runtime.detail.into());
            }
        }
        if tokio::time::Instant::now() >= until {
            return Err(
                "executor readiness is unconfirmed; inspect executor.log, then resume setup".into(),
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    println!(
        "Agent and executor are ready. Enter a task; submission starts it. Required checks still gate every landing."
    );
    han_tui::run_task(
        han_tui::Options {
            space: Some(flow.space),
            data: person,
            endpoint: None,
        },
        actor,
        plan.repo,
    )
    .await?;
    Ok(())
}

/// The managed setup owns this child's input pipe. Losing the parent closes it and stops the executor.
async fn stop_executor(stop: &mut tokio::sync::watch::Receiver<bool>) {
    tokio::select! {
        _ = crate::shutdown::requested() => {}
        _ = stop.wait_for(|s| *s) => {}
    }
}

pub(super) async fn supervise_executor(
    data: &Path,
    runtime: tokio::sync::watch::Sender<Runtime>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> Res<()> {
    let mut attempted = None;
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        let flow = match load(data) {
            Ok(flow) => flow,
            Err(e) => {
                runtime.send_modify(|r| {
                    r.ready = false;
                    r.detail = format!("failed: {e}");
                });
                None
            }
        };
        if let Some(flow) = flow.filter(|f| f.executor_enabled && attempted != Some(f.generation)) {
            attempted = Some(flow.generation);
            runtime.send_replace(Runtime {
                generation: flow.generation,
                ready: false,
                detail: "starting".into(),
            });
            if let Err(e) = supervise_attempt(data, &runtime, &mut attempted, &mut stop).await {
                runtime.send_modify(|r| {
                    r.ready = false;
                    r.detail =
                        format!("failed: {e}; inspect executor.log and resume setup explicitly");
                });
                if *stop.borrow() {
                    return Err(e);
                }
            }
        }
        tokio::select! {
            biased;
            _ = stop_executor(&mut stop) => return Ok(()),
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
    }
}

async fn supervise_attempt(
    data: &Path,
    runtime: &tokio::sync::watch::Sender<Runtime>,
    attempted: &mut Option<u64>,
    stop: &mut tokio::sync::watch::Receiver<bool>,
) -> Res<()> {
    use tokio::io::AsyncBufReadExt as _;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(data.join("executor.log"))?;
    let mut child = tokio::process::Command::new(std::env::current_exe()?)
        .no_window()
        .args(["setup", "serve-executor", "--data", &data.to_string_lossy()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(log)
        .kill_on_drop(true)
        .spawn()?;
    let keep_alive = child.stdin.take();
    let mut output =
        tokio::io::BufReader::new(child.stdout.take().ok_or("missing executor output pipe")?)
            .lines();
    loop {
        tokio::select! {
            biased;
            _ = stop_executor(stop) => break,
            status = child.wait() => return Err(format!("managed executor stopped ({})", status?).into()),
            line = output.next_line() => match line? {
                Some(line) if line == "HAN_SETUP_EXECUTOR_READY" => { runtime.send_modify(|r| { r.ready = true; r.detail = "ready".into(); }); }
                Some(line) => eprintln!("executor: {line}"),
                None => return Err("executor output closed".into()),
            },
            _ = tokio::time::sleep(Duration::from_millis(250)) => {
                // A resume while healthy consumes its retry, rather than arming a future restart.
                if let Some(flow) = load(data)?.filter(|f| f.executor_enabled && runtime.borrow().ready) {
                    *attempted = Some(flow.generation);
                    runtime.send_modify(|r| r.generation = flow.generation);
                }
            }
        }
    }
    runtime.send_modify(|r| {
        r.ready = false;
        r.detail = "stopping".into();
    });
    drop(keep_alive);
    let status = tokio::time::timeout(Duration::from_secs(9), child.wait())
        .await
        .map_err(|_| "executor shutdown exceeded 9 seconds; state is retained")??;
    if !status.success() {
        return Err(format!("executor shutdown failed ({status}); inspect retained state").into());
    }
    Ok(())
}

pub(super) async fn run_executor(data: &Path) -> Res<()> {
    let flow = load(data)?
        .filter(|f| f.executor_enabled)
        .ok_or("this setup has no enabled executor")?;
    let state = super::load(data)?;
    std::thread::spawn(|| {
        use std::io::Read as _;
        let mut byte = [0u8; 1];
        while std::io::stdin().read(&mut byte).is_ok_and(|n| n > 0) {}
        crate::shutdown::request();
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while !super::node_ready(data, &state.actor).await {
        if tokio::time::Instant::now() >= deadline {
            return Err("person node was not ready within 60 seconds".into());
        }
        tokio::select! {
            biased;
            _ = crate::shutdown::requested() => return Ok(()),
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }
    let client = RpcClient::connect(&default_endpoint(&data.join("person"))).await?;
    tokio::select! {
        biased;
        _ = crate::shutdown::requested() => return Ok(()),
        r = verify_space(&client, &state, &flow) => { r?; }
    }
    verify_configuration(data, &state, &flow)?;
    let ticket = std::fs::read_to_string(data.join("home").join(crate::seq::TICKET_FILE))?;
    let dir = data.join("executor");
    let crate::Cmd::Exec(args) = crate::Cli::try_parse_from([
        "han",
        "exec",
        "--actor",
        &format!("@setup-exec:{}", state.server),
        "--space",
        &flow.space,
        "--home",
        ticket.trim(),
        "--keys",
        &dir.to_string_lossy(),
        "--data",
        &dir.join("state").to_string_lossy(),
        "--config",
        &dir.join("exec.json").to_string_lossy(),
        "--relay",
        &state.relay,
    ])?
    .command
    else {
        unreachable!()
    };
    let (ready, received) = tokio::sync::oneshot::channel();
    let run = Box::pin(crate::exec::run_with_ready(args, Some(ready)));
    tokio::pin!(run);
    tokio::select! {
        result = &mut run => return result,
        result = received => { result.map_err(|_| "executor stopped before readiness")?; }
    }
    println!("HAN_SETUP_EXECUTOR_READY");
    std::io::stdout().flush()?;
    run.await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejected_existing_team_keeps_recovery_and_does_not_save_a_workflow() {
        use han_node::api::{SpaceStatusRow, TrustedSetup};
        use han_tui::testing::{FakeApi, node_status, snapshot};
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("person/tui.json");
        let config = han_tui::config::Config {
            setup_recovery: Some("inspect the original request".into()),
            ..Default::default()
        };
        config.save(&config_path).unwrap();
        let state = State {
            version: 1,
            actor: "@alice:local.example".into(),
            server: "local.example".into(),
            file_keys: true,
            relay: "none".into(),
            public_key: String::new(),
        };
        for (home, can_manage, accepted) in [
            ("remote.example", true, false),
            ("local.example", false, false),
            ("local.example", true, true),
        ] {
            let api = FakeApi::new();
            let space = format!("team:{home}");
            let row = SpaceStatusRow {
                key: space.clone(),
                space: space.clone(),
                home: home.into(),
                link: SpaceLink::Live,
                head: 1,
                sessions: 0,
                members: 1,
                outbox_pending: 0,
                signals: 0,
                error: None,
                trusted_setup: Some(TrustedSetup {
                    can_manage,
                    repos: vec!["github:o/r".into()],
                }),
            };
            api.serve(move |query| {
                Some(snapshot(
                    match query {
                        Query::NodeStatus { .. } => vec![node_status("@alice:local.example")],
                        Query::SpaceStatus { .. } => vec![Row::SpaceStatus(row.clone())],
                        _ => vec![],
                    },
                    1,
                    0,
                    None,
                ))
            });
            let flow = Flow {
                version: 1,
                space,
                plan: None,
                executor_enabled: false,
                generation: 0,
                configuration: None,
            };
            let result = accept_space(api.as_ref(), dir.path(), &state, &flow, true).await;
            assert_eq!(result.is_ok(), accepted, "{result:?}");
            if accepted {
                assert_eq!(load(dir.path()).unwrap().unwrap().space, flow.space);
                assert!(
                    han_tui::config::Config::load(&config_path)
                        .setup_recovery
                        .is_none()
                );
            } else {
                assert!(!dir.path().join("workflow.json").exists());
                assert_eq!(han_tui::config::Config::load(&config_path), config);
            }
            assert!(
                api.calls()
                    .iter()
                    .all(|c| matches!(c, Command::SpaceAwait { .. })),
                "acceptance performs no provisioning mutation"
            );
        }
    }

    fn plan() -> Plan {
        Plan {
            agent_name: "coder".into(),
            harness: "codex".into(),
            repo: "github:o/r".into(),
            target_ref: "refs/heads/main".into(),
            checks: vec!["ci".into()],
            app_id: 1,
            installation_id: 2,
            key_source: std::path::absolute("app.pem").unwrap(),
            read_app: Some(3),
        }
    }

    #[test]
    fn journal_retains_confirmed_team_and_requires_valid_choices_before_resume() {
        let dir = tempfile::tempdir().unwrap();
        let mut flow = Flow {
            version: 1,
            space: "s1:setup.example".into(),
            plan: None,
            executor_enabled: false,
            generation: 0,
            configuration: None,
        };
        save(dir.path(), &flow).unwrap();
        assert_eq!(load(dir.path()).unwrap().unwrap().space, flow.space);
        flow.executor_enabled = true;
        save(dir.path(), &flow).unwrap();
        assert!(load(dir.path()).is_err());
        flow.plan = Some(plan());
        flow.configuration = Some(serde_json::json!({}));
        save(dir.path(), &flow).unwrap();
        assert!(load(dir.path()).unwrap().unwrap().executor_enabled);
        flow.plan.as_mut().unwrap().checks.clear();
        save(dir.path(), &flow).unwrap();
        assert!(load(dir.path()).is_err());
        assert!(dir.path().join("workflow.json").exists());
    }

    #[test]
    fn saved_plan_validation_rejects_unsafe_or_unsupported_targets() {
        for mutate in [
            |p: &mut Plan| p.harness = "../driver".into(),
            |p: &mut Plan| p.repo = "github:o/r/extra".into(),
            |p: &mut Plan| p.target_ref = "main".into(),
            |p: &mut Plan| p.checks = vec!["".into()],
            |p: &mut Plan| p.read_app = Some(p.app_id),
            |p: &mut Plan| p.key_source = "relative.pem".into(),
        ] {
            let mut p = plan();
            mutate(&mut p);
            assert!(validate_plan(&p).is_err());
        }
        assert!(validate_plan(&plan()).is_ok());
    }

    #[test]
    fn resume_refuses_configuration_changes_without_rewriting_them() {
        let dir = tempfile::tempdir().unwrap();
        let state = State {
            version: 1,
            actor: "@me:setup.example".into(),
            server: "setup.example".into(),
            file_keys: true,
            relay: "none".into(),
            public_key: "unused".into(),
        };
        let mut flow = Flow {
            version: 1,
            space: "s1:setup.example".into(),
            plan: Some(plan()),
            executor_enabled: true,
            generation: 1,
            configuration: None,
        };
        let person = dir.path().join("person");
        std::fs::create_dir_all(&person).unwrap();
        std::fs::create_dir_all(dir.path().join("executor")).unwrap();
        let mut agents = han_registry::AgentsFile::load(&person).unwrap();
        agents
            .set(
                "@coder:setup.example",
                &han_registry::AgentEdit {
                    sandbox: Some("bwrap".into()),
                    harness: Some("codex".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        agents.save(&person).unwrap();
        let config_path = dir.path().join("executor/exec.json");
        std::fs::write(
            &config_path,
            serde_json::to_vec(&han_exec::ExecConfig::default()).unwrap(),
        )
        .unwrap();
        flow.configuration = Some(configuration(dir.path(), &state, &flow).unwrap());
        verify_configuration(dir.path(), &state, &flow).unwrap();
        let changed = han_exec::ExecConfig {
            http_hosts: vec!["example.com".into()],
            ..Default::default()
        };
        let bytes = serde_json::to_vec(&changed).unwrap();
        std::fs::write(&config_path, &bytes).unwrap();
        assert!(verify_configuration(dir.path(), &state, &flow).is_err());
        assert_eq!(std::fs::read(&config_path).unwrap(), bytes);
        agents
            .set(
                "@coder:setup.example",
                &han_registry::AgentEdit {
                    allow_advisory: Some(true),
                    ..Default::default()
                },
            )
            .unwrap();
        agents.save(&person).unwrap();
        assert!(configuration(dir.path(), &state, &flow).is_err());
    }

    #[test]
    fn harness_alias_is_saved_as_the_name_agent_enrollment_uses() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            canonical_harness(dir.path(), "claude-code").unwrap(),
            "claude"
        );
        assert_eq!(canonical_harness(dir.path(), "codex").unwrap(), "codex");
        assert!(canonical_harness(dir.path(), "pi").is_err());
    }

    #[test]
    fn workflow_lock_excludes_concurrent_launchers() {
        let dir = tempfile::tempdir().unwrap();
        let lock = super::super::lock_file(&dir.path().join("workflow.lock")).unwrap();
        assert!(super::super::lock_file(&dir.path().join("workflow.lock")).is_err());
        drop(lock);
        assert!(super::super::lock_file(&dir.path().join("workflow.lock")).is_ok());
    }
}

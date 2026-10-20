//! `han host`: one agent session (PLAN §3). The node's supervisor starts it for `AgentStart`; it runs as the
//! agent's own node (the agent's key is its node id), drives the harness over ACP inside the sandbox, and
//! publishes status and timeline items until the work stops.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use clap::Args;
use han_host::driver::{DriverConfig, builtin};
use han_host::host::{HostConfig, HostDeps, RepoConfig, SandboxChoice, run_session};
use han_host::sandbox::{self, SandboxError, ShimCommand};
use han_host::toolchain;
use han_node::api::ConnectionClass;
use han_registry::{
    HarnessSpec, HostAdmission, HostSpec, RefusalKind, unit_admission_file, unit_spec_file,
};
use han_vcs::GitVcs;

use crate::rolenode;

/// Options of `han host`.
#[derive(Args)]
pub struct HostArgs {
    /// The host spec (JSON, a `HostSpec`): a file, or `-` for standard input. The node writes it; it is
    /// everything the host needs.
    #[arg(long)]
    spec: String,
}

fn driver(h: &HarnessSpec) -> Result<DriverConfig, Box<dyn std::error::Error>> {
    // A driver file named by the spec stands in for the built-in; else the name is a built-in, else a path.
    if h.driver.is_none()
        && let Some(d) = builtin(&h.name)
    {
        return Ok(d);
    }
    let path = h.driver.as_deref().unwrap_or(Path::new(&h.name));
    Ok(DriverConfig::load(path)?)
}

fn repo(spec: Option<&str>, base: &str, default_branch: &str) -> RepoConfig {
    let spec = spec.unwrap_or(".");
    let source = match spec.strip_prefix("github:") {
        Some(r) => format!("https://github.com/{r}.git"),
        None => spec.to_owned(),
    };
    RepoConfig {
        name: spec.to_owned(),
        source,
        base: base.to_owned(),
        default_branch: default_branch.to_owned(),
    }
}

/// The sandbox, checked in this process: the place the agent will run. A host in its own `han-agent@` unit is
/// the only one the node could not check for it (the node's unit forbids namespaces), so it says what it found
/// in its admission file, which the node reads for its start, its status and its problems. Whatever the answer,
/// no agent has started yet; on a refusal none does.
fn admit_sandbox(args: &HostArgs, spec: &HostSpec) -> Result<(), Box<dyn std::error::Error>> {
    // The unit reads `<state>/spawn/host.json`; the node looks for the answer beside it.
    let unit_file = Path::new(&args.spec);
    let in_unit = unit_file.file_name() == unit_spec_file(Path::new("")).file_name();
    let shim = ShimCommand::of_current_exe()?;
    let found = sandbox::gate(&spec.sandbox.backend, spec.sandbox.allow_advisory, &shim);
    if in_unit {
        let admission = match &found {
            Ok(()) => HostAdmission::Admitted,
            Err(e) => HostAdmission::Refused {
                kind: match e {
                    SandboxError::LoopbackBlocked(_) => RefusalKind::LoopbackBlocked,
                    _ => RefusalKind::SandboxUnavailable,
                },
                message: e.to_string(),
            },
        };
        if let Err(e) = admission.write(
            &unit_file.with_file_name(
                unit_admission_file(Path::new(""))
                    .file_name()
                    .unwrap_or_default(),
            ),
        ) {
            // The node then waits in vain and the host's exit still says no: never start on a guess.
            eprintln!("han host: cannot leave its admission: {e}");
        }
    }
    Ok(found?)
}

pub async fn run(args: HostArgs) -> Result<(), Box<dyn std::error::Error>> {
    let spec = HostSpec::read(&args.spec)?;
    admit_sandbox(&args, &spec)?;
    let driver = driver(&spec.harness)?;
    let mut tools = spec.toolchains.clone();
    toolchain::validate(&tools.enabled)?;
    // Per agent: a cache in its own state directory, shared by its sessions.
    tools
        .cargo_home
        .get_or_insert_with(|| spec.dirs.state.join("cargo-home"));
    // The agent's own login must exist before a session opens: it is the one thing of the user's that the
    // sandbox gets (`han agent login <agent>`).
    han_host::driver::login_binding(&driver, Some(&spec.dirs.login))?;
    let key_name = rolenode::key_name(&spec.agent);
    let role = rolenode::start(
        &spec.agent,
        &spec.key_id,
        &key_name,
        &spec.dirs.keys,
        &spec.dirs.state.join("node"),
        &spec.relay,
        &spec.home,
        &spec.space,
        ConnectionClass::Host,
    )
    .await?;
    // API-key mode: the real key stays in the proxy (SR-33, SR-39); the agent sees a dummy.
    let model_key = match (&driver.credentials, &spec.proxy.model_key_file) {
        (han_host::driver::CredentialMode::ApiKey { .. }, Some(f)) => {
            Some(std::fs::read_to_string(f)?.trim().to_owned())
        }
        _ => None,
    };
    let seed = rolenode::key_store(&spec.dirs.keys, &key_name)?
        .load(han_node::keys::DEVICE_KEY)?
        .ok_or("the agent's key is missing")?;
    let session_dir = spec.dirs.state.join("sessions").join(
        spec.work
            .issue
            .as_deref()
            .unwrap_or("adhoc")
            .replace(['/', '#', ':'], "_"),
    );
    let executor = match &spec.work.executor {
        Some(e) => e.clone(),
        None => format!("@exec:{}", rolenode::server_of(&spec.home)?),
    };
    let cfg = HostConfig {
        space: spec.space.clone(),
        agent: spec.agent.clone(),
        operator: spec.operator.clone(),
        driver,
        repo: repo(
            spec.work.repo.as_deref(),
            &spec.work.base,
            &spec.work.default_branch,
        ),
        issue: spec.work.issue.clone(),
        task: spec.work.task.clone(),
        executor,
        session_dir,
        taint_db: spec.dirs.state.join("taint.db"),
        guarded: spec.work.guarded,
        strict_on_read: true,
        sandbox: SandboxChoice {
            backend: spec.sandbox.backend.clone(),
            allow_advisory: spec.sandbox.allow_advisory,
            ro: Vec::new(),
            login_dir: Some(spec.dirs.login.clone()),
            allow_hosts: spec.proxy.allow_hosts.clone(),
            toolchains: tools,
        },
        shim: ShimCommand::of_current_exe()?,
        node: spec.harness.node.clone().unwrap_or_else(|| "node".into()),
        adapter_dir: spec.harness.adapter_dir.clone().unwrap_or_default(),
        model_key,
        budget: spec.budget.clone(),
        turn_timeout: Duration::from_secs(30 * 60),
        exclusion: Vec::new(),
        finish: spec.work.finish.clone(),
    };
    // The operator names the repository, never the agent: a local path (a dev stack, a test remote) may be
    // cloned from disk; anything else may only be fetched over https.
    let local = spec
        .work
        .repo
        .as_deref()
        .is_some_and(|r| !r.starts_with("github:") && !r.starts_with("https://"));
    let protocols: &[&str] = if local {
        &["https", "file"]
    } else {
        &["https"]
    };
    let deps = HostDeps {
        api: role.api.clone(),
        vcs: Arc::new(
            GitVcs::new(spec.dirs.state.join("objects")).with_protocols(protocols.iter().copied()),
        ),
        signer: Some((spec.agent.clone(), spec.key_id.clone(), seed)),
        proxy_hook: None,
    };
    let (stop, rx) = tokio::sync::watch::channel(false);
    let runner = run_session(cfg, deps, rx);
    tokio::pin!(runner);
    let end = tokio::select! {
        r = &mut runner => r,
        _ = stop_signal() => {
            let _ = stop.send(true);
            runner.await
        }
    };
    role.node.shutdown();
    role.transport.shutdown().await;
    let end = end?;
    match &end.reason {
        Some(why) => {
            println!("session {} ended {:?}: {why}", end.session, end.state);
        }
        None => println!("session {} ended {:?}", end.session, end.state),
    }
    Ok(())
}

/// Resolves when the host is asked to stop: Ctrl-C (SIGINT, what the units send), or SIGTERM from a plain `kill`
/// or a container runtime. Either way the session is closed on the way out; a host that is killed outright
/// (SIGKILL, a stop timeout) cannot, and the node marks its session ended instead.
async fn stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

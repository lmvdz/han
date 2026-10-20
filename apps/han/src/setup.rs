//! Resumable local home and person-node bootstrap, using the normal production roles.
//! Permissions remain on their normal presence-gated face connection.

use std::fs::{File, OpenOptions};
use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand};
use han_node::api::{NodeApi, Query, Row, ViewEvent, Window};
use han_node::keys::{DEVICE_KEY, FileKeyStore, KeyStore, KeyringStore};
use han_node::rpc::{RpcClient, connect_local, default_endpoint, listen_local};
use han_proc::NoWindow as _;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// Options for the guided local bootstrap.
#[derive(Args)]
pub struct SetupArgs {
    #[command(subcommand)]
    cmd: Option<SetupCmd>,
    /// Folder for the home, person node and resumable setup record.
    #[arg(long, default_value = "han-team", global = true)]
    data: PathBuf,
    /// Your short actor name; prompted on a fresh interactive setup.
    #[arg(long)]
    person: Option<String>,
    /// Home server name; defaults to a unique reserved local name.
    #[arg(long)]
    server: Option<String>,
    /// Use private key files instead of this device's OS keychain (headless machines).
    #[arg(long)]
    file_keys: bool,
    /// Relay configuration for a fresh setup: n0, none, or relay URLs.
    #[arg(long)]
    relay: Option<String>,
    /// Start the services and print how to open the TUI, without opening it now.
    #[arg(long)]
    no_tui: bool,
}

#[derive(Subcommand)]
enum SetupCmd {
    /// Show whether this setup's home and person node are ready.
    Status,
    /// Stop this setup's home and node, keeping all state for the next start.
    Stop,
    /// Run both roles in one managed process (used by the launcher).
    #[command(hide = true)]
    Serve,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u32,
    actor: String,
    server: String,
    file_keys: bool,
    relay: String,
    public_key: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Status {
    actor: String,
    ready: bool,
    stopping: bool,
}

fn load(data: &Path) -> Res<State> {
    let state: State = serde_json::from_slice(&std::fs::read(data.join("setup.json"))?)?;
    if state.version != 1
        || !han_core::grammar::is_actor_id(&state.actor)
        || han_core::grammar::server_of(&state.actor) != Some(state.server.as_str())
    {
        return Err(
            "invalid or unsupported setup record; leave it intact and use another directory".into(),
        );
    }
    Ok(state)
}

fn control_endpoint(data: &Path) -> String {
    #[cfg(windows)]
    {
        default_endpoint(&data.join("setup-control"))
    }
    #[cfg(not(windows))]
    {
        data.join("setup.sock").to_string_lossy().into_owned()
    }
}

fn lock_file(path: &Path) -> Res<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    file.try_lock()
        .map_err(|e| format!("{} is already in use: {e}", path.display()))?;
    Ok(file)
}

fn running(data: &Path) -> Res<bool> {
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(data.join("runtime.lock"))
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    Ok(file.try_lock().is_err())
}

fn store(data: &Path, state: &State) -> Res<()> {
    let temp = data.join("setup.json.tmp");
    let mut file = File::create(&temp)?;
    file.write_all(&serde_json::to_vec_pretty(state)?)?;
    file.sync_all()?;
    std::fs::rename(temp, data.join("setup.json"))?;
    Ok(())
}

fn keys(data: &Path, file_keys: bool) -> Box<dyn KeyStore> {
    if file_keys {
        Box::new(FileKeyStore::new(data.join("person/keys")))
    } else {
        Box::new(KeyringStore::new("han"))
    }
}

fn person_name(given: Option<&str>) -> Res<String> {
    if let Some(name) = given {
        return Ok(name.to_owned());
    }
    if !std::io::stdin().is_terminal() {
        return Err("a fresh non-interactive setup needs --person <name>".into());
    }
    print!("Your short name [you]: ");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(if input.trim().is_empty() {
        "you".into()
    } else {
        input.trim().into()
    })
}

fn prepare(data: &Path, args: &SetupArgs) -> Res<State> {
    if data.join("setup.json").exists() {
        let mut state = load(data)?;
        if args
            .person
            .as_ref()
            .is_some_and(|n| state.actor != format!("@{n}:{}", state.server))
            || args.server.as_ref().is_some_and(|s| s != &state.server)
            || args.relay.as_ref().is_some_and(|r| r != &state.relay)
            || (args.file_keys && !state.file_keys)
        {
            return Err(
                "setup options differ from the saved identity; use another --data directory".into(),
            );
        }
        prepare_key(data, &mut state)?;
        return Ok(state);
    }
    if std::fs::read_dir(data)?.any(|entry| entry.map_or(true, |e| e.file_name() != "setup.lock")) {
        return Err(
            "this directory is not an empty new setup; choose another --data directory".into(),
        );
    }
    let person = person_name(args.person.as_deref())?;
    let mut random = [0u8; 6];
    getrandom::fill(&mut random).map_err(|e| e.to_string())?;
    let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let server = args
        .server
        .clone()
        .unwrap_or_else(|| format!("team-{suffix}.han.home.arpa"));
    let actor = format!("@{person}:{server}");
    if person.is_empty() || !han_core::grammar::is_actor_id(&actor) {
        return Err("use a valid nonempty short actor name and lowercase home server name".into());
    }
    let mut state = State {
        version: 1,
        actor,
        server,
        file_keys: args.file_keys,
        relay: args.relay.clone().unwrap_or_else(|| "n0".into()),
        public_key: String::new(),
    };
    store(data, &state)?;
    prepare_key(data, &mut state)?;
    Ok(state)
}

fn prepare_key(data: &Path, state: &mut State) -> Res<()> {
    let store = keys(data, state.file_keys);
    let seed = if state.public_key.is_empty() {
        store.load_or_create(DEVICE_KEY)?
    } else {
        store
            .load(DEVICE_KEY)?
            .ok_or("the saved device key is missing; restore it before resuming")?
    };
    let public_key = han_core::SigningKey::from_seed(seed)
        .public_key()
        .to_base64url();
    if state.public_key.is_empty() {
        state.public_key = public_key;
        self::store(data, state)?;
    } else if state.public_key != public_key {
        return Err(
            "the device key no longer matches this setup; restore it before resuming".into(),
        );
    }
    Ok(())
}

async fn request(data: &Path, action: &str) -> Res<Status> {
    use interprocess::local_socket::traits::tokio::Stream as _;
    tokio::time::timeout(Duration::from_secs(3), async {
        let conn = connect_local(&control_endpoint(data)).await?;
        let (recv, mut send) = conn.split();
        send.write_all(format!("{action}\n").as_bytes()).await?;
        send.flush().await?;
        let mut line = String::new();
        BufReader::new(recv.take(4096)).read_line(&mut line).await?;
        Ok::<_, Box<dyn std::error::Error>>(serde_json::from_str(&line)?)
    })
    .await
    .map_err(|_| "setup control did not respond within 3 seconds")?
}

async fn node_ready(data: &Path, actor: &str) -> bool {
    tokio::time::timeout(Duration::from_secs(1), async {
        let Ok(client) = RpcClient::connect(&default_endpoint(&data.join("person"))).await else {
            return false;
        };
        let mut view = client.view(Query::NodeStatus {
            window: Window::default(),
        });
        matches!(view.next().await, Some(ViewEvent::Snapshot { rows, .. })
            if rows.iter().any(|r| matches!(r, Row::NodeStatus(n) if n.actor == actor)))
    })
    .await
    .unwrap_or(false)
}

async fn serve_control(data: &Path, state: &State) -> Res<()> {
    use interprocess::local_socket::traits::tokio::{Listener as _, Stream as _};
    let listener = listen_local(&control_endpoint(data))?;
    loop {
        let conn = listener.accept().await?;
        let (recv, mut send) = conn.split();
        let mut action = String::new();
        let mut reader = BufReader::new(recv.take(32));
        if !matches!(
            tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut action)).await,
            Ok(Ok(_))
        ) {
            continue;
        }
        let stopping = action.trim() == "stop";
        if !stopping && action.trim() != "status" {
            continue;
        }
        let status = Status {
            actor: state.actor.clone(),
            ready: node_ready(data, &state.actor).await,
            stopping,
        };
        let _ = send
            .write_all(format!("{}\n", serde_json::to_string(&status)?).as_bytes())
            .await;
        let _ = send.flush().await;
        if stopping {
            return Ok(());
        }
    }
}

async fn person_node(data: &Path, state: &State) -> Res<()> {
    let admin = crate::seqadmin::endpoint(&data.join("home"));
    let deadline = Instant::now() + Duration::from_secs(60);
    while connect_local(&admin).await.is_err() {
        if Instant::now() >= deadline {
            return Err("home did not open its local admin endpoint within 60 seconds".into());
        }
        tokio::select! {
            _ = crate::shutdown::requested() => return Ok(()),
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }
    crate::seqadmin::ask(
        &data.join("home"),
        &crate::seqadmin::Register {
            actor: state.actor.clone(),
            kind: "human".into(),
            key_id: "d1".into(),
            public_key: state.public_key.clone(),
            operator: None,
        },
    )
    .await?;
    let ticket = std::fs::read_to_string(data.join("home").join(crate::seq::TICKET_FILE))?;
    let mut args = vec![
        "han".into(),
        "node".into(),
        "--actor".into(),
        state.actor.clone(),
        "--data".into(),
        data.join("person").to_string_lossy().into_owned(),
        "--home".into(),
        ticket.trim().into(),
        "--relay".into(),
        state.relay.clone(),
    ];
    if state.file_keys {
        args.push("--file-keys".into());
    }
    let crate::Cmd::Node(args) = crate::Cli::try_parse_from(args)?.command else {
        unreachable!()
    };
    Box::pin(crate::node::run(*args)).await
}

async fn serve(data: &Path) -> Res<()> {
    let _lock = lock_file(&data.join("runtime.lock"))?;
    eprintln!("setup: starting managed home and node");
    let state = load(data)?;
    let seed = keys(data, state.file_keys)
        .load(DEVICE_KEY)?
        .ok_or("setup device key is missing")?;
    if han_core::SigningKey::from_seed(seed)
        .public_key()
        .to_base64url()
        != state.public_key
    {
        return Err("setup device key does not match its saved identity".into());
    }
    if connect_local(&crate::seqadmin::endpoint(&data.join("home")))
        .await
        .is_ok()
        || RpcClient::connect(&default_endpoint(&data.join("person")))
            .await
            .is_ok()
    {
        return Err("a standalone home or node is already using this setup; stop it before starting the managed setup".into());
    }
    let crate::Cmd::Seq(args) =
        crate::Cli::try_parse_from(["han", "seq", "--data", &data.join("home").to_string_lossy()])?
            .command
    else {
        unreachable!()
    };
    eprintln!("setup: identity verified; starting production roles");
    // Both roles run in this process. Stopping or losing it cannot leave an untracked child daemon.
    // Each role owns an independent task: polling both inline adds the role call stacks to
    // the launcher's main-thread stack, which is particularly small on Windows.
    let mut home = tokio::spawn(async move {
        Box::pin(crate::seq::run(*args))
            .await
            .map_err(|e| e.to_string())
    });
    let node_data = data.to_path_buf();
    let node_state = state.clone();
    let mut node = tokio::spawn(async move {
        Box::pin(person_node(&node_data, &node_state))
            .await
            .map_err(|e| e.to_string())
    });
    let grace = Duration::from_secs(10);
    tokio::select! {
        result = &mut home => {
            crate::shutdown::request();
            let _ = tokio::time::timeout(grace, &mut node).await;
            role_result(result)
        }
        result = &mut node => {
            crate::shutdown::request();
            let _ = tokio::time::timeout(grace, &mut home).await;
            role_result(result)
        }
        result = serve_control(data, &state) => {
            crate::shutdown::request();
            tokio::time::timeout(grace, async {
                let (home, node) = tokio::join!(&mut home, &mut node);
                role_result(home)?;
                role_result(node)
            }).await.map_err(|_| "setup shutdown exceeded 10 seconds; state is retained")??;
            result
        }
    }
}

fn role_result(result: Result<Result<(), String>, tokio::task::JoinError>) -> Res<()> {
    result.map_err(|e| e.to_string())?.map_err(Into::into)
}

/// Bootstraps or resumes a local home and person node, then opens the existing TUI setup flow.
pub async fn run(args: SetupArgs) -> Res<()> {
    let data = std::path::absolute(&args.data)?;
    match args.cmd {
        Some(SetupCmd::Serve) => return Box::pin(serve(&data)).await,
        Some(SetupCmd::Status) => {
            let state = load(&data)?;
            match request(&data, "status").await {
                Ok(status) => println!(
                    "{}: {}",
                    status.actor,
                    if status.ready { "ready" } else { "starting" }
                ),
                Err(_) if running(&data)? => println!(
                    "{}: starting or not responding; inspect {}",
                    state.actor,
                    data.join("setup.log").display()
                ),
                Err(_) => println!(
                    "{}: stopped (resume with han setup --data {})",
                    state.actor,
                    data.display()
                ),
            }
            return Ok(());
        }
        Some(SetupCmd::Stop) => {
            let status = request(&data, "stop").await?;
            let deadline = Instant::now() + Duration::from_secs(15);
            while running(&data)? {
                if Instant::now() >= deadline {
                    return Err("stop requested but shutdown is not confirmed; inspect setup.log before restarting".into());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            println!(
                "Stopped {}. State is kept in {}.",
                status.actor,
                data.display()
            );
            return Ok(());
        }
        None => {}
    }
    std::fs::create_dir_all(&data)?;
    let startup_lock = lock_file(&data.join("setup.lock"))?;
    let state = prepare(&data, &args)?;
    println!("Your identity: {}", state.actor);
    println!("Home and node state: {}", data.display());
    let mut started = None;
    if request(&data, "status").await.is_err() {
        if running(&data)? {
            return Err(format!(
                "the setup process is running but not responding; inspect {} before restarting",
                data.join("setup.log").display()
            )
            .into());
        }
        crate::homecmd::init(
            &data.join("home"),
            Some(state.server.clone()),
            Some("0.0.0.0:0".parse()?),
            Some("127.0.0.1:0".parse()?),
            Some(state.relay.clone()),
            Some("known".into()),
        )
        .await?;
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(data.join("setup.log"))?;
        let mut command = std::process::Command::new(std::env::current_exe()?);
        command
            .detached_service()
            .args(["setup", "serve", "--data", &data.to_string_lossy()])
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
        }
        han_proc::isolate_standard_handles()?;
        started = Some(command.spawn()?);
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if request(&data, "status")
            .await
            .is_ok_and(|s| s.ready && s.actor == state.actor)
        {
            break;
        }
        if let Some(child) = &mut started
            && let Some(status) = child.try_wait()?
        {
            return Err(format!("managed setup exited ({status}); inspect {} and rerun the same command after correcting the cause", data.join("setup.log").display()).into());
        }
        if Instant::now() >= deadline {
            return Err(format!("setup is not ready; inspect {} then rerun the same setup command (existing identity is preserved)", data.join("setup.log").display()).into());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    drop(startup_lock);
    println!("Home and person node ready. They keep running when you leave the TUI.");
    println!("Open: han tui --data {}", data.join("person").display());
    println!("Stop: han setup stop --data {}", data.display());
    println!(
        "Next: :setup creates or joins a space. Agent login and runner setup follow separately."
    );
    if !args.no_tui {
        han_tui::run_setup(han_tui::Options {
            space: None,
            data: data.join("person"),
            endpoint: None,
        })
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(data: &Path) -> SetupArgs {
        SetupArgs {
            cmd: None,
            data: data.into(),
            person: Some("alice".into()),
            server: Some("setup.han.home.arpa".into()),
            file_keys: true,
            relay: Some("none".into()),
            no_tui: true,
        }
    }

    #[test]
    fn resume_keeps_identity_and_refuses_a_lost_or_changed_key() {
        let dir = tempfile::tempdir().unwrap();
        let a = args(dir.path());
        let original = prepare(dir.path(), &a).unwrap();
        assert_eq!(
            prepare(dir.path(), &a).unwrap().public_key,
            original.public_key
        );
        std::fs::remove_file(dir.path().join("person/keys/device.key")).unwrap();
        assert!(
            prepare(dir.path(), &a)
                .unwrap_err()
                .to_string()
                .contains("missing")
        );
        assert!(!dir.path().join("person/keys/device.key").exists());
        keys(dir.path(), true).store(DEVICE_KEY, [17; 32]).unwrap();
        assert!(
            prepare(dir.path(), &a)
                .unwrap_err()
                .to_string()
                .contains("no longer matches")
        );
    }

    #[test]
    fn provisional_record_resumes_key_creation_but_unknown_directories_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let a = args(dir.path());
        std::fs::write(dir.path().join("user-file"), "keep").unwrap();
        assert!(prepare(dir.path(), &a).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("user-file")).unwrap(),
            "keep"
        );
        std::fs::remove_file(dir.path().join("user-file")).unwrap();
        let provisional = State {
            version: 1,
            actor: "@alice:setup.han.home.arpa".into(),
            server: "setup.han.home.arpa".into(),
            file_keys: true,
            relay: "none".into(),
            public_key: String::new(),
        };
        store(dir.path(), &provisional).unwrap();
        assert!(!prepare(dir.path(), &a).unwrap().public_key.is_empty());
        let mut changed = args(dir.path());
        changed.person = Some("bob".into());
        assert!(prepare(dir.path(), &changed).is_err());
    }

    #[test]
    fn runtime_lock_excludes_a_second_supervisor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime.lock");
        let first = lock_file(&path).unwrap();
        assert!(lock_file(&path).is_err());
        drop(first);
        assert!(lock_file(&path).is_ok());
    }
}

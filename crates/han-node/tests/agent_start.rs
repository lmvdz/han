//! `AgentStart` hands the host everything it needs (the harness as set up, the node, the sandbox, the home
//! ticket) and names the agent's operator from its profile, not the node's own actor.

mod common;

use std::sync::{Arc, Mutex};

use common::{World, call, pubkey, space_of};
use han_net::{HomeTicket, NodeAddr};
use han_node::agents::HarnessInstall;
use han_node::api::{CallResult, Command, NodeApi, NodeError};
use han_node::supervisor::{RoleSpec, RoleStatus, Supervisor, SupervisorError};

const ALICE: &str = "@alice:a.localhost";
const BOB: &str = "@bob:a.localhost";
const AGENT: &str = "@coder:a.localhost";

#[derive(Default)]
struct Recording(Mutex<Vec<RoleSpec>>);

impl Supervisor for Recording {
    fn start(&self, spec: RoleSpec) -> Result<(), SupervisorError> {
        self.0.lock().unwrap().push(spec);
        Ok(())
    }
    fn stop(&self, name: &str) -> Result<(), SupervisorError> {
        Err(SupervisorError::NotRunning(name.into()))
    }
    fn status(&self) -> Vec<(String, RoleStatus)> {
        Vec::new()
    }
}

/// The value after `flag` in the arguments.
fn arg<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

struct Setup {
    world: World,
    bob: han_node::Node<han_net::MemTransport>,
    sup: Arc<Recording>,
    data: std::path::PathBuf,
    space: String,
}

/// Bob's node, with a space; the agent's profile (operator: alice) is registered at the home.
async fn setup() -> Setup {
    let world = World::new(&[(ALICE, 31), (BOB, 32)]);
    world
        .home
        .register(AGENT, "agent", "k1", &pubkey(33), Some(ALICE))
        .unwrap();
    let data = world.dir.join("bob-data");
    std::fs::create_dir_all(&data).unwrap();
    let sup = Arc::new(Recording::default());
    let d = data.clone();
    let bob = world.node_supervised(BOB, 32, sup.clone(), move |c| c.data_dir = Some(d));
    let space = space_of(
        &call(
            &bob,
            Command::SpaceCreate {
                home: world.home_ref.clone(),
                name: "team".into(),
                visibility: "members".into(),
                join: "invite".into(),
            },
        )
        .await,
    );
    Setup {
        world,
        bob,
        sup,
        data,
        space,
    }
}

fn start(space: &str, harness: &str) -> Command {
    Command::AgentStart {
        space: space.into(),
        agent: AGENT.into(),
        harness: harness.into(),
        repo: Some("github:o/r".into()),
        issue: Some("github:o/r#1".into()),
    }
}

async fn follow_profile(s: &Setup) {
    call(
        &s.bob,
        Command::SpaceFollow {
            home: s.world.home_ref.clone(),
            space: format!("profile:{AGENT}"),
        },
    )
    .await;
}

/// Starts until the profile has arrived (the node fetches it in the background).
async fn start_when_known(s: &Setup, harness: &str) -> Result<CallResult, NodeError> {
    for _ in 0..500 {
        match s.bob.call(start(&s.space, harness)).await {
            Err(NodeError::Unavailable { message, .. }) if message.contains("has not arrived") => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            other => return other,
        }
    }
    panic!("the profile never arrived");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn start_names_the_profiles_operator_and_passes_the_install() {
    let s = setup().await;
    // the profile is not followed: the operator is unknown, and nothing starts
    let e = s.bob.call(start(&s.space, "pi")).await.unwrap_err();
    assert!(matches!(e, NodeError::NotFound { .. }), "{e:?}");
    follow_profile(&s).await;

    // the harness is not set up: the error says what to run
    let Err(NodeError::Unavailable { message, .. }) = start_when_known(&s, "claude").await else {
        panic!("expected unavailable")
    };
    assert!(
        message.contains("han agent setup-harness claude"),
        "{message}"
    );
    assert!(s.sup.0.lock().unwrap().is_empty());

    HarnessInstall {
        harness: "claude".into(),
        node: "/opt/node22/bin/node".into(),
        adapter_dir: "/opt/han/claude/node_modules/@agentclientprotocol/claude-agent-acp".into(),
        package: "@agentclientprotocol/claude-agent-acp".into(),
        version: "0.89.1".into(),
        integrity: String::new(),
    }
    .save(&s.data)
    .unwrap();
    std::fs::write(s.data.join("agents.json"), r#"{"sandbox":"bwrap"}"#).unwrap();
    // a home joined by ticket: the host gets the ticket in its environment
    let ticket = HomeTicket {
        server: "a.localhost".into(),
        node: NodeAddr {
            node: pubkey(200),
            addrs: vec![],
            relays: vec!["https://mem.invalid".into()],
        },
        keys: s.world.home.server_keys(),
    }
    .to_text();
    call(
        &s.bob,
        Command::HomePin {
            ticket: ticket.clone(),
        },
    )
    .await;

    start_when_known(&s, "claude").await.unwrap();
    let specs = s.sup.0.lock().unwrap();
    let spec = specs.last().unwrap();
    let a = &spec.args;
    assert_eq!(
        arg(a, "--operator"),
        Some(ALICE),
        "the profile's operator, not {BOB}"
    );
    assert_eq!(arg(a, "--harness"), Some("claude"));
    assert_eq!(arg(a, "--node"), Some("/opt/node22/bin/node"));
    assert!(
        arg(a, "--adapter-dir")
            .unwrap()
            .ends_with("claude-agent-acp")
    );
    assert_eq!(arg(a, "--sandbox"), Some("bwrap"));
    assert_eq!(arg(a, "--agent"), Some(AGENT));
    assert_eq!(arg(a, "--issue"), Some("github:o/r#1"));
    assert_eq!(
        arg(a, "--keys").map(std::path::Path::new),
        Some(s.data.as_path())
    );
    assert!(
        arg(a, "--data")
            .unwrap()
            .replace('\\', "/")
            .contains("agents/_coder_a_localhost")
    );
    assert_eq!(arg(a, "--home-server"), Some("a.localhost"));
    assert!(spec.run_as.is_none());
    assert!(
        spec.env
            .iter()
            .any(|(k, v)| k == "HAN_HOME_TICKET" && v == &ticket),
        "{:?}",
        spec.env.iter().map(|(k, _)| k).collect::<Vec<_>>()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_custom_harness_needs_no_install_and_pi_does() {
    let s = setup().await;
    follow_profile(&s).await;
    let Err(NodeError::Unavailable { message, .. }) = start_when_known(&s, "pi").await else {
        panic!("expected unavailable")
    };
    assert!(message.contains("han agent setup-harness pi"), "{message}");
    start_when_known(&s, "/etc/han/driver.toml").await.unwrap();
    let specs = s.sup.0.lock().unwrap();
    let a = &specs.last().unwrap().args;
    assert_eq!(arg(a, "--harness"), Some("/etc/han/driver.toml"));
    assert_eq!(arg(a, "--node"), None);
    assert_eq!(arg(a, "--sandbox"), Some("auto"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_mapped_to_an_os_user_runs_as_it() {
    let s = setup().await;
    follow_profile(&s).await;
    let state = s.world.dir.join("han-agent1");
    std::fs::write(
        s.data.join("agents.json"),
        format!(
            r#"{{"agents":{{"{AGENT}":{{"osUser":"han-agent1","stateDir":{:?},"sandbox":"bwrap"}}}}}}"#,
            state.to_string_lossy()
        ),
    )
    .unwrap();
    // the agent's key goes to its own state directory, not the node's key store
    call(
        &s.bob,
        Command::AgentCreate {
            name: "coder".into(),
            server: "a.localhost".into(),
            operator: ALICE.into(),
        },
    )
    .await;
    assert!(state.join("keys/agent-coder-a.localhost.key").exists());
    let r = start_when_known(&s, "/x.toml").await;
    if cfg!(target_os = "linux") {
        r.unwrap();
        let specs = s.sup.0.lock().unwrap();
        let spec = specs.last().unwrap();
        let run_as = spec.run_as.as_ref().unwrap();
        assert_eq!(run_as.os_user, "han-agent1");
        assert_eq!(run_as.state_dir, state);
        assert_eq!(
            arg(&spec.args, "--keys").map(std::path::Path::new),
            Some(state.as_path())
        );
        assert_eq!(
            arg(&spec.args, "--data").map(std::path::Path::new),
            Some(state.as_path())
        );
    } else {
        assert!(matches!(r, Err(NodeError::Unavailable { .. })), "{r:?}");
    }
}

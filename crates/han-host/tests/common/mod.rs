//! The test world the host's integration tests share: one home on the in-memory network, people and agents
//! with their keys, a git remote, and a fake harness.

#![allow(dead_code, missing_docs)]

use std::path::{Path, PathBuf};
use std::process::Command as Proc;
use std::sync::Arc;
use std::time::Duration;

use han_core::SigningKey;
use han_host::driver::{CredentialMode, DriverConfig, McpMode};
use han_net::{MemNetwork, MemTransport, ServeOptions, serve};
use han_node::api::{CallResult, Command, HomeRef, NodeApi, Query, Row, ViewEvent};
use han_node::keys::{DEVICE_KEY, FileKeyStore, KeyStore};
use han_node::supervisor::NoSupervisor;
use han_node::{Node, NodeConfig, NodeParts, StaticKeys};
use han_seq::{Home, HomeConfig, MemStore, TestClock};

pub const SERVER: &str = "a.localhost";
pub const ALICE: &str = "@alice:a.localhost";
pub const CODER: &str = "@coder:a.localhost";
pub const SCOUT: &str = "@scout:a.localhost";
pub const EXEC: &str = "@exec:a.localhost";
pub const ISSUE: &str = "github:o/r#12";

pub fn seed(n: u8) -> [u8; 32] {
    [n; 32]
}

pub fn pubkey(n: u8) -> String {
    SigningKey::from_seed(seed(n)).public_key().to_base64url()
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Proc::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(["-c", "init.defaultBranch=main", "-c", "core.autocrlf=false"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A bare remote with one commit on `main`.
pub fn origin(root: &Path) -> PathBuf {
    let seed = root.join("seed");
    std::fs::create_dir_all(seed.join("src")).unwrap();
    std::fs::write(seed.join("README.md"), "a demo repository\n").unwrap();
    std::fs::write(seed.join("src/lib.rs"), "pub mod auth;\n").unwrap();
    git(&seed, &["init", "-q"]);
    git(&seed, &["add", "-A"]);
    git(&seed, &["commit", "-q", "-m", "init"]);
    let bare = root.join("origin.git");
    git(
        root,
        &[
            "clone",
            "-q",
            "--bare",
            seed.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    bare
}

pub struct World {
    _net: MemNetwork,
    pub home_ref: HomeRef,
    pub home: Arc<Home>,
    keys: Arc<StaticKeys>,
    clock: Arc<TestClock>,
    net: MemNetwork,
    dir: PathBuf,
    _server: han_net::ServerHandle,
}

impl World {
    pub fn new(dir: &Path) -> World {
        let net = MemNetwork::default();
        let clock = Arc::new(TestClock::new(1_767_225_600));
        let home = Arc::new(
            Home::open(
                HomeConfig {
                    server: SERVER.into(),
                    key: SigningKey::from_seed(seed(200)),
                    key_id: "h1".into(),
                    durability: han_core::types::Durability::Local,
                },
                clock.clone(),
                Arc::new(MemStore::new()),
            )
            .unwrap(),
        );
        home.register(ALICE, "human", "d1", &pubkey(1), None)
            .unwrap();
        home.register(CODER, "agent", "d1", &pubkey(2), Some(ALICE))
            .unwrap();
        home.register(SCOUT, "agent", "d1", &pubkey(3), Some(ALICE))
            .unwrap();
        home.register(EXEC, "service", "d1", &pubkey(4), Some(ALICE))
            .unwrap();
        home.set_nodes(vec![pubkey(200)]);
        let keys = Arc::new(StaticKeys::new());
        keys.set(SERVER, home.server_keys());
        let server = serve(
            MemTransport::new(&net, &pubkey(200)),
            Arc::clone(&home),
            ServeOptions::default(),
        );
        World {
            _net: net.clone(),
            home_ref: HomeRef {
                server: SERVER.into(),
                node: pubkey(200),
                addrs: vec![],
                relays: vec!["https://mem.invalid".into()],
            },
            home,
            keys,
            clock,
            net,
            dir: dir.to_path_buf(),
            _server: server,
        }
    }

    /// Registers one more actor at the home, with device key `[n; 32]`.
    pub fn register(&self, actor: &str, kind: &str, n: u8, operator: Option<&str>) {
        self.home
            .register(actor, kind, "d1", &pubkey(n), operator)
            .unwrap();
    }

    pub fn node(&self, actor: &str, n: u8) -> Node<MemTransport> {
        let store = FileKeyStore::new(self.dir.join(format!("keys-{n}")));
        store.store(DEVICE_KEY, seed(n)).unwrap();
        let mut config = NodeConfig::new(actor);
        config.retry = Duration::from_millis(50);
        config.flush_every = Duration::from_millis(100);
        config.connect_wait = Duration::from_secs(3);
        config.data_dir = Some(self.dir.join(format!("node-{n}")));
        Node::start(NodeParts {
            config,
            transport: MemTransport::new(&self.net, &pubkey(n)),
            clock: self.clock.clone(),
            keys: Arc::new(store),
            server_keys: self.keys.clone(),
            supervisor: Arc::new(NoSupervisor),
        })
        .unwrap()
    }
}

pub async fn until<F: FnMut() -> bool>(what: &str, secs: u64, mut f: F) {
    for _ in 0..secs * 20 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

pub fn rows(api: &dyn NodeApi, q: Query) -> Vec<Row> {
    let mut v = api.view(q);
    match v.try_next() {
        Some(ViewEvent::Snapshot { rows, .. }) => rows,
        _ => Vec::new(),
    }
}

pub async fn call(api: &dyn NodeApi, cmd: Command) -> CallResult {
    api.call(cmd).await.expect("the command succeeds")
}

pub fn gated_policy() -> String {
    let mut p = han_seq::build::space_policy("members", "invite");
    if let han_core::Value::Object(m) = &mut p {
        m.insert(
            "approvals".into(),
            han_core::json::parse(br#"[{"action":"*","approvers":["owner"]}]"#).unwrap(),
        );
    }
    String::from_utf8(han_core::json::canonicalize(&p).unwrap()).unwrap()
}

pub fn fake_driver(script: &Path) -> DriverConfig {
    DriverConfig {
        name: "fake".into(),
        harness: "fake-pi".into(),
        command: vec![
            env!("CARGO_BIN_EXE_han-fake-agent").into(),
            "replay".into(),
            script.to_string_lossy().into_owned(),
        ],
        env: Vec::new(),
        mcp: McpMode::AcpMcpServers,
        credentials: CredentialMode::Subscription {
            connect_hosts: Vec::new(),
        },
        connect_hosts: Vec::new(),
        tested_version: "0.0.34".into(),
        adapter: Default::default(),
        gate_fidelity: han_host::acp::GateFidelity::PromptOnly,
        isolation: Default::default(),
        node_flags: Vec::new(),
        strict_mode: None,
        auto_mode: None,
        session_log_dir: None,
    }
}

/// The operator confirms its agent (`member.set {actor: operator, role: owner}` in the agent's profile space,
/// signed by the operator's device key `[n; 32]`), handed to the home directly. Since round 32's Z7 the
/// operator holds no role in the profile space before confirming, so the peer filter refuses that append over
/// han/1 (a spec gap, reported for round 33).
pub fn confirm_operator(home: &Home, operator: &str, n: u8, agent: &str) {
    use han_core::Value;
    let space = format!("profile:{agent}");
    let mut body = std::collections::BTreeMap::new();
    body.insert("actor".to_owned(), Value::Str(operator.to_owned()));
    body.insert("role".to_owned(), Value::Str("owner".to_owned()));
    let (entry, content) = han_seq::build::sign_entry(
        han_seq::build::EntrySpec {
            space,
            kind: "member.set".into(),
            author: operator.to_owned(),
            key_id: "d1".into(),
            body: Value::Object(body),
            content: None,
            refs: vec![],
            seen: 1,
            time_secs: home.now_secs(),
        },
        &SigningKey::from_seed([n; 32]),
    )
    .unwrap();
    home.append(&entry, content.as_ref()).unwrap();
}

pub const BOB: &str = "@bob:a.localhost";

/// Alice's space: she owns it, gates every action on an owner's approval, and confirms and admits `agents`
/// (each also follows it); `owners` are more owners.
pub async fn setup_space(
    world: &World,
    alice: &Node<MemTransport>,
    agents: &[(&str, &Node<MemTransport>)],
    owners: &[(&str, &Node<MemTransport>)],
) -> String {
    let CallResult::Space { space } = call(
        alice,
        Command::SpaceCreate {
            home: world.home_ref.clone(),
            name: "team".into(),
            visibility: "members".into(),
            join: "invite".into(),
        },
    )
    .await
    else {
        panic!("a space")
    };
    for (agent, _) in agents {
        confirm_operator(&world.home, ALICE, 1, agent);
    }
    for (who, _) in agents {
        call(
            alice,
            Command::MemberSet {
                space: space.clone(),
                actor: (*who).into(),
                role: "member".into(),
            },
        )
        .await;
    }
    for (who, _) in owners {
        call(
            alice,
            Command::MemberSet {
                space: space.clone(),
                actor: (*who).into(),
                role: "owner".into(),
            },
        )
        .await;
    }
    call(
        alice,
        Command::PolicySet {
            space: space.clone(),
            policy: gated_policy(),
        },
    )
    .await;
    for (_, n) in agents.iter().chain(owners) {
        call(
            *n,
            Command::SpaceFollow {
                home: world.home_ref.clone(),
                space: space.clone(),
            },
        )
        .await;
    }
    space
}

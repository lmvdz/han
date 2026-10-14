//! A team in one process: one home and any number of nodes on an in-memory network, sharing a test clock.

#![allow(dead_code, missing_docs)]

use std::sync::Arc;
use std::time::Duration;

use han_core::SigningKey;
use han_net::wellknown::{Fetcher, WellKnown};
use han_net::{MemNetwork, MemTransport, ServeOptions, ServerHandle, serve};
use han_node::api::{
    CallResult, Command, HomeRef, NodeApi, Query, Row, ViewEvent, ViewMirror, ViewStream, Window,
};
use han_node::keys::{DEVICE_KEY, FileKeyStore, KeyStore};
use han_node::supervisor::NoSupervisor;
use han_node::{Node, NodeConfig, NodeParts, StaticKeys};
use han_seq::{Home, HomeConfig, MemStore, TestClock};

pub const SERVER: &str = "a.localhost";
pub const START_SECS: u64 = 1_767_225_600;

pub fn seed(n: u8) -> [u8; 32] {
    [n; 32]
}

pub fn pubkey(n: u8) -> String {
    SigningKey::from_seed(seed(n)).public_key().to_base64url()
}

pub struct World {
    pub net: MemNetwork,
    pub clock: Arc<TestClock>,
    pub home: Arc<Home>,
    pub keys: Arc<StaticKeys>,
    /// How a command names the home: by its ServerName, which the world's well-known document answers.
    pub home_ref: Option<HomeRef>,
    /// Answers `/.well-known/han` for the home from memory (Z4: a ticket's node is confirmed there).
    pub fetcher: Fetcher,
    pub dir: std::path::PathBuf,
    server: Option<ServerHandle>,
}

impl World {
    /// A home for `a.localhost` that knows `people` (actor, device seed).
    pub fn new(people: &[(&str, u8)]) -> World {
        let mut w = World::offline(people);
        w.start_home();
        w
    }

    /// A world whose home is not serving yet: nodes cannot reach it until `start_home`.
    pub fn offline(people: &[(&str, u8)]) -> World {
        let net = MemNetwork::default();
        let clock = Arc::new(TestClock::new(START_SECS));
        // The counter keeps two worlds of one process apart even when the clock is coarse.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "han-node-test-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = Arc::new(
            Home::open(
                HomeConfig {
                    server: SERVER.into(),
                    key: SigningKey::from_seed(seed(200)),
                    key_id: "h1".into(),
                    durability: han_core::types::Durability::Local,
                    hosting: han_seq::HostingPolicy::KnownActors,
                },
                clock.clone(),
                Arc::new(MemStore::new()),
            )
            .unwrap(),
        );
        for (actor, n) in people {
            home.register(actor, "human", "d1", &pubkey(*n), None)
                .unwrap();
        }
        home.set_nodes(vec![pubkey(200)]);
        let keys = Arc::new(StaticKeys::new());
        keys.set(SERVER, home.server_keys());
        let home_ref = Some(HomeRef::Server(SERVER.into()));
        let fetcher = Fetcher::new().with_document(WellKnown {
            server: SERVER.into(),
            sequencer: vec![han_net::NodeAddr {
                node: pubkey(200),
                addrs: vec![],
                relays: vec!["https://mem.invalid".into()],
            }],
            keys: home.server_keys(),
        });
        World {
            fetcher,
            net,
            clock,
            home,
            keys,
            home_ref,
            dir,
            server: None,
        }
    }

    /// Starts serving the home on the network.
    pub fn start_home(&mut self) {
        let transport = MemTransport::new(&self.net, &pubkey(200));
        self.server = Some(serve(
            transport,
            Arc::clone(&self.home),
            ServeOptions::default(),
        ));
    }

    /// A node for `actor` whose device key is `[n; 32]`.
    pub fn node(&self, actor: &str, n: u8) -> Node<MemTransport> {
        self.node_with(actor, n, |_| {})
    }

    /// Like `node`, with a change to its configuration.
    pub fn node_with(
        &self,
        actor: &str,
        n: u8,
        tweak: impl FnOnce(&mut NodeConfig),
    ) -> Node<MemTransport> {
        self.node_supervised(actor, n, Arc::new(NoSupervisor), tweak)
    }

    /// Like `node_with`, with a supervisor.
    pub fn node_supervised(
        &self,
        actor: &str,
        n: u8,
        supervisor: Arc<dyn han_node::supervisor::Supervisor>,
        tweak: impl FnOnce(&mut NodeConfig),
    ) -> Node<MemTransport> {
        let key_dir = self.dir.join(format!("keys-{n}"));
        let store = FileKeyStore::new(&key_dir);
        store.store(DEVICE_KEY, seed(n)).unwrap();
        let mut config = NodeConfig::new(actor);
        // A person is not at this machine: the tests' approvals pass the presence check (tests/presence.rs
        // covers the check itself).
        config.set_presence(std::sync::Arc::new(han_presence::testing::AllowAll));
        config.retry = Duration::from_millis(50);
        config.flush_every = Duration::from_millis(100);
        config.connect_wait = Duration::from_secs(2);
        config.fetcher = self.fetcher.clone();
        tweak(&mut config);
        Node::start(NodeParts {
            config,
            transport: MemTransport::new(&self.net, &pubkey(n)),
            clock: self.clock.clone(),
            keys: Arc::new(store),
            server_keys: self.keys.clone(),
            supervisor,
        })
        .unwrap()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Polls `f` until it holds, for up to 15 s.
pub async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..750 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

pub fn space_of(r: &CallResult) -> String {
    match r {
        CallResult::Space { space } => space.clone(),
        other => panic!("not a space: {other:?}"),
    }
}

pub fn id_of(r: &CallResult) -> String {
    match r {
        CallResult::Receipt { id, .. } => id.clone(),
        other => panic!("not a receipt: {other:?}"),
    }
}

/// A view and the rows its events add up to.
pub struct Watched {
    pub stream: ViewStream,
    pub mirror: ViewMirror,
    pub query: Query,
    pub events: Vec<ViewEvent>,
}

impl Watched {
    pub fn open(api: &dyn NodeApi, query: Query) -> Watched {
        Watched {
            stream: api.view(query.clone()),
            mirror: ViewMirror::new(),
            query,
            events: Vec::new(),
        }
    }

    pub fn drain(&mut self) {
        while let Some(ev) = self.stream.try_next() {
            // A watcher that is not read for 256 events is dropped by the node (DAT-9): a test reads often.
            assert!(
                !matches!(ev, ViewEvent::Resync { .. }),
                "the view was dropped for lagging; drain it more often: {:?}",
                self.query
            );
            self.events.push(ev.clone());
            self.mirror.apply(ev);
        }
    }

    pub fn rows(&mut self) -> Vec<Row> {
        self.drain();
        self.mirror.rows()
    }
}

pub fn window() -> Window {
    Window::default()
}

pub async fn call(api: &dyn NodeApi, cmd: Command) -> CallResult {
    api.call(cmd).await.unwrap()
}

/// The operator confirms its agent over han/1 (section 4.3, P-03): `operator`'s node follows the agent's
/// profile space and appends `member.set {actor: <operator>, role: owner}` there. The operator holds no role in
/// that space; the peer filter admits this append because the profile names it as the operator (N-02), and
/// Policy judges the confirmation.
pub async fn confirm_operator(
    world: &World,
    operator: &Node<MemTransport>,
    operator_actor: &str,
    agent: &str,
) {
    let profile = format!("profile:{agent}");
    let _ = operator
        .call(Command::SpaceFollow {
            home: world.home_ref.clone(),
            space: profile.clone(),
        })
        .await;
    operator
        .call(Command::SpaceAwait {
            space: profile.clone(),
            timeout_ms: Some(30_000),
        })
        .await
        .expect("the profile space syncs");
    let r = operator
        .call(Command::MemberSet {
            space: profile.clone(),
            actor: operator_actor.into(),
            role: "owner".into(),
        })
        .await;
    assert!(
        matches!(r, Ok(CallResult::Receipt { .. })),
        "the operator's confirmation over han/1: {r:?}"
    );
    let state = world.home.state_of(&profile).expect("the profile space");
    assert!(
        state.profile.as_ref().is_some_and(|p| p.operator_confirmed),
        "{agent}'s operator is confirmed"
    );
}

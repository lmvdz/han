//! A team in one process: one home and any number of nodes on an in-memory network, sharing a test clock.

#![allow(dead_code, missing_docs)]

use std::sync::Arc;
use std::time::Duration;

use han_core::SigningKey;
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
    pub home_ref: HomeRef,
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
        let dir = std::env::temp_dir().join(format!(
            "han-node-test-{}-{}",
            std::process::id(),
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
        let home_ref = HomeRef {
            server: SERVER.into(),
            node: pubkey(200),
            addrs: vec![],
            // The memory network ignores addresses; tickets need one to parse.
            relays: vec!["https://mem.invalid".into()],
        };
        World {
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
        let key_dir = self.dir.join(format!("keys-{n}"));
        let store = FileKeyStore::new(&key_dir);
        store.store(DEVICE_KEY, seed(n)).unwrap();
        let mut config = NodeConfig::new(actor);
        config.retry = Duration::from_millis(50);
        config.flush_every = Duration::from_millis(100);
        config.connect_wait = Duration::from_secs(2);
        tweak(&mut config);
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

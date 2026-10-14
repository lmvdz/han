//! A live test of `han agent confirm` (section 4.3, P-03): a home and two people's nodes on QUIC endpoints on
//! localhost, each node driven by a face over its pipe with the commands `han agent confirm` sends (follow the
//! agent's profile space, then `member.set {actor: <operator>, role: owner}` in it). The operator holds no role
//! in the agent's profile space; the peer filter admits its confirmation because the profile names it as the
//! operator (N-02), so it reaches Policy and is recorded. Anyone else's `member.set` there is refused by the
//! filter, and so is the operator's append of any other kind.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use han_core::SigningKey;
use han_core::json::Value;
use han_net::client::args;
use han_net::dial::trusted_nodes_for_server;
use han_net::wellknown::{Fetcher, document, serve_http, wellknown_routes};
use han_net::{Client, IrohOptions, IrohTransport, RelayChoice, ServeOptions, Transport, serve};
use han_node::api::{CallResult, Command, HomeRef, NodeApi, NodeError};
use han_node::keys::{DEVICE_KEY, FileKeyStore, KeyStore};
use han_node::rpc::{RpcClient, default_endpoint};
use han_node::supervisor::NoSupervisor;
use han_node::{Node, NodeConfig, NodeParts, StaticKeys};
use han_seq::build::{EntrySpec, sign_entry};
use han_seq::{Code, Home, HomeConfig, SqliteStore, SystemClock};

const SERVER: &str = "a.localhost";
const ALICE: &str = "@alice:a.localhost";
const BOB: &str = "@bob:a.localhost";
const CODER: &str = "@coder:a.localhost";
const PROFILE: &str = "profile:@coder:a.localhost";

fn key(seed: u8) -> SigningKey {
    SigningKey::from_seed([seed; 32])
}

async fn endpoint(seed: u8) -> IrohTransport {
    let mut opts = IrohOptions::new([seed; 32]);
    opts.relay = RelayChoice::Disabled;
    opts.bind = Some(SocketAddr::from(([127, 0, 0, 1], 0)));
    IrohTransport::bind(opts).await.unwrap()
}

/// `han agent confirm`'s commands, sent by a face over the node's pipe: follow the profile space, then append
/// the operator's `member.set` once the node holds it.
async fn agent_confirm(
    face: &dyn NodeApi,
    home: &HomeRef,
    operator: &str,
) -> Result<CallResult, NodeError> {
    let _ = face
        .call(Command::SpaceFollow {
            home: Some(home.clone()),
            space: PROFILE.into(),
        })
        .await;
    loop {
        match face
            .call(Command::MemberSet {
                space: PROFILE.into(),
                actor: operator.into(),
                role: "owner".into(),
            })
            .await
        {
            Err(NodeError::NotFound { .. }) => tokio::time::sleep(Duration::from_millis(50)).await,
            other => return other,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_operator_confirms_its_agent_over_quic() {
    tokio::time::timeout(Duration::from_secs(90), scenario())
        .await
        .unwrap();
}

async fn scenario() {
    let dir = std::env::temp_dir().join(format!(
        "han-opconf-{}-{}",
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
                key: key(30),
                key_id: "h1".into(),
                durability: han_core::types::Durability::Local,
            },
            Arc::new(SystemClock::new()),
            Arc::new(SqliteStore::open(&dir.join("home")).unwrap()),
        )
        .unwrap(),
    );
    let pk = |n: u8| key(n).public_key().to_base64url();
    home.register(ALICE, "human", "d1", &pk(31), None).unwrap();
    home.register(BOB, "human", "d1", &pk(32), None).unwrap();
    home.register(CODER, "agent", "k1", &pk(33), Some(ALICE))
        .unwrap();
    let transport = endpoint(30).await;
    let addr = transport.loopback_addr();
    home.set_nodes(vec![transport.node_id()]);
    let _server = serve(transport, Arc::clone(&home), ServeOptions::default());
    let (doc_home, doc_addr) = (Arc::clone(&home), addr.clone());
    let http = serve_http(
        SocketAddr::from(([127, 0, 0, 1], 0)),
        wellknown_routes(move || document(&doc_home, std::slice::from_ref(&doc_addr))),
    )
    .await
    .unwrap();
    let fetcher = Fetcher::new().with_base(SERVER, &format!("http://{}", http.local_addr()));
    let home_ref = HomeRef::Server(SERVER.into());
    let server_keys = Arc::new(StaticKeys::new());
    server_keys.set(SERVER, home.server_keys());

    // On bare connections first: the operator's append of another kind in the profile space is refused by
    // the peer filter (it admits only her member.set there), and so is Bob's member.set
    let (nodes, addrs) = trusted_nodes_for_server(&fetcher, SERVER).await.unwrap();
    for (actor, n, kind) in [(ALICE, 31, "key.add"), (BOB, 32, "member.set")] {
        let t = endpoint(n).await;
        let c = Client::connect(&t, &addrs[0], SERVER, &nodes)
            .await
            .unwrap();
        c.hello(actor, "d1", &key(n)).await.unwrap();
        let mut body = std::collections::BTreeMap::new();
        if kind == "key.add" {
            body.insert("keyId".to_owned(), Value::Str("k9".into()));
            body.insert("publicKey".to_owned(), Value::Str(pk(99)));
        } else {
            body.insert("actor".to_owned(), Value::Str(actor.into()));
            body.insert("role".to_owned(), Value::Str("owner".into()));
        }
        let (entry, _) = sign_entry(
            EntrySpec {
                space: PROFILE.into(),
                kind: kind.into(),
                author: actor.into(),
                key_id: "d1".into(),
                body: Value::Object(body),
                content: None,
                refs: vec![],
                seen: 1,
                time_secs: home.now_secs(),
            },
            &key(n),
        )
        .unwrap();
        let e = c
            .call("append", args([("entry", entry)]))
            .await
            .unwrap_err();
        let w = e.wire().expect("a wire error");
        assert_eq!(w.code, Code::Denied, "{actor} {kind}");
        assert!(
            w.message.contains("member's device"),
            "the peer filter refuses it: {}",
            w.message
        );
        c.close();
        t.shutdown().await;
    }

    // a person's device: a node on its own QUIC endpoint, served to faces over a pipe
    let start = |actor: &str, n: u8| {
        let store = FileKeyStore::new(dir.join(format!("keys-{n}")));
        store.store(DEVICE_KEY, [n; 32]).unwrap();
        let mut config = NodeConfig::new(actor);
        config.retry = Duration::from_millis(50);
        config.flush_every = Duration::from_millis(100);
        config.connect_wait = Duration::from_secs(5);
        config.fetcher = fetcher.clone();
        (config, store)
    };
    let mut faces = Vec::new();
    let mut servers = Vec::new();
    for (actor, n) in [(ALICE, 31), (BOB, 32)] {
        let (config, store) = start(actor, n);
        let node = Node::start(NodeParts {
            config,
            transport: endpoint(n).await,
            clock: Arc::new(SystemClock::new()),
            keys: Arc::new(store),
            server_keys: server_keys.clone(),
            supervisor: Arc::new(NoSupervisor),
        })
        .unwrap();
        let pipe = dir.join(format!("pipe-{n}"));
        std::fs::create_dir_all(&pipe).unwrap();
        let endpoint = default_endpoint(&pipe);
        let api: Arc<dyn NodeApi> = Arc::new(node);
        servers.push(han_node::rpc::serve(api, &endpoint).unwrap());
        faces.push(RpcClient::connect(&endpoint).await.unwrap());
    }
    let (alice, bob) = (&faces[0], &faces[1]);

    // Bob is not the operator the profile names: the peer filter refuses his member.set
    let e = agent_confirm(bob, &home_ref, BOB).await.unwrap_err();
    assert!(matches!(e, NodeError::Denied { .. }), "{e:?}");
    // Alice is: her confirmation reaches Policy and is recorded
    let r = agent_confirm(alice, &home_ref, ALICE).await.unwrap();
    assert!(
        matches!(&r, CallResult::Receipt { space, .. } if space == PROFILE),
        "{r:?}"
    );
    let state = home.state_of(PROFILE).unwrap();
    let profile = state.profile.as_ref().unwrap();
    assert_eq!(profile.operator.as_deref(), Some(ALICE));
    assert!(profile.operator_confirmed, "the operator is confirmed");

    assert_eq!(
        home.state_of(PROFILE).unwrap().head,
        2,
        "nothing else was recorded"
    );

    drop(faces);
    drop(servers);
    let _ = std::fs::remove_dir_all(&dir);
}

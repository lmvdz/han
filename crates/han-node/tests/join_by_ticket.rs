//! A node joins a home that has no HTTPS well-known (a placeholder ServerName, no DNS) by a home ticket
//! (`HomeRef::Ticket` in a command, which the node pins): the ticket's node is dialed, its keys check the receipts (ticket-pinned trust), and
//! the binding survives a restart of the node with no flag. `han agent confirm` then works end to end.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use han_core::SigningKey;
use han_net::wellknown::Fetcher;
use han_net::{
    HomeTicket, IrohOptions, IrohTransport, RelayChoice, ServeOptions, Transport, serve,
};
use han_node::api::{
    CallResult, Command, HomeRef, NodeApi, NodeError, Query, Row, SpaceLink, ViewEvent, Window,
};
use han_node::keys::{DEVICE_KEY, FileKeyStore, KeyStore};
use han_node::supervisor::NoSupervisor;
use han_node::{Node, NodeConfig, NodeParts, StaticKeys};
use han_seq::{Home, HomeConfig, SqliteStore, SystemClock};

const SERVER: &str = "runner.han.home.arpa";
const ALICE: &str = "@alice:runner.han.home.arpa";
const CODER: &str = "@coder:runner.han.home.arpa";
const PROFILE: &str = "profile:@coder:runner.han.home.arpa";

fn key(seed: u8) -> SigningKey {
    SigningKey::from_seed([seed; 32])
}

async fn endpoint(seed: u8) -> IrohTransport {
    let mut opts = IrohOptions::new([seed; 32]);
    opts.relay = RelayChoice::Disabled;
    opts.bind = Some(SocketAddr::from(([127, 0, 0, 1], 0)));
    IrohTransport::bind(opts).await.unwrap()
}

/// A node whose only source of server keys is the (empty) static set and whose fetcher cannot reach any
/// well-known: whatever it learns about the home comes from the ticket.
async fn start(dir: &std::path::Path) -> Node<IrohTransport> {
    start_with_home(dir, None).await
}

async fn start_with_home(dir: &std::path::Path, ticket: Option<String>) -> Node<IrohTransport> {
    let store = FileKeyStore::new(dir.join("keys"));
    store.store(DEVICE_KEY, [31; 32]).unwrap();
    let mut config = NodeConfig::new(ALICE);
    config.data_dir = Some(dir.join("data"));
    config.retry = Duration::from_millis(50);
    config.flush_every = Duration::from_millis(100);
    config.connect_wait = Duration::from_secs(5);
    config.fetcher = Fetcher::new();
    config.initial_home = ticket;
    Node::start(NodeParts {
        config,
        transport: endpoint(31).await,
        clock: Arc::new(SystemClock::new()),
        keys: Arc::new(store),
        server_keys: Arc::new(StaticKeys::new()),
        supervisor: Arc::new(NoSupervisor),
    })
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_joins_a_home_by_ticket() {
    tokio::time::timeout(Duration::from_secs(90), scenario())
        .await
        .unwrap();
}

async fn scenario() {
    let dir = std::env::temp_dir().join(format!(
        "han-join-{}-{}",
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
                hosting: han_seq::HostingPolicy::KnownActors,
            },
            Arc::new(SystemClock::new()),
            Arc::new(SqliteStore::open(&dir.join("home")).unwrap()),
        )
        .unwrap(),
    );
    let pk = |n: u8| key(n).public_key().to_base64url();
    home.register(ALICE, "human", "d1", &pk(31), None).unwrap();
    home.register(CODER, "agent", "k1", &pk(33), Some(ALICE))
        .unwrap();
    let transport = endpoint(30).await;
    let addr = transport.loopback_addr();
    home.set_nodes(vec![transport.node_id()]);
    let _server = serve(
        transport.clone(),
        Arc::clone(&home),
        ServeOptions::default(),
    );
    // no HTTP server: the home has no well-known anywhere
    let ticket = HomeTicket {
        server: SERVER.into(),
        node: addr.clone(),
        keys: home.server_keys(),
    };
    assert_eq!(HomeTicket::parse(&ticket.to_text()), Some(ticket.clone()));
    let home_ref = HomeRef::Ticket(ticket.to_text());

    // a ticket that names no address, or is not a home ticket, is refused
    let node = start(&dir).await;
    let e = node
        .call(Command::HomePin {
            ticket: "han1abc".into(),
        })
        .await
        .unwrap_err();
    assert!(matches!(e, NodeError::Invalid { .. }), "{e:?}");
    let e = node
        .call(Command::SpaceFollow {
            home: Some(HomeRef::Ticket("han1abc".into())),
            space: PROFILE.into(),
        })
        .await
        .unwrap_err();
    assert!(matches!(e, NodeError::Invalid { .. }), "{e:?}");

    // without a pin the node has no way to learn the home's keys (no well-known), so it cannot follow
    let confirm = |node: &Node<IrohTransport>| {
        let node = node.clone();
        let home_ref = home_ref.clone();
        async move {
            let _ = node
                .call(Command::SpaceFollow {
                    home: Some(home_ref),
                    space: PROFILE.into(),
                })
                .await;
            node.call(Command::SpaceAwait {
                space: PROFILE.into(),
                timeout_ms: Some(30_000),
            })
            .await?;
            node.call(Command::MemberSet {
                space: PROFILE.into(),
                actor: ALICE.into(),
                role: "owner".into(),
            })
            .await
        }
    };

    // the ticket in the command is pinned by the node: no separate step
    let r = confirm(&node).await.unwrap();
    assert!(
        matches!(&r, CallResult::Receipt { space, .. } if space == PROFILE),
        "{r:?}"
    );
    let state = home.state_of(PROFILE).unwrap();
    let profile = state.profile.as_ref().unwrap();
    assert!(profile.operator_confirmed, "the operator is confirmed");
    node.shutdown();
    drop(node);

    // a restart needs no flag: the pin is in node.db, so the link to the home verifies receipts again
    let node = start(&dir).await;
    let mut linked = false;
    for _ in 0..100 {
        let mut v = node.view(Query::Spaces {
            window: Window::default(),
        });
        if let Some(ViewEvent::Snapshot { rows, .. }) = v.try_next()
            && rows.iter().any(
                |r| matches!(r, Row::SpaceStatus(x) if x.space == PROFILE && x.link == SpaceLink::Live),
            )
        {
            linked = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(linked, "the restarted node reached the home without a flag");
    node.shutdown();
    drop(node);
    transport.shutdown().await;

    // A managed local home binds a new port after restart. The launcher must supply its current
    // ticket before the person node starts links from an older persisted pin.
    let restarted_transport = endpoint(30).await;
    let restarted_ticket = HomeTicket {
        server: SERVER.into(),
        node: restarted_transport.loopback_addr(),
        keys: home.server_keys(),
    };
    assert_ne!(restarted_ticket.node.addrs, ticket.node.addrs);
    let _restarted_server = serve(
        restarted_transport.clone(),
        Arc::clone(&home),
        ServeOptions::default(),
    );
    let node = start(&dir).await;
    let pending = node
        .call(Command::SpaceCreateTrusted {
            home: None,
            name: "Queued before restart".into(),
            repos: vec!["github:team/project".into()],
        })
        .await
        .unwrap();
    let CallResult::Queued {
        space: pending_space,
        ..
    } = pending
    else {
        panic!("the stale address must retain the request in the outbox: {pending:?}");
    };
    node.shutdown();
    drop(node);
    let node = start_with_home(&dir, Some(restarted_ticket.to_text())).await;
    node.call(Command::SpaceAwait {
        space: pending_space.clone(),
        timeout_ms: Some(10_000),
    })
    .await
    .unwrap();
    assert!(
        home.state_of(&pending_space).is_some(),
        "startup delivered the original queued creation"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let mut outbox = node.view(Query::Outbox {
                space: Some(pending_space.clone()),
                window: Window::default(),
            });
            if matches!(outbox.try_next(), Some(ViewEvent::Snapshot { rows, .. })
                if rows.len() == 1 && matches!(&rows[0], Row::Outbox(row) if row.state == "sent"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the original request must settle without a second submission");
    let result = node
        .call(Command::SpaceCreateTrusted {
            home: None,
            name: "After home restart".into(),
            repos: vec!["github:team/project".into()],
        })
        .await
        .unwrap();
    assert!(
        matches!(result, CallResult::Space { .. }),
        "a startup ticket must reach the restarted home, not queue on its old port: {result:?}"
    );
    node.shutdown();
    restarted_transport.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

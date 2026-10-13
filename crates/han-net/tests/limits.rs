//! Bounds on who may connect: connections per node (G-08) and the incoming filter before the handshake.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;

use common::{SERVER, key};
use han_net::{
    Client, IrohOptions, IrohTransport, MemNetwork, MemTransport, NodeAddr, RawConn, RelayChoice,
    ServeOptions, Transport, serve,
};
use han_seq::{Home, HomeConfig, MemStore, SystemClock};
use iroh::protocol::IncomingFilterOutcome;

fn home() -> Arc<Home> {
    Arc::new(
        Home::open(
            HomeConfig {
                server: SERVER.into(),
                key: key(30),
                key_id: "h1".into(),
                durability: han_core::types::Durability::Local,
            },
            Arc::new(SystemClock::new()),
            Arc::new(MemStore::new()),
        )
        .unwrap(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_node_holds_a_bounded_number_of_connections() {
    let net = MemNetwork::default();
    let (server_id, client_id) = (
        key(1).public_key().to_base64url(),
        key(2).public_key().to_base64url(),
    );
    let server_t = MemTransport::new(&net, &server_id);
    let client_t = MemTransport::new(&net, &client_id);
    let _server = serve(
        server_t,
        home(),
        ServeOptions {
            max_conns_per_peer: 2,
            ..ServeOptions::default()
        },
    );
    let addr = NodeAddr {
        node: server_id,
        addrs: vec![],
        relays: vec![],
    };
    let mut held = Vec::new();
    for _ in 0..2 {
        held.push(
            RawConn::open(client_t.dial(&addr).await.unwrap())
                .await
                .unwrap(),
        );
    }
    // over the bound the connection is closed, and no Challenge is issued (G-08)
    let third = RawConn::open(client_t.dial(&addr).await.unwrap()).await;
    assert!(third.is_err());
    // another node is not affected
    let other = MemTransport::new(&net, &key(3).public_key().to_base64url());
    assert!(
        RawConn::open(other.dial(&addr).await.unwrap())
            .await
            .is_ok()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_incoming_filter_refuses_before_the_handshake() {
    let bind = |seed: u8, filter: Option<iroh::protocol::IncomingFilter>| async move {
        let mut opts = IrohOptions::new([seed; 32]);
        opts.relay = RelayChoice::Disabled;
        opts.bind = Some(SocketAddr::from(([127, 0, 0, 1], 0)));
        opts.incoming_filter = filter;
        IrohTransport::bind(opts).await.unwrap()
    };
    let refuse: iroh::protocol::IncomingFilter = Arc::new(|_| IncomingFilterOutcome::Reject);
    let closed_door = bind(60, Some(refuse)).await;
    let open_door = bind(61, None).await;
    let client = bind(62, None).await;
    let _a = serve(closed_door.clone(), home(), ServeOptions::default());
    let _b = serve(open_door.clone(), home(), ServeOptions::default());
    let nodes = |t: &IrohTransport| vec![t.node_id()];
    assert!(
        Client::connect(
            &client,
            &closed_door.loopback_addr(),
            SERVER,
            &nodes(&closed_door)
        )
        .await
        .is_err()
    );
    assert!(
        Client::connect(
            &client,
            &open_door.loopback_addr(),
            SERVER,
            &nodes(&open_door)
        )
        .await
        .is_ok()
    );
}

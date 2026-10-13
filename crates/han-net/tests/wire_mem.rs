//! The wire vectors over the in-process network: the frame-level vectors are carrier-independent (G-11).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use han_core::SigningKey;
use han_net::wellknown::{document, serve_http, wellknown_routes};
use han_net::wirevec::{TestConfig, WireRunner};
use han_net::{MemNetwork, MemTransport, NodeAddr, ServeOptions, serve};
use han_seq::MemStore;

fn conformance() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wire_vectors_in_process() {
    let dir = conformance().join("wire-vectors");
    let config = TestConfig::load(&dir.join("config.json")).unwrap();
    let home = config.home(Arc::new(MemStore::new())).unwrap();
    let server_id = SigningKey::from_seed([7; 32]).public_key().to_base64url();
    let client_id = SigningKey::from_seed([8; 32]).public_key().to_base64url();
    let net = MemNetwork::default();
    let server_t = MemTransport::new(&net, &server_id);
    let client_t = MemTransport::new(&net, &client_id);
    home.set_nodes(vec![server_id.clone()]);
    let _server = serve(server_t, Arc::clone(&home), ServeOptions::default());
    let addr = NodeAddr {
        node: server_id.clone(),
        addrs: vec!["127.0.0.1:1".into()],
        relays: vec![],
    };
    let doc_home = Arc::clone(&home);
    let doc_addr = addr.clone();
    let http = serve_http(
        SocketAddr::from(([127, 0, 0, 1], 0)),
        wellknown_routes(move || document(&doc_home, std::slice::from_ref(&doc_addr))),
    )
    .await
    .unwrap();
    let runner = WireRunner::new(client_t, addr, Some(http.local_addr()), config);
    let report = runner.run(&dir, std::env::var("WIRE_ONLY").ok().as_deref()).await.unwrap();
    for (id, why) in &report.failed {
        eprintln!("FAIL {id}: {why}");
    }
    eprintln!("wire vectors in process: {}", report.summary());
    assert!(report.failed.is_empty(), "{} vectors failed", report.failed.len());
}

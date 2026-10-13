//! The wire vectors over real QUIC (iroh, ALPN `han/1`) on localhost: the same frame vectors, a different
//! carrier (G-11).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use han_net::wellknown::{document, serve_http, wellknown_routes};
use han_net::wirevec::{TestConfig, WireRunner};
use han_net::{IrohOptions, IrohTransport, RelayChoice, ServeOptions, Transport, serve};
use han_seq::MemStore;

fn conformance() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance")
}

async fn endpoint(seed: u8) -> IrohTransport {
    let mut opts = IrohOptions::new([seed; 32]);
    opts.relay = RelayChoice::Disabled;
    opts.bind = Some(SocketAddr::from(([127, 0, 0, 1], 0)));
    IrohTransport::bind(opts).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wire_vectors_over_quic() {
    let dir = conformance().join("wire-vectors");
    let config = TestConfig::load(&dir.join("config.json")).unwrap();
    let home = config.home(Arc::new(MemStore::new())).unwrap();
    let server_t = endpoint(21).await;
    let client_t = endpoint(22).await;
    let addr = server_t.loopback_addr();
    home.set_nodes(vec![server_t.node_id()]);
    let _server = serve(server_t.clone(), Arc::clone(&home), ServeOptions::default());
    let (doc_home, doc_addr) = (Arc::clone(&home), addr.clone());
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
    eprintln!("wire vectors over QUIC: {}", report.summary());
    assert!(report.failed.is_empty(), "{} vectors failed", report.failed.len());
}

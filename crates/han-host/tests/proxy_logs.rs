//! SR-44: proxy logs carry host, port, route, decision and counts, never headers, bodies, query strings,
//! paths or keys. Canaries go in every part of the requests; the captured log must hold none of them.

use std::io::Write;
use std::sync::{Arc, Mutex};

use han_host::proxy::{AllowEntry, Proxy, ProxyConfig, StaticResolver};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

const CANARIES: [&str; 6] = [
    "CANARY-HEADER-7f3a",
    "CANARY-QUERY-91bc",
    "CANARY-BODY-c0de",
    "CANARY-PATH-55aa",
    "sk-test-CANARY-KEY-0042",
    "CANARY-TOKEN-xxxxxxxxxxxx",
];

async fn send(addr: std::net::SocketAddr, req: String) {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), s.read_to_end(&mut out)).await;
}

#[tokio::test]
async fn logs_hold_no_canary() {
    let buf = Buf::default();
    let b2 = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || b2.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let mut cfg = ProxyConfig::new("s-log", CANARIES[5]);
    cfg.allow = vec![AllowEntry::https("example.com")];
    cfg.resolver = Arc::new(StaticResolver(vec![(
        "example.com".into(),
        vec![std::net::IpAddr::from([127, 0, 0, 1])],
    )]));
    cfg.model = Some(han_host::proxy::ModelRoute {
        name: "anthropic".into(),
        base: "https://127.0.0.1:9".into(),
        paths: vec!["/v1/messages".into()],
        style: han_host::proxy::KeyStyle::XApiKey,
        key: han_host::proxy::KeySource::new(CANARIES[4]),
        roots: han_host::proxy::Roots::Only(vec![]),
    });
    let p = Proxy::start(cfg).await.unwrap();
    let body = format!("{{\"x\":\"{}\"}}", CANARIES[2]);
    // a model call that reaches the (unreachable) upstream, one refused for its path, one with a bad token
    for (path, token) in [
        (format!("/v1/messages?q={}", CANARIES[1]), CANARIES[5]),
        (
            format!("/v1/{}?q={}", CANARIES[3], CANARIES[1]),
            CANARIES[5],
        ),
        (format!("/v1/messages?q={}", CANARIES[1]), "bad-token"),
    ] {
        send(
            p.addr(),
            format!(
                "POST {path} HTTP/1.1\r\nHost: x\r\nx-secret: {}\r\nx-api-key: {}\r\nx-han-session: {token}\r\ncontent-length: {}\r\n\r\n{body}",
                CANARIES[0],
                CANARIES[4],
                body.len()
            ),
        )
        .await;
    }
    // a CONNECT with the token and a canary header, refused at the address check
    send(
        p.addr(),
        format!(
            "CONNECT example.com:443 HTTP/1.1\r\nx-secret: {}\r\nProxy-Authorization: Basic aGFuOkNBTkFSWS1UT0tFTi14eHh4eHh4eHh4eHg=\r\n\r\n",
            CANARIES[0]
        ),
    )
    .await;
    drop(p);
    let log = String::from_utf8_lossy(&buf.0.lock().unwrap()).into_owned();
    assert!(log.contains("proxy decision"), "the proxy logged: {log}");
    for c in CANARIES {
        assert!(!log.contains(c), "{c} leaked into the log:\n{log}");
    }
    assert!(
        !log.contains("aGFuOkNBTkFSWS1UT0tFTi"),
        "the encoded token leaked"
    );
}

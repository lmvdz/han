//! The egress proxy against a local TLS mock provider and local tunnel upstreams (SR-28, SR-35…SR-41,
//! SR-44, SR-45, SR-48). Every canary below is fake.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use han_host::proxy::{
    AllowEntry, Budget, KeySource, KeyStyle, Ledger, ModelRoute, Price, Proxy, ProxyConfig,
    ProxyHandle, Roots, StaticResolver, Usage, budget::DEFAULT_MAX_OUTPUT_TOKENS, sni,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const TOKEN: &str = "tok-AAAAAAAAAAAAAAAAAAAAAAAA";
const OTHER_TOKEN: &str = "tok-BBBBBBBBBBBBBBBBBBBBBBBB";
const REAL_KEY: &str = "sk-test-REALKEY-canary-0001";
const NEW_KEY: &str = "sk-test-REALKEY-canary-0002";

// ───────────────────────── a TLS mock provider ─────────────────────────

/// One request the mock saw: the request line, the headers (lowercase names) and the body.
type Request = (String, Vec<(String, String)>, Vec<u8>);

#[derive(Default)]
struct Seen {
    requests: Vec<Request>,
}

struct Mock {
    addr: SocketAddr,
    ca_pem: String,
    seen: Arc<Mutex<Seen>>,
    /// While false, the mock holds its answers back (after recording the request).
    gate: tokio::sync::watch::Sender<bool>,
}

fn tls_config() -> (String, tokio_rustls::rustls::ServerConfig) {
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "han test ca");
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let ca = rcgen::CertifiedIssuer::self_signed(ca_params, ca_key).unwrap();
    let leaf_params =
        rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned(), "localhost".to_owned()])
            .unwrap();
    let leaf_key = rcgen::KeyPair::generate().unwrap();
    let leaf = leaf_params.signed_by(&leaf_key, &ca).unwrap();
    let cfg = tokio_rustls::rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![leaf.der().clone()],
            tokio_rustls::rustls::pki_types::PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into()),
        )
        .unwrap();
    (ca.pem(), cfg)
}

async fn read_request<S: tokio::io::AsyncRead + Unpin>(s: &mut S) -> Option<Request> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let p = loop {
        let n = s.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
    };
    let head = String::from_utf8_lossy(&buf[..p]).into_owned();
    let mut lines = head.split("\r\n");
    let first = lines.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let len: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[p + 4..].to_vec();
    while body.len() < len {
        let n = s.read(&mut tmp).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    Some((first, headers, body))
}

impl Mock {
    async fn start() -> Mock {
        Mock::start_with(true).await
    }

    /// A mock that records requests but answers only once [`Mock::open`] is called.
    async fn start_gated() -> Mock {
        Mock::start_with(false).await
    }

    fn open(&self) {
        self.gate.send_replace(true);
    }

    async fn start_with(open: bool) -> Mock {
        let (gate, gate_rx) = tokio::sync::watch::channel(open);
        let (ca_pem, cfg) = tls_config();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let s2 = Arc::clone(&seen);
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = l.accept().await else {
                    continue;
                };
                let acceptor = acceptor.clone();
                let seen = Arc::clone(&s2);
                let mut gate = gate_rx.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(sock).await else {
                        return;
                    };
                    let Some((first, headers, body)) = read_request(&mut tls).await else {
                        return;
                    };
                    let path = first.split(' ').nth(1).unwrap_or("").to_owned();
                    seen.lock().unwrap().requests.push((first, headers, body));
                    let _ = gate.wait_for(|open| *open).await;
                    let resp = if path.starts_with("/v1/messages/redirect") {
                        "HTTP/1.1 302 Found\r\nlocation: https://127.0.0.1:9/steal\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_owned()
                    } else {
                        let body = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":90,\"output_tokens\":1}}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":5}}\n\n";
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        )
                    };
                    let _ = tls.write_all(resp.as_bytes()).await;
                    let _ = tls.shutdown().await;
                });
            }
        });
        Mock {
            addr,
            ca_pem,
            seen,
            gate,
        }
    }

    fn route(&self, key: Arc<KeySource>, style: KeyStyle) -> ModelRoute {
        ModelRoute {
            name: "mock".into(),
            base: format!("https://127.0.0.1:{}", self.addr.port()),
            paths: vec!["/v1/messages".into()],
            style,
            key,
            roots: Roots::Only(vec![self.ca_pem.as_bytes().to_vec()]),
        }
    }

    fn requests(&self) -> Vec<Request> {
        self.seen.lock().unwrap().requests.clone()
    }
}

/// Sends one raw HTTP request to the proxy and returns the whole response.
async fn raw(proxy: SocketAddr, req: &str) -> String {
    let mut s = TcpStream::connect(proxy).await.unwrap();
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut out)).await;
    String::from_utf8_lossy(&out).into_owned()
}

fn model_req(path: &str, token: &str, body: &str) -> String {
    format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nx-api-key: han-dummy-key\r\nauthorization: Bearer han-dummy\r\nx-han-session: {token}\r\nanthropic-version: 2023-06-01\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
}

async fn model_proxy(mock: &Mock, key: Arc<KeySource>, style: KeyStyle) -> ProxyHandle {
    let mut cfg = ProxyConfig::new("s1", TOKEN);
    cfg.model = Some(mock.route(key, style));
    Proxy::start(cfg).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_route_injects_the_real_key_and_counts_tokens() {
    let mock = Mock::start().await;
    let key = KeySource::new(REAL_KEY);
    let p = model_proxy(&mock, Arc::clone(&key), KeyStyle::XApiKey).await;
    assert_eq!(
        p.addr().ip(),
        IpAddr::from([127, 0, 0, 1]),
        "SR-45: loopback only"
    );

    let r = raw(
        p.addr(),
        &model_req("/v1/messages?beta=true", TOKEN, "{\"m\":1}"),
    )
    .await;
    assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    assert!(r.contains("message_delta"));
    let reqs = mock.requests();
    assert_eq!(reqs.len(), 1);
    let (first, headers, body) = &reqs[0];
    assert!(first.starts_with("POST /v1/messages?beta=true "), "{first}");
    let get = |k: &str| {
        headers
            .iter()
            .filter(|(x, _)| x == k)
            .map(|(_, v)| v.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        get("x-api-key"),
        vec![REAL_KEY.to_owned()],
        "the real key, once"
    );
    assert!(
        get("authorization").is_empty(),
        "the agent's auth header is dropped"
    );
    assert!(
        get("x-han-session").is_empty(),
        "the session token is not forwarded"
    );
    assert_eq!(get("anthropic-version"), vec!["2023-06-01".to_owned()]);
    assert_eq!(body, b"{\"m\":1}");
    let u = p.usage();
    assert_eq!(
        (u.input, u.output, u.cache_read, u.requests),
        (10, 5, 90, 1)
    );
    assert!(
        p.last_request()
            .is_some_and(|l| l.headers.iter().all(|(k, _)| k != "x-api-key"))
    );

    // SR-48: a new key is used by the next request, without a restart.
    key.set(NEW_KEY);
    let r = raw(p.addr(), &model_req("/v1/messages", TOKEN, "{}")).await;
    assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    let reqs = mock.requests();
    assert!(
        reqs[1]
            .1
            .iter()
            .any(|(k, v)| k == "x-api-key" && v == NEW_KEY)
    );
    assert_eq!(p.usage().requests, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bearer_style_and_refusals() {
    let mock = Mock::start().await;
    let p = model_proxy(&mock, KeySource::new(REAL_KEY), KeyStyle::Bearer).await;
    let r = raw(p.addr(), &model_req("/v1/messages", TOKEN, "{}")).await;
    assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    let h = &mock.requests()[0].1;
    assert!(
        h.iter()
            .any(|(k, v)| k == "authorization" && v == &format!("Bearer {REAL_KEY}"))
    );
    assert!(!h.iter().any(|(k, _)| k == "x-api-key"));

    // SR-28: a missing or wrong token, and another session's token, are refused.
    let other = {
        let mut cfg = ProxyConfig::new("s2", OTHER_TOKEN);
        cfg.model = Some(mock.route(KeySource::new(REAL_KEY), KeyStyle::Bearer));
        Proxy::start(cfg).await.unwrap()
    };
    for (proxy, token) in [
        (p.addr(), "nope"),
        (p.addr(), OTHER_TOKEN),
        (other.addr(), TOKEN),
    ] {
        let r = raw(proxy, &model_req("/v1/messages", token, "{}")).await;
        assert!(r.starts_with("HTTP/1.1 403"), "{r}");
    }
    // SR-40: other provider paths are refused.
    for path in [
        "/v1/organizations",
        "/v1/files",
        "/v1/messages/../files",
        "/v1/messagesx",
    ] {
        let r = raw(p.addr(), &model_req(path, TOKEN, "{}")).await;
        assert!(r.starts_with("HTTP/1.1 403"), "{path}: {r}");
    }
    // absolute-form is accepted for the proxy's own address
    let r = raw(
        p.addr(),
        &model_req(
            &format!("http://127.0.0.1:{}/v1/messages", p.addr().port()),
            TOKEN,
            "{}",
        ),
    )
    .await;
    assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    assert_eq!(
        mock.requests().len(),
        2,
        "refused requests never reach the provider"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redirects_are_returned_not_followed() {
    let mock = Mock::start().await;
    let p = model_proxy(&mock, KeySource::new(REAL_KEY), KeyStyle::XApiKey).await;
    let r = raw(p.addr(), &model_req("/v1/messages/redirect", TOKEN, "{}")).await;
    assert!(r.starts_with("HTTP/1.1 302"), "{r}");
    assert!(r.contains("location: https://127.0.0.1:9/steal"));
    assert_eq!(mock.requests().len(), 1);
}

struct OneCall(Mutex<u64>);

impl Budget for OneCall {
    fn exhausted(&self) -> bool {
        *self.0.lock().unwrap() >= 1
    }
    fn record(&self, u: &Usage) {
        *self.0.lock().unwrap() += u.requests;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_spent_budget_refuses_model_calls() {
    let mock = Mock::start().await;
    let mut cfg = ProxyConfig::new("s1", TOKEN);
    cfg.model = Some(mock.route(KeySource::new(REAL_KEY), KeyStyle::XApiKey));
    cfg.budget = Some(Arc::new(OneCall(Mutex::new(0))));
    let p = Proxy::start(cfg).await.unwrap();
    assert!(
        raw(p.addr(), &model_req("/v1/messages", TOKEN, "{}"))
            .await
            .starts_with("HTTP/1.1 200")
    );
    let r = raw(p.addr(), &model_req("/v1/messages", TOKEN, "{}")).await;
    assert!(r.starts_with("HTTP/1.1 429"), "{r}");
    assert_eq!(mock.requests().len(), 1);
    assert!(p.decisions().iter().any(|d| d.reason == "budget-spent"));
}

/// Counts a call only after a delay, as a budget backed by a slow store would.
struct SlowRecord(Mutex<u64>);

impl Budget for SlowRecord {
    fn exhausted(&self) -> bool {
        *self.0.lock().unwrap() >= 1
    }
    fn record(&self, u: &Usage) {
        std::thread::sleep(std::time::Duration::from_millis(300));
        *self.0.lock().unwrap() += u.requests;
    }
}

/// The spend of a call is recorded before the client sees the end of its response, so a
/// client that reads to EOF and calls again is always refused (SR-41).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spend_is_recorded_before_the_response_ends() {
    let mock = Mock::start().await;
    let mut cfg = ProxyConfig::new("s1", TOKEN);
    cfg.model = Some(mock.route(KeySource::new(REAL_KEY), KeyStyle::XApiKey));
    cfg.budget = Some(Arc::new(SlowRecord(Mutex::new(0))));
    let p = Proxy::start(cfg).await.unwrap();
    assert!(
        raw(p.addr(), &model_req("/v1/messages", TOKEN, "{}"))
            .await
            .starts_with("HTTP/1.1 200")
    );
    let r = raw(p.addr(), &model_req("/v1/messages", TOKEN, "{}")).await;
    assert!(r.starts_with("HTTP/1.1 429"), "{r}");
    assert_eq!(mock.requests().len(), 1);
}

/// Waits until `cond` holds. It waits on state, not for a fixed time; the bound only turns a hang into a failure.
async fn until(what: &str, mut cond: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    while !cond() {
        assert!(start.elapsed() < Duration::from_secs(30), "never: {what}");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// One unit per input token, five per output token (cache reads free, cache writes two).
const PRICE: Price = Price {
    input: 1,
    output: 5,
    cache_read: 0,
    cache_creation: 2,
};

fn ledger(limit: u64) -> Arc<Ledger> {
    Arc::new(Ledger::new(
        limit,
        HashMap::from([("claude-test".to_owned(), PRICE)]),
    ))
}

async fn ledger_proxy(mock: &Mock, ledger: &Arc<Ledger>) -> ProxyHandle {
    let mut cfg = ProxyConfig::new("s1", TOKEN);
    cfg.model = Some(mock.route(KeySource::new(REAL_KEY), KeyStyle::XApiKey));
    let budget: Arc<dyn Budget> = Arc::clone(ledger) as Arc<dyn Budget>;
    cfg.budget = Some(budget);
    Proxy::start(cfg).await.unwrap()
}

fn call_body(max_tokens: Option<u64>) -> String {
    match max_tokens {
        Some(m) => format!("{{\"model\":\"claude-test-1\",\"max_tokens\":{m}}}"),
        None => "{\"model\":\"claude-test-1\"}".to_owned(),
    }
}

/// The worst case of a call with this `max_tokens`: two bytes of body per input token at the dearer input
/// price (2), and the output cap at 5.
fn worst(max_tokens: Option<u64>, default_cap: u64) -> u64 {
    let body = call_body(max_tokens);
    (body.len() as u64).div_ceil(2) * 2 + max_tokens.unwrap_or(default_cap) * 5
}

/// SR-41: N concurrent calls against a budget that fits k of them. Exactly k are forwarded, and the spend
/// never passes the limit. The mock holds every answer back, so nothing settles while the others arrive: the
/// reservation, not the spend, must refuse them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_calls_cannot_overshoot_the_budget() {
    const N: usize = 12;
    const K: usize = 3;
    let w = worst(Some(1_000), 0);
    let limit = w * K as u64 + w / 2;
    let mock = Mock::start_gated().await;
    let l = ledger(limit);
    let p = ledger_proxy(&mock, &l).await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    for _ in 0..N {
        let (tx, addr) = (tx.clone(), p.addr());
        tokio::spawn(async move {
            let r = raw(
                addr,
                &model_req("/v1/messages", TOKEN, &call_body(Some(1_000))),
            )
            .await;
            let _ = tx.send(r);
        });
    }
    for _ in 0..N - K {
        let r = rx.recv().await.unwrap();
        assert!(r.starts_with("HTTP/1.1 429"), "{r}");
        assert!(r.contains("x-han-refusal: budget-would-exceed"), "{r}");
    }
    until("k calls reach the provider", || mock.requests().len() == K).await;
    assert_eq!(l.reserved(), w * K as u64);
    mock.open();
    for _ in 0..K {
        let r = rx.recv().await.unwrap();
        assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    }
    assert_eq!(mock.requests().len(), K, "exactly k were forwarded");
    until("reservations settle", || l.reserved() == 0).await;
    assert!(l.spent() <= limit, "spent {} of {limit}", l.spent());
    // the mock reports 10 input, 90 cache-read and 5 output tokens: 10 + 0 + 25 each
    assert_eq!(l.spent(), 35 * K as u64);
}

/// A call whose client goes away mid-stream still ends: the reservation is released and the call settled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_aborted_call_releases_its_reservation() {
    let mock = Mock::start_gated().await;
    let l = ledger(1_000_000);
    let p = ledger_proxy(&mock, &l).await;
    let mut s = TcpStream::connect(p.addr()).await.unwrap();
    s.write_all(model_req("/v1/messages", TOKEN, &call_body(Some(100))).as_bytes())
        .await
        .unwrap();
    until("the call is forwarded", || mock.requests().len() == 1).await;
    assert_eq!(l.reserved(), worst(Some(100), 0));
    drop(s);
    mock.open();
    until("the reservation is gone", || l.reserved() == 0).await;
    // the freed budget admits the next call
    let r = raw(
        p.addr(),
        &model_req("/v1/messages", TOKEN, &call_body(Some(100))),
    )
    .await;
    assert!(r.starts_with("HTTP/1.1 200"), "{r}");
}

/// A request that sets no `max_tokens` reserves the documented default cap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_without_max_tokens_reserves_the_default_cap() {
    let mock = Mock::start_gated().await;
    let l = ledger(1_000_000);
    let p = ledger_proxy(&mock, &l).await;
    let mut s = TcpStream::connect(p.addr()).await.unwrap();
    s.write_all(model_req("/v1/messages", TOKEN, &call_body(None)).as_bytes())
        .await
        .unwrap();
    until("the call is forwarded", || mock.requests().len() == 1).await;
    assert_eq!(l.reserved(), worst(None, DEFAULT_MAX_OUTPUT_TOKENS));
    mock.open();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out).await;
    until("the reservation is gone", || l.reserved() == 0).await;
}

/// A model without a price reserves nothing (subscription mode has no per-call price).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unpriced_model_is_not_reserved() {
    let mock = Mock::start_gated().await;
    let l = ledger(1_000_000);
    let p = ledger_proxy(&mock, &l).await;
    let mut s = TcpStream::connect(p.addr()).await.unwrap();
    s.write_all(model_req("/v1/messages", TOKEN, "{\"model\":\"other\"}").as_bytes())
        .await
        .unwrap();
    until("the call is forwarded", || mock.requests().len() == 1).await;
    assert_eq!(l.reserved(), 0);
    mock.open();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out).await;
    assert_eq!(l.spent(), 0);
}

// ───────────────────────── tunnels ─────────────────────────

/// An upstream that records what it receives and answers "ok".
async fn upstream() -> (SocketAddr, Arc<Mutex<Vec<u8>>>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let got = Arc::new(Mutex::new(Vec::new()));
    let g = Arc::clone(&got);
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                continue;
            };
            let g = Arc::clone(&g);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                if let Ok(n) = s.read(&mut buf).await {
                    g.lock().unwrap().extend_from_slice(&buf[..n]);
                    let _ = s.write_all(b"ok").await;
                }
            });
        }
    });
    (addr, got)
}

fn basic(token: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(format!("han:{token}"))
}

/// CONNECT, then (if 200) send `then` and read the answer.
async fn tunnel(proxy: SocketAddr, target: &str, token: &str, then: &[u8]) -> (String, Vec<u8>) {
    let mut s = TcpStream::connect(proxy).await.unwrap();
    s.write_all(
        format!(
            "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\nProxy-Authorization: Basic {}\r\n\r\n",
            basic(token)
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if s.read(&mut b).await.unwrap_or(0) == 0 {
            break;
        }
        head.push(b[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let mut rest = Vec::new();
    if head.starts_with("HTTP/1.1 200") {
        let _ = s.write_all(then).await;
        let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut rest)).await;
    }
    (head, rest)
}

async fn tunnel_proxy(port: u16, private_ok: bool, model: bool) -> ProxyHandle {
    let mut cfg = ProxyConfig::new("s1", TOKEN);
    cfg.allow = vec![
        AllowEntry {
            host: "upstream.test".into(),
            ports: vec![port],
        },
        AllowEntry::https("example.com"),
        AllowEntry {
            host: "internal.test".into(),
            ports: vec![port],
        },
        AllowEntry {
            host: "tail.test".into(),
            ports: vec![port],
        },
    ];
    cfg.resolver = Arc::new(StaticResolver(vec![
        ("upstream.test".into(), vec![IpAddr::from([127, 0, 0, 1])]),
        ("internal.test".into(), vec![IpAddr::from([127, 0, 0, 1])]),
        ("tail.test".into(), vec![IpAddr::from([100, 100, 1, 1])]),
        ("example.com".into(), vec![IpAddr::from([127, 0, 0, 1])]),
    ]));
    cfg.allow_private_upstreams = private_ok;
    if model {
        cfg.model = None;
    }
    Proxy::start(cfg).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_allow_list_sni_and_no_credentials() {
    let (up, got) = upstream().await;
    let p = tunnel_proxy(up.port(), true, false).await;
    let target = format!("upstream.test:{}", up.port());
    let hello = sni::client_hello(Some("upstream.test"));
    let (head, answer) = tunnel(p.addr(), &target, TOKEN, &hello).await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert_eq!(answer, b"ok");
    let seen = got.lock().unwrap().clone();
    assert_eq!(
        seen, hello,
        "the upstream gets exactly the client's bytes: no credentials added"
    );

    // SR-36: SNI mismatch, missing SNI, not TLS: the tunnel closes before the upstream hears anything.
    got.lock().unwrap().clear();
    for bytes in [
        sni::client_hello(Some("evil.test")),
        sni::client_hello(None),
        b"GET / HTTP/1.1\r\nHost: upstream.test\r\n\r\n".to_vec(),
    ] {
        let (head, answer) = tunnel(p.addr(), &target, TOKEN, &bytes).await;
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert!(answer.is_empty(), "closed");
    }
    assert!(got.lock().unwrap().is_empty());
    let reasons: Vec<&str> = p.decisions().iter().map(|d| d.reason).collect();
    for r in ["sni-mismatch", "sni-missing", "not-tls"] {
        assert!(reasons.contains(&r), "{reasons:?}");
    }

    // SR-35: not on the list, suffix confusion, IP literals, other ports.
    for t in [
        "evilexample.com:443",
        "example.com.evil.net:443",
        "sub.example.com:443",
        "127.0.0.1:443",
        "[::1]:443",
        "example.com:80",
    ] {
        let (head, _) = tunnel(p.addr(), t, TOKEN, b"").await;
        assert!(head.starts_with("HTTP/1.1 403"), "{t}: {head}");
    }
    // SR-28: no token, or another session's.
    let (head, _) = tunnel(p.addr(), &target, OTHER_TOKEN, b"").await;
    assert!(head.starts_with("HTTP/1.1 407"), "{head}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn allow_listed_names_resolving_to_private_addresses_are_refused() {
    let (up, got) = upstream().await;
    let p = tunnel_proxy(up.port(), false, false).await;
    for host in ["internal.test", "tail.test", "upstream.test"] {
        let (head, _) = tunnel(p.addr(), &format!("{host}:{}", up.port()), TOKEN, b"").await;
        assert!(head.starts_with("HTTP/1.1 403"), "{host}: {head}");
    }
    assert!(got.lock().unwrap().is_empty());
    assert!(
        p.decisions()
            .iter()
            .all(|d| !d.allowed && d.reason == "private-address")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscription_mode_has_no_model_route() {
    let (up, _) = upstream().await;
    let p = tunnel_proxy(up.port(), true, true).await;
    let r = raw(p.addr(), &model_req("/v1/messages", TOKEN, "{}")).await;
    assert!(r.starts_with("HTTP/1.1 403"), "{r}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_mcp_tunnel_needs_the_token() {
    let mut cfg = ProxyConfig::new("s1", TOKEN);
    cfg.mcp = Some(Arc::new(|mut s: TcpStream| {
        tokio::spawn(async move {
            let _ = s.write_all(b"pong").await;
        });
    }));
    let p = Proxy::start(cfg).await.unwrap();
    let (head, answer) = tunnel(p.addr(), "han-mcp.invalid:1", TOKEN, b"").await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert_eq!(answer, b"pong");
    let (head, _) = tunnel(p.addr(), "han-mcp.invalid:1", "wrong-token-xxxxxxxx", b"").await;
    assert!(head.starts_with("HTTP/1.1 407"), "{head}");
    let (head, _) = tunnel(p.addr(), "han-mcp.invalid:2", TOKEN, b"").await;
    assert!(head.starts_with("HTTP/1.1 403"), "{head}");
    // the stub in the shim opens the same tunnel
    let s = han_host::shim::open_mcp_tunnel(p.addr(), TOKEN)
        .await
        .unwrap();
    let mut s = s;
    let mut b = [0u8; 4];
    s.read_exact(&mut b).await.unwrap();
    assert_eq!(&b, b"pong");
    assert!(
        han_host::shim::open_mcp_tunnel(p.addr(), "wrong-token-xxxxxxxx")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn short_tokens_are_refused() {
    assert!(Proxy::start(ProxyConfig::new("s1", "short")).await.is_err());
}

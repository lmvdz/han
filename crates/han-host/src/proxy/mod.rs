//! The egress proxy of one session (PLAN §3 "Agent egress", §6.4; SR-28, SR-35…SR-45).
//!
//! The sandbox reaches only this proxy. It listens on `127.0.0.1` on a port of its own (SR-45) and answers
//! three routes, each only with the session's token (SR-28):
//!
//! * **CONNECT tunnels** to hostnames on the allow-list, on the ports their entry allows (SR-35). The proxy
//!   resolves the name itself, refuses private, loopback, link-local, CGNAT, ULA, multicast and other
//!   non-public addresses and connects to the exact address it checked (SR-37), then reads the TLS
//!   ClientHello and closes the tunnel unless its SNI is the CONNECT host (SR-36). No credentials are added
//!   to tunnels and TLS is never intercepted.
//! * **The model route** (API-key mode): plain HTTP requests to allowed provider paths (SR-40). Every key
//!   header the agent sends is dropped, the real key is injected, and the request is forwarded over verified
//!   TLS only to the configured provider; redirects are returned, never followed (SR-39). Token counts are
//!   read from the response (PLAN §6.4 Cost) and a budget can refuse further calls (SR-41).
//! * **The MCP tunnel**: `CONNECT han-mcp.invalid:1` hands the raw stream to han-mcp outside the sandbox
//!   (SR-29), so the agent's tools never need the node API.
//!
//! In subscription mode there is no model route: the harness keeps its own login and the proxy only tunnels.
//!
//! Logs and spans carry the host, port, route, decision, byte and token counts and nothing else: no headers,
//! bodies, query strings, paths or keys (SR-44).

pub mod addr;
pub mod sni;
pub mod usage;

use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;

pub use addr::{Resolve, StaticResolver, SystemResolver, refused_class};
pub use usage::Usage;

/// The host of the MCP tunnel.
pub const MCP_HOST: &str = "han-mcp.invalid";
/// The port of the MCP tunnel.
pub const MCP_PORT: u16 = 1;
/// The header that carries the session token on the model route.
pub const SESSION_HEADER: &str = "x-han-session";
/// The user name in `Proxy-Authorization: Basic` for CONNECT.
pub const PROXY_USER: &str = "han";

const MAX_HEAD: usize = 64 * 1024;
const MAX_BODY: usize = 32 * 1024 * 1024;
const LOG_CAPACITY: usize = 512;
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// One host the sandbox may tunnel to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllowEntry {
    /// The exact hostname (no wildcards; case-insensitive; a trailing dot is ignored).
    pub host: String,
    /// The ports it may be reached on (default `[443]`).
    pub ports: Vec<u16>,
}

impl AllowEntry {
    /// An entry for `host` on port 443.
    pub fn https(host: impl Into<String>) -> Self {
        AllowEntry {
            host: host.into(),
            ports: vec![443],
        }
    }
}

/// How the real key is sent to the provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyStyle {
    /// `x-api-key: <key>` (Anthropic API keys).
    XApiKey,
    /// `Authorization: Bearer <key>` (OpenAI-style keys, OAuth access tokens).
    Bearer,
}

/// A model key held only in the proxy's memory. It can be replaced while sessions run (SR-48); the next
/// request uses the new key. Its `Debug` never shows it.
pub struct KeySource(RwLock<String>);

impl KeySource {
    /// A source holding `key`.
    pub fn new(key: impl Into<String>) -> Arc<Self> {
        Arc::new(KeySource(RwLock::new(key.into())))
    }

    /// Replaces the key.
    pub fn set(&self, key: impl Into<String>) {
        *self.0.write().unwrap_or_else(|p| p.into_inner()) = key.into();
    }

    fn get(&self) -> String {
        self.0.read().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

impl std::fmt::Debug for KeySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KeySource(<redacted>)")
    }
}

/// Which roots verify the provider's certificate.
#[derive(Clone, Debug)]
pub enum Roots {
    /// The operating system's roots.
    System,
    /// Only these PEM certificates (tests, private providers).
    Only(Vec<Vec<u8>>),
}

/// The model route of API-key mode.
#[derive(Clone, Debug)]
pub struct ModelRoute {
    /// A name for logs, for example `anthropic`.
    pub name: String,
    /// The provider's base URL, `https://host[:port]` (the harness's base URL points at the proxy instead).
    pub base: String,
    /// Allowed path prefixes, for example `/v1/messages`. A path matches a prefix when it equals it or
    /// continues it after a `/`.
    pub paths: Vec<String>,
    /// How the key is sent.
    pub style: KeyStyle,
    /// The key.
    pub key: Arc<KeySource>,
    /// Certificate roots for the provider.
    pub roots: Roots,
}

/// The budget the proxy asks before each model call (SR-41).
pub trait Budget: Send + Sync + 'static {
    /// Whether the session's budget is spent.
    fn exhausted(&self) -> bool;
    /// Records what one call used.
    fn record(&self, usage: &Usage);
}

/// What the proxy hands a stream on the MCP tunnel to.
pub type McpAcceptor = Arc<dyn Fn(TcpStream) + Send + Sync>;

/// How one session's proxy is set up.
pub struct ProxyConfig {
    /// The session, for logs.
    pub session: String,
    /// The per-session token every request must carry (SR-28). At least 16 characters.
    pub token: String,
    /// Hosts the sandbox may tunnel to.
    pub allow: Vec<AllowEntry>,
    /// The model route (API-key mode); `None` in subscription mode.
    pub model: Option<ModelRoute>,
    /// The MCP tunnel's acceptor.
    pub mcp: Option<McpAcceptor>,
    /// The resolver for tunnel hosts.
    pub resolver: Arc<dyn Resolve>,
    /// Lets tunnels reach non-public addresses. Tests only; never set in a real session.
    pub allow_private_upstreams: bool,
    /// The budget.
    pub budget: Option<Arc<dyn Budget>>,
    /// The port to listen on (0: any free port).
    pub port: u16,
}

impl ProxyConfig {
    /// A configuration with nothing allowed and the system resolver.
    pub fn new(session: impl Into<String>, token: impl Into<String>) -> Self {
        ProxyConfig {
            session: session.into(),
            token: token.into(),
            allow: Vec::new(),
            model: None,
            mcp: None,
            resolver: Arc::new(SystemResolver),
            allow_private_upstreams: false,
            budget: None,
            port: 0,
        }
    }
}

/// Which route a request took.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// A CONNECT tunnel.
    Connect,
    /// The model route.
    Model,
    /// The MCP tunnel.
    Mcp,
    /// Anything else (refused).
    Other,
}

impl Route {
    fn name(self) -> &'static str {
        match self {
            Route::Connect => "connect",
            Route::Model => "model",
            Route::Mcp => "mcp",
            Route::Other => "other",
        }
    }
}

/// One decision of the proxy, as the host may show it (a hint on the board). No paths, no headers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    /// Milliseconds since the proxy started.
    pub at_ms: u64,
    /// The route.
    pub route: Route,
    /// The destination host (the allow-list name, or the provider route's name).
    pub host: String,
    /// The destination port.
    pub port: u16,
    /// Whether it was let through.
    pub allowed: bool,
    /// A fixed reason code, for example `allowed`, `not-on-allow-list`, `bad-token`, `private-address`.
    pub reason: &'static str,
    /// Bytes from the sandbox.
    pub bytes_up: u64,
    /// Bytes to the sandbox.
    pub bytes_down: u64,
    /// HTTP status of a model call.
    pub status: Option<u16>,
}

/// The last model request, kept in memory for a later summary fork (SR-61). Credential headers are not kept.
#[derive(Clone, Debug)]
pub struct LastRequest {
    /// The path and query as the agent sent them.
    pub path_and_query: String,
    /// The request headers, credentials and hop-by-hop headers removed.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

struct Shared {
    session: String,
    token: String,
    allow: Vec<AllowEntry>,
    model: Option<(ModelRoute, reqwest::Client)>,
    mcp: Option<McpAcceptor>,
    resolver: Arc<dyn Resolve>,
    allow_private: bool,
    budget: Option<Arc<dyn Budget>>,
    started: Instant,
    log: Mutex<VecDeque<Decision>>,
    usage: watch::Sender<Usage>,
    last: Mutex<Option<LastRequest>>,
}

/// A running proxy. Dropping it stops the listener; open tunnels end with their sockets.
pub struct ProxyHandle {
    addr: SocketAddr,
    shared: Arc<Shared>,
    usage: watch::Receiver<Usage>,
    task: JoinHandle<()>,
}

impl ProxyHandle {
    /// The address it listens on (always `127.0.0.1`).
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The token totals so far.
    pub fn usage(&self) -> Usage {
        *self.usage.borrow()
    }

    /// A watch of the token totals, updated after each model call.
    pub fn usage_watch(&self) -> watch::Receiver<Usage> {
        self.usage.clone()
    }

    /// The most recent decisions, oldest first.
    pub fn decisions(&self) -> Vec<Decision> {
        lock(&self.shared.log).iter().cloned().collect()
    }

    /// The last model request.
    pub fn last_request(&self) -> Option<LastRequest> {
        lock(&self.shared.last).clone()
    }

    /// Stops listening.
    pub fn shutdown(&self) {
        self.task.abort();
    }
}

impl Drop for ProxyHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The proxy.
pub struct Proxy;

impl Proxy {
    /// Starts a proxy on `127.0.0.1`.
    pub async fn start(cfg: ProxyConfig) -> io::Result<ProxyHandle> {
        if cfg.token.len() < 16 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a session token has at least 16 characters",
            ));
        }
        let model = match cfg.model {
            None => None,
            Some(route) => {
                let client = model_client(&route)?;
                Some((route, client))
            }
        };
        let listener = TcpListener::bind(("127.0.0.1", cfg.port)).await?;
        let addr = listener.local_addr()?;
        let (tx, rx) = watch::channel(Usage::default());
        let shared = Arc::new(Shared {
            session: cfg.session,
            token: cfg.token,
            allow: cfg
                .allow
                .into_iter()
                .map(|mut e| {
                    e.host = norm_host(&e.host);
                    if e.ports.is_empty() {
                        e.ports = vec![443];
                    }
                    e
                })
                .collect(),
            model,
            mcp: cfg.mcp,
            resolver: cfg.resolver,
            allow_private: cfg.allow_private_upstreams,
            budget: cfg.budget,
            started: Instant::now(),
            log: Mutex::new(VecDeque::new()),
            usage: tx,
            last: Mutex::new(None),
        });
        let s = Arc::clone(&shared);
        let task = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((sock, _)) => {
                        let s = Arc::clone(&s);
                        tokio::spawn(async move {
                            let _ = handle(sock, s).await;
                        });
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
        });
        tracing::info!(target: "han_host::proxy", session = %shared.session, port = addr.port(), "proxy listening");
        Ok(ProxyHandle {
            addr,
            shared,
            usage: rx,
            task,
        })
    }
}

fn model_client(route: &ModelRoute) -> io::Result<reqwest::Client> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    if !route.base.starts_with("https://") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the model route forwards over TLS only",
        ));
    }
    let mut b = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .connect_timeout(CONNECT_TIMEOUT);
    if let Roots::Only(pems) = &route.roots {
        let mut certs = Vec::new();
        for p in pems {
            certs.push(reqwest::Certificate::from_pem(p).map_err(io::Error::other)?);
        }
        b = b.tls_certs_only(certs);
    }
    b.build().map_err(io::Error::other)
}

fn norm_host(h: &str) -> String {
    h.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Constant-time comparison of two tokens.
fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

struct Head {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    rest: Vec<u8>,
}

impl Head {
    fn get(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

async fn read_head<R: AsyncRead + Unpin>(s: &mut R) -> io::Result<Head> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        let n = s.read(&mut tmp).await?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..p]).into_owned();
            let rest = buf[p + 4..].to_vec();
            let mut lines = head.split("\r\n");
            let first = lines.next().unwrap_or("");
            let mut parts = first.split(' ');
            let method = parts.next().unwrap_or("").to_owned();
            let target = parts.next().unwrap_or("").to_owned();
            let version = parts.next().unwrap_or("");
            if !version.starts_with("HTTP/1.") {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let headers = lines
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
                .collect();
            return Ok(Head {
                method,
                target,
                headers,
                rest,
            });
        }
        if buf.len() > MAX_HEAD {
            return Err(io::ErrorKind::InvalidData.into());
        }
    }
}

impl Shared {
    fn record(&self, mut d: Decision) {
        d.at_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        tracing::info!(
            target: "han_host::proxy",
            session = %self.session,
            route = d.route.name(),
            host = %d.host,
            port = d.port,
            allowed = d.allowed,
            reason = d.reason,
            bytes_up = d.bytes_up,
            bytes_down = d.bytes_down,
            status = d.status.unwrap_or(0),
            "proxy decision"
        );
        let mut log = lock(&self.log);
        if log.len() >= LOG_CAPACITY {
            log.pop_front();
        }
        log.push_back(d);
    }

    fn connect_token_ok(&self, h: &Head) -> bool {
        let Some(v) = h.get("proxy-authorization") else {
            return false;
        };
        let Some(b64) = v
            .strip_prefix("Basic ")
            .or_else(|| v.strip_prefix("basic "))
        else {
            return false;
        };
        let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(b64.trim()) else {
            return false;
        };
        let Ok(text) = String::from_utf8(raw) else {
            return false;
        };
        match text.split_once(':') {
            Some((user, pass)) => user == PROXY_USER && same(pass, &self.token),
            None => false,
        }
    }
}

fn decision(route: Route, host: &str, port: u16, allowed: bool, reason: &'static str) -> Decision {
    Decision {
        at_ms: 0,
        route,
        host: host.to_owned(),
        port,
        allowed,
        reason,
        bytes_up: 0,
        bytes_down: 0,
        status: None,
    }
}

async fn reply(s: &mut TcpStream, status: &str, extra: &str) -> io::Result<()> {
    let body = format!(
        "{{\"type\":\"error\",\"error\":{{\"type\":\"permission_error\",\"message\":\"han proxy: {status}\"}}}}"
    );
    s.write_all(
        format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n{extra}\r\n{body}",
            body.len()
        )
        .as_bytes(),
    )
    .await?;
    let _ = s.shutdown().await;
    Ok(())
}

async fn handle(mut s: TcpStream, sh: Arc<Shared>) -> io::Result<()> {
    let head = match tokio::time::timeout(HELLO_TIMEOUT, read_head(&mut s)).await {
        Ok(Ok(h)) => h,
        _ => {
            sh.record(decision(Route::Other, "", 0, false, "malformed"));
            return reply(&mut s, "400 Bad Request", "").await;
        }
    };
    if head.method.eq_ignore_ascii_case("CONNECT") {
        connect(s, sh, head).await
    } else {
        model(s, sh, head).await
    }
}

fn split_authority(t: &str) -> Option<(String, u16)> {
    let (host, port) = if let Some(rest) = t.strip_prefix('[') {
        let (h, p) = rest.split_once("]:")?;
        (h.to_owned(), p)
    } else {
        let (h, p) = t.rsplit_once(':')?;
        (h.to_owned(), p)
    };
    let port: u16 = port.parse().ok()?;
    if host.is_empty() || port == 0 && host != MCP_HOST {
        return None;
    }
    Some((norm_host(&host), port))
}

async fn connect(mut s: TcpStream, sh: Arc<Shared>, head: Head) -> io::Result<()> {
    let Some((host, port)) = split_authority(&head.target) else {
        sh.record(decision(Route::Connect, "", 0, false, "malformed-target"));
        return reply(&mut s, "400 Bad Request", "").await;
    };
    if !sh.connect_token_ok(&head) {
        sh.record(decision(Route::Connect, &host, port, false, "bad-token"));
        return reply(
            &mut s,
            "407 Proxy Authentication Required",
            "proxy-authenticate: Basic realm=\"han\"\r\n",
        )
        .await;
    }
    if host == MCP_HOST {
        let ok = port == MCP_PORT && head.rest.is_empty();
        let Some(mcp) = sh.mcp.clone().filter(|_| ok) else {
            sh.record(decision(Route::Mcp, &host, port, false, "no-mcp-route"));
            return reply(&mut s, "403 Forbidden", "").await;
        };
        s.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        sh.record(decision(Route::Mcp, &host, port, true, "allowed"));
        mcp(s);
        return Ok(());
    }
    if addr::is_ip_literal(&host) {
        sh.record(decision(Route::Connect, &host, port, false, "ip-literal"));
        return reply(&mut s, "403 Forbidden", "").await;
    }
    let Some(entry) = sh.allow.iter().find(|e| e.host == host) else {
        sh.record(decision(
            Route::Connect,
            &host,
            port,
            false,
            "not-on-allow-list",
        ));
        return reply(&mut s, "403 Forbidden", "").await;
    };
    if !entry.ports.contains(&port) {
        sh.record(decision(
            Route::Connect,
            &host,
            port,
            false,
            "port-not-allowed",
        ));
        return reply(&mut s, "403 Forbidden", "").await;
    }
    let addrs = match sh.resolver.resolve(&host, port).await {
        Ok(a) if !a.is_empty() => a,
        _ => {
            sh.record(decision(Route::Connect, &host, port, false, "unresolved"));
            return reply(&mut s, "502 Bad Gateway", "").await;
        }
    };
    if !sh.allow_private && addrs.iter().any(|a| refused_class(a.ip()).is_some()) {
        sh.record(decision(
            Route::Connect,
            &host,
            port,
            false,
            "private-address",
        ));
        return reply(&mut s, "403 Forbidden", "").await;
    }
    let mut upstream = None;
    for a in &addrs {
        if let Ok(Ok(u)) = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(*a)).await {
            upstream = Some(u);
            break;
        }
    }
    let Some(mut up) = upstream else {
        sh.record(decision(
            Route::Connect,
            &host,
            port,
            false,
            "connect-failed",
        ));
        return reply(&mut s, "502 Bad Gateway", "").await;
    };
    s.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    let hello =
        tokio::time::timeout(HELLO_TIMEOUT, sni::read_client_hello(&mut s, head.rest)).await;
    let (bytes, hello) = match hello {
        Ok(Ok(x)) => x,
        _ => {
            sh.record(decision(
                Route::Connect,
                &host,
                port,
                false,
                "no-client-hello",
            ));
            return Ok(());
        }
    };
    let reason = match &hello {
        sni::Hello::Sni(name) if *name == host => None,
        sni::Hello::Sni(_) => Some("sni-mismatch"),
        sni::Hello::NoSni => Some("sni-missing"),
        sni::Hello::NotTls => Some("not-tls"),
    };
    if let Some(r) = reason {
        sh.record(decision(Route::Connect, &host, port, false, r));
        return Ok(()); // dropping both sockets closes the tunnel
    }
    up.write_all(&bytes).await?;
    let res = tokio::io::copy_bidirectional(&mut s, &mut up).await;
    let (a, b) = res.unwrap_or((0, 0));
    let mut d = decision(Route::Connect, &host, port, true, "allowed");
    d.bytes_up = a + bytes.len() as u64;
    d.bytes_down = b;
    sh.record(d);
    Ok(())
}

/// Headers never forwarded to the provider: credentials, the session token, hop-by-hop and framing.
const DROP: [&str; 13] = [
    "host",
    "x-api-key",
    "authorization",
    "proxy-authorization",
    "proxy-connection",
    "x-han-session",
    "connection",
    "keep-alive",
    "content-length",
    "transfer-encoding",
    "accept-encoding",
    "expect",
    "upgrade",
];

fn path_allowed(path: &str, prefixes: &[String]) -> bool {
    if path.contains("..") || path.contains("//") || path.contains('%') && path.contains("%2") {
        return false;
    }
    prefixes.iter().any(|p| {
        path == p
            || path
                .strip_prefix(p.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

async fn read_body(s: &mut TcpStream, head: &Head) -> io::Result<Vec<u8>> {
    let mut body = head.rest.clone();
    let chunked = head
        .get("transfer-encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    if chunked {
        let mut raw = body;
        let mut out = Vec::new();
        loop {
            // one chunk: size line, data, CRLF
            let line_end = loop {
                if let Some(p) = raw.windows(2).position(|w| w == b"\r\n") {
                    break p;
                }
                read_more(s, &mut raw).await?;
            };
            let size_txt = String::from_utf8_lossy(&raw[..line_end]).into_owned();
            let size = usize::from_str_radix(size_txt.split(';').next().unwrap_or("").trim(), 16)
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
            raw.drain(..line_end + 2);
            if size == 0 {
                return Ok(out);
            }
            if out.len() + size > MAX_BODY {
                return Err(io::ErrorKind::InvalidData.into());
            }
            while raw.len() < size + 2 {
                read_more(s, &mut raw).await?;
            }
            out.extend_from_slice(&raw[..size]);
            raw.drain(..size + 2);
        }
    }
    let len: usize = head
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if len > MAX_BODY {
        return Err(io::ErrorKind::InvalidData.into());
    }
    while body.len() < len {
        read_more(s, &mut body).await?;
    }
    body.truncate(len);
    Ok(body)
}

async fn read_more(s: &mut TcpStream, buf: &mut Vec<u8>) -> io::Result<()> {
    let mut tmp = vec![0u8; 64 * 1024];
    let n = s.read(&mut tmp).await?;
    if n == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    if buf.len() + n > MAX_BODY + 64 * 1024 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    buf.extend_from_slice(&tmp[..n]);
    Ok(())
}

async fn model(mut s: TcpStream, sh: Arc<Shared>, head: Head) -> io::Result<()> {
    let Some((route, client)) = sh.model.as_ref() else {
        sh.record(decision(Route::Other, "", 0, false, "no-model-route"));
        return reply(&mut s, "403 Forbidden", "").await;
    };
    let name = route.name.as_str();
    // origin-form, or absolute-form addressed to the proxy itself
    let pq = match head.target.strip_prefix("http://") {
        Some(rest) => rest.find('/').map(|i| rest[i..].to_owned()),
        None if head.target.starts_with('/') => Some(head.target.clone()),
        None => None,
    };
    let Some(pq) = pq else {
        sh.record(decision(Route::Other, name, 0, false, "not-origin-form"));
        return reply(&mut s, "403 Forbidden", "").await;
    };
    let token_ok = head.get(SESSION_HEADER).is_some_and(|t| same(t, &sh.token));
    if !token_ok {
        sh.record(decision(Route::Model, name, 0, false, "bad-token"));
        return reply(&mut s, "403 Forbidden", "").await;
    }
    let path = pq.split('?').next().unwrap_or("");
    let method_ok = matches!(head.method.as_str(), "POST" | "GET");
    if !method_ok || !path_allowed(path, &route.paths) {
        sh.record(decision(Route::Model, name, 0, false, "path-not-allowed"));
        return reply(&mut s, "403 Forbidden", "").await;
    }
    if sh.budget.as_ref().is_some_and(|b| b.exhausted()) {
        sh.record(decision(Route::Model, name, 0, false, "budget-spent"));
        return reply(&mut s, "429 Too Many Requests", "").await;
    }
    if head
        .get("expect")
        .is_some_and(|v| v.eq_ignore_ascii_case("100-continue"))
    {
        s.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").await?;
    }
    let body = match read_body(&mut s, &head).await {
        Ok(b) => b,
        Err(_) => {
            sh.record(decision(Route::Model, name, 0, false, "bad-body"));
            return reply(&mut s, "400 Bad Request", "").await;
        }
    };
    let kept: Vec<(String, String)> = head
        .headers
        .iter()
        .filter(|(k, _)| !DROP.iter().any(|d| k.eq_ignore_ascii_case(d)))
        .cloned()
        .collect();
    *lock(&sh.last) = Some(LastRequest {
        path_and_query: pq.clone(),
        headers: kept.clone(),
        body: body.clone(),
    });
    let url = format!("{}{}", route.base.trim_end_matches('/'), pq);
    let method =
        reqwest::Method::from_bytes(head.method.as_bytes()).unwrap_or(reqwest::Method::POST);
    let up_len = body.len() as u64;
    let mut req = client.request(method, url).body(body);
    for (k, v) in &kept {
        req = req.header(k.as_str(), v.as_str());
    }
    let key = route.key.get();
    req = match route.style {
        KeyStyle::XApiKey => req.header("x-api-key", key),
        KeyStyle::Bearer => req.header("authorization", format!("Bearer {key}")),
    };
    let mut resp = match req.send().await {
        Ok(r) => r,
        Err(_) => {
            sh.record(decision(Route::Model, name, 0, false, "upstream-failed"));
            return reply(&mut s, "502 Bad Gateway", "").await;
        }
    };
    let status = resp.status();
    let mut out = format!(
        "HTTP/1.1 {} {}\r\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    );
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    for (k, v) in resp.headers() {
        let k = k.as_str();
        if [
            "content-length",
            "transfer-encoding",
            "connection",
            "keep-alive",
        ]
        .contains(&k)
        {
            continue;
        }
        out.push_str(&format!("{k}: {}\r\n", v.to_str().unwrap_or("")));
    }
    out.push_str("connection: close\r\n\r\n");
    s.write_all(out.as_bytes()).await?;
    let mut scan = usage::Scanner::new(&ctype);
    let mut down = 0u64;
    while let Ok(Some(c)) = resp.chunk().await {
        scan.feed(&c);
        down += c.len() as u64;
        if s.write_all(&c).await.is_err() {
            break;
        }
    }
    let used = scan.finish();
    // Spend is recorded before the client sees the end of the response. Closing first lets
    // a client that reads to EOF and immediately calls again pass the budget check before
    // this call is counted (SR-41).
    if let Some(b) = &sh.budget {
        b.record(&used);
    }
    sh.usage.send_modify(|u| u.add(&used));
    let mut d = decision(Route::Model, name, 443, true, "allowed");
    d.bytes_up = up_len;
    d.bytes_down = down;
    d.status = Some(status.as_u16());
    sh.record(d);
    let _ = s.shutdown().await;
    tracing::info!(
        target: "han_host::proxy",
        session = %sh.session,
        route = "model",
        input_tokens = used.input,
        output_tokens = used.output,
        cache_read_tokens = used.cache_read,
        cache_creation_tokens = used.cache_creation,
        "model usage"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        let p = vec!["/v1/messages".to_owned()];
        assert!(path_allowed("/v1/messages", &p));
        assert!(path_allowed("/v1/messages/count_tokens", &p));
        assert!(!path_allowed("/v1/messagesx", &p));
        assert!(!path_allowed("/v1/organizations", &p));
        assert!(!path_allowed("/v1/messages/../files", &p));
        assert!(!path_allowed("/v1/files", &p));
    }

    #[test]
    fn authority() {
        assert_eq!(
            split_authority("Example.com:443"),
            Some(("example.com".into(), 443))
        );
        assert_eq!(split_authority("[::1]:443"), Some(("::1".into(), 443)));
        assert_eq!(split_authority("example.com"), None);
        assert_eq!(split_authority("example.com:x"), None);
    }
}

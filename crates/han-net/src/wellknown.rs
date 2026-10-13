//! `/.well-known/han` and `/.well-known/han-peers` (section 4.3): the documents, a minimal HTTP server for
//! them, and the fetcher that is the one mandatory discovery path (so a federated network forms from the
//! documents alone).
//!
//! The server speaks plain HTTP: put it behind a TLS-terminating proxy to serve the HTTPS the spec asks for.
//! The fetcher takes `https://` URLs (system roots through rustls) and, for loopback and tests, `http://`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use han_core::grammar;
use han_core::json::{self, Value};
use han_core::sign::is_node_id;
use han_seq::{Directory, Home, ServerInfo, ServerKey};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::addr::{NodeAddr, node_addr_verdict};
use crate::error::{NetError, Result};

/// A document may be cached this long (section 4.3).
pub const CACHE: Duration = Duration::from_secs(300);
/// The largest document the fetcher reads.
const MAX_DOC_BYTES: usize = 64 * 1024;

/// `GET /.well-known/han`: the server's nodes and keys.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WellKnown {
    /// The ServerName.
    pub server: String,
    /// Its HAN nodes, 1 to 16.
    pub sequencer: Vec<NodeAddr>,
    /// Its published keys.
    pub keys: Vec<ServerKey>,
}

impl WellKnown {
    /// The node ids of the `sequencer` list.
    pub fn nodes(&self) -> Vec<String> {
        self.sequencer.iter().map(|n| n.node.clone()).collect()
    }
}

/// Why a document is not a well-formed `/.well-known/han`, or `None` if it is (vectors `wellknown.json`).
pub fn wellknown_verdict(doc: &Value) -> Option<&'static str> {
    let Value::Object(m) = doc else {
        return Some("members");
    };
    if m.len() != 3
        || !["server", "sequencer", "keys"]
            .iter()
            .all(|k| m.contains_key(*k))
    {
        return Some("members");
    }
    if !doc["server"].as_str().is_some_and(grammar::is_server_name) {
        return Some("server");
    }
    let Some(seq) = doc["sequencer"]
        .as_array()
        .filter(|l| (1..=16).contains(&l.len()))
    else {
        return Some("sequencer: 1 to 16 nodes");
    };
    if seq.iter().any(|n| node_addr_verdict(n).is_some()) {
        return Some("sequencer: a NodeAddr");
    }
    let nodes: std::collections::HashSet<&str> =
        seq.iter().filter_map(|n| n["node"].as_str()).collect();
    if nodes.len() != seq.len() {
        return Some("sequencer: a node twice");
    }
    let Some(keys) = doc["keys"].as_array().filter(|l| !l.is_empty()) else {
        return Some("keys");
    };
    for k in keys {
        let Value::Object(km) = k else {
            return Some("key members");
        };
        let required = ["keyId", "publicKey", "from"]
            .iter()
            .all(|f| km.contains_key(*f));
        if !required
            || km.keys().any(|f| {
                !["keyId", "publicKey", "from", "until", "compromised"].contains(&f.as_str())
            })
        {
            return Some("key members");
        }
        let key_ok = k["keyId"].as_str().is_some_and(grammar::is_key_id)
            && k["publicKey"].as_str().is_some_and(is_node_id)
            && k["from"].as_str().is_some_and(grammar::is_time);
        if !key_ok {
            return Some("key");
        }
        if let Some(until) = km.get("until")
            && !until
                .as_str()
                .is_some_and(|u| grammar::is_time(u) && u > k["from"].as_str().unwrap_or(""))
        {
            return Some("key until");
        }
        if km
            .get("compromised")
            .is_some_and(|c| *c != Value::Bool(true))
        {
            return Some("key compromised");
        }
    }
    None
}

/// The document a home serves, listing `sequencer` as its nodes.
pub fn document(home: &Home, sequencer: &[NodeAddr]) -> WellKnown {
    WellKnown {
        server: home.server().to_owned(),
        sequencer: sequencer.to_vec(),
        keys: home.server_keys(),
    }
}

/// `GET /.well-known/han-peers` for a server with no peers.
pub fn peers_document() -> Value {
    Value::Object([("peers".to_owned(), Value::Array(vec![]))].into())
}

// ───────────────────────── serving ─────────────────────────

/// A running HTTP server.
pub struct HttpServer {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl HttpServer {
    /// Where it listens.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Maps a request path to a JSON body; `None` is a 404.
pub type Routes = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// The routes of the two documents.
pub fn wellknown_routes(doc: impl Fn() -> WellKnown + Send + Sync + 'static) -> Routes {
    Arc::new(move |path| match path {
        "/.well-known/han" => json::to_value(&doc())
            .ok()
            .and_then(|v| json::canonicalize(&v).ok())
            .and_then(|b| String::from_utf8(b).ok()),
        "/.well-known/han-peers" => json::canonicalize(&peers_document())
            .ok()
            .and_then(|b| String::from_utf8(b).ok()),
        _ => None,
    })
}

/// Serves `routes` over HTTP/1.1 at `addr`. Every answer is `200 application/json; charset=utf-8` or a
/// plain 404, on a `GET`.
pub async fn serve_http(addr: SocketAddr, routes: Routes) -> std::io::Result<HttpServer> {
    let listener = TcpListener::bind(addr).await?;
    let addr = listener.local_addr()?;
    let task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let routes = Arc::clone(&routes);
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                let read = tokio::time::timeout(Duration::from_secs(10), async {
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 8192 {
                        match stream.read(&mut buf).await {
                            Ok(0) | Err(_) => return false,
                            Ok(n) => head.extend_from_slice(&buf[..n]),
                        }
                    }
                    true
                })
                .await;
                if read != Ok(true) {
                    return;
                }
                let text = String::from_utf8_lossy(&head);
                let mut parts = text.lines().next().unwrap_or_default().split(' ');
                let (method, target) = (
                    parts.next().unwrap_or_default(),
                    parts.next().unwrap_or_default(),
                );
                let path = target.split('?').next().unwrap_or_default();
                let response = match (method, routes(path)) {
                    ("GET", Some(body)) => format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    ),
                    ("GET", None) => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
                    _ => "HTTP/1.1 405 Method Not Allowed\r\nAllow: GET\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
                };
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    Ok(HttpServer { addr, task })
}

// ───────────────────────── fetching ─────────────────────────

/// Fetches the well-known documents of other servers.
#[derive(Clone, Debug, Default)]
pub struct Fetcher {
    /// `server -> base URL` instead of `https://<server>`, for loopback and tests.
    overrides: Arc<HashMap<String, String>>,
    /// `server -> document` served from memory instead of fetched, for tests and embedders with no web server.
    documents: Arc<HashMap<String, WellKnown>>,
}

impl Fetcher {
    /// A fetcher that uses `https://<server>`.
    pub fn new() -> Self {
        Self::default()
    }

    /// A fetcher that reads `server`'s documents from `base` (such as `http://127.0.0.1:8080`).
    #[must_use]
    pub fn with_base(self, server: &str, base: &str) -> Self {
        let mut map = (*self.overrides).clone();
        map.insert(server.to_owned(), base.trim_end_matches('/').to_owned());
        Fetcher {
            overrides: Arc::new(map),
            ..self
        }
    }

    /// A fetcher that answers for `doc.server` with `doc`, without fetching.
    #[must_use]
    pub fn with_document(self, doc: WellKnown) -> Self {
        let mut map = (*self.documents).clone();
        map.insert(doc.server.clone(), doc);
        Fetcher {
            documents: Arc::new(map),
            ..self
        }
    }

    fn url(&self, server: &str, path: &str) -> String {
        let base = self
            .overrides
            .get(server)
            .cloned()
            .unwrap_or_else(|| format!("https://{server}"));
        format!("{base}{path}")
    }

    /// `GET /.well-known/han` of `server`: `200 application/json`, well formed, and naming `server`.
    pub async fn wellknown(&self, server: &str) -> Result<WellKnown> {
        if !grammar::is_server_name(server) {
            return Err(NetError::Protocol(format!("not a ServerName: {server}")));
        }
        if let Some(doc) = self.documents.get(server) {
            return Ok(doc.clone());
        }
        let body = self.get(&self.url(server, "/.well-known/han")).await?;
        let doc = json::parse(&body).map_err(|e| NetError::Protocol(format!("well-known: {e}")))?;
        if let Some(why) = wellknown_verdict(&doc) {
            return Err(NetError::Protocol(format!("well-known: {why}")));
        }
        let parsed: WellKnown =
            json::from_value(&doc).map_err(|e| NetError::Protocol(e.to_string()))?;
        if parsed.server != server {
            return Err(NetError::Protocol("well-known names another server".into()));
        }
        Ok(parsed)
    }

    async fn get(&self, url: &str) -> Result<Vec<u8>> {
        // rustls needs a process-wide crypto provider; installing it twice is harmless.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| NetError::Dial(e.to_string()))?;
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|e| NetError::Dial(e.to_string()))?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(NetError::Protocol(format!(
                "GET {url}: {}",
                response.status()
            )));
        }
        let json_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| t.starts_with("application/json"));
        if !json_type {
            return Err(NetError::Protocol(format!(
                "GET {url}: not application/json"
            )));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|e| NetError::Dial(e.to_string()))?;
        if bytes.len() > MAX_DOC_BYTES {
            return Err(NetError::Protocol("document too large".into()));
        }
        Ok(bytes.to_vec())
    }
}

/// The nodes and keys of other servers, from their well-known documents, cached for 300 s. A home reads it
/// through [`Directory`]; refresh a server before a call that needs it.
#[derive(Clone)]
pub struct WellKnownDirectory {
    fetcher: Fetcher,
    cache: Arc<Mutex<HashMap<String, (Instant, ServerInfo)>>>,
}

impl WellKnownDirectory {
    /// A directory that fetches with `fetcher`.
    pub fn new(fetcher: Fetcher) -> Self {
        WellKnownDirectory {
            fetcher,
            cache: Arc::default(),
        }
    }

    /// Fetches `server`'s document unless a fresh copy is cached.
    pub async fn refresh(&self, server: &str) -> Result<WellKnown> {
        let doc = self.fetcher.wellknown(server).await?;
        self.cache.lock().unwrap_or_else(|p| p.into_inner()).insert(
            server.to_owned(),
            (
                Instant::now(),
                ServerInfo {
                    nodes: doc.nodes(),
                    keys: doc.keys.clone(),
                },
            ),
        );
        Ok(doc)
    }
}

impl Directory for WellKnownDirectory {
    fn server(&self, server: &str) -> Option<ServerInfo> {
        let cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        cache
            .get(server)
            .filter(|(at, _)| at.elapsed() < CACHE)
            .map(|(_, info)| info.clone())
    }
}

//! Shared set-up of the live tests: QUIC endpoints on localhost and a home that serves them.

#![allow(dead_code, missing_docs)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use han_core::SigningKey;
use han_core::json::Value;
use han_net::wellknown::{HttpServer, document, serve_http, wellknown_routes};
use han_net::{
    IrohOptions, IrohTransport, NodeAddr, RelayChoice, ServeOptions, ServerHandle, Transport, serve,
};
use han_seq::build::{EntrySpec, sign_entry};
use han_seq::{Home, HomeConfig, SqliteStore, SystemClock};

pub const SERVER: &str = "a.localhost";

pub fn obj<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

pub fn s(t: &str) -> Value {
    Value::Str(t.to_owned())
}

/// An endpoint on loopback whose key is `[seed; 32]`: its node id is that key's public key (N-01).
pub async fn endpoint(seed: u8) -> IrohTransport {
    let mut opts = IrohOptions::new([seed; 32]);
    opts.relay = RelayChoice::Disabled;
    opts.bind = Some(SocketAddr::from(([127, 0, 0, 1], 0)));
    IrohTransport::bind(opts).await.unwrap()
}

pub fn key(seed: u8) -> SigningKey {
    SigningKey::from_seed([seed; 32])
}

/// Who signs an entry: an actor, its key, and the key's id in its profile.
pub struct Signer<'a> {
    pub actor: &'a str,
    pub key: &'a SigningKey,
    pub key_id: &'a str,
}

pub fn entry(
    by: &Signer<'_>,
    space: &str,
    kind: &str,
    body: Value,
    content: Option<Value>,
    seen: u64,
) -> (Value, Option<Value>) {
    sign_entry(
        EntrySpec {
            space: space.into(),
            kind: kind.into(),
            author: by.actor.into(),
            key_id: by.key_id.into(),
            body,
            content,
            refs: vec![],
            seen,
            time_secs: 1_767_225_600,
        },
        by.key,
    )
    .unwrap()
}

/// A running home on loopback.
pub struct Rig {
    pub home: Arc<Home>,
    pub addr: NodeAddr,
    pub http: HttpServer,
    pub dir: PathBuf,
    _server: ServerHandle,
}

impl Rig {
    /// Starts a home for `a.localhost` with a SQLite store in a fresh directory; `people` are registered with
    /// their `d1` key at `[seed; 32]`.
    pub async fn start(people: &[(&str, u8)], opts: ServeOptions) -> Rig {
        let dir = std::env::temp_dir().join(format!(
            "han-live-{}-{}",
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
                Arc::new(SqliteStore::open(&dir).unwrap()),
            )
            .unwrap(),
        );
        for (actor, seed) in people {
            home.register(
                actor,
                "human",
                "d1",
                &key(*seed).public_key().to_base64url(),
                None,
            )
            .unwrap();
        }
        let transport = endpoint(30).await;
        let addr = transport.loopback_addr();
        home.set_nodes(vec![transport.node_id()]);
        let server = serve(transport, Arc::clone(&home), opts);
        let (doc_home, doc_addr) = (Arc::clone(&home), addr.clone());
        let http = serve_http(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            wellknown_routes(move || document(&doc_home, std::slice::from_ref(&doc_addr))),
        )
        .await
        .unwrap();
        Rig {
            home,
            addr,
            http,
            dir,
            _server: server,
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

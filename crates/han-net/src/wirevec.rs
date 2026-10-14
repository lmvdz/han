//! A runner for `conformance/wire-vectors` over any [`Transport`]: the frame vectors of `han/1` (G-11),
//! plus the test-mode home they run against.
//!
//! The vectors are frame-level, so one runner serves the in-process network and QUIC alike. Each label of a
//! vector is its own connection; frames from every stream of a connection arrive in one queue, and an
//! `expect` takes the first received frame that matches it (a frame that matches no step is a failure at
//! the end of the vector).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use han_core::json::{self, Numbers, Value};
use han_core::{PublicKey, Receipt, SigningKey, base64url, grammar, ids};
use han_seq::{Home, HomeConfig, Store, TestClock};
use sha2::{Digest, Sha256};

use crate::addr::NodeAddr;
use crate::client::{Inbound, RawConn};
use crate::error::{NetError, Result};
use crate::transport::{Connection, Transport};

fn seed_key(seed: &str) -> SigningKey {
    SigningKey::from_seed(Sha256::digest(seed.as_bytes()).into())
}

/// One actor of the test-mode contract.
pub struct TestActor {
    /// The ActorId.
    pub actor: String,
    /// `human`, `agent` or `service`.
    pub kind: String,
    /// The key's id.
    pub key_id: String,
    /// Its public key.
    pub public_key: String,
    /// Its signing key (derived from the seed).
    pub key: SigningKey,
    /// The operator of a non-human actor.
    pub operator: Option<String>,
}

/// `config.json` of the wire vectors: the test-mode contract (G-11).
pub struct TestConfig {
    /// The ServerName (`a.localhost`).
    pub server: String,
    /// The clock's start, as a HAN Time.
    pub clock: String,
    /// The server key's id.
    pub server_key_id: String,
    /// The server key.
    pub server_key: SigningKey,
    /// The profile spaces to provision.
    pub actors: Vec<TestActor>,
    raw: Value,
}

impl TestConfig {
    /// Reads `config.json`.
    pub fn load(path: &Path) -> std::io::Result<TestConfig> {
        let bad =
            |what: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, what.to_owned());
        let raw = json::parse_with(&std::fs::read(path)?, Numbers::Any)
            .map_err(|e| bad(&e.to_string()))?;
        let text = |v: &Value, k: &str| v[k].as_str().map(str::to_owned).ok_or_else(|| bad(k));
        let mut actors = Vec::new();
        for a in raw["actors"].as_array().ok_or_else(|| bad("actors"))? {
            actors.push(TestActor {
                actor: text(a, "actor")?,
                kind: text(a, "kind")?,
                key_id: text(a, "keyId")?,
                public_key: text(a, "publicKey")?,
                key: seed_key(&text(a, "seed")?),
                operator: a["operator"].as_str().map(str::to_owned),
            });
        }
        Ok(TestConfig {
            server: text(&raw, "server")?,
            clock: text(&raw, "clock")?,
            server_key_id: text(&raw["serverKey"], "keyId")?,
            server_key: seed_key(&text(&raw["serverKey"], "seed")?),
            actors,
            raw,
        })
    }

    fn actor(&self, who: &str) -> Option<&TestActor> {
        self.actors.iter().find(|a| a.actor == who)
    }

    /// Starts a home in test mode (G-11): ServerName and key from the config, a test clock at the config
    /// clock, and the profile spaces of the listed actors provisioned.
    pub fn home(
        &self,
        store: Arc<dyn Store>,
    ) -> std::result::Result<Arc<Home>, Box<dyn std::error::Error>> {
        let start = grammar::epoch_seconds(&self.clock).ok_or("config clock")?;
        let clock = Arc::new(TestClock::new(u64::try_from(start)?));
        let key = SigningKey::from_seed(
            Sha256::digest(
                self.raw["serverKey"]["seed"]
                    .as_str()
                    .ok_or("seed")?
                    .as_bytes(),
            )
            .into(),
        );
        let home = Home::open_test(
            HomeConfig {
                server: self.server.clone(),
                key,
                key_id: self.server_key_id.clone(),
                durability: han_core::types::Durability::Local,
                // the wire vectors' strangers create spaces
                hosting: han_seq::HostingPolicy::Open,
            },
            clock,
            store,
        )?;
        for a in &self.actors {
            home.register(
                &a.actor,
                &a.kind,
                &a.key_id,
                &a.public_key,
                a.operator.as_deref(),
            )
            .map_err(|e| format!("register {}: {e}", a.actor))?;
        }
        Ok(Arc::new(home))
    }
}

/// The outcome of a run.
#[derive(Default)]
pub struct Report {
    /// Vectors that passed.
    pub passed: usize,
    /// Vectors that failed, with why.
    pub failed: Vec<(String, String)>,
}

impl Report {
    /// `113/113`.
    pub fn summary(&self) -> String {
        format!("{}/{}", self.passed, self.passed + self.failed.len())
    }
}

/// Runs wire vectors against a home reached over `transport`.
pub struct WireRunner<T: Transport> {
    transport: T,
    server: NodeAddr,
    http: Option<SocketAddr>,
    config: TestConfig,
    wall: tokio::sync::Mutex<String>,
    steady: tokio::sync::Mutex<u64>,
    limit: tokio::sync::Mutex<u64>,
}

struct Conn<C: Connection> {
    raw: RawConn<C>,
    challenge: String,
    queue: VecDeque<Value>,
    closed: bool,
    /// request id -> id of the entry sent with it (for the `<receipt>` matcher)
    entries: HashMap<u64, String>,
}

type Fail = String;

impl<T: Transport> WireRunner<T> {
    /// A runner that dials `server` from `transport`; `http` is where the well-known documents are served.
    pub fn new(
        transport: T,
        server: NodeAddr,
        http: Option<SocketAddr>,
        config: TestConfig,
    ) -> Self {
        let wall = config.clock.clone();
        WireRunner {
            transport,
            server,
            http,
            config,
            wall: tokio::sync::Mutex::new(wall),
            steady: tokio::sync::Mutex::new(0),
            limit: tokio::sync::Mutex::new(han_core::limits::MAX_FRAME_BYTES as u64),
        }
    }

    /// Runs the files of `index.json` in `dir`, in order. `only` selects vector ids or file names containing it.
    pub async fn run(
        &self,
        dir: &Path,
        only: Option<&str>,
    ) -> std::result::Result<Report, Box<dyn std::error::Error>> {
        let index = json::parse_with(&std::fs::read(dir.join("index.json"))?, Numbers::Any)?;
        let mut report = Report::default();
        for file in index["files"].as_array().ok_or("index files")? {
            let name = file.as_str().ok_or("file name")?;
            let doc = json::parse_with(&std::fs::read(dir.join(name))?, Numbers::Any)?;
            let vectors: Vec<Value> = match doc.get("vectors") {
                Some(v) => v.as_array().ok_or("vectors")?.to_vec(),
                None => vec![doc.clone()],
            };
            for v in vectors {
                let id = v["id"]
                    .as_str()
                    .or_else(|| v["name"].as_str())
                    .unwrap_or(name)
                    .to_owned();
                if only.is_some_and(|o| !id.contains(o) && !name.contains(o)) {
                    continue;
                }
                let started = std::time::Instant::now();
                let outcome = self.vector(&v).await;
                if std::env::var_os("WIRE_TIMES").is_some() {
                    eprintln!("{name}:{id} {:?}", started.elapsed());
                }
                match outcome {
                    Ok(()) => report.passed += 1,
                    Err(why) => report.failed.push((format!("{name}:{id}"), why)),
                }
            }
        }
        Ok(report)
    }

    async fn open(&self) -> Result<Conn<T::Conn>> {
        let conn = self.transport.dial(&self.server).await?;
        let raw = RawConn::open(conn).await?;
        let challenge = challenge_of(raw.challenge_frame())
            .ok_or_else(|| NetError::Protocol("challenge".into()))?;
        Ok(Conn {
            raw,
            challenge,
            queue: VecDeque::new(),
            closed: false,
            entries: HashMap::new(),
        })
    }

    fn proof(&self, who: &str, challenge: &str, dialed: &str) -> Option<String> {
        // An actor the config does not know signs with the first actor's key (as ref/run_wire_vectors.py does).
        let a = self
            .config
            .actor(who)
            .or_else(|| self.config.actors.first())?;
        Some(han_seq::auth::make_proof(&a.key, challenge, dialed, None))
    }

    async fn authenticate(
        &self,
        c: &mut Conn<T::Conn>,
        who: &str,
    ) -> std::result::Result<(), Fail> {
        let proof = self
            .proof(who, &c.challenge, &self.config.server)
            .ok_or("unknown actor")?;
        let key_id = self
            .config
            .actor(who)
            .map(|a| a.key_id.clone())
            .unwrap_or_default();
        let hello = obj([
            ("id", Value::Int(1)),
            ("call", s("hello")),
            (
                "args",
                obj([
                    ("who", s(who)),
                    ("keyId", s(&key_id)),
                    ("proof", s(&proof)),
                    ("version", Value::Int(1)),
                ]),
            ),
        ]);
        c.raw.send(&text(&hello)).await.map_err(|e| e.to_string())?;
        let reply = recv_json(c, Duration::from_secs(5))
            .await
            .ok_or("no hello reply")?;
        if reply.get("ok").is_none() {
            return Err(format!("set-up hello failed: {}", text(&reply)));
        }
        Ok(())
    }

    /// The runner's own connection, authenticated as `@alice`, for `test.clock` and `test.limits`.
    async fn control(&self, call: &str, args: Value) -> std::result::Result<Value, Fail> {
        let mut c = self.open().await.map_err(|e| e.to_string())?;
        let alice = self
            .config
            .actors
            .first()
            .map(|a| a.actor.clone())
            .ok_or("no actors")?;
        self.authenticate(&mut c, &alice).await?;
        let req = obj([("id", Value::Int(2)), ("call", s(call)), ("args", args)]);
        c.raw.send(&text(&req)).await.map_err(|e| e.to_string())?;
        let reply = recv_json(&mut c, Duration::from_secs(5))
            .await
            .ok_or("no control reply")?;
        c.raw.close();
        Ok(reply)
    }

    async fn vector(&self, v: &Value) -> std::result::Result<(), Fail> {
        let steps = v["steps"].as_array().ok_or("steps")?;
        let mut conns: BTreeMap<String, Conn<T::Conn>> = BTreeMap::new();
        let mut result = Ok(());
        for (i, step) in steps.iter().enumerate() {
            if let Err(why) = self.step(&mut conns, steps, i, step).await {
                result = Err(format!("step {i}: {why}"));
                break;
            }
        }
        if result.is_ok() {
            // A frame that no step expected is a failure.
            tokio::time::sleep(Duration::from_millis(30)).await;
            for (label, c) in &mut conns {
                while let Some(Inbound::Frame(f)) = c.raw.recv(Duration::from_millis(1)).await {
                    c.queue
                        .push_back(json::parse(f.as_bytes()).unwrap_or(Value::Null));
                }
                if let Some(extra) = c.queue.front() {
                    result = Err(format!("{label}: unexpected frame {}", text(extra)));
                    break;
                }
            }
        }
        for c in conns.values() {
            c.raw.close();
        }
        result
    }

    async fn step(
        &self,
        conns: &mut BTreeMap<String, Conn<T::Conn>>,
        steps: &[Value],
        at: usize,
        step: &Value,
    ) -> std::result::Result<(), Fail> {
        if let Some(path) = step.get("http_get") {
            return self.http_step(path.as_str().ok_or("http_get")?, step).await;
        }
        if let Some(c) = step.get("clock") {
            return self.clock_step(c).await;
        }
        if let Some(l) = step.get("limits") {
            return self.limits_step(l).await;
        }
        let label = step["conn"].as_str().ok_or("conn")?.to_owned();
        if !conns.contains_key(&label) {
            let mut c = self.open().await.map_err(|e| e.to_string())?;
            let first = steps
                .iter()
                .find(|s| s["conn"].as_str() == Some(&label))
                .unwrap_or(step);
            let first_is_hello = first["send"]["call"].as_str() == Some("hello");
            let first_is_challenge = first
                .get("expect")
                .is_some_and(|e| e.get("challenge").is_some());
            if grammar::is_actor_id(&label) && !first_is_hello && !first_is_challenge {
                self.authenticate(&mut c, &label).await?;
            } else if first_is_challenge {
                c.queue.push_back(
                    json::parse(c.raw.challenge_frame().as_bytes()).map_err(|e| e.to_string())?,
                );
            }
            conns.insert(label.clone(), c);
        }
        let c = conns.get_mut(&label).ok_or("conn")?;
        let _ = at;
        if let Some(frame) = step.get("send") {
            let frame = self.expand(frame, &c.challenge)?;
            if frame["call"].as_str() == Some("append")
                && let (Some(id), Ok(eid)) = (
                    frame["id"].as_u64(),
                    han_seq::build::entry_id(&frame["args"]["entry"]),
                )
            {
                c.entries.insert(id, eid);
            }
            let wire = text(&frame);
            c.raw.send(&wire).await.map_err(|e| e.to_string())?;
            if matches!(frame["call"].as_str(), Some("subscribe" | "watch"))
                || (frame["call"].as_str() == Some("read")
                    && frame["args"]["follow"] == Value::Bool(true))
            {
                tokio::time::sleep(Duration::from_millis(250)).await; // settleMs: the registration has happened
            }
        } else if let Some(raw) = step.get("send_raw") {
            c.raw
                .send(raw.as_str().ok_or("send_raw")?)
                .await
                .map_err(|e| e.to_string())?;
        } else if let Some(hex) = step.get("send_binary") {
            c.raw
                .send_payload(&from_hex(hex.as_str().ok_or("send_binary")?))
                .await
                .map_err(|e| e.to_string())?;
        } else if let Some(g) = step.get("generate") {
            self.generate(c, g).await?;
        } else if let Some(want) = step.get("expect") {
            self.expect(c, want).await?;
        } else if step.get("expect_close").is_some() {
            expect_close(c).await?;
        } else if step.get("expect_silence").is_some() {
            if let Some(Inbound::Frame(f)) = c.raw.recv(Duration::from_millis(400)).await {
                return Err(format!("expected silence, got {f}"));
            }
        } else {
            return Err("unknown step".into());
        }
        Ok(())
    }

    /// Expands the placeholders of a client frame (README, "Placeholders").
    fn expand(&self, v: &Value, challenge: &str) -> std::result::Result<Value, Fail> {
        let who = v["args"]["who"].as_str().unwrap_or_default().to_owned();
        let offset = v["args"]["offset"].as_u64().unwrap_or(0);
        self.expand_in(v, challenge, &who, offset)
    }

    fn expand_in(
        &self,
        v: &Value,
        challenge: &str,
        who: &str,
        offset: u64,
    ) -> std::result::Result<Value, Fail> {
        Ok(match v {
            Value::Str(t) if t == "<proof>" => s(&self
                .proof(who, challenge, &self.config.server)
                .ok_or("proof")?),
            Value::Str(t) if t.starts_with("<proof challenge=") => {
                let other = t
                    .trim_start_matches("<proof challenge=")
                    .trim_end_matches('>');
                s(&self.proof(who, other, &self.config.server).ok_or("proof")?)
            }
            Value::Str(t) if t.starts_with("<proof dialed=") => {
                let other = t.trim_start_matches("<proof dialed=").trim_end_matches('>');
                s(&self.proof(who, challenge, other).ok_or("proof")?)
            }
            Value::Str(t) if t.starts_with("<b64u len=") => {
                let (len, hash) = parse_b64u_marker(t).ok_or("b64u marker")?;
                let bytes: Vec<u8> = (0..len * 3 / 4)
                    .map(|i| u8::try_from(((offset + i as u64) * 7 + 3) % 251).unwrap_or(0))
                    .collect();
                let data = base64url::encode(&bytes);
                if hex_prefix(&data) != hash {
                    return Err("blob expansion does not hash to the marker".into());
                }
                s(&data)
            }
            Value::Str(t) if t.starts_with("<repeat ") => {
                let mut parts = t
                    .trim_start_matches("<repeat ")
                    .trim_end_matches('>')
                    .split(' ');
                let (c, n) = (
                    parts.next().unwrap_or("a"),
                    parts.next().and_then(|n| n.parse().ok()).unwrap_or(0),
                );
                s(&c.repeat(n))
            }
            Value::Object(m) => Value::Object(
                m.iter()
                    .map(|(k, x)| Ok((k.clone(), self.expand_in(x, challenge, who, offset)?)))
                    .collect::<std::result::Result<_, Fail>>()?,
            ),
            Value::Array(a) => Value::Array(
                a.iter()
                    .map(|x| self.expand_in(x, challenge, who, offset))
                    .collect::<std::result::Result<_, Fail>>()?,
            ),
            other => other.clone(),
        })
    }

    async fn expect(&self, c: &mut Conn<T::Conn>, want: &Value) -> std::result::Result<(), Fail> {
        let limit = *self.limit.lock().await;
        let mut deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut last = String::from("nothing");
        loop {
            for i in 0..c.queue.len() {
                match self.matches(want, &c.queue[i], c) {
                    Ok(()) => {
                        c.queue.remove(i);
                        return Ok(());
                    }
                    Err(why) => last = format!("{why}; got {}", text(&c.queue[i])),
                }
            }
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return Err(format!("no matching frame ({last}); wanted {}", text(want)));
            }
            match c.raw.recv(left).await {
                Some(Inbound::Frame(f)) => {
                    if f.len() as u64 > limit {
                        return Err(format!(
                            "a server frame over the lowered limit ({} > {limit})",
                            f.len()
                        ));
                    }
                    let v = json::parse_with(f.as_bytes(), Numbers::IntegersOnly)
                        .map_err(|e| format!("a frame that is not I-JSON: {e}"))?;
                    c.queue.push_back(mask(v));
                }
                Some(Inbound::Closed) => {
                    // The reply that came before the close may still be on its way.
                    if c.closed {
                        return Err(format!("connection closed ({last}); wanted {}", text(want)));
                    }
                    c.closed = true;
                    deadline =
                        deadline.min(tokio::time::Instant::now() + Duration::from_millis(300));
                }
                None => {}
            }
        }
    }

    fn matches(
        &self,
        want: &Value,
        got: &Value,
        c: &Conn<T::Conn>,
    ) -> std::result::Result<(), Fail> {
        match (want, got) {
            (Value::Str(m), _)
                if m.starts_with('<') && m.ends_with('>') && !m.starts_with("<b64u") =>
            {
                self.matcher(m, got, c)
            }
            (Value::Object(w), Value::Object(g)) => {
                if w.len() != g.len() {
                    return Err(format!(
                        "members differ: want {:?}, got {:?}",
                        w.keys().collect::<Vec<_>>(),
                        g.keys().collect::<Vec<_>>()
                    ));
                }
                for (k, wv) in w {
                    let gv = g.get(k).ok_or_else(|| format!("missing {k}"))?;
                    // the receipt matcher needs the request id of the frame it sits in
                    if let (Value::Str(m), Some(id)) = (wv, got["id"].as_u64())
                        && m.starts_with("<receipt")
                    {
                        self.receipt(m, gv, c.entries.get(&id).map(String::as_str))?;
                        continue;
                    }
                    self.matches(wv, gv, c).map_err(|e| format!("{k}: {e}"))?;
                }
                Ok(())
            }
            (Value::Array(w), Value::Array(g)) if w.len() == g.len() => {
                w.iter().zip(g).enumerate().try_for_each(|(i, (a, b))| {
                    self.matches(a, b, c).map_err(|e| format!("[{i}]: {e}"))
                })
            }
            _ if want == got => Ok(()),
            _ => Err(format!("want {}, got {}", text(want), text(got))),
        }
    }

    fn matcher(&self, m: &str, got: &Value, _c: &Conn<T::Conn>) -> std::result::Result<(), Fail> {
        let inner = m.trim_start_matches('<').trim_end_matches('>');
        let word = inner.split(' ').next().unwrap_or_default();
        let g = got.as_str();
        match word {
            "challenge" => g
                .and_then(|g| base64url::decode(g).ok().filter(|b| b.len() == 32))
                .map(|_| ())
                .ok_or_else(|| "challenge: not 43 characters of canonical base64url".into()),
            "challenge-shape" => g
                .filter(|g| {
                    (22..=128).contains(&g.len())
                        && g.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                })
                .map(|_| ())
                .ok_or_else(|| "challenge shape".into()),
            "posint" => got
                .as_u64()
                .filter(|n| *n > 0)
                .map(|_| ())
                .ok_or_else(|| "posint".into()),
            "url-of-the-binding" => {
                // `han/1` replaces the WebSocket URL by node ids (D13a): the `sequencer` list of NodeAddrs.
                let ok = got.as_array().is_some_and(|l| {
                    !l.is_empty()
                        && l.iter()
                            .all(|n| crate::addr::node_addr_verdict(n).is_none())
                });
                ok.then_some(())
                    .ok_or_else(|| "sequencer: a list of NodeAddrs".into())
            }
            "profile-bootstrap" => {
                let mut parts = inner.split(' ').skip(1);
                self.bootstrap(
                    got,
                    parts.next().unwrap_or_default(),
                    parts.next().unwrap_or_default(),
                )
            }
            other => Err(format!("unknown matcher {other}")),
        }
    }

    /// `<receipt>`, `<receipt seq=N>`, `<receipt seq=N time=T>`.
    fn receipt(
        &self,
        m: &str,
        got: &Value,
        entry_id: Option<&str>,
    ) -> std::result::Result<(), Fail> {
        let r: Receipt = json::from_value(got).map_err(|e| format!("receipt: {e}"))?;
        let pk = self.server_public()?;
        r.verify(&pk).map_err(|_| "homeSig does not verify")?;
        if r.home != self.config.server || r.home_key != self.config.server_key_id {
            return Err("receipt home or homeKey".into());
        }
        if entry_id.is_some_and(|e| e != r.id) {
            return Err("receipt id is not the id of the entry sent".into());
        }
        for part in m
            .trim_start_matches("<receipt")
            .trim_end_matches('>')
            .split_whitespace()
        {
            match part.split_once('=') {
                Some(("seq", n)) if n.parse() == Ok(r.seq) => {}
                Some(("time", t)) if t == r.time => {}
                _ => return Err(format!("receipt {part}: got seq {} time {}", r.seq, r.time)),
            }
        }
        Ok(())
    }

    fn server_public(&self) -> std::result::Result<PublicKey, Fail> {
        Ok(self.config.server_key.public_key())
    }

    /// `<profile-bootstrap @actor kind>`: seq 1 of the actor's profile space (section 4.3).
    fn bootstrap(&self, got: &Value, actor: &str, kind: &str) -> std::result::Result<(), Fail> {
        let a = self.config.actor(actor).ok_or("bootstrap actor")?;
        let rec: han_core::LogRecord = json::from_value(got).map_err(|e| format!("record: {e}"))?;
        let e = &rec.entry;
        let r = &rec.receipt;
        let server_actor = format!("@:{}", self.config.server);
        let body = &e.body;
        let ok = e.kind == "space.create"
            && e.author == server_actor
            && e.key_id == self.config.server_key_id
            && e.seen == 0
            && e.refs.is_empty()
            && e.space == format!("profile:{actor}")
            && body["profile"]["actorKind"].as_str() == Some(kind)
            && body["profile"]["key"]["keyId"].as_str() == Some(&a.key_id)
            && body["profile"]["key"]["publicKey"].as_str() == Some(&a.public_key)
            && body["policy"].as_object().is_some()
            && rec.content == Some(obj([("name", s(actor))]))
            && !rec.redacted
            && r.seq == 1
            && r.prev.is_empty()
            && r.home_key == self.config.server_key_id
            && r.profiles == BTreeMap::from([(server_actor, 0)]);
        if !ok {
            return Err("not the profile bootstrap record".into());
        }
        let pk = self.server_public()?;
        e.verify(&pk).map_err(|_| "bootstrap entry signature")?;
        r.verify(&pk).map_err(|_| "bootstrap homeSig")?;
        Ok(())
    }

    async fn http_step(&self, path: &str, step: &Value) -> std::result::Result<(), Fail> {
        let http = self.http.ok_or("no HTTP address for this run")?;
        // rustls needs a process-wide crypto provider; installing it twice is harmless.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let url = format!("http://{http}{path}");
        let body = reqwest::get(&url).await.map_err(|e| e.to_string())?;
        let status = body.status().as_u16().to_string();
        let ctype = body
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let bytes = body.bytes().await.map_err(|e| e.to_string())?;
        if step["expect_status"].as_str() != Some(&status)
            && step["expect_status"].as_i64().map(|n| n.to_string()) != Some(status.clone())
        {
            return Err(format!("status {status}"));
        }
        if !ctype.starts_with(step["expect_type"].as_str().unwrap_or("application/json")) {
            return Err(format!("content type {ctype}"));
        }
        let got = json::parse(&bytes).map_err(|e| e.to_string())?;
        self.match_free(&step["expect_json"], &got)
    }

    /// Like `matches`, for documents that sit outside a connection.
    fn match_free(&self, want: &Value, got: &Value) -> std::result::Result<(), Fail> {
        match (want, got) {
            (Value::Str(m), _) if m.starts_with('<') && m.ends_with('>') => {
                match m.trim_matches(['<', '>']) {
                    "url-of-the-binding" => got
                        .as_array()
                        .filter(|l| {
                            !l.is_empty()
                                && l.iter()
                                    .all(|n| crate::addr::node_addr_verdict(n).is_none())
                        })
                        .map(|_| ())
                        .ok_or_else(|| "sequencer: a list of NodeAddrs".into()),
                    other => Err(format!("unknown matcher {other}")),
                }
            }
            (Value::Object(w), Value::Object(g)) if w.len() == g.len() => {
                w.iter().try_for_each(|(k, wv)| {
                    self.match_free(wv, g.get(k).ok_or_else(|| format!("missing {k}"))?)
                        .map_err(|e| format!("{k}: {e}"))
                })
            }
            (Value::Array(w), Value::Array(g)) if w.len() == g.len() => {
                w.iter().zip(g).try_for_each(|(a, b)| self.match_free(a, b))
            }
            _ if want == got => Ok(()),
            _ => Err(format!("want {}, got {}", text(want), text(got))),
        }
    }

    async fn clock_step(&self, c: &Value) -> std::result::Result<(), Fail> {
        let mut args = BTreeMap::new();
        let mut wall = self.wall.lock().await;
        let mut steady = self.steady.lock().await;
        if let Some(w) = c.get("wall") {
            args.insert("wall".to_owned(), w.clone());
        }
        let advance = c["steadyAdvance"].as_u64();
        if let Some(a) = advance {
            args.insert(
                "steadyAdvance".to_owned(),
                Value::Int(i64::try_from(a).unwrap_or(0)),
            );
        }
        let reply = self.control("test.clock", Value::Object(args)).await?;
        if let Some(w) = c["wall"].as_str() {
            *wall = w.to_owned();
        }
        *steady += advance.unwrap_or(0);
        let want = obj([
            ("wall", s(&wall)),
            ("steady", Value::Int(i64::try_from(*steady).unwrap_or(0))),
        ]);
        if reply.get("ok") != Some(&want) {
            return Err(format!(
                "test.clock: want {}, got {}",
                text(&want),
                text(&reply)
            ));
        }
        Ok(())
    }

    async fn limits_step(&self, l: &Value) -> std::result::Result<(), Fail> {
        let mut limit = self.limit.lock().await;
        let args = l.as_object().cloned().unwrap_or_default();
        let reply = self.control("test.limits", Value::Object(args)).await?;
        if let Some(n) = l["maxFrameBytes"].as_u64() {
            *limit = n;
        }
        let want = obj([(
            "maxFrameBytes",
            Value::Int(i64::try_from(*limit).unwrap_or(0)),
        )]);
        if reply.get("ok") != Some(&want) {
            return Err(format!(
                "test.limits: want {}, got {}",
                text(&want),
                text(&reply)
            ));
        }
        Ok(())
    }

    /// `generate`: appends the runner builds itself (README, "Steps").
    async fn generate(&self, c: &mut Conn<T::Conn>, g: &Value) -> std::result::Result<(), Fail> {
        let count = g["count"].as_u64().ok_or("count")?;
        let (head, first) = (
            g["head"].as_u64().ok_or("head")?,
            g["firstId"].as_u64().ok_or("firstId")?,
        );
        let author = g["author"].as_str().ok_or("author")?;
        let a = self.config.actor(author).ok_or("author")?;
        let start = grammar::epoch_seconds(&self.config.clock).ok_or("clock")?;
        for k in 0..count {
            let sub = |v: &Value| -> Value { subst(v, k) };
            let content = g.get("content").map(sub);
            let nonce_in = g["nonce"]
                .as_str()
                .ok_or("nonce")?
                .replace("{k}", &k.to_string());
            let digest = Sha256::digest(format!("wire-vector-nonce-{nonce_in}").as_bytes());
            let nonce: String = base32(&digest).chars().take(26).collect();
            let mut entry = han_core::Entry {
                v: 1,
                space: g["space"].as_str().ok_or("space")?.to_owned(),
                kind: g["kind"].as_str().ok_or("kind")?.to_owned(),
                author: author.to_owned(),
                key_id: a.key_id.clone(),
                body: sub(&g["body"]),
                content_hash: ids::content_hash(content.as_ref()).map_err(|e| e.to_string())?,
                refs: vec![],
                seen: head + k,
                nonce,
                time: han_seq::time::format_time(start),
                sig: String::new(),
            };
            entry.sign(&a.key).map_err(|e| e.to_string())?;
            let eid = entry.id().map_err(|e| e.to_string())?;
            let id = first + k;
            let mut args = BTreeMap::from([(
                "entry".to_owned(),
                json::to_value(&entry).map_err(|e| e.to_string())?,
            )]);
            if let Some(content) = content {
                args.insert("content".to_owned(), content);
            }
            let req = obj([
                ("id", Value::Int(i64::try_from(id).unwrap_or(0))),
                ("call", s("append")),
                ("args", Value::Object(args)),
            ]);
            c.entries.insert(id, eid);
            c.raw.send(&text(&req)).await.map_err(|e| e.to_string())?;
            let want = obj([
                ("id", Value::Int(i64::try_from(id).unwrap_or(0))),
                ("ok", s(&format!("<receipt seq={}>", head + k + 1))),
            ]);
            self.expect(c, &want)
                .await
                .map_err(|e| format!("generated append {k}: {e}"))?;
        }
        Ok(())
    }
}

fn subst(v: &Value, k: u64) -> Value {
    match v {
        Value::Str(t) => Value::Str(t.replace("{k}", &k.to_string())),
        Value::Object(m) => {
            Value::Object(m.iter().map(|(a, b)| (a.clone(), subst(b, k))).collect())
        }
        Value::Array(l) => Value::Array(l.iter().map(|b| subst(b, k)).collect()),
        other => other.clone(),
    }
}

fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for b in bytes {
        acc = (acc << 8) | u32::from(*b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((acc >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((acc << (5 - bits)) & 31) as usize] as char);
    }
    out
}

fn from_hex(h: &str) -> Vec<u8> {
    (0..h.len() / 2)
        .filter_map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).ok())
        .collect()
}

fn hex_prefix(data: &str) -> String {
    ids::sha256_hex(data.as_bytes())[..16].to_owned()
}

fn parse_b64u_marker(t: &str) -> Option<(usize, String)> {
    let inner = t.strip_prefix("<b64u len=")?.strip_suffix('>')?;
    let (len, hash) = inner.split_once(" sha256=")?;
    Some((len.parse().ok()?, hash.to_owned()))
}

/// Masks a received frame (README, "Comparison"): `error.message` is removed, and blob `data` longer than
/// 300 characters becomes `<b64u len=N sha256=H>`.
fn mask(mut v: Value) -> Value {
    if let Value::Object(m) = &mut v {
        if let Some(Value::Object(e)) = m.get_mut("error") {
            e.remove("message");
        }
        if let Some(Value::Object(ok)) = m.get_mut("ok")
            && let Some(Value::Str(d)) = ok.get("data")
            && d.len() > 300
        {
            let marker = format!("<b64u len={} sha256={}>", d.len(), hex_prefix(d));
            ok.insert("data".to_owned(), Value::Str(marker));
        }
    }
    v
}

fn challenge_of(frame: &str) -> Option<String> {
    json::parse(frame.as_bytes()).ok()?["challenge"]
        .as_str()
        .map(str::to_owned)
}

async fn recv_json<C: Connection>(c: &mut Conn<C>, within: Duration) -> Option<Value> {
    match c.raw.recv(within).await? {
        Inbound::Frame(f) => json::parse(f.as_bytes()).ok(),
        Inbound::Closed => None,
    }
}

async fn expect_close<C: Connection>(c: &mut Conn<C>) -> std::result::Result<(), Fail> {
    if c.closed {
        return Ok(());
    }
    match c.raw.recv(Duration::from_secs(5)).await {
        Some(Inbound::Closed) => Ok(()),
        Some(Inbound::Frame(f)) => Err(format!("expected the connection to close, got {f}")),
        None => Err("the connection did not close".into()),
    }
}

fn obj<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

fn s(t: &str) -> Value {
    Value::Str(t.to_owned())
}

fn text(v: &Value) -> String {
    json::canonicalize(v)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_default()
}

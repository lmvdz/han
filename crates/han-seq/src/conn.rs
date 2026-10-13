//! One connection's view of a home, independent of what carries it: the call dispatcher of G-07 and G-08.
//!
//! A carrier feeds each request frame to [`Conn::handle`] and writes back what it returns. The frame
//! vectors (G-11) hold for any carrier because everything they pin happens here.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use han_core::admit::{AdmitConn, AdmitVerdict, Hello, Tier, admit};
use han_core::frame::request_verdict;
use han_core::json::{self, Value};
use han_core::{base64url, grammar};

use crate::calls::{
    Authed, Feed, MAX_BLOB_PART_BYTES, MAX_FOLLOWS, MAX_SIGNAL_BYTES, ReadReq, StateSel,
};
use crate::error::{Result, WireError};
use crate::fanout::{Live, Next, SubId};
use crate::home::Home;
use crate::jv;
use crate::services::parse_blobref;
use crate::time::{format_time, parse_time};

/// A challenge is valid this long (G-08).
const CHALLENGE_MS: u64 = 60_000;
/// Calls an open-tier node may make per window (N-02: an open call is rate-limited per node).
const OPEN_CALLS: u32 = 600;
/// The window of that limit.
const OPEN_WINDOW_MS: u64 = 60_000;

static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

/// The members each call's `args` may hold (D2: every `args` is closed).
fn allowed_args(call: &str) -> &'static [&'static str] {
    match call {
        "hello" => &["who", "keyId", "proof", "version", "knownHeads"],
        "cancel" => &["target"],
        "append" => &["entry", "content"],
        "signal" => &["space", "body", "from"],
        "blobPut" => &["ref", "offset", "data"],
        "blobGet" => &["ref", "space", "offset", "length"],
        "stream" => &["item"],
        "read" => &["space", "after", "follow", "limit", "knownHeads"],
        "state" => &["space", "at", "cursor", "effect", "agent"],
        "subscribe" => &["space", "after"],
        "watch" => &["space", "session"],
        "host" => &["space", "records", "head"],
        "invite" => &["space", "hash", "expiresAt"],
        "join" => &["space", "secret"],
        "test.clock" => &["wall", "steadyAdvance"],
        "test.limits" => &["maxFrameBytes"],
        _ => &[],
    }
}

/// What a carrier does with one request frame.
pub struct Handled {
    /// Frames to write on the call stream, in order. The stream then finishes.
    pub reply: Vec<String>,
    /// Close the whole connection after writing them.
    pub close: bool,
    /// A streaming call that stays open: its backlog, then live items.
    pub feed: Option<CallFeed>,
}

/// The deliveries of `read` with `follow`, `subscribe` and `watch`, which go on a stream the server opens
/// for the call (G-07).
pub struct CallFeed {
    /// The request id every frame carries.
    pub request_id: u64,
    /// Frames to write first.
    pub backlog: Vec<String>,
    live: Live,
    done: bool,
}

impl CallFeed {
    /// The next frame, waiting for it; `None` when the call has ended (cancelled, the connection closing, or
    /// after the final `unavailable` of a subscriber that fell behind).
    pub async fn next_frame(&mut self) -> Option<String> {
        if self.done {
            return None;
        }
        match self.live.next().await {
            Next::Item(item) => Some(jv::text(&jv::obj([
                ("id", jv::int(self.request_id)),
                ("item", item),
            ]))),
            Next::Overflow => {
                self.done = true;
                Some(error_frame(
                    self.request_id,
                    &WireError::unavailable("subscriber too slow; re-read"),
                ))
            }
            Next::Ended => {
                self.done = true;
                None
            }
        }
    }
}

struct State {
    who: Option<String>,
    /// Whether the peer's node was among those that vouch for `who` when it said `hello`.
    bound: bool,
    challenge: Option<String>,
    issued_ms: u64,
    open: HashMap<u64, SubId>,
}

/// One connection.
pub struct Conn {
    home: Arc<Home>,
    peer: String,
    /// This end's node id on the connection, which `hello` proofs bind (Z4). `None` on the legacy carrier,
    /// and in test mode, whose wire vectors speak that carrier's hello.
    node: Option<String>,
    id: u64,
    state: Mutex<State>,
}

fn ok_frame(id: u64, v: Value) -> String {
    jv::text(&jv::obj([("id", jv::int(id)), ("ok", v)]))
}

fn error_frame(id: u64, e: &WireError) -> String {
    jv::text(&jv::obj([("id", jv::int(id)), ("error", e.to_value())]))
}

impl Home {
    /// A new connection from the node `peer` (its authenticated node id) to this home's own node `node`.
    /// Returns it with the bare Challenge frame the carrier sends first, on the control stream.
    pub fn connect(self: &Arc<Self>, peer: &str, node: &str) -> (Conn, String) {
        let mut raw = [0u8; 32];
        // The system random source failing is not recoverable.
        getrandom::fill(&mut raw).expect("system random source");
        let challenge = base64url::encode(&raw);
        let frame = jv::text(&jv::obj([
            ("challenge", jv::s(&challenge)),
            ("server", jv::s(&self.server)),
            ("versions", Value::Array(vec![jv::int(1)])),
            ("time", jv::s(&self.now_str())),
        ]));
        let conn = Conn {
            home: Arc::clone(self),
            peer: peer.to_owned(),
            node: (!self.is_test_mode()).then(|| node.to_owned()),
            id: NEXT_CONN.fetch_add(1, Ordering::Relaxed),
            state: Mutex::new(State {
                who: None,
                bound: false,
                challenge: Some(challenge),
                issued_ms: self.steady_ms(),
                open: HashMap::new(),
            }),
        };
        (conn, frame)
    }
}

impl Conn {
    fn st(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The authenticated actor, after a successful `hello`.
    pub fn actor(&self) -> Option<String> {
        self.st().who.clone()
    }

    /// Whether the peer's node still vouches for the actor it said `hello` for (X14). A carrier calls this
    /// at least every 300 s and closes the connection when it is false.
    pub fn still_vouched(&self) -> bool {
        let (who, bound) = {
            let st = self.st();
            (st.who.clone(), st.bound)
        };
        match who {
            Some(who) if bound && !self.home.is_test_mode() => {
                self.home.nodes_for(&who).contains(&self.peer)
            }
            _ => true,
        }
    }

    /// Ends the live call with request id `id` (its feed finished).
    pub fn end_feed(&self, id: u64) {
        if let Some(sub) = self.st().open.remove(&id) {
            self.home.unsubscribe(sub);
        }
    }

    /// The connection is gone: ends every live call.
    pub fn close(&self) {
        let open: Vec<SubId> = self.st().open.drain().map(|(_, s)| s).collect();
        for sub in open {
            self.home.unsubscribe(sub);
        }
    }

    /// Handles one request frame (G-07): the frame checks, authentication, the peer filter, the args, the
    /// call.
    pub fn handle(&self, text: &str) -> Handled {
        let finish = |reply: Vec<String>, close: bool| Handled {
            reply,
            close,
            feed: None,
        };
        if let Some(rejected) = request_verdict(text, self.home.is_test_mode()) {
            let e = WireError::invalid("not a request frame");
            return finish(vec![error_frame(rejected.id, &e)], rejected.close);
        }
        // A request frame: `request_verdict` has checked its shape.
        let Ok((frame, _)) = json::parse_frame(text.as_bytes()) else {
            return finish(vec![error_frame(0, &WireError::invalid("frame"))], true);
        };
        let id = frame["id"].as_u64().unwrap_or(0);
        let call = frame["call"].as_str().unwrap_or_default().to_owned();
        let args = &frame["args"];
        match self.dispatch(id, &call, args) {
            Ok(Done::Ok(v)) => finish(vec![ok_frame(id, v)], false),
            Ok(Done::Items(items)) => {
                let mut reply: Vec<String> = items
                    .into_iter()
                    .map(|item| jv::text(&jv::obj([("id", jv::int(id)), ("item", item)])))
                    .collect();
                reply.push(jv::text(&jv::obj([
                    ("id", jv::int(id)),
                    ("end", Value::Bool(true)),
                ])));
                finish(reply, false)
            }
            Ok(Done::Feed(feed)) => {
                let Feed { items, live } = feed;
                let (sub, live) = live.map_or((None, None), |(s, l)| (Some(s), Some(l)));
                let Some(live) = live else {
                    return finish(vec![], false);
                };
                if let Some(sub) = sub {
                    self.st().open.insert(id, sub);
                }
                let backlog = items
                    .into_iter()
                    .map(|item| jv::text(&jv::obj([("id", jv::int(id)), ("item", item)])))
                    .collect();
                Handled {
                    reply: vec![],
                    close: false,
                    feed: Some(CallFeed {
                        request_id: id,
                        backlog,
                        live,
                        done: false,
                    }),
                }
            }
            Err((e, close)) => finish(vec![error_frame(id, &e)], close),
        }
    }

    fn dispatch(
        &self,
        id: u64,
        call: &str,
        args: &Value,
    ) -> std::result::Result<Done, (WireError, bool)> {
        let plain = |e: WireError| (e, false);
        if call != "hello" {
            self.authenticated().map_err(plain)?;
            self.admit(call, args).map_err(plain)?;
        }
        let Value::Object(members) = args else {
            return Err(plain(WireError::invalid("args must be an object")));
        };
        let extra: Vec<&str> = members
            .keys()
            .map(String::as_str)
            .filter(|k| !allowed_args(call).contains(k))
            .collect();
        if !extra.is_empty() {
            return Err(plain(WireError::invalid(format!(
                "unknown args for {call}: {}",
                extra.join(", ")
            ))));
        }
        let who = self.st().who.clone().unwrap_or_default();
        let home = &self.home;
        match call {
            "hello" => self.hello(args).map(Done::Ok).map_err(plain),
            "cancel" => {
                let target = args["target"]
                    .as_u64()
                    .ok_or_else(|| plain(WireError::invalid("target")))?;
                self.end_feed(target);
                Ok(Done::Ok(Value::Null))
            }
            "append" => {
                let receipt = home
                    .append(&args["entry"], args.get("content"))
                    .map_err(plain)?;
                Ok(Done::Ok(
                    json::to_value(&receipt).map_err(|e| plain(WireError::from_json(e)))?,
                ))
            }
            "read" => {
                let space = space_arg(args).map_err(plain)?;
                let follow = match args.get("follow") {
                    None => false,
                    Some(Value::Bool(b)) => *b,
                    Some(_) => return Err(plain(WireError::invalid("follow"))),
                };
                self.room(id, follow).map_err(plain)?;
                let after = args["after"]
                    .as_u64()
                    .ok_or_else(|| plain(WireError::invalid("after")))?;
                let limit = opt_nat(args, "limit").map_err(plain)?;
                let req = ReadReq {
                    after,
                    follow,
                    limit,
                    known_heads: args.get("knownHeads"),
                };
                let feed = home.read(&who, self.id, space, &req).map_err(plain)?;
                Ok(if follow {
                    Done::Feed(feed)
                } else {
                    Done::Items(feed.items)
                })
            }
            "state" => {
                let space = space_arg(args).map_err(plain)?;
                let sel = StateSel {
                    at: args.get("at"),
                    cursor: args.get("cursor"),
                    effect: args.get("effect"),
                    agent: args.get("agent"),
                };
                home.state(&who, space, &sel).map(Done::Ok).map_err(plain)
            }
            "subscribe" => {
                let space = space_arg(args).map_err(plain)?;
                self.room(id, true).map_err(plain)?;
                let after = opt_nat(args, "after").map_err(plain)?;
                home.subscribe(&who, self.id, space, after)
                    .map(Done::Feed)
                    .map_err(plain)
            }
            "watch" => {
                let space = space_arg(args).map_err(plain)?;
                self.room(id, true).map_err(plain)?;
                let session = args["session"]
                    .as_str()
                    .filter(|s| grammar::is_hex(s))
                    .ok_or_else(|| plain(WireError::invalid("session")))?;
                home.watch(&who, self.id, space, session)
                    .map(Done::Feed)
                    .map_err(plain)
            }
            "signal" => self
                .signal(&who, args)
                .map(|()| Done::Ok(Value::Null))
                .map_err(plain),
            "blobPut" => {
                let r = parse_blobref(&args["ref"])
                    .ok_or_else(|| plain(WireError::invalid("blobPut ref")))?;
                let offset = args["offset"]
                    .as_u64()
                    .ok_or_else(|| plain(WireError::invalid("offset")))?;
                let data = args["data"]
                    .as_str()
                    .and_then(|d| base64url::decode(d).ok())
                    .filter(|d| d.len() <= MAX_BLOB_PART_BYTES)
                    .ok_or_else(|| plain(WireError::invalid("blobPut data")))?;
                home.blob_put(&who, &r, offset, &data)
                    .map(Done::Ok)
                    .map_err(plain)
            }
            "blobGet" => {
                let r = parse_blobref(&args["ref"])
                    .ok_or_else(|| plain(WireError::invalid("blobGet ref")))?;
                let space = space_arg(args).map_err(plain)?;
                let offset = opt_nat(args, "offset").map_err(plain)?.unwrap_or(0);
                let length = opt_nat(args, "length").map_err(plain)?;
                home.blob_get(&who, &r, space, offset, length)
                    .map(Done::Ok)
                    .map_err(plain)
            }
            "stream" => home
                .stream(&who, &args["item"])
                .map(|()| Done::Ok(Value::Null))
                .map_err(plain),
            "host" => self.host(&who, args).map(Done::Ok).map_err(plain),
            "invite" => {
                let space = space_arg(args).map_err(plain)?;
                let hash = args["hash"].as_str().filter(|h| grammar::is_hex(h));
                let until = args["expiresAt"].as_str().filter(|t| grammar::is_time(t));
                let (Some(hash), Some(until)) = (hash, until) else {
                    return Err(plain(WireError::invalid("invite args")));
                };
                home.invite(&who, space, hash, until)
                    .map(|()| Done::Ok(Value::Null))
                    .map_err(plain)
            }
            "join" => {
                let space = space_arg(args).map_err(plain)?;
                let secret = args["secret"]
                    .as_str()
                    .and_then(|s| base64url::decode(s).ok())
                    .filter(|b| b.len() == 32)
                    .ok_or_else(|| plain(WireError::invalid("secret")))?;
                home.join(&who, space, &secret)
                    .map(|()| Done::Ok(Value::Null))
                    .map_err(|e| (e, true)) // the home closes the connection after a failed join
            }
            "test.clock" => self.test_clock(args).map(Done::Ok).map_err(plain),
            "test.limits" => self.test_limits(args).map(Done::Ok).map_err(plain),
            other => Err(plain(WireError::invalid(format!("unknown call {other}")))),
        }
    }

    fn authenticated(&self) -> Result<()> {
        if self.st().who.is_some() {
            Ok(())
        } else {
            Err(WireError::denied("hello first"))
        }
    }

    /// N-02: the peer filter, after the frame checks and before the args.
    fn admit(&self, call: &str, args: &Value) -> Result<()> {
        if self.home.is_test_mode() {
            return Ok(()); // G-11: in test mode the peer filter admits every node
        }
        let (who, bound) = {
            let st = self.st();
            (st.who.clone(), st.bound)
        };
        let space = match call {
            "append" => args["entry"]["space"].as_str(),
            "stream" => args["item"]["space"].as_str(),
            _ => args["space"].as_str(),
        };
        let nodes = match &who {
            Some(w) if bound => self.home.nodes_for(w),
            _ => Vec::new(),
        };
        let conn = AdmitConn {
            node: self.peer.clone(),
            call: call.to_owned(),
            space: space.map(str::to_owned),
            hello: who.map(|who| Hello { who, nodes }),
        };
        match admit(&conn, &self.home.admit_spaces()) {
            AdmitVerdict::Deny(why) => Err(WireError::denied(why)),
            AdmitVerdict::Allow(Tier::Open) => self.home.allow_open(&self.peer),
            AdmitVerdict::Allow(_) => Ok(()),
        }
    }

    /// G-07: a request id still open is `invalid`; at most `maxFollows` open follows, one more is `limit`.
    fn room(&self, id: u64, counts: bool) -> Result<()> {
        let st = self.st();
        if st.open.contains_key(&id) {
            return Err(WireError::invalid("request id still open"));
        }
        if counts && st.open.len() >= MAX_FOLLOWS {
            return Err(WireError::limit("too many open streams", 1000));
        }
        Ok(())
    }

    fn hello(&self, args: &Value) -> Result<Value> {
        // Args and version first: a `hello` that is invalid for either leaves the challenge unconsumed.
        if args["version"].as_i64() != Some(1) && args.get("version").is_some() {
            return Err(WireError::invalid("unsupported version"));
        }
        let text = |k: &str| args[k].as_str();
        let (Some(who), Some(key_id), Some(proof)) = (text("who"), text("keyId"), text("proof"))
        else {
            return Err(WireError::invalid("hello args"));
        };
        if args["version"].as_i64() != Some(1)
            || !grammar::is_actor_id(who)
            || args
                .get("knownHeads")
                .is_some_and(|k| k.as_array().is_none_or(|l| l.len() > MAX_FOLLOWS))
        {
            return Err(WireError::invalid("hello args"));
        }
        let challenge = {
            let mut st = self.st();
            if st.who.is_some() {
                return Err(WireError::invalid("already authenticated"));
            }
            let challenge = st
                .challenge
                .take()
                .ok_or_else(|| WireError::denied("the challenge was already used"))?; // single use (G-08)
            if self.home.steady_ms().saturating_sub(st.issued_ms) > CHALLENGE_MS {
                return Err(WireError::denied("the challenge expired"));
            }
            challenge
        };
        let Authed { ok, nodes } = self.home.hello(
            who,
            key_id,
            proof,
            &challenge,
            self.node.as_deref(),
            args.get("knownHeads"),
        )?;
        let mut st = self.st();
        st.bound = nodes.contains(&self.peer);
        st.who = Some(who.to_owned());
        Ok(ok)
    }

    fn signal(&self, who: &str, args: &Value) -> Result<()> {
        let space = space_arg(args)?;
        let body = &args["body"];
        if args.get("body").is_none()
            || !grammar::is_opaque_json(body)
            || json::canonicalize(body).map_or(usize::MAX, |b| b.len()) > MAX_SIGNAL_BYTES
        {
            return Err(WireError::invalid("signal args"));
        }
        let mut sender = who;
        let mut relayed_by = None;
        if let Some(from) = args.get("from") {
            // D10: `from` is a server's relay for one of its own users
            if !who.starts_with("@:") {
                return Err(WireError::invalid("from is for a server's relay"));
            }
            let from = from
                .as_str()
                .filter(|f| grammar::is_actor_id(f) && !f.starts_with("@:"))
                .ok_or_else(|| {
                    WireError::denied("a server may relay signals only from its own actors")
                })?;
            if grammar::server_of(from) != who.strip_prefix("@:") {
                return Err(WireError::denied(
                    "a server may relay signals only from its own actors",
                ));
            }
            sender = from;
            relayed_by = Some(who);
        }
        self.home.signal(sender, self.id, relayed_by, space, body)
    }

    /// G-06: `host` is for a home server acting as `@:A`. This home does not take over spaces yet.
    fn host(&self, who: &str, args: &Value) -> Result<Value> {
        if !who.starts_with("@:") || who.strip_prefix("@:") == Some(self.home.server()) {
            return Err(WireError::denied("host is for a home server acting as @:A"));
        }
        let records_ok = args["records"]
            .as_array()
            .is_some_and(|l| l.len() <= 1000 && l.iter().all(|r| matches!(r, Value::Object(_))));
        let head_ok = args
            .get("head")
            .is_none_or(|h| matches!(h, Value::Object(_)));
        if args["space"]
            .as_str()
            .is_none_or(|s| !grammar::is_space_id(s))
            || !records_ok
            || !head_ok
        {
            return Err(WireError::invalid("host args"));
        }
        Err(WireError::denied("this server does not take over spaces"))
    }

    fn test_clock(&self, args: &Value) -> Result<Value> {
        let Some(clock) = &self.home.test_clock else {
            return Err(WireError::invalid("unknown call test.clock"));
        };
        let wall = match args.get("wall") {
            None => None,
            Some(w) => Some(
                w.as_str()
                    .and_then(parse_time)
                    .and_then(|t| u64::try_from(t).ok())
                    .ok_or_else(|| WireError::invalid("wall is not a Time"))?,
            ),
        };
        let advance = match args.get("steadyAdvance") {
            None => 0,
            Some(v) => v.as_u64().ok_or_else(|| {
                WireError::invalid("steadyAdvance is a non-negative integer (seconds)")
            })?,
        };
        if let Some(w) = wall {
            clock.set_wall(w);
        }
        clock.advance_steady(advance);
        use crate::clock::Clock;
        Ok(jv::obj([
            (
                "wall",
                jv::s(&format_time(
                    i64::try_from(clock.now_ms() / 1000).unwrap_or(0),
                )),
            ),
            ("steady", jv::int(clock.steady_ms() / 1000)),
        ]))
    }

    fn test_limits(&self, args: &Value) -> Result<Value> {
        if !self.home.is_test_mode() {
            return Err(WireError::invalid("unknown call test.limits"));
        }
        let n = match args.get("maxFrameBytes") {
            None => None,
            Some(v) => Some(
                v.as_u64()
                    .filter(|n| (1024..=han_core::limits::MAX_FRAME_BYTES as u64).contains(n))
                    .ok_or_else(|| {
                        WireError::invalid("maxFrameBytes is an integer from 1024 to 1310720")
                    })?,
            ),
        };
        Ok(jv::obj([(
            "maxFrameBytes",
            jv::int(
                self.home
                    .set_max_frame_bytes(n.map(|n| usize::try_from(n).unwrap_or(0))),
            ),
        )]))
    }
}

/// The outcome of a call before framing.
enum Done {
    Ok(Value),
    Items(Vec<Value>),
    Feed(Feed),
}

fn space_arg(args: &Value) -> Result<&str> {
    args["space"]
        .as_str()
        .filter(|s| grammar::is_space_id(s))
        .ok_or_else(|| WireError::invalid("space"))
}

fn opt_nat(args: &Value, key: &str) -> Result<Option<u64>> {
    match args.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| WireError::invalid(key.to_owned())),
    }
}

impl Home {
    /// N-02: an open-tier node is rate-limited per node (`limit` with `retryAfterMs`).
    pub(crate) fn allow_open(&self, node: &str) -> Result<()> {
        let now = self.steady_ms();
        let mut g = self.lock();
        let slot = g.open_rate.entry(node.to_owned()).or_insert((now, 0));
        if now.saturating_sub(slot.0) >= OPEN_WINDOW_MS {
            *slot = (now, 0);
        }
        slot.1 += 1;
        if slot.1 > OPEN_CALLS {
            let wait = OPEN_WINDOW_MS - now.saturating_sub(slot.0);
            return Err(WireError::limit(
                "too many calls from this node",
                wait.max(1),
            ));
        }
        Ok(())
    }

    /// `test.limits`: sets (or only reads) the `maxFrameBytes` that `hello` reports and state pages are cut to.
    pub(crate) fn set_max_frame_bytes(&self, n: Option<usize>) -> u64 {
        let mut g = self.lock();
        if let Some(n) = n {
            g.max_frame_bytes = n;
        }
        g.max_frame_bytes as u64
    }
}

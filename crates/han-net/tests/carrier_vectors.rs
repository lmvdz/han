//! The carrier vectors that han-net owns (`conformance/carrier-vectors`): `control`, `wellknown`, `tickets`,
//! `stream-items`, `typed-streams` and `frames`. The peer filter, strict Ed25519, the header checks and the hello proof
//! bytes are han-core's and replayed there.

use std::path::PathBuf;
use std::sync::Arc;

use han_core::json::{self, Numbers, Value};
use han_net::control::challenge_verdict;
use han_net::dial::dial_verdict;
use han_net::frame::{read_frame, write_frame};
use han_net::ticket::InviteTicket;
use han_net::typed::{self, TypedStream, Verdict};
use han_net::wellknown::wellknown_verdict;
use han_seq::stream_item;

fn load(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/carrier-vectors")
        .join(name);
    json::parse_with(&std::fs::read(path).unwrap(), Numbers::Any).unwrap()
}

fn cases<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v[key].as_array().unwrap()
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap()
}

fn from_hex(h: &str) -> Vec<u8> {
    (0..h.len() / 2)
        .map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn control_vectors() {
    let v = load("control.json");
    assert!(!cases(&v, "challenges").is_empty());
    for c in cases(&v, "challenges") {
        let frames: Vec<String> = cases(c, "frames")
            .iter()
            .map(|f| f.as_str().unwrap().to_owned())
            .collect();
        let nodes: Vec<String> = cases(c, "nodes")
            .iter()
            .map(|n| n.as_str().unwrap().to_owned())
            .collect();
        let got = challenge_verdict(&frames, s(c, "dialed"), s(c, "node"), &nodes);
        assert_eq!(
            got.is_ok(),
            s(c, "expect") == "ok",
            "{}: {got:?}",
            s(c, "name")
        );
    }
}

#[test]
fn wellknown_vectors() {
    let v = load("wellknown.json");
    assert!(!cases(&v, "cases").is_empty());
    for c in cases(&v, "cases") {
        let got = wellknown_verdict(&c["doc"]);
        assert_eq!(
            got.is_none(),
            s(c, "expect") == "ok",
            "{}: {got:?}",
            s(c, "name")
        );
    }
}

#[test]
fn ticket_vectors() {
    let v = load("tickets.json");
    assert!(!cases(&v, "cases").is_empty());
    for c in cases(&v, "cases") {
        let name = s(c, "name");
        let parsed = InviteTicket::parse(s(c, "text"));
        match &c["expect"] {
            Value::Str(w) if w == "invalid" => assert!(parsed.is_none(), "{name}"),
            want => {
                let t = parsed.unwrap_or_else(|| panic!("{name}: should parse"));
                assert_eq!(json::to_value(&t).unwrap(), *want, "{name}");
                assert_eq!(
                    t.to_text(),
                    s(c, "text"),
                    "{name}: encodes back to exactly the text"
                );
            }
        }
    }
}

#[test]
fn stream_item_vectors() {
    let v = load("stream-items.json");
    assert!(!cases(&v, "cases").is_empty());
    for c in cases(&v, "cases") {
        let name = s(c, "name");
        let got = stream_item::verdict(&c["item"]);
        assert_eq!(got.is_none(), s(c, "expect") == "ok", "{name}: {got:?}");
        if got.is_none() {
            assert_eq!(stream_item::right(&c["item"]), s(c, "right"), "{name}");
        }
    }
}

#[test]
fn stream_item_bodies_are_checked_by_faces() {
    let v = load("stream-items.json");
    assert!(!cases(&v, "bodies").is_empty());
    for c in cases(&v, "bodies") {
        let got = stream_item::status_body_verdict(&c["body"]);
        assert_eq!(
            got.is_none(),
            s(c, "expect") == "ok",
            "{}: {got:?}",
            s(c, "name")
        );
    }
}

#[test]
fn dial_vectors() {
    let v = load("control.json");
    assert!(!cases(&v, "dials").is_empty());
    for c in cases(&v, "dials") {
        let got = dial_verdict(s(c, "server"), s(c, "node"), &c["wellknown"]);
        assert_eq!(
            got.is_none(),
            s(c, "expect") == "ok",
            "{}: {got:?}",
            s(c, "name")
        );
    }
}

#[tokio::test]
async fn typed_stream_vectors() {
    let v = load("typed-streams.json");
    assert!(!cases(&v, "encode").is_empty());
    for c in cases(&v, "encode") {
        let frames: Vec<Vec<u8>> = cases(c, "frames")
            .iter()
            .map(|f| from_hex(f.as_str().unwrap()))
            .collect();
        let out = typed::encode(s(c, "type"), &frames).await.unwrap();
        assert_eq!(to_hex(&out), s(c, "hex"));
    }
    assert!(!cases(&v, "cases").is_empty());
    for c in cases(&v, "cases") {
        let name = s(c, "name");
        let finished = c["finished"] == Value::Bool(true);
        let got = typed::decode(&from_hex(s(c, "hex")), finished).await;
        match &c["expect"] {
            Value::Str(w) if w == "more" => assert_eq!(got, Verdict::More, "{name}"),
            Value::Str(w) if w == "close" => assert_eq!(got, Verdict::Close, "{name}"),
            want => {
                let frames: Vec<Vec<u8>> = cases(want, "frames")
                    .iter()
                    .map(|f| from_hex(f.as_str().unwrap()))
                    .collect();
                let kind = s(want, "type").to_owned();
                assert_eq!(got, Verdict::Stream(TypedStream { kind, frames }), "{name}");
            }
        }
    }
}

#[tokio::test]
async fn frame_vectors() {
    let v = load("frames.json");
    for c in cases(&v, "encode") {
        let mut out = Vec::new();
        write_frame(&mut out, s(c, "text")).await.unwrap();
        assert_eq!(to_hex(&out), s(c, "hex"));
    }
    for c in cases(&v, "decode") {
        let bytes = from_hex(s(c, "hex"));
        let mut rest: &[u8] = &bytes;
        let got = read_frame(&mut rest).await;
        match (&c["expect"], got) {
            (Value::Str(w), Err(han_net::NetError::Closed(_))) => assert_eq!(w, "more", "{c:?}"),
            (Value::Str(w), Err(han_net::NetError::Frame(_))) => assert_eq!(w, "close", "{c:?}"),
            (want @ Value::Object(_), Ok(Some(text))) => {
                assert_eq!(text, s(want, "frame"));
                assert_eq!(to_hex(rest), s(want, "rest"));
            }
            (want, got) => panic!("{}: expected {want:?}, got {got:?}", s(c, "hex")),
        }
    }
    // the requests: a rejected frame is answered `invalid` on its id, and the connection closes when it says so
    let config = |server: &str| han_seq::HomeConfig {
        server: server.into(),
        key: han_core::SigningKey::from_seed([1; 32]),
        key_id: "h1".into(),
        durability: han_core::types::Durability::Local,
    };
    let test_home = Arc::new(
        han_seq::Home::open_test(
            config("a.example"),
            Arc::new(han_seq::TestClock::new(1_767_225_600)),
            Arc::new(han_seq::MemStore::new()),
        )
        .unwrap(),
    );
    let home = Arc::new(
        han_seq::Home::open(
            config("a.example"),
            Arc::new(han_seq::SystemClock::new()),
            Arc::new(han_seq::MemStore::new()),
        )
        .unwrap(),
    );
    for c in cases(&v, "requests") {
        let test_mode = c.get("test_mode") == Some(&Value::Bool(true));
        let (conn, _challenge) = if test_mode { &test_home } else { &home }.connect("x", "s");
        let handled = conn.handle(s(c, "text"));
        match &c["expect"] {
            Value::Null => assert!(!handled.close, "{}", s(c, "text")),
            Value::Array(w) => {
                let reply = json::parse(handled.reply[0].as_bytes()).unwrap();
                assert_eq!(reply["id"].as_u64(), w[0].as_u64(), "{}", s(c, "text"));
                assert_eq!(reply["error"]["code"].as_str(), Some("invalid"));
                assert_eq!(Value::Bool(handled.close), w[1], "{}", s(c, "text"));
            }
            other => panic!("unexpected expectation {other:?}"),
        }
    }
}

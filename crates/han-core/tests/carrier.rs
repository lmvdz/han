//! Replays the parts of `conformance/carrier-vectors` that han-core owns: the peer filter
//! (`admit.json`), frames and request frames (`frames.json`), the header and key checks
//! (`header.json`), strict Ed25519 (`ed25519.json`) and the `han/1` hello proof bytes
//! (`control.json`, `proofs`). Well-known documents, tickets and the dial check, the hello binding,
//! stream items, typed streams and the control stream's challenge belong to the carrier crate (M2).

use std::path::{Path, PathBuf};

use han_core::admit::{AdmitConn, AdmitSpaces, AdmitVerdict, Tier, admit};
use han_core::frame::{Decoded, Rejected, decode_frame, encode_frame, request_verdict};
use han_core::json::{self, Numbers, Value};
use han_core::sign::{PublicKey, han1_hello_message, is_node_id};
use han_core::{Entry, Error};

fn load(name: &str) -> Value {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/carrier-vectors")
        .join(name);
    json::parse_with(&std::fs::read(path).unwrap(), Numbers::Any).unwrap()
}

fn items<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key).and_then(Value::as_array).unwrap()
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap()
}

fn from_hex(h: &str) -> Vec<u8> {
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
        .collect()
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn admit_vectors() {
    let v = load("admit.json");
    let shared: AdmitSpaces = json::from_value(&v["spaces"]).unwrap();
    let cases = items(&v, "cases");
    assert!(!cases.is_empty());
    for c in cases {
        let conn: AdmitConn = json::from_value(&c["conn"]).unwrap();
        let spaces: AdmitSpaces = match c.get("spaces") {
            Some(own) => json::from_value(own).unwrap(),
            None => shared.clone(),
        };
        let want = &c["expect"];
        let got = admit(&conn, &spaces);
        let name = s(c, "name");
        match (want["allow"] == Value::Bool(true), got) {
            (true, AdmitVerdict::Allow(tier)) => {
                let tier_name = match tier {
                    Tier::Member => "member",
                    Tier::Server => "server",
                    Tier::Open => "open",
                };
                assert_eq!(s(want, "tier"), tier_name, "{name}");
            }
            (false, AdmitVerdict::Deny(_)) => {}
            (_, got) => panic!("{name}: expected {want:?}, got {got:?}"),
        }
    }
}

#[test]
fn frame_vectors() {
    let v = load("frames.json");
    for c in items(&v, "encode") {
        let got = encode_frame(s(c, "text")).unwrap();
        assert_eq!(to_hex(&got), s(c, "hex"));
    }
    for c in items(&v, "decode") {
        let buf = from_hex(s(c, "hex"));
        match (decode_frame(&buf), &c["expect"]) {
            (Decoded::More, Value::Str(w)) => assert_eq!(w, "more"),
            (Decoded::Close(_), Value::Str(w)) => assert_eq!(w, "close"),
            (Decoded::Frame { text, rest }, want @ Value::Object(_)) => {
                assert_eq!(text, s(want, "frame"));
                assert_eq!(to_hex(rest), s(want, "rest"));
            }
            (got, want) => panic!("{}: expected {want:?}, got {got:?}", s(c, "hex")),
        }
    }
    for c in items(&v, "requests") {
        let test_mode = c.get("test_mode") == Some(&Value::Bool(true));
        let got = request_verdict(s(c, "text"), test_mode);
        match (&c["expect"], got) {
            (Value::Null, None) => {}
            (Value::Array(w), Some(Rejected { id, close })) => {
                assert_eq!(w[0].as_u64(), Some(id), "{}", s(c, "text"));
                assert_eq!(w[1], Value::Bool(close), "{}", s(c, "text"));
            }
            (want, got) => panic!("{}: expected {want:?}, got {got:?}", s(c, "text")),
        }
    }
}

#[test]
fn header_vectors() {
    let v = load("header.json");
    for c in items(&v, "cases") {
        let bytes = json::canonicalize(&c["entry"]).unwrap();
        let got: Result<Entry, Error> = Entry::from_json(&bytes);
        let want = s(c, "expect");
        assert_eq!(got.is_ok(), want == "ok", "{}: {got:?}", s(c, "name"));
    }
}

#[test]
fn ed25519_vectors() {
    let v = load("ed25519.json");
    for c in items(&v, "cases") {
        let name = s(c, "name");
        let key = PublicKey::from_base64url(s(c, "publicKey"));
        assert_eq!(key.is_ok(), s(c, "key") == "ok", "{name} (key)");
        assert_eq!(
            is_node_id(s(c, "publicKey")),
            s(c, "key") == "ok",
            "{name} (node id)"
        );
        let valid = key.is_ok_and(|k| k.verify(&from_hex(s(c, "msg")), s(c, "sig")).is_ok());
        assert_eq!(Value::Bool(valid), c["valid"], "{name}");
    }
}

#[test]
fn hello_proof_vectors() {
    let v = load("control.json");
    for c in items(&v, "proofs") {
        let message = han1_hello_message(s(c, "challenge"), s(c, "dialed"), s(c, "node"));
        assert_eq!(to_hex(&message), s(c, "message"));
        let key = PublicKey::from_base64url(s(c, "publicKey")).ok();
        let valid = key
            .as_ref()
            .is_some_and(|k| k.verify(&message, s(c, "proof")).is_ok());
        assert_eq!(Value::Bool(valid), c["valid"], "{:?}", c.get("note"));
        if let Some(other) = c.get("checkedAs").and_then(Value::as_str) {
            let there = han1_hello_message(s(c, "challenge"), s(c, "dialed"), other);
            assert!(
                !key.is_some_and(|k| k.verify(&there, s(c, "proof")).is_ok()),
                "{:?}",
                c.get("note")
            );
        }
    }
}

//! The dispatcher of one connection: the rules of G-07 and G-08 that are about the connection itself.

use std::sync::Arc;

use han_core::SigningKey;
use han_core::json::{self, Value};
use han_core::types::Durability;
use han_seq::{Conn, Home, HomeConfig, MemStore, TestClock, auth};

const ALICE: &str = "@alice:a.localhost";

fn alice_key() -> SigningKey {
    SigningKey::from_seed([5; 32])
}

fn home() -> (Arc<Home>, Arc<TestClock>) {
    let clock = Arc::new(TestClock::new(1_767_225_600));
    let home = Home::open_test(
        HomeConfig {
            server: "a.localhost".into(),
            key: SigningKey::from_seed([6; 32]),
            key_id: "h1".into(),
            durability: Durability::Local,
        },
        Arc::clone(&clock),
        Arc::new(MemStore::new()),
    )
    .unwrap();
    home.register(
        ALICE,
        "human",
        "k1",
        &alice_key().public_key().to_base64url(),
        None,
    )
    .unwrap();
    (Arc::new(home), clock)
}

fn challenge_of(frame: &str) -> String {
    json::parse(frame.as_bytes()).unwrap()["challenge"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn hello(challenge: &str, version: i64) -> String {
    let proof = auth::make_proof(&alice_key(), challenge, "a.localhost", None);
    format!(
        r#"{{"id":1,"call":"hello","args":{{"who":"{ALICE}","keyId":"k1","proof":"{proof}","version":{version}}}}}"#
    )
}

fn code(conn: &Conn, frame: &str) -> String {
    let handled = conn.handle(frame);
    let Some(first) = handled.reply.first() else {
        return "ok".to_owned(); // a streaming call that stays open has no reply
    };
    let reply = json::parse(first.as_bytes()).unwrap();
    match reply.get("error") {
        Some(e) => e["code"].as_str().unwrap().to_owned(),
        None => "ok".to_owned(),
    }
}

#[test]
fn the_challenge_is_32_random_bytes_and_fresh_per_connection() {
    let (home, _) = home();
    let (_, a) = home.connect("n1", "s1");
    let (_, b) = home.connect("n1", "s1");
    let (ca, cb) = (challenge_of(&a), challenge_of(&b));
    assert_eq!(ca.len(), 43);
    assert_ne!(ca, cb);
    let frame: Value = json::parse(a.as_bytes()).unwrap();
    assert_eq!(frame.as_object().unwrap().len(), 4, "bare: no id");
}

#[test]
fn a_challenge_expires_after_60_seconds() {
    let (home, clock) = home();
    let (conn, frame) = home.connect("n1", "s1");
    clock.advance_steady(61);
    assert_eq!(code(&conn, &hello(&challenge_of(&frame), 1)), "denied");
    assert!(conn.actor().is_none());
}

#[test]
fn a_challenge_is_used_by_the_first_well_formed_hello() {
    let (home, _) = home();
    // a hello that is invalid for its version leaves the challenge unconsumed
    let (conn, frame) = home.connect("n1", "s1");
    let challenge = challenge_of(&frame);
    assert_eq!(code(&conn, &hello(&challenge, 2)), "invalid");
    assert_eq!(code(&conn, &hello(&challenge, 1)), "ok");
    assert_eq!(conn.actor().as_deref(), Some(ALICE));
    // a second hello on an authenticated connection is invalid and leaves it authenticated
    assert_eq!(code(&conn, &hello(&challenge, 1)), "invalid");
    assert_eq!(conn.actor().as_deref(), Some(ALICE));
    // a failed hello uses the challenge up: a later hello on the connection is denied
    let (conn, frame) = home.connect("n1", "s1");
    let challenge = challenge_of(&frame);
    assert_eq!(
        code(&conn, &hello("another-challenge-another-challenge", 1)),
        "denied"
    );
    assert_eq!(code(&conn, &hello(&challenge, 1)), "denied");
}

#[test]
fn nothing_is_admitted_before_hello() {
    let (home, _) = home();
    let (conn, _) = home.connect("n1", "s1");
    for call in [
        "append", "read", "state", "join", "invite", "signal", "cancel",
    ] {
        let frame = format!(r#"{{"id":1,"call":"{call}","args":{{}}}}"#);
        assert_eq!(code(&conn, &frame), "denied", "{call}");
    }
}

#[test]
fn a_han1_hello_proof_binds_the_servers_node() {
    // outside test mode the connection is `han/1`: the proof signs the server's node id (Z4, G-08)
    let home = Arc::new(
        Home::open(
            HomeConfig {
                server: "a.localhost".into(),
                key: SigningKey::from_seed([6; 32]),
                key_id: "h1".into(),
                durability: Durability::Local,
            },
            Arc::new(han_seq::SystemClock::new()),
            Arc::new(MemStore::new()),
        )
        .unwrap(),
    );
    home.register(
        ALICE,
        "human",
        "k1",
        &alice_key().public_key().to_base64url(),
        None,
    )
    .unwrap();
    let hello_from = |peer: &str, node: Option<&str>| {
        let (conn, frame) = home.connect(peer, "s1");
        let proof = auth::make_proof(&alice_key(), &challenge_of(&frame), "a.localhost", node);
        let text = format!(
            r#"{{"id":1,"call":"hello","args":{{"who":"{ALICE}","keyId":"k1","proof":"{proof}","version":1}}}}"#
        );
        code(&conn, &text)
    };
    let hello_with =
        |node: Option<&str>| hello_from(&alice_key().public_key().to_base64url(), node);
    assert_eq!(
        hello_with(None),
        "denied",
        "a v1 proof is not a han/1 proof"
    );
    assert_eq!(hello_with(Some("s2")), "denied", "a proof for another node");
    assert_eq!(hello_with(Some("s1")), "ok");
    assert_eq!(
        hello_from("n1", Some("s1")),
        "denied",
        "a valid proof on another device's connection (SR-13)"
    );
}

#[test]
fn test_calls_exist_only_in_test_mode() {
    let (home, _) = home();
    let production = Arc::new(
        Home::open(
            HomeConfig {
                server: "a.localhost".into(),
                key: SigningKey::from_seed([6; 32]),
                key_id: "h1".into(),
                durability: Durability::Local,
            },
            Arc::new(han_seq::SystemClock::new()),
            Arc::new(MemStore::new()),
        )
        .unwrap(),
    );
    let (conn, _) = production.connect("n1", "s1");
    assert_eq!(
        code(&conn, r#"{"id":1,"call":"test.clock","args":{}}"#),
        "invalid"
    );
    let (conn, _) = home.connect("n1", "s1");
    assert_eq!(
        code(&conn, r#"{"id":1,"call":"test.clock","args":{}}"#),
        "denied"
    );
}

#[test]
fn a_streaming_call_id_that_is_still_open_is_invalid_and_cancel_ends_it() {
    let (home, _) = home();
    let (conn, frame) = home.connect("n1", "s1");
    assert_eq!(code(&conn, &hello(&challenge_of(&frame), 1)), "ok");
    let own_profile = format!("profile:{ALICE}");
    let subscribe = format!(r#"{{"id":7,"call":"subscribe","args":{{"space":"{own_profile}"}}}}"#);
    let handled = conn.handle(&subscribe);
    assert!(handled.feed.is_some());
    assert_eq!(code(&conn, &subscribe), "invalid", "id 7 is still open");
    assert_eq!(
        code(&conn, r#"{"id":8,"call":"cancel","args":{"target":7}}"#),
        "ok"
    );
    assert_eq!(
        code(&conn, &subscribe),
        "ok",
        "id 7 may be reused once it has ended"
    );
}

#[test]
fn at_most_max_follows_open_calls() {
    let (home, _) = home();
    let (conn, frame) = home.connect("n1", "s1");
    assert_eq!(code(&conn, &hello(&challenge_of(&frame), 1)), "ok");
    let own_profile = format!("profile:{ALICE}");
    let mut keep = Vec::new();
    for id in 1..=256 {
        let frame =
            format!(r#"{{"id":{id},"call":"subscribe","args":{{"space":"{own_profile}"}}}}"#);
        let handled = conn.handle(&frame);
        assert!(handled.feed.is_some(), "{id}");
        keep.push(handled);
    }
    let one_more = r#"{"id":257,"call":"subscribe","args":{"space":"profile:@alice:a.localhost"}}"#;
    assert_eq!(code(&conn, one_more), "limit");
}

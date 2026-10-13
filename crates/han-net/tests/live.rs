//! A live test: three QUIC endpoints on localhost, a home and two people's devices (the device key is the
//! node id, N-01). Alice creates a space, invites Bob with a ticket, Bob joins, Alice admits him, they append,
//! read and follow, and every receipt verifies against the keys the home publishes at `/.well-known/han`.

mod common;

use std::time::Duration;

use common::{Rig, SERVER, Signer, endpoint, entry, key, obj, s};
use han_core::json::{self, Value};
use han_core::{LogRecord, Receipt, ids};
use han_net::client::args;
use han_net::dial::{trusted_nodes_for_server, trusted_nodes_for_ticket};
use han_net::wellknown::Fetcher;
use han_net::{Client, Connection, InviteTicket, NodeAddr, ServeOptions, Transport};
use std::sync::Arc;

use han_core::SigningKey;
use han_seq::build::space_policy;
use han_seq::{Code, Home, HomeConfig, SqliteStore, SystemClock};

fn record(item: &Value) -> LogRecord {
    json::from_value(&item["record"]).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_people_on_two_devices_over_quic() {
    tokio::time::timeout(Duration::from_secs(90), scenario())
        .await
        .unwrap();
}

async fn scenario() {
    let (alice_key, bob_key) = (key(31), key(32));
    let (alice, bob) = ("@alice:a.localhost", "@bob:a.localhost");
    let rig = Rig::start(&[(alice, 31), (bob, 32)], ServeOptions::default()).await;
    let (home_addr, http, dir) = (rig.addr.clone(), &rig.http, rig.dir.clone());

    // Alice finds the home from its well-known document and dials it
    let fetcher = Fetcher::new().with_base(SERVER, &format!("http://{}", http.local_addr()));
    let (nodes, addrs) = trusted_nodes_for_server(&fetcher, SERVER).await.unwrap();
    let doc = fetcher.wellknown(SERVER).await.unwrap();
    let alice_t = endpoint(31).await;
    assert_eq!(
        alice_t.node_id(),
        alice_key.public_key().to_base64url(),
        "the node id is the device key"
    );
    let a = Client::connect(&alice_t, &addrs[0], SERVER, &nodes)
        .await
        .unwrap();
    a.hello(alice, "d1", &alice_key).await.unwrap();

    // a stranger's device can say hello for nobody, and cannot read a members-only space
    let stranger_key = key(99);
    let stranger_t = endpoint(99).await;
    let st = Client::connect(&stranger_t, &addrs[0], SERVER, &nodes)
        .await
        .unwrap();
    let e = st
        .hello("@mallory:a.localhost", "d1", &stranger_key)
        .await
        .unwrap_err();
    assert_eq!(
        e.wire().map(|w| w.code),
        Some(Code::NotFound),
        "no profile on this server"
    );
    let denied = st
        .call(
            "read",
            args([("space", s("team:a.localhost")), ("after", Value::Int(0))]),
        )
        .await
        .unwrap_err();
    assert_eq!(
        denied.wire().map(|w| w.code),
        Some(Code::Denied),
        "before hello nothing is admitted"
    );
    st.close();

    // Alice creates the space
    let (e1, c1) = entry(
        &Signer {
            actor: alice,
            key: &alice_key,
            key_id: "d1",
        },
        "team:a.localhost",
        "space.create",
        obj([("policy", space_policy("members", "invite"))]),
        Some(obj([("name", s("team"))])),
        0,
    );
    let r1: Receipt = a.append(&e1, c1.as_ref()).await.unwrap();
    assert_eq!((r1.seq, r1.prev.as_str()), (1, ""));

    // Alice follows the space (owners are told about joins on subscribe, and records on follow)
    let mut follow = a.follow("team:a.localhost", 0).await.unwrap();
    let first = follow
        .next_within(Duration::from_secs(5))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record(&first).receipt, r1);
    let mut signals = a
        .stream("subscribe", args([("space", s("team:a.localhost"))]))
        .await
        .unwrap();

    // Alice invites Bob: the home keeps the hash of a secret, and the ticket carries the secret and a node
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    let ticket = InviteTicket {
        server: SERVER.into(),
        node: home_addr.clone(),
        space: "team:a.localhost".into(),
        secret: han_core::base64url::encode(&secret),
    };
    let expires = han_seq::time::format_time(
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap()
            + 3600,
    );
    a.call(
        "invite",
        args([
            ("space", s("team:a.localhost")),
            ("hash", s(&ids::sha256_hex(&secret))),
            ("expiresAt", s(&expires)),
        ]),
    )
    .await
    .unwrap();
    let text = ticket.to_text();

    // Bob, on his own device, parses the ticket, dials its node, says hello and joins
    let ticket = InviteTicket::parse(&text).unwrap();
    let trusted = trusted_nodes_for_ticket(&fetcher, &ticket).await.unwrap();
    assert_eq!(trusted, vec![ticket.node.node.clone()]);
    // a crafted ticket names an attacker's node for this home: the well-known does not list it, so it is
    // refused before any hello (Z4)
    let crafted = InviteTicket {
        node: NodeAddr {
            node: key(66).public_key().to_base64url(),
            ..ticket.node.clone()
        },
        ..ticket.clone()
    };
    let refused = trusted_nodes_for_ticket(&fetcher, &crafted).await;
    assert!(
        matches!(&refused, Err(han_net::NetError::Dial(why)) if why.contains("not listed")),
        "{refused:?}"
    );
    let bob_t = endpoint(32).await;
    let b = Client::connect(&bob_t, &ticket.node, &ticket.server, &trusted)
        .await
        .unwrap();
    b.hello(bob, "d1", &bob_key).await.unwrap();
    let before = b
        .call(
            "read",
            args([("space", s("team:a.localhost")), ("after", Value::Int(0))]),
        )
        .await;
    assert_eq!(
        before.unwrap_err().wire().map(|w| w.code),
        Some(Code::Denied),
        "a device of someone with no role in the space is a stranger to it (N-02, Z7)"
    );
    // the same person on a node that is not one of her registered devices cannot say hello (G-08, SR-13)
    let other_t = endpoint(77).await;
    let o = Client::connect(&other_t, &ticket.node, &ticket.server, &trusted)
        .await
        .unwrap();
    let e = o.hello(bob, "d1", &bob_key).await.unwrap_err();
    assert_eq!(
        e.wire().map(|w| w.code),
        Some(Code::Denied),
        "an unregistered node is not bound to her key"
    );
    let e = o
        .call(
            "read",
            args([("space", s("team:a.localhost")), ("after", Value::Int(0))]),
        )
        .await
        .unwrap_err();
    assert_eq!(
        e.wire().map(|w| w.code),
        Some(Code::Denied),
        "an unauthenticated node reads no members-only space (N-02)"
    );
    o.close();
    b.call(
        "join",
        args([("space", s(&ticket.space)), ("secret", s(&ticket.secret))]),
    )
    .await
    .unwrap();
    let join = signals
        .next_within(Duration::from_secs(5))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        join["body"],
        obj([("type", s("join")), ("actor", s(bob))]),
        "the owners are signalled"
    );

    // the secret is single-use: a second join is denied and the connection closes
    let again = Client::connect(&bob_t, &ticket.node, &ticket.server, &trusted)
        .await
        .unwrap();
    again.hello(bob, "d1", &bob_key).await.unwrap();
    let e = again
        .call(
            "join",
            args([("space", s(&ticket.space)), ("secret", s(&ticket.secret))]),
        )
        .await
        .unwrap_err();
    assert_eq!(e.wire().map(|w| w.code), Some(Code::Denied));
    tokio::time::timeout(Duration::from_secs(5), again.connection().closed())
        .await
        .expect("closed after a failed join");

    // Alice admits Bob
    let (e2, _) = entry(
        &Signer {
            actor: alice,
            key: &alice_key,
            key_id: "d1",
        },
        "team:a.localhost",
        "member.set",
        obj([("actor", s(bob)), ("role", s("member"))]),
        None,
        1,
    );
    let r2 = a.append(&e2, None).await.unwrap();
    assert_eq!(r2.seq, 2);
    assert_eq!(r2.prev, r1.hash().unwrap());
    let second = follow
        .next_within(Duration::from_secs(5))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record(&second).receipt, r2);

    // Bob appends a message; Alice's follow sees it
    let (e3, c3) = entry(
        &Signer {
            actor: bob,
            key: &bob_key,
            key_id: "d1",
        },
        "team:a.localhost",
        "message",
        obj([]),
        Some(obj([("text", s("hello from bob"))])),
        2,
    );
    let r3 = b.append(&e3, c3.as_ref()).await.unwrap();
    assert_eq!(r3.seq, 3);
    let third = follow
        .next_within(Duration::from_secs(5))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record(&third).receipt, r3);
    // an append sent twice returns the same receipt (R-01)
    assert_eq!(b.append(&e3, c3.as_ref()).await.unwrap(), r3);

    // Bob reads the whole log and checks it against the keys in the home's well-known document
    let items = b.read("team:a.localhost", 0).await.unwrap();
    let records: Vec<LogRecord> = items
        .iter()
        .filter(|i| i["type"] == s("record"))
        .map(record)
        .collect();
    assert_eq!(records.len(), 3);
    assert_eq!(records[2].content, c3);
    han_seq::verify::check_chain(&records, &doc.keys, "").unwrap();
    // a tampered chain does not verify
    let mut tampered = records.clone();
    tampered[1].receipt.time = "2099-01-01T00:00:00Z".into();
    assert!(han_seq::verify::check_chain(&tampered, &doc.keys, "").is_err());

    // Bob's state read shows both as members
    let state = b
        .call("state", args([("space", s("team:a.localhost"))]))
        .await
        .unwrap();
    assert_eq!(state["state"]["roles"][bob], s("member"));
    assert_eq!(state["state"]["roles"][alice], s("owner"));

    // after a restart from the same files the chain is the same (R-03: durable before the receipt)
    drop(follow);
    a.close();
    b.close();
    let reopened = Home::open(
        HomeConfig {
            server: SERVER.into(),
            key: SigningKey::from_seed([30; 32]),
            key_id: "h1".into(),
            durability: han_core::types::Durability::Local,
        },
        Arc::new(SystemClock::new()),
        Arc::new(SqliteStore::open(&dir).unwrap()),
    )
    .unwrap();
    assert_eq!(reopened.append(&e3, c3.as_ref()).unwrap(), r3);
    let _ = std::fs::remove_dir_all(&dir);
}

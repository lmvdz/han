//! Federation of profiles (section 4.1 step 4, section 4.3, G-08, N-02): two homes on separate QUIC
//! endpoints, `a.localhost` and `b.localhost`. A user of b appends at a; a reads b's profile space from b,
//! verifies it against b's well-known keys, pins it and accepts the append.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Signer, endpoint, entry, key, obj, s};
use han_core::LogRecord;
use han_core::json::Value;
use han_core::types::Durability;
use han_net::wellknown::{Fetcher, WellKnown, WellKnownDirectory};
use han_net::{
    Client, Connection, IrohTransport, NetError, NodeAddr, ProfileDirectory, ServeOptions,
    ServerHandle, Transport, serve, verify_profile_chain,
};
use han_seq::build::space_policy;
use han_seq::{
    Code, EPOCH, Home, HomeConfig, HostingPolicy, MemStore, ServerKey, TestClock, WireError,
};

const A: &str = "a.localhost";
const B: &str = "b.localhost";
const ALICE: &str = "@alice:a.localhost";
const BOB: &str = "@bob:b.localhost";
const SPACE: &str = "team:a.localhost";

struct Side {
    home: Arc<Home>,
    addr: NodeAddr,
    dir: ProfileDirectory<IrohTransport>,
    clock: Arc<TestClock>,
    _server: ServerHandle,
}

fn doc(server: &str, seed: u8, addr: &NodeAddr) -> WellKnown {
    WellKnown {
        server: server.into(),
        sequencer: vec![addr.clone()],
        keys: vec![ServerKey {
            key_id: "h1".into(),
            public_key: key(seed).public_key().to_base64url(),
            from: EPOCH.into(),
            until: None,
            compromised: None,
        }],
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Starts one home. `others` are the documents of the servers it reads from; `seed` is its server key and node.
async fn side(
    server: &str,
    seed: u8,
    transport: IrohTransport,
    others: &[WellKnown],
    people: &[(&str, u8)],
    revalidate_every: Duration,
) -> Side {
    let clock = Arc::new(TestClock::new(now_secs()));
    let mut fetcher = Fetcher::new();
    for d in others {
        fetcher = fetcher.with_document(d.clone());
    }
    let dir = ProfileDirectory::new(
        WellKnownDirectory::new(fetcher),
        transport.clone(),
        clock.clone(),
    );
    let home = Arc::new(
        Home::open(
            HomeConfig {
                server: server.into(),
                key: key(seed),
                key_id: "h1".into(),
                durability: Durability::Local,
            },
            clock.clone(),
            Arc::new(MemStore::new()),
        )
        .unwrap(),
    );
    home.set_directory(Arc::new(dir.clone()));
    dir.bind(&home);
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
    let addr = transport.loopback_addr();
    home.set_nodes(vec![transport.node_id()]);
    let server = serve(
        transport,
        Arc::clone(&home),
        ServeOptions {
            revalidate_every,
            profiles: Some(Arc::new(dir.clone())),
            ..ServeOptions::default()
        },
    );
    Side {
        home,
        addr,
        dir,
        clock,
        _server: server,
    }
}

struct Fed {
    a: Side,
    b: Side,
}

/// a and b, each knowing the other's document; Bob (keys d1 and d2) lives at b and Alice at a. a hosts only the
/// actors it knows, so Bob's profile is fetched only because Alice, who is known, invites him.
async fn federation(revalidate_every: Duration, b_doc_key: u8) -> Fed {
    let (ta, tb) = (endpoint(30).await, endpoint(60).await);
    let (addr_a, addr_b) = (ta.loopback_addr(), tb.loopback_addr());
    let doc_a = doc(A, 30, &addr_a);
    // what a is told about b: `b_doc_key` is b's real key (60) unless a test forges the document
    let doc_b = doc(B, b_doc_key, &addr_b);
    let a = side(A, 30, ta, &[doc_b], &[(ALICE, 31)], revalidate_every).await;
    let b = side(B, 60, tb, &[doc_a], &[(BOB, 51)], revalidate_every).await;
    // b knows a's keys and nodes, so it accepts a's hello as `@:a`
    b.dir.wellknown().refresh(A).await.unwrap();
    Fed { a, b }
}

fn signer<'a>(actor: &'a str, k: &'a han_core::SigningKey, key_id: &'a str) -> Signer<'a> {
    Signer {
        actor,
        key: k,
        key_id,
    }
}

async fn connect(
    t: &IrohTransport,
    side: &Side,
    server: &str,
) -> Client<<IrohTransport as Transport>::Conn> {
    Client::connect(t, &side.addr, server, std::slice::from_ref(&side.addr.node))
        .await
        .unwrap()
}

fn code(e: &NetError) -> Option<Code> {
    e.wire().map(|w: &WireError| w.code)
}

/// Retries a call that answers `unavailable` (a remote profile is being read), as a client does.
async fn retried<T, F, Fut>(mut call: F) -> Result<T, NetError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, NetError>>,
{
    for _ in 0..40 {
        match call().await {
            Err(e) if code(&e) == Some(Code::Unavailable) => {
                let ms = e.wire().and_then(|w| w.retry_after_ms).unwrap_or(100);
                tokio::time::sleep(Duration::from_millis(ms.min(200))).await;
            }
            other => return other,
        }
    }
    Err(NetError::Closed("still unavailable".into()))
}

/// Bob adds d2 to his profile at b with d1, so a device other than the first has a key.
async fn bob_adds_d2(fed: &Fed) {
    let bob_t = endpoint(51).await;
    let c = connect(&bob_t, &fed.b, B).await;
    c.hello(BOB, "d1", &key(51)).await.unwrap();
    let (add, _) = entry(
        &signer(BOB, &key(51), "d1"),
        &format!("profile:{BOB}"),
        "key.add",
        obj([
            ("keyId", s("d2")),
            ("publicKey", s(&key(52).public_key().to_base64url())),
        ]),
        None,
        1,
    );
    c.append(&add, None).await.unwrap();
    c.close();
}

/// Alice creates the team space at a and admits Bob of b. The member.set pins Bob's profile, so a reads it
/// from b first and the call is `unavailable` until it has.
async fn team(fed: &Fed) {
    let ta = endpoint(31).await;
    let c = connect(&ta, &fed.a, A).await;
    c.hello(ALICE, "d1", &key(31)).await.unwrap();
    let (create, content) = entry(
        &signer(ALICE, &key(31), "d1"),
        SPACE,
        "space.create",
        obj([("policy", space_policy("members", "invite"))]),
        Some(obj([("name", s("team"))])),
        0,
    );
    c.append(&create, content.as_ref()).await.unwrap();
    let (member, _) = entry(
        &signer(ALICE, &key(31), "d1"),
        SPACE,
        "member.set",
        obj([("actor", s(BOB)), ("role", s("member"))]),
        None,
        1,
    );
    retried(|| c.append(&member, None)).await.unwrap();
    c.close();
}

fn message(by: &Signer<'_>, seen: u64, text: &str) -> (Value, Option<Value>) {
    entry(
        by,
        SPACE,
        "message",
        obj([]),
        Some(obj([("text", s(text))])),
        seen,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_user_of_b_appends_at_a() {
    let fed = federation(Duration::from_secs(3600), 60).await;
    bob_adds_d2(&fed).await;
    team(&fed).await;

    // a pinned Bob's profile at b's head: the space create and the key.add
    let pinned = fed.a.dir.pinned(BOB).expect("pinned");
    assert_eq!(pinned.seq, 2);
    assert!(pinned.keys.contains_key("d2"));

    // Bob's own device (d2) connects to a: its hello is authenticated by the profile read from b
    let bob_t = endpoint(52).await;
    let c = connect(&bob_t, &fed.a, A).await;
    c.hello(BOB, "d2", &key(52)).await.unwrap();
    let (m, content) = message(&signer(BOB, &key(52), "d2"), 2, "hello from b");
    let r = c.append(&m, content.as_ref()).await.unwrap();
    assert_eq!(r.seq, 3);
    assert_eq!(
        r.profiles.get(BOB),
        Some(&2),
        "the receipt pins the seq read"
    );
    c.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revoked_key_at_b_is_refused_at_a_after_re_pinning() {
    let fed = federation(Duration::from_secs(3600), 60).await;
    bob_adds_d2(&fed).await;
    team(&fed).await;

    let bob2 = endpoint(52).await;
    let c = connect(&bob2, &fed.a, A).await;
    c.hello(BOB, "d2", &key(52)).await.unwrap();
    let (m1, c1) = message(&signer(BOB, &key(52), "d2"), 2, "one");
    c.append(&m1, c1.as_ref()).await.unwrap();

    // b revokes d2 (with d1)
    let bob1 = endpoint(51).await;
    let at_b = connect(&bob1, &fed.b, B).await;
    at_b.hello(BOB, "d1", &key(51)).await.unwrap();
    let (revoke, _) = entry(
        &signer(BOB, &key(51), "d1"),
        &format!("profile:{BOB}"),
        "key.revoke",
        obj([("keyId", s("d2"))]),
        None,
        2,
    );
    at_b.append(&revoke, None).await.unwrap();

    // within the 300 s window a still holds the old view (R-10: fresh means read within 300 s)
    let (m2, c2) = message(&signer(BOB, &key(52), "d2"), 3, "two");
    c.append(&m2, c2.as_ref()).await.unwrap();

    // past the window the profile is read again: d2's genuine signature is Policy's `denied` (R-12)
    fed.a.clock.advance_steady(301);
    let (m3, c3) = message(&signer(BOB, &key(52), "d2"), 4, "three");
    let e = c.append(&m3, c3.as_ref()).await.unwrap_err();
    assert_eq!(code(&e), Some(Code::Denied), "{e}");
    assert_eq!(fed.a.dir.pinned(BOB).unwrap().seq, 3);

    // d1 still works
    let (m4, c4) = message(&signer(BOB, &key(51), "d1"), 4, "four");
    let c1_conn = connect(&bob1, &fed.a, A).await;
    c1_conn.hello(BOB, "d1", &key(51)).await.unwrap();
    c1_conn.append(&m4, c4.as_ref()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connected_remote_device_is_disconnected_when_revoked() {
    // the re-pin timer, not the clock, drives this one
    let fed = federation(Duration::from_millis(300), 60).await;
    bob_adds_d2(&fed).await;
    team(&fed).await;

    let bob2 = endpoint(52).await;
    let c = connect(&bob2, &fed.a, A).await;
    c.hello(BOB, "d2", &key(52)).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(1000), c.connection().closed())
            .await
            .is_err(),
        "still vouched"
    );

    let bob1 = endpoint(51).await;
    let at_b = connect(&bob1, &fed.b, B).await;
    at_b.hello(BOB, "d1", &key(51)).await.unwrap();
    let (revoke, _) = entry(
        &signer(BOB, &key(51), "d1"),
        &format!("profile:{BOB}"),
        "key.revoke",
        obj([("keyId", s("d2"))]),
        None,
        2,
    );
    at_b.append(&revoke, None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), c.connection().closed())
        .await
        .expect("the revoked device's connection closes after the re-pin");
}

/// Reads b's profile space of Bob as a client would.
async fn bob_records(fed: &Fed) -> Vec<LogRecord> {
    let t = endpoint(51).await;
    let c = connect(&t, &fed.b, B).await;
    c.hello(BOB, "d1", &key(51)).await.unwrap();
    let items = c.read(&format!("profile:{BOB}"), 0).await.unwrap();
    c.close();
    items
        .iter()
        .filter(|i| i["type"] == s("record"))
        .map(|i| han_core::json::from_value(&i["record"]).unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_forged_profile_chain_is_refused() {
    let fed = federation(Duration::from_secs(3600), 60).await;
    bob_adds_d2(&fed).await;
    let records = bob_records(&fed).await;
    assert_eq!(records.len(), 2);
    let keys = fed.a.dir.wellknown().refresh(B).await.unwrap().keys;
    let view = verify_profile_chain(BOB, &keys, &records).unwrap();
    assert_eq!((view.seq, view.keys.len()), (2, 2));

    // an attacker's key swapped into the genuine key.add: the id no longer matches the receipt
    let mut swapped = records.clone();
    swapped[1].entry.body = obj([
        ("keyId", s("d2")),
        ("publicKey", s(&key(99).public_key().to_base64url())),
    ]);
    assert!(verify_profile_chain(BOB, &keys, &swapped).is_err());

    // a rewritten receipt time
    let mut retimed = records.clone();
    retimed[1].receipt.time = "2099-01-01T00:00:00Z".into();
    assert!(verify_profile_chain(BOB, &keys, &retimed).is_err());

    // a chain cut at the front, or with a record missing
    assert!(verify_profile_chain(BOB, &keys, &records[1..]).is_err());
    assert!(verify_profile_chain(BOB, &keys, &[]).is_err());

    // the genuine chain under keys that are not b's
    let wrong = doc(B, 99, &fed.b.addr).keys;
    assert!(verify_profile_chain(BOB, &wrong, &records).is_err());

    // a chain whose entry another key signed: re-signed by Mallory and re-receipted by nobody
    let mut resigned = records.clone();
    let (forged, _) = entry(
        &signer(BOB, &key(99), "d1"),
        &format!("profile:{BOB}"),
        "key.add",
        obj([("keyId", s("d2")), ("publicKey", s("x"))]),
        None,
        1,
    );
    resigned[1].entry = han_core::json::from_value(&forged).unwrap();
    assert!(verify_profile_chain(BOB, &keys, &resigned).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_home_given_the_wrong_keys_for_b_pins_nothing() {
    // a believes b's server key is key(99); b signs with key(60): every receipt fails
    let fed = federation(Duration::from_secs(3600), 99).await;
    // these read profiles that no entry of a known author asks for, so b's actors are let in by the list
    fed.a
        .home
        .set_hosting(HostingPolicy::AllowList(vec![B.to_owned()]));
    let err = fed.a.dir.ensure_profile(BOB).await.unwrap_err();
    assert!(
        matches!(err, NetError::Protocol(_) | NetError::Dial(_)),
        "{err}"
    );
    assert!(fed.a.dir.pinned(BOB).is_none());

    // and a's home does not take Bob's append
    let bob_t = endpoint(51).await;
    let c = connect(&bob_t, &fed.a, A).await;
    let e = c.hello(BOB, "d1", &key(51)).await.unwrap_err();
    assert_eq!(code(&e), Some(Code::Unavailable), "{e}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn strangers_cost_no_fetch_and_a_flood_is_rate_limited() {
    let fed = federation(Duration::from_secs(3600), 60).await;
    let eve = "@eve:c.localhost";
    // nobody known asks for it, so b's actors are strangers too: denied, nothing fetched
    assert_eq!(
        fed.a.home.may_fetch_profile(BOB, None).unwrap_err().code,
        Code::Denied
    );
    fed.a.home.may_fetch_profile(BOB, Some(ALICE)).unwrap();
    fed.a
        .home
        .set_hosting(HostingPolicy::AllowList(vec![B.to_owned()]));
    // a stranger of a third server: denied, nothing fetched
    assert_eq!(
        fed.a.home.may_fetch_profile(eve, None).unwrap_err().code,
        Code::Denied
    );
    let t = endpoint(70).await;
    let c = connect(&t, &fed.a, A).await;
    let e = c.hello(eve, "d1", &key(70)).await.unwrap_err();
    assert_eq!(code(&e), Some(Code::Denied), "{e}");

    // a flood of reads of listed actors runs into the budget, and the dials stop
    let mut limited = None;
    for _ in 0..40 {
        match fed.a.dir.refresh_profile("@nobody:b.localhost").await {
            Err(NetError::Remote(w)) if w.code == Code::Limit => {
                limited = Some(w);
                break;
            }
            _ => {}
        }
    }
    let w = limited.expect("the budget runs out");
    assert!(w.retry_after_ms.is_some());
    assert_eq!(
        fed.a.home.may_fetch_profile(BOB, None).unwrap_err().code,
        Code::Limit
    );
}

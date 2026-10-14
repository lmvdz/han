//! The hosting rule (section 4.1 step 2, "the server's own hosting rules"): who may create a space whose id
//! names this server.

use std::sync::Arc;

use han_core::SigningKey;
use han_core::json::Value;
use han_core::types::Durability;
use han_seq::build::{EntrySpec, sign_entry, space_policy};
use han_seq::{
    Code, Directory, HOSTING_DENIED, Home, HomeConfig, HostingPolicy, MemStore, ServerInfo,
    ServerKey, SystemClock, TestClock, WireError,
};

const SERVER: &str = "a.localhost";
const ALICE: &str = "@alice:a.localhost";
const STRANGER: &str = "@:b.example";

fn key(n: u8) -> SigningKey {
    SigningKey::from_seed([n; 32])
}

/// The directory of one other server, `b.example`, whose key is `key(9)`.
struct Other;

impl Directory for Other {
    fn server(&self, server: &str) -> Option<ServerInfo> {
        (server == "b.example").then(|| ServerInfo {
            nodes: vec![],
            keys: vec![ServerKey {
                key_id: "b1".into(),
                public_key: key(9).public_key().to_base64url(),
                from: "1970-01-01T00:00:00Z".into(),
                until: None,
                compromised: None,
            }],
        })
    }
}

fn home(test_mode: bool) -> Home {
    let config = HomeConfig {
        server: SERVER.into(),
        key: key(6),
        key_id: "h1".into(),
        durability: Durability::Local,
        hosting: if test_mode {
            han_seq::HostingPolicy::Open
        } else {
            han_seq::HostingPolicy::KnownActors
        },
    };
    let home = if test_mode {
        Home::open_test(
            config,
            Arc::new(TestClock::new(1_767_225_600)),
            Arc::new(MemStore::new()),
        )
    } else {
        Home::open(
            config,
            Arc::new(SystemClock::new()),
            Arc::new(MemStore::new()),
        )
    }
    .unwrap();
    home.register(
        ALICE,
        "human",
        "k1",
        &key(5).public_key().to_base64url(),
        None,
    )
    .unwrap();
    home.set_directory(Arc::new(Other));
    home
}

fn entry(
    home: &Home,
    space: &str,
    kind: &str,
    author: &str,
    key_id: &str,
    k: &SigningKey,
    body: Value,
) -> (Value, Option<Value>) {
    sign_entry(
        EntrySpec {
            space: space.into(),
            kind: kind.into(),
            author: author.into(),
            key_id: key_id.into(),
            body,
            content: (kind == "space.create")
                .then(|| Value::Object([("name".to_owned(), Value::Str("x".into()))].into())),
            refs: vec![],
            seen: 0,
            time_secs: home.now_secs(),
        },
        k,
    )
    .unwrap()
}

fn create(
    home: &Home,
    space: &str,
    author: &str,
    key_id: &str,
    k: &SigningKey,
) -> Result<(), WireError> {
    let body = Value::Object([("policy".to_owned(), space_policy("members", "invite"))].into());
    let (e, content) = entry(home, space, "space.create", author, key_id, k, body);
    home.append(&e, content.as_ref()).map(|_| ())
}

#[test]
fn a_stranger_who_has_said_hello_cannot_create_a_space() {
    let home = home(false);
    // the stranger is a server actor of another home whose keys the directory knows: its profile resolves
    // and it can say hello, yet it is no human to own a space and no known actor
    let err = create(&home, "x1:a.localhost", STRANGER, "b1", &key(9)).unwrap_err();
    assert_eq!(err.code, Code::Denied);
    assert_eq!(err.message, HOSTING_DENIED);
    assert!(home.state_of("x1:a.localhost").is_none());
    // a human of another home is refused the same way, before anything is fetched for it
    let err = create(&home, "x2:a.localhost", "@eve:c.example", "k1", &key(1)).unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str()),
        (Code::Denied, HOSTING_DENIED)
    );
    assert_eq!(
        home.may_fetch_profile("@eve:c.example", None)
            .unwrap_err()
            .code,
        Code::Denied
    );
}

#[test]
fn a_local_user_can_create_a_space() {
    let home = home(false);
    create(&home, "x1:a.localhost", ALICE, "k1", &key(5)).unwrap();
    assert!(home.state_of("x1:a.localhost").is_some());
}

#[test]
fn a_member_of_a_hosted_space_is_known() {
    let home = home(false);
    create(&home, "x1:a.localhost", ALICE, "k1", &key(5)).unwrap();
    assert_eq!(
        home.may_fetch_profile(STRANGER, None).unwrap_err().code,
        Code::Denied
    );
    let body = Value::Object(
        [
            ("actor".to_owned(), Value::Str(STRANGER.into())),
            ("role".to_owned(), Value::Str("member".into())),
        ]
        .into(),
    );
    let (e, c) = entry(
        &home,
        "x1:a.localhost",
        "member.set",
        ALICE,
        "k1",
        &key(5),
        body,
    );
    home.append(&e, c.as_ref()).unwrap();
    // the rule now passes for it: the refusal left is P-03 (a server actor is no owner), not the hosting rule
    home.may_fetch_profile(STRANGER, None).unwrap();
    let err = create(&home, "x2:a.localhost", STRANGER, "b1", &key(9)).unwrap_err();
    assert_ne!(err.message, HOSTING_DENIED);
}

#[test]
fn the_setting_can_be_changed() {
    let home = home(false);
    assert_eq!(home.hosting(), HostingPolicy::KnownActors);
    let hosting = |h: &Home, who: &str| {
        create(h, "x1:a.localhost", who, "k1", &key(1))
            .unwrap_err()
            .message
            == HOSTING_DENIED
    };
    let eve = "@eve:c.example";
    let fay = "@fay:b.example";
    assert!(hosting(&home, eve) && hosting(&home, fay));
    home.set_hosting(HostingPolicy::Open);
    assert_eq!(home.hosting(), HostingPolicy::Open);
    assert!(!hosting(&home, eve) && !hosting(&home, fay));
    home.set_hosting(HostingPolicy::AllowList(vec![
        "b.example".into(),
        eve.into(),
    ]));
    assert!(!hosting(&home, eve) && !hosting(&home, fay));
    assert!(hosting(&home, "@gus:c.example"));
    // known actors keep working under the list
    create(&home, "x2:a.localhost", ALICE, "k1", &key(5)).unwrap();
    home.set_hosting(HostingPolicy::KnownActors);
    assert!(hosting(&home, eve));
}

#[test]
fn test_mode_homes_start_open() {
    let home = home(true);
    assert_eq!(home.hosting(), HostingPolicy::Open);
    let err = create(&home, "x1:a.localhost", "@eve:c.example", "k1", &key(1)).unwrap_err();
    assert_ne!(err.message, HOSTING_DENIED);
}

#[test]
fn profile_spaces_and_registration_are_not_governed() {
    let home = home(false);
    let public = key(4).public_key().to_base64url();
    home.register("@bob:a.localhost", "human", "k1", &public, None)
        .unwrap();
    home.register("@bob:a.localhost", "human", "k1", &public, None)
        .unwrap();
    assert!(home.state_of("profile:@bob:a.localhost").is_some());
    // and a freshly registered user is known at once
    create(&home, "x1:a.localhost", "@bob:a.localhost", "k1", &key(4)).unwrap();
}

#[test]
fn fetches_for_strangers_are_bounded() {
    let home = home(false);
    home.set_hosting(HostingPolicy::Open);
    let mut limited = None;
    for _ in 0..100 {
        if let Err(e) = home.may_fetch_profile("@eve:c.example", None) {
            limited = Some(e);
            break;
        }
    }
    let e = limited.expect("a budget");
    assert_eq!(e.code, Code::Limit);
    assert!(e.retry_after_ms.is_some());
}

#[test]
fn a_fetch_for_a_known_authors_entry_is_allowed_but_a_strangers_own_is_not() {
    let home = home(false);
    create(&home, "x1:a.localhost", ALICE, "k1", &key(5)).unwrap();
    let foreign = "@x:b.example";
    // a known author's invite needs the target's profile: allowed
    home.may_fetch_profile(foreign, Some(ALICE)).unwrap();
    // nothing asked by a stranger, or on a stranger's behalf, is
    for requester in [None, Some(STRANGER), Some(foreign)] {
        assert_eq!(
            home.may_fetch_profile(foreign, requester).unwrap_err().code,
            Code::Denied
        );
    }
}

#[test]
fn a_local_actor_without_a_profile_is_refused_not_unavailable() {
    let home = home(false);
    create(&home, "x1:a.localhost", ALICE, "k1", &key(5)).unwrap();
    // a member.set naming an actor of this server that has no profile here will never resolve by waiting
    let body = Value::Object(
        [
            ("actor".to_owned(), Value::Str("@exec:a.localhost".into())),
            ("role".to_owned(), Value::Str("member".into())),
        ]
        .into(),
    );
    let (e, c) = entry(
        &home,
        "x1:a.localhost",
        "member.set",
        ALICE,
        "k1",
        &key(5),
        body,
    );
    let err = home.append(&e, c.as_ref()).unwrap_err();
    assert_eq!(err.code, Code::Invalid);
    assert_eq!(err.message, "@exec:a.localhost has no profile at this home");
    // an author of this server with no profile is refused too
    let (e, c) = entry(
        &home,
        "x1:a.localhost",
        "member.set",
        "@ghost:a.localhost",
        "k1",
        &key(1),
        Value::Object(
            [
                ("actor".to_owned(), Value::Str(ALICE.into())),
                ("role".to_owned(), Value::Str("member".into())),
            ]
            .into(),
        ),
    );
    let err = home.append(&e, c.as_ref()).unwrap_err();
    assert_eq!(err.code, Code::Denied);
    // a foreign actor whose profile is still being fetched stays `unavailable`
    let body = Value::Object(
        [
            ("actor".to_owned(), Value::Str("@bob:b.example".into())),
            ("role".to_owned(), Value::Str("member".into())),
        ]
        .into(),
    );
    let (e, c) = entry(
        &home,
        "x1:a.localhost",
        "member.set",
        ALICE,
        "k1",
        &key(5),
        body,
    );
    let err = home.append(&e, c.as_ref()).unwrap_err();
    assert_eq!(err.code, Code::Unavailable);
}

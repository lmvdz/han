//! Accountability (P-03, P-07 E1/E2) on a real home with real profile spaces: an agent whose operator never
//! confirmed it, in a space whose policy says `allowAgents: "with-operator"`. The home pins the profile views
//! it reads from its own profile spaces, so a confirmation or a withdrawal there changes what Policy sees at
//! the next entry (vector `never-confirmed-agent-is-unaccountable` is the same story for Policy alone).

use std::sync::Arc;

use han_core::SigningKey;
use han_core::json::Value;
use han_core::types::Durability;
use han_seq::build::{EntrySpec, sign_entry, space_policy};
use han_seq::{Code, Home, HomeConfig, MemStore, TestClock, WireError};

const SERVER: &str = "a.localhost";
const SPACE: &str = "s1:a.localhost";
const ALICE: &str = "@alice:a.localhost";
const BOT: &str = "@bot:a.localhost";
const BOT2: &str = "@bot2:a.localhost";
const EXEC: &str = "@exec:a.localhost";

fn key(n: u8) -> SigningKey {
    SigningKey::from_seed([n; 32])
}

fn s(v: &str) -> Value {
    Value::Str(v.into())
}

fn obj<const N: usize>(fields: [(&str, Value); N]) -> Value {
    Value::Object(fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

/// The actor's signing key and its key id (every actor here has one key, `k1`).
fn key_of(actor: &str) -> SigningKey {
    match actor {
        ALICE => key(1),
        BOT => key(2),
        BOT2 => key(3),
        EXEC => key(4),
        _ => unreachable!("{actor}"),
    }
}

fn home() -> Home {
    let home = Home::open_test(
        HomeConfig {
            server: SERVER.into(),
            key: key(6),
            key_id: "h1".into(),
            durability: Durability::Local,
        },
        Arc::new(TestClock::new(1_767_225_600)),
        Arc::new(MemStore::new()),
    )
    .unwrap();
    let pk = |a: &str| key_of(a).public_key().to_base64url();
    home.register(ALICE, "human", "k1", &pk(ALICE), None)
        .unwrap();
    for (actor, kind) in [(BOT, "agent"), (BOT2, "agent"), (EXEC, "service")] {
        home.register(actor, kind, "k1", &pk(actor), Some(ALICE))
            .unwrap();
    }
    home
}

/// Signs `body` as `author` and appends it; the entry's id on success.
fn append(
    home: &Home,
    space: &str,
    kind: &str,
    author: &str,
    body: Value,
) -> Result<String, WireError> {
    append_with(home, space, kind, author, body, None)
}

fn append_with(
    home: &Home,
    space: &str,
    kind: &str,
    author: &str,
    body: Value,
    content: Option<Value>,
) -> Result<String, WireError> {
    let (e, content) = sign_entry(
        EntrySpec {
            space: space.into(),
            kind: kind.into(),
            author: author.into(),
            key_id: "k1".into(),
            body,
            content: content.or_else(|| (kind == "space.create").then(|| obj([("name", s("x"))]))),
            refs: vec![],
            seen: 0,
            time_secs: home.now_secs(),
        },
        &key_of(author),
    )
    .unwrap();
    home.append(&e, content.as_ref()).map(|r| r.id)
}

fn denied(r: Result<String, WireError>, why: &str) {
    let err = r.expect_err(why);
    assert_eq!(err.code, Code::Denied, "{why}: {err:?}");
    // denied for accountability, not for something else
    assert!(err.message.contains("operator"), "{why}: {}", err.message);
}

fn member(home: &Home, actor: &str, role: &str) -> Result<String, WireError> {
    append(
        home,
        SPACE,
        "member.set",
        ALICE,
        obj([("actor", s(actor)), ("role", s(role))]),
    )
}

/// The operator confirms (`owner`) or withdraws (`none`) in the agent's profile space (section 4.3).
fn operator(home: &Home, agent: &str, role: &str) {
    append(
        home,
        &format!("profile:{agent}"),
        "member.set",
        ALICE,
        obj([("actor", s(ALICE)), ("role", s(role))]),
    )
    .unwrap();
    let confirmed = home
        .state_of(&format!("profile:{agent}"))
        .and_then(|st| st.profile.as_ref().map(|p| p.operator_confirmed));
    assert_eq!(confirmed, Some(role == "owner"));
}

fn propose(home: &Home, author: &str, executor: &str, label: &str) -> Result<String, WireError> {
    let plan = obj([("action", s("git.push")), ("target", s(label))]);
    let digest = han_core::ids::json_digest(&plan).unwrap();
    append_with(
        home,
        SPACE,
        "effect.propose",
        author,
        obj([
            ("executor", s(executor)),
            ("action", s("git.push")),
            ("planDigest", s(&digest)),
            ("repeatable", Value::Bool(false)),
        ]),
        Some(obj([("plan", plan)])),
    )
}

fn claim(home: &Home, author: &str, proposal: &str) -> Result<String, WireError> {
    append(
        home,
        SPACE,
        "effect.claim",
        author,
        obj([
            ("proposal", s(proposal)),
            ("attempt", Value::Int(1)),
            ("instance", s("k1")),
            ("window", Value::Int(30)),
        ]),
    )
}

fn set_policy(home: &Home, allow_agents: &str) {
    let mut p = space_policy("members", "invite");
    if let Value::Object(m) = &mut p {
        m.insert("allowAgents".into(), s(allow_agents));
    }
    append(home, SPACE, "policy.set", ALICE, obj([("policy", p)])).unwrap();
}

fn create(home: &Home, allow_agents: &str) {
    let mut p = space_policy("members", "invite");
    if let Value::Object(m) = &mut p {
        m.insert("allowAgents".into(), s(allow_agents));
    }
    append(home, SPACE, "space.create", ALICE, obj([("policy", p)])).unwrap();
    // the executor is a service whose operator confirmed it
    operator(home, EXEC, "owner");
    member(home, EXEC, "member").unwrap();
}

#[test]
fn with_operator_denies_an_unconfirmed_agent_a_role() {
    let home = home();
    create(&home, "with-operator");
    let profile = home.state_of(&format!("profile:{BOT}")).unwrap();
    assert!(!profile.profile.as_ref().unwrap().operator_confirmed);
    denied(
        member(&home, BOT, "member"),
        "member.set of an unconfirmed agent",
    );
    denied(member(&home, BOT, "guest"), "a guest role is readable too");
    assert!(!home.state_of(SPACE).unwrap().roles.contains_key(BOT));
    // the confirmation is read from the profile space at the next entry: no restart, no stale view
    operator(&home, BOT, "owner");
    member(&home, BOT, "member").unwrap();
}

#[test]
fn an_unconfirmed_agent_already_in_the_space_cannot_propose_or_claim() {
    let home = home();
    // under "any" an unconfirmed agent joins and proposes
    create(&home, "any");
    member(&home, BOT, "member").unwrap();
    let p0 = propose(&home, BOT, EXEC, "p0").unwrap();
    // the space switches to with-operator: the agents already in it stop
    set_policy(&home, "with-operator");
    denied(
        member(&home, BOT2, "member"),
        "granting a role to another unconfirmed agent",
    );
    denied(
        propose(&home, BOT, EXEC, "p1"),
        "effect.propose by the agent",
    );
    denied(claim(&home, EXEC, &p0), "E2: its proposal can't be claimed");
    let p2 = propose(&home, ALICE, BOT, "p2").unwrap();
    denied(claim(&home, BOT, &p2), "E1: effect.claim by the agent");
    // a move between readable roles is never re-checked
    member(&home, BOT, "guest").unwrap();
    member(&home, BOT, "member").unwrap();
    // the operator confirms: all of it goes through
    operator(&home, BOT, "owner");
    claim(&home, EXEC, &p0).unwrap();
    claim(&home, BOT, &p2).unwrap();
    propose(&home, BOT, EXEC, "p3").unwrap();
    // and a withdrawal stops it again
    operator(&home, BOT, "none");
    denied(propose(&home, BOT, EXEC, "p4"), "after the withdrawal");
}

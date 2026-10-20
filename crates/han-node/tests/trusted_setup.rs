//! Trusted team creation publishes one complete policy, with narrow automatic authority.

mod common;

use common::{World, call, space_of};
use han_core::policy::{Change, Gate, change_tier, proposer_suffices};
use han_node::api::{
    CallResult, Command, CommandClass, ConnectionClass, NodeApi, NodeError, trusted_team_policy,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn trusted_creation_installs_the_complete_scoped_policy() {
    let world = World::new(&[("@alice:a.localhost", 41)]);
    let alice = world.node("@alice:a.localhost", 41);
    let command = Command::SpaceCreateTrusted {
        home: world.home_ref.clone(),
        name: "Team".into(),
        repos: vec!["github:o/r".into()],
    };
    assert_eq!(command.class(), CommandClass::Face);
    assert!(command.needs_presence());
    let space = space_of(&call(&alice, command).await);
    let state = world.home.state_of(&space).unwrap();
    assert_eq!(state.head, 1, "creation and policy are one entry");
    let policy = &state.policy;
    let expected = trusted_team_policy(&["github:o/r".into()]).unwrap();
    assert_eq!(serde_json::to_value(policy).unwrap(), expected);
    for (repo, path, lines, truncated, automatic) in [
        ("github:o/r", "src/main.rs", 500, false, true),
        ("github:o/r", "src/main.rs", 501, false, false),
        ("github:o/other", "src/main.rs", 5, false, false),
        ("github:o/r", ".github/workflows/test.yml", 5, false, false),
        ("github:o/r", "Cargo.toml", 5, false, false),
        ("github:o/r", "src/main.rs", 5, true, false),
    ] {
        let change: Change = serde_json::from_value(serde_json::json!({
            "repo": repo, "deltaDigest": "a".repeat(64), "total": if truncated {2} else {1},
            "files": [{"path": path, "kind": "modify", "lines": lines}],
            "truncated": truncated
        }))
        .unwrap();
        let gate: Gate = serde_json::from_value(serde_json::json!({
            "repo": repo, "tier": change_tier(policy.gates.as_ref().unwrap(), &change)
        }))
        .unwrap();
        assert_eq!(
            proposer_suffices(&policy.approvals, "vcs.land", Some(&gate)),
            automatic,
            "{repo} {path} {lines} truncated={truncated}"
        );
    }
    assert!(!proposer_suffices(&policy.approvals, "vcs.land", None));
    assert!(!proposer_suffices(&policy.approvals, "vcs.try", None));
}

#[test]
fn invalid_repository_selection_is_rejected_before_creation() {
    for repos in [
        vec![],
        vec!["*".into()],
        vec!["github:o/*".into()],
        vec!["github:o/r".into(); 2],
        vec!["a".repeat(129)],
        vec!["a b".into()],
    ] {
        assert!(trusted_team_policy(&repos).is_err(), "{repos:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn offline_creation_returns_queued_and_delivers_the_same_policy_later() {
    let mut world = World::offline(&[("@alice:a.localhost", 41)]);
    let alice = world.node_with("@alice:a.localhost", 41, |c| {
        c.connect_wait = std::time::Duration::from_millis(200)
    });
    let result = call(
        &alice,
        Command::SpaceCreateTrusted {
            home: world.home_ref.clone(),
            name: "Team".into(),
            repos: vec!["github:o/r".into()],
        },
    )
    .await;
    let CallResult::Queued { space, .. } = result else {
        panic!("must not report confirmed creation: {result:?}")
    };
    assert!(world.home.state_of(&space).is_none());
    world.start_home();
    common::until("queued trusted creation reaches the home", || {
        world.home.state_of(&space).is_some()
    })
    .await;
    let state = world.home.state_of(&space).unwrap();
    assert_eq!(state.head, 1);
    assert_eq!(
        serde_json::to_value(&state.policy).unwrap(),
        trusted_team_policy(&["github:o/r".into()]).unwrap()
    );
    alice.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn automatic_authority_requires_a_person_and_cannot_be_granted_by_a_host() {
    use han_node::rpc::{RpcClient, default_endpoint, serve_as};
    use std::sync::Arc;
    let world = World::new(&[("@alice:a.localhost", 41)]);
    let script = han_presence::testing::Scripted::new();
    let alice = world.node_with("@alice:a.localhost", 41, |c| c.set_presence(script.clone()));
    let command = Command::SpaceCreateTrusted {
        home: world.home_ref.clone(),
        name: "Team".into(),
        repos: vec!["github:o/r".into()],
    };
    let dir = world.dir.join("trusted-rpc");
    std::fs::create_dir_all(&dir).unwrap();
    let endpoint = default_endpoint(&dir);
    let api: Arc<dyn NodeApi> = Arc::new(alice.clone());
    let _face = serve_as(api.clone(), &endpoint, ConnectionClass::Face).unwrap();
    let _host = serve_as(api, &format!("{endpoint}-host"), ConnectionClass::Host).unwrap();
    let host = RpcClient::connect(&format!("{endpoint}-host"))
        .await
        .unwrap();
    assert!(matches!(
        host.call(command.clone()).await,
        Err(NodeError::Denied { .. })
    ));
    assert!(script.prompts().is_empty());
    let face = RpcClient::connect(&endpoint).await.unwrap();
    script.fail(han_presence::PresenceError::Cancelled);
    assert!(face.call(command).await.is_err());
    assert!(
        !script.prompts().is_empty(),
        "even a server-name home needs presence for this grant"
    );
    alice.shutdown();
}

//! A revoked device keeps no connection (N-02, X14): the node re-checks that it still vouches for its actor,
//! and closes the connection when it no longer does.

mod common;

use std::time::Duration;

use common::{Rig, SERVER, Signer, endpoint, entry, key, obj, s};
use han_net::{Client, Connection, ServeOptions, Transport};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revoked_device_is_disconnected() {
    let alice = "@alice:a.localhost";
    let rig = Rig::start(
        &[(alice, 31)],
        ServeOptions {
            // the push on the revoking receipt, not the timer, closes the connection
            revalidate_every: Duration::from_secs(3600),
            ..ServeOptions::default()
        },
    )
    .await;
    let profile = format!("profile:{alice}");
    let laptop = endpoint(31).await;
    let phone = endpoint(41).await;
    let nodes = vec![rig.addr.node.clone()];

    // the laptop registers the phone's key (N-01) in Alice's profile
    let l = Client::connect(&laptop, &rig.addr, SERVER, &nodes)
        .await
        .unwrap();
    l.hello(alice, "d1", &key(31)).await.unwrap();
    let (add, _) = entry(
        &Signer {
            actor: alice,
            key: &key(31),
            key_id: "d1",
        },
        &profile,
        "key.add",
        obj([
            ("keyId", s("d2")),
            ("publicKey", s(&key(41).public_key().to_base64url())),
        ]),
        None,
        1,
    );
    l.append(&add, None).await.unwrap();

    // the phone connects: its node vouches for Alice now
    let p = Client::connect(&phone, &rig.addr, SERVER, &nodes)
        .await
        .unwrap();
    p.hello(alice, "d2", &key(41)).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(700), p.connection().closed())
            .await
            .is_err(),
        "still vouched"
    );

    // the laptop revokes the phone's key: from that receipt the phone's connection closes
    let (revoke, _) = entry(
        &Signer {
            actor: alice,
            key: &key(31),
            key_id: "d1",
        },
        &profile,
        "key.revoke",
        obj([("keyId", s("d2"))]),
        None,
        2,
    );
    l.append(&revoke, None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), p.connection().closed())
        .await
        .expect("the revoked device's connection closes");
    // and it cannot start another
    let again = Client::connect(&phone, &rig.addr, SERVER, &nodes)
        .await
        .unwrap();
    assert!(again.hello(alice, "d2", &key(41)).await.is_err());
    let _ = laptop.node_id();
}

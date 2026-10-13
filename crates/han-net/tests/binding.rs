//! G-08, SR-13: on a live `han/1` connection a `hello` binds to the connection. A proof that verifies but was made
//! for a key that is not the connecting node (a relayed proof) is denied; the actor's own device is let in.

mod common;

use std::time::Duration;

use common::{Rig, SERVER, endpoint, key};
use han_net::{Client, ServeOptions};
use han_seq::Code;

const ALICE: &str = "@alice:a.localhost";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relayed_hello_is_refused_over_quic() {
    tokio::time::timeout(Duration::from_secs(60), scenario())
        .await
        .unwrap();
}

async fn scenario() {
    let rig = Rig::start(&[(ALICE, 31)], ServeOptions::default()).await;
    let server_nodes = rig.home.nodes_for(&format!("@:{SERVER}"));
    let alice_key = key(31);

    // another device's connection carries Alice's signature: the proof verifies, the node is not hers
    let relay_t = endpoint(77).await;
    let relayed = Client::connect(&relay_t, &rig.addr, SERVER, &server_nodes)
        .await
        .unwrap();
    let e = relayed.hello(ALICE, "d1", &alice_key).await.unwrap_err();
    assert_eq!(e.wire().map(|w| w.code), Some(Code::Denied), "{e:?}");
    // and it did not authenticate: nothing but hello is open to it
    let again = relayed.hello(ALICE, "d1", &alice_key).await.unwrap_err();
    assert_eq!(again.wire().map(|w| w.code), Some(Code::Denied));
    let read = relayed.read("team:a.localhost", 0).await.unwrap_err();
    assert_eq!(read.wire().map(|w| w.code), Some(Code::Denied), "{read:?}");

    // the same holds for the server actor on a node its sequencer list lacks
    let server = Client::connect(&relay_t, &rig.addr, SERVER, &server_nodes)
        .await
        .unwrap();
    let e = server
        .hello(&format!("@:{SERVER}"), "h1", &key(30))
        .await
        .unwrap_err();
    assert_eq!(e.wire().map(|w| w.code), Some(Code::Denied), "{e:?}");

    // Alice's own device: the node is her d1 key
    let alice_t = endpoint(31).await;
    let own = Client::connect(&alice_t, &rig.addr, SERVER, &server_nodes)
        .await
        .unwrap();
    own.hello(ALICE, "d1", &alice_key).await.unwrap();
}

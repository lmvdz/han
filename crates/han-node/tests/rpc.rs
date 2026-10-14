//! Local JSON-RPC over the platform's pipe: a named pipe on Windows (current user only), a Unix socket
//! elsewhere. A client holds the node API as a trait object and cannot tell it from the node itself.

mod common;

use std::sync::Arc;

use common::{World, until, window};
use han_node::api::{
    CallResult, Command, NodeApi, NodeError, Query, Row, SpaceLink, ViewEvent, ViewMirror,
};
use han_node::rpc::{RpcClient, default_endpoint, serve};

const ALICE: &str = "@alice:a.localhost";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_face_drives_the_node_over_the_pipe() {
    let world = World::new(&[(ALICE, 31)]);
    let node = world.node(ALICE, 31);
    let dir = world.dir.join("rpc-test");
    std::fs::create_dir_all(&dir).unwrap();
    let endpoint = default_endpoint(&dir);
    let api: Arc<dyn NodeApi> = Arc::new(node.clone());
    let _server = serve(api, &endpoint).unwrap();

    let face: Box<dyn NodeApi> = Box::new(RpcClient::connect(&endpoint).await.unwrap());

    // A view first: the snapshot arrives before anything else.
    let mut node_view = face.view(Query::NodeStatus { window: window() });
    let Some(ViewEvent::Snapshot { rows, total, .. }) = node_view.next().await else {
        panic!("a snapshot first");
    };
    assert_eq!(total, 1);
    assert!(matches!(&rows[..], [Row::NodeStatus(n)] if n.actor == ALICE && n.spaces == 0));

    // A command with a result, a command that fails with a closed-set error.
    let CallResult::Space { space } = face
        .call(Command::SpaceCreate {
            home: world.home_ref.clone(),
            name: "team".into(),
            visibility: "members".into(),
            join: "invite".into(),
        })
        .await
        .unwrap()
    else {
        panic!("a space");
    };
    let e = face
        .call(Command::MemberSet {
            space: "none:a.localhost".into(),
            actor: ALICE.into(),
            role: "member".into(),
        })
        .await
        .unwrap_err();
    assert!(matches!(e, NodeError::NotFound { .. }), "{e:?}");

    // The node view shows the new space as a diff; mirror it and compare with a fresh read.
    let mut mirror = ViewMirror::new();
    mirror.apply(ViewEvent::Snapshot {
        seq: 1,
        rows,
        total,
        offset: 0,
        next_cursor: None,
        now_ms: 0,
        totals: None,
    });
    until("the node status shows the space", || {
        while let Some(ev) = node_view.try_next() {
            mirror.apply(ev);
        }
        mirror
            .rows()
            .iter()
            .any(|r| matches!(r, Row::NodeStatus(n) if n.spaces == 1))
    })
    .await;

    // Ending a view over the wire ends it on the node.
    let mut status = face.view(Query::SpaceStatus {
        space: space.clone(),
        window: window(),
    });
    assert!(matches!(
        status.next().await,
        Some(ViewEvent::Snapshot { .. })
    ));
    until("the space is live", || {
        while let Some(ViewEvent::Diff { .. } | ViewEvent::Snapshot { .. }) = status.try_next() {}
        matches!(
            node.read(&Query::SpaceStatus { space: space.clone(), window: window() }),
            Ok(ViewEvent::Snapshot { rows, .. }) if matches!(&rows[..], [Row::SpaceStatus(s)] if s.link == SpaceLink::Live)
        )
    })
    .await;
    drop(status);
    drop(node_view);
    until("the node has dropped both views", || {
        // Only the views this test still holds remain; the server cancelled the dropped ones.
        std::thread::sleep(std::time::Duration::from_millis(20));
        node.open_views() == 0
    })
    .await;

    // A view that cannot be served fails over the wire too.
    let mut bad = face.view(Query::Overlaps {
        space: "none:a.localhost".into(),
        window: window(),
    });
    assert!(matches!(
        bad.next().await,
        Some(ViewEvent::Failed {
            error: NodeError::NotFound { .. },
            ..
        })
    ));
    node.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn garbage_gets_a_json_rpc_error_not_a_crash() {
    use interprocess::local_socket::tokio::{Stream, prelude::*};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let world = World::new(&[(ALICE, 31)]);
    let node = world.node(ALICE, 31);
    let dir = world.dir.join("rpc-garbage");
    std::fs::create_dir_all(&dir).unwrap();
    let endpoint = default_endpoint(&dir);
    let _server = serve(Arc::new(node.clone()), &endpoint).unwrap();
    #[cfg(windows)]
    let name = {
        use interprocess::local_socket::{GenericNamespaced, ToNsName};
        endpoint.as_str().to_ns_name::<GenericNamespaced>().unwrap()
    };
    #[cfg(not(windows))]
    let name = {
        use interprocess::local_socket::{GenericFilePath, ToFsName};
        endpoint.as_str().to_fs_name::<GenericFilePath>().unwrap()
    };
    let conn = Stream::connect(name).await.unwrap();
    let (recv, mut send) = conn.split();
    let mut lines = BufReader::new(recv).lines();
    for (input, code) in [
        ("not json", -32700),
        (r#"{"jsonrpc":"2.0","id":1,"method":"nope"}"#, -32601),
        (
            r#"{"jsonrpc":"2.0","id":2,"method":"call","params":{"type":"warp"}}"#,
            -32602,
        ),
    ] {
        send.write_all(format!("{input}\n").as_bytes())
            .await
            .unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["error"]["code"], code, "{line}");
    }
    node.shutdown();
}

//! The host reads the landing lane through its node: the lane's `han.land/1` items (published on the executor's
//! `han.land lane` session, as `han-exec`'s lane does) come back as the latest item of each change, on a node
//! that is not the executor's.

use std::sync::Arc;

use han_host::land::{LaneChange, LaneFeed, NodeLaneFeed};
use han_node::api::{CallResult, Command, NodeApi};
use serde_json::json;

mod common;
mod production_shim;
use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lane_items_reach_the_host_through_the_node() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let world = World::new(root);
    let alice = world.node(ALICE, 1);
    let coder = world.node(CODER, 2);
    let exec = world.node(EXEC, 4);
    let space = setup_space(&world, &alice, &[(CODER, &coder), (EXEC, &exec)], &[]).await;

    // the lane's session, as `Lane::session` opens it
    let CallResult::Receipt { id: lane, .. } = call(
        &exec,
        Command::SessionOpen {
            space: space.clone(),
            steer_mode: None,
            task: None,
            title: Some("han.land lane".into()),
        },
    )
    .await
    else {
        panic!("a session")
    };
    let publish = |body: serde_json::Value| {
        call(
            &exec,
            Command::StreamPublish {
                space: space.clone(),
                session: lane.clone(),
                name: "han.land/1".into(),
                body: body.to_string(),
            },
        )
    };
    // Stream items are live only (the home replays nothing), so a node that has not yet registered its watch
    // misses them: the lane's item is sent until this host's node has it.
    let api: Arc<dyn NodeApi> = Arc::new(coder.clone());
    let feed = NodeLaneFeed::new(api, space.clone(), EXEC.into());
    let mut seen: Vec<LaneChange> = Vec::new();
    for _ in 0..100 {
        publish(json!({
            "item": "change", "proposal": "E-1", "repo": "local:origin", "ref": "refs/heads/main",
            "lane": "local:origin#refs/heads/main", "state": "queued", "position": 1, "seq": 1,
            "at": "2026-10-15T10:00:00Z",
        }))
        .await;
        publish(json!({
            "item": "lane", "repo": "local:origin", "ref": "refs/heads/main", "instance": EXEC,
            "depth": 1, "batchSize": 1, "parallel": 1, "running": [], "seq": 1,
            "at": "2026-10-15T10:00:00Z",
        }))
        .await;
        match feed.changes().await {
            Ok(v) => seen = v,
            Err(e) => eprintln!("feed: {e}"),
        }
        if !seen.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(
        seen.len(),
        1,
        "the lane's one change, and not its lane item: {seen:?}"
    );
    assert_eq!(seen[0].proposal, "E-1");
    assert_eq!(seen[0].state, "queued");
    assert_eq!(seen[0].position, Some(1));

    // a later item of the same change replaces the earlier one
    publish(json!({
        "item": "change", "proposal": "E-1", "repo": "local:origin", "ref": "refs/heads/main",
        "lane": "local:origin#refs/heads/main", "state": "rejected", "reason": "conflict", "seq": 2,
        "at": "2026-10-15T10:01:00Z",
    }))
    .await;
    until("the rejection reaches the host's node", 15, || {
        let feed = &feed;
        let seen = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(feed.changes())
        });
        seen.map(|s| s.len() == 1 && s[0].state == "rejected")
            .unwrap_or(false)
    })
    .await;
}

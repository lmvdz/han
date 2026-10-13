//! han-github against a mock GitHub (wiremock) and a recording node: ETag 304, pagination, labels that
//! trigger tasks, scoped read tokens. No live GitHub call.

use std::sync::{Arc, Mutex};

use han_exec::github::{GithubMinter, JwtSource};
use han_exec::{GithubApp, Secret};
use han_github::{Config, Reader, Syncer};
use han_node::api::{
    BoxFuture, CallResult, Command, NodeApi, NodeError, Query, Row, TaskRow, ViewEvent, ViewStream,
};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct StaticJwt;

impl JwtSource for StaticJwt {
    fn jwt(&self, _: u64, _: u64) -> Result<Secret, String> {
        Ok(Secret::new("test-app-jwt"))
    }
}

/// A node that records commands and answers `Tasks` with a fixed list.
#[derive(Default)]
struct Node {
    calls: Mutex<Vec<Command>>,
    tasks: Mutex<Vec<TaskRow>>,
}

impl Node {
    fn calls(&self) -> Vec<Command> {
        self.calls.lock().unwrap().clone()
    }
    fn published(&self) -> Vec<Value> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                Command::StreamPublish { name, body, .. } => {
                    assert_eq!(name, "github.issue/1");
                    Some(serde_json::from_str(&body).unwrap())
                }
                _ => None,
            })
            .collect()
    }
    fn task_externals(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                Command::TaskCreate { external, .. } => external,
                _ => None,
            })
            .collect()
    }
}

impl NodeApi for Node {
    fn call(&self, cmd: Command) -> BoxFuture<'_, Result<CallResult, NodeError>> {
        let n = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(cmd);
            calls.len()
        };
        Box::pin(async move {
            Ok(CallResult::Receipt {
                space: "s".into(),
                seq: n as u64,
                id: format!("e{n}"),
                time: "t".into(),
            })
        })
    }

    fn view(&self, query: Query) -> ViewStream {
        assert!(matches!(query, Query::Tasks { .. }));
        let (sink, stream) = ViewStream::channel();
        let rows: Vec<Row> = self
            .tasks
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .map(Row::Task)
            .collect();
        sink.send(ViewEvent::Snapshot {
            rows,
            total: 0,
            offset: 0,
            next_cursor: None,
        });
        stream
    }
}

fn task_row(external: &str) -> TaskRow {
    TaskRow {
        key: "k".into(),
        space: "s".into(),
        task: "t".into(),
        seq: 1,
        author: "@a:h".into(),
        to: vec![],
        external: Some(external.into()),
        ask: None,
        open: false,
    }
}

fn issue(n: u64, title: &str, labels: &[&str], updated: &str) -> Value {
    json!({
        "number": n, "title": title, "state": "open", "html_url": format!("https://github.com/o/r/issues/{n}"),
        "labels": labels.iter().map(|l| json!({"name": l})).collect::<Vec<_>>(),
        "assignees": [], "updated_at": updated,
    })
}

async fn mock_tokens(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/app/installations/7/access_tokens"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"token": "ghs_test"})))
        .mount(server)
        .await;
}

fn rig(server: &MockServer, tasks_to: &str, node: &Arc<Node>) -> Syncer<Reader> {
    let cfg = Config::parse(&format!(
        r#"
        space = "!space:h"
        data_dir = "/nowhere"
        repos = ["o/r"]
        [app]
        app_id = 1
        installation_id = 7
        private_key = "/secret/key.pem"
        api_base = "{}"
        [tasks]
        pool = [{tasks_to}]
        "#,
        server.uri()
    ))
    .unwrap();
    let app = GithubApp {
        app_id: 1,
        installation_id: 7,
        api_base: Some(server.uri()),
        private_key: String::new(),
    };
    let reader = Reader::new(GithubMinter::new(&app, Box::new(StaticJwt)).unwrap());
    Syncer::new(&cfg, node.clone(), reader)
}

#[tokio::test]
async fn etag_304_publishes_nothing_and_sends_the_etag() {
    let server = MockServer::start().await;
    mock_tokens(&server).await;
    // The conditional request gets 304; the unconditional one gets the list. The more specific mock wins by
    // being mounted first.
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .and(header("if-none-match", "\"v1\""))
        .respond_with(ResponseTemplate::new(304))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", "\"v1\"")
                .set_body_json(json!([issue(1, "One", &["bug"], "2026-10-01T00:00:00Z")])),
        )
        .expect(1)
        .mount(&server)
        .await;
    let node = Arc::new(Node::default());
    let mut s = rig(&server, "", &node);

    let t = s.poll("o/r").await.unwrap();
    assert!(!t.not_modified);
    assert_eq!(t.published, 1);
    let t = s.poll("o/r").await.unwrap();
    assert!(t.not_modified);
    assert_eq!(t.published, 0);

    let items = node.published();
    assert_eq!(items.len(), 1);
    let first = &items[0]["issues"][0];
    assert_eq!(first["ref"], "github:o/r#1");
    assert_eq!(first["labels"], json!(["bug"]));
    assert_eq!(first["agent_ready"], false);
    assert_eq!(items[0]["repo"], "github:o/r");
    // One session for the repo, opened once.
    let opens = node
        .calls()
        .iter()
        .filter(|c| matches!(c, Command::SessionOpen { .. }))
        .count();
    assert_eq!(opens, 1);
}

#[tokio::test]
async fn read_tokens_are_scoped_to_one_repo_and_issues_read() {
    let server = MockServer::start().await;
    mock_tokens(&server).await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .and(header("authorization", "Bearer ghs_test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&server)
        .await;
    let node = Arc::new(Node::default());
    let mut s = rig(&server, "", &node);
    s.poll("o/r").await.unwrap();

    let reqs = server.received_requests().await.unwrap();
    let mint = reqs.iter().find(|r| r.method.as_str() == "POST").unwrap();
    let body: Value = serde_json::from_slice(&mint.body).unwrap();
    assert_eq!(
        body,
        json!({"repositories": ["r"], "permissions": {"issues": "read"}})
    );
    // Reads only: no request but GETs after the mint.
    assert!(
        reqs.iter()
            .all(|r| matches!(r.method.as_str(), "GET" | "POST"))
    );
    assert_eq!(
        reqs.iter().filter(|r| r.method.as_str() == "POST").count(),
        1
    );
}

#[tokio::test]
async fn pagination_follows_next_links_and_skips_pull_requests() {
    let server = MockServer::start().await;
    mock_tokens(&server).await;
    let next = format!(
        "<{}/repos/o/r/issues?state=all&sort=updated&direction=desc&per_page=100&page=2>; rel=\"next\"",
        server.uri()
    );
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .and(query_param_is_missing("page"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("link", next.as_str())
                .set_body_json(json!([
                    issue(3, "Three", &[], "2026-10-03T00:00:00Z"),
                    {"number": 9, "title": "A PR", "state": "open", "pull_request": {}, "updated_at": "2026-10-03T00:00:00Z"},
                ])),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            issue(2, "Two", &[], "2026-10-02T00:00:00Z"),
            issue(1, "One", &[], "2026-10-01T00:00:00Z"),
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let node = Arc::new(Node::default());
    let mut s = rig(&server, "", &node);

    let t = s.poll("o/r").await.unwrap();
    assert_eq!(t.published, 3);
    let refs: Vec<String> = node
        .published()
        .iter()
        .flat_map(|b| b["issues"].as_array().unwrap().clone())
        .map(|i| i["ref"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(refs, ["github:o/r#3", "github:o/r#2", "github:o/r#1"]);
}

#[tokio::test]
async fn a_next_link_off_the_api_base_is_refused() {
    let server = MockServer::start().await;
    mock_tokens(&server).await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("link", "<https://evil.example/steal?page=2>; rel=\"next\"")
                .set_body_json(json!([issue(1, "One", &[], "2026-10-01T00:00:00Z")])),
        )
        .mount(&server)
        .await;
    let node = Arc::new(Node::default());
    let mut s = rig(&server, "", &node);
    assert!(s.poll("o/r").await.is_err());
    assert!(node.published().is_empty());
}

#[tokio::test]
async fn the_label_makes_a_task_once_and_only_for_open_issues() {
    let server = MockServer::start().await;
    mock_tokens(&server).await;
    let mut closed = issue(5, "Closed one", &["agent-ready"], "2026-10-05T00:00:00Z");
    closed["state"] = json!("closed");
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            issue(4, "Plain", &["bug"], "2026-10-06T00:00:00Z"),
            issue(3, "Ready", &["agent-ready"], "2026-10-04T00:00:00Z"),
            closed,
            issue(
                2,
                "Has a task already",
                &["agent-ready"],
                "2026-10-02T00:00:00Z"
            ),
        ])))
        .mount(&server)
        .await;
    let node = Arc::new(Node::default());
    node.tasks.lock().unwrap().push(task_row("github:o/r#2"));
    let mut s = rig(&server, "\"@bot:h\"", &node);

    let t = s.poll("o/r").await.unwrap();
    assert_eq!(t.tasks, ["github:o/r#3"]);
    assert_eq!(node.task_externals(), ["github:o/r#3"]);
    let create = node
        .calls()
        .into_iter()
        .find_map(|c| match c {
            Command::TaskCreate { to, ask, .. } => Some((to, ask)),
            _ => None,
        })
        .unwrap();
    assert_eq!(create.0, ["@bot:h"]);
    assert!(create.1.contains("Ready"));

    // The same list again (no ETag from this mock) changes nothing, so no second task and no republish.
    let t = s.poll("o/r").await.unwrap();
    assert_eq!(t.published, 0);
    assert_eq!(node.task_externals().len(), 1);
    let flags: Vec<bool> = node.published()[0]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["agent_ready"].as_bool().unwrap())
        .collect();
    assert_eq!(flags, [false, true, false, true]);
}

#[tokio::test]
async fn gaining_the_label_later_makes_the_task() {
    let server = MockServer::start().await;
    mock_tokens(&server).await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .and(header("if-none-match", "\"v1\""))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", "\"v2\"")
                .set_body_json(json!([issue(
                    1,
                    "Now ready",
                    &["agent-ready"],
                    "2026-10-02T00:00:00Z"
                )])),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", "\"v1\"")
                .set_body_json(json!([issue(1, "Now ready", &[], "2026-10-01T00:00:00Z")])),
        )
        .mount(&server)
        .await;
    let node = Arc::new(Node::default());
    let mut s = rig(&server, "\"@bot:h\"", &node);

    assert!(s.poll("o/r").await.unwrap().tasks.is_empty());
    let t = s.poll("o/r").await.unwrap();
    assert_eq!(t.tasks, ["github:o/r#1"]);
}

#[tokio::test]
async fn no_assignees_configured_means_no_tasks_and_a_human_request_is_deduplicated() {
    let server = MockServer::start().await;
    mock_tokens(&server).await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([issue(
            1,
            "Ready",
            &["agent-ready"],
            "2026-10-01T00:00:00Z"
        )])))
        .mount(&server)
        .await;
    let node = Arc::new(Node::default());
    let mut s = rig(&server, "", &node);
    assert!(s.poll("o/r").await.unwrap().tasks.is_empty());
    assert!(node.task_externals().is_empty());

    let mut s = rig(&server, "\"@bot:h\"", &node);
    assert!(
        s.request("github:o/r#8", "By request")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        s.request("github:o/r#8", "By request")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(node.task_externals(), ["github:o/r#8"]);
}

#[test]
fn the_config_never_shows_the_key_path() {
    let cfg = Config::parse(
        r#"
        space = "!s:h"
        endpoint = "e"
        repos = ["o/r"]
        [app]
        app_id = 1
        installation_id = 2
        private_key = "/very/secret/path.pem"
        "#,
    )
    .unwrap();
    assert!(!format!("{cfg:?}").contains("secret"));
    assert_eq!(cfg.poll_secs, 60);
    assert_eq!(cfg.tasks.label, "agent-ready");
    assert!(Config::parse("space=\"s\"\nendpoint=\"e\"\nrepos=[\"bad\"]\n[app]\napp_id=1\ninstallation_id=2\nprivate_key=\"k\"").is_err());
}

#[test]
fn tasks_go_to_the_pool_and_to_is_merged_in_without_duplicates() {
    let parse = |tasks: &str| {
        Config::parse(&format!(
            "space=\"s\"\nendpoint=\"e\"\nrepos=[\"o/r\"]\n[app]\napp_id=1\ninstallation_id=2\nprivate_key=\"k\"\n[tasks]\n{tasks}"
        ))
        .unwrap()
    };
    let cfg = parse("pool = [\"@r1:h\", \"@r2:h\"]\nto = [\"@r2:h\", \"@lars:h\"]");
    assert_eq!(cfg.tasks.addressees(), ["@r1:h", "@r2:h", "@lars:h"]);
    assert_eq!(parse("to = [\"@lars:h\"]").tasks.addressees(), ["@lars:h"]);
    assert!(parse("").tasks.addressees().is_empty());
}

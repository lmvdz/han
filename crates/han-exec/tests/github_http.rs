//! `open_pr` against a mock GitHub (scoped installation tokens, SR-49/SR-67; an existing PR found first,
//! SR-69), `http_request` against a local server, the address classes of SR-37, and a canary scan of the
//! ledger for the installation token (SR-50).

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use han_exec::actions::{GitPush, HttpRequest, OpenPr};
use han_exec::github::{GithubMinter, JwtSource};
use han_exec::{Action, ExecConfig, GithubApp, PlanError, RepoTarget, Report, Secret};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const CANARY: &str = "ghs_CANARYinstallationTOKEN0123456789";

#[derive(Default)]
struct Seen {
    requests: Vec<(String, String, String, String)>, // method, path, authorization, body
    existing_pr: bool,
}

/// A one-request-per-connection HTTP/1.1 server answering like GitHub.
async fn mock_github(seen: Arc<Mutex<Seen>>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let (head, mut body) = loop {
                    let n = s.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break (
                            String::from_utf8_lossy(&buf[..p]).to_string(),
                            buf[p + 4..].to_vec(),
                        );
                    }
                };
                let mut lines = head.split("\r\n");
                let first = lines.next().unwrap_or("").to_owned();
                let headers: Vec<(String, String)> = lines
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
                    .collect();
                let get = |k: &str| headers.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
                let len: usize = get("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                while body.len() < len {
                    let n = s.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    body.extend_from_slice(&tmp[..n]);
                }
                let mut parts = first.split(' ');
                let method = parts.next().unwrap_or("").to_owned();
                let path = parts.next().unwrap_or("").to_owned();
                let auth = get("authorization").unwrap_or_default();
                let body_s = String::from_utf8_lossy(&body).to_string();
                let (status, reply) = {
                    let mut st = seen.lock().unwrap();
                    st.requests
                        .push((method.clone(), path.clone(), auth, body_s));
                    if method == "POST" && path == "/app/installations/7/access_tokens" {
                        (
                            201,
                            json!({"token": CANARY, "expires_at": "2099-01-01T00:00:00Z"}),
                        )
                    } else if method == "GET" && path.starts_with("/repos/o/r/pulls") {
                        if st.existing_pr {
                            (
                                200,
                                json!([{"number": 5, "html_url": "https://example/pr/5"}]),
                            )
                        } else {
                            (200, json!([]))
                        }
                    } else if method == "POST" && path == "/repos/o/r/pulls" {
                        st.existing_pr = true;
                        (
                            201,
                            json!({"number": 6, "html_url": "https://example/pr/6"}),
                        )
                    } else if method == "GET" && path == "/app/installations/7" {
                        (
                            200,
                            json!({"id": 7, "permissions": {"contents": "write", "checks": "read", "metadata": "read"}}),
                        )
                    } else if method == "POST" && path == "/hook" {
                        (204, Value::Null)
                    } else {
                        (404, json!({"message": "Not Found"}))
                    }
                };
                let text = if reply.is_null() {
                    String::new()
                } else {
                    reply.to_string()
                };
                let resp = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

struct StaticJwt;

impl JwtSource for StaticJwt {
    fn jwt(&self, _: u64, _: u64) -> Result<Secret, String> {
        Ok(Secret::new("test-app-jwt"))
    }
}

fn github_rig(api: &str) -> (Rig, Arc<GithubMinter>) {
    let mut rig = Rig::new(vec![Arc::new(OpenPr), Arc::new(GitPush)]);
    rig.config.repos = vec![RepoTarget {
        github: Some("o/r".into()),
        ..RepoTarget::new("github:o/r", "https://github.invalid/o/r.git")
    }];
    let app = GithubApp {
        app_id: 1,
        installation_id: 7,
        api_base: Some(api.into()),
        private_key: String::new(),
    };
    rig.config.github = Some(app.clone());
    (
        rig,
        Arc::new(GithubMinter::new(&app, Box::new(StaticJwt)).unwrap()),
    )
}

fn token_bodies(seen: &Seen) -> Vec<Value> {
    seen.requests
        .iter()
        .filter(|r| r.1 == "/app/installations/7/access_tokens")
        .map(|r| serde_json::from_str(&r.3).unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_pr_mints_a_scoped_token_and_opens_once() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let api = mock_github(seen.clone()).await;
    let (rig, minter) = github_rig(&api);
    let plan = json!({"action": "open_pr", "target": "github:o/r",
        "args": {"head": "han/1-agent", "base": "main", "title": "Fix it", "body": "from HAN"}});
    let p = rig.node.propose(&plan);
    rig.node.approve(&p);
    let ex = rig.executor_with(Some(minter));
    let r = ex.step().await.unwrap();
    assert_eq!(
        r.reported,
        vec![(p.clone(), Report::new("succeeded", "yes"))]
    );

    let st = seen.lock().unwrap();
    // SR-67: one repository, pull requests only; the JWT only goes to the token endpoint.
    let bodies = token_bodies(&st);
    assert!(!bodies.is_empty());
    for b in &bodies {
        assert_eq!(
            b,
            &json!({"repositories": ["r"], "permissions": {"pull_requests": "write"}})
        );
    }
    for (m, path, auth, _) in &st.requests {
        if path.starts_with("/app/") {
            assert_eq!(auth, "Bearer test-app-jwt");
        } else {
            assert!(
                auth.contains(CANARY),
                "{m} {path} uses the installation token"
            );
        }
    }
    let creates = st
        .requests
        .iter()
        .filter(|r| r.0 == "POST" && r.1 == "/repos/o/r/pulls")
        .count();
    assert_eq!(creates, 1);
    let body: Value = serde_json::from_str(
        &st.requests
            .iter()
            .find(|r| r.0 == "POST" && r.1 == "/repos/o/r/pulls")
            .unwrap()
            .3,
    )
    .unwrap();
    assert_eq!(body["head"], "han/1-agent");
    drop(st);

    // SR-50: the installation token is nowhere in the executor's state.
    drop(ex);
    for entry in std::fs::read_dir(rig.dir.path()).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let bytes = std::fs::read(&path).unwrap();
            assert!(
                !bytes.windows(CANARY.len()).any(|w| w == CANARY.as_bytes()),
                "canary in {}",
                path.display()
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_installations_permissions_are_read_with_the_app_jwt_and_no_token_is_made() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let api = mock_github(seen.clone()).await;
    let (_rig, minter) = github_rig(&api);
    let perms = minter.installation_permissions().await.unwrap();
    assert_eq!(perms["contents"], "write");
    assert_eq!(perms["checks"], "read");
    assert!(!perms.contains_key("statuses"));
    let st = seen.lock().unwrap();
    assert_eq!(st.requests.len(), 1);
    assert_eq!(st.requests[0].1, "/app/installations/7");
    assert_eq!(st.requests[0].2, "Bearer test-app-jwt");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_existing_pr_for_the_head_branch_counts_as_done() {
    let seen = Arc::new(Mutex::new(Seen {
        existing_pr: true,
        ..Seen::default()
    }));
    let api = mock_github(seen.clone()).await;
    let (rig, minter) = github_rig(&api);
    let p = rig
        .node
        .propose(&json!({"action": "open_pr", "target": "github:o/r",
        "args": {"head": "han/1-agent", "base": "main", "title": "Fix it"}}));
    rig.node.approve(&p);
    let r = rig.executor_with(Some(minter)).step().await.unwrap();
    assert_eq!(r.reported, vec![(p, Report::new("succeeded", "yes"))]);
    let st = seen.lock().unwrap();
    assert!(
        !st.requests
            .iter()
            .any(|r| r.0 == "POST" && r.1 == "/repos/o/r/pulls")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_push_to_a_github_repo_uses_a_contents_token() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let api = mock_github(seen.clone()).await;
    let (rig, minter) = github_rig(&api);
    rig.vcs.0.lock().unwrap().digest = "d".repeat(64);
    let p = rig.node.propose(
        &json!({"action": "git_push", "target": "github:o/r", "args": {
        "branch": "han/1-agent", "base": "a".repeat(40), "from": null, "to": "b".repeat(40),
        "changeset": "d".repeat(64), "source": "/ws"}}),
    );
    rig.node.approve(&p);
    let r = rig.executor_with(Some(minter)).step().await.unwrap();
    assert_eq!(r.reported, vec![(p, Report::new("succeeded", "yes"))]);
    assert_eq!(rig.vcs.0.lock().unwrap().auth_seen, vec![CANARY.to_owned()]);
    let st = seen.lock().unwrap();
    assert_eq!(
        token_bodies(&st),
        vec![json!({"repositories": ["r"], "permissions": {"contents": "write"}})]
    );
}

fn http_config(port_host: &str, allow_private: bool) -> ExecConfig {
    ExecConfig {
        http_hosts: vec![port_host.into()],
        test_allow_private_http: allow_private,
        ..ExecConfig::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_request_runs_against_an_allowed_host() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let base = mock_github(seen.clone()).await;
    let mut rig = Rig::new(vec![Arc::new(HttpRequest)]);
    rig.config = http_config("127.0.0.1", true);
    let p = rig.node.propose(&json!({"action": "http_request", "target": "127.0.0.1",
        "args": {"method": "POST", "url": format!("{base}/hook"),
                 "headers": {"content-type": "application/json"}, "body": "{\"status\":\"green\"}"}}));
    rig.node.approve(&p);
    let r = rig.executor().step().await.unwrap();
    assert_eq!(r.reported, vec![(p, Report::new("succeeded", "yes"))]);
    let st = seen.lock().unwrap();
    assert_eq!(st.requests.len(), 1);
    assert_eq!(st.requests[0].3, "{\"status\":\"green\"}");
}

#[tokio::test]
async fn http_request_refuses_private_destinations_and_plain_http() {
    let base = mock_github(Arc::default()).await;
    let plan = json!({"action": "http_request", "target": "127.0.0.1",
        "args": {"method": "POST", "url": format!("{base}/hook")}});
    // Without the test override, plain http is a schema error.
    assert!(matches!(
        HttpRequest.plan_check(&plan, &http_config("127.0.0.1", false)),
        Err(PlanError::Schema(_))
    ));
    // An https URL to a host that resolves to loopback is refused before connecting.
    let mut rig = Rig::new(vec![Arc::new(HttpRequest)]);
    rig.config = http_config("localhost", false);
    let p = rig
        .node
        .propose(&json!({"action": "http_request", "target": "localhost",
        "args": {"method": "GET", "url": "https://localhost:1/x"}}));
    rig.node.approve(&p);
    let r = rig.executor().step().await.unwrap();
    assert_eq!(r.reported, vec![(p, Report::not_started())]);
    // Credential headers and hosts off the list are refused.
    let creds = json!({"action": "http_request", "target": "example.com",
        "args": {"method": "GET", "url": "https://example.com/", "headers": {"Authorization": "x"}}});
    assert!(
        HttpRequest
            .plan_check(&creds, &http_config("example.com", false))
            .is_err()
    );
    let off = json!({"action": "http_request", "target": "evil.example",
        "args": {"method": "GET", "url": "https://evil.example/"}});
    assert!(matches!(
        HttpRequest.plan_check(&off, &http_config("example.com", false)),
        Err(PlanError::OutOfScope(_))
    ));
}

#[test]
fn address_classes() {
    use han_exec::actions::is_public;
    for bad in [
        "127.0.0.1",
        "10.1.2.3",
        "172.16.0.1",
        "192.168.1.1",
        "169.254.169.254",
        "100.64.0.1",
        "100.127.255.255",
        "0.0.0.0",
        "224.0.0.1",
        "255.255.255.255",
        "::1",
        "::",
        "fc00::1",
        "fd12::1",
        "fe80::1",
        "ff02::1",
        "::ffff:127.0.0.1",
        "::ffff:10.0.0.1",
    ] {
        assert!(!is_public(bad.parse().unwrap()), "{bad} must be refused");
    }
    for good in ["1.1.1.1", "140.82.112.3", "100.128.0.1", "2606:4700::1111"] {
        assert!(is_public(good.parse().unwrap()), "{good} is public");
    }
}

#[test]
fn secrets_do_not_debug_print() {
    let s = Secret::new(CANARY);
    assert!(!format!("{s:?}").contains(CANARY));
    let a = han_vcs::RemoteAuth::new("x-access-token", CANARY);
    assert!(!format!("{a:?}").contains(CANARY));
}

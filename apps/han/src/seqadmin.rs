//! A home's admin socket: `han seq register` creates a profile space on a running home, with no restart.
//!
//! Who may ask a server to register a profile is out of scope of the protocol (section 4.3), so there is no
//! wire call for it. The home's operator does it locally instead: the socket is reachable by the OS user that
//! runs `han seq` and nobody else (a Unix socket of mode 0600, a named pipe whose access list names that
//! user), which is the whole authorisation. The protocol is one JSON line each way.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use han_seq::Home;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

/// The admin endpoint of the home whose data directory is `data`.
pub fn endpoint(data: &Path) -> String {
    #[cfg(windows)]
    {
        han_node::rpc::default_endpoint(&data.join("seq-admin"))
    }
    #[cfg(not(windows))]
    {
        data.join("seq-admin.sock").to_string_lossy().into_owned()
    }
}

/// What to register.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Register {
    /// The actor, `@name:server`.
    pub actor: String,
    /// `human`, `agent` or `service`.
    pub kind: String,
    /// Its first key's id.
    pub key_id: String,
    /// Its first key, base64url.
    pub public_key: String,
    /// The operator of an agent or a service.
    #[serde(default)]
    pub operator: Option<String>,
}

/// Registers a profile on `home`; the result is the seq of its first record, and whether the profile was
/// there already (registering again leaves it as it is).
pub fn register(home: &Home, r: &Register) -> Result<(u64, bool), String> {
    let (name, server) = r
        .actor
        .rsplit_once(':')
        .ok_or("the actor is @name:server")?;
    if server != home.server() {
        return Err(format!(
            "{} is not an actor of {}: register it at its own home",
            r.actor,
            home.server()
        ));
    }
    if !name.starts_with('@') {
        return Err("the actor is @name:server".into());
    }
    let already = home.state_of(&format!("profile:{}", r.actor)).is_some();
    home.register(
        &r.actor,
        &r.kind,
        &r.key_id,
        &r.public_key,
        r.operator.as_deref(),
    )
    .map(|receipt| (receipt.seq, already))
    .map_err(|e| e.to_string())
}

/// Serves the admin socket of `home` until the returned handle is dropped.
pub fn serve(home: Arc<Home>, endpoint: &str) -> std::io::Result<tokio::task::JoinHandle<()>> {
    use interprocess::local_socket::traits::tokio::Listener as _;
    let listener = han_node::rpc::listen_local(endpoint)?;
    Ok(tokio::spawn(async move {
        loop {
            let conn = match listener.accept().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("admin accept: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    continue;
                }
            };
            let home = Arc::clone(&home);
            tokio::spawn(async move {
                use interprocess::local_socket::traits::tokio::Stream as _;
                let (recv, mut send) = conn.split();
                let mut line = String::new();
                // A request is small; anything longer is not one.
                let mut reader = BufReader::new(tokio::io::AsyncReadExt::take(recv, 64 * 1024));
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                let request = serde_json::from_str::<Value>(&line).ok();
                if let Some(actor) = request
                    .as_ref()
                    .and_then(|v| v.get("profile"))
                    .and_then(Value::as_str)
                {
                    let state = home.state_of(&format!("profile:{actor}"));
                    let confirmed = state
                        .as_ref()
                        .and_then(|s| s.profile.as_ref())
                        .is_some_and(|p| p.operator_confirmed);
                    let reply =
                        json!({"ok": {"registered": state.is_some(), "confirmed": confirmed}});
                    let _ = send.write_all(format!("{reply}\n").as_bytes()).await;
                    let _ = send.flush().await;
                    return;
                }
                let reply = match request
                    .and_then(|v| v.get("register").cloned())
                    .and_then(|v| serde_json::from_value::<Register>(v).ok())
                {
                    Some(r) => match register(&home, &r) {
                        Ok((seq, already)) => {
                            json!({"ok": {"actor": r.actor, "seq": seq, "already": already}})
                        }
                        Err(e) => json!({"error": e}),
                    },
                    None => {
                        json!({"error": "want {\"register\": {actor, kind, keyId, publicKey, operator?}}"})
                    }
                };
                let _ = send.write_all(format!("{reply}\n").as_bytes()).await;
                let _ = send.flush().await;
            });
        }
    }))
}

/// Asks the running home at `data` whether `actor` has a profile and whether its operator confirmed it:
/// `(registered, confirmed)`.
pub async fn profile(data: &Path, actor: &str) -> Result<(bool, bool), Box<dyn std::error::Error>> {
    use interprocess::local_socket::traits::tokio::Stream as _;
    let conn = han_node::rpc::connect_local(&endpoint(data)).await?;
    let (recv, mut send) = conn.split();
    send.write_all(format!("{}\n", json!({ "profile": actor })).as_bytes())
        .await?;
    send.flush().await?;
    let mut line = String::new();
    BufReader::new(recv).read_line(&mut line).await?;
    let v: Value = serde_json::from_str(&line).map_err(|_| "the home did not answer")?;
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        return Err(e.to_owned().into());
    }
    Ok((
        v["ok"]["registered"].as_bool().unwrap_or(false),
        v["ok"]["confirmed"].as_bool().unwrap_or(false),
    ))
}

/// How long a client waits for the admin socket of a home that is starting (just restarted, say).
pub const CLIENT_PATIENCE: Duration = Duration::from_secs(5);

/// How long `han runner setup` waits for a restarted service to be ready.
#[cfg(target_os = "linux")]
pub const READY_TIMEOUT: Duration = Duration::from_secs(30);

const POLL: Duration = Duration::from_millis(100);

/// Whether a failed connect may be a service that is starting: no socket yet, or one nobody accepts on yet.
fn starting(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}

/// Connects to the admin socket at `endpoint`, retrying for up to `patience` while the home may be starting.
async fn connect_patiently(
    endpoint: &str,
    patience: Duration,
) -> std::io::Result<interprocess::local_socket::tokio::Stream> {
    let deadline = Instant::now() + patience;
    loop {
        match han_node::rpc::connect_local(endpoint).await {
            Err(e) if starting(&e) && Instant::now() < deadline => tokio::time::sleep(POLL).await,
            r => return r,
        }
    }
}

/// Polls `probe` until it succeeds, for at most `timeout`; on failure, says what `what` did not do and the
/// last error.
#[cfg(any(target_os = "linux", test))]
pub async fn wait_until<F, Fut, E>(
    what: &str,
    timeout: Duration,
    mut probe: F,
) -> Result<(), String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(), E>>,
    E: std::fmt::Display,
{
    let deadline = Instant::now() + timeout;
    loop {
        let last = match probe().await {
            Ok(()) => return Ok(()),
            Err(e) => e.to_string(),
        };
        if Instant::now() >= deadline {
            return Err(format!(
                "{what} was not ready within {timeout:?} (last: {last})"
            ));
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Waits until the home with data directory `data` accepts connections on its admin socket.
#[cfg(any(target_os = "linux", test))]
pub async fn wait_ready(data: &Path, timeout: Duration) -> Result<(), String> {
    let endpoint = endpoint(data);
    wait_until(
        &format!("han-seq: its admin socket {endpoint} did not appear, so it"),
        timeout,
        || async { han_node::rpc::connect_local(&endpoint).await.map(drop) },
    )
    .await
}

/// Asks the running home at `data` to register a profile.
pub async fn ask(data: &Path, r: &Register) -> Result<(u64, bool), Box<dyn std::error::Error>> {
    ask_within(data, r, CLIENT_PATIENCE).await
}

async fn ask_within(
    data: &Path,
    r: &Register,
    patience: Duration,
) -> Result<(u64, bool), Box<dyn std::error::Error>> {
    use interprocess::local_socket::traits::tokio::Stream as _;
    let endpoint = endpoint(data);
    // A home that was just restarted has no socket for a moment; a stale one never will, so do not wait for it.
    let patience = if crate::homecmd::stale_home(data).is_some() {
        Duration::ZERO
    } else {
        patience
    };
    let conn = connect_patiently(&endpoint, patience).await.map_err(|e| {
        let stale = crate::homecmd::stale_home(data)
            .map(|pid| {
                format!(
                    "; the home (pid {pid}) is running a binary that was replaced, so it has no admin socket: restart it (for example `systemctl restart han-seq`)"
                )
            })
            .unwrap_or_default();
        let waited = if patience.is_zero() {
            String::new()
        } else {
            format!(" waited {patience:?} for it")
        };
        format!(
            "{endpoint}: {e} (is `han seq` running with --data {}?{waited}){stale}",
            data.display()
        )
    })?;
    let (recv, mut send) = conn.split();
    send.write_all(format!("{}\n", json!({ "register": r })).as_bytes())
        .await?;
    send.flush().await?;
    let mut line = String::new();
    BufReader::new(recv).read_line(&mut line).await?;
    let v: Value = serde_json::from_str(&line).map_err(|_| "the home did not answer")?;
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        return Err(e.to_owned().into());
    }
    let seq = v["ok"]["seq"]
        .as_u64()
        .ok_or("the home's answer has no seq")?;
    Ok((seq, v["ok"]["already"].as_bool().unwrap_or(false)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> Register {
        Register {
            actor: "@a:h".into(),
            kind: "agent".into(),
            key_id: "k".into(),
            public_key: "p".into(),
            operator: None,
        }
    }

    /// Answers one request like a home would, after `delay`.
    fn serve_after(data: &Path, delay: Duration) -> tokio::task::JoinHandle<()> {
        use interprocess::local_socket::traits::tokio::{Listener as _, Stream as _};
        let endpoint = endpoint(data);
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let listener = han_node::rpc::listen_local(&endpoint).unwrap();
            let conn = listener.accept().await.unwrap();
            let (recv, mut send) = conn.split();
            let mut line = String::new();
            BufReader::new(recv).read_line(&mut line).await.unwrap();
            send.write_all(
                b"{\"ok\":{\"seq\":7,\"already\":false}}
",
            )
            .await
            .unwrap();
            send.flush().await.unwrap();
        })
    }

    #[tokio::test]
    async fn the_client_waits_for_a_home_that_is_starting() {
        let dir = tempfile::tempdir().unwrap();
        let server = serve_after(dir.path(), Duration::from_millis(600));
        let got = ask_within(dir.path(), &reg(), Duration::from_secs(10)).await;
        assert_eq!(got.unwrap(), (7, false));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn the_client_gives_up_with_a_clear_message() {
        let dir = tempfile::tempdir().unwrap();
        let err = ask_within(dir.path(), &reg(), Duration::from_millis(300))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("is `han seq` running"), "{err}");
        assert!(err.contains("waited 300ms"), "{err}");
    }

    #[tokio::test]
    async fn the_waiter_succeeds_once_the_socket_appears() {
        let dir = tempfile::tempdir().unwrap();
        let _server = serve_after(dir.path(), Duration::from_millis(600));
        wait_ready(dir.path(), Duration::from_secs(10))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn the_waiter_fails_naming_the_unit_when_the_socket_never_appears() {
        let dir = tempfile::tempdir().unwrap();
        let err = wait_ready(dir.path(), Duration::from_millis(300))
            .await
            .unwrap_err();
        assert!(err.contains("han-seq"), "{err}");
        assert!(err.contains("admin socket"), "{err}");
        assert!(err.contains("was not ready within"), "{err}");
    }
}

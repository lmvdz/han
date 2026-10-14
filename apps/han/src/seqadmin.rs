//! A home's admin socket: `han seq register` creates a profile space on a running home, with no restart.
//!
//! Who may ask a server to register a profile is out of scope of the protocol (section 4.3), so there is no
//! wire call for it. The home's operator does it locally instead: the socket is reachable by the OS user that
//! runs `han seq` and nobody else (a Unix socket of mode 0600, a named pipe whose access list names that
//! user), which is the whole authorisation. The protocol is one JSON line each way.

use std::path::Path;
use std::sync::Arc;

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

/// Asks the running home at `data` to register a profile.
pub async fn ask(data: &Path, r: &Register) -> Result<(u64, bool), Box<dyn std::error::Error>> {
    use interprocess::local_socket::traits::tokio::Stream as _;
    let endpoint = endpoint(data);
    let conn = han_node::rpc::connect_local(&endpoint).await.map_err(|e| {
        let stale = crate::homecmd::stale_home(data)
            .map(|pid| {
                format!(
                    "; the home (pid {pid}) is running a binary that was replaced, so it has no admin socket: restart it (for example `systemctl restart han-seq`)"
                )
            })
            .unwrap_or_default();
        format!(
            "{endpoint}: {e} (is `han seq` running with --data {}?){stale}",
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

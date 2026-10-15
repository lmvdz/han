//! The JSON-RPC server: a [`NodeApi`] on a named pipe (Windows, current user only) or a Unix socket (mode 0600).

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use interprocess::local_socket::ListenerOptions;
use interprocess::local_socket::tokio::{Stream, prelude::*};
use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};

use super::{ErrorObject, Message, code, local_name, read_line};
use crate::api::{Command, ConnectionClass, NodeApi, NodeError, Query};

/// A listening node API. Dropping it stops accepting, closes the connections that are open and ends their
/// views.
pub struct RpcServer {
    task: JoinHandle<()>,
    endpoint: String,
}

impl RpcServer {
    /// The endpoint it listens on.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl Drop for RpcServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Serves `api` at `endpoint` (see [`super::default_endpoint`]) to faces. On Windows the pipe's access list
/// names the current user and nobody else; on Unix the socket file is mode 0600.
pub fn serve(api: Arc<dyn NodeApi>, endpoint: &str) -> io::Result<RpcServer> {
    serve_as(api, endpoint, ConnectionClass::Face)
}

/// Like [`serve`], for connections of `class`: a command above the class is refused as `denied` before it
/// reaches the node. A host or an executor gets its own endpoint of the class it is, so it cannot send a
/// face's command (an approval) even if it is compromised.
pub fn serve_as(
    api: Arc<dyn NodeApi>,
    endpoint: &str,
    class: ConnectionClass,
) -> io::Result<RpcServer> {
    let options = ListenerOptions::new().name(local_name(endpoint)?);
    let options = restrict(options)?;
    let listener = options.create_tokio()?;
    let task = tokio::spawn(async move {
        // Aborting this task drops the set, and the set aborts every connection in it.
        let mut conns = JoinSet::new();
        loop {
            match listener.accept().await {
                Ok(conn) => {
                    while conns.try_join_next().is_some() {}
                    conns.spawn(connection(conn, Arc::clone(&api), class));
                }
                Err(e) => {
                    tracing::warn!("rpc accept: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            }
        }
    });
    Ok(RpcServer {
        task,
        endpoint: endpoint.to_owned(),
    })
}

/// The three endpoints of a node: one per [`ConnectionClass`], each its own owner-only pipe or socket with
/// its own name ([`super::role_endpoint`]). Faces get the face endpoint; a spawned host is given the host one
/// and an executor the executor one, and neither is told the others (SR-107).
pub struct NodeServers {
    face: RpcServer,
    host: RpcServer,
    executor: RpcServer,
}

impl NodeServers {
    /// The face endpoint.
    pub fn endpoint(&self) -> &str {
        self.face.endpoint()
    }

    /// The endpoint of `class`.
    pub fn endpoint_of(&self, class: ConnectionClass) -> &str {
        match class {
            ConnectionClass::Face => self.face.endpoint(),
            ConnectionClass::Host => self.host.endpoint(),
            ConnectionClass::Executor => self.executor.endpoint(),
        }
    }
}

/// Serves `api` on the face endpoint `endpoint` and on a host and an executor endpoint beside it.
pub fn serve_roles(api: Arc<dyn NodeApi>, endpoint: &str) -> io::Result<NodeServers> {
    let on = |class| {
        serve_as(
            Arc::clone(&api),
            &super::role_endpoint(endpoint, class),
            class,
        )
    };
    Ok(NodeServers {
        face: on(ConnectionClass::Face)?,
        host: on(ConnectionClass::Host)?,
        executor: on(ConnectionClass::Executor)?,
    })
}

/// A listener on `endpoint` that only the current user can reach (same restriction as [`serve`]), for local
/// tools with their own protocol, such as a home's admin socket.
pub fn listen_local(endpoint: &str) -> io::Result<interprocess::local_socket::tokio::Listener> {
    let options = ListenerOptions::new().name(local_name(endpoint)?);
    restrict(options)?.create_tokio()
}

#[cfg(windows)]
fn restrict(options: ListenerOptions<'_>) -> io::Result<ListenerOptions<'_>> {
    use interprocess::os::windows::local_socket::ListenerOptionsExt as _;
    use interprocess::os::windows::security_descriptor::SecurityDescriptor;
    let sid = current_user_sid()?;
    // Protected DACL (no inherited entries) with one allow entry: generic all for the current user.
    let sddl = widestring::U16CString::from_str(format!("D:P(A;;GA;;;{sid})"))
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(options.security_descriptor(SecurityDescriptor::deserialize(&sddl)?))
}

#[cfg(unix)]
fn restrict(options: ListenerOptions<'_>) -> io::Result<ListenerOptions<'_>> {
    use interprocess::os::unix::local_socket::ListenerOptionsExt as _;
    // A socket left by a daemon that died is replaced; the file is readable and writable by its owner only.
    Ok(options.mode(0o600).try_overwrite(true))
}

/// The SID of the user running this process, from `whoami /user`.
#[cfg(windows)]
fn current_user_sid() -> io::Result<String> {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let whoami = std::path::Path::new(&root)
        .join("System32")
        .join("whoami.exe");
    let out = std::process::Command::new(whoami)
        .args(["/user", "/fo", "csv", "/nh"])
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    // "DOMAIN\user","S-1-5-21-..."
    text.split('"')
        .map(str::trim)
        .find(|s| {
            s.starts_with("S-1-") && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        .map(str::to_owned)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "cannot tell the current user's SID; refusing to open an unrestricted pipe",
            )
        })
}

/// A task that ends when its owner lets go of it, so a connection that ends takes its work with it.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

type Views = Arc<Mutex<HashMap<u64, AbortOnDrop>>>;

fn respond(id: &Value, result: Result<Value, ErrorObject>) -> String {
    let msg = match result {
        Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}),
        Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
    };
    msg.to_string()
}

fn protocol_error(code: i64, message: &str) -> ErrorObject {
    ErrorObject {
        code,
        message: message.to_owned(),
        data: None,
    }
}

/// The lock of one face connection. It belongs to the connection: another connection to the same pipe starts
/// locked and needs its own check.
#[derive(Default)]
struct Lock {
    /// When the person's unlock ends, by the monotonic clock and by the wall clock; the unlock holds only
    /// while both say it has not ended, so neither a suspend nor a clock change can stretch it.
    until: Mutex<Option<(Instant, SystemTime)>>,
    /// An unlock waits for the person.
    pending: AtomicBool,
    /// Spacing after refused unlocks on this connection.
    backoff: Mutex<crate::presence::Backoff>,
}

impl Lock {
    /// How long the unlock still holds, if it does.
    fn remaining(&self) -> Option<Duration> {
        let (mono, wall) = (*self.until.lock().unwrap_or_else(|e| e.into_inner()))?;
        let by_mono = mono.checked_duration_since(Instant::now())?;
        let by_wall = wall.duration_since(SystemTime::now()).ok()?;
        Some(by_mono.min(by_wall)).filter(|d| !d.is_zero())
    }
}

/// Clears `pending` when the unlock ends, however it ends (a closed connection aborts it).
struct Pending<'a>(&'a AtomicBool);

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Whether `cmd` may go to the node now: a face connection sends a command that governs or steers only while
/// it is unlocked and only if the node requires a person for it.
fn gate(api: &dyn NodeApi, lock: &Lock, cmd: &Command) -> Result<(), NodeError> {
    if !cmd.needs_presence() || !api.presence_required()? {
        return Ok(());
    }
    if lock.remaining().is_some() {
        return Ok(());
    }
    Err(NodeError::presence_required(
        "this connection has not confirmed a person on the device, or the confirmation ran out",
    ))
}

/// What the prompt may say about the peer: its process name (where the system tells) and pid.
fn describe_peer(conn: &Stream) -> Option<String> {
    let pid = conn.peer_creds().ok()?.pid()?;
    #[cfg(target_os = "linux")]
    if let Ok(name) = std::fs::read_to_string(format!("/proc/{pid}/comm")) {
        return Some(format!("{} (pid {pid})", name.trim()));
    }
    Some(format!("process {pid}"))
}

/// One `unlock` request on a face connection.
async fn unlock(
    api: Arc<dyn NodeApi>,
    lock: Arc<Lock>,
    client: Option<String>,
) -> Result<Value, ErrorObject> {
    let reply = |window: Duration| Ok(json!({ "window_ms": window.as_millis() as u64 }));
    // already unlocked: nothing to ask
    if let Some(left) = lock.remaining() {
        return reply(left);
    }
    if lock.pending.swap(true, Ordering::SeqCst) {
        let e = NodeError::conflict("an unlock already waits for a person on this connection");
        return Err(ErrorObject::node(&e));
    }
    let _pending = Pending(&lock.pending);
    let wait = lock
        .backoff
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .wait();
    if let Some(wait) = wait {
        return Err(ErrorObject::node(&crate::presence::spaced_out(wait)));
    }
    match api.confirm_unlock(client).await {
        Ok(window) => {
            lock.backoff
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .allowed();
            *lock.until.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((Instant::now() + window, SystemTime::now() + window));
            reply(window)
        }
        Err(e) => {
            if matches!(e, NodeError::Denied { .. }) {
                lock.backoff
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .refused();
            }
            Err(ErrorObject::node(&e))
        }
    }
}

async fn connection(conn: Stream, api: Arc<dyn NodeApi>, class: ConnectionClass) {
    let client = describe_peer(&conn);
    let (recv, mut send) = conn.split();
    let (out, mut lines) = mpsc::unbounded_channel::<String>();
    // Every task of the connection is owned by it: when the connection is aborted (the server dropped) or
    // ends, these drop and abort, the writer's half of the pipe closes, and the client sees the end.
    let writer = AbortOnDrop(tokio::spawn(async move {
        while let Some(line) = lines.recv().await {
            if send.write_all(line.as_bytes()).await.is_err()
                || send.write_all(b"\n").await.is_err()
                || send.flush().await.is_err()
            {
                break;
            }
        }
    }));
    let mut calls = JoinSet::new();
    let views: Views = Arc::default();
    let lock: Arc<Lock> = Arc::default();
    let mut reader = BufReader::new(recv);
    let mut buf = Vec::new();
    loop {
        match read_line(&mut reader, &mut buf).await {
            Ok(Some(())) => {}
            Ok(None) => break,
            Err(_) => {
                let e = protocol_error(code::INVALID_REQUEST, "message too long");
                let _ = out.send(respond(&Value::Null, Err(e)));
                break;
            }
        }
        let msg: Message = match serde_json::from_slice(&buf) {
            Ok(m) => m,
            Err(_) => {
                let e = protocol_error(code::PARSE, "parse error");
                let _ = out.send(respond(&Value::Null, Err(e)));
                continue;
            }
        };
        let id = msg.id.clone().unwrap_or(Value::Null);
        let params = msg.params.unwrap_or(Value::Null);
        match msg.method.as_deref() {
            Some("call") => match serde_json::from_value::<Command>(params) {
                Ok(cmd) if !class.permits(cmd.class()) => {
                    let e = NodeError::denied(format!(
                        "a {class:?} connection may not send a {:?} command",
                        cmd.class()
                    ));
                    let _ = out.send(respond(&id, Err(ErrorObject::node(&e))));
                }
                Ok(cmd) => {
                    if class == ConnectionClass::Face
                        && let Err(e) = gate(&*api, &lock, &cmd)
                    {
                        let _ = out.send(respond(&id, Err(ErrorObject::node(&e))));
                        continue;
                    }
                    let (api, out) = (Arc::clone(&api), out.clone());
                    while calls.try_join_next().is_some() {}
                    calls.spawn(async move {
                        let result = match api.call(cmd).await {
                            Ok(r) => serde_json::to_value(r)
                                .map_err(|e| ErrorObject::node(&NodeError::invalid(e.to_string()))),
                            Err(e) => Err(ErrorObject::node(&e)),
                        };
                        let _ = out.send(respond(&id, result));
                    });
                }
                Err(e) => {
                    let data = serde_json::to_value(NodeError::invalid(e.to_string())).ok();
                    let err = ErrorObject {
                        code: code::INVALID_PARAMS,
                        message: format!("invalid params: {e}"),
                        data,
                    };
                    let _ = out.send(respond(&id, Err(err)));
                }
            },
            Some("unlock") if class != ConnectionClass::Face => {
                let e = NodeError::denied(format!("a {class:?} connection cannot unlock"));
                let _ = out.send(respond(&id, Err(ErrorObject::node(&e))));
            }
            Some("unlock") => {
                let (api, out, lock, client) = (
                    Arc::clone(&api),
                    out.clone(),
                    Arc::clone(&lock),
                    client.clone(),
                );
                calls.spawn(async move {
                    let _ = out.send(respond(&id, unlock(api, lock, client).await));
                });
            }
            Some("view") => match (id.as_u64(), serde_json::from_value::<Query>(params)) {
                (Some(sub), Ok(query)) => {
                    let _ = out.send(respond(&id, Ok(json!({ "sub": sub }))));
                    let mut stream = api.view(query);
                    let out = out.clone();
                    let task = AbortOnDrop(tokio::spawn(async move {
                        while let Some(event) = stream.next().await {
                            let note = json!({
                                "jsonrpc": "2.0",
                                "method": "view.event",
                                "params": {"sub": sub, "event": event},
                            });
                            if out.send(note.to_string()).is_err() {
                                break;
                            }
                        }
                    }));
                    views
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(sub, task);
                }
                (None, _) => {
                    let e = protocol_error(code::INVALID_REQUEST, "a view needs a numeric id");
                    let _ = out.send(respond(&id, Err(e)));
                }
                (_, Err(e)) => {
                    let data = serde_json::to_value(NodeError::invalid(e.to_string())).ok();
                    let err = ErrorObject {
                        code: code::INVALID_PARAMS,
                        message: format!("invalid params: {e}"),
                        data,
                    };
                    let _ = out.send(respond(&id, Err(err)));
                }
            },
            Some("unsubscribe") => {
                if let Some(sub) = params["sub"].as_u64() {
                    // Dropping the task aborts it.
                    views.lock().unwrap_or_else(|e| e.into_inner()).remove(&sub);
                }
            }
            Some(_) => {
                let e = protocol_error(code::METHOD_NOT_FOUND, "method not found");
                let _ = out.send(respond(&id, Err(e)));
            }
            None => {} // a response or a malformed message: nothing to do
        }
    }
    views.lock().unwrap_or_else(|e| e.into_inner()).clear();
    calls.abort_all();
    // Let what was already answered reach the client, then end.
    drop(out);
    let mut writer = writer;
    let _ = (&mut writer.0).await;
}

//! Local JSON-RPC (PLAN section 3): faces that are separate processes attach to the daemon over a named pipe
//! on Windows (restricted to the current user) or a Unix socket elsewhere.
//!
//! The framing is JSON-RPC 2.0, one message per line (newline-delimited JSON, at most [`MAX_LINE`] bytes):
//!
//! * `call`: params are a [`Command`](crate::api::Command); the result is a
//!   [`CallResult`](crate::api::CallResult), or an error whose `data` is a [`NodeError`](crate::api::NodeError).
//! * `view`: params are a [`Query`](crate::api::Query); the result is `{"sub": <id>}`, then notifications
//!   `view.event` with `{"sub", "event"}` carrying [`ViewEvent`](crate::api::ViewEvent)s until `unsubscribe`.
//!
//! [`RpcClient`] implements [`NodeApi`](crate::api::NodeApi), so a face holds the same trait whether the node
//! runs in its process or another.

mod client;
#[cfg(feature = "runtime")]
mod server;

use std::io;

use interprocess::local_socket::Name;
use interprocess::local_socket::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::NodeError;

pub use client::RpcClient;
#[cfg(feature = "runtime")]
pub use server::{RpcServer, listen_local, serve};

/// The longest message, in bytes, either side accepts.
pub const MAX_LINE: usize = 4 * 1024 * 1024;

/// JSON-RPC error codes.
#[cfg_attr(not(feature = "runtime"), allow(dead_code))]
mod code {
    pub const PARSE: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    /// A `NodeError`, in `data`.
    pub const NODE: i64 = -32000;
}

/// A request, notification or response.
#[derive(Serialize, Deserialize)]
pub(crate) struct Message {
    #[serde(default)]
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorObject>,
}

/// The `error` member of a response.
#[derive(Serialize, Deserialize)]
pub(crate) struct ErrorObject {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl ErrorObject {
    #[cfg_attr(not(feature = "runtime"), allow(dead_code))]
    pub(crate) fn node(e: &NodeError) -> ErrorObject {
        ErrorObject {
            code: code::NODE,
            message: e.to_string(),
            data: serde_json::to_value(e).ok(),
        }
    }

    pub(crate) fn into_node(self) -> NodeError {
        self.data
            .and_then(|d| serde_json::from_value(d).ok())
            .unwrap_or_else(|| NodeError::unavailable(format!("rpc: {}", self.message)))
    }
}

/// The local socket name for `endpoint`: a pipe name on Windows (it lives under `\\.\pipe\`), a filesystem
/// path elsewhere.
pub(crate) fn local_name(endpoint: &str) -> io::Result<Name<'_>> {
    #[cfg(windows)]
    {
        endpoint.to_ns_name::<interprocess::local_socket::GenericNamespaced>()
    }
    #[cfg(not(windows))]
    {
        endpoint.to_fs_name::<interprocess::local_socket::GenericFilePath>()
    }
}

/// Connects to a local endpoint, for tools with their own protocol (see `listen_local`).
pub async fn connect_local(
    endpoint: &str,
) -> io::Result<interprocess::local_socket::tokio::Stream> {
    use interprocess::local_socket::tokio::{Stream, prelude::*};
    Stream::connect(local_name(endpoint)?).await
}

/// The endpoint a daemon with data directory `data_dir` listens on: `<data_dir>/node.sock` on Unix; on
/// Windows a pipe name made from the directory, so two daemons with different data never collide.
pub fn default_endpoint(data_dir: &std::path::Path) -> String {
    #[cfg(windows)]
    {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut h = DefaultHasher::new();
        data_dir.hash(&mut h);
        format!("han-node-{:016x}", h.finish())
    }
    #[cfg(not(windows))]
    {
        data_dir.join("node.sock").to_string_lossy().into_owned()
    }
}

/// Reads one newline-terminated message of at most [`MAX_LINE`] bytes; `None` at end of input.
pub(crate) async fn read_line<R>(r: &mut R, buf: &mut Vec<u8>) -> io::Result<Option<()>>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::AsyncBufReadExt as _;
    use tokio::io::AsyncReadExt as _;
    buf.clear();
    let n = r.take(MAX_LINE as u64 + 1).read_until(b'\n', buf).await?;
    if n == 0 {
        return Ok(None);
    }
    if buf.len() > MAX_LINE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too long",
        ));
    }
    Ok(Some(()))
}

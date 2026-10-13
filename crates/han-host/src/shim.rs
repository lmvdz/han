//! The in-sandbox side of egress (PLAN §6.4; spike `research/m4-spike.md` Q2).
//!
//! * `listen`: a loopback listener inside the sandbox that forwards each connection to the session's proxy
//!   through the one channel the sandbox may use: a named pipe (Windows, MXC) or a bind-mounted unix socket
//!   (Linux, bubblewrap). It starts the agent command once the listener is up, with stdio inherited, and exits
//!   with the agent's code.
//! * `mcp-stub`: an MCP stdio server stand-in for harnesses that launch MCP servers themselves: it opens the
//!   proxy's MCP tunnel (`CONNECT han-mcp.invalid:1` with the session token) and copies stdin to the tunnel
//!   and the tunnel to stdout, so han-mcp itself runs outside the sandbox (SR-29).
//!
//! Usage (the `han-agent-shim` binary and `han shim` call [`main`]):
//!
//! ```text
//! han-agent-shim listen --port P (--pipe NAME | --unix PATH | --tcp ADDR) -- program args…
//! han-agent-shim mcp-stub --proxy 127.0.0.1:P --token T
//! ```

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;

use base64::Engine as _;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Where the shim forwards loopback connections.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShimTarget {
    /// A named pipe (Windows): the name after `\\.\pipe\`.
    Pipe(String),
    /// A unix socket.
    Unix(PathBuf),
    /// A TCP address (no sandbox, and tests).
    Tcp(SocketAddr),
}

/// Copies `client` to `upstream` and back. The relay ends when the upstream side ends; the client ending
/// its writes only half-closes upstream (when `half_close` holds, otherwise it waits). Named pipes have no
/// half-close, so a pipe is never shut down early: it ends when its reader sees the upstream end.
pub async fn relay<C, U>(client: C, upstream: U, half_close: bool) -> io::Result<(u64, u64)>
where
    C: AsyncRead + AsyncWrite + Send,
    U: AsyncRead + AsyncWrite + Send,
{
    let (mut cr, mut cw) = tokio::io::split(client);
    let (mut ur, mut uw) = tokio::io::split(upstream);
    let up = async {
        let n = tokio::io::copy(&mut cr, &mut uw).await.unwrap_or(0);
        if half_close {
            let _ = uw.shutdown().await;
        } else {
            let _ = uw.flush().await;
        }
        n
    };
    let down = async {
        let n = tokio::io::copy(&mut ur, &mut cw).await.unwrap_or(0);
        let _ = cw.shutdown().await;
        n
    };
    tokio::pin!(up, down);
    let mut sent = None;
    loop {
        tokio::select! {
            n = &mut up, if sent.is_none() => sent = Some(n),
            n = &mut down => return Ok((sent.unwrap_or(0), n)),
        }
    }
}

/// Opens one connection to the target.
pub async fn open_target(target: &ShimTarget) -> io::Result<Box<dyn Duplex>> {
    match target {
        ShimTarget::Tcp(a) => Ok(Box::new(TcpStream::connect(a).await?)),
        #[cfg(unix)]
        ShimTarget::Unix(p) => Ok(Box::new(tokio::net::UnixStream::connect(p).await?)),
        #[cfg(not(unix))]
        ShimTarget::Unix(_) => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "unix sockets are not available here",
        )),
        #[cfg(windows)]
        ShimTarget::Pipe(name) => {
            use interprocess::local_socket::tokio::Stream;
            use interprocess::local_socket::traits::tokio::Stream as _;
            use interprocess::local_socket::{GenericNamespaced, ToNsName as _};
            let name = name
                .as_str()
                .to_ns_name::<GenericNamespaced>()
                .map_err(io::Error::other)?;
            // Every instance may be busy for a moment; retry briefly.
            let mut last = None;
            for _ in 0..50 {
                match Stream::connect(name.clone()).await {
                    Ok(s) => return Ok(Box::new(s)),
                    Err(e) => last = Some(e),
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(last.unwrap_or_else(|| io::ErrorKind::NotFound.into()))
        }
        #[cfg(not(windows))]
        ShimTarget::Pipe(_) => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "named pipes are not available here",
        )),
    }
}

/// A byte stream in both directions.
pub trait Duplex: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Duplex for T {}

/// Forwards every connection accepted on `listener` to `target`.
pub async fn forward(listener: TcpListener, target: ShimTarget) {
    let half_close = !matches!(target, ShimTarget::Pipe(_));
    loop {
        let Ok((sock, _)) = listener.accept().await else {
            continue;
        };
        let target = target.clone();
        tokio::spawn(async move {
            if let Ok(up) = open_target(&target).await {
                let _ = relay(sock, up, half_close).await;
            }
        });
    }
}

/// Opens the proxy's MCP tunnel and returns the stream after the proxy's `200`.
pub async fn open_mcp_tunnel(proxy: SocketAddr, token: &str) -> io::Result<TcpStream> {
    let mut s = TcpStream::connect(proxy).await?;
    let auth = base64::engine::general_purpose::STANDARD
        .encode(format!("{}:{token}", crate::proxy::PROXY_USER));
    s.write_all(
        format!(
            "CONNECT {h}:{p} HTTP/1.1\r\nHost: {h}:{p}\r\nProxy-Authorization: Basic {auth}\r\n\r\n",
            h = crate::proxy::MCP_HOST,
            p = crate::proxy::MCP_PORT
        )
        .as_bytes(),
    )
    .await?;
    // Read the response head one byte at a time, so nothing after it is consumed.
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if s.read(&mut b).await? == 0 || head.len() > 8192 {
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        head.push(b[0]);
    }
    if !head.starts_with(b"HTTP/1.1 200") && !head.starts_with(b"HTTP/1.0 200") {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the proxy refused the MCP tunnel",
        ));
    }
    Ok(s)
}

async fn mcp_stub(proxy: SocketAddr, token: &str) -> io::Result<()> {
    let s = open_mcp_tunnel(proxy, token).await?;
    let (mut r, mut w) = s.into_split();
    let up = tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let _ = tokio::io::copy(&mut stdin, &mut w).await;
        let _ = w.shutdown().await;
    });
    let mut stdout = tokio::io::stdout();
    let _ = tokio::io::copy(&mut r, &mut stdout).await;
    let _ = stdout.flush().await;
    up.abort();
    Ok(())
}

fn usage() -> i32 {
    eprintln!(
        "usage: han-agent-shim listen --port P (--pipe NAME | --unix PATH | --tcp ADDR) -- program args...\n       han-agent-shim mcp-stub --proxy 127.0.0.1:P --token T"
    );
    2
}

/// The shim's entry point: `args` without the program name. Returns the process exit code.
pub fn main(args: Vec<String>) -> i32 {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("han-agent-shim: {e}");
            return 1;
        }
    };
    rt.block_on(run(args))
}

async fn run(args: Vec<String>) -> i32 {
    let Some(cmd) = args.first() else {
        return usage();
    };
    let (opts, rest) = match args.iter().position(|a| a == "--") {
        Some(i) => (&args[1..i], &args[i + 1..]),
        None => (&args[1..], &args[args.len()..]),
    };
    let opt = |name: &str| {
        opts.iter()
            .position(|a| a == name)
            .and_then(|i| opts.get(i + 1))
            .cloned()
    };
    match cmd.as_str() {
        "listen" => {
            let Some(port) = opt("--port").and_then(|p| p.parse::<u16>().ok()) else {
                return usage();
            };
            let target = if let Some(p) = opt("--pipe") {
                ShimTarget::Pipe(p)
            } else if let Some(p) = opt("--unix") {
                ShimTarget::Unix(p.into())
            } else if let Some(a) = opt("--tcp").and_then(|a| a.parse().ok()) {
                ShimTarget::Tcp(a)
            } else {
                return usage();
            };
            let listener = match TcpListener::bind(("127.0.0.1", port)).await {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("han-agent-shim: listen on 127.0.0.1:{port}: {e}");
                    return 1;
                }
            };
            tokio::spawn(forward(listener, target));
            let Some((program, pargs)) = rest.split_first() else {
                // No command: forward until killed.
                std::future::pending::<()>().await;
                return 0;
            };
            let status = tokio::process::Command::new(program)
                .args(pargs)
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .status()
                .await;
            match status {
                Ok(s) => s.code().unwrap_or(1),
                Err(e) => {
                    eprintln!("han-agent-shim: start {program}: {e}");
                    127
                }
            }
        }
        "mcp-stub" => {
            let (Some(proxy), Some(token)) = (
                opt("--proxy").and_then(|a| a.parse::<SocketAddr>().ok()),
                opt("--token"),
            ) else {
                return usage();
            };
            match mcp_stub(proxy, &token).await {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("han-agent-shim: mcp tunnel: {e}");
                    1
                }
            }
        }
        _ => usage(),
    }
}

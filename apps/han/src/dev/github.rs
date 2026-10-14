//! A tiny fake GitHub for `han dev up`: just enough of the REST API for `han-github` (installation tokens and
//! the issue list with ETags). The issues come from a JSON file that is read on every request, so editing the
//! file changes what the next poll sees. It never needs the network and holds no real credentials.

use std::net::SocketAddr;
use std::path::PathBuf;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

/// Serves until the process is stopped.
pub async fn serve(addr: SocketAddr, issues: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(addr).await?;
    println!("listening http://{}", listener.local_addr()?);
    loop {
        let (stream, _) = listener.accept().await?;
        let issues = issues.clone();
        tokio::spawn(async move {
            let _ = handle(stream, &issues).await;
        });
    }
}

async fn handle(mut s: TcpStream, issues: &std::path::Path) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = s.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > 64 * 1024 {
            return Ok(());
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.lines();
    let request = lines.next().unwrap_or_default();
    let (method, path) = {
        let mut p = request.split_whitespace();
        (
            p.next().unwrap_or_default().to_owned(),
            p.next().unwrap_or_default().to_owned(),
        )
    };
    let mut length = 0usize;
    let mut none_match = None;
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "content-length" => length = v.trim().parse().unwrap_or(0),
                "if-none-match" => none_match = Some(v.trim().to_owned()),
                _ => {}
            }
        }
    }
    // read (and drop) a request body
    let mut have = buf.len() - head_end;
    while have < length {
        let n = s.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        have += n;
    }
    let route = path.split('?').next().unwrap_or_default();
    let (status, extra, body) = if method == "POST" && route.ends_with("/access_tokens") {
        (
            "201 Created",
            String::new(),
            r#"{"token":"ghs_fake_dev_token","expires_at":"2099-01-01T00:00:00Z"}"#.to_owned(),
        )
    } else if method == "GET" && route.starts_with("/repos/") && route.ends_with("/issues") {
        let body = std::fs::read_to_string(issues).unwrap_or_else(|_| "[]".into());
        let etag = format!("\"{}\"", hex(&Sha256::digest(body.as_bytes())[..8]));
        if none_match.as_deref() == Some(etag.as_str()) {
            (
                "304 Not Modified",
                format!("ETag: {etag}\r\n"),
                String::new(),
            )
        } else {
            ("200 OK", format!("ETag: {etag}\r\n"), body)
        }
    } else {
        ("404 Not Found", String::new(), "{}".to_owned())
    };
    println!("{method} {path} -> {status}");
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(response.as_bytes()).await?;
    s.shutdown().await
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

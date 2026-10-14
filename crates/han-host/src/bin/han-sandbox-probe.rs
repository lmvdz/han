//! The sandbox probe (SR-23…SR-33, research/security-requirements.md "probe"): run inside a sandbox, it tries
//! each operation of the spec in `HAN_PROBE_SPEC` (JSON) and prints one JSON line per attempt:
//! `{"op": "...", "ok": true|false, "detail": "..."}`. With `"child": true` it also runs itself as a child
//! and reports the child's attempts with a `child:` prefix, to show the tree is confined like its root.
//!
//! The test harness decides which attempts must fail; the probe only reports.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs, UdpSocket};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Value, json};

fn emit(prefix: &str, op: String, r: Result<String, String>) {
    let (ok, detail) = match r {
        Ok(d) => (true, d),
        Err(d) => (false, d),
    };
    println!(
        "{}",
        json!({"op": format!("{prefix}{op}"), "ok": ok, "detail": detail})
    );
}

fn strs(v: &Value, k: &str) -> Vec<String> {
    v.get(k)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn tcp(addr: &str) -> Result<String, String> {
    let a = addr
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or("no address")?;
    TcpStream::connect_timeout(&a, Duration::from_secs(3))
        .map(|_| "connected".to_owned())
        .map_err(|e| e.to_string())
}

fn udp_dns(addr: &str) -> Result<String, String> {
    let s = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    // A query for example.com A.
    let q: [u8; 29] = [
        0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 7, b'e', b'x', b'a', b'm', b'p', b'l', b'e', 3,
        b'c', b'o', b'm', 0, 0, 1, 0, 1,
    ];
    s.send_to(&q, addr).map_err(|e| e.to_string())?;
    let mut buf = [0u8; 512];
    s.recv_from(&mut buf)
        .map(|(n, _)| format!("{n} bytes"))
        .map_err(|e| e.to_string())
}

fn local(name: &str) -> Result<String, String> {
    #[cfg(windows)]
    {
        let path = if name.starts_with(r"\\") {
            name.to_owned()
        } else {
            format!(r"\\.\pipe\{name}")
        };
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map(|_| "opened".to_owned())
            .map_err(|e| e.to_string())
    }
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::connect(name)
            .map(|_| "connected".to_owned())
            .map_err(|e| e.to_string())
    }
}

/// CONNECT han-mcp.invalid:1 through the proxy named in HTTPS_PROXY; the test's acceptor answers "pong".
fn proxy_route() -> Result<String, String> {
    let url = std::env::var("HTTPS_PROXY").map_err(|_| "no HTTPS_PROXY".to_owned())?;
    let rest = url.strip_prefix("http://").ok_or("not http")?;
    let (creds, hostport) = rest.rsplit_once('@').ok_or("no credentials")?;
    let mut s = TcpStream::connect(hostport).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    let auth = base64::engine::general_purpose::STANDARD.encode(creds);
    write!(
        s,
        "CONNECT han-mcp.invalid:1 HTTP/1.1\r\nHost: han-mcp.invalid:1\r\nProxy-Authorization: Basic {auth}\r\n\r\n"
    )
    .map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    let mut b = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        let n = s.read(&mut b).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("closed".into());
        }
        buf.push(b[0]);
    }
    if !buf.starts_with(b"HTTP/1.1 200") {
        return Err(String::from_utf8_lossy(&buf)
            .lines()
            .next()
            .unwrap_or("")
            .to_owned());
    }
    let mut pong = [0u8; 4];
    s.read_exact(&mut pong).map_err(|e| e.to_string())?;
    if &pong == b"pong" {
        Ok("pong".into())
    } else {
        Err("no pong".into())
    }
}

fn run(spec: &Value, prefix: &str) {
    for p in strs(spec, "write") {
        let r = std::fs::write(&p, b"probe")
            .map(|_| "written".to_owned())
            .map_err(|e| e.to_string());
        emit(prefix, format!("write:{p}"), r);
    }
    for p in strs(spec, "read") {
        let r = std::fs::read(&p)
            .map(|b| format!("{} bytes", b.len()))
            .map_err(|e| e.to_string());
        emit(prefix, format!("read:{p}"), r);
    }
    for n in strs(spec, "envvar") {
        let r = std::env::var(&n).map_err(|_| "unset".to_owned());
        emit(prefix, format!("envvar:{n}"), r);
    }
    for a in strs(spec, "tcp") {
        emit(prefix, format!("tcp:{a}"), tcp(&a));
    }
    for a in strs(spec, "udp") {
        emit(prefix, format!("udp:{a}"), udp_dns(&a));
    }
    for n in strs(spec, "dns") {
        let r = (n.as_str(), 80)
            .to_socket_addrs()
            .map(|mut a| format!("{:?}", a.next()))
            .map_err(|e| e.to_string());
        emit(prefix, format!("dns:{n}"), r);
    }
    for n in strs(spec, "local") {
        emit(prefix, format!("local:{n}"), local(&n));
    }
    if let Some(c) = spec.get("canary").and_then(Value::as_str) {
        let found = std::env::vars()
            .filter(|(k, _)| k != "HAN_PROBE_SPEC")
            .any(|(k, v)| k.contains(c) || v.contains(c));
        emit(
            prefix,
            "env-canary".into(),
            if found {
                Ok("present".into())
            } else {
                Err("absent".into())
            },
        );
    }
    if spec.get("env").and_then(Value::as_bool) == Some(true) {
        let mut names: Vec<String> = std::env::vars_os()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        names.sort();
        emit(prefix, "env".into(), Ok(names.join(",")));
    }
    if spec.get("proxy").and_then(Value::as_bool) == Some(true) {
        emit(prefix, "proxy-route".into(), proxy_route());
    }
}

fn main() {
    let spec: Value = std::env::var("HAN_PROBE_SPEC")
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    let is_child = std::env::var_os("HAN_PROBE_CHILD").is_some();
    run(&spec, if is_child { "child:" } else { "" });
    if !is_child && spec.get("child").and_then(Value::as_bool) == Some(true) {
        let exe = std::env::current_exe().unwrap_or_default();
        match std::process::Command::new(exe)
            .env("HAN_PROBE_CHILD", "1")
            .output()
        {
            Ok(o) => {
                let _ = std::io::stdout().write_all(&o.stdout);
            }
            Err(e) => emit("", "child-spawn".into(), Err(e.to_string())),
        }
    }
    let _ = std::io::stdout().flush();
}

//! Reading the server name from a TLS ClientHello (SR-36). The proxy does not intercept TLS; it only reads
//! the first handshake message of a tunnel to check that the client asks for the host it was allowed.

use tokio::io::{AsyncRead, AsyncReadExt};

/// The most bytes the proxy reads while looking for the ClientHello.
pub const MAX_HELLO: usize = 16 * 1024;

/// What the first handshake message said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hello {
    /// A ClientHello with this server name (lowercase, no trailing dot).
    Sni(String),
    /// A ClientHello without a server name.
    NoSni,
    /// Not a TLS ClientHello.
    NotTls,
}

/// Reads records from `r` until the ClientHello is complete, and returns every byte read (to forward) with
/// what it said.
pub async fn read_client_hello<R: AsyncRead + Unpin>(
    r: &mut R,
    already: Vec<u8>,
) -> std::io::Result<(Vec<u8>, Hello)> {
    let mut buf = already;
    loop {
        match parse(&buf) {
            Parse::Done(h) => return Ok((buf, h)),
            Parse::Bad => return Ok((buf, Hello::NotTls)),
            Parse::More => {}
        }
        if buf.len() >= MAX_HELLO {
            return Ok((buf, Hello::NotTls));
        }
        let mut chunk = vec![0u8; 4096];
        let n = r.read(&mut chunk).await?;
        if n == 0 {
            return Ok((buf, Hello::NotTls));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

enum Parse {
    Done(Hello),
    More,
    Bad,
}

/// Collects the handshake bytes carried by the leading handshake records.
fn handshake_bytes(buf: &[u8]) -> Result<(Vec<u8>, bool), ()> {
    let mut hs = Vec::new();
    let mut i = 0;
    loop {
        if buf.len() < i + 5 {
            return Ok((hs, false));
        }
        if buf[i] != 22 || buf[i + 1] != 3 {
            return Err(());
        }
        let len = usize::from(u16::from_be_bytes([buf[i + 3], buf[i + 4]]));
        if len == 0 || len > 16 * 1024 + 2048 {
            return Err(());
        }
        if buf.len() < i + 5 + len {
            hs.extend_from_slice(&buf[i + 5..]);
            return Ok((hs, false));
        }
        hs.extend_from_slice(&buf[i + 5..i + 5 + len]);
        i += 5 + len;
        if hs.len() >= 4 {
            let need =
                4 + ((usize::from(hs[1]) << 16) | (usize::from(hs[2]) << 8) | usize::from(hs[3]));
            if hs.len() >= need {
                return Ok((hs, true));
            }
        }
    }
}

fn parse(buf: &[u8]) -> Parse {
    let Ok((hs, complete)) = handshake_bytes(buf) else {
        return Parse::Bad;
    };
    if !hs.is_empty() && hs[0] != 1 {
        return Parse::Bad;
    }
    if !complete {
        return Parse::More;
    }
    match server_name(&hs) {
        Some(Some(name)) => Parse::Done(Hello::Sni(name)),
        Some(None) => Parse::Done(Hello::NoSni),
        None => Parse::Bad,
    }
}

struct Cur<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.i..self.i.checked_add(n)?)?;
        self.i += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<usize> {
        self.take(1).map(|s| usize::from(s[0]))
    }
    fn u16(&mut self) -> Option<usize> {
        self.take(2)
            .map(|s| usize::from(u16::from_be_bytes([s[0], s[1]])))
    }
}

/// `Some(Some(name))`, `Some(None)` for no SNI, `None` for a malformed hello.
fn server_name(hs: &[u8]) -> Option<Option<String>> {
    let mut c = Cur { b: hs, i: 4 };
    c.take(2)?; // legacy version
    c.take(32)?; // random
    let sid = c.u8()?;
    c.take(sid)?;
    let suites = c.u16()?;
    c.take(suites)?;
    let comp = c.u8()?;
    c.take(comp)?;
    if c.i == c.b.len() {
        return Some(None);
    }
    let ext_len = c.u16()?;
    let exts = c.take(ext_len)?;
    let mut e = Cur { b: exts, i: 0 };
    let mut found = None;
    while e.i < exts.len() {
        let ty = e.u16()?;
        let len = e.u16()?;
        let body = e.take(len)?;
        if ty == 0 {
            if found.is_some() {
                return None; // two server_name extensions
            }
            let mut s = Cur { b: body, i: 0 };
            let list = s.u16()?;
            let list = s.take(list)?;
            let mut l = Cur { b: list, i: 0 };
            let mut names = Vec::new();
            while l.i < list.len() {
                let kind = l.u8()?;
                let n = l.u16()?;
                let name = l.take(n)?;
                if kind == 0 {
                    names.push(name);
                }
            }
            if names.len() != 1 {
                return None;
            }
            let name = std::str::from_utf8(names[0]).ok()?;
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_graphic()) {
                return None;
            }
            found = Some(name.trim_end_matches('.').to_ascii_lowercase());
        }
    }
    Some(found)
}

/// A minimal ClientHello for `name` (tests and the probe use it).
pub fn client_hello(name: Option<&str>) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&[3, 3]);
    body.extend_from_slice(&[7u8; 32]);
    body.push(0); // session id
    body.extend_from_slice(&[0, 2, 0x13, 0x01]); // one suite
    body.extend_from_slice(&[1, 0]); // null compression
    let mut exts = Vec::new();
    if let Some(n) = name {
        let n = n.as_bytes();
        let mut sn = Vec::new();
        sn.extend_from_slice(&((n.len() + 3) as u16).to_be_bytes());
        sn.push(0);
        sn.extend_from_slice(&(n.len() as u16).to_be_bytes());
        sn.extend_from_slice(n);
        exts.extend_from_slice(&[0, 0]);
        exts.extend_from_slice(&(sn.len() as u16).to_be_bytes());
        exts.extend_from_slice(&sn);
    }
    body.extend_from_slice(&(exts.len() as u16).to_be_bytes());
    body.extend_from_slice(&exts);
    let mut hs = vec![1u8];
    hs.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
    hs.extend_from_slice(&body);
    let mut rec = vec![22u8, 3, 1];
    rec.extend_from_slice(&(hs.len() as u16).to_be_bytes());
    rec.extend_from_slice(&hs);
    rec
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_sni() {
        let hello = client_hello(Some("Example.COM."));
        let (bytes, h) = read_client_hello(&mut &hello[..], Vec::new())
            .await
            .unwrap();
        assert_eq!(h, Hello::Sni("example.com".into()));
        assert_eq!(bytes, hello);
        let (_, h) = read_client_hello(&mut &client_hello(None)[..], Vec::new())
            .await
            .unwrap();
        assert_eq!(h, Hello::NoSni);
        let (_, h) = read_client_hello(&mut &b"GET / HTTP/1.1\r\n\r\n"[..], Vec::new())
            .await
            .unwrap();
        assert_eq!(h, Hello::NotTls);
        // split across reads and records
        let mut split = hello.clone();
        let tail = split.split_off(9);
        let (_, h) = read_client_hello(&mut &tail[..], split).await.unwrap();
        assert_eq!(h, Hello::Sni("example.com".into()));
    }
}

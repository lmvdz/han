//! Destination checks (SR-37): the proxy resolves every hostname itself, refuses any address that is not a
//! public unicast address, and connects to the exact address it checked.

use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;

/// A boxed future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Resolves a hostname. Injected so tests can make an allow-listed name resolve to a private address.
pub trait Resolve: Send + Sync + 'static {
    /// The addresses of `host` at `port`.
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> BoxFuture<'a, io::Result<Vec<SocketAddr>>>;
}

/// The operating system's resolver (`tokio::net::lookup_host`).
pub struct SystemResolver;

impl Resolve for SystemResolver {
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> BoxFuture<'a, io::Result<Vec<SocketAddr>>> {
        Box::pin(async move { Ok(tokio::net::lookup_host((host, port)).await?.collect()) })
    }
}

/// A resolver with fixed answers (tests, and pinning a host to an address).
#[derive(Default)]
pub struct StaticResolver(pub Vec<(String, Vec<IpAddr>)>);

impl Resolve for StaticResolver {
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> BoxFuture<'a, io::Result<Vec<SocketAddr>>> {
        Box::pin(async move {
            self.0
                .iter()
                .find(|(h, _)| h.eq_ignore_ascii_case(host))
                .map(|(_, ips)| ips.iter().map(|ip| SocketAddr::new(*ip, port)).collect())
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such host"))
        })
    }
}

/// Why an address is refused, or `None` when it is a public unicast address.
pub fn refused_class(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) => v4_class(v4),
        IpAddr::V6(v6) => {
            // IPv4-mapped (::ffff:a.b.c.d) and IPv4-compatible (::a.b.c.d) forms are judged as IPv4.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return v4_class(v4);
            }
            let s = v6.segments();
            if s[0..6] == [0, 0, 0, 0, 0, 0] && !(s[6] == 0 && s[7] <= 1) {
                return Some("ipv4-compatible");
            }
            v6_class(v6)
        }
    }
}

fn v4_class(ip: Ipv4Addr) -> Option<&'static str> {
    let o = ip.octets();
    if ip.is_unspecified() || o[0] == 0 {
        Some("unspecified")
    } else if ip.is_loopback() {
        Some("loopback")
    } else if ip.is_private() {
        Some("private")
    } else if ip.is_link_local() {
        Some("link-local")
    } else if o[0] == 100 && (o[1] & 0xc0) == 64 {
        Some("cgnat")
    } else if ip.is_multicast() || o[0] >= 224 {
        Some("multicast-or-reserved")
    } else if ip.is_broadcast() {
        Some("broadcast")
    } else if ip.is_documentation() || (o[0] == 198 && (o[1] & 0xfe) == 18) {
        Some("documentation-or-benchmark")
    } else if o[0] == 192 && o[1] == 0 && o[2] == 0 {
        Some("ietf-protocol")
    } else {
        None
    }
}

fn v6_class(ip: Ipv6Addr) -> Option<&'static str> {
    let s = ip.segments();
    if ip.is_unspecified() {
        Some("unspecified")
    } else if ip.is_loopback() {
        Some("loopback")
    } else if (s[0] & 0xfe00) == 0xfc00 {
        Some("ula")
    } else if (s[0] & 0xffc0) == 0xfe80 || (s[0] & 0xffc0) == 0xfec0 {
        Some("link-local")
    } else if ip.is_multicast() {
        Some("multicast")
    } else if s[0] == 0x2001 && s[1] == 0x0db8 {
        Some("documentation")
    } else if s[0] == 0x0064 && s[1] == 0xff9b {
        // NAT64: the embedded IPv4 address decides.
        let v4 = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
        v4_class(v4).map(|_| "nat64-private")
    } else if s[0] == 0x2002 {
        // 6to4: the embedded IPv4 address decides.
        let v4 = Ipv4Addr::new((s[1] >> 8) as u8, s[1] as u8, (s[2] >> 8) as u8, s[2] as u8);
        v4_class(v4).map(|_| "6to4-private")
    } else if s[0] == 0x2001 && s[1] < 0x0200 {
        Some("ietf-protocol")
    } else {
        None
    }
}

/// Whether `host` is an IP literal (with or without brackets).
pub fn is_ip_literal(host: &str) -> bool {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_classes() {
        let refused = [
            "127.0.0.1",
            "127.255.0.9",
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "100.127.255.254",
            "224.0.0.1",
            "255.255.255.255",
            "240.0.0.1",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.5",
            "198.18.0.1",
            "::",
            "::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:10.1.2.3",
            "::ffff:169.254.169.254",
            "::ffff:100.64.1.1",
            "::127.0.0.1",
            "64:ff9b::a00:1",
            "2002:a00:1::1",
        ];
        for a in refused {
            let ip: IpAddr = a.parse().unwrap();
            assert!(refused_class(ip).is_some(), "{a} must be refused");
        }
        let allowed = [
            "1.1.1.1",
            "8.8.8.8",
            "160.79.104.10",
            "100.128.0.1",
            "172.32.0.1",
            "2606:4700::1111",
            "::ffff:1.1.1.1",
            "64:ff9b::101:101",
        ];
        for a in allowed {
            let ip: IpAddr = a.parse().unwrap();
            assert_eq!(refused_class(ip), None, "{a} must be allowed");
        }
    }
}

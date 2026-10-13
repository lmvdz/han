//! `http_request`: one HTTPS request to an allow-listed host (`ExecConfig.http_hosts`). The executor resolves
//! the name itself, refuses loopback, private, link-local, CGNAT, ULA, multicast and unspecified addresses
//! (and IPv4-mapped forms of them), and connects to the address it checked. Redirects are not followed. No
//! credential is added: credential headers in a plan are refused (a destination credential would come from
//! the executor's configuration, none is configured yet). The response is summarised as status and size.
//!
//! Plan: `{action: "http_request", target: <host>, args: {method, url, headers?, body?}}`.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use crate::{Action, ActionCtx, ExecConfig, IdempotencyKey, Outcome, PlanError, plan_args};

/// The action.
pub struct HttpRequest;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    method: String,
    url: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    body: Option<String>,
}

fn args(plan: &Value) -> Result<Args, PlanError> {
    serde_json::from_value(plan_args(plan, "http_request")?)
        .map_err(|e| PlanError::Schema(e.to_string()))
}

const FORBIDDEN_HEADERS: [&str; 7] = [
    "authorization",
    "proxy-authorization",
    "cookie",
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
];

/// Whether an address may be dialled from the executor (SR-37's classes).
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (64..128).contains(&o[1]))
                || o[0] >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let seg0 = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg0 & 0xfe00) == 0xfc00
                || (seg0 & 0xffc0) == 0xfe80)
        }
    }
}

struct Parsed {
    scheme: String,
    host: String,
    port: u16,
}

fn parse_url(url: &str) -> Result<Parsed, PlanError> {
    let u = reqwest::Url::parse(url).map_err(|_| PlanError::Schema("not a URL".into()))?;
    if !u.username().is_empty() || u.password().is_some() {
        return Err(PlanError::Schema("credentials in the URL".into()));
    }
    let host = u
        .host_str()
        .ok_or_else(|| PlanError::Schema("no host".into()))?
        .to_ascii_lowercase();
    let port = u
        .port_or_known_default()
        .ok_or_else(|| PlanError::Schema("no port".into()))?;
    Ok(Parsed {
        scheme: u.scheme().to_owned(),
        host,
        port,
    })
}

#[async_trait::async_trait]
impl Action for HttpRequest {
    fn kind(&self) -> &'static str {
        "http_request"
    }

    fn plan_check(&self, plan: &Value, config: &ExecConfig) -> Result<(), PlanError> {
        let a = args(plan)?;
        if !["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"].contains(&a.method.as_str()) {
            return Err(PlanError::Schema(format!("method {}", a.method)));
        }
        let p = parse_url(&a.url)?;
        if p.scheme != "https" && !(config.test_allow_private_http && p.scheme == "http") {
            return Err(PlanError::Schema("only https URLs".into()));
        }
        if crate::plan_target(plan).to_ascii_lowercase() != p.host {
            return Err(PlanError::Schema("the target is not the URL's host".into()));
        }
        if !config
            .http_hosts
            .iter()
            .any(|h| h.eq_ignore_ascii_case(&p.host))
        {
            return Err(PlanError::OutOfScope(format!("host {}", p.host)));
        }
        for k in a.headers.keys() {
            let k = k.to_ascii_lowercase();
            if FORBIDDEN_HEADERS.contains(&k.as_str())
                || k.is_empty()
                || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                return Err(PlanError::Schema(format!("header {k} is not allowed")));
            }
        }
        if a.body.as_ref().is_some_and(|b| b.len() > 1 << 20) {
            return Err(PlanError::Schema("the body is over 1 MiB".into()));
        }
        Ok(())
    }

    async fn preflight(&self, _plan: &Value, _ctx: &ActionCtx) -> Result<(), PlanError> {
        Ok(())
    }

    async fn run(&self, plan: &Value, _key: &IdempotencyKey, ctx: &ActionCtx) -> Outcome {
        let not_started = |detail: String| Outcome::Failed {
            started: false,
            detail,
        };
        let Ok(a) = args(plan) else {
            return not_started("plan".into());
        };
        let Ok(p) = parse_url(&a.url) else {
            return not_started("url".into());
        };
        let addrs: Vec<SocketAddr> = match tokio::net::lookup_host((p.host.as_str(), p.port)).await
        {
            Ok(it) => it.collect(),
            Err(_) => return not_started(format!("cannot resolve {}", p.host)),
        };
        let allow_private = ctx.config.test_allow_private_http;
        let Some(addr) = addrs
            .into_iter()
            .find(|a| allow_private || is_public(a.ip()))
        else {
            return not_started(format!("{} resolves only to refused addresses", p.host));
        };
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .resolve(&p.host, addr)
            .build()
        {
            Ok(c) => c,
            Err(_) => return not_started("client".into()),
        };
        let method = match reqwest::Method::from_bytes(a.method.as_bytes()) {
            Ok(m) => m,
            Err(_) => return not_started("method".into()),
        };
        let mut req = client.request(method, &a.url);
        for (k, v) in &a.headers {
            req = req.header(k, v);
        }
        if let Some(b) = a.body {
            req = req.body(b);
        }
        match req.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let size = resp.bytes().await.map(|b| b.len()).unwrap_or(0);
                let detail = format!("HTTP {status}, {size} bytes from {}", p.host);
                if (200..300).contains(&status) {
                    Outcome::Succeeded { detail }
                } else {
                    Outcome::Failed {
                        started: true,
                        detail,
                    }
                }
            }
            Err(e) if e.is_connect() => not_started(format!("cannot connect to {}", p.host)),
            Err(e) => Outcome::Unknown {
                detail: format!("{}: {}", p.host, crate::github::without_url(&e)),
            },
        }
    }

    async fn lookup(
        &self,
        _plan: &Value,
        _key: &IdempotencyKey,
        _ctx: &ActionCtx,
    ) -> Option<Outcome> {
        // A request is not desired state: nothing in the world says whether it was made.
        None
    }
}

//! A fake model provider for the budget scenario: an Anthropic-style `/v1/messages` over TLS (the session proxy
//! forwards to TLS only, SR-39) with a certificate authority of its own. Every call reports the same usage, so
//! the proxy has real counts to price. It holds no key; it only checks that one arrived, as a provider would.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;

/// The model name the dev stack prices (see `han dev up --model-url`).
pub const MODEL: &str = "fake-model";

pub struct FakeModel {
    pub url: String,
    /// The PEM file of the authority that signed its certificate.
    pub ca_file: std::path::PathBuf,
    calls: Arc<AtomicU64>,
}

impl FakeModel {
    /// Starts it on a free loopback port; the authority is written under `dir`. Each call used `input` tokens
    /// in and `output` out.
    pub async fn start(dir: &Path, input: u64, output: u64) -> FakeModel {
        let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
        let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "han e2e model ca");
        let ca =
            rcgen::CertifiedIssuer::self_signed(ca_params, rcgen::KeyPair::generate().unwrap())
                .unwrap();
        let leaf_key = rcgen::KeyPair::generate().unwrap();
        let leaf = rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned()])
            .unwrap()
            .signed_by(&leaf_key, &ca)
            .unwrap();
        let tls = tokio_rustls::rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![leaf.der().clone()],
                tokio_rustls::rustls::pki_types::PrivateKeyDer::Pkcs8(
                    leaf_key.serialize_der().into(),
                ),
            )
            .unwrap();
        std::fs::create_dir_all(dir).unwrap();
        let ca_file = dir.join("model-ca.pem");
        std::fs::write(&ca_file, ca.pem()).unwrap();

        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&calls);
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    continue;
                };
                let (acceptor, counted) = (acceptor.clone(), Arc::clone(&counted));
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(sock).await else {
                        return;
                    };
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 4096];
                    let (head_end, length, keyed) = loop {
                        let Ok(n) = tls.read(&mut tmp).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&buf[..p]).to_ascii_lowercase();
                            let header = |name: &str| {
                                head.lines().find_map(|l| {
                                    l.strip_prefix(name)
                                        .and_then(|v| v.strip_prefix(':'))
                                        .map(|v| v.trim().to_owned())
                                })
                            };
                            break (
                                p + 4,
                                header("content-length")
                                    .and_then(|v| v.parse::<usize>().ok())
                                    .unwrap_or(0),
                                header("x-api-key").is_some_and(|v| !v.is_empty()),
                            );
                        }
                    };
                    while buf.len() < head_end + length {
                        let Ok(n) = tls.read(&mut tmp).await else {
                            return;
                        };
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let (status, body) = if keyed {
                        counted.fetch_add(1, Ordering::SeqCst);
                        (
                            "200 OK",
                            format!(
                                r#"{{"id":"msg_fake","type":"message","role":"assistant","model":"{MODEL}","content":[{{"type":"text","text":"ok"}}],"stop_reason":"end_turn","usage":{{"input_tokens":{input},"output_tokens":{output}}}}}"#
                            ),
                        )
                    } else {
                        (
                            "401 Unauthorized",
                            r#"{"type":"error","error":{"type":"authentication_error","message":"no key"}}"#
                                .to_owned(),
                        )
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = tls.write_all(response.as_bytes()).await;
                    let _ = tls.shutdown().await;
                });
            }
        });
        FakeModel {
            url: format!("https://127.0.0.1:{}", addr.port()),
            ca_file,
            calls,
        }
    }

    /// How many calls it answered with a key.
    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

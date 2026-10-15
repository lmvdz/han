//! The one GitHub App token minter (SR-49): han-exec alone holds the App's private key, signs the App JWT
//! (RS256, with `ring`) and asks GitHub for an installation token scoped to **one repository** and **the
//! permissions of one action** (SR-67). Tokens live in memory only, for one action ([`Secret`], SR-50).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};

use crate::{GithubApp, Secret};

/// Makes the App's JWT.
pub trait JwtSource: Send + Sync + 'static {
    /// A JWT for `app_id`, valid around `now_secs`.
    fn jwt(&self, app_id: u64, now_secs: u64) -> Result<Secret, String>;
}

/// A JWT signed with the App's RSA private key.
pub struct RsaJwt {
    key: ring::signature::RsaKeyPair,
}

impl RsaJwt {
    /// From a PEM file's text: PKCS#1 (`RSA PRIVATE KEY`, what GitHub issues) or PKCS#8 (`PRIVATE KEY`).
    pub fn from_pem(pem: &str) -> Result<RsaJwt, String> {
        let body: String = pem
            .lines()
            .filter(|l| !l.starts_with("-----"))
            .map(str::trim)
            .collect();
        let der = base64::engine::general_purpose::STANDARD
            .decode(body)
            .map_err(|_| "the App key is not PEM".to_string())?;
        let key = if pem.contains("BEGIN RSA PRIVATE KEY") {
            ring::signature::RsaKeyPair::from_der(&der)
        } else {
            ring::signature::RsaKeyPair::from_pkcs8(&der)
        }
        .map_err(|_| "the App key is not an RSA private key".to_string())?;
        Ok(RsaJwt { key })
    }
}

impl JwtSource for RsaJwt {
    fn jwt(&self, app_id: u64, now_secs: u64) -> Result<Secret, String> {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let claims = json!({"iat": now_secs.saturating_sub(60), "exp": now_secs + 540, "iss": app_id.to_string()});
        let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
        let input = format!("{header}.{payload}");
        let mut sig = vec![0u8; self.key.public().modulus_len()];
        self.key
            .sign(
                &ring::signature::RSA_PKCS1_SHA256,
                &ring::rand::SystemRandom::new(),
                input.as_bytes(),
                &mut sig,
            )
            .map_err(|_| "signing the App JWT failed".to_string())?;
        Ok(Secret::new(format!(
            "{input}.{}",
            URL_SAFE_NO_PAD.encode(sig)
        )))
    }
}

/// The minter.
pub struct GithubMinter {
    app_id: u64,
    installation_id: u64,
    api_base: String,
    jwt: Box<dyn JwtSource>,
    http: reqwest::Client,
}

/// The default API base.
pub const GITHUB_API: &str = "https://api.github.com";

impl GithubMinter {
    /// A minter for `app` whose JWT comes from `jwt`.
    pub fn new(app: &GithubApp, jwt: Box<dyn JwtSource>) -> Result<GithubMinter, String> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("han-exec")
            .build()
            .map_err(|e| e.to_string())?;
        Ok(GithubMinter {
            app_id: app.app_id,
            installation_id: app.installation_id,
            api_base: app
                .api_base
                .clone()
                .unwrap_or_else(|| GITHUB_API.into())
                .trim_end_matches('/')
                .to_owned(),
            jwt,
            http,
        })
    }

    /// A minter that reads the App's private key from `app.private_key` (a PEM file).
    pub fn from_app(app: &GithubApp) -> Result<GithubMinter, String> {
        let pem = std::fs::read_to_string(&app.private_key)
            .map_err(|e| format!("reading the App key: {}", e.kind()))?;
        GithubMinter::new(app, Box::new(RsaJwt::from_pem(&pem)?))
    }

    /// The API base.
    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    /// A GitHub client whose installation token is scoped to `repo` (`owner/name`) and exactly `permissions`.
    pub async fn client(
        &self,
        repo: &str,
        permissions: &[(&str, &str)],
    ) -> Result<octocrab::Octocrab, String> {
        let token = self.token(repo, permissions).await?;
        let _ = rustls::crypto::ring::default_provider().install_default();
        octocrab::Octocrab::builder()
            .base_uri(self.api_base())
            .map_err(|_| "bad API base".to_string())?
            .personal_token(token.expose().to_owned())
            .build()
            .map_err(|_| "building the GitHub client failed".to_string())
    }

    /// An installation token for `repo` (`owner/name`) with exactly `permissions` (for example
    /// `[("contents", "write")]`).
    pub async fn token(&self, repo: &str, permissions: &[(&str, &str)]) -> Result<Secret, String> {
        let name = repo
            .split_once('/')
            .map(|(_, n)| n)
            .filter(|n| !n.is_empty())
            .ok_or_else(|| format!("not owner/repo: {repo}"))?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let jwt = self.jwt.jwt(self.app_id, now)?;
        let perms: serde_json::Map<String, Value> = permissions
            .iter()
            .map(|(k, v)| ((*k).to_owned(), Value::String((*v).to_owned())))
            .collect();
        let body = json!({"repositories": [name], "permissions": perms});
        let url = format!(
            "{}/app/installations/{}/access_tokens",
            self.api_base, self.installation_id
        );
        let resp = self
            .http
            .post(&url)
            .bearer_auth(jwt.expose())
            .header("accept", "application/vnd.github+json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("token request: {}", without_url(&e)))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("token request: HTTP {}", status.as_u16()));
        }
        let v: Value = resp
            .json()
            .await
            .map_err(|_| "token response is not JSON".to_string())?;
        v.get("token")
            .and_then(Value::as_str)
            .map(Secret::new)
            .ok_or_else(|| "token response has no token".to_string())
    }
}

/// A reqwest error's text without its URL (query strings could carry data).
pub(crate) fn without_url(e: &reqwest::Error) -> String {
    let kind = if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connect"
    } else if e.is_request() {
        "request"
    } else {
        "transport"
    };
    kind.to_owned()
}

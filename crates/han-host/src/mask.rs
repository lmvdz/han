//! Secret masking (PLAN §6.3 Secrets; SR-52): everything han-host publishes (stream items, timeline ops, tool
//! answers) passes through a [`Masker`] first. Two kinds of rules:
//!
//! * gitleaks-style patterns for well-known credential shapes (cloud keys, GitHub, Anthropic, OpenAI and Slack
//!   tokens, private key blocks, JWTs, bearer tokens, and `api_key = …`-style assignments with a secret-looking
//!   value);
//! * exact known secrets: the model keys the proxy holds, the session's proxy token, and any other value the
//!   host knows is secret.
//!
//! A match is replaced with `[masked:<rule>]`. Paths on the exclusion list (`.env`, `*.pem`, `~/.ssh`…) are
//! never mirrored or streamed at all ([`Masker::is_excluded`]).

use std::borrow::Cow;

use regex::Regex;
use serde_json::Value;

use crate::mirror::glob::glob_match;

/// The default exclusion list (PLAN §6.3): paths that are never mirrored or streamed.
pub const DEFAULT_EXCLUSIONS: &[&str] = &[
    ".env",
    ".env.*",
    "*.env",
    "*.pem",
    "*.key",
    "*.p12",
    "*.pfx",
    "*.keystore",
    "*.jks",
    "id_rsa*",
    "id_dsa*",
    "id_ecdsa*",
    "id_ed25519*",
    ".ssh/**",
    "**/credentials*",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".git-credentials",
    ".aws/**",
    ".config/gh/**",
    ".docker/config.json",
    "*.kdbx",
];

/// Known secrets shorter than this are not masked exactly: too many false positives.
const MIN_KNOWN: usize = 6;

struct Rule {
    name: &'static str,
    re: Regex,
    /// The capture group to mask (0: the whole match).
    group: usize,
    /// Whether the masked text must look like a secret (a digit and a letter, not a call).
    secretish: bool,
}

/// Masks secrets in text before it leaves the device.
pub struct Masker {
    rules: Vec<Rule>,
    known: Vec<String>,
    exclusions: Vec<String>,
}

impl std::fmt::Debug for Masker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Masker")
            .field("rules", &self.rules.len())
            .field("known", &self.known.len())
            .field("exclusions", &self.exclusions)
            .finish()
    }
}

fn rule(name: &'static str, re: &str, group: usize, secretish: bool) -> Rule {
    Rule {
        name,
        re: Regex::new(re).expect("mask rule compiles"),
        group,
        secretish,
    }
}

fn looks_secret(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_digit()) && s.chars().any(|c| c.is_ascii_alphabetic())
}

impl Masker {
    /// A masker with the built-in rules, these exact secrets, and the default exclusion list.
    pub fn new(known_secrets: Vec<String>) -> Self {
        Self::with_exclusions(
            known_secrets,
            DEFAULT_EXCLUSIONS.iter().map(|s| (*s).to_owned()).collect(),
        )
    }

    /// A masker with its own exclusion list (the space's).
    pub fn with_exclusions(known_secrets: Vec<String>, exclusions: Vec<String>) -> Self {
        let rules = vec![
            rule(
                "private-key",
                r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----(?s:.*?)(?:-----END [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----|\z)",
                0,
                false,
            ),
            rule(
                "aws-access-key",
                r"\b(?:AKIA|ASIA|AGPA|AIDA|AROA|ANPA|ANVA|AIPA)[0-9A-Z]{16}\b",
                0,
                false,
            ),
            rule(
                "aws-secret-key",
                r#"(?i)aws_?secret_?(?:access_?)?key["']?\s*[:=]\s*["']?([A-Za-z0-9/+=]{40})"#,
                1,
                false,
            ),
            rule(
                "github-token",
                r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{36,255}\b",
                0,
                false,
            ),
            rule(
                "github-pat",
                r"\bgithub_pat_[A-Za-z0-9_]{22,255}\b",
                0,
                false,
            ),
            rule("anthropic-key", r"\bsk-ant-[A-Za-z0-9_\-]{16,}", 0, false),
            rule(
                "openai-key",
                r"\bsk-(?:proj-|svcacct-|admin-)?[A-Za-z0-9_\-]{20,}",
                0,
                false,
            ),
            rule(
                "slack-token",
                r"\bxox[abposre]-[A-Za-z0-9\-]{10,}",
                0,
                false,
            ),
            rule(
                "slack-webhook",
                r"https://hooks\.slack\.com/services/[A-Za-z0-9_/]{20,}",
                0,
                false,
            ),
            rule(
                "jwt",
                r"\beyJ[A-Za-z0-9_\-]{8,}\.eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}",
                0,
                false,
            ),
            rule(
                "bearer",
                r"(?i)\bbearer\s+([A-Za-z0-9\-._~+/]{20,}=*)",
                1,
                true,
            ),
            rule(
                "url-credentials",
                r"(?i)\b[a-z][a-z0-9+.\-]*://[^\s:/@]+:([^\s@/]{6,})@",
                1,
                false,
            ),
            rule(
                "assignment",
                r#"(?i)\b(?:api[_-]?key|apikey|secret[_-]?key|client[_-]?secret|secret|password|passwd|pwd|access[_-]?token|auth[_-]?token|refresh[_-]?token|token|private[_-]?key)\b["']?\s*(?:[:=]|:=|=>)\s*["']?([A-Za-z0-9_\-+/=.~!@#$%^&*]{8,})"#,
                1,
                true,
            ),
        ];
        let mut known: Vec<String> = known_secrets
            .into_iter()
            .filter(|k| k.len() >= MIN_KNOWN)
            .collect();
        // Longest first, so a secret that contains another is masked whole.
        known.sort_by_key(|k| std::cmp::Reverse(k.len()));
        known.dedup();
        Masker {
            rules,
            known,
            exclusions,
        }
    }

    /// Adds an exact secret (for example a rotated key).
    pub fn add_known(&mut self, secret: impl Into<String>) {
        let s = secret.into();
        if s.len() >= MIN_KNOWN && !self.known.contains(&s) {
            self.known.push(s);
            self.known.sort_by_key(|k| std::cmp::Reverse(k.len()));
        }
    }

    /// Whether a path (relative, `/`-separated) is on the exclusion list.
    pub fn is_excluded(&self, path: &str) -> bool {
        self.exclusions.iter().any(|p| glob_match(p, path))
    }

    /// The exclusion list.
    pub fn exclusions(&self) -> &[String] {
        &self.exclusions
    }

    /// The text with every secret replaced by `[masked:<rule>]`.
    pub fn mask<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let mut out: Cow<'a, str> = Cow::Borrowed(text);
        for k in &self.known {
            if out.contains(k.as_str()) {
                out = Cow::Owned(out.replace(k.as_str(), "[masked:known]"));
            }
        }
        for r in &self.rules {
            if !r.re.is_match(&out) {
                continue;
            }
            let src = out.as_ref();
            let mut s = String::with_capacity(src.len());
            let mut last = 0;
            let mut changed = false;
            for caps in r.re.captures_iter(src) {
                let Some(m) = caps.get(r.group) else { continue };
                if r.secretish {
                    let value = m.as_str();
                    let call = src[m.end()..].starts_with('(');
                    if !looks_secret(value) || call || value.starts_with("[masked") {
                        continue;
                    }
                }
                s.push_str(&src[last..m.start()]);
                s.push_str("[masked:");
                s.push_str(r.name);
                s.push(']');
                last = m.end();
                changed = true;
            }
            if changed {
                s.push_str(&src[last..]);
                out = Cow::Owned(s);
            }
        }
        out
    }

    /// Masks every string in a JSON value, keys included.
    pub fn mask_json(&self, v: &mut Value) {
        match v {
            Value::String(s) => {
                if let Cow::Owned(m) = self.mask(s) {
                    *s = m;
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|x| self.mask_json(x)),
            Value::Object(o) => {
                let old = std::mem::take(o);
                for (k, mut x) in old {
                    self.mask_json(&mut x);
                    o.insert(self.mask(&k).into_owned(), x);
                }
            }
            _ => {}
        }
    }

    /// Whether the text holds anything this masker would mask.
    pub fn finds(&self, text: &str) -> bool {
        matches!(self.mask(text), Cow::Owned(_))
    }
}

impl Default for Masker {
    fn default() -> Self {
        Masker::new(Vec::new())
    }
}

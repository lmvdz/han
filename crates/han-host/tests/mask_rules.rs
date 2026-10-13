//! Secret masking (SR-52): each rule fires on its credential shape, known secrets are masked exactly, JSON is
//! masked throughout, and ordinary code is left alone. Token-shaped fixtures carry a `~` that is removed at
//! run time, so no credential-shaped literal sits in the source.

use han_host::mask::Masker;
use serde_json::json;

#[test]
fn each_rule_masks_its_shape() {
    let m = Masker::new(vec![]);
    let cases = [
        ("aws-access-key", "key AKIA~IOSFODNN7EXAMPLE end"),
        (
            "aws-secret-key",
            "aws_secret_access_key = wJalr~XUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        ),
        ("github-token", "gh~p_abcdefghijklmnopqrstuvwxyz0123456789"),
        (
            "github-token",
            "token: gh~s_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
        ),
        (
            "github-pat",
            "github~_pat_11ABCDEFG0123456789_abcdefghijklmnop",
        ),
        ("anthropic-key", "sk-~ant-api03-abcdefghijklmnopqrstuvwxyz"),
        ("openai-key", "OPENAI=sk-~proj-abcdefghijklmnopqrstuvwx"),
        ("slack-token", "xo~xb-1234567890-abcdefghij"),
        (
            "slack-webhook",
            "https://hooks.slack.com/serv~ices/T000/B000/XXXXXXXXXXXXXXXXXXXX",
        ),
        (
            "private-key",
            "-----BEGIN RSA PRIV~ATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----",
        ),
        (
            "private-key",
            "-----BEGIN OPENSSH PRIV~ATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAA (cut)",
        ),
        (
            "jwt",
            "ey~JhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
        ),
        (
            "bearer",
            "Authorization: Bearer abc123def456ghi789jkl012mno",
        ),
        ("url-credentials", "https://user:s3cret~Pass@example.com/x"),
        ("assignment", "password = \"hunter2hunter2\""),
        ("assignment", "api_key: 9f8e7d6c5b4a39281706"),
        ("assignment", "{\"client_secret\": \"abc123XYZ789qwe\"}"),
    ];
    for (rule, text) in cases {
        let text = &text.replace('~', "");
        let out = m.mask(text);
        assert!(
            out.contains(&format!("[masked:{rule}]")),
            "{rule} did not fire on {text:?}: {out}"
        );
    }
}

#[test]
fn known_secrets_are_masked_exactly_and_longest_first() {
    let m = Masker::new(vec![
        "tok-123456".into(),
        "tok-123456-longer".into(),
        "abc".into(),
    ]);
    assert_eq!(m.mask("x tok-123456-longer y"), "x [masked:known] y");
    assert_eq!(m.mask("tok-123456"), "[masked:known]");
    // Too short to be masked exactly.
    assert_eq!(m.mask("abc"), "abc");
}

#[test]
fn ordinary_code_is_left_alone() {
    let m = Masker::new(vec![]);
    let code = r#"
fn token(&self) -> &str { &self.token }
let password = read_password();
let api_key = config.api_key.clone();
const SECRET_HEADER: &str = "x-api-key";
// The bearer of bad news.
let key = "skeleton";
let sk = "sk-short";
headers.insert("Authorization", format!("Bearer {}", token));
https://example.com/path?query=1
"#;
    assert_eq!(m.mask(code), code);
}

#[test]
fn json_is_masked_throughout() {
    let m = Masker::new(vec!["SESSION-TOKEN-XYZ".into()]);
    let gh = "gh~p_abcdefghijklmnopqrstuvwxyz0123456789".replace('~', "");
    let mut v = json!({
        "a": [gh, 1, {"b": "SESSION-TOKEN-XYZ"}],
        "SESSION-TOKEN-XYZ": true,
        "fine": "nothing here"
    });
    m.mask_json(&mut v);
    let s = v.to_string();
    assert!(
        !s.contains("ghp_") && !s.contains("SESSION-TOKEN-XYZ"),
        "{s}"
    );
    assert_eq!(v["fine"], "nothing here");
}

#[test]
fn exclusions() {
    let m = Masker::new(vec![]);
    for p in [
        ".env",
        "app/.env.local",
        "certs/a.pem",
        ".ssh/id_ed25519",
        "x/credentials.json",
        "id_rsa",
    ] {
        assert!(m.is_excluded(p), "{p}");
    }
    for p in ["src/env.rs", "README.md", "keyboard.rs", "docs/ssh.md"] {
        assert!(!m.is_excluded(p), "{p}");
    }
}

//! The reviewer against real git repositories in temporary directories and a fake model.

#![allow(missing_docs)]

use std::path::{Path, PathBuf};
use std::process::Command;

use han_review::{
    AuthorInfo, Disposition, FakeBackend, ModelReply, Refusal, ReviewError, ReviewInput,
    ReviewRecord, Reviewer, ReviewerConfig, ReviewerInfo, Severity, UNRESOLVED, Verdict,
    independent, prompt_hash, vendor_of,
};
use han_vcs::{GitVcs, Vcs};

fn git(dir: &Path, args: &[&str]) -> String {
    let empty = std::env::temp_dir().join("han-review-test-empty.gitconfig");
    if !empty.exists() {
        std::fs::write(&empty, b"").unwrap();
    }
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &empty)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .args(["-c", "core.autocrlf=false", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

struct Fixture {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    vcs: GitVcs,
    base: String,
    to: String,
    digest: String,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    let base = git(&dir, &["rev-parse", "HEAD"]);
    std::fs::write(dir.join("a.rs"), "fn a() { b() }\n").unwrap();
    std::fs::write(dir.join("b.rs"), "fn b() {}\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "change"]);
    let to = git(&dir, &["rev-parse", "HEAD"]);
    let vcs = GitVcs::new(tmp.path().join("store"));
    let digest = vcs.delta(&dir, &base, &to).unwrap().digest().unwrap();
    Fixture {
        _tmp: tmp,
        dir,
        vcs,
        base,
        to,
        digest,
    }
}

fn author() -> AuthorInfo {
    AuthorInfo {
        actor: "agent-claude".into(),
        operator_chain: vec!["lars".into()],
        harness: "pi".into(),
        model: "claude-opus-4-1".into(),
    }
}

fn reviewer_info() -> ReviewerInfo {
    ReviewerInfo {
        actor: "agent-reviewer".into(),
        operator_chain: vec!["review-operator".into()],
        model: "agy/gemini-flash".into(),
    }
}

fn config() -> ReviewerConfig {
    let mut c = ReviewerConfig::new(reviewer_info(), 16384);
    c.enabled = true;
    c
}

fn input<'a>(f: &'a Fixture, a: &'a AuthorInfo, digest: &'a str) -> ReviewInput<'a> {
    ReviewInput {
        proposal: "prop-1",
        tier: 1,
        base: &f.base,
        to: &f.to,
        delta_digest: digest,
        repo: &f.dir,
        author: a,
        at: "2026-10-15T12:00:00Z",
    }
}

const APPROVE: &str = r#"{"verdict":"approve","findings":[{"path":"b.rs","line":1,"severity":"info","text":"fine"}],"confidence":93}"#;

async fn run(
    f: &Fixture,
    cfg: ReviewerConfig,
    fake: &FakeBackend,
) -> Result<ReviewRecord, ReviewError> {
    let a = author();
    let model = cfg.reviewer.model.clone();
    Reviewer::new(cfg, &model, &f.vcs, fake)
        .unwrap()
        .review(&input(f, &a, &f.digest))
        .await
}

#[tokio::test]
async fn a_record_round_trips_and_carries_what_was_run() {
    let f = fixture();
    let fake = FakeBackend::answering(APPROVE, Some("gemini-3-flash-preview-0925"));
    let r = run(&f, config(), &fake).await.unwrap();
    assert_eq!(r.verdict, Verdict::Approve);
    assert_eq!(r.delta_digest, f.digest);
    assert_eq!(r.proposal, "prop-1");
    assert_eq!(r.model, "agy/gemini-flash");
    assert_eq!(r.model_version, "gemini-3-flash-preview-0925");
    assert_eq!(r.backend, "fake");
    assert_eq!(r.prompt_hash, prompt_hash());
    assert_eq!(r.findings[0].severity, Severity::Info);
    assert_eq!(r.usage.input_tokens, 100);

    let blob = r.to_blob().unwrap();
    assert_eq!(ReviewRecord::from_blob(&blob).unwrap(), r);
    assert_eq!(r.blob_hash().unwrap().len(), 64);
    // canonical: camelCase keys, sorted, no spaces
    let text = String::from_utf8(blob.clone()).unwrap();
    assert!(
        text.starts_with(r#"{"at":"2026-10-15T12:00:00Z","backend":"fake""#),
        "{text}"
    );
    assert!(ReviewRecord::from_blob(&blob[..blob.len() - 1]).is_err());
    let other = text.replace("han.review/1", "han.review/2");
    assert!(ReviewRecord::from_blob(other.as_bytes()).is_err());

    // the request the model saw sets the ceiling explicitly and fences the diff by the digest
    let seen = fake.requests();
    assert_eq!(seen[0].max_tokens, 16384);
    assert!(
        seen[0]
            .user
            .contains(&format!("=====diff {}=====", f.digest))
    );
    assert!(seen[0].user.contains("+fn b() {}"));
    assert!(seen[0].user.contains("- b.rs"));
}

#[tokio::test]
async fn a_digest_mismatch_is_refused_before_the_model_is_asked() {
    let f = fixture();
    let fake = FakeBackend::answering(APPROVE, Some("m"));
    let a = author();
    let model = reviewer_info().model;
    let wrong = "0".repeat(64);
    let err = Reviewer::new(config(), &model, &f.vcs, &fake)
        .unwrap()
        .review(&input(&f, &a, &wrong))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ReviewError::DigestMismatch { plan, computed } if *plan == wrong && *computed == f.digest),
        "{err}"
    );
    assert!(fake.requests().is_empty());
}

#[tokio::test]
async fn independence_refusals_come_before_the_model_is_asked() {
    let f = fixture();
    type Tweak = Box<dyn Fn(&mut ReviewerConfig)>;
    let cases: Vec<(&str, Tweak)> = vec![
        (
            "same vendor as the author's model",
            Box::new(|c| c.reviewer.model = "anthropic/claude-sonnet".into()),
        ),
        (
            "reviewer is the author",
            Box::new(|c| c.reviewer.actor = "agent-claude".into()),
        ),
        (
            "reviewer is the author's operator",
            Box::new(|c| c.reviewer.actor = "lars".into()),
        ),
        (
            "shared operator",
            Box::new(|c| c.reviewer.operator_chain = vec!["x".into(), "lars".into()]),
        ),
        (
            "unknown vendor",
            Box::new(|c| c.reviewer.model = "mystery-9".into()),
        ),
    ];
    for (name, tweak) in cases {
        let mut c = config();
        tweak(&mut c);
        let model = c.reviewer.model.clone();
        let fake = FakeBackend::answering(APPROVE, Some("m"));
        let a = author();
        let err = Reviewer::new(c, &model, &f.vcs, &fake)
            .unwrap()
            .review(&input(&f, &a, &f.digest))
            .await
            .unwrap_err();
        assert!(matches!(err, ReviewError::Refused(_)), "{name}: {err}");
        assert!(fake.requests().is_empty(), "{name}");
    }
}

#[test]
fn vendor_rules_cover_harness_model_and_chain() {
    let r = reviewer_info();
    let mut a = author();
    // a harness the table does not know fails closed; one of the reviewer's vendor is refused
    a.harness = "mystery-cli".into();
    assert!(matches!(
        independent(&a, &r),
        Err(Refusal::UnknownVendor { .. })
    ));
    a.harness = "google/gemini-cli".into();
    assert!(matches!(
        independent(&a, &r),
        Err(Refusal::SameVendor { of: "harness", .. })
    ));
    // claude-code is Anthropic's harness: fine against Gemini, refused against Claude
    a.harness = "claude-code".into();
    assert_eq!(independent(&a, &r), Ok(()));
    let mut r2 = r.clone();
    r2.model = "claude-haiku".into();
    assert!(matches!(
        independent(&a, &r2),
        Err(Refusal::SameVendor { .. })
    ));
    // the author's own operator reviewing
    let mut r3 = r.clone();
    r3.actor = "lars".into();
    assert_eq!(
        independent(&a, &r3),
        Err(Refusal::ReviewerIsAuthorSide("lars".into()))
    );
    // the reviewer's operator is the author's operator: the dogfood case, where one human runs everything
    let mut r4 = r;
    r4.operator_chain = vec!["lars".into()];
    assert_eq!(
        independent(&a, &r4),
        Err(Refusal::SharedOperator("lars".into()))
    );
    assert_eq!(vendor_of("agy/gemini-flash"), Some("google"));
    assert_eq!(vendor_of("openai/gpt-5"), Some("openai"));
    assert_eq!(vendor_of(""), None);
}

#[tokio::test]
async fn malformed_model_output_is_needs_human() {
    let f = fixture();
    let bad = [
        "I think it is fine.",
        "",
        r#"{"verdict":"approve"}"#,
        r#"{"verdict":"merge_now","findings":[],"confidence":90}"#,
        r#"{"verdict":"approve","findings":[],"confidence":90,"action":"effect.approve"}"#,
        r#"{"verdict":"approve","findings":[],"confidence":900}"#,
        r#"{"verdict":"approve","findings":[],"confidence":0.9}"#,
        r#"{"verdict":"approve","findings":[{"path":"elsewhere.rs","severity":"info","text":"x"}],"confidence":90}"#,
        r#"{"verdict":"approve","findings":[{"path":"a.rs","severity":"error","text":"bug"}],"confidence":90}"#,
        r#"{"verdict":"approve","findings":[],"confidence":90} and then some"#,
    ];
    for text in bad {
        let fake = FakeBackend::answering(text, Some("m"));
        let r = run(&f, config(), &fake).await.unwrap();
        assert_eq!(r.verdict, Verdict::NeedsHuman, "{text}");
        assert_eq!(r.confidence, 0, "{text}");
        assert!(!r.findings.is_empty(), "{text}");
    }
    // one fenced block is fine
    let fenced = format!("```json\n{APPROVE}\n```");
    let r = run(&f, config(), &FakeBackend::answering(&fenced, Some("m")))
        .await
        .unwrap();
    assert_eq!(r.verdict, Verdict::Approve);
}

#[tokio::test]
async fn truncation_oversize_and_an_unnamed_model_never_approve() {
    let f = fixture();
    let cut = FakeBackend::new(vec![Ok(ModelReply {
        text: APPROVE.into(),
        resolved_model: Some("m".into()),
        usage: Default::default(),
        truncated: true,
    })]);
    assert_eq!(
        run(&f, config(), &cut).await.unwrap().verdict,
        Verdict::NeedsHuman
    );

    let mut small = config();
    small.max_diff_bytes = 10;
    let fake = FakeBackend::answering(APPROVE, Some("m"));
    let r = run(&f, small, &fake).await.unwrap();
    assert_eq!(r.verdict, Verdict::NeedsHuman);
    assert!(fake.requests().is_empty(), "an oversize diff is not sent");

    let fake = FakeBackend::answering(APPROVE, None);
    let r = run(&f, config(), &fake).await.unwrap();
    assert_eq!(r.verdict, Verdict::NeedsHuman);
    assert_eq!(r.model_version, UNRESOLVED);

    let down = FakeBackend::new(vec![Err("HTTP 502".into())]);
    assert!(matches!(
        run(&f, config(), &down).await,
        Err(ReviewError::Backend(_))
    ));
}

#[tokio::test]
async fn off_by_default_tier_limited_and_authority_gated() {
    let f = fixture();
    let fake = FakeBackend::answering(APPROVE, Some("m"));
    let model = reviewer_info().model;
    let a = author();

    let off = ReviewerConfig::new(reviewer_info(), 16384);
    assert!(!off.enabled && !off.authority);
    let err = Reviewer::new(off, &model, &f.vcs, &fake)
        .unwrap()
        .review(&input(&f, &a, &f.digest))
        .await
        .unwrap_err();
    assert!(matches!(err, ReviewError::Disabled));

    let mut i = input(&f, &a, &f.digest);
    i.tier = 2;
    let r = Reviewer::new(config(), &model, &f.vcs, &fake).unwrap();
    assert!(matches!(r.review(&i).await, Err(ReviewError::Tier(2))));
    assert!(fake.requests().is_empty());

    assert!(
        Reviewer::new(
            ReviewerConfig::new(reviewer_info(), 0),
            &model,
            &f.vcs,
            &fake
        )
        .is_err()
    );

    // an approve is only an approval with authority on, Policy agreeing and enough confidence
    let rec = run(&f, config(), &FakeBackend::answering(APPROVE, Some("m")))
        .await
        .unwrap();
    let no_authority = Reviewer::new(config(), &model, &f.vcs, &fake).unwrap();
    assert_eq!(no_authority.disposition(&rec, true), Disposition::Post);
    let mut c = config();
    c.authority = true;
    let with = Reviewer::new(c, &model, &f.vcs, &fake).unwrap();
    assert_eq!(with.disposition(&rec, true), Disposition::Approve);
    assert_eq!(with.disposition(&rec, false), Disposition::Post);
    let mut weak = rec.clone();
    weak.confidence = 50;
    assert_eq!(with.disposition(&weak, true), Disposition::Post);
    let mut rc = rec;
    rc.verdict = Verdict::RequestChanges;
    assert_eq!(with.disposition(&rc, true), Disposition::Post);
}

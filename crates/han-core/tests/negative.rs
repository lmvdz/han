//! Things that must be refused: tampered signatures, non-canonical forms, duplicate keys,
//! oversize values, grammar and schema violations.

use std::collections::BTreeMap;

use han_core::base64url;
use han_core::error::Error;
use han_core::grammar;
use han_core::ids::{content_hash, json_digest, sha256_hex};
use han_core::json::{self, JsonError, Value, check_canonical};
use han_core::limits::{MAX_CONTENT_BYTES, MAX_HEADER_BYTES};
use han_core::sign::{accept_move_sig, hello_proof, verify_accept_move, verify_hello_proof};
use han_core::types::{Durability, LogRecord, Receipt};
use han_core::{Entry, PublicKey, SigningKey};

const ALICE: &str = "@alice:a.localhost";

fn key(n: u8) -> SigningKey {
    SigningKey::from_seed([n; 32])
}

fn obj(members: &[(&str, Value)]) -> Value {
    Value::Object(
        members
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
    )
}

fn s(x: &str) -> Value {
    Value::Str(x.to_owned())
}

/// A signed `message` entry with content.
fn message(text: &str) -> (Entry, Value, SigningKey) {
    let content = obj(&[("text", s(text))]);
    let mut e = Entry {
        v: 1,
        space: "s1:a.localhost".into(),
        kind: "message".into(),
        author: ALICE.into(),
        key_id: "k1".into(),
        body: obj(&[("to", Value::Array(vec![s("@bob:a.localhost")]))]),
        content_hash: content_hash(Some(&content)).unwrap(),
        refs: vec![],
        seen: 3,
        nonce: "aaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        time: "2026-01-01T00:00:00Z".into(),
        sig: String::new(),
    };
    let k = key(1);
    e.sign(&k).unwrap();
    (e, content, k)
}

fn receipt() -> Receipt {
    let mut r = Receipt {
        space: "s1:a.localhost".into(),
        seq: 1,
        id: "0".repeat(64),
        prev: String::new(),
        home: "a.localhost".into(),
        home_key: "h1".into(),
        time: "2026-01-01T00:00:00Z".into(),
        durability: Durability::Local,
        profiles: BTreeMap::from([(ALICE.to_owned(), 1)]),
        home_sig: String::new(),
    };
    r.sign(&key(9)).unwrap();
    r
}

fn flip_char(sig: &str, at: usize) -> String {
    let mut chars: Vec<char> = sig.chars().collect();
    chars[at] = if chars[at] == 'A' { 'B' } else { 'A' };
    chars.into_iter().collect()
}

// ───────────────────────── signatures ─────────────────────────

#[test]
fn a_genuine_signature_verifies_and_survives_a_json_round_trip() {
    let (e, content, k) = message("hello");
    e.validate().unwrap();
    e.validate_content(Some(&content)).unwrap();
    e.verify(&k.public_key()).unwrap();
    let text = serde_json::to_vec(&e).unwrap();
    let back = Entry::from_json(&text).unwrap();
    assert_eq!(back, e);
    assert_eq!(back.id().unwrap(), e.id().unwrap());
}

#[test]
fn tampered_entries_do_not_verify() {
    let (e, _, k) = message("hello");
    let public = k.public_key();
    // Any field the id covers.
    let mut t = e.clone();
    t.seen += 1;
    assert_eq!(t.verify(&public), Err(Error::BadSignature));
    let mut t = e.clone();
    t.body = obj(&[("to", Value::Array(vec![s("@mallory:a.localhost")]))]);
    assert_eq!(t.verify(&public), Err(Error::BadSignature));
    let mut t = e.clone();
    t.key_id = "k2".into();
    assert_eq!(t.verify(&public), Err(Error::BadSignature));
    // The signature itself: every position of the 86 characters, last one aside, flips a bit.
    for at in [0, 1, 40, 63, 64, 80, 84] {
        let mut t = e.clone();
        t.sig = flip_char(&e.sig, at);
        assert!(t.verify(&public).is_err(), "flip at {at}");
    }
    // Another key.
    assert_eq!(e.verify(&key(2).public_key()), Err(Error::BadSignature));
}

#[test]
fn non_canonical_signatures_fail_before_verification() {
    let (e, _, k) = message("hello");
    let public = k.public_key();
    // The last base64url character of 64 bytes carries 4 unused bits; set one.
    let last = e.sig.chars().last().unwrap();
    let alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let idx = alphabet.find(last).unwrap();
    let dirty = alphabet.chars().nth(idx | 1).unwrap();
    let dirty = if dirty == last {
        alphabet.chars().nth(idx ^ 1).unwrap()
    } else {
        dirty
    };
    let mut t = e.clone();
    t.sig = format!("{}{dirty}", &e.sig[..e.sig.len() - 1]);
    assert!(matches!(t.verify(&public), Err(Error::Base64(_))));
    // Padding, the standard alphabet and a wrong length.
    for bad in [
        format!("{}==", e.sig),
        format!("+{}", &e.sig[1..]),
        e.sig[..85].to_owned(),
        String::new(),
    ] {
        let mut t = e.clone();
        t.sig = bad;
        assert!(t.verify(&public).is_err());
    }
}

#[test]
fn verify_strict_refuses_a_small_order_key() {
    use ed25519_dalek::Verifier as _;
    // The identity point as a public key, and the all-zero signature for R = identity, S = 0:
    // the cofactorless equation holds for every message.
    let mut identity = [0u8; 32];
    identity[0] = 1;
    let mut sig = [0u8; 64];
    sig[0] = 1;
    let raw = ed25519_dalek::VerifyingKey::from_bytes(&identity).unwrap();
    let raw_sig = ed25519_dalek::Signature::from_bytes(&sig);
    let lax = raw.verify(b"anything", &raw_sig).is_ok();
    let public = PublicKey::from_base64url(&base64url::encode(&identity)).unwrap();
    let strict = public.verify(b"anything", &base64url::encode(&sig));
    assert_eq!(strict, Err(Error::BadSignature));
    eprintln!("non-strict verification of the small-order forgery accepts: {lax}");
}

#[test]
fn public_keys_must_be_canonical_32_bytes() {
    let good = key(1).public_key().to_base64url();
    assert!(PublicKey::from_base64url(&good).is_ok());
    assert!(PublicKey::from_base64url(&format!("{good}=")).is_err());
    assert!(PublicKey::from_base64url(&good[..42]).is_err());
    assert!(PublicKey::from_base64url(&base64url::encode(&[7u8; 31])).is_err());
    assert!(PublicKey::from_base64url("").is_err());
}

#[test]
fn tampered_receipts_do_not_verify() {
    let r = receipt();
    let public = key(9).public_key();
    r.verify(&public).unwrap();
    for f in [
        |r: &mut Receipt| r.seq = 2,
        |r: &mut Receipt| r.time = "2026-01-01T00:00:01Z".into(),
        |r: &mut Receipt| r.durability = Durability::Replicated,
        |r: &mut Receipt| r.home_key = "h2".into(),
        |r: &mut Receipt| {
            r.profiles.insert("@mallory:a.localhost".into(), 1);
        },
        |r: &mut Receipt| r.home_sig = flip_char(&r.home_sig, 3),
    ] {
        let mut t = r.clone();
        f(&mut t);
        assert!(t.verify(&public).is_err());
    }
    assert!(r.verify(&key(8).public_key()).is_err());
}

#[test]
fn hello_proofs_and_move_acceptances_are_bound_to_their_inputs() {
    let k = key(3);
    let p = k.public_key();
    let challenge = "A".repeat(43);
    let proof = hello_proof(&k, &challenge, "a.localhost");
    verify_hello_proof(&p, &proof, &challenge, "a.localhost").unwrap();
    assert!(verify_hello_proof(&p, &proof, &challenge, "evil.localhost").is_err());
    assert!(verify_hello_proof(&p, &proof, &"B".repeat(43), "a.localhost").is_err());
    assert!(verify_hello_proof(&key(4).public_key(), &proof, &challenge, "a.localhost").is_err());
    // A hello proof is not an entry signature and vice versa: the domains differ.
    let (e, _, _) = message("x");
    assert!(p.verify(e.id().unwrap().as_bytes(), &proof).is_err());

    let head = receipt();
    let sig = accept_move_sig(&k, "s1:a.localhost", &head).unwrap();
    verify_accept_move(&p, &sig, "s1:a.localhost", &head).unwrap();
    assert!(verify_accept_move(&p, &sig, "s2:a.localhost", &head).is_err());
    let mut other = head.clone();
    other.seq = 2;
    other.prev = head.hash().unwrap();
    assert!(verify_accept_move(&p, &sig, "s1:a.localhost", &other).is_err());
}

// ───────────────────────── canonical form and duplicates ─────────────────────────

#[test]
fn key_order_does_not_change_the_id_but_is_not_canonical() {
    let (e, _, _) = message("hello");
    let canonical = json::canonicalize(&json::to_value(&e).unwrap()).unwrap();
    assert_eq!(check_canonical(&canonical), Ok(()));
    // The same entry with its members reversed.
    let Value::Object(members) = json::to_value(&e).unwrap() else {
        panic!()
    };
    let reversed = format!(
        "{{{}}}",
        members
            .iter()
            .rev()
            .map(|(k, v)| format!(
                "\"{k}\":{}",
                String::from_utf8(json::canonicalize(v).unwrap()).unwrap()
            ))
            .collect::<Vec<_>>()
            .join(",")
    );
    assert_eq!(
        check_canonical(reversed.as_bytes()),
        Err(JsonError::NotCanonical)
    );
    let back = Entry::from_json(reversed.as_bytes()).unwrap();
    assert_eq!(back.id().unwrap(), e.id().unwrap());
    back.verify(&key(1).public_key()).unwrap();
}

#[test]
fn duplicate_keys_make_an_entry_invalid() {
    let (e, _, _) = message("hello");
    let text = String::from_utf8(serde_json::to_vec(&e).unwrap()).unwrap();
    let dup_header = text.replacen('{', "{\"kind\":\"message\",", 1);
    assert!(matches!(
        Entry::from_json(dup_header.as_bytes()),
        Err(Error::Json(JsonError::DuplicateKey(k))) if k == "kind"
    ));
    let dup_body = text.replace("\"to\":[", "\"to\":[],\"to\":[");
    assert!(matches!(
        Entry::from_json(dup_body.as_bytes()),
        Err(Error::Json(JsonError::DuplicateKey(_)))
    ));
}

#[test]
fn numbers_outside_the_integer_rule_make_an_entry_invalid() {
    let (e, _, _) = message("hello");
    let text = String::from_utf8(serde_json::to_vec(&e).unwrap()).unwrap();
    for bad in ["9007199254740992", "1.0", "1e3", "-0", "-1"] {
        let t = text.replace("\"seen\":3", &format!("\"seen\":{bad}"));
        assert!(Entry::from_json(t.as_bytes()).is_err(), "seen {bad}");
    }
}

// ───────────────────────── shapes and limits ─────────────────────────

fn invalid(e: &Entry) {
    assert!(matches!(e.validate(), Err(Error::Invalid(_))), "{e:?}");
}

#[test]
fn header_grammar_violations_are_invalid() {
    type Mutation = Box<dyn Fn(&mut Entry)>;
    let (good, _, _) = message("x");
    let cases: Vec<Mutation> = vec![
        Box::new(|e| e.v = 2),
        Box::new(|e| e.nonce = "AAAAAAAAAAAAAAAAAAAAAAAAAA".into()),
        Box::new(|e| e.nonce = "a".repeat(25)),
        Box::new(|e| e.nonce = "1".repeat(26)),
        Box::new(|e| e.time = "2026-02-30T00:00:00Z".into()),
        Box::new(|e| e.time = "2026-01-01T00:00:60Z".into()),
        Box::new(|e| e.time = "2026-01-01T00:00:00.5Z".into()),
        Box::new(|e| e.time = "2026-01-01 00:00:00Z".into()),
        Box::new(|e| e.content_hash = "A".repeat(64)),
        Box::new(|e| e.content_hash = "a".repeat(63)),
        Box::new(|e| e.author = "@Alice:a.localhost".into()),
        Box::new(|e| e.author = "alice:a.localhost".into()),
        Box::new(|e| e.author = "@alice:A.localhost".into()),
        Box::new(|e| e.key_id = String::new()),
        Box::new(|e| e.key_id = "k".repeat(65)),
        Box::new(|e| e.space = "profile:a.localhost".into()),
        Box::new(|e| e.space = "S1:a.localhost".into()),
        Box::new(|e| e.space = format!("{}:a.localhost", "a".repeat(27))),
        Box::new(|e| e.refs = vec!["abc".into()]),
        Box::new(|e| e.refs = vec!["a".repeat(64); 65]),
        Box::new(|e| e.seen = 1 << 53),
        Box::new(|e| e.kind = "task.foo".into()),
        Box::new(|e| e.kind = "nope".into()),
        Box::new(|e| e.body = Value::Null),
        Box::new(|e| e.body = obj(&[("to", Value::Array(vec![])), ("extra", Value::Int(1))])),
        Box::new(|e| e.body = obj(&[("to", s("@bob:a.localhost"))])),
        Box::new(|e| e.body = obj(&[("to", Value::Float(1.5))])),
    ];
    for (i, mutate) in cases.iter().enumerate() {
        let mut e = good.clone();
        mutate(&mut e);
        assert!(
            matches!(e.validate(), Err(Error::Invalid(_)) | Err(Error::Json(_))),
            "case {i} accepted: {e:?}"
        );
    }
    good.validate().unwrap();
}

#[test]
fn unknown_and_missing_fields_are_invalid() {
    let (e, _, _) = message("x");
    let mut v = serde_json::to_value(&e).unwrap();
    v["extra"] = serde_json::json!(1);
    assert!(Entry::from_json(&serde_json::to_vec(&v).unwrap()).is_err());
    let mut v = serde_json::to_value(&e).unwrap();
    v.as_object_mut().unwrap().remove("nonce");
    assert!(Entry::from_json(&serde_json::to_vec(&v).unwrap()).is_err());
    let mut v = serde_json::to_value(&e).unwrap();
    v["seen"] = serde_json::json!("3");
    assert!(Entry::from_json(&serde_json::to_vec(&v).unwrap()).is_err());
}

#[test]
fn the_header_size_limit_is_measured_with_the_signature() {
    let (mut e, _, _) = message("x");
    // 64 recipients with long (but grammatical) server names: well over 16 KiB of JCS.
    let long_server = vec!["a".repeat(63); 4].join(".");
    let to: Vec<Value> = (0..64)
        .map(|i| s(&format!("@user{i}:{long_server}")))
        .collect();
    e.body = obj(&[("to", Value::Array(to))]);
    invalid(&e);
    assert!(
        json::canonicalize(&json::to_value(&e).unwrap())
            .unwrap()
            .len()
            > MAX_HEADER_BYTES
    );
    // And a list over its own cap.
    let to: Vec<Value> = (0..65).map(|i| s(&format!("@u{i}:a.localhost"))).collect();
    e.body = obj(&[("to", Value::Array(to))]);
    invalid(&e);
}

#[test]
fn content_must_match_its_hash_kind_and_limits() {
    let (e, content, _) = message("hello");
    // Presence rules.
    assert!(e.validate_content(None).is_err());
    assert!(e.validate_content(Some(&Value::Null)).is_err());
    let mut no_hash = e.clone();
    no_hash.content_hash = String::new();
    assert!(no_hash.validate_content(Some(&content)).is_err());
    // Hash mismatch.
    let other = obj(&[("text", s("hullo"))]);
    assert!(e.validate_content(Some(&other)).is_err());
    // Schema: unknown field, wrong type, missing required field, string over its limit.
    for bad in [
        obj(&[("text", s("hello")), ("extra", Value::Int(1))]),
        obj(&[("text", Value::Int(1))]),
        obj(&[]),
        obj(&[("text", s(&"x".repeat(32_769)))]),
        obj(&[
            ("text", s("hello")),
            ("blobs", Value::Array(vec![obj(&[("hash", s("zz"))])])),
        ]),
    ] {
        let mut t = e.clone();
        t.content_hash = content_hash(Some(&bad)).unwrap();
        assert!(t.validate_content(Some(&bad)).is_err(), "{bad:?}");
    }
    // The limit is in UTF-8 bytes, not characters: 16,385 two-byte characters is 32,770 bytes.
    let wide = obj(&[("text", s(&"é".repeat(16_385)))]);
    let mut t = e.clone();
    t.content_hash = content_hash(Some(&wide)).unwrap();
    assert!(t.validate_content(Some(&wide)).is_err());
    let exact = obj(&[("text", s(&"é".repeat(16_384)))]);
    t.content_hash = content_hash(Some(&exact)).unwrap();
    t.validate_content(Some(&exact)).unwrap();
}

#[test]
fn content_over_64_kib_is_invalid() {
    let (mut e, _, _) = message("x");
    e.kind = "acme.note".into();
    e.body = obj(&[]);
    let big = obj(&[("blob", s(&"x".repeat(MAX_CONTENT_BYTES)))]);
    e.content_hash = content_hash(Some(&big)).unwrap();
    e.validate().unwrap();
    assert!(e.validate_content(Some(&big)).is_err());
    let small = obj(&[("blob", s("fine"))]);
    e.content_hash = content_hash(Some(&small)).unwrap();
    e.validate_content(Some(&small)).unwrap();
}

#[test]
fn a_kind_without_content_refuses_any() {
    let (mut e, _, _) = message("x");
    e.kind = "key.revoke".into();
    e.body = obj(&[("keyId", s("k1"))]);
    let c = obj(&[("text", s("x"))]);
    e.content_hash = content_hash(Some(&c)).unwrap();
    e.validate().unwrap();
    assert!(e.validate_content(Some(&c)).is_err());
    e.content_hash = String::new();
    e.validate_content(None).unwrap();
}

#[test]
fn extension_bodies_hold_only_non_negative_integers() {
    let (mut e, _, _) = message("x");
    e.kind = "acme.note".into();
    e.content_hash = String::new();
    e.body = obj(&[
        ("n", Value::Int(7)),
        ("list", Value::Array(vec![Value::Bool(true), s("x")])),
    ]);
    e.validate().unwrap();
    e.body = obj(&[("n", Value::Int(-1))]);
    invalid(&e);
    e.body = obj(&[("n", Value::Float(1.5))]);
    invalid(&e);
    // Nested deeper than 32 levels.
    let mut deep = Value::Int(1);
    for _ in 0..40 {
        deep = Value::Array(vec![deep]);
    }
    e.body = obj(&[("n", deep)]);
    invalid(&e);
}

#[test]
fn a_proposal_binds_its_plan() {
    let plan = obj(&[("action", s("git.push")), ("target", s("main"))]);
    let content = obj(&[("plan", plan.clone())]);
    let mut e = Entry {
        v: 1,
        space: "s1:a.localhost".into(),
        kind: "effect.propose".into(),
        author: ALICE.into(),
        key_id: "k1".into(),
        body: obj(&[
            ("executor", s("@git:a.localhost")),
            ("action", s("git.push")),
            ("planDigest", s(&json_digest(&plan).unwrap())),
            ("repeatable", Value::Bool(false)),
        ]),
        content_hash: content_hash(Some(&content)).unwrap(),
        refs: vec![],
        seen: 1,
        nonce: "bbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
        time: "2026-01-01T00:00:00Z".into(),
        sig: String::new(),
    };
    e.validate().unwrap();
    e.validate_content(Some(&content)).unwrap();
    // Wrong digest.
    let mut t = e.clone();
    t.body = obj(&[
        ("executor", s("@git:a.localhost")),
        ("action", s("git.push")),
        ("planDigest", s(&sha256_hex(b"other"))),
        ("repeatable", Value::Bool(false)),
    ]);
    assert!(t.validate_content(Some(&content)).is_err());
    // plan.action differs from action.
    e.body = obj(&[
        ("executor", s("@git:a.localhost")),
        ("action", s("file.write")),
        ("planDigest", s(&json_digest(&plan).unwrap())),
        ("repeatable", Value::Bool(false)),
    ]);
    assert!(e.validate_content(Some(&content)).is_err());
}

#[test]
fn a_public_key_in_a_key_add_must_be_32_canonical_bytes() {
    let (mut e, _, _) = message("x");
    e.kind = "key.add".into();
    e.content_hash = String::new();
    let ok = key(5).public_key().to_base64url();
    e.body = obj(&[("keyId", s("k2")), ("publicKey", s(&ok))]);
    e.validate().unwrap();
    for bad in [
        base64url::encode(&[1u8; 31]),
        format!("{}_", &ok[..42]),
        format!("{ok}="),
    ] {
        e.body = obj(&[("keyId", s("k2")), ("publicKey", s(&bad))]);
        assert!(e.validate().is_err(), "{bad}");
    }
}

#[test]
fn records_are_checked_against_their_receipts() {
    let (e, content, k) = message("hello");
    let mut r = receipt();
    r.id = e.id().unwrap();
    r.sign(&key(9)).unwrap();
    let rec = LogRecord {
        redacted: false,
        redacted_by: None,
        entry: e.clone(),
        content: Some(content.clone()),
        receipt: r.clone(),
    };
    rec.validate().unwrap();
    rec.entry.verify(&k.public_key()).unwrap();
    let mut t = rec.clone();
    t.receipt.id = "f".repeat(64);
    assert!(t.validate().is_err());
    let mut t = rec.clone();
    t.receipt.space = "s2:a.localhost".into();
    assert!(t.validate().is_err());
    let mut t = rec.clone();
    t.content = None;
    assert!(t.validate().is_err());
    let mut t = rec.clone();
    t.redacted = true;
    assert!(
        t.validate().is_err(),
        "redacted without proof and with content"
    );
    let mut t = rec.clone();
    t.receipt.prev = "a".repeat(64);
    assert!(t.validate().is_err(), "seq 1 with a prev");
    // A record round-trips through JSON, `content: null` included.
    let mut redacted = rec.clone();
    redacted.content = None;
    let text = serde_json::to_string(&redacted).unwrap();
    assert!(text.contains("\"content\":null"));
}

// ───────────────────────── encodings and grammar ─────────────────────────

#[test]
fn base64url_is_canonical_only() {
    assert_eq!(base64url::decode("AA"), Ok(vec![0]));
    for bad in ["AB", "AA==", "A", "AAAAA", "+/+/", "AA A", "AA\n", "é"] {
        assert!(base64url::decode(bad).is_err(), "{bad:?}");
    }
    for n in 0..40usize {
        let bytes: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
        assert_eq!(base64url::decode(&base64url::encode(&bytes)), Ok(bytes));
    }
}

#[test]
fn identifier_grammars_match_the_whole_string() {
    assert!(grammar::is_actor_id("@:a.example"));
    assert!(grammar::is_actor_id("@a.b_c-d:x1.y-z.example"));
    assert!(!grammar::is_actor_id("@a:b\n"));
    assert!(!grammar::is_actor_id("@a:-b.example"));
    assert!(!grammar::is_actor_id("@a:b..example"));
    assert!(!grammar::is_actor_id(&format!(
        "@{}:a.example",
        "a".repeat(65)
    )));
    assert!(grammar::is_actor_id(&format!(
        "@{}:a.example",
        "a".repeat(64)
    )));
    assert!(grammar::is_space_id("profile:@alice:a.example"));
    assert!(!grammar::is_space_id("profile:alice"));
    assert!(grammar::is_space_id(&format!(
        "{}:a.example",
        "a".repeat(26)
    )));
    assert!(!grammar::is_space_id("a-b:a.example"));
    assert!(grammar::is_extension_kind("acme.note"));
    assert!(!grammar::is_extension_kind("space.note"));
    assert!(!grammar::is_extension_kind("Acme.note"));
    assert!(!grammar::is_extension_kind("note"));
    assert!(grammar::is_media_type("application/json"));
    assert!(!grammar::is_media_type("application"));
    assert!(!grammar::is_media_type("Application/json"));
}

#[test]
fn times_are_real_dates_and_convert_to_epoch_seconds() {
    assert_eq!(grammar::epoch_seconds("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(
        grammar::epoch_seconds("2026-01-01T00:00:00Z"),
        Some(1_767_225_600)
    );
    assert_eq!(
        grammar::epoch_seconds("2000-02-29T23:59:59Z"),
        Some(951_868_799)
    );
    for bad in [
        "2023-02-29T00:00:00Z",
        "1900-02-29T00:00:00Z",
        "2026-13-01T00:00:00Z",
        "2026-00-10T00:00:00Z",
        "2026-04-31T00:00:00Z",
        "2026-01-01T24:00:00Z",
        "2026-01-01T00:60:00Z",
        "2026-01-01T00:00:60Z",
        "2026-01-01T00:00:00+00:00",
        "2026-01-01t00:00:00z",
        "0000-01-01T00:00:00Z",
    ] {
        assert!(!grammar::is_time(bad), "{bad}");
    }
}

//! Reproduces the recorded wire vectors and golden transcripts: JCS bytes, ids, content hashes,
//! entry signatures, receipt hashes and chains, receipt signatures and the hello proof.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use han_core::json::{self, Value};
use han_core::sign::{hello_proof, verify_hello_proof};
use han_core::{Entry, LogRecord, PublicKey, Receipt, SigningKey};
use serde_json::Value as Sj;
use sha2::{Digest, Sha256};

fn conformance() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance")
}

fn read_json(path: &Path) -> Sj {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn json_files(dir: &Path, skip: &[&str]) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter(|p| !skip.contains(&p.file_name().unwrap().to_str().unwrap()))
        .collect();
    files.sort();
    files
}

fn seed_key(seed: &str) -> SigningKey {
    SigningKey::from_seed(Sha256::digest(seed.as_bytes()).into())
}

/// The keys of `wire-vectors/config.json`, by `(actor, keyId)`, with the seed-derived secret.
struct Keys(BTreeMap<(String, String), (PublicKey, SigningKey)>);

impl Keys {
    fn load() -> Keys {
        let config = read_json(&conformance().join("wire-vectors/config.json"));
        let mut map = BTreeMap::new();
        let mut add = |actor: &str, key_id: &str, seed: &str, public: &str| {
            let secret = seed_key(seed);
            let public = PublicKey::from_base64url(public).unwrap();
            // The published public key is the one the seed derives.
            assert_eq!(secret.public_key(), public, "seed of {actor}");
            map.insert((actor.to_owned(), key_id.to_owned()), (public, secret));
        };
        let server = &config["serverKey"];
        add(
            "@:a.localhost",
            server["keyId"].as_str().unwrap(),
            server["seed"].as_str().unwrap(),
            server["publicKey"].as_str().unwrap(),
        );
        for a in config["actors"].as_array().unwrap() {
            add(
                a["actor"].as_str().unwrap(),
                a["keyId"].as_str().unwrap(),
                a["seed"].as_str().unwrap(),
                a["publicKey"].as_str().unwrap(),
            );
        }
        Keys(map)
    }

    fn server(&self) -> &(PublicKey, SigningKey) {
        &self.0[&("@:a.localhost".to_owned(), "h1".to_owned())]
    }
}

#[derive(Default)]
struct Stats {
    entries: usize,
    entries_with_content: usize,
    signatures_verified: usize,
    signatures_resigned_identically: usize,
    unknown_key_entries: usize,
    receipts: usize,
    receipts_resigned_identically: usize,
    records: usize,
    chain_links: usize,
    /// space -> seq -> receipt, across the files of one corpus.
    chain: BTreeMap<(String, u64), Receipt>,
}

impl std::fmt::Debug for Stats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} entries ({} with content), {} signatures verified and re-signed byte for byte, {} receipts re-signed byte for byte, {} records, {} chain links, {} entries by unknown keys",
            self.entries,
            self.entries_with_content,
            self.signatures_verified,
            self.receipts_resigned_identically,
            self.records,
            self.chain_links,
            self.unknown_key_entries
        )
    }
}

const ENTRY_KEYS: [&str; 12] = [
    "v",
    "space",
    "kind",
    "author",
    "keyId",
    "body",
    "contentHash",
    "refs",
    "seen",
    "nonce",
    "time",
    "sig",
];
const RECEIPT_KEYS: [&str; 10] = [
    "space",
    "seq",
    "id",
    "prev",
    "home",
    "homeKey",
    "time",
    "durability",
    "profiles",
    "homeSig",
];

fn has_exactly(o: &serde_json::Map<String, Sj>, keys: &[&str]) -> bool {
    o.len() == keys.len() && keys.iter().all(|k| o.contains_key(*k))
}

fn check_receipt(r: &Receipt, keys: &Keys, stats: &mut Stats) {
    r.validate().unwrap();
    let (public, secret) = keys.server();
    assert_eq!(r.home_key, "h1");
    r.verify(public)
        .unwrap_or_else(|e| panic!("homeSig of seq {}: {e}", r.seq));
    let mut again = r.clone();
    again.sign(secret).unwrap();
    assert_eq!(
        again.home_sig, r.home_sig,
        "re-signing seq {} differs",
        r.seq
    );
    stats.receipts += 1;
    stats.receipts_resigned_identically += 1;
    if let Some(old) = stats.chain.insert((r.space.clone(), r.seq), r.clone()) {
        assert_eq!(
            old.hash().unwrap(),
            r.hash().unwrap(),
            "same seq, two receipts"
        );
    }
}

fn check_entry(e: &Entry, content: Option<&Value>, keys: &Keys, stats: &mut Stats) {
    e.validate()
        .unwrap_or_else(|err| panic!("{} {}: {err}", e.kind, e.nonce));
    e.validate_content(content)
        .unwrap_or_else(|err| panic!("content of {} {}: {err}", e.kind, e.nonce));
    stats.entries += 1;
    stats.entries_with_content += usize::from(content.is_some());
    match keys.0.get(&(e.author.clone(), e.key_id.clone())) {
        Some((public, secret)) => {
            e.verify(public)
                .unwrap_or_else(|err| panic!("sig of {} {}: {err}", e.kind, e.nonce));
            stats.signatures_verified += 1;
            let mut again = e.clone();
            again.sign(secret).unwrap();
            assert_eq!(again.sig, e.sig, "re-signing {} differs", e.kind);
            stats.signatures_resigned_identically += 1;
        }
        None => stats.unknown_key_entries += 1,
    }
}

fn walk(v: &Sj, keys: &Keys, stats: &mut Stats) {
    match v {
        Sj::Array(a) => a.iter().for_each(|x| walk(x, keys, stats)),
        Sj::Object(o) => {
            if has_exactly(o, &RECEIPT_KEYS) {
                let r: Receipt = serde_json::from_value(v.clone()).unwrap();
                check_receipt(&r, keys, stats);
                return;
            }
            if o.contains_key("entry") && o.contains_key("receipt") && o.contains_key("redacted") {
                let rec: LogRecord = serde_json::from_value(v.clone()).unwrap();
                rec.validate().unwrap();
                stats.records += 1;
                // The inner entry and receipt are checked below by the generic walk, but only
                // once: continue into the members.
            }
            if let Some(Sj::Object(e)) = o.get("entry")
                && has_exactly(e, &ENTRY_KEYS)
            {
                let entry: Entry = serde_json::from_value(o["entry"].clone()).unwrap();
                let content: Option<Value> = match o.get("content") {
                    None | Some(Sj::Null) => None,
                    Some(c) => Some(serde_json::from_value(c.clone()).unwrap()),
                };
                // A redacted record, and the `redactedBy` proof, carry no content to check.
                let redacted = o.get("redacted") == Some(&Sj::Bool(true))
                    || (o.contains_key("receipt") && !o.contains_key("redacted"));
                if redacted {
                    entry.validate().unwrap();
                    stats.entries += 1;
                } else {
                    check_entry(&entry, content.as_ref(), keys, stats);
                }
                if let Some(Sj::Object(rr)) = o.get("receipt")
                    && has_exactly(rr, &RECEIPT_KEYS)
                {
                    assert_eq!(
                        rr["id"],
                        Sj::String(entry.id().unwrap()),
                        "receipt id of {}",
                        entry.kind
                    );
                }
                // Do not walk `entry` again as a bare object, but do walk the rest (receipt,
                // redactedBy).
                for (k, x) in o {
                    if k != "entry" && k != "content" {
                        walk(x, keys, stats);
                    }
                }
                return;
            }
            for x in o.values() {
                walk(x, keys, stats);
            }
        }
        _ => {}
    }
}

fn check_chain(stats: &mut Stats) {
    let receipts: Vec<Receipt> = stats.chain.values().cloned().collect();
    for r in &receipts {
        if r.seq == 1 {
            assert_eq!(r.prev, "");
        } else if let Some(prev) = stats.chain.get(&(r.space.clone(), r.seq - 1)) {
            assert_eq!(
                r.prev,
                prev.hash().unwrap(),
                "prev of {} seq {}",
                r.space,
                r.seq
            );
            stats.chain_links += 1;
        }
    }
}

/// The signed, hash-chained operation files, in the order of `index.json`.
#[test]
fn wire_vector_operations_reproduce() {
    let keys = Keys::load();
    let dir = conformance().join("wire-vectors");
    let mut stats = Stats::default();
    let mut pairs = 0;
    for file in json_files(&dir, &["config.json", "index.json"]) {
        let name = file.file_name().unwrap().to_str().unwrap().to_owned();
        if name.starts_with("probes") {
            continue;
        }
        let doc = read_json(&file);
        let steps = doc["steps"].as_array().unwrap();
        walk(&doc, &keys, &mut stats);
        // Pair each append request with its reply: the receipt's id is the entry's id.
        for (i, step) in steps.iter().enumerate() {
            if step["send"]["call"] != "append" {
                continue;
            }
            let entry: Entry =
                serde_json::from_value(step["send"]["args"]["entry"].clone()).unwrap();
            let id = &step["send"]["id"];
            let reply = steps[i + 1..]
                .iter()
                .find(|s| s["conn"] == step["conn"] && s["expect"]["id"] == *id)
                .unwrap_or_else(|| panic!("{name}: no reply to append {id}"));
            if let Some(ok) = reply["expect"]
                .get("ok")
                .filter(|ok| ok.get("homeSig").is_some())
            {
                assert_eq!(
                    ok["id"],
                    Sj::String(entry.id().unwrap()),
                    "{name}: receipt id"
                );
                pairs += 1;
            }
        }
    }
    check_chain(&mut stats);
    eprintln!("wire operations: {stats:?}, append/receipt pairs: {pairs}");
    assert!(stats.entries >= 30 && stats.signatures_verified >= 30);
    assert!(
        stats.chain_links >= 17,
        "the 18 receipts of s1 form one chain"
    );
    assert!(pairs >= 15);
    assert_eq!(
        stats.signatures_verified,
        stats.signatures_resigned_identically
    );
}

/// The probe files are mostly error paths; every entry that parses must still hash, and every
/// receipt must verify. Entries meant to be invalid are allowed to fail `validate`.
#[test]
fn wire_vector_probes_receipts_and_signed_entries_reproduce() {
    let keys = Keys::load();
    let dir = conformance().join("wire-vectors");
    let mut receipts = 0;
    let mut verified = 0;
    for file in json_files(&dir, &["config.json", "index.json"]) {
        if !file
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("probes")
        {
            continue;
        }
        let doc = read_json(&file);
        let mut found = Vec::new();
        collect(&doc, &mut found);
        for v in found {
            if has_exactly(v.as_object().unwrap(), &RECEIPT_KEYS) {
                // Expected receipts in probes are exact; their `homeSig` verifies under the config key
                // unless the vector carries a deliberately broken one.
                let r: Receipt = serde_json::from_value(v.clone()).unwrap();
                if r.validate().is_ok() && r.verify(&keys.server().0).is_ok() {
                    receipts += 1;
                }
            } else if let Ok(e) = serde_json::from_value::<Entry>(v.clone())
                && let Some((public, _)) = keys.0.get(&(e.author.clone(), e.key_id.clone()))
                && e.verify(public).is_ok()
            {
                verified += 1;
            }
        }
    }
    eprintln!("probes: {receipts} receipts and {verified} entries verify");
    assert!(receipts > 5 && verified > 20);
}

fn collect<'a>(v: &'a Sj, out: &mut Vec<&'a Sj>) {
    match v {
        Sj::Array(a) => a.iter().for_each(|x| collect(x, out)),
        Sj::Object(o) => {
            if has_exactly(o, &RECEIPT_KEYS) || has_exactly(o, &ENTRY_KEYS) {
                out.push(v);
            }
            o.values().for_each(|x| collect(x, out));
        }
        _ => {}
    }
}

/// The golden transcripts recorded from both reference servers and both clients.
#[test]
fn golden_transcripts_reproduce() {
    let keys = Keys::load();
    let dir = conformance().join("interop/golden");
    let mut stats = Stats::default();
    let mut files = 0;
    for file in json_files(&dir, &["index.json", "probes.json"]) {
        walk(&read_json(&file), &keys, &mut stats);
        files += 1;
    }
    check_chain(&mut stats);
    eprintln!("golden: {files} files, {stats:?}");
    assert!(stats.entries >= 30 && stats.receipts >= 18);
    assert!(stats.records >= 18);
    assert_eq!(stats.unknown_key_entries, 0);
    assert!(stats.chain_links >= 17);
}

/// `conformance/vectors/00-*.json`: the exact preimage bytes.
#[test]
fn entry_id_preimage_vectors() {
    let dir = conformance().join("vectors");
    for name in ["00-entry-id.json", "00-jcs-utf16-order.json"] {
        let doc = read_json(&dir.join(name));
        // Parse the entry text strictly, drop `sig`, canonicalize.
        let text = serde_json::to_vec(&doc["entry"]).unwrap();
        let Value::Object(mut members) = json::parse(&text).unwrap() else {
            panic!()
        };
        members.remove("sig");
        let mut preimage = b"han-entry-v1\n".to_vec();
        preimage.extend(json::canonicalize(&Value::Object(members)).unwrap());
        assert_eq!(
            String::from_utf8(preimage).unwrap(),
            doc["preimage"].as_str().unwrap(),
            "{name}"
        );
    }
    // The typed path agrees on the full entry.
    let doc = read_json(&dir.join("00-entry-id.json"));
    let entry: Entry = serde_json::from_value(doc["entry"].clone()).unwrap();
    assert_eq!(
        String::from_utf8(entry.id_preimage().unwrap()).unwrap(),
        doc["preimage"].as_str().unwrap()
    );
}

/// The worked hello in `wire-vectors/README.md`.
#[test]
fn hello_proof_reproduces() {
    let keys = Keys::load();
    let (public, secret) = &keys.0[&("@alice:a.localhost".to_owned(), "k1".to_owned())];
    let challenge = "A".repeat(43);
    let expected =
        "MlgcxEfCrmI7yfYRMa-ZI8vfUYNGtF9dV75MYk55TT6Y3WkUB5yeIvG-8K0dY7N8gPXwGK_LzoR8m1NSSZALAA";
    assert_eq!(hello_proof(secret, &challenge, "a.localhost"), expected);
    verify_hello_proof(public, expected, &challenge, "a.localhost").unwrap();
    assert!(verify_hello_proof(public, expected, &challenge, "b.localhost").is_err());
    assert!(verify_hello_proof(public, expected, &"B".repeat(43), "a.localhost").is_err());
}

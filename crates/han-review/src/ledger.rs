//! What the service has done, persisted so a restart neither reviews a proposal again nor loses the point it
//! had reached. One entry per space, proposal and reviewer version; the records themselves are kept as files
//! so a restart can finish an approval without asking the model again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use han_node::api::EvidenceRef;
use serde::{Deserialize, Serialize};

/// How far an entry got.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// The model answered and the record is on disk; not uploaded.
    Reviewed,
    /// The record blob is in the space; the approval or message is not sent.
    Uploaded,
    /// Nothing more to do for this proposal and version.
    Done,
}

/// One review.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// How far it got.
    pub stage: Stage,
    /// The record's blob hash (a file `records/<hash>.json` holds it); absent for a refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
    /// The blob reference, once uploaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<EvidenceRef>,
    /// What was done, once `Done`: `approved`, `messaged`, `recorded: …`, `refused: …`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// The persisted ledger.
pub struct Ledger {
    dir: PathBuf,
    entries: Mutex<BTreeMap<String, Entry>>,
}

/// The ledger's file name.
const FILE: &str = "ledger.json";

impl Ledger {
    /// Opens (or starts) the ledger under `dir`. A file that can't be read is an error: guessing would
    /// review everything again.
    pub fn open(dir: &Path) -> Result<Ledger, String> {
        std::fs::create_dir_all(dir.join("records"))
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join(FILE);
        let entries = match std::fs::read(&path) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        Ok(Ledger {
            dir: dir.to_path_buf(),
            entries: Mutex::new(entries),
        })
    }

    /// The key of a review.
    pub fn key(space: &str, proposal: &str, version: &str) -> String {
        format!("{space}|{proposal}|{version}")
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Entry>> {
        self.entries.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The entry of `key`.
    pub fn get(&self, key: &str) -> Option<Entry> {
        self.lock().get(key).cloned()
    }

    /// Sets the entry of `key` and writes the file (to a temporary name first, then renamed over it).
    pub fn put(&self, key: &str, entry: Entry) -> Result<(), String> {
        let mut g = self.lock();
        g.insert(key.to_owned(), entry);
        let bytes = serde_json::to_vec_pretty(&*g).map_err(|e| e.to_string())?;
        let tmp = self.dir.join(format!("{FILE}.tmp"));
        std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, self.dir.join(FILE))
            .map_err(|e| format!("{}: {e}", self.dir.join(FILE).display()))
    }

    /// How many entries there are.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// The file of a record, for a hash that is a sha256 in hex (nothing else names a path).
    fn record_path(&self, hash: &str) -> Result<PathBuf, String> {
        if hash.len() != 64 || !hash.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
            return Err("not a record hash".into());
        }
        Ok(self.dir.join("records").join(format!("{hash}.json")))
    }

    /// Keeps a record's bytes under its hash.
    pub fn save_record(&self, hash: &str, bytes: &[u8]) -> Result<(), String> {
        let p = self.record_path(hash)?;
        std::fs::write(&p, bytes).map_err(|e| format!("{}: {e}", p.display()))
    }

    /// The bytes of a record kept by [`Ledger::save_record`].
    pub fn load_record(&self, hash: &str) -> Result<Vec<u8>, String> {
        let p = self.record_path(hash)?;
        std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))
    }
}

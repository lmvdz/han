//! Storage behind a small trait: one SQLite file per space (log and receipts) and one `home.db` for the
//! rest of a home's data (blobs, uploads, stream cursors, invites). A format version is kept in every file.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use han_core::LogRecord;
use han_core::types::ActorId;
use rusqlite::{Connection, OptionalExtension, params};
use rusqlite_migration::{M, Migrations};
use sha2::{Digest, Sha256};

use crate::identity::PinView;

/// The storage format version written by this crate. A file with a higher one is refused.
pub const FORMAT_VERSION: u32 = 1;

/// Why storage failed.
#[derive(Debug, thiserror::Error)]
#[error("storage: {0}")]
pub struct StoreError(pub String);

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError(e.to_string())
    }
}

impl From<rusqlite_migration::Error> for StoreError {
    fn from(e: rusqlite_migration::Error) -> Self {
        StoreError(format!("migration: {e}"))
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        StoreError(format!("json: {e}"))
    }
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError(e.to_string())
    }
}

/// Storage results.
pub type StoreResult<T> = Result<T, StoreError>;

/// A record with the profile views its receipt pinned (R-10): everything needed to replay the fold.
#[derive(Clone, Debug)]
pub struct Stored {
    /// The record as served.
    pub record: LogRecord,
    /// The views pinned for it, by actor.
    pub pins: BTreeMap<ActorId, PinView>,
}

/// What a sequencer needs of storage. A write is durable when it returns (R-03). The key-value part holds
/// everything outside the logs, in namespaces of the sequencer's own choosing.
pub trait Store: Send + Sync {
    /// Every space with at least one record.
    fn spaces(&self) -> StoreResult<Vec<String>>;
    /// The records of `space` in seq order.
    fn load(&self, space: &str) -> StoreResult<Vec<Stored>>;
    /// Appends the next record of `space`.
    fn append(&self, space: &str, rec: &Stored) -> StoreResult<()>;
    /// Replaces the stored record of `space` at its seq (a redaction rewrites its content, R-04).
    fn replace(&self, space: &str, rec: &Stored) -> StoreResult<()>;
    /// A value from the key-value part.
    fn get(&self, ns: &str, key: &str) -> StoreResult<Option<Vec<u8>>>;
    /// Sets a value.
    fn put(&self, ns: &str, key: &str, value: &[u8]) -> StoreResult<()>;
    /// Removes a value; whether it existed.
    fn delete(&self, ns: &str, key: &str) -> StoreResult<bool>;
    /// Every `(key, value)` of a namespace.
    fn list(&self, ns: &str) -> StoreResult<Vec<(String, Vec<u8>)>>;
}

// ───────────────────────── memory ─────────────────────────

/// A store in memory, for tests and simulations.
#[derive(Default)]
pub struct MemStore {
    inner: Mutex<MemInner>,
}

#[derive(Default)]
struct MemInner {
    logs: HashMap<String, Vec<Stored>>,
    kv: HashMap<(String, String), Vec<u8>>,
}

impl MemStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MemInner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl Store for MemStore {
    fn spaces(&self) -> StoreResult<Vec<String>> {
        Ok(self.lock().logs.keys().cloned().collect())
    }

    fn load(&self, space: &str) -> StoreResult<Vec<Stored>> {
        Ok(self.lock().logs.get(space).cloned().unwrap_or_default())
    }

    fn append(&self, space: &str, rec: &Stored) -> StoreResult<()> {
        self.lock()
            .logs
            .entry(space.to_owned())
            .or_default()
            .push(rec.clone());
        Ok(())
    }

    fn replace(&self, space: &str, rec: &Stored) -> StoreResult<()> {
        let mut g = self.lock();
        let log = g
            .logs
            .get_mut(space)
            .ok_or_else(|| StoreError("no such space".into()))?;
        let at = usize::try_from(rec.record.receipt.seq).unwrap_or(0);
        *log.get_mut(at.wrapping_sub(1))
            .ok_or_else(|| StoreError("no such record".into()))? = rec.clone();
        Ok(())
    }

    fn get(&self, ns: &str, key: &str) -> StoreResult<Option<Vec<u8>>> {
        Ok(self
            .lock()
            .kv
            .get(&(ns.to_owned(), key.to_owned()))
            .cloned())
    }

    fn put(&self, ns: &str, key: &str, value: &[u8]) -> StoreResult<()> {
        self.lock()
            .kv
            .insert((ns.to_owned(), key.to_owned()), value.to_vec());
        Ok(())
    }

    fn delete(&self, ns: &str, key: &str) -> StoreResult<bool> {
        Ok(self
            .lock()
            .kv
            .remove(&(ns.to_owned(), key.to_owned()))
            .is_some())
    }

    fn list(&self, ns: &str) -> StoreResult<Vec<(String, Vec<u8>)>> {
        Ok(self
            .lock()
            .kv
            .iter()
            .filter(|((n, _), _)| n == ns)
            .map(|((_, k), v)| (k.clone(), v.clone()))
            .collect())
    }
}

// ───────────────────────── SQLite ─────────────────────────

const SPACE_SCHEMA: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE records (
    seq    INTEGER PRIMARY KEY,
    id     TEXT NOT NULL UNIQUE,
    record TEXT NOT NULL,
    pins   TEXT NOT NULL
);
INSERT INTO meta (key, value) VALUES ('format', '1');";

const HOME_SCHEMA: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE kv (
    ns    TEXT NOT NULL,
    key   TEXT NOT NULL,
    value BLOB NOT NULL,
    PRIMARY KEY (ns, key)
) WITHOUT ROWID;
INSERT INTO meta (key, value) VALUES ('format', '1');";

fn open_db(path: &Path, schema: &'static str) -> StoreResult<Connection> {
    let mut conn = Connection::open(path)?;
    // Durable at the stated level before a receipt leaves (R-03): "local" survives a process crash and a power cut.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    // The format version is the schema version SQLite keeps in the file (`user_version`); a newer one is refused.
    Migrations::new(vec![M::up(schema)]).to_latest(&mut conn)?;
    Ok(conn)
}

/// SQLite storage: `<dir>/home.db` and `<dir>/spaces/<hash of the space id>.db`. Leaving, moving or
/// deleting a space is a file operation (PLAN 6.3).
pub struct SqliteStore {
    dir: PathBuf,
    home: Mutex<Connection>,
    spaces: Mutex<HashMap<String, Connection>>,
}

impl SqliteStore {
    /// Opens (creating if needed) the store in `dir`.
    pub fn open(dir: &Path) -> StoreResult<Self> {
        std::fs::create_dir_all(dir.join("spaces"))?;
        Ok(SqliteStore {
            dir: dir.to_owned(),
            home: Mutex::new(open_db(&dir.join("home.db"), HOME_SCHEMA)?),
            spaces: Mutex::new(HashMap::new()),
        })
    }

    fn space_path(&self, space: &str) -> PathBuf {
        let digest = Sha256::digest(space.as_bytes());
        let name: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
        self.dir.join("spaces").join(format!("{name}.db"))
    }

    fn with_space<T>(
        &self,
        space: &str,
        create: bool,
        f: impl FnOnce(&mut Connection) -> StoreResult<T>,
    ) -> StoreResult<Option<T>> {
        let mut spaces = self.spaces.lock().unwrap_or_else(|p| p.into_inner());
        if !spaces.contains_key(space) {
            let path = self.space_path(space);
            if !create && !path.exists() {
                return Ok(None);
            }
            let conn = open_db(&path, SPACE_SCHEMA)?;
            conn.execute(
                "INSERT OR IGNORE INTO meta (key, value) VALUES ('space', ?1)",
                [space],
            )?;
            spaces.insert(space.to_owned(), conn);
        }
        match spaces.get_mut(space) {
            Some(conn) => f(conn).map(Some),
            None => Ok(None),
        }
    }
}

fn encode(rec: &Stored) -> StoreResult<(String, String, i64)> {
    Ok((
        serde_json::to_string(&rec.record)?,
        serde_json::to_string(&rec.pins)?,
        i64::try_from(rec.record.receipt.seq).map_err(|_| StoreError("seq".into()))?,
    ))
}

impl Store for SqliteStore {
    fn spaces(&self) -> StoreResult<Vec<String>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(self.dir.join("spaces"))? {
            let path = entry?.path();
            if path.extension().is_none_or(|e| e != "db") {
                continue;
            }
            let conn = open_db(&path, SPACE_SCHEMA)?;
            let space: Option<String> = conn
                .query_row("SELECT value FROM meta WHERE key = 'space'", [], |r| {
                    r.get(0)
                })
                .optional()?;
            let has_records: bool =
                conn.query_row("SELECT EXISTS (SELECT 1 FROM records)", [], |r| r.get(0))?;
            if let (Some(space), true) = (space, has_records) {
                out.push(space);
            }
        }
        out.sort();
        Ok(out)
    }

    fn load(&self, space: &str) -> StoreResult<Vec<Stored>> {
        self.with_space(space, false, |conn| {
            let mut stmt = conn.prepare("SELECT record, pins FROM records ORDER BY seq")?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (record, pins) = row?;
                out.push(Stored {
                    record: serde_json::from_str(&record)?,
                    pins: serde_json::from_str(&pins)?,
                });
            }
            Ok(out)
        })
        .map(Option::unwrap_or_default)
    }

    fn append(&self, space: &str, rec: &Stored) -> StoreResult<()> {
        let (record, pins, seq) = encode(rec)?;
        self.with_space(space, true, |conn| {
            conn.execute(
                "INSERT INTO records (seq, id, record, pins) VALUES (?1, ?2, ?3, ?4)",
                params![seq, rec.record.receipt.id, record, pins],
            )?;
            Ok(())
        })
        .map(|_| ())
    }

    fn replace(&self, space: &str, rec: &Stored) -> StoreResult<()> {
        let (record, pins, seq) = encode(rec)?;
        self.with_space(space, false, |conn| {
            conn.execute(
                "UPDATE records SET record = ?2, pins = ?3 WHERE seq = ?1",
                params![seq, record, pins],
            )?;
            Ok(())
        })
        .map(|_| ())
    }

    fn get(&self, ns: &str, key: &str) -> StoreResult<Option<Vec<u8>>> {
        let conn = self.home.lock().unwrap_or_else(|p| p.into_inner());
        Ok(conn
            .query_row(
                "SELECT value FROM kv WHERE ns = ?1 AND key = ?2",
                [ns, key],
                |r| r.get(0),
            )
            .optional()?)
    }

    fn put(&self, ns: &str, key: &str, value: &[u8]) -> StoreResult<()> {
        let conn = self.home.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute(
            "INSERT OR REPLACE INTO kv (ns, key, value) VALUES (?1, ?2, ?3)",
            params![ns, key, value],
        )?;
        Ok(())
    }

    fn delete(&self, ns: &str, key: &str) -> StoreResult<bool> {
        let conn = self.home.lock().unwrap_or_else(|p| p.into_inner());
        Ok(conn.execute("DELETE FROM kv WHERE ns = ?1 AND key = ?2", [ns, key])? > 0)
    }

    fn list(&self, ns: &str) -> StoreResult<Vec<(String, Vec<u8>)>> {
        let conn = self.home.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = conn.prepare("SELECT key, value FROM kv WHERE ns = ?1 ORDER BY key")?;
        let rows = stmt.query_map([ns], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

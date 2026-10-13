//! `node.db`: the device's SQLite file (PLAN 6.3). It holds the spaces this device follows with how far their
//! receipts are verified, a cache of the verified records (so views start without a network), the last
//! status item of each session, the outbox, and small config values. A format version is kept in the file
//! and migrations run on open.

use std::path::Path;
use std::sync::Mutex;

use han_core::LogRecord;
use han_core::json::Value;
use rusqlite::{Connection, OptionalExtension, params};
use rusqlite_migration::{M, Migrations};

use crate::api::{HomeRef, StatusBody};

/// The storage format version this crate writes. A file with a higher one is refused.
pub const FORMAT_VERSION: u32 = 1;

/// Why storage failed.
#[derive(Debug, thiserror::Error)]
#[error("node.db: {0}")]
pub struct DbError(pub String);

impl From<rusqlite::Error> for DbError {
    fn from(e: rusqlite::Error) -> Self {
        DbError(e.to_string())
    }
}

impl From<rusqlite_migration::Error> for DbError {
    fn from(e: rusqlite_migration::Error) -> Self {
        DbError(format!("migration: {e}"))
    }
}

impl From<serde_json::Error> for DbError {
    fn from(e: serde_json::Error) -> Self {
        DbError(format!("json: {e}"))
    }
}

type Result<T> = std::result::Result<T, DbError>;

/// Version 1. Later versions are added to `MIGRATIONS`, never edited.
const V1: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE config (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE follows (
    space     TEXT PRIMARY KEY,
    home      TEXT NOT NULL,
    head      INTEGER NOT NULL DEFAULT 0,
    tail_hash TEXT NOT NULL DEFAULT ''
) WITHOUT ROWID;
CREATE TABLE records (
    space  TEXT NOT NULL,
    seq    INTEGER NOT NULL,
    record TEXT NOT NULL,
    PRIMARY KEY (space, seq)
) WITHOUT ROWID;
CREATE TABLE status (
    space   TEXT NOT NULL,
    session TEXT NOT NULL,
    seq     INTEGER NOT NULL,
    body    TEXT NOT NULL,
    PRIMARY KEY (space, session)
) WITHOUT ROWID;
CREATE TABLE outbox (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    space    TEXT NOT NULL,
    entry_id TEXT NOT NULL UNIQUE,
    entry    TEXT NOT NULL,
    content  TEXT,
    state    TEXT NOT NULL DEFAULT 'pending',
    attempts INTEGER NOT NULL DEFAULT 0,
    error    TEXT,
    receipt  TEXT
);
CREATE INDEX outbox_pending ON outbox (space, id) WHERE state = 'pending';
INSERT INTO meta (key, value) VALUES ('format', '1');";

fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up(V1)])
}

/// A followed space, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Follow {
    /// The space.
    pub space: String,
    /// Its home.
    pub home: HomeRef,
    /// The last seq whose receipt this device verified.
    pub head: u64,
    /// The receipt hash of that seq (`""` before seq 1), the `prev` the next record must carry.
    pub tail_hash: String,
}

/// What became of an outbox row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboxState {
    /// Signed, not yet ordered by the home.
    Pending,
    /// Ordered; the receipt is stored.
    Sent,
    /// The home refused it for good.
    Rejected,
}

impl OutboxState {
    fn name(self) -> &'static str {
        match self {
            OutboxState::Pending => "pending",
            OutboxState::Sent => "sent",
            OutboxState::Rejected => "rejected",
        }
    }

    fn parse(s: &str) -> OutboxState {
        match s {
            "sent" => OutboxState::Sent,
            "rejected" => OutboxState::Rejected,
            _ => OutboxState::Pending,
        }
    }
}

/// An entry signed on this device and queued for its home.
#[derive(Clone, Debug)]
pub struct OutboxRow {
    /// Row id; rows go to the home in this order.
    pub id: u64,
    /// The space.
    pub space: String,
    /// The entry's id.
    pub entry_id: String,
    /// The signed entry, as the `append` call carries it.
    pub entry: Value,
    /// Its content.
    pub content: Option<Value>,
    /// Where it stands.
    pub state: OutboxState,
    /// Delivery attempts so far.
    pub attempts: u32,
}

/// The device database.
pub struct NodeDb {
    conn: Mutex<Connection>,
}

impl NodeDb {
    /// Opens (creating and migrating) `path`.
    pub fn open(path: &Path) -> Result<NodeDb> {
        Self::init(Connection::open(path)?)
    }

    /// An in-memory database, for tests.
    pub fn in_memory() -> Result<NodeDb> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<NodeDb> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        // The schema version SQLite keeps in the file (`user_version`) is the format version; a newer one is
        // refused by the migration runner.
        migrations().to_latest(&mut conn)?;
        Ok(NodeDb {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    // ───────────────────────── config ─────────────────────────

    /// A config value.
    pub fn config(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .lock()
            .query_row("SELECT value FROM config WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// Sets a config value.
    pub fn set_config(&self, key: &str, value: &str) -> Result<()> {
        self.lock().execute(
            "INSERT OR REPLACE INTO config (key, value) VALUES (?1, ?2)",
            [key, value],
        )?;
        Ok(())
    }

    // ───────────────────────── follows and records ─────────────────────────

    /// The homes joined by ticket, by ServerName; a stored ticket that no longer parses is skipped.
    pub fn home_pins(&self) -> Result<std::collections::HashMap<String, han_net::HomeTicket>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT value FROM config WHERE key LIKE 'home-pin/%'")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = std::collections::HashMap::new();
        for text in rows {
            if let Some(t) = han_net::HomeTicket::parse(&text?) {
                out.insert(t.server.clone(), t);
            }
        }
        Ok(out)
    }

    /// Every followed space.
    pub fn follows(&self) -> Result<Vec<Follow>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT space, home, head, tail_hash FROM follows ORDER BY space")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (space, home, head, tail_hash) = row?;
            out.push(Follow {
                space,
                home: serde_json::from_str(&home)?,
                head: u64::try_from(head).unwrap_or(0),
                tail_hash,
            });
        }
        Ok(out)
    }

    /// Starts following `space` (a no-op if it already is).
    pub fn add_follow(&self, space: &str, home: &HomeRef) -> Result<bool> {
        let n = self.lock().execute(
            "INSERT OR IGNORE INTO follows (space, home) VALUES (?1, ?2)",
            [space, &serde_json::to_string(home)?],
        )?;
        Ok(n > 0)
    }

    /// Stores a verified record and moves the space's verified head to it, in one transaction.
    pub fn put_record(&self, record: &LogRecord, tail_hash: &str) -> Result<()> {
        let seq = i64::try_from(record.receipt.seq).map_err(|_| DbError("seq".into()))?;
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO records (space, seq, record) VALUES (?1, ?2, ?3)",
            params![record.receipt.space, seq, serde_json::to_string(record)?],
        )?;
        tx.execute(
            "UPDATE follows SET head = ?2, tail_hash = ?3 WHERE space = ?1 AND head < ?2",
            params![record.receipt.space, seq, tail_hash],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The cached records of `space`, in seq order.
    pub fn records(&self, space: &str) -> Result<Vec<LogRecord>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT record FROM records WHERE space = ?1 ORDER BY seq")?;
        let rows = stmt.query_map([space], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(serde_json::from_str(&row?)?);
        }
        Ok(out)
    }

    // ───────────────────────── status ─────────────────────────

    /// Keeps the latest status item of a session.
    pub fn put_status(&self, space: &str, status: &StatusBody) -> Result<()> {
        let seq = i64::try_from(status.seq).map_err(|_| DbError("seq".into()))?;
        self.lock().execute(
            "INSERT INTO status (space, session, seq, body) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (space, session) DO UPDATE SET seq = ?3, body = ?4 WHERE ?3 > status.seq",
            params![space, status.session, seq, serde_json::to_string(status)?],
        )?;
        Ok(())
    }

    /// The stored status items of `space`.
    pub fn statuses(&self, space: &str) -> Result<Vec<StatusBody>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT body FROM status WHERE space = ?1")?;
        let rows = stmt.query_map([space], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(serde_json::from_str(&row?)?);
        }
        Ok(out)
    }

    // ───────────────────────── outbox ─────────────────────────

    /// Queues a signed entry; returns its row id. An entry already queued keeps its row.
    pub fn outbox_insert(
        &self,
        space: &str,
        entry_id: &str,
        entry: &Value,
        content: Option<&Value>,
    ) -> Result<u64> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR IGNORE INTO outbox (space, entry_id, entry, content) VALUES (?1, ?2, ?3, ?4)",
            params![
                space,
                entry_id,
                serde_json::to_string(entry)?,
                content.map(serde_json::to_string).transpose()?
            ],
        )?;
        let id: i64 = conn.query_row(
            "SELECT id FROM outbox WHERE entry_id = ?1",
            [entry_id],
            |r| r.get(0),
        )?;
        Ok(u64::try_from(id).unwrap_or(0))
    }

    /// Rows still waiting for the home, oldest first.
    pub fn outbox_pending(&self, space: &str) -> Result<Vec<OutboxRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, space, entry_id, entry, content, state, attempts FROM outbox
             WHERE space = ?1 AND state = 'pending' ORDER BY id",
        )?;
        let rows = stmt.query_map([space], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, space, entry_id, entry, content, state, attempts) = row?;
            out.push(OutboxRow {
                id: u64::try_from(id).unwrap_or(0),
                space,
                entry_id,
                entry: serde_json::from_str(&entry)?,
                content: content.map(|c| serde_json::from_str(&c)).transpose()?,
                state: OutboxState::parse(&state),
                attempts: u32::try_from(attempts).unwrap_or(0),
            });
        }
        Ok(out)
    }

    /// How many rows of `space` are pending.
    pub fn outbox_count(&self, space: &str) -> Result<u32> {
        let n: i64 = self.lock().query_row(
            "SELECT COUNT(*) FROM outbox WHERE space = ?1 AND state = 'pending'",
            [space],
            |r| r.get(0),
        )?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    }

    /// Notes a delivery attempt that did not reach a verdict.
    pub fn outbox_attempt(&self, id: u64, error: &str) -> Result<()> {
        self.lock().execute(
            "UPDATE outbox SET attempts = attempts + 1, error = ?2 WHERE id = ?1",
            params![i64::try_from(id).unwrap_or(0), error],
        )?;
        Ok(())
    }

    /// The home ordered the entry.
    pub fn outbox_sent(&self, id: u64, receipt: &str) -> Result<()> {
        self.lock().execute(
            "UPDATE outbox SET state = ?2, receipt = ?3, error = NULL WHERE id = ?1",
            params![
                i64::try_from(id).unwrap_or(0),
                OutboxState::Sent.name(),
                receipt
            ],
        )?;
        Ok(())
    }

    /// The home refused the entry for good.
    pub fn outbox_rejected(&self, id: u64, error: &str) -> Result<()> {
        self.lock().execute(
            "UPDATE outbox SET state = ?2, error = ?3 WHERE id = ?1",
            params![
                i64::try_from(id).unwrap_or(0),
                OutboxState::Rejected.name(),
                error
            ],
        )?;
        Ok(())
    }

    /// The format version recorded in the file.
    pub fn format(&self) -> Result<u32> {
        let v: String =
            self.lock()
                .query_row("SELECT value FROM meta WHERE key = 'format'", [], |r| {
                    r.get(0)
                })?;
        Ok(v.parse().unwrap_or(0))
    }
}

/// What the outbox knows of one row.
#[derive(Clone, Debug)]
pub struct OutboxInfo {
    /// `pending`, `sent` or `rejected`.
    pub state: String,
    /// The receipt as JSON, once sent.
    pub receipt: Option<String>,
    /// The last error.
    pub error: Option<String>,
}

impl NodeDb {
    /// One outbox row.
    pub fn outbox_get(&self, id: u64) -> Result<Option<OutboxInfo>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT state, receipt, error FROM outbox WHERE id = ?1",
                [i64::try_from(id).unwrap_or(0)],
                |r| {
                    Ok(OutboxInfo {
                        state: r.get(0)?,
                        receipt: r.get(1)?,
                        error: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// The newest `limit` outbox rows of `space`, oldest first, for the `Outbox` view.
    pub fn outbox_recent(&self, space: &str, limit: u32) -> Result<Vec<crate::views::OutboxItem>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, entry_id, entry, state, attempts, error, receipt FROM outbox
             WHERE space = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![space, limit], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, entry_id, entry, state, attempts, error, receipt) = row?;
            let kind = serde_json::from_str::<serde_json::Value>(&entry)
                .ok()
                .and_then(|e| e["kind"].as_str().map(str::to_owned))
                .unwrap_or_default();
            let seq = receipt
                .and_then(|r| serde_json::from_str::<serde_json::Value>(&r).ok())
                .and_then(|r| r["seq"].as_u64());
            out.push(crate::views::OutboxItem {
                id: u64::try_from(id).unwrap_or(0),
                entry_id,
                kind,
                state,
                attempts: u32::try_from(attempts).unwrap_or(0),
                seq,
                error,
            });
        }
        out.reverse();
        Ok(out)
    }

    /// Stops following `space`; its cached records go with it.
    pub fn remove_follow(&self, space: &str) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM follows WHERE space = ?1", [space])?;
        tx.execute("DELETE FROM records WHERE space = ?1", [space])?;
        tx.execute("DELETE FROM status WHERE space = ?1", [space])?;
        tx.commit()?;
        Ok(())
    }
}

//! The launch journal (X-01…X-06, SR-68): SQLite, keyed by [`IdempotencyKey`], written durably
//! (`synchronous=FULL`) before every transition. It is private to one executor instance and held by one
//! process at a time ([`InstanceLock`], X-07).
//!
//! States of an attempt, in order:
//!
//! | state | meaning |
//! |---|---|
//! | `claiming` | journalled before the claim's first send, with the elapsed-clock reading and its name (X-02) |
//! | `launching` | the claim's receipt is in hand and the plan checked again; the last step before launch (X-04) |
//! | `expired` | the deadline check failed after `launching`: never launched (X-04) |
//! | `result` | the attempt ended; the `(result, started)` to report is stored |
//! | `reported` | `effect.outcome` was appended (or the home refused it as already settled) |
//! | `refused` | the claim was refused, or never landed: there is nothing to report |
//!
//! The ledger never holds a credential: plans are stored as the proposal carried them, details are free text
//! written by actions that never put a token in them.

use std::fs::File;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use rusqlite_migration::{M, Migrations};
use serde::{Deserialize, Serialize};

use crate::IdempotencyKey;

/// The journal's format version (in `PRAGMA user_version` via the migrations, and in `meta`).
pub const FORMAT_VERSION: u32 = 1;

/// Why a ledger operation failed.
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    /// This attempt number was already used: invocation ids are never reused.
    #[error("attempt {0} was already journalled")]
    Exists(String),
    /// No such attempt.
    #[error("no attempt {0}")]
    Missing(String),
    /// A transition the journal does not allow.
    #[error("attempt {key} is {state}; it cannot become {to}")]
    Transition {
        /// The attempt.
        key: String,
        /// Its state.
        state: String,
        /// The refused state.
        to: String,
    },
    /// Another live process holds this instance (X-07).
    #[error("X-07: executor instance already held by a running process ({0})")]
    Held(String),
    /// SQLite.
    #[error("ledger: {0}")]
    Db(String),
    /// I/O.
    #[error("ledger io: {0}")]
    Io(#[from] std::io::Error),
}

impl From<rusqlite::Error> for LedgerError {
    fn from(e: rusqlite::Error) -> Self {
        LedgerError::Db(e.to_string())
    }
}

/// What `effect.outcome` says.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// `succeeded`, `failed`, `cancelled` or `unknown`.
    pub result: String,
    /// `yes`, `no` or `unknown`.
    pub started: String,
}

impl Report {
    /// A report.
    pub fn new(result: &str, started: &str) -> Self {
        Report {
            result: result.into(),
            started: started.into(),
        }
    }

    /// Not launched: `failed`, `no`.
    pub fn not_started() -> Self {
        Report::new("failed", "no")
    }

    /// Cannot tell: `unknown`, `unknown`.
    pub fn unknown() -> Self {
        Report::new("unknown", "unknown")
    }
}

/// One journalled attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attempt {
    /// Its key.
    pub key: IdempotencyKey,
    /// The action.
    pub action: String,
    /// The plan as JSON text.
    pub plan: String,
    /// The state (see the module documentation).
    pub state: String,
    /// The claim's entry id, once its receipt is in hand.
    pub claim_id: Option<String>,
    /// The elapsed clock's name.
    pub clock: String,
    /// Its reading at the claim's first send (X-02).
    pub sent_ms: u64,
    /// The claim's `window`, seconds.
    pub window: u64,
    /// The report, once there is one.
    pub report: Option<Report>,
    /// Free text from the last transition.
    pub detail: String,
}

impl Attempt {
    /// Whether nothing more will happen to it.
    pub fn is_final(&self) -> bool {
        matches!(self.state.as_str(), "reported" | "refused")
    }
}

fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up(
        "CREATE TABLE meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
         INSERT INTO meta VALUES ('format', '1');
         CREATE TABLE attempts (
             key TEXT PRIMARY KEY,
             space TEXT NOT NULL,
             proposal TEXT NOT NULL,
             attempt INTEGER NOT NULL,
             action TEXT NOT NULL,
             plan TEXT NOT NULL,
             state TEXT NOT NULL,
             claim_id TEXT,
             clock TEXT NOT NULL,
             sent_ms INTEGER NOT NULL,
             window INTEGER NOT NULL,
             report TEXT,
             detail TEXT NOT NULL DEFAULT ''
         );
         CREATE INDEX attempts_proposal ON attempts (space, proposal);
         CREATE TABLE journal (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             key TEXT NOT NULL,
             state TEXT NOT NULL,
             detail TEXT NOT NULL
         );",
    )])
}

/// The journal.
pub struct Ledger {
    conn: Connection,
}

impl Ledger {
    /// Opens (or creates) the journal at `path`.
    pub fn open(path: &Path) -> Result<Ledger, LedgerError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        migrations()
            .to_latest(&mut conn)
            .map_err(|e| LedgerError::Db(e.to_string()))?;
        Ok(Ledger { conn })
    }

    /// The format version stored in the file.
    pub fn format(&self) -> Result<u32, LedgerError> {
        let v: String = self
            .conn
            .query_row("SELECT v FROM meta WHERE k = 'format'", [], |r| r.get(0))?;
        Ok(v.parse().unwrap_or(0))
    }

    fn log(&self, key: &str, state: &str, detail: &str) -> Result<(), LedgerError> {
        self.conn.execute(
            "INSERT INTO journal (key, state, detail) VALUES (?1, ?2, ?3)",
            params![key, state, detail],
        )?;
        Ok(())
    }

    /// X-02: journals `claiming` before the claim's first send. Fails with [`LedgerError::Exists`] when the
    /// attempt number was used before.
    pub fn claiming(
        &self,
        key: &IdempotencyKey,
        action: &str,
        plan: &str,
        clock: &str,
        sent_ms: u64,
        window: u64,
    ) -> Result<(), LedgerError> {
        let k = key.as_string();
        let tx = self.conn.unchecked_transaction()?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO attempts
               (key, space, proposal, attempt, action, plan, state, clock, sent_ms, window)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'claiming', ?7, ?8, ?9)",
            params![
                k,
                key.space,
                key.proposal,
                key.attempt as i64,
                action,
                plan,
                clock,
                sent_ms as i64,
                window as i64
            ],
        )?;
        if inserted == 0 {
            // An attempt whose claim never landed (refused, or given up with no receipt) may be claimed again
            // under the same number: Policy accepts at most one claim per attempt number (E5), so the two can
            // never both land, and the claim entry ids stay distinct.
            let again = tx.execute(
                "UPDATE attempts SET state = 'claiming', action = ?2, plan = ?3, clock = ?4, sent_ms = ?5,
                        window = ?6, report = NULL, detail = ''
                 WHERE key = ?1 AND state = 'refused' AND claim_id IS NULL",
                params![k, action, plan, clock, sent_ms as i64, window as i64],
            )?;
            if again == 0 {
                return Err(LedgerError::Exists(k));
            }
        }
        tx.execute(
            "INSERT INTO journal (key, state, detail) VALUES (?1, 'claiming', ?2)",
            params![k, format!("clock={clock} reading={sent_ms}")],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn transition(
        &self,
        key: &IdempotencyKey,
        from: &[&str],
        to: &str,
        detail: &str,
    ) -> Result<(), LedgerError> {
        let k = key.as_string();
        let state = self
            .get(key)?
            .ok_or_else(|| LedgerError::Missing(k.clone()))?
            .state;
        if !from.contains(&state.as_str()) {
            return Err(LedgerError::Transition {
                key: k,
                state,
                to: to.into(),
            });
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE attempts SET state = ?2, detail = ?3 WHERE key = ?1",
            params![k, to, detail],
        )?;
        tx.execute(
            "INSERT INTO journal (key, state, detail) VALUES (?1, ?2, ?3)",
            params![k, to, detail],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The claim's receipt is in hand.
    pub fn claimed(&self, key: &IdempotencyKey, claim_id: &str) -> Result<(), LedgerError> {
        let k = key.as_string();
        self.conn.execute(
            "UPDATE attempts SET claim_id = ?2 WHERE key = ?1",
            params![k, claim_id],
        )?;
        self.log(&k, "claimed", claim_id)
    }

    /// X-04: the last journalled step before launching.
    pub fn launching(&self, key: &IdempotencyKey) -> Result<(), LedgerError> {
        self.transition(key, &["claiming"], "launching", "")
    }

    /// X-04: the deadline passed after `launching`; never launched.
    pub fn expired(&self, key: &IdempotencyKey, detail: &str) -> Result<(), LedgerError> {
        self.transition(key, &["launching"], "expired", detail)
    }

    /// The attempt ended with this report.
    pub fn result(
        &self,
        key: &IdempotencyKey,
        report: &Report,
        detail: &str,
    ) -> Result<(), LedgerError> {
        // `refused` too: a claim given up on (never answered) that landed after all is reported not started.
        self.transition(
            key,
            &["claiming", "launching", "expired", "refused"],
            "result",
            detail,
        )?;
        let r = serde_json::to_string(report).map_err(|e| LedgerError::Db(e.to_string()))?;
        self.conn.execute(
            "UPDATE attempts SET report = ?2 WHERE key = ?1",
            params![key.as_string(), r],
        )?;
        Ok(())
    }

    /// `effect.outcome` was appended.
    pub fn reported(&self, key: &IdempotencyKey, detail: &str) -> Result<(), LedgerError> {
        self.transition(key, &["result"], "reported", detail)
    }

    /// The claim was refused or never landed.
    pub fn refused(&self, key: &IdempotencyKey, detail: &str) -> Result<(), LedgerError> {
        self.transition(key, &["claiming"], "refused", detail)
    }

    /// One attempt.
    pub fn get(&self, key: &IdempotencyKey) -> Result<Option<Attempt>, LedgerError> {
        Ok(self
            .conn
            .query_row(
                "SELECT space, proposal, attempt, action, plan, state, claim_id, clock, sent_ms, window,
                        report, detail
                 FROM attempts WHERE key = ?1",
                params![key.as_string()],
                row_to_attempt,
            )
            .optional()?)
    }

    /// Attempts that are not final, oldest first.
    pub fn pending(&self) -> Result<Vec<Attempt>, LedgerError> {
        self.select(
            "WHERE state NOT IN ('reported', 'refused') ORDER BY rowid",
            [],
        )
    }

    /// Every attempt of one proposal, by attempt number.
    pub fn attempts_of(&self, space: &str, proposal: &str) -> Result<Vec<Attempt>, LedgerError> {
        self.select(
            "WHERE space = ?1 AND proposal = ?2 ORDER BY attempt",
            params![space, proposal],
        )
    }

    fn select(&self, tail: &str, p: impl rusqlite::Params) -> Result<Vec<Attempt>, LedgerError> {
        let mut st = self.conn.prepare(&format!(
            "SELECT space, proposal, attempt, action, plan, state, claim_id, clock, sent_ms, window,
                    report, detail
             FROM attempts {tail}"
        ))?;
        let rows = st.query_map(p, row_to_attempt)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// The journal of one attempt, in order: `(state, detail)` (X-10 evidence).
    pub fn journal(&self, key: &IdempotencyKey) -> Result<Vec<(String, String)>, LedgerError> {
        let mut st = self
            .conn
            .prepare("SELECT state, detail FROM journal WHERE key = ?1 ORDER BY id")?;
        let rows = st.query_map(params![key.as_string()], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

fn row_to_attempt(r: &rusqlite::Row<'_>) -> rusqlite::Result<Attempt> {
    let report: Option<String> = r.get(10)?;
    Ok(Attempt {
        key: IdempotencyKey {
            space: r.get(0)?,
            proposal: r.get(1)?,
            attempt: r.get::<_, i64>(2)? as u64,
        },
        action: r.get(3)?,
        plan: r.get(4)?,
        state: r.get(5)?,
        claim_id: r.get(6)?,
        clock: r.get(7)?,
        sent_ms: r.get::<_, i64>(8)? as u64,
        window: r.get::<_, i64>(9)? as u64,
        report: report.and_then(|s| serde_json::from_str(&s).ok()),
        detail: r.get(11)?,
    })
}

/// The exclusive hold of one executor instance's state (X-07): an OS file lock on `exec.lock`, released by the
/// OS when the holder dies, so a crashed process never blocks a restart. The file holds the holder's pid for
/// the refusal message.
pub struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    /// Takes the lock in `dir`, or refuses with [`LedgerError::Held`] (a message naming X-07).
    pub fn acquire(dir: &Path) -> Result<InstanceLock, LedgerError> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("exec.lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                let holder = std::fs::read_to_string(dir.join("exec.pid")).unwrap_or_default();
                return Err(LedgerError::Held(format!(
                    "pid {}",
                    holder.trim().if_empty("unknown")
                )));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        }
        std::fs::write(dir.join("exec.pid"), std::process::id().to_string())?;
        Ok(InstanceLock { _file: file })
    }
}

trait IfEmpty {
    fn if_empty<'a>(&'a self, other: &'a str) -> &'a str;
}

impl IfEmpty for str {
    fn if_empty<'a>(&'a self, other: &'a str) -> &'a str {
        if self.is_empty() { other } else { self }
    }
}

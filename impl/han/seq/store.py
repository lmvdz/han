"""The sequencer's SQLite storage (WAL, synchronous=FULL: a commit is on disk before the receipt leaves, R-03 "local").
Plain tables behind a few methods; nothing outside han.seq sees it."""
import json, sqlite3
from collections.abc import MutableMapping

SCHEMA = """
CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS records (space TEXT NOT NULL, seq INTEGER NOT NULL, id TEXT NOT NULL, entry TEXT NOT NULL, content TEXT,
  receipt TEXT NOT NULL, pinned TEXT NOT NULL, redacted INTEGER NOT NULL DEFAULT 0, redacted_by TEXT,
  PRIMARY KEY (space, seq), UNIQUE (space, id));
CREATE TABLE IF NOT EXISTS blobs (hash TEXT PRIMARY KEY, data BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS uploads (hash TEXT NOT NULL, actor TEXT NOT NULL, PRIMARY KEY (hash, actor));
CREATE TABLE IF NOT EXISTS supplied (space TEXT NOT NULL, hash TEXT NOT NULL, PRIMARY KEY (space, hash));
CREATE TABLE IF NOT EXISTS notices (n INTEGER PRIMARY KEY AUTOINCREMENT, actor TEXT NOT NULL, item TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS peer_records (space TEXT NOT NULL, seq INTEGER NOT NULL, record TEXT NOT NULL, PRIMARY KEY (space, seq));
CREATE TABLE IF NOT EXISTS stream_items (space TEXT NOT NULL, session TEXT NOT NULL, n INTEGER NOT NULL, item TEXT NOT NULL,
  PRIMARY KEY (space, session, n));
"""
MAX_NOTICES = 256          # per actor; notices are hints (G-03), so the oldest may go

def _j(v): return json.dumps(v, separators=(",", ":"), ensure_ascii=False)

class Store:
    def __init__(self, path: str):
        self.db = sqlite3.connect(path)
        self.db.execute("PRAGMA journal_mode=WAL")
        self.db.execute("PRAGMA synchronous=FULL")
        self.db.executescript(SCHEMA)
        self.db.commit()

    def commit(self): self.db.commit()
    def close(self): self.db.close()

    # meta
    def get_meta(self, k, default=None):
        r = self.db.execute("SELECT v FROM meta WHERE k=?", (k,)).fetchone()
        return json.loads(r[0]) if r else default
    def set_meta(self, k, v): self.db.execute("INSERT OR REPLACE INTO meta VALUES (?,?)", (k, _j(v)))

    # records (not committed: the caller commits once per append)
    def add_record(self, rec: dict, pinned: dict):
        e, r = rec["entry"], rec["receipt"]
        self.db.execute("INSERT INTO records (space, seq, id, entry, content, receipt, pinned) VALUES (?,?,?,?,?,?,?)",
                        (r["space"], r["seq"], r["id"], _j(e), None if rec["content"] is None else _j(rec["content"]), _j(r), _j(pinned)))
    def mark_redacted(self, space: str, rid: str, proof: dict):
        self.db.execute("UPDATE records SET content=NULL, redacted=1, redacted_by=? WHERE space=? AND id=?", (_j(proof), space, rid))
    def spaces(self) -> list[str]:
        return [r[0] for r in self.db.execute("SELECT DISTINCT space FROM records ORDER BY space")]
    def records(self, space: str):
        """Yield (record, pinned) in seq order."""
        for e, c, r, p, red, rb in self.db.execute(
                "SELECT entry, content, receipt, pinned, redacted, redacted_by FROM records WHERE space=? ORDER BY seq", (space,)):
            rec = {"entry": json.loads(e), "content": None if c is None else json.loads(c), "redacted": bool(red), "receipt": json.loads(r)}
            if rb: rec["redactedBy"] = json.loads(rb)
            yield rec, json.loads(p)

    # blobs, uploads, supply (G-01)
    def add_upload(self, h, actor): self.db.execute("INSERT OR IGNORE INTO uploads VALUES (?,?)", (h, actor))
    def add_supplied(self, space, h): self.db.execute("INSERT OR IGNORE INTO supplied VALUES (?,?)", (space, h))
    def uploads(self) -> dict:
        out = {}
        for h, a in self.db.execute("SELECT hash, actor FROM uploads"): out.setdefault(h, set()).add(a)
        return out
    def supplied(self) -> set:
        return set(self.db.execute("SELECT space, hash FROM supplied"))

    # notices (G-03): the actor's inbox, kept until the actor reads its profile
    def add_notice(self, actor, item):
        self.db.execute("INSERT INTO notices (actor, item) VALUES (?,?)", (actor, _j(item)))
        self.db.execute("DELETE FROM notices WHERE actor=? AND n NOT IN (SELECT n FROM notices WHERE actor=? ORDER BY n DESC LIMIT ?)",
                        (actor, actor, MAX_NOTICES))
    def notices(self) -> dict:
        out = {}
        for a, i in self.db.execute("SELECT actor, item FROM notices ORDER BY n"): out.setdefault(a, []).append(json.loads(i))
        return out

    # verified profile records of actors on other servers (federation, §4.3)
    def add_peer_record(self, rec: dict):
        r = rec["receipt"]
        self.db.execute("INSERT OR IGNORE INTO peer_records VALUES (?,?,?)", (r["space"], r["seq"], _j(rec)))
    def peer_records(self) -> list[dict]:
        return [json.loads(r) for (r,) in self.db.execute("SELECT record FROM peer_records ORDER BY space, seq")]

    # session streams (G-05)
    def add_stream_item(self, space, session, n, item):
        self.db.execute("INSERT INTO stream_items VALUES (?,?,?,?)", (space, session, n, _j(item)))
    def stream_items(self, space, session) -> list:
        return [json.loads(i) for (i,) in self.db.execute("SELECT item FROM stream_items WHERE space=? AND session=? ORDER BY n", (space, session))]
    def last_stream_n(self, space, session) -> int:
        r = self.db.execute("SELECT MAX(n) FROM stream_items WHERE space=? AND session=?", (space, session)).fetchone()
        return r[0] or 0

class BlobStore(MutableMapping):
    """hash → bytes, backed by the blobs table (Home.blobs is used as a dict). Writes are committed by the caller."""
    def __init__(self, store: Store): self.s = store
    def __getitem__(self, h):
        r = self.s.db.execute("SELECT data FROM blobs WHERE hash=?", (h,)).fetchone()
        if r is None: raise KeyError(h)
        return bytes(r[0])
    def __setitem__(self, h, data): self.s.db.execute("INSERT OR REPLACE INTO blobs VALUES (?,?)", (h, bytes(data)))
    def __delitem__(self, h): self.s.db.execute("DELETE FROM blobs WHERE hash=?", (h,))
    def __contains__(self, h): return self.s.db.execute("SELECT 1 FROM blobs WHERE hash=?", (h,)).fetchone() is not None
    def __iter__(self): return (r[0] for r in self.s.db.execute("SELECT hash FROM blobs"))
    def __len__(self): return self.s.db.execute("SELECT COUNT(*) FROM blobs").fetchone()[0]

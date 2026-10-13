"""The client's durable outbox (R-07): every signed entry is stored, fsync'd, before its first send, and stays until it has a
receipt or a definitive refusal."""
import json, sqlite3

SCHEMA = """CREATE TABLE IF NOT EXISTS outbox (n INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, space TEXT NOT NULL,
  entry TEXT NOT NULL, content TEXT, state TEXT NOT NULL DEFAULT 'pending', receipt TEXT, error TEXT)"""

def _j(v): return json.dumps(v, separators=(",", ":"), ensure_ascii=False)

class Outbox:
    def __init__(self, path: str):
        self.db = sqlite3.connect(path)
        self.db.execute("PRAGMA journal_mode=WAL"); self.db.execute("PRAGMA synchronous=FULL")
        self.db.execute(SCHEMA); self.db.commit()

    def add(self, eid: str, space: str, entry: dict, content) -> dict:
        """Store the entry before it is sent; a second add of the same id is the same row."""
        self.db.execute("INSERT OR IGNORE INTO outbox (id, space, entry, content) VALUES (?,?,?,?)",
                        (eid, space, _j(entry), None if content is None else _j(content)))
        self.db.commit()
        return self.get(eid)

    def get(self, eid: str) -> dict | None:
        r = self.db.execute("SELECT id, space, entry, content, state, receipt, error FROM outbox WHERE id=?", (eid,)).fetchone()
        return None if r is None else {"id": r[0], "space": r[1], "entry": json.loads(r[2]), "content": None if r[3] is None else json.loads(r[3]),
                                       "state": r[4], "receipt": json.loads(r[5]) if r[5] else None, "error": json.loads(r[6]) if r[6] else None}

    def pending(self) -> list[dict]:
        return [self.get(i) for (i,) in self.db.execute("SELECT id FROM outbox WHERE state='pending' ORDER BY n").fetchall()]

    def done(self, eid: str, receipt: dict):
        self.db.execute("UPDATE outbox SET state='done', receipt=? WHERE id=?", (_j(receipt), eid)); self.db.commit()

    def rejected(self, eid: str, error: dict):
        self.db.execute("UPDATE outbox SET state='rejected', error=? WHERE id=?", (_j(error), eid)); self.db.commit()

    def close(self): self.db.close()

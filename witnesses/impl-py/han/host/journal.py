"""The host's durable state (R-18): the cursor over the followed space, the session id, the intents decided but not yet
delivered, the stream counter and the transcript. One SQLite file, fsync'd. A turn's intents and its cursor move are written in
one transaction, so after a crash a trigger is either fully decided (its intents are delivered, never decided again) or not at
all (it is run again and nothing was ever proposed)."""
import json, sqlite3

SCHEMA = """
CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS intents (n INTEGER PRIMARY KEY AUTOINCREMENT, key TEXT NOT NULL UNIQUE, kind TEXT NOT NULL,
  payload TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'pending', result TEXT);
CREATE TABLE IF NOT EXISTS stream (n INTEGER PRIMARY KEY, item TEXT NOT NULL, sent INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS transcript (i INTEGER PRIMARY KEY AUTOINCREMENT, line TEXT NOT NULL);
"""

def _j(v): return json.dumps(v, separators=(",", ":"), ensure_ascii=False)

class TaintStore:
    """R-24, T5: what the host keeps per AGENT, not per session or space: whether the agent has taken an outside instruction (in any of
    its sessions, in any space), and the spaces in which that taint has been seen with the agent guarded (strict from then on, also
    after an unguard). One small SQLite file that every host of the agent shares (one row per fact, so two processes don't overwrite
    each other); the log holds nothing for it."""
    def __init__(self, path: str):
        self.db = sqlite3.connect(path, timeout=10)
        self.db.execute("PRAGMA journal_mode=WAL"); self.db.execute("PRAGMA synchronous=FULL")
        self.db.executescript("CREATE TABLE IF NOT EXISTS taint (k INTEGER PRIMARY KEY CHECK (k = 1));"
                              "CREATE TABLE IF NOT EXISTS armed (space TEXT PRIMARY KEY);"); self.db.commit()

    def tainted(self) -> bool: return self.db.execute("SELECT 1 FROM taint").fetchone() is not None
    def taint(self):
        with self.db: self.db.execute("INSERT OR IGNORE INTO taint (k) VALUES (1)")
    def armed(self, space: str) -> bool: return self.db.execute("SELECT 1 FROM armed WHERE space=?", (space,)).fetchone() is not None
    def arm(self, space: str):
        with self.db: self.db.execute("INSERT OR IGNORE INTO armed (space) VALUES (?)", (space,))
    def close(self): self.db.close()

class Journal:
    def __init__(self, path: str):
        self.db = sqlite3.connect(path)
        self.db.execute("PRAGMA journal_mode=WAL"); self.db.execute("PRAGMA synchronous=FULL")
        self.db.executescript(SCHEMA); self.db.commit()

    # ── meta ──
    def get(self, k, default=None):
        r = self.db.execute("SELECT v FROM meta WHERE k=?", (k,)).fetchone()
        return default if r is None else json.loads(r[0])

    def put(self, k, v):
        self.db.execute("INSERT OR REPLACE INTO meta (k, v) VALUES (?,?)", (k, _j(v))); self.db.commit()

    # ── intents ──
    def commit_turn(self, cursor: int, intents: list[tuple[str, str, dict]]):
        """Atomically: move the cursor to `cursor` and store `intents` [(key, kind, payload)]. A key that exists is skipped, so the
        same decision can never be stored twice."""
        with self.db:
            for key, kind, payload in intents:
                self.db.execute("INSERT OR IGNORE INTO intents (key, kind, payload) VALUES (?,?,?)", (key, kind, _j(payload)))
            self.db.execute("INSERT OR REPLACE INTO meta (k, v) VALUES ('cursor', ?)", (_j(cursor),))

    def cursor(self) -> int: return self.get("cursor", 0)

    def pending(self) -> list[dict]:
        return [self._intent(r) for r in self.db.execute(
            "SELECT key, kind, payload, state, result FROM intents WHERE state='pending' ORDER BY n").fetchall()]

    def intents(self) -> list[dict]:
        return [self._intent(r) for r in self.db.execute("SELECT key, kind, payload, state, result FROM intents ORDER BY n").fetchall()]

    @staticmethod
    def _intent(r): return {"key": r[0], "kind": r[1], "payload": json.loads(r[2]), "state": r[3], "result": json.loads(r[4]) if r[4] else None}

    def settle(self, key: str, state: str, result=None):
        self.db.execute("UPDATE intents SET state=?, result=? WHERE key=?", (state, None if result is None else _j(result), key)); self.db.commit()

    # ── session stream (G-05: n is one more than the last item; a resend of the last item is harmless) ──
    def add_item(self, make) -> dict:
        """Allocate the next n, build the item with `make(n)` and store it unsent, in one transaction."""
        with self.db:
            n = (self.db.execute("SELECT MAX(n) FROM stream").fetchone()[0] or 0) + 1
            item = make(n)
            self.db.execute("INSERT INTO stream (n, item) VALUES (?,?)", (n, _j(item)))
        return item

    def unsent_items(self) -> list[dict]:
        return [json.loads(i) for (i,) in self.db.execute("SELECT item FROM stream WHERE sent=0 ORDER BY n").fetchall()]

    def sent(self, n: int):
        self.db.execute("UPDATE stream SET sent=1 WHERE n=?", (n,)); self.db.commit()

    # ── transcript ──
    def log(self, line: str):
        self.db.execute("INSERT INTO transcript (line) VALUES (?)", (line,)); self.db.commit()

    def transcript(self) -> str:
        return "\n".join(l for (l,) in self.db.execute("SELECT line FROM transcript ORDER BY i").fetchall())

    def close(self): self.db.close()

"""The appservice's own SQLite state: which room is bound to which space, which Matrix events carried which entry, how far each
space is published to its room, and the session rooms. (The log itself is han.seq's store.)"""
import sqlite3

SCHEMA = """
CREATE TABLE IF NOT EXISTS rooms (room TEXT PRIMARY KEY, space TEXT NOT NULL UNIQUE);
CREATE TABLE IF NOT EXISTS events (event TEXT PRIMARY KEY, room TEXT NOT NULL, entry TEXT NOT NULL, n INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS events_entry ON events (entry);
CREATE TABLE IF NOT EXISTS published (space TEXT PRIMARY KEY, seq INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS sessions (space TEXT NOT NULL, session TEXT NOT NULL, watch TEXT NOT NULL, read TEXT NOT NULL,
  done INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (space, session));
"""

class AsStore:
    def __init__(self, path: str):
        self.db = sqlite3.connect(path)
        self.db.execute("PRAGMA journal_mode=WAL"); self.db.execute("PRAGMA synchronous=FULL")
        self.db.executescript(SCHEMA); self.db.commit()

    def room_space(self, room):
        r = self.db.execute("SELECT space FROM rooms WHERE room=?", (room,)).fetchone(); return r and r[0]
    def space_room(self, space):
        r = self.db.execute("SELECT room FROM rooms WHERE space=?", (space,)).fetchone(); return r and r[0]
    def bind(self, room, space):
        self.db.execute("INSERT OR IGNORE INTO rooms VALUES (?,?)", (room, space)); self.db.commit()
    def spaces(self): return [r[0] for r in self.db.execute("SELECT space FROM rooms")]

    def add_event(self, event, room, entry):
        self.db.execute("INSERT OR IGNORE INTO events (event, room, entry, n) VALUES (?,?,?,(SELECT COUNT(*) FROM events))", (event, room, entry))
        self.db.commit()
    def origin(self, entry):
        """The first Matrix event that carried `entry` (the one its record's receipt is related to)."""
        r = self.db.execute("SELECT event FROM events WHERE entry=? ORDER BY n LIMIT 1", (entry,)).fetchone(); return r and r[0]
    def events_of(self, entry): return [r[0] for r in self.db.execute("SELECT event FROM events WHERE entry=? ORDER BY n", (entry,))]

    def published(self, space):
        r = self.db.execute("SELECT seq FROM published WHERE space=?", (space,)).fetchone(); return r[0] if r else 0
    def set_published(self, space, seq):
        self.db.execute("INSERT INTO published VALUES (?,?) ON CONFLICT(space) DO UPDATE SET seq=excluded.seq", (space, seq)); self.db.commit()

    def session(self, space, session):
        r = self.db.execute("SELECT watch, read, done FROM sessions WHERE space=? AND session=?", (space, session)).fetchone()
        return None if r is None else {"watch": r[0], "read": r[1], "done": bool(r[2])}
    def add_session(self, space, session, watch, read):
        self.db.execute("INSERT OR IGNORE INTO sessions (space, session, watch, read) VALUES (?,?,?,?)", (space, session, watch, read)); self.db.commit()
    def finish_session(self, space, session):
        self.db.execute("UPDATE sessions SET done=1 WHERE space=? AND session=?", (space, session)); self.db.commit()

    def close(self): self.db.close()

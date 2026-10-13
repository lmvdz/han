"""The executor's launch journal (X-01, X-02, X-07): SQLite with synchronous=FULL, private to one instance, behind an exclusive
OS-level lock. A marker file next to the key records which journal the key was last used with, so a key restored without its
journal (or with a different one) refuses to start."""
from __future__ import annotations
import json, os, sqlite3, time, uuid

class StartRefused(Exception):
    """The instance may not start (X-07)."""

def elapsed_clock_name() -> str:
    """Which clock `elapsed_clock` reads, for the start-up log (X-02)."""
    if hasattr(time, "CLOCK_BOOTTIME"): return "CLOCK_BOOTTIME"
    return "time.monotonic (QueryPerformanceCounter on Windows, mach_continuous_time-backed on macOS)"

def elapsed_clock():
    """The elapsed-time clock of X-02: it must keep counting through suspend. CLOCK_BOOTTIME where the platform has it,
    otherwise time.monotonic (on Windows the QueryPerformanceCounter-based clock)."""
    if hasattr(time, "CLOCK_BOOTTIME"): return lambda: time.clock_gettime(time.CLOCK_BOOTTIME)
    return time.monotonic

def _lock(fh):
    if os.name == "nt":
        import msvcrt
        msvcrt.locking(fh.fileno(), msvcrt.LK_NBLCK, 1)
    else:
        import fcntl
        fcntl.flock(fh.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)

def _unlock(fh):
    try:
        if os.name == "nt":
            import msvcrt
            fh.seek(0); msvcrt.locking(fh.fileno(), msvcrt.LK_UNLCK, 1)
        else:
            import fcntl
            fcntl.flock(fh.fileno(), fcntl.LOCK_UN)
    except OSError: pass

class Journal:
    def __init__(self, path: str, key_id: str, marker_path: str):
        self.path, self.key_id, self.marker_path = path, key_id, marker_path
        self._lockfh = self.db = None
        os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
        self._lockfh = open(path + ".lock", "a+b")
        if self._lockfh.seek(0, 2) == 0: self._lockfh.write(b"\0"); self._lockfh.flush()
        self._lockfh.seek(0)
        try: _lock(self._lockfh)
        except OSError:
            self._lockfh.close(); self._lockfh = None
            raise StartRefused(f"instance {key_id} is already running (journal {path} is locked)") from None
        try: self._open()
        except BaseException: self.close(); raise

    def _open(self):
        marker = None
        if os.path.exists(self.marker_path):
            with open(self.marker_path, encoding="utf-8") as fh: marker = json.load(fh)
        if not os.path.exists(self.path):
            if marker is not None:
                raise StartRefused(f"key {self.key_id} has been used (marker {self.marker_path}) but its journal {self.path} is missing: "
                                   "retire the key (key.revoke) and let an owner abandon its open claims (X-07)")
            self.db = self._connect()
            self.db.executescript("CREATE TABLE meta(k TEXT PRIMARY KEY, v TEXT);"
                                  "CREATE TABLE events(n INTEGER PRIMARY KEY AUTOINCREMENT, claim TEXT, event TEXT, data TEXT);")
            self.db.execute("INSERT INTO meta VALUES('instance', ?)", (self.key_id,))
            self.db.execute("INSERT INTO meta VALUES('uuid', ?)", (uuid.uuid4().hex,))
        else:
            self.db = self._connect()
        meta = dict(self.db.execute("SELECT k, v FROM meta"))
        if meta.get("instance") != self.key_id:
            raise StartRefused(f"journal {self.path} belongs to instance {meta.get('instance')}, not {self.key_id}")
        if marker is not None and marker.get("journal") != meta["uuid"]:
            raise StartRefused(f"journal {self.path} is not the journal this key was used with (X-07)")
        if marker is None:
            tmp = self.marker_path + ".tmp"
            with open(tmp, "w", encoding="utf-8") as fh:
                json.dump({"instance": self.key_id, "journal": meta["uuid"]}, fh); fh.flush(); os.fsync(fh.fileno())
            os.replace(tmp, self.marker_path)

    def _connect(self):
        db = sqlite3.connect(self.path, isolation_level=None, check_same_thread=False)   # autocommit: every statement is durable
        db.execute("PRAGMA synchronous=FULL")
        return db

    def add(self, claim: str, event: str, data: dict | None = None):
        """Durably append an event (committed and fsynced before this returns)."""
        self.db.execute("INSERT INTO events(claim, event, data) VALUES(?,?,?)", (claim, event, json.dumps(data or {})))

    def claims(self) -> dict[str, list[tuple[str, dict]]]:
        """claim id → its events in order."""
        out: dict[str, list] = {}
        for claim, event, data in self.db.execute("SELECT claim, event, data FROM events WHERE claim NOT LIKE 'decline:%' ORDER BY n"):
            out.setdefault(claim, []).append((event, json.loads(data)))
        return out

    def declined(self, pid: str) -> bool:
        """Has this executor already told the proposer it won't run `pid`? (kept apart from claims: it never claimed)"""
        return self.db.execute("SELECT 1 FROM events WHERE claim=? AND event='declined'", ("decline:" + pid,)).fetchone() is not None

    def decline(self, pid: str, why: str): self.add("decline:" + pid, "declined", {"why": why})

    def events(self, claim: str) -> list[tuple[str, dict]]:
        return [(e, json.loads(d)) for e, d in self.db.execute("SELECT event, data FROM events WHERE claim=? ORDER BY n", (claim,))]

    def close(self):
        if self.db: self.db.close(); self.db = None
        if self._lockfh: _unlock(self._lockfh); self._lockfh.close(); self._lockfh = None

"""One path-form rule for every sandbox check, the same on Windows and POSIX (os.path alone reads a drive form such as C:\\x as a
relative name on POSIX)."""
from __future__ import annotations
import re

_DRIVE = re.compile(r"^[A-Za-z]:")
_CONTROL = re.compile(r"[\x00-\x1f\x7f]")

def portable_relative(p, root_ok: bool = False) -> bool:
    """May `p` be a relative path under a root? Both `/` and `\\` separate, on every OS. Refused: a non-string, a control character,
    empty or `.` (unless `root_ok`), a leading separator (POSIX-absolute, UNC, rooted), a drive letter (`C:`, `C:\\`, `C:/`) and
    any `..` segment."""
    if not isinstance(p, str) or _CONTROL.search(p): return False
    if p in ("", "."): return root_ok
    if p[0] in "/\\" or _DRIVE.match(p): return False
    return ".." not in re.split(r"[\\/]", p)

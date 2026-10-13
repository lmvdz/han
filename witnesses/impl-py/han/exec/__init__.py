"""han.exec: the HAN executor (architecture.md §4.5, X-01…X-10). An Actor fed by `subscribe`; no interface but behaviour."""
from .executor import Executor, Crash, STALL_BOUND
from .journal import Journal, StartRefused

__all__ = ["Executor", "Crash", "StartRefused", "Journal", "STALL_BOUND"]

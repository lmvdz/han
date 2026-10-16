"""The tool gate for ClaudeCodeAgent (architecture.md §8.4): a pure function from (tool, input, mode) to a Decision, so it is
testable without the SDK. It knows nothing of HAN entries: a `world` decision names the action the host will propose."""
from __future__ import annotations
import os, re
from ...paths import portable_relative
from dataclasses import dataclass, field

FILE_TOOLS = ("Read", "Write", "Edit", "NotebookEdit", "Glob", "Grep")
WRITE_TOOLS = ("Write", "Edit", "NotebookEdit")      # V7: local inside the session's worktree in every mode; outside it is refused, and in a strict turn the model is pointed at a proposal
# V1: HAN's own governing actions, as tools of HAN's MCP server. This host declares none of them (the gate would deny them as unknown), but a
# harness that does must not get them in a strict turn (R-24): approving, governing (`member.set`, `policy.set`) and steering another's session.
GOVERN_TOOLS = ("approve", "effect_approve", "member_set", "policy_set", "steer", "session_input")
WEB_TOOLS = {"WebFetch": ("web.fetch", "url"), "WebSearch": ("web.search", "query")}
MCP_PREFIX = "mcp__han__"            # the one in-process MCP server; external servers are disabled

@dataclass(frozen=True)
class WorldTool:
    """A custom tool the session may call. It never runs in the session: the call becomes a proposal for `action`."""
    name: str
    action: str                       # the HAN action name (executor side)
    description: str
    params: dict                      # {param: python type}, as the SDK's `tool` decorator takes it

DEFAULT_WORLD_TOOLS = (
    WorldTool("deploy", "file.write", "Publish a file to the shared output directory. target: relative path inside it. content: the text.",
              {"target": str, "content": str}),
    WorldTool("http_post", "http.post", "POST a text body to a URL.", {"url": str, "body": str}),
)

@dataclass
class Decision:
    kind: str                         # "allow" | "deny" | "world"
    message: str = ""                 # for deny: what the model is told
    action: str = ""                  # for world: the HAN action
    args: dict = field(default_factory=dict)

NETWORK = re.compile(r"(?i)(?<![\w.-])(curl|wget|ssh|scp|sftp|rsync|nc|ncat|netcat|telnet|ftp|ping|nslookup|dig|invoke-webrequest|iwr|"
                     r"invoke-restmethod|irm|git\s+(push|pull|fetch|clone|remote)|pip3?\s+install|npm\s+(i|install|publish)|"
                     r"npx|yarn\s+add|docker|az|aws|gcloud|gh)(?![\w-])|\b[a-z][a-z0-9+.-]*://")
# F6: every shell metacharacter is refused in every mode: separators, redirections, expansion ($, backticks, !, braces, ~) and every glob
# character (*?[]{}), so no word can name anything but itself. `set -f;` (globbing off) is then prefixed to what runs, a second line.
UNSAFE_CHARS = re.compile(r"[;&|<>$`!()\n\r{}\[\]*?\"'\\~]")
NOGLOB = "set -f; "

# E7: Bash is default-deny. A command runs only if it is in COMMANDS and EVERY argument is accounted for: a flag must be one the command
# lists (an unknown flag is refused), a flag's value is checked by its kind, and every other word is a path, a number or plain text as
# the command says. Kinds: "bool" (no value), "num" (digits), "text" (a pattern or key; never a path), "path" (read), "wpath" (written).
# V7: a write is local only inside the session's worktree (`cwd`), in a strict turn (R-24) as in an auto one: `wpath` is checked like `path`.
# A write outside it is refused, and a strict turn's model is pointed at the proposal path.
@dataclass(frozen=True)
class Cmd:
    short: dict = field(default_factory=dict)        # letter -> kind
    long: dict = field(default_factory=dict)         # name -> kind (`--name` or `--name=value`)
    pos: str = "path"                                 # kind of the positional words; "none" for no positional at all
    max_pos: int = 99
    first_text: bool = False                          # grep: the first positional is the pattern, unless -e/-f/--regexp/--file gave one
    digits: bool = False                              # head/tail: `-5`
    write: bool = False                               # the whole command writes

COMMANDS = {
    "ls": Cmd({c: "bool" for c in "alhrtSFd1"}, {"all": "bool", "reverse": "bool"}),
    "dir": Cmd(),
    "pwd": Cmd(pos="none"),
    "cat": Cmd({c: "bool" for c in "nbs"}, {"number": "bool", "squeeze-blank": "bool"}),
    "type": Cmd(),
    "head": Cmd({"n": "num", "c": "num", "q": "bool"}, {"lines": "num", "bytes": "num"}, digits=True),
    "tail": Cmd({"n": "num", "c": "num", "q": "bool"}, {"lines": "num", "bytes": "num"}, digits=True),
    "wc": Cmd({c: "bool" for c in "lwcm"}, {"lines": "bool", "words": "bool", "bytes": "bool", "chars": "bool"}),
    "echo": Cmd({"n": "bool"}, pos="text"),
    "date": Cmd({"u": "bool"}, {"utc": "bool"}, pos="none"),
    "sort": Cmd({**{c: "bool" for c in "rnufbV"}, "k": "text", "t": "text", "o": "wpath"},
                {"reverse": "bool", "numeric-sort": "bool", "unique": "bool", "ignore-case": "bool", "key": "text", "output": "wpath"}),
    "uniq": Cmd({c: "bool" for c in "cdui"}, {"count": "bool", "repeated": "bool", "unique": "bool", "ignore-case": "bool"}, max_pos=1),
    "grep": Cmd({**{c: "bool" for c in "ivncwxFEHhoqsl"}, "e": "text", "f": "path", "m": "num", "A": "num", "B": "num", "C": "num"},
                {"ignore-case": "bool", "invert-match": "bool", "line-number": "bool", "count": "bool", "word-regexp": "bool",
                 "line-regexp": "bool", "fixed-strings": "bool", "extended-regexp": "bool", "files-with-matches": "bool",
                 "only-matching": "bool", "quiet": "bool", "no-messages": "bool", "regexp": "text", "file": "path",
                 "max-count": "num", "after-context": "num", "before-context": "num", "context": "num"}, first_text=True),
    "mkdir": Cmd({"p": "bool"}, {"parents": "bool"}, pos="wpath", write=True),
    "touch": Cmd(pos="wpath", write=True),
}
LOCAL_BASH = tuple(COMMANDS)
_CONTROL = re.compile(r"[\x00-\x1f\x7f]")

def inside(root: str, path: str) -> bool:
    """Is `path` (relative to `root` if not absolute; symlinks resolved) within `root`?"""
    if not isinstance(path, str) or _CONTROL.search(path): return False
    if not os.path.isabs(path) and not portable_relative(path, True): return False      # a drive form is a relative name to POSIX: refuse it everywhere
    root = os.path.realpath(root)
    full = os.path.realpath(path if os.path.isabs(path) else os.path.join(root, path))
    try: return os.path.commonpath([os.path.normcase(full), os.path.normcase(root)]) == os.path.normcase(root)
    except ValueError: return False                          # another drive

PROPOSE_HINT = "; in a strict turn, propose the change as an effect (the deploy tool)"

class Gate:
    def __init__(self, cwd: str, world_tools=DEFAULT_WORLD_TOOLS, bash_allow=LOCAL_BASH):
        self.cwd, self.world_tools, self.bash_allow = os.path.realpath(cwd), {t.name: t for t in world_tools}, tuple(bash_allow)

    def decide(self, tool: str, inp: dict, mode: str) -> Decision:
        """`mode` is "auto" or "strict" (the harness mode of this turn, R-24)."""
        inp = inp or {}
        if tool in FILE_TOOLS: return self._file(tool, inp, mode)
        if tool in WEB_TOOLS:                                # world reads leak context through URLs: effects, not calls (§8.4)
            action, key = WEB_TOOLS[tool]
            return Decision("world", action=action, args={key: str(inp.get(key, ""))[:2000]})
        if tool == "Bash": return self._bash(str(inp.get("command", "")), mode)
        if tool.startswith(MCP_PREFIX) and tool[len(MCP_PREFIX):] in GOVERN_TOOLS:
            if mode == "strict": return Decision("deny", f"{tool[len(MCP_PREFIX):]}: approving, governing and steering others are not available in a strict turn")
            return Decision("deny", f"{tool} is not available in this session")
        if tool.startswith(MCP_PREFIX) and tool[len(MCP_PREFIX):] in self.world_tools:
            wt = self.world_tools[tool[len(MCP_PREFIX):]]
            return Decision("world", action=wt.action, args={k: v for k, v in inp.items() if k in wt.params})
        return Decision("deny", f"{tool} is not available in this session")

    def _file(self, tool: str, inp: dict, mode: str) -> Decision:
        paths = [inp[k] for k in ("file_path", "notebook_path", "path") if inp.get(k)]
        if any(not isinstance(p, str) or not inside(self.cwd, p) for p in paths):
            return Decision("deny", f"{tool}: path is outside the workspace" + (PROPOSE_HINT if mode == "strict" and tool in WRITE_TOOLS else ""))
        pat = inp.get("pattern") if tool == "Glob" else None       # a glob can climb out with .. or an absolute prefix
        if isinstance(pat, str) and not portable_relative(pat, True):
            return Decision("deny", "Glob: pattern leaves the workspace")
        return Decision("allow")

    def _bash(self, cmd: str, mode: str) -> Decision:
        """No sandbox exists for Bash on Windows, so there is no way to bound what a shell command touches. A command that names the
        network is a world action (an effect); anything else is default-deny (E7): one plain command from COMMANDS with no shell syntax,
        every flag known and every path argument inside the workspace. A write is local inside the worktree in every mode (V7)."""
        if cmd.startswith(NOGLOB): cmd = cmd[len(NOGLOB):]             # our own prefix, seen again by the second line (can_use_tool)
        if NETWORK.search(cmd): return Decision("world", action="bash.network", args={"command": cmd[:2000]})
        if UNSAFE_CHARS.search(cmd): return Decision("deny", "Bash: shell syntax is not allowed; one plain command only")
        words, strict = cmd.split(), mode == "strict"
        spec = COMMANDS.get(words[0]) if words and words[0] in self.bash_allow else None
        if not spec:
            return Decision("deny", f"Bash: only {', '.join(n for n in self.bash_allow if n in COMMANDS)} are allowed" + (" in a strict turn" if strict else ""))
        why = self._check_args(spec, words[1:], strict)
        return Decision("deny", f"Bash: {why}") if why else Decision("allow", args={"command": NOGLOB + cmd})

    def path_ok(self, p: str) -> bool:
        """E7: a path argument names something inside the workspace. `~` (home expansion), UNC (`\\\\`, `//`), drive letters, absolute
        paths and `..` are refused outright; anything else must resolve inside (symlinks resolved)."""
        if not p or "~" in p or "//" in p or "\\\\" in p: return False
        if not portable_relative(p, True): return False
        return inside(self.cwd, p)

    def _check_value(self, kind: str, v: str, strict: bool) -> str | None:
        """Why the value `v` of kind `kind` is refused, else None."""
        if kind == "num": return None if v.isdigit() else f"{v!r} is not a number"
        if kind == "text": return None
        if kind in ("path", "wpath"):
            return None if self.path_ok(v) else "path leaves the workspace" + (PROPOSE_HINT if strict and kind == "wpath" else "")
        return "unexpected value"

    def _check_args(self, spec: Cmd, args: list[str], strict: bool) -> str | None:
        """Why the arguments are refused, else None: each word is a flag the command lists, a value of one, or a positional."""
        pos, i, pattern_given, ended = [], 0, False, False
        while i < len(args):
            w = args[i]; i += 1
            if ended or not w.startswith("-") or w == "-":
                if w == "-": return "'-' (standard input) is not allowed"
                pos.append(w); continue
            if w == "--": ended = True; continue
            if w.startswith("--"):
                name, eq, val = w[2:].partition("=")
                kind = spec.long.get(name)
                if kind is None: return f"unknown flag --{name}"
                if kind == "bool":
                    if eq: return f"--{name} takes no value"
                    continue
                if not eq:
                    if i >= len(args): return f"--{name} needs a value"
                    val = args[i]; i += 1
                if (why := self._check_value(kind, val, strict)): return f"--{name}: {why}"
                pattern_given |= name in ("regexp", "file")
                continue
            if spec.digits and w[1:].isdigit(): continue
            j = 1
            while j < len(w):
                kind = spec.short.get(w[j])
                if kind is None: return f"unknown flag -{w[j]}"
                if kind == "bool": j += 1; continue
                val = w[j + 1:]                                   # `-n5` or `-n 5`
                if not val:
                    if i >= len(args): return f"-{w[j]} needs a value"
                    val = args[i]; i += 1
                if (why := self._check_value(kind, val, strict)): return f"-{w[j]}: {why}"
                pattern_given |= w[j] in "ef"
                break
        if spec.pos == "none" and pos: return "no arguments are allowed"
        if len(pos) > spec.max_pos: return "too many arguments"
        for n, v in enumerate(pos):
            kind = "text" if spec.pos == "text" or (spec.first_text and n == 0 and not pattern_given) else spec.pos
            if (why := self._check_value(kind, v, strict)): return why
        return None

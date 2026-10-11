"""The tool gate for ClaudeCodeAgent (architecture.md §8.4): a pure function from (tool, input, mode) to a Decision, so it is
testable without the SDK. It knows nothing of HAN entries: a `world` decision names the action the host will propose."""
from __future__ import annotations
import os, re
from dataclasses import dataclass, field

FILE_TOOLS = ("Read", "Write", "Edit", "NotebookEdit", "Glob", "Grep")
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
LOCAL_BASH = ("ls", "dir", "pwd", "cat", "type", "head", "tail", "wc", "echo", "date", "sort", "uniq", "mkdir", "touch", "grep")
UNSAFE_CHARS = re.compile(r"[;&|<>$`()\n\r{}*?\"'\\~]")

def inside(root: str, path: str) -> bool:
    """Is `path` (relative to `root` if not absolute; symlinks resolved) within `root`?"""
    root = os.path.realpath(root)
    full = os.path.realpath(path if os.path.isabs(path) else os.path.join(root, path))
    try: return os.path.commonpath([os.path.normcase(full), os.path.normcase(root)]) == os.path.normcase(root)
    except ValueError: return False                          # another drive

class Gate:
    def __init__(self, cwd: str, world_tools=DEFAULT_WORLD_TOOLS, bash_allow=LOCAL_BASH):
        self.cwd, self.world_tools, self.bash_allow = os.path.realpath(cwd), {t.name: t for t in world_tools}, tuple(bash_allow)

    def decide(self, tool: str, inp: dict, mode: str) -> Decision:
        """`mode` is "auto" or "strict" (the harness mode of this turn, R-24)."""
        inp = inp or {}
        if tool in FILE_TOOLS: return self._file(tool, inp)
        if tool in WEB_TOOLS:                                # world reads leak context through URLs: effects, not calls (§8.4)
            action, key = WEB_TOOLS[tool]
            return Decision("world", action=action, args={key: str(inp.get(key, ""))[:2000]})
        if tool == "Bash": return self._bash(str(inp.get("command", "")), mode)
        if tool.startswith(MCP_PREFIX) and tool[len(MCP_PREFIX):] in self.world_tools:
            wt = self.world_tools[tool[len(MCP_PREFIX):]]
            return Decision("world", action=wt.action, args={k: v for k, v in inp.items() if k in wt.params})
        return Decision("deny", f"{tool} is not available in this session")

    def _file(self, tool: str, inp: dict) -> Decision:
        paths = [inp[k] for k in ("file_path", "notebook_path", "path") if inp.get(k)]
        if any(not isinstance(p, str) or not inside(self.cwd, p) for p in paths):
            return Decision("deny", f"{tool}: path is outside the workspace")
        pat = inp.get("pattern") if tool == "Glob" else None       # a glob can climb out with .. or an absolute prefix
        if isinstance(pat, str) and (os.path.isabs(pat) or re.match(r"^[A-Za-z]:", pat) or ".." in re.split(r"[\\/]", pat)):
            return Decision("deny", "Glob: pattern leaves the workspace")
        return Decision("allow")

    def _bash(self, cmd: str, mode: str) -> Decision:
        """No sandbox exists for Bash on Windows, so there is no way to bound what a shell command touches. A command that names the
        network is a world action (an effect); anything else is denied in strict turns and, in auto turns, allowed only if it is one
        plain command from a small local allow-list with no shell syntax and no path leaving the workspace."""
        if NETWORK.search(cmd): return Decision("world", action="bash.network", args={"command": cmd[:2000]})
        if mode == "strict": return Decision("deny", "Bash is not available in a strict turn (no sandbox)")
        if UNSAFE_CHARS.search(cmd): return Decision("deny", "Bash: shell syntax is not allowed; one plain command only")
        words = cmd.split()
        if not words or words[0] not in self.bash_allow: return Decision("deny", f"Bash: only {', '.join(self.bash_allow)} are allowed")
        for w in words[1:]:
            if w.startswith("-"): continue
            if os.path.isabs(w) or re.match(r"^[A-Za-z]:", w) or ".." in re.split(r"[\\/]", w) or not inside(self.cwd, w):
                return Decision("deny", "Bash: path leaves the workspace")
        return Decision("allow")

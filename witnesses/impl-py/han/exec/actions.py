"""The world the executor touches: `file.write` (inside a sandbox directory) and `http.post` (to allow-listed URL prefixes).
A plan is {action, target, args?}: file.write → target = relative path, args = {"content": str}; http.post → target = URL,
args = the JSON body."""
from __future__ import annotations
import json, os, urllib.request

class Refused(Exception):
    """The plan is not something this executor will run (checked before claiming)."""

class Actions:
    def __init__(self, sandbox: str, allow_urls=(), enabled=("file.write", "http.post")):
        self.sandbox, self.allow_urls, self.enabled = os.path.realpath(sandbox), tuple(allow_urls), tuple(enabled)

    def check(self, plan: dict):
        """Raise Refused unless the plan may run here."""
        action = plan["action"]
        if action not in self.enabled: raise Refused(f"action {action} not enabled")
        if action == "file.write":
            self._path(plan["target"])
            if not isinstance((plan.get("args") or {}).get("content"), str): raise Refused("file.write needs args.content (string)")
        elif action == "http.post":
            if not any(plan["target"].startswith(p) for p in self.allow_urls): raise Refused("url not allow-listed")
        else: raise Refused(f"unknown action {action}")

    def _path(self, target: str) -> str:
        if not target or os.path.isabs(target) or os.path.splitdrive(target)[0]: raise Refused("path must be relative")
        full = os.path.realpath(os.path.join(self.sandbox, target))
        if os.path.commonpath([full, self.sandbox]) != self.sandbox or full == self.sandbox: raise Refused("path escapes the sandbox")
        return full

    def run(self, plan: dict) -> dict:
        """Blocking. Returns evidence (a JSON-able dict); raises on failure."""
        if plan["action"] == "file.write":
            full = self._path(plan["target"])
            os.makedirs(os.path.dirname(full), exist_ok=True)
            tmp = full + ".han-tmp"
            with open(tmp, "w", encoding="utf-8", newline="") as fh:
                fh.write(plan["args"]["content"]); fh.flush(); os.fsync(fh.fileno())
            os.replace(tmp, full)
            return {"path": plan["target"], "bytes": os.path.getsize(full)}
        req = urllib.request.Request(plan["target"], data=json.dumps(plan.get("args")).encode(), method="POST",
                                     headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=30) as r:
            return {"status": r.status}

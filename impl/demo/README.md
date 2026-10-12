# Demo: real Claude Code behind HAN

`claude_demo.py` is the seed of v1 test 1 ("used daily"): a guarded Claude Code agent, driven through the Claude Agent SDK, is steered by a member, its world action waits for the operator, the operator approves, an executor writes the file, and a second steer by the operator still runs strict (carol's steer entered the session's context, R-24), so its local write is refused and it proposes nothing.

```
cd H:\HAN\v1\impl
.venv\Scripts\python.exe demo\claude_demo.py [--model MODEL] [--keep DIR]
```

It starts everything in-process in a temp directory (a sequencer on a free port, `han-exec` limited to `file.write` into `<dir>/output`, the host with its workspace in `<dir>/workspace`), prints a timestamped narration and exits 0 when the outcome is as expected. It uses the logged-in Claude Code (bundled CLI 2.1.294) for two short turns: about 2 x 13k input tokens (mostly cached prompt), 150 output tokens, about 3 cents, 10 s.

What you see: carol's steer runs a strict turn (SDK mode `default`); Claude calls the `deploy` tool; `can_use_tool` answers at once "proposed as <id>, waiting for @alice"; the effect is `awaiting-approval` with `guardedFor` naming the agent; alice approves; `hello.txt` appears; alice's own steer is not outside input, but the session is tainted, so it runs strict too (`default`) and `notes.txt` is not written.

The same flow is the one live test, `tests/test_host_claude_live.py` (`HAN_LIVE=1`, skipped otherwise). Without API calls: `tests/test_host_claude_gate.py`.

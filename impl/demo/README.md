# Demo: real Claude Code behind HAN

`claude_demo.py` is the seed of v1 test 1 ("used daily"): a guarded Claude Code agent, driven through the Claude Agent SDK, is steered by a member, its world action is held for the operator, the operator approves, an executor writes the file, and a second, local-only steer runs in auto mode with no effect.

```
cd H:\HAN\v1\impl
.venv\Scripts\python.exe demo\claude_demo.py [--model MODEL] [--keep DIR]
```

It starts everything in-process in a temp directory (a sequencer on a free port, `han-exec` limited to `file.write` into `<dir>/output`, the host with its workspace in `<dir>/workspace`), prints a timestamped narration and exits 0 when the outcome is as expected. It uses the logged-in Claude Code (bundled CLI 2.1.294) for two short turns: about 2 x 13k input tokens (mostly cached prompt), 150 output tokens, about 3 cents, 10 s.

What you see: carol's steer runs a strict turn (SDK mode `default`); Claude calls the `deploy` tool; `can_use_tool` answers at once "proposed as <id>, waiting for @alice"; the effect is `awaiting-approval` and the agent held; alice approves; `hello.txt` appears; alice's release and own steer give an auto turn (`acceptEdits`) that writes `notes.txt` in the workspace and proposes nothing.

The same flow is the one live test, `tests/test_host_claude_live.py` (`HAN_LIVE=1`, skipped otherwise). Without API calls: `tests/test_host_claude_gate.py`.

"""ONE live end-to-end test through the real Claude Agent SDK (a few cents of Claude usage). Skipped unless HAN_LIVE=1.
Same flow as demo/claude_demo.py: a member steers a guarded Claude Code agent to deploy a file; the proposal waits for the operator; the operator
approves; han-exec writes the file; then the operator's own steer runs strict too (carol's steer tainted the session, R-24), so the local write is refused and no effect appears."""
import os, sys, pytest
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "demo"))

pytestmark = [pytest.mark.live, pytest.mark.skipif(os.environ.get("HAN_LIVE") != "1", reason="live Claude run: set HAN_LIVE=1")]

async def test_live_guarded_deploy_then_local_edit(tmp_path):
    """P-15, R-24, §8.4 with the real SDK: guarded proposal → approval → executor outcome; the tainted session stays strict."""
    from claude_demo import run
    res = await run(str(tmp_path), say=lambda m: print(m))
    assert res["written"] == "hi from HAN"
    assert res["phase"] == "succeeded" and res["effects"] == 1               # exactly one effect: the deploy; the second turn proposed none
    assert res["local"] is None and res["modes"] == ["default", "default"]       # strict: the write is refused, not proposed
    assert ("mcp__han__deploy", "world") in res["trace"]

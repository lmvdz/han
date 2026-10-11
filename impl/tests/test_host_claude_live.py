"""ONE live end-to-end test through the real Claude Agent SDK (a few cents of Claude usage). Skipped unless HAN_LIVE=1.
Same flow as demo/claude_demo.py: a member steers a guarded Claude Code agent to deploy a file; the proposal is held; the operator
approves; han-exec writes the file; then the operator's own (auto) steer edits a local file with no effect."""
import os, sys, pytest
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "demo"))

pytestmark = [pytest.mark.live, pytest.mark.skipif(os.environ.get("HAN_LIVE") != "1", reason="live Claude run: set HAN_LIVE=1")]

async def test_live_guarded_deploy_then_local_edit(tmp_path):
    """P-15, R-24, §8.4 with the real SDK: held proposal → approval → executor outcome; the auto turn stays local."""
    from claude_demo import run
    res = await run(str(tmp_path), say=lambda m: print(m))
    assert res["written"] == "hi from HAN"
    assert res["phase"] == "succeeded" and res["effects"] == 1               # exactly one effect: the deploy; the local edit proposed none
    assert res["local"] and "local only" in res["local"]
    assert res["held_after"] == {} and res["modes"] == ["default", "acceptEdits"]
    assert ("mcp__han__deploy", "world") in res["trace"]

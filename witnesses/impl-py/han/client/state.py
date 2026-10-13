"""Reading the StateView in pages (H3): the call `state` answers one page `{state, next?}`; every paged map (`W.PAGED`) is cut, so a big state
comes in further pages, each carrying `next`, the cursor for `state {cursor}`. A client class mixes `StateReads` in; it needs `state(space, **sel)`
with at most one of the `at`, `cursor` and `effect` keywords."""
from __future__ import annotations
from ..core import W

class StateReads:
    async def state_pages(self, space: str, at: int | None = None):
        """The state of the head (or after record `at`), then every further page, one `{state, next?}` reply each."""
        reply = await self.state(space, **({} if at is None else {"at": at}))
        while True:
            yield reply
            if "next" not in reply: return
            reply = await self.state(space, cursor=reply["next"])

    async def read_state(self, space: str, at: int | None = None, effect: str | None = None) -> dict:
        """The whole StateView, its pages merged (every paged map complete). With `effect`: the one effect and the repeated members only
        (`{space, head, home, policy, effects}`, never cut), which is all an executor or a host needs for one claim."""
        if effect is not None: return (await self.state(space, effect=effect))["state"]
        st = None
        async for page in self.state_pages(space, at):
            if st is None: st = dict(page["state"])
            else:
                for p in W.PAGED:
                    if W._has(page["state"], p): W._put_map(st, p, dict(W._get_map(st, p), **W._get_map(page["state"], p)))
        return st

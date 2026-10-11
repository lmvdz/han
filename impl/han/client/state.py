"""Reading the StateView in pages (E8): `read {state: true}` never answers `limit` for a big state. `effects` and `tasks` beyond
`maxStateItems` come in further pages, each item carrying `next`, the cursor for `read {state: true, cursor}`. A client class
mixes `StateReads` in; it needs `read_all(space, after, **kw)` with the `state`, `at`, `cursor` and `effect` keywords."""
from __future__ import annotations
from ..core import WireError

class StateReads:
    async def state_pages(self, space: str, at: int | None = None):
        """The state item of the head (or of the state after record `at`), then every further page, one `state` ReadItem each."""
        kw = {} if at is None else {"at": at}
        item = next((i for i in await self.read_all(space, 2 ** 31, state=True, limit=1, **kw) if i["type"] == "state"), None)
        while item is not None:
            yield item
            if "next" not in item: return
            item = next((i for i in await self.read_all(space, 0, state=True, cursor=item["next"]) if i["type"] == "state"), None)
        raise WireError("denied", "no state item")

    async def read_state(self, space: str, at: int | None = None, effect: str | None = None) -> dict:
        """The whole StateView, its pages merged (`effects` and `tasks` complete). With `effect`: only that effect plus policy, roles,
        sessions, held, guarded and agents (`read {state, effect}`), which is all an executor needs for one claim."""
        if effect is not None:
            items = await self.read_all(space, 2 ** 31, state=True, limit=1, effect=effect)
            return next(i for i in items if i["type"] == "state")["state"]
        st = None
        async for page in self.state_pages(space, at):
            if st is None: st = dict(page["state"])
            else: st["effects"].update(page["state"]["effects"]); st["tasks"].update(page["state"]["tasks"])
            st["effects"], st["tasks"] = dict(st["effects"]), dict(st["tasks"])
        return st

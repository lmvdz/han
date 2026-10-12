"""Reading the StateView in pages (E8): `read {state: true}` never answers `limit` for a big state. `effects`, `tasks`, `sessions` and `roleHistory` beyond
`maxStateItems` or the frame size (F3(c)) come in further pages, each item carrying `next`, the cursor for `read {state: true, cursor}`. A client class
mixes `StateReads` in; it needs `read_all(space, after, **kw)` with the `state`, `at`, `cursor` and `effect` keywords."""
from __future__ import annotations
from ..core import WireError

PAGED = ("effects", "tasks", "sessions", "roleHistory")      # the members a server cuts into pages; the others repeat on every page

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
        sessions, held and guarded (`read {state, effect}`; its `sessions` are paged by bytes like the state's), which is all an executor needs for one claim."""
        if effect is not None:
            item = next(i for i in await self.read_all(space, 2 ** 31, state=True, limit=1, effect=effect) if i["type"] == "state")
            st = dict(item["state"])
            while "next" in item:                                   # F3(c): the effect is whole; only `sessions` can continue
                item = next(i for i in await self.read_all(space, 0, state=True, cursor=item["next"]) if i["type"] == "state")
                st["sessions"] = dict(st["sessions"], **item["state"]["sessions"])
            return st
        st = None
        async for page in self.state_pages(space, at):
            if st is None: st = dict(page["state"])
            else:
                for k in PAGED: st[k] = dict(st[k], **page["state"][k])
        return st

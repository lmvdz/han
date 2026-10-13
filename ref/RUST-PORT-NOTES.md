# Notes for the Rust port of Policy and `admit` (han-core)

Written at round 31 by the agent that implemented it in `ref/`. These are the details a port must reproduce exactly; the vectors and differential fuzz check them, but these say why.

1. **`carried(x)`** is the union of `guardedFor` over **all** tasks (open or closed) whose `accepted` contains x, in task `seq` order, deduplicated first-seen. Order matters: `guardedFor` is compared as an ordered list.
2. **Task `guardedFor`** = the parent's list if there is a parent, else `carried(author)`; then the author if guarded.
3. **Effect `guardedFor`** = the task's list if a task is named, else `carried(proposer)`; then `carried(executor)` (new items only); then the proposer, then the executor, each if guarded. Each agent appears once.
4. **Bounds.** A task is denied when its `guardedFor` has more than 17 agents. A proposal is denied when |guardedFor ∪ {proposer, executor}| > 19. Each is the last check of its kind ("too many guarded tasks; close some").
5. **`task.update` decide order:**
   1. not a task → `invalid`;
   2. closed and not (final and author ∈ `accepted`) → `denied`;
   3. no role → `denied`;
   4. owner, not an assignee, final → allow;
   5. assignee, final, with an effect on the task `claimed` or `unknown` → `denied`;
   6. assignee → allow;
   7. otherwise `denied`.
6. **`task.update` fold.** If the task was open: set `state`, and on a final state close it and refund. Only `"accepted"` appends to `accepted`. On a final state, an assignee author removes itself; a non-assignee (owner) clears the list. **`task.cancel` fold:** an owner author clears `accepted`.
7. **Dead approvals.** An approval is dead when its author is in `guarded`, or forcing is on and its recorded `actorKind ≠ human`, or its author's server is in `servers.deny`. `policy.set` drops dead open approvals. A not-started outcome drops a returned approval whose author lost its role or is dead.
8. **Depth.** `json_ok` allows at most 32 nested containers: a container nested 32 deep fails if it holds a container. Frames allow depth ≤ 128.
9. **Grammar.** ServerName is at most 253 characters, inside ActorId and SpaceId too. The Time year is 0001–9999.
10. **`admit` decision order:**
    1. `hello` → open;
    2. no hello → deny;
    3. the node vouches for the actor (the node is among the actor's valid profile keys, or among the `@:S` well-known nodes) and the actor is `@:S` → server;
    4. the node vouches and the actor holds owner, member or guest (not banned) in any served space → member;
    5. `join` or `cancel` → open;
    6. read, subscribe, state or blobGet on a space not held, a profile space or a public space → open; on any other space → deny;
    7. `append` to a served space with `join: "open"` → open;
    8. otherwise deny.
11. **Ed25519.** Use `verify_strict`, and also reject small-order or non-canonical public keys **at parse** (`key.add`, a profile's first key, node ids, well-known keys). `conformance/carrier-vectors/ed25519.json` has two cases that plain cofactorless verify accepts and that must be refused.
12. **Number edge cases.** `-0` anywhere in a frame → id 0 and close. An integer with |x| > 2^53−1 is answered on the request's own id when that id is usable. Accept any key order, and hash over JCS.
13. **Carrier frames.** A frame is a 4-byte big-endian length followed by I-JSON; a length over 1,310,720 closes the connection before the payload is read.

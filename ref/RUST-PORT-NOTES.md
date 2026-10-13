# Notes for the Rust port of Policy and `admit` (han-core)

Written at round 31 by the agent that implemented it in `ref/`, and brought up to round 32 (Z1–Z3, Z7). These are the details a port must reproduce exactly; the vectors and differential fuzz check them, but these say why.

1. **`carried(x)`** is the union of `guardedFor` over **all** tasks (open or closed) whose `accepted` contains x, in task `seq` order, deduplicated first-seen. Order matters: `guardedFor` is compared as an ordered list.
2. **`guards(x)`** (round 32, Z1) = `[x]` if `x` is in the state's `guarded` set (no profile needed) or is forced (forcing on and the pinned profile not human), then `carried(x)` (new items only). `guardStatus` returns `{guard?, guarded, forced, guards}` with `guarded` = `guards` not empty; it reads `policy`, `guarded` and `tasks` (a `state {agent}` view carries the tasks whose `accepted` lists the agent).
   **Task `guardedFor`** = the parent's list if there is a parent (else empty), then `guards(author)` (new items only).
3. **Effect `guardedFor`** = the task's list if a task is named (else empty), then `guards(proposer)`, then `guards(executor)` (new items only each time). Each agent appears once.
4. **Bounds.** A task is denied when its `guardedFor` has more than 17 agents. A proposal is denied when |guardedFor ∪ {proposer, executor}| > 19. Each is the last check of its kind ("too many guarded tasks; close some").
5. **`task.update` decide order:**
   1. not a task → `invalid`;
   2. closed and not final → `denied` (round 32: any final update passes here);
   3. no role → `denied`;
   4. owner, not an assignee, final → allow;
   5. assignee, final, with an effect on the task `claimed` or `unknown` → `denied`;
   6. assignee → allow;
   7. otherwise `denied`.
6. **`task.update` fold.** If the task was open: set `state`, and on a final state close it and refund. Only `"accepted"` appends to `accepted`. On a final state, open or closed, an assignee author removes itself; a non-assignee (owner) clears the list. **`task.cancel` fold:** sets `cancelRequested` only (round 32). **`member.set` fold:** a target whose new role is `none` or `banned` is removed from every task's `accepted` (round 32), next to dropping its open approvals.
6a. **Step 8, the Limits** (round 32): `guards(author)` not empty and the entry is an `effect.approve` whose proposal is missing or whose `guards_of` lacks the author; a `member.set` other than `{actor: author, role: "none"}`; `policy.set`; `space.move`; a final `task.update` on an existing task whose `to` lacks the author; a `task.cancel` of an existing task authored by another; a `session.input` into an existing session of another actor → `denied`.
6b. **E3 guards** (round 32): `claim_guards` = `guards(proposer)` then `guards(executor)` (new items only). After the rule's candidate and before the guard loop, the claim is `denied` if `claim_guards` holds an agent in neither `guardedFor` nor {proposer, executor}. `guards_of` = the guard (pinned confirmed operator, or none) of each agent in `guardedFor`, then of each agent of `claim_guards` that is the proposer or executor and not in `guardedFor`, deduplicated first-seen. The cancel right by approval needs `guards(author)` empty.
7. **Dead approvals.** An approval is dead when its author is in `guarded`, or forcing is on and its recorded `actorKind ≠ human`, or its author's server is in `servers.deny`. `policy.set` drops dead open approvals. A not-started outcome drops a returned approval whose author lost its role or is dead.
8. **Depth.** `json_ok` allows at most 32 nested containers: a container nested 32 deep fails if it holds a container. Frames allow depth ≤ 128.
9. **Grammar.** ServerName is at most 253 characters, inside ActorId and SpaceId too. The Time year is 0001–9999.
10. **`admit` decision order** (round 32, Z7: the member tier is per space):
    1. `hello` → open;
    2. no hello → deny;
    3. the node vouches for the actor (the node is among the actor's valid profile keys, or among the `@:S` well-known nodes) and the actor is `@:S` → server;
    4. the node vouches and the actor holds owner, member or guest in the space the call names → member;
    5. a call naming no space (`blobPut`): member if the node vouches and the actor is owner or member of a served space whose `join` isn't `"open"` (profile spaces count);
    6. `join` or `cancel` → open;
    7. any call naming a space this node doesn't hold → open (routing answers it);
    8. reads of a profile or public space → open;
    9. `append` to a served space with `join: "open"` → open;
    10. otherwise deny. `ref/carrier.py` `admit` and `crates/han-core/src/admit.rs` are the reference.
11. **Ed25519.** Use `verify_strict`, and also reject small-order or non-canonical public keys **at parse** (`key.add`, a profile's first key, node ids, well-known keys). `conformance/carrier-vectors/ed25519.json` has two cases that plain cofactorless verify accepts and that must be refused.
12. **Number edge cases.** `-0` anywhere in a frame → id 0 and close. An integer with |x| > 2^53−1 is answered on the request's own id when that id is usable. Accept any key order, and hash over JCS.
13. **Carrier frames.** A frame is a 4-byte big-endian length followed by I-JSON; a length over 1,310,720 closes the connection before the payload is read.

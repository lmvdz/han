# Profile `han.status/1`

Status: profile, round 30; delivery fixed in round 31 (X11); round 32: text bounds, `sandbox` and `gate`, summaries moved to context. **Normative for interoperability between HAN apps; carries no authority.** Policy never reads a status item, no approval, steer or claim depends on one, and a face MUST NOT present one as fact: what an agent did is in the log (effects, outcomes, `task.update`). Source: PLAN.md §5.

## What it is

One item that says where a live session is, as its host sees it. It feeds the board, overlap alerts, agent tools and integrations (for example a GitHub sync). The session's host publishes it on the session's stream as the body of a `profile` stream item, `{type: "profile", name: "han.status/1", body: <this item>}` (T-07, G-05; round 31, X11), so it reaches exactly the actors `sessionAccess` grants watch. The server checks only the stream item's shape and never this body (architecture.md G-05, round 32 Z6): a **face** checks it by this profile and treats a body that fails as absent. Both checks are in the carrier vectors `conformance/carrier-vectors/stream-items.json` (`cases` for the server, `bodies` for a face).

## Shape

```
{ session, agent, delegate_of?: ActorId, harness,
  issue?: "github:owner/repo#123", repo?: "github:owner/repo", base?: sha,
  state: "working" | "waiting-approval" | "blocked" | "idle" | "done" | "stale" | "failed",
  step: string, tool?: string, plan?: [{text, done}],
  files: [{path, hunks: [[start, len]]}],
  tests?: {pass, fail}, tokens?: {in, out, cost},
  sandbox?: string, gate?: "every_call" | "prompt_only", seq, at }
```

| Field | Meaning |
|---|---|
| `session` | the `session.open` entry id (EntryId) |
| `agent` | the session's actor (ActorId) |
| `delegate_of` | the actor whose task drives the session, if any (the task's author) |
| `harness` | the agent program, free text (for example `claude-code`, `pi`) |
| `issue` | the work's external reference, the same string as the task's `external` when the session's task has one (`github:owner/repo#123`) |
| `repo`, `base` | the repository and the commit the session's worktree started from |
| `state` | one of the seven values above |
| `step`, `tool`, `plan` | the current step (at most 256 characters), the tool in use (at most 128), the plan with what is done (at most 64 items, each `text` at most 256 characters) |
| `files` | the files changed so far, with changed line ranges `[start, len]` against `base` |
| `tests`, `tokens` | test counts, token use and cost (in the space budget's unit, if any) |
| `sandbox` | the session's sandbox backend as its host names it (for example `mxc`, `bwrap`, `seatbelt`, `wsl2`), at most 64 characters, or `none` (architecture.md §8.4, SR-34) |
| `gate` | the host's gate fidelity: `every_call` when it sees every tool call, `prompt_only` when it sees only the calls the harness asks about (§8.4, SR-34) |
| `seq` | a counter per session, starting at 1, one more on every item |
| `at` | the host's Time when it made the item (informational) |

Rules:
- `seq` orders the items of one session, so freshness never depends on wall clocks; a face keeps the item with the highest `seq` and ignores older ones.
- **`stale` is derived, never claimed.** A face shows a session as `stale` when no item has arrived for a time it chooses (for example 120 s of its own clock); an item whose `state` is `stale` is treated as `idle`.
- `waiting-approval` means the session has an open effect waiting for an approval; a face reads which one from the log, never from the item.
- **Untrusted text** (SR-59). `step`, `tool` and `plan[].text` are free text from the session's host, which no one checked: a face shows them as text, never as markup or a link it follows, and an agent gets them only inside the data envelope (architecture.md R-24, SR-54). A host MUST NOT send longer ones than the bounds above; a face that gets one treats the body as failing (absent).
- **No summaries here** (SR-66). A summary derived from the session's context is context: it travels as a `context` stream item with `what: "summary"`, under the read right (T-07), never in this item, which reaches watchers. The `summary` member of round 30 is withdrawn: a face ignores it, as an unknown member.
- **Sandbox and gate** (SR-34, SR-99). A host MUST send `sandbox` and `gate` in every item. A face MUST show both, and MUST mark the session as **advisory** when `sandbox` is `none`, `gate` is `prompt_only`, or either is missing: its `guarded` mode is then only as good as the host's gate.
- Unknown members are ignored, so the profile can grow without a new version; a change of meaning is `han.status/2`.

Implementation extension: HAN's host and faces use `tokens.cost_measured`
(absent means false) together with the session's `budget.unit` to distinguish a
complete reported amount from unmeasured cost. This adds measurement provenance,
not execution authority. See [usage accounting](../../docs/usage-accounting.md)
for coverage and compatibility limits.

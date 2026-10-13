# Profile `han.status/1`

Status: profile, round 30. **Normative for interoperability between HAN apps; carries no authority.** Policy never reads a status item, no approval, steer or claim depends on one, and a face MUST NOT present one as fact: what an agent did is in the log (effects, outcomes, `task.update`). Source: PLAN.md §5.

## What it is

One item that says where a live session is, as its host sees it. It feeds the board, overlap alerts, agent tools and integrations (for example a GitHub sync). The session's host publishes it on the session's stream (T-07; on `han/1` as bulk data, G-07), so it reaches exactly the actors `sessionAccess` grants watch.

## Shape

```
{ session, agent, delegate_of?: ActorId, harness,
  issue?: "github:owner/repo#123", repo?: "github:owner/repo", base?: sha,
  state: "working" | "waiting-approval" | "blocked" | "idle" | "done" | "stale" | "failed",
  step: string, tool?: string, plan?: [{text, done}],
  files: [{path, hunks: [[start, len]]}],
  tests?: {pass, fail}, tokens?: {in, out, cost}, summary?: string, seq, at }
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
| `step`, `tool`, `plan` | the current step, the tool in use, the plan with what is done |
| `files` | the files changed so far, with changed line ranges `[start, len]` against `base` |
| `tests`, `tokens`, `summary` | test counts, token use and cost (in the space budget's unit, if any), a short summary |
| `seq` | a counter per session, starting at 1, one more on every item |
| `at` | the host's Time when it made the item (informational) |

Rules:
- `seq` orders the items of one session, so freshness never depends on wall clocks; a face keeps the item with the highest `seq` and ignores older ones.
- **`stale` is derived, never claimed.** A face shows a session as `stale` when no item has arrived for a time it chooses (for example 120 s of its own clock); an item whose `state` is `stale` is treated as `idle`.
- `waiting-approval` means the session has an open effect waiting for an approval; a face reads which one from the log, never from the item.
- Unknown members are ignored, so the profile can grow without a new version; a change of meaning is `han.status/2`.

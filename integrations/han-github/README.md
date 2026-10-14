# han-github

HAN's GitHub integration (PLAN §6.7, milestone M5). A separate process and an actor over the node API
(`RpcClient`, the local pipe): it holds no key of the space and no write access to GitHub.

```
han-github run --config han-github.toml
han-github request --config han-github.toml github:owner/repo#42 --title "Fix the parser"
```

## What it does

1. Every `poll_secs` (default 60) it reads each configured repository's issues with a conditional request
   (`If-None-Match`). A `304` costs one request and changes nothing.
2. What changed becomes `github.issue/1` items on a session stream (below).
3. An **open** issue that has the trigger label (default `agent-ready`) gets one
   `TaskCreate { external: "github:owner/repo#N", to: tasks.pool + tasks.to }`, once. `han-github request` does the same
   for one issue on a human's say-so. A task is never created for closed issues, for pull requests, or when
   the space already has a task with that `external` (open or not), so a restart creates no duplicates.
4. It cancels its own open task (`TaskCancel`) when the issue is closed, or loses the trigger label, before any agent
   accepted the task. Only tasks whose author is this process's node count (only the author may cancel), a task an
   agent already accepted is left to its assignee, and a task a human asked for with `request` (while the process that made it runs) is not governed by the
   label. A restart sees every issue as changed, so stale tasks are cancelled on the first poll.
5. It does not assign. The task is a suggestion addressed to the shared agent pool (`tasks.pool`); any one pool agent that takes the issue makes
   **its own signed task** with the same `external` (PLAN §6.6). Issues without the label are only shown.

Pagination: page 1 (`state=all&sort=updated&direction=desc&per_page=100`) is the conditional request. Sorted by
update time, any change to an issue moves it to page 1, so an unchanged page 1 means nothing changed. When it
changed, later pages (`Link: rel="next"`) are read only while they still reach back to the newest `updated_at`
seen, up to `max_pages` (default 5, so 500 issues on the first poll). A `Link` that leaves the API base is
refused, so a token never goes elsewhere.

## Credentials and the single-minter rule

PLAN §6.7 and SR-49: han-exec is the single minter for **write** actions (`open_pr`, `git_push`, comments),
approved in the log, and only han-exec holds write-capable key material. This process parses untrusted issue text,
so it has **its own read-only App**: a separate GitHub App with only Issues and Metadata read, configured as the
`[app]` below. The executor's write App is a *different* App whose key only han-exec holds; `[app]` must never point
at it. han-github has no `han-exec` dependency (`crates/han-node/tests/layering.rs` checks that its dependency tree
has no `han-exec` and no `rsa`), and mints with a small JWT and installation-token mint of its own (`src/mint.rs`,
`ring`) for **reads only**: each token is scoped to one repository and the single permission `issues: read`, is held in
memory for 45 minutes, and is used through octocrab. Nothing here can write to GitHub: there is no code path
that sends anything but a `GET` (and the token request). The App's private key stays in a PEM file; the config
holds only its path, and neither the path, the key nor a token appears in logs, errors or `Debug` output.
The App's permissions must be `issues: read` and `metadata: read` and nothing else. The user's App 5309257 is
already read-only and is this App; the executor's write App is a second App, created later. Future work (PLAN 6.7): a
read-token broker in the executor, so this process holds no key at all.

## Config (TOML)

```toml
space = "!abc:home.example"        # the space issues are surfaced in
data_dir = "/home/me/.han"         # the node's data dir (or: endpoint = "<pipe or socket>")
repos = ["lmvdz/han"]
poll_secs = 60                     # at least 10
max_pages = 5

[app]
app_id = 123456
installation_id = 7890123
private_key = "/etc/han/github-app.pem"
# api_base = "https://github.example/api/v3"   # GitHub Enterprise

[tasks]
label = "agent-ready"
# the team's shared agent pool (agents or services): any one of them claims the task
pool = ["@runner-1:home.example", "@runner-2:home.example"]
# to = ["@lars-coder:home.example"]  # optional extra addressees, merged into the pool
# pool and to both empty: issues are shown, no task is ever created
```

## `github.issue/1`

Published as the body of a `profile` stream item `{type: "profile", name: "github.issue/1", body}` on the
stream of a session that han-github opens per repository, titled `github:owner/repo` (it closes it on exit).
Like `han.status/1`, it is a **profile, not authority**: Policy never reads it, no claim or approval depends on
it, and a face must not present it as fact about HAN's own state. A face keeps, per `ref`, the issue from the
item with the highest `seq`.

```
{ repo: "github:owner/repo", seq,
  issues: [ { ref: "github:owner/repo#N", number, title, state: "open" | "closed",
              labels: [string], assignees: [string], url, updated_at, agent_ready: bool } ] }
```

- `seq`: a counter per repository and process run, one more on every item; at most 50 issues per item.
- Only issues that are new or changed since the last item are sent; the first poll sends all (up to `max_pages`).
- `ref` equals the `external` of a task and the `issue` of `han.status/1`, so a board joins them by that string.
- `agent_ready`: the issue is open and has the trigger label.
- `assignees` are GitHub logins, for display only. HAN assignment is a task.
- **Untrusted text.** `title`, `labels` and `assignees` come from GitHub users. They are cut (title 256
  characters, a label or login 64, 20 labels) and stripped of control characters; a face shows them as text,
  never markup or a link it follows, and an agent gets them only inside the data envelope (SR-54, SR-59).
  The task's `ask` embeds the title the same way (at most 200 characters) and is data to the agent, too.
  The `ask` also carries the issue **body**, because the agent's sandbox cannot reach GitHub: after the title line, between
  `----- BEGIN ISSUE TEXT (untrusted, ...) -----` and `----- END ISSUE TEXT -----`, with control characters removed
  (newlines and tabs stay), the delimiter text defused, and at most 4,096 bytes (cut with `[truncated]`). An empty body adds no
  block. The ask is fixed when the task is made: if the issue's body (or title) changes later, the existing task is **not**
  rewritten and the agent works from the text it was made with (it can ask for more context). `han-github request` has only
  the title you pass, so its ask has no body.

## Not here

Writes (comments, labels, PRs) and the finish flow are han-exec actions. The webhook relay is later; polling
with ETags is the MVP. Rate limits: this client does not read `Retry-After` yet; a failed poll is logged and retried on the next interval.

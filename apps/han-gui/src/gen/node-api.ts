// Generated from crates/han-node by `cargo test -p han-node --test parts` (set HAN_UPDATE_GEN=1 to rewrite).
// Do not edit by hand. These are the node API types: call(Command) and view(Query).

export type NodeError = { "code": "denied", 
/**
 * Free text, never normative.
 */
message: string, } | { "code": "invalid", 
/**
 * Free text.
 */
message: string, } | { "code": "not_found", 
/**
 * Free text.
 */
message: string, } | { "code": "unavailable", 
/**
 * Free text.
 */
message: string, 
/**
 * When waiting helps, how long.
 */
retry_after_ms: number | null, } | { "code": "conflict", 
/**
 * Free text.
 */
message: string, };

export type HomeRef = { 
/**
 * The ServerName, as put in `dialed` (G-08).
 */
server: string, 
/**
 * The node id of the home's sequencer.
 */
node: string, 
/**
 * Direct addresses, `ip:port`.
 */
addrs: Array<string>, 
/**
 * Relay URLs.
 */
relays: Array<string>, };

export type Command = { "type": "space_create", 
/**
 * The home that will sequence it.
 */
home: HomeRef, 
/**
 * The space's name (content of `space.create`).
 */
name: string, 
/**
 * `members` or `public`.
 */
visibility: string, 
/**
 * `invite`, `open` or `closed`.
 */
join: string, } | { "type": "space_join", 
/**
 * The ticket text.
 */
ticket: string, } | { "type": "home_pin", 
/**
 * The home ticket text.
 */
ticket: string, } | { "type": "space_follow", 
/**
 * The home of the space.
 */
home: HomeRef, 
/**
 * The space.
 */
space: string, } | { "type": "invite", 
/**
 * The space.
 */
space: string, 
/**
 * Seconds the ticket lives (default a day, at most 7 days).
 */
ttl_secs?: number, } | { "type": "member_set", 
/**
 * The space.
 */
space: string, 
/**
 * The actor.
 */
actor: string, 
/**
 * Its new role.
 */
role: string, } | { "type": "agent_create", 
/**
 * The agent's name, without `@` and server.
 */
name: string, 
/**
 * The server it lives on.
 */
server: string, 
/**
 * Its operator.
 */
operator: string, } | { "type": "agent_start", 
/**
 * The space.
 */
space: string, 
/**
 * The agent actor.
 */
agent: string, 
/**
 * The harness.
 */
harness: string, 
/**
 * The repository, `github:owner/repo`.
 */
repo?: string, 
/**
 * The issue, `github:owner/repo#123`.
 */
issue?: string, } | { "type": "agent_stop", 
/**
 * The session, or the agent actor.
 */
target: string, } | { "type": "task_create", 
/**
 * The space.
 */
space: string, 
/**
 * The assignees.
 */
to: Array<string>, 
/**
 * The ask (content).
 */
ask: string, 
/**
 * The work's external reference, `github:owner/repo#123`.
 */
external?: string, } | { "type": "task_update", 
/**
 * The space.
 */
space: string, 
/**
 * The task's entry id.
 */
task: string, 
/**
 * `accepted`, `progress`, `done`, `failed` or `declined`.
 */
state: string, } | { "type": "task_cancel", 
/**
 * The space.
 */
space: string, 
/**
 * The task's entry id.
 */
task: string, } | { "type": "session_open", 
/**
 * The space.
 */
space: string, 
/**
 * `auto` (default) or `guarded` (R-24: an agent that chooses guarding).
 */
steer_mode?: string, 
/**
 * The task it works.
 */
task?: string, 
/**
 * A title.
 */
title?: string, } | { "type": "session_close", 
/**
 * The space.
 */
space: string, 
/**
 * The `session.open` entry id.
 */
session: string, } | { "type": "session_input", 
/**
 * The space.
 */
space: string, 
/**
 * The session.
 */
session: string, 
/**
 * The text.
 */
text: string, } | { "type": "status_publish", 
/**
 * The space.
 */
space: string, 
/**
 * The status; its `seq` is set by the node when it is 0.
 */
status: StatusBody, } | { "type": "lease_set", 
/**
 * The space.
 */
space: string, 
/**
 * The paths, relative to the repository; a path ending in `/` covers what is under it.
 */
paths: Array<string>, 
/**
 * Seconds it lasts.
 */
ttl_secs: number, 
/**
 * The issue it is for.
 */
issue?: string, } | { "type": "device_offer" } | { "type": "device_pair", 
/**
 * The code the new device showed.
 */
code: string, } | { "type": "device_revoke", 
/**
 * The key id.
 */
key_id: string, } | { "type": "effect_approve", 
/**
 * The space.
 */
space: string, 
/**
 * The proposal's entry id.
 */
proposal: string, 
/**
 * A note.
 */
note?: string, } | { "type": "effect_cancel", 
/**
 * The space.
 */
space: string, 
/**
 * The proposal's entry id.
 */
proposal: string, } | { "type": "effect_propose", 
/**
 * The space.
 */
space: string, 
/**
 * Who may claim and run it.
 */
executor: string, 
/**
 * The plan as JSON text: `{action, target, args?}` (han.ts `Plan`; `action` is also the body's).
 */
plan: string, 
/**
 * The task it is charged to (R-24: the driving task, when a task drives the turn).
 */
task?: string, 
/**
 * The cost unit and amount, in a budgeted space.
 */
cost?: [string, number], 
/**
 * Whether a failed attempt may be retried.
 */
repeatable: boolean, 
/**
 * No claim at or after this time.
 */
expires_at?: string, } | { "type": "effect_claim", 
/**
 * The space.
 */
space: string, 
/**
 * The proposal.
 */
proposal: string, 
/**
 * One more than the number of earlier claims.
 */
attempt: number, 
/**
 * The space's `launchWindow` in seconds.
 */
window: number, } | { "type": "effect_outcome", 
/**
 * The space.
 */
space: string, 
/**
 * The claim's entry id.
 */
claim: string, 
/**
 * `succeeded`, `failed`, `cancelled` or `unknown`.
 */
result: string, 
/**
 * `yes`, `no` or `unknown`.
 */
started: string, 
/**
 * What it cost, with `started: yes` only.
 */
cost?: [string, number], } | { "type": "policy_set", 
/**
 * The space.
 */
space: string, 
/**
 * The whole policy as JSON text.
 */
policy: string, } | { "type": "stream_publish", 
/**
 * The space.
 */
space: string, 
/**
 * The session.
 */
session: string, 
/**
 * The profile's name, such as `han.timeline/1`.
 */
name: string, 
/**
 * The item's body as JSON text (at most 64 KiB with its envelope).
 */
body: string, };

export type CallResult = { "type": "receipt", 
/**
 * The space.
 */
space: string, 
/**
 * The entry's position in the log.
 */
seq: number, 
/**
 * The entry's id.
 */
id: string, 
/**
 * The receipt's time.
 */
time: string, } | { "type": "queued", 
/**
 * The outbox row.
 */
outbox_id: number, 
/**
 * The space.
 */
space: string, 
/**
 * The entry's id.
 */
id: string, } | { "type": "space", 
/**
 * The space.
 */
space: string, } | { "type": "ticket", 
/**
 * The text to pass on.
 */
text: string, } | { "type": "agent", 
/**
 * The actor id.
 */
actor: string, 
/**
 * The key id to register.
 */
key_id: string, 
/**
 * The public key (also the node id of the host that runs it).
 */
public_key: string, } | { "type": "device_code", 
/**
 * The code to type or scan on the existing device.
 */
code: string, 
/**
 * This device's node id.
 */
node_id: string, 
/**
 * Short check both screens show, derived from the code.
 */
check: string, } | { "type": "status", 
/**
 * The item's number on the session's stream.
 */
n: number, } | { "type": "done" };

export type Filter = { 
/**
 * Free text: every word must occur, case-insensitively, in the row's searchable text.
 */
text?: string, 
/**
 * Only these states (empty: any).
 */
states: Array<SessionState>, 
/**
 * Only rows that involve this actor (a person, an agent or a delegate).
 */
actor?: string, 
/**
 * Only rows about this issue.
 */
issue?: string, 
/**
 * Include closed sessions (a board shows live work by default).
 */
include_closed: boolean, };

export type Window = { 
/**
 * Which rows.
 */
filter: Filter, 
/**
 * At most this many rows (1 to 10,000).
 */
limit: number, 
/**
 * Rows after this key.
 */
cursor?: string, };

export type Group = "issue" | "person" | "state";

export type Target = { "kind": "issue", 
/**
 * The reference.
 */
issue: string, } | { "kind": "path", 
/**
 * The path.
 */
path: string, };

export type Scope = { "kind": "space", 
/**
 * The space.
 */
space: string, } | { "kind": "session", 
/**
 * The session's entry id.
 */
session: string, };

export type Query = { "type": "board", 
/**
 * The space.
 */
space: string, 
/**
 * How rows are grouped.
 */
group: Group, 
/**
 * Groups (the `group` label of their rows) the face has collapsed: their rows are left out of the
 * window and the total.
 */
collapsed: Array<string>, 
/**
 * The window.
 */
window: Window, } | { "type": "session", 
/**
 * The session's entry id.
 */
id: string, 
/**
 * The window.
 */
window: Window, } | { "type": "who_is_on", 
/**
 * The space; every followed space when absent.
 */
space?: string, 
/**
 * The issue or path.
 */
target: Target, 
/**
 * The window.
 */
window: Window, } | { "type": "overlaps", 
/**
 * The space.
 */
space: string, 
/**
 * The window.
 */
window: Window, } | { "type": "approvals", 
/**
 * The approving actor.
 */
actor: string, 
/**
 * The window.
 */
window: Window, } | { "type": "space_status", 
/**
 * The space.
 */
space: string, 
/**
 * The window.
 */
window: Window, } | { "type": "cost", 
/**
 * What to add up.
 */
scope: Scope, 
/**
 * The window.
 */
window: Window, } | { "type": "outbox", 
/**
 * Only this space; every space when absent.
 */
space?: string, 
/**
 * The window.
 */
window: Window, } | { "type": "join_progress", 
/**
 * The space.
 */
space: string, 
/**
 * The window.
 */
window: Window, } | { "type": "node_status", 
/**
 * The window.
 */
window: Window, } | { "type": "timeline", 
/**
 * The session.
 */
session: string, 
/**
 * Only points at or before this time (time travel).
 */
at?: string, 
/**
 * The window (pages of up to 2,000 points).
 */
window: Window, } | { "type": "patches", 
/**
 * The session.
 */
session: string, 
/**
 * The earlier point; the session's base when absent.
 */
from?: string, 
/**
 * The later point.
 */
to: string, 
/**
 * The window.
 */
window: Window, } | { "type": "inputs", 
/**
 * The session.
 */
session: string, 
/**
 * The window (its cursor is the key of the last input handled).
 */
window: Window, } | { "type": "tasks", 
/**
 * The space.
 */
space: string, 
/**
 * Only tasks with this assignee.
 */
assignee?: string, 
/**
 * The window.
 */
window: Window, } | { "type": "guard", 
/**
 * The space.
 */
space: string, 
/**
 * The actor.
 */
actor: string, 
/**
 * The window.
 */
window: Window, } | { "type": "effects", 
/**
 * Only this space; every followed space when absent.
 */
space?: string, 
/**
 * Only effects whose executor is this actor.
 */
executor?: string, 
/**
 * Include effects in a final phase.
 */
include_done: boolean, 
/**
 * The window.
 */
window: Window, } | { "type": "change_set", 
/**
 * The space.
 */
space: string, 
/**
 * The proposal's entry id.
 */
proposal: string, 
/**
 * The window.
 */
window: Window, };

export type SessionState = "working" | "waiting-approval" | "blocked" | "idle" | "done" | "stale" | "failed";

export type PlanItem = { 
/**
 * The step.
 */
text: string, 
/**
 * Whether it is done.
 */
done: boolean, };

export type FileTouch = { 
/**
 * The path, relative to the repository.
 */
path: string, 
/**
 * `[start, len]` pairs.
 */
hunks: Array<[number, number]>, };

export type Tests = { 
/**
 * Passing.
 */
pass: number, 
/**
 * Failing.
 */
fail: number, };

export type Tokens = { 
/**
 * Tokens in.
 */
in: number, 
/**
 * Tokens out.
 */
out: number, 
/**
 * Cost, in integer minor units.
 */
cost: number, };

export type StatusBody = { 
/**
 * The `session.open` entry id.
 */
session: string, 
/**
 * The session's actor.
 */
agent: string, 
/**
 * The actor whose task drives the session.
 */
delegate_of?: string, 
/**
 * The agent program.
 */
harness: string, 
/**
 * The work's external reference.
 */
issue?: string, 
/**
 * The repository.
 */
repo?: string, 
/**
 * The commit the worktree started from.
 */
base?: string, 
/**
 * The state the host claims.
 */
state: SessionState, 
/**
 * The current step.
 */
step: string, 
/**
 * The tool in use.
 */
tool?: string, 
/**
 * The plan.
 */
plan?: Array<PlanItem>, 
/**
 * Files changed so far.
 */
files: Array<FileTouch>, 
/**
 * Test counts.
 */
tests?: Tests, 
/**
 * Token use and cost.
 */
tokens?: Tokens, 
/**
 * A short summary.
 */
summary?: string, 
/**
 * The sandbox the host runs the session in. Not a member of `han.status/1` proper (unknown members are
 * ignored, so it can ride along): a node-side extension the board shows (SR-99).
 */
sandbox?: Sandbox, 
/**
 * A counter per session, starting at 1.
 */
seq: number, 
/**
 * The host's time.
 */
at: string, };

export type OverlapKind = "issue_claim" | "path_lease" | "same_file" | "hunk_range";

export type OverlapParty = { 
/**
 * The actor.
 */
actor: string, 
/**
 * Its session, when the party is a session.
 */
session?: string, };

export type BoardRow = { 
/**
 * Orders and identifies the row: the group, then the session.
 */
key: string, 
/**
 * The space.
 */
space: string, 
/**
 * The group the row is in (issue, person or state, as the query asked).
 */
group: string, 
/**
 * The session's entry id.
 */
session: string, 
/**
 * The session's actor.
 */
agent: string, 
/**
 * The person behind it: the delegating actor, the task's author, or the actor itself.
 */
person: string, 
/**
 * The harness.
 */
harness?: string, 
/**
 * The issue.
 */
issue?: string, 
/**
 * The repository.
 */
repo?: string, 
/**
 * The state, with silence applied.
 */
state: SessionState, 
/**
 * The current step.
 */
step: string, 
/**
 * The tool in use.
 */
tool?: string, 
/**
 * Files changed so far.
 */
files: Array<FileTouch>, 
/**
 * Kinds of overlap this session is part of (none: no badge).
 */
overlaps: Array<OverlapKind>, 
/**
 * Test counts.
 */
tests?: Tests, 
/**
 * Token use and cost.
 */
tokens?: Tokens, 
/**
 * A short summary.
 */
summary?: string, 
/**
 * The sandbox, when the host reports one.
 */
sandbox?: Sandbox, 
/**
 * This node's steady clock (ms) when the last item arrived; a face computes the age from its own clock.
 */
last_active_ms: number, 
/**
 * Whether the session is closed.
 */
closed: boolean, 
/**
 * The session waits on this device's actor (blocked or waiting for approval on its work).
 */
needs_me: boolean, 
/**
 * This device's actor may steer the session. The home's Policy decides when it is tried; this is the
 * node's reading of the space's `sessions.steer` level.
 */
can_steer: boolean, 
/**
 * This device's actor may call the session's work off.
 */
can_cancel: boolean, };

export type SessionRow = { 
/**
 * The session's entry id.
 */
key: string, 
/**
 * The space.
 */
space: string, 
/**
 * The session's actor.
 */
actor: string, 
/**
 * The receipt time of `session.open`.
 */
opened: string, 
/**
 * The task it works.
 */
task?: string, 
/**
 * The task's external reference, if it has one.
 */
external?: string, 
/**
 * Whether it is closed.
 */
closed: boolean, 
/**
 * The state, with silence applied.
 */
state: SessionState, 
/**
 * The latest status item.
 */
status?: StatusBody, 
/**
 * This node's steady clock (ms) when the last item arrived.
 */
last_active_ms: number, };

export type WhoRow = { 
/**
 * Orders and identifies the row.
 */
key: string, 
/**
 * The space.
 */
space: string, 
/**
 * The actor.
 */
actor: string, 
/**
 * The session, if the row is one.
 */
session?: string, 
/**
 * How the actor is on it: `claim`, `session` or `lease`.
 */
via: string, 
/**
 * What matched: the issue or the path.
 */
subject: string, 
/**
 * The session's state, for a session.
 */
state?: SessionState, };

export type OverlapRow = { 
/**
 * Orders and identifies the row.
 */
key: string, 
/**
 * The space.
 */
space: string, 
/**
 * The kind.
 */
kind: OverlapKind, 
/**
 * 1 or 2.
 */
tier: number, 
/**
 * The issue or path.
 */
subject: string, 
/**
 * The actors involved, in order.
 */
parties: Array<OverlapParty>, 
/**
 * What overlaps, in words.
 */
detail: string, };

export type ApprovalRow = { 
/**
 * Orders and identifies the row: the space and the proposal.
 */
key: string, 
/**
 * The space.
 */
space: string, 
/**
 * The proposal's entry id.
 */
proposal: string, 
/**
 * The proposer.
 */
proposer: string, 
/**
 * The executor.
 */
executor: string, 
/**
 * The action.
 */
action: string, 
/**
 * The digest of the plan.
 */
plan_digest: string, 
/**
 * Authors of approvals so far.
 */
approvals: Array<string>, 
/**
 * The cost unit and amount, when the proposal has one.
 */
cost?: [string, number], 
/**
 * No claim at or after this time.
 */
expires_at?: string, 
/**
 * The seq of the proposal.
 */
seq: number, 
/**
 * The asking actor may give an approval that is worth trying (a member, not the proposer, not yet
 * approved); Policy has the last word when it is appended.
 */
can_approve: boolean, 
/**
 * Who must approve: the actors and roles of the approval rule that applies, or the owners by default.
 */
approvers: Array<string>, 
/**
 * Why the effect is gated: which rule or default required an approval.
 */
why_gated: string, };

export type EffectRow = { 
/**
 * Orders and identifies the row: the space and the proposal's seq, zero-padded.
 */
key: string, 
/**
 * The space.
 */
space: string, 
/**
 * The proposal's entry id.
 */
proposal: string, 
/**
 * The proposer.
 */
proposer: string, 
/**
 * The executor.
 */
executor: string, 
/**
 * The action.
 */
action: string, 
/**
 * The digest of the plan.
 */
plan_digest: string, 
/**
 * The plan as JSON text, from the proposal's content (absent when redacted or not delivered).
 */
plan?: string, 
/**
 * The task it is charged to.
 */
task?: string, 
/**
 * `open`, `claimed` or `done` (a final phase, as far as this device folds it).
 */
phase: string, 
/**
 * Authors of the open approvals.
 */
approvals: Array<string>, 
/**
 * How many claims it has had.
 */
claims: number, 
/**
 * The latest claim's entry id.
 */
last_claim?: string, 
/**
 * The space's `launchWindow` in seconds.
 */
launch_window: number, 
/**
 * Whether a failed attempt may be retried.
 */
repeatable: boolean, 
/**
 * The cost unit and amount, when the proposal has one.
 */
cost?: [string, number], 
/**
 * No claim at or after this time.
 */
expires_at?: string, 
/**
 * The seq of the proposal.
 */
seq: number, 
/**
 * The head of the space when the row was made (an executor claims each effect at most once per head).
 */
head: number, };

export type GuardRow = { 
/**
 * Orders and identifies the row: the space and the actor.
 */
key: string, 
/**
 * The space.
 */
space: string, 
/**
 * The actor.
 */
actor: string, 
/**
 * It chose guarding, or is forced.
 */
guarded: boolean, 
/**
 * The owners' `forceGuarded` is on (it applies to non-humans).
 */
forced: boolean, 
/**
 * The head the answer is at.
 */
head: number, };

export type InputRow = { 
/**
 * Orders and identifies the row: the seq, zero-padded.
 */
key: string, 
/**
 * The space.
 */
space: string, 
/**
 * The session.
 */
session: string, 
/**
 * The input's entry id.
 */
id: string, 
/**
 * The input's seq.
 */
seq: number, 
/**
 * Its author.
 */
author: string, 
/**
 * Its text (absent when redacted).
 */
text?: string, };

export type TaskRow = { 
/**
 * Orders and identifies the row: the space and the seq, zero-padded.
 */
key: string, 
/**
 * The space.
 */
space: string, 
/**
 * The task's entry id.
 */
task: string, 
/**
 * The seq of the task.
 */
seq: number, 
/**
 * Its author.
 */
author: string, 
/**
 * Its assignees.
 */
to: Array<string>, 
/**
 * The work's external reference.
 */
external?: string, 
/**
 * The ask (content), when delivered.
 */
ask?: string, 
/**
 * Whether it is still open.
 */
open: boolean, };

export type SpaceLink = "connecting" | "live" | "offline" | "rejected";

export type SpaceStatusRow = { 
/**
 * The space.
 */
key: string, 
/**
 * The space.
 */
space: string, 
/**
 * The home's ServerName.
 */
home: string, 
/**
 * The link.
 */
link: SpaceLink, 
/**
 * The last verified seq.
 */
head: number, 
/**
 * Open sessions.
 */
sessions: number, 
/**
 * Actors holding the role owner or member.
 */
members: number, 
/**
 * Entries signed here and not yet ordered by the home.
 */
outbox_pending: number, 
/**
 * Signals received (for example joins), a counter.
 */
signals: number, 
/**
 * Why the space was rejected or is offline.
 */
error?: string, };

export type CostRow = { 
/**
 * Orders and identifies the row: `0` for the total, `1/<session>` for a session.
 */
key: string, 
/**
 * The space.
 */
space: string, 
/**
 * The session, or none for the total.
 */
session?: string, 
/**
 * The session's actor.
 */
agent?: string, 
/**
 * Tokens in.
 */
tokens_in: number, 
/**
 * Tokens out.
 */
tokens_out: number, 
/**
 * Cost, in the unit of the space's budget.
 */
cost: number, 
/**
 * That unit, if the space has a budget.
 */
unit?: string, };

export type SandboxBackend = "mxc" | "bwrap" | "none";

export type Fidelity = "enforced" | "advisory";

export type Sandbox = { 
/**
 * The backend.
 */
backend: SandboxBackend, 
/**
 * How far it can be trusted.
 */
fidelity: Fidelity, };

export type OutboxRow = { 
/**
 * Orders and identifies the row: the outbox id, zero-padded.
 */
key: string, 
/**
 * The outbox id `Queued` returned.
 */
outbox_id: number, 
/**
 * The space.
 */
space: string, 
/**
 * The entry's id.
 */
entry_id: string, 
/**
 * The entry's kind.
 */
kind: string, 
/**
 * `pending`, `sent` or `rejected`.
 */
state: string, 
/**
 * Delivery attempts so far.
 */
attempts: number, 
/**
 * The receipt's seq once sent.
 */
seq?: number, 
/**
 * Why it was refused, or the last reason a try failed.
 */
error?: string, };

export type JoinStage = "connecting" | "waiting_for_admission" | "syncing" | "ready" | "rejected";

export type JoinProgressRow = { 
/**
 * The space.
 */
key: string, 
/**
 * The space.
 */
space: string, 
/**
 * The stage.
 */
stage: JoinStage, 
/**
 * The actor an owner must admit (this device's actor).
 */
actor: string, 
/**
 * The last verified seq.
 */
head: number, 
/**
 * Why it is not further along.
 */
error?: string, };

export type RoleInfo = { 
/**
 * The instance name.
 */
name: string, 
/**
 * `running` or `exited`.
 */
state: string, };

export type NodeStatusRow = { 
/**
 * Always `node`.
 */
key: string, 
/**
 * The actor this device acts as.
 */
actor: string, 
/**
 * This device's node id (its public key).
 */
node_id: string, 
/**
 * The node's version.
 */
version: string, 
/**
 * Followed spaces.
 */
spaces: number, 
/**
 * Spaces that are following live.
 */
spaces_live: number, 
/**
 * Entries waiting in the outbox.
 */
outbox_pending: number, 
/**
 * Supervised roles.
 */
roles: Array<RoleInfo>, };

export type TimelinePointRow = { 
/**
 * Orders and identifies the row: the point's position, zero-padded.
 */
key: string, 
/**
 * The session.
 */
session: string, 
/**
 * The point's id.
 */
point: string, 
/**
 * A label (a step, a checkpoint, a tool call).
 */
label: string, 
/**
 * When, as the host saw it.
 */
at: string, 
/**
 * Files changed at this point.
 */
files: number, };

export type PatchStatus = "added" | "modified" | "deleted";

export type FilePatchRow = { 
/**
 * Orders and identifies the row: the path.
 */
key: string, 
/**
 * The path.
 */
path: string, 
/**
 * Added, modified or deleted.
 */
status: PatchStatus, 
/**
 * Unified diff text.
 */
patch: string, 
/**
 * Lines added.
 */
added: number, 
/**
 * Lines removed.
 */
removed: number, };

export type Row = { "view": "board", "row": BoardRow } | { "view": "session", "row": SessionRow } | { "view": "who_is_on", "row": WhoRow } | { "view": "overlap", "row": OverlapRow } | { "view": "approval", "row": ApprovalRow } | { "view": "space_status", "row": SpaceStatusRow } | { "view": "cost", "row": CostRow } | { "view": "outbox", "row": OutboxRow } | { "view": "join_progress", "row": JoinProgressRow } | { "view": "node_status", "row": NodeStatusRow } | { "view": "timeline_point", "row": TimelinePointRow } | { "view": "file_patch", "row": FilePatchRow } | { "view": "effect", "row": EffectRow } | { "view": "guard", "row": GuardRow } | { "view": "input", "row": InputRow } | { "view": "task", "row": TaskRow };

export type DiffOp = { "op": "insert", 
/**
 * Where it goes.
 */
index: number, 
/**
 * The row.
 */
row: Row, } | { "op": "update", 
/**
 * The new row (same key).
 */
row: Row, } | { "op": "remove", 
/**
 * Its key.
 */
key: string, } | { "op": "move", 
/**
 * Its key.
 */
key: string, 
/**
 * Its new position.
 */
index: number, };

export type ViewEvent = { "type": "snapshot", 
/**
 * The rows.
 */
rows: Array<Row>, 
/**
 * How many rows match the filter in all (collapsed groups left out).
 */
total: number, 
/**
 * How many matching rows come before the first of this window.
 */
offset: number, 
/**
 * The key to put in `cursor` for the next page, when rows follow this window.
 */
next_cursor?: string, } | { "type": "diff", 
/**
 * The changes.
 */
ops: Array<DiffOp>, 
/**
 * The new total.
 */
total: number, 
/**
 * The new offset.
 */
offset: number, 
/**
 * The new cursor of the page after this window.
 */
next_cursor?: string, } | { "type": "resnapshot", 
/**
 * The rows.
 */
rows: Array<Row>, 
/**
 * How many rows match the filter in all.
 */
total: number, 
/**
 * How many matching rows come before the first of this window.
 */
offset: number, 
/**
 * The key for the next page.
 */
next_cursor?: string, } | { "type": "failed", 
/**
 * Why.
 */
error: NodeError, };

# R33: deterministic in-flight state and `han_query`

Status: **design for round 33.** Nothing here is normative yet, and nothing is built beyond what the [fact catalog](../../../docs/inflight-state.md) marks `exists`. Decided principles (2026-10-15): facts are observed, not claimed; answers cost zero tokens to produce; the state is derived data. This builds on [r33-land-profile.md](r33-land-profile.md) (`han.land/1`: changesets, lanes, conflicts as data) and PLAN.md §12 ("Views as derived data", "Version control is pluggable", "Conflicts as data", "Liveness"), and does not repeat them.

## Problem

Agents coordinate today by reading each other: `team_read` hands an agent another session's stream as text, and the agent spends tokens working out whether that session touches its code. At thousands of agents per operator, many spaces and high event rates, that cost grows with every pair of agents. Most of what agents ask each other is mechanical: who is in this file, who changed this function, does my branch conflict with anything, has anyone seen this error, what am I waiting on. A host or node can answer each of these from what it already observes, with no model on the answering side.

Goal: a set of facts about work in flight, each derived by the host or node from what it sees itself, indexed incrementally on every node that follows the space, and answered through one typed MCP tool and standing subscriptions. The same inputs give the same answer on every node.

## 1. Principles

1. **Observed, not claimed.** A fact is derived from the worktree diff, a parser, the proxy's ledger, tool output the host sees, git, or the log. Agent-declared intent (planned paths, a "doing" line) is **advisory**: it is a separate member, faces label it, and no query mixes it into observed rows unless asked to (`advisory: true`, rows marked).
2. **Trust levels.** `log`, `observed`, `reported`, `advisory`, as defined in the catalog. Every row a query returns says which level it has. Observed facts are still a host's word and carry no authority: Policy never reads them, and no approval, claim or steer depends on one (profiles/README.md).
3. **Zero tokens to answer.** The node answers from in-memory indexes. Text only appears where a fact is text (an error's normalised message), and then only bounded and inside the data envelope (R-24, SR-54).
4. **Push, not poll.** A standing query is a node view (snapshot, then diffs: PLAN §5). The host hands changes to the agent between turns.
5. **Derived data.** Every index is a declaration over the log, the latest profile items and the timelines (PLAN §12 "Views as derived data"). Each is a pure function of its inputs (§6).
6. **Version control through `Vcs`.** Host-side derivations reach git only through the `Vcs` trait (crates/han-vcs), so Sapling or Scion can supply the same facts.

## 2. What exists

Files touched with hunks, session state, test counts, claims and leases, approvals, spend, base, the worktree history and the session items all exist. The catalog's entries F1–F9 give, for each, the derivation, shape, index and queries, with code locations. Two gaps matter for scale:
- **Overlaps are recomputed in full.** `SpaceModel::overlaps` rebuilds tiers 1–2 whenever `rev` moves. The cost is O(Σ k²) over the sessions k touching each path, and at 10,000 busy sessions `rev` moves many times a second. §4 makes it incremental.
- **Tier 3 is a stub.** `OverlapConflict{checked, conflicts}` is in the row, but the node never sends it. `Vcs::merge_simulate` exists and returns `{clean, conflicts: [path]}`, but nothing calls it outside tests.

## 3. The facts

The catalog lists every fact. This section decides the new ones (F10–F20). "Host" means han-host for the session. Every host-side extractor runs on the masked text the mirror records (SR-52), never on unmasked bytes.

### 3.1 Files touched (F1, extended)

- **Shape.** `files: [{path, hunks, from?, to?}]`. `from` and `to` are the `Vcs` backend's content ids of the file at the base and now. They are already computed (`FileChange.old_hash`, `new_hash`); `touches()` drops them today.
- **Why.** Equal `to` on both sides means no conflict. Different `from` means the two sessions started from different bases (base skew). Peer conflict prediction (§3.8) needs both.
- **Update.** Unchanged: after each tool call and at the end of each turn.
- **Index.** Per space, a sorted map `(repo, path) → postings{session, hunks, from, to}`, plus an inverse map `session → paths`. A glob is compiled to its literal prefix and a matcher, and the matcher runs only over the prefix's range. An update applies the difference between the session's old and new path sets: O(Δ log P).
- **Queries.** `touching(path_glob)`, `who_is_on(path)`.

### 3.2 Symbols touched (F10)

- **Derivation (host).** For each changed text file, the host parses the base version (cached by content id) and the current version with tree-sitter, using each grammar's standard `tags.scm` definition and reference captures. Whether a definition counts:
  - `modified`: a definition in the new version whose range meets a new-side hunk;
  - `added`: its qualified name is absent from the base;
  - `removed`: it is absent from the new version.
- **Refs.** References are the identifiers referenced inside changed hunks (for `who_depends_on`).
- **Update.** With `files`, debounced to the mirror's quiet period. Parsing is incremental: tree-sitter edits come from the mirror's line edits.
- **Shape.**
  ```
  {fact: "symbols", extractor: "tags/1", grammars: "<digest of grammar versions>",
   files: [{path, lang, defs: [{qname, kind, change: "added"|"modified"|"removed", range: [start, len]}],
            refs: [qname...], status?: "unsupported"|"too_large"|"timeout"|"parse_error"}]}
  ```
  - `qname` is the container chain joined by `::` (for example `mirror::Mirror::open`), at most 256 bytes.
  - `kind` is the grammar's capture kind: function, method, class, struct, trait, interface, enum, constant, module, type.
  - Bounds: at most 2,000 defs and 2,000 refs per session.
- **Languages.** First: Rust, TypeScript/TSX, JavaScript, Python, Go (the dogfood is Rust and TS). Next: Java, C, C++, C#, Ruby, Kotlin, Swift.
- **Structured files** get symbols too, without tree-sitter:
  - Markdown headings, as `#`-joined paths;
  - TOML, JSON and YAML key paths (for example `dependencies.serde`).
- **Fallback.** Any other file has `status: "unsupported"`. It keeps its file-level and hunk-level facts, so `touching` still finds it.
- **Skipped.** Generated, vendored and minified files (by `.gitattributes` `linguist-generated`, or a line longer than 4 KiB).
- **Trust and right.** Observed; **read**, because names reveal code.
- **Index.** `(repo, lang, qname) → postings{session, change, path}`, plus a reverse map from refs to sessions. The keys are interned. An update replaces one session's set: O(Δ log S).
- **Queries.** `touching_symbol(sym)`. `sym` is an exact qname, or a prefix ending in `::` that matches everything under it.

### 3.3 Public API surface (F11)

- **Derivation (host).** The subset of F10 defs that the language's rules make public:
  - Rust `pub` outside a `pub(crate)`/private module;
  - TS/JS `export`;
  - Python names in `__all__`, else names without a leading `_`;
  - Go names with a capital first letter.
- **Signature hash.** For each public def, the hash of its declaration node with the body removed and the whitespace normalised. A change counts as `signature` when that hash differs from the base, and as `body` otherwise.
- **Schema files** are API too: `.proto`, OpenAPI documents, GraphQL schemas, and files the space marks with a path glob (`policy.api: [glob]`, decided per space).
- **Shape.** `{fact: "api", items: [{qname, path, lang, change: "added"|"removed"|"signature"|"body", sigFrom?, sigTo?}]}`, at most 500.
- **Trust and right.** Observed; read.
- **Index.** `qname → session`.
- **Queries.** `who_depends_on(api)`. It joins the API index with the refs index (§3.2): the in-flight sessions whose changed code references `api` by name.
  - The match is by name, without type resolution. Rows say `match: "name"`.
  - Dependents on `main` that nobody is changing are out of scope: that needs a code-search index of the base (SCIP or an LSP), a later step.

### 3.4 Dependencies and lockfiles (F12)

- **Derivation (host).** Parsers for each manifest and lockfile, run over the base and the new version:
  - `Cargo.toml`/`Cargo.lock`;
  - `package.json` with npm, pnpm, yarn or bun lockfiles;
  - `go.mod`/`go.sum`;
  - `pyproject.toml` with `uv.lock`/`poetry.lock`, and `requirements*.txt`.
- **Shape.** `{fact: "deps", items: [{ecosystem, name, from?, to?, change: "add"|"remove"|"bump", manifest, direct: bool}]}`, at most 500.
- **Trust and right.** Observed; **watch**. Package names rarely reveal code, and an operator wants to see a new dependency early. The right is per space: see open question 2.
- **Index.** `(ecosystem, name) → sessions`.
- **Queries.**
  - `deps_changed(name?)`.
  - A lockfile counts as touched for F1. For conflict prediction (§3.8) it is classified `lockfile`, whose fix is to regenerate, so it ranks below code conflicts.
  - PLAN §12 already puts manifests and lockfiles in "never auto"; F12 shows the reason for that gate early.

### 3.5 Build and test state per test id (F13)

- **Derivation (host).** Test results are read from structured output when the run produces it:
  - libtest JSON, nextest JUnit;
  - pytest `--junitxml`;
  - jest and vitest JSON reporters;
  - `go test -json`.
- **When there is none,** the host falls back to today's counts (F4).
- **Who reads the files.** The host sets the reporter flags or environment only where the harness lets it. It reads report files only inside the workspace, after the tool call ends.
- **Shape.** `{fact: "tests", runner, run: <tool call id>, at_seq, counts: {pass, fail, skip}, failing: [{id, fp?}], flaky: [id], changed: [{id, from, to}]}`.
  - `id` is the runner's own test id, at most 256 bytes, in a restricted character class.
  - Only failing tests, flaky tests and tests whose status changed since the last run are listed (at most 1,000), never every passing test.
  - `fp` links a failing test to its error fingerprint (§3.6).
- **Builds** are the same fact with `runner: "build:<tool>"` (cargo, tsc, go build) and errors in place of failing tests.
- **Trust and right.** Observed; read (test names reveal code).
- **Index.** `test id → sessions` with their last status; per session, the latest run.
- **Queries.** `failing(test_id?)`, `tests(session)`.

### 3.6 Error fingerprints (F14)

- **Derivation (host).** The host extracts diagnostics from the output of each completed tool call: rustc/cargo JSON and human output, tsc, eslint, go, Python tracebacks, and test-failure messages. A versioned normaliser (`errnorm/1`) then:
  - makes paths relative to the workspace;
  - replaces line and column numbers, hex addresses, temp directories, timestamps, UUIDs and numbers in messages with placeholders;
  - keeps the error code (`E0308`, `TS2345`), the message template, and the innermost frame's symbol.
- **Fingerprint.** `fp = sha256("errnorm/1" || tool || code || template || frame)`, shown as base32, 26 characters.
- **Shape.** `{fact: "errors", items: [{fp, tool, code?, template, path?, symbol?, count, first_seq, last_seq}]}`.
  - `template` is at most 256 characters and masked.
  - At most 64 distinct fingerprints per session.
- **Resolution (node-derived, no wire).** The node keeps a fingerprint's open occurrences per session. An occurrence is **resolved** when a later run of the same tool in that session ends without the fingerprint and with no new error. The node then records `{session, seen_seq, resolved_seq, files: [path], symbols: [qname]}`: what F1 and F10 show changed between the two items. Two things make a resolution stronger:
  - the session later lands through `han.land/1` (`proposal` is filled; that is log trust);
  - the fingerprint stays absent for the rest of the session.
- **Ranking.** Resolutions are ranked landed first, then by how many sessions show the same fix files.
- **Trust and right.** The occurrences are observed. The resolution link is node-derived from observed facts and carries the trust of what it is derived from. Read right.
- **Index.** `fp → {occurrences: [session], resolutions: top 8}`, kept for a time the space sets (default 30 days), after sessions close. A resolution keeps only ids, never content.
- **Queries.** `seen_error(fp | {tool, code, template})`. An agent that has raw error text passes it to the tool, and the node normalises it with the same `errnorm/1`.
  - The answer is ids and counts. To read the fix, the agent uses `team_read`, enveloped and under the read right.

### 3.7 Base and freshness (F16)

- **Derivation (host).** Inputs:
  - the target ref's head, from the webhook relay (PLAN §12), a `han.land/1` lane item, or a fetch at most every 60 s as the fallback;
  - the base;
  - `Vcs`: `behind` (commits on the target since the base), `ahead` (the session's commits), and `moved` (how many of the session's paths the target changed since the base).
- **Landability.** `rebase` applies r33-land-profile §1's path-disjoint check to the session's current delta: `"path-disjoint"` predicts that the lane will carry the change under its approval; `"overlap"` predicts that it will be gated again.
- **Shape.** New `han.status/1` member `fresh: {target, head, behind, ahead, moved, rebase: "path-disjoint"|"overlap"|"unknown", checked_seq}`.
- **Trust and right.** Observed; watch.
- **Index.** `target → sorted by behind`.
- **Queries.** `stale(base?, behind_gt?, rebase?)`.

### 3.8 Predicted conflicts (F17, F18)

This reuses the land profile; it does not define a second conflict model.

- **Record.** The record is r33-land-profile §5's: `{clean, conflicts: [{path, kind, base?, ours?, theirs?, hunks?}], against}`. In flight, `changeset` is replaced by `subject: {session, tree}`, and `onto` names a commit (against main) or `{session, tree}` (against a peer). A new conflict kind, `lockfile`, joins the §5 kinds (§3.4).
- **Snapshots.** In-flight work is uncommitted, so the host takes a tree snapshot (a tree object, no ref, no commit). This needs one new `Vcs` method, `snapshot_tree(dir) → tree id` (for git, a temporary index plus `write-tree`). It moves no ref and touches neither the index nor the working tree.
- **F17, against main (host, observed).** When the target's head moves, or the session's `files` change after a quiet period of at least 10 s, the host runs `Vcs::merge_simulate(snapshot, head)`.
  - It uses the richer return shape the land study gives `merge_simulate`.
  - It publishes the record as `han.inflight/1` `conflicts`, with `against` empty.
  - The node fills `against` from the log and `han.land/1`: the landed proposals that touched those paths since the base, and their tasks.
- **F18, against peers (node, derived).** Candidates come only from the tier-2 index (§3.1): pairs that touch the same path. That keeps the work proportional to real overlap, not to n². For each candidate path:
  - **Same base.** When both sides have the same `from`, the node of either party runs a pinned three-way line merge, `han-line3/1`, on the two sides' current contents from their `han.timeline/1` histories (masked text, the read right). It is Myers diff with a fixed tie-break, then diff3 chunking. Overlapping or adjacent changed regions with different contents conflict.
  - **Different bases.** When the sides' `from` differ, the path is `base_skew`: unchecked, and left to F17 and the lane.
  - **Sharing.** The node that computes a record MAY publish it as a `han.inflight/1` `conflicts` item on its own session's stream, so the other party's node does not recompute it. Any node can check it, since the inputs are content ids and the algorithm is pinned.
- **The tier-3 row.** F18 fills the existing `OverlapConflict{checked, conflicts}` in the row (tier 3, PLAN §6.6), and the row links the full record.
- **Handover.** Once the session's changeset is proposed, the `han.land/1` items (state, `reason: conflict`, the conflict BlobRef) are the facts. The in-flight records for that session become `superseded` and the query returns the lane's record. The lane's `merge_simulate` is the truth. Predictions are deterministic but advisory in precision: `han-line3/1` and git's merge can disagree, and rows say `predicted: true`.
- **Trust and right.** F17 is observed; F18 is derived. Both are read, since hunks reveal code. The tier-2 fact that two sessions share a path is still watch, as today.
- **Queries.** `conflicts(session | branch, with?: main | peers | all)`.

### 3.9 Blocked-on (F15)

- **Derivation (node, no wire).** The node folds one list per session from:
  - effects the session's actor proposed that wait for approval, with who may approve them (from `ApprovalRow.needed`, log trust);
  - `han.land/1` states with something pending, with their deadline and the `stalled` flag;
  - the host's `waiting-approval`, `blocked` and "waiting for input" (observed);
  - `dependsOn` proposals that have not landed;
  - questions to other agents, when agent questions exist (PLAN §6.6).
- **Shape.** `{session, on: [{kind: "approval"|"input"|"lane"|"dependency"|"question", ref, who: [actor], since_seq, deadline?, stalled: bool, trust}]}`.
- **Index.** `session → on[]`, and the inverse `actor → [(session, kind, ref)]`.
- **Queries.** `blocked_on(actor)` (what waits on this actor), `blocking(session)`.

### 3.10 Claims and leases (F5)

They exist. One change: the lease and path indexes share one path trie, so `claims(path_glob)` returns leases, open issue claims and in-flight `effect.claim` fences for a path in one query. Lease expiry stays time-driven through the explicit `now` (§6).

### 3.11 Spend (F7)

It exists. Rows carry their trust: `observed` from the proxy, `reported` from ACP. The queries are `spend(session | agent | space)` and `top_spenders(k)`. Whether a space carries its own price table remains SR-41's round-33 question, and is not decided here.

### 3.12 Intent (F19, advisory)

- **Shape.** New `han.status/1` member `intent: {paths?: [glob], doing?: string}`, set by the agent through a `han_intent` MCP tool.
  - `doing` is at most 200 characters; at most 32 globs.
- **Use.** The node indexes `paths` in the path trie with `trust: advisory`. `touching(glob, advisory: true)` includes these rows, marked.
- **Why.** Agents can say where they are about to go before they touch anything, which observed facts can't show.

## 4. Indexes

- **Where.** In han-node, one set of indexes per followed space, inside the `SpaceModel`. Each one is a declared derived view (PLAN §12): inputs, a key, and an update function that takes one item's difference. Nothing is shared across spaces: two spaces have two indexes even on one node.
- **Inputs.**
  - verified log records;
  - the latest item per `(session, profile, fact)` by `seq`;
  - `han.timeline/1` histories, read on demand for F18 only;
  - `now`.
- **Incremental.** Each status or `han.inflight/1` item replaces its session's previous one. The index applies the difference between the old and new sets, then emits row diffs to the views that depend on the keys it changed. Tier-2 overlaps become incremental: a session's changed paths re-pair only with the postings on those paths.

**Size at 10,000 live sessions in one space** (assumed per session: 20 files, 40 symbols and refs, 10 API items, 5 deps, 50 test ids listed, 20 fingerprints):

| Index | Postings | Estimate |
|---|---|---|
| path trie (files and intent) | 200k | 25 MB |
| symbols and refs | 400k | 40 MB |
| API, deps | 150k | 12 MB |
| tests | 500k | 45 MB |
| errors (with 30 days of closed sessions) | 1M | 60 MB |
| blocked-on, claims, freshness | 30k | 3 MB |
| latest raw items (for rebuild and diffs) | 10k × ~16 KiB | 160 MB |
| **total** | | **≈ 350 MB per space**; target ≤ 512 MB |

- **CPU.** At an event rate of 2,000 items/s per space (each session updates every 5 s), one update costs about 50 µs per index touched. That is about 0.1 core per space. Peer merges (F18) run only on candidate paths, at most 4 at a time per node, and only for the node's own sessions.
- **Catching up.**
  - A node that falls behind syncs the log as today.
  - Streams have no backlog, but each `han.status/1` and `han.inflight/1` item is the full state for its `(session, fact)`, so the latest item is enough. Hosts re-send every fact at least every 60 s (the heartbeat that keeps a session from going `stale`), and when a new watcher appears.
  - Until a session's latest items arrive, its rows say `complete: false`.
  - A node persists an index checkpoint `{log head, per-session seq vector, digest}`. On restart it loads the checkpoint, replays the log after the head, and takes fresh items.
  - Open question 4 asks whether the home should keep the latest item per key instead.

## 5. The query API

**One MCP tool, `han_query`.** It is not `han.query`, because the Anthropic API and several clients refuse dots in tool names. `team_who_is_on` and `team_overlaps` stay, as aliases of `touching` and `conflicts`.

```
han_query {op, args, space?, limit? (default 20, max 200), cursor?, advisory? (default false)}
→ {asOf: {space, head, inputs: <digest>, complete: bool, lag_ms}, rows: [...], next?, truncated: bool}
```

| op | args | row | order |
|---|---|---|---|
| `touching` | `path_glob`, `repo?` | `{session, agent, state, path, hunks, trust}` | path, then session id |
| `touching_symbol` | `sym`, `lang?` | `{session, agent, qname, change, path}` | qname, then session |
| `conflicts` | `session` or `branch`, `with?` | `{with: session | "main" | proposal, paths: n, kinds, predicted, record: BlobRef?, superseded?}` | main first, then by number of paths, descending |
| `seen_error` | `fp` or `{tool, code?, text}` | `{fp, sessions: n, resolutions: [{session, files, symbols, proposal?, trust}]}` | landed resolutions first |
| `who_depends_on` | `api` (qname) | `{session, agent, path, ref, match: "name"}` | session |
| `stale` | `target?`, `behind_gt?`, `rebase?` | `{session, base, behind, moved, rebase}` | behind, descending |
| `blocked_on` | `actor` | `{session, kind, ref, since_seq, deadline?, stalled}` | stalled first, then since_seq |
| `failing` | `test_id?` | `{test_id, session, fp?, since_seq}` | test id, then session |
| `deps_changed` | `name?`, `ecosystem?` | `{session, ecosystem, name, from?, to?, change}` | name, then session |
| `claims` | `path_glob` or `issue` | `{kind: lease | issue | effect, ref, actor, session?, expires_at?}` | ref |
| `facts` | `session` | the session's whole in-flight record | n/a |

- **Bounds.**
  - Rows contain ids, numbers and enumerations. Strings are bounded: paths 1 KiB, qnames 256 bytes, test ids 256 bytes.
  - The only free text, an error `template` or an intent's `doing`, goes in the data envelope's `text` field, never in a structured member.
  - A default answer of 20 rows is about 1,000 tokens.
  - Top-k orders are total and fixed by key, never by node-local time, so a cursor means the same on every node.
- **Subscriptions.** `han_subscribe {op, args, space?}` registers a standing query with the host, which opens it as a node view (snapshot, then diffs); `han_unsubscribe {id}` ends it.
  - Between turns, the host delivers the changes since the agent last saw them as one `han_data` envelope (`kind: "han.query.change"`), coalesced and bounded to 2,000 tokens.
  - Two kinds of change may interrupt a turn, if the space allows it: a new conflict against the agent's own session, and an approval it waits on being decided.
  - An agent has at most 16 subscriptions, and they end with the session.
- **Visibility.**
  - Every query names one space, or runs in each followed space separately with rows labelled by space.
  - Rights are judged for the **asking agent's actor**, never the node owner's: a node run by an operator answers its agent with the agent's rights.
  - Watch facts need watch on that session; read facts need read (T-07, `sessionAccess` at the node's head).
  - A row the asker may not see is left out, and nothing counts it. `seen_error` and `who_depends_on` never join across spaces, even for one fingerprint.
- **Node API.** One new `Query::Facts{space, op, args, window}`, whose rows are the table above. The MCP tool is a thin wrapper.

## 6. Wire, protocol and determinism

| Fact | Where |
|---|---|
| `files[].from/to`, `fresh`, `intent` | new `han.status/1` members (unknown members are ignored: no new version) |
| symbols, api, deps, tests, errors, conflicts (F17 and the shared F18) | new profile **`han.inflight/1`**: one body per `fact`, full state, with its own `seq` |
| blocked-on, resolutions, `against`, the indexes, peer predictions not shared | node-local derived views, never sent |
| landing states, lane conflicts | `han.land/1` (han-29), read as inputs |

- **Carrier.** `han.inflight/1` bodies whose right is read travel as `context` items (T-07: only actors granted read get them). This needs a carrier rule for profile bodies under the read right, `{type: "context", what: "profile", name, body}`, beside G-05's watch-level profile items. The `deps` body travels as a profile item (watch).
- **Size.** A body is at most 48 KiB after JCS. A larger fact is split into numbered parts `{part, of}` with one `seq`, and a node uses it only when it has every part.
- **Determinism.**
  - The index is a pure function `I(L, M, T, now)`: the verified log prefix L, the latest item per `(session, profile, fact)` M, the timelines T read for F18, and an explicit `now` (lease expiry, stale, deadlines).
  - Rows are ordered by key, ties broken by bytes.
  - `asOf.inputs` is the sha256 of `(head, sorted [(session, fact, seq)])`.
  - Two nodes with equal inputs give byte-equal answers (JCS).
  - Host extractors are deterministic for a given extractor version. Each body names it (`extractor`, `grammars`, `errnorm/1`, `han-line3/1`), and the index keys include it, so mixed versions never compare unlike things.
- **Conformance vectors**, in `conformance/inflight-vectors/`:
  1. **index**: inputs (L, M, `now`), queries, and the expected answers in JCS;
  2. **errnorm**: raw diagnostics in, `{fp, template}` out, per tool;
  3. **symbols**: base and new file in, defs and refs out, per language and grammar version;
  4. **line3**: base, ours and theirs in, the conflict record out;
  5. **rights**: the same inputs asked by actors with watch only, read, and none.

## 7. Privacy and safety

- **Rights.** Symbol names, signatures, test ids, error templates and conflict hunks reveal code, so they are read facts. File paths and dependency names stay watch, as today. Nothing crosses a space (§5).
- **Data, never instructions.** Every string an extractor takes from a diff or tool output is untrusted:
  - **identifiers** (qnames, test ids, package names) must match a stated character class; one that fails is dropped and counted in `publish_errors`;
  - **free text** (`template`, `doing`) is masked (SR-52), cut to its bound, and reaches an agent only as the `text` of a `han_data` envelope (R-24, SR-54);
  - none of it is ever put into a tool's name, description or schema, a system prompt or a user turn, and no model summarises it on the answer path.
- **Taint.** A `han_query` answer is structured facts about other sessions, not their content. Whether it taints the turn is open question 5. A `team_read` that follows a resolution link taints as today (SR-56).
- **Crafted input.**
  - A crafted diff can't break the parser host: tree-sitter runs with a 50 ms budget per file, a 1 MiB cap (the mirror's `MAX_TEXT`), and no grammar execution beyond its queries; on failure the file gets `status: timeout` or `parse_error`.
  - A crafted error output can at worst create fingerprints in its own session, within the per-session caps.
- **Poisoning.** A session could plant fake errors and "fixes" to steer others. Rows name the resolving actor and session and their trust; a resolution by a landed proposal (log) ranks above one seen only in a session. A face can filter resolutions to landed ones.
- **Flooding.** Per-session caps (§3) and a per-space cap on postings (default 5M) limit it. When the cap is hit, the node drops that session's newest facts, never older sessions' facts, and marks it `complete: false`.
- **Probing.** `seen_error` with raw text is normalised on the node of the asker and answered from the asker's visible rows only, so it can't confirm another space's errors.

## 8. Targets

| Measure | Target (10,000 live sessions per space, 2,000 items/s) |
|---|---|
| point query (`touching` exact path, `touching_symbol`, `seen_error` by fp, `blocked_on`) | p99 ≤ 1 ms in the node; ≤ 20 ms through MCP |
| glob query over the whole space | p99 ≤ 10 ms in the node |
| item arrival → index updated | p99 ≤ 5 ms |
| item arrival → subscriber diff | p99 ≤ 50 ms |
| host: change → `symbols` published | p99 ≤ 1 s after the quiet period |
| target moves → F17 published | p99 ≤ 10 s |
| F18 for a new candidate pair | p99 ≤ 2 s |
| index memory per space | ≤ 512 MB, and ≤ 64 KB per session |
| answer size by default | ≤ 1,000 tokens |
| restart from checkpoint | ≤ 5 s to complete answers |

`crates/han-node/benches/inflight.rs` measures these on a generated space, and CI fails when a result regresses by more than 20 % (`perf-ratchet`).

## 9. Rollout

Dogfood slice, in order. Each step has its own e2e test:
1. **Incremental tier-2 and `touching`.** The path trie, `from/to` in `files`, `han_query` with `touching`, `claims` and `facts`, and `han_subscribe`. Test: two sessions edit one file, and each gets one `han.query.change` between turns, with no `team_read`.
2. **Error fingerprints.** `errnorm/1` for rustc/cargo and tsc, the `errors` body, and `seen_error` with resolutions. Test: a session fixes E0308 and lands it; a second session hitting the same error gets the resolution's files from `seen_error`.
3. **Symbols.** tree-sitter `tags` for Rust and TS, `touching_symbol`, and the symbol vectors. Test: two sessions change one function in different files of a rename; `touching_symbol` finds both.
4. **Conflicts.** `Vcs::snapshot_tree`, the richer `merge_simulate`, F17 against main, F16 `fresh`, then F18 with `han-line3/1` filling `OverlapConflict`. Test: the #99 case. A session whose path main changes gets a `conflicts` change, and `stale` shows `rebase: overlap`, before any PR exists.
5. **Then:** tests by id, deps, API surface and `who_depends_on`, blocked-on with `han.land/1` as it lands, and intent.

## Core and profile changes for round 33

1. `han.status/1`: the members `files[].from/to`, `fresh` and `intent` (advisory).
2. A new profile `han.inflight/1`: the bodies of §3, their bounds, the extractor versions and the parts rule.
3. A carrier rule (T-07/G-05) for profile bodies under the read right.
4. The `han_query`/`han_subscribe` result shapes and rights rule, as an appendix to the agent tools (R-24's data envelope applies).
5. `conformance/inflight-vectors/` (§6).
6. Code, not spec: `Vcs::snapshot_tree`, and the `merge_simulate` return shape shared with the land study.

## Open questions for the user

1. **Who computes peer conflicts (F18).** Either party's node, on demand, sharing the result (proposed), or one designated fact node per space that computes all pairs and publishes them?
2. **The right for each fact.** Should symbols (and so `touching_symbol`) be watch rather than read, so agents in the same space can coordinate on symbols without the read right? And should deps be watch?
3. **Interrupts.** Should a new conflict against an agent's own session interrupt its turn, or only arrive between turns?
4. **Catch-up.** Should hosts re-send every fact on a heartbeat (proposed), or should the home keep the latest item per `(session, profile, fact)` for new watchers (a transport change)?
5. **Taint.** Does a `han_query` answer (ids and counts, no content) count as reading another session for SR-56 taint?
6. **Retention.** How long are error fingerprints and resolutions kept after sessions close: 30 days per space, or set by the operator?
7. **Cross-space.** Should an operator be able to opt in to `seen_error` across their own spaces, which this design forbids by default?
8. **API dependents on main.** Is a code-search index of the base (SCIP or LSP) in scope after the dogfood, or is `who_depends_on` limited to in-flight work?

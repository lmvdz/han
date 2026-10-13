# Decisions for round 30 (draft)

Status: draft. Round 30 runs after the repo restructure (PLAN.md §10, "Restructure timing").

Inputs:
- the round-29 review (77.9, defects U1–U8);
- the transport, identity and profile decisions in PLAN.md §10.

Theme: **one protocol over QUIC, and delegation that carries its limits.** Stay inside recorded guarding (the round-27 choice): no holds. Everything new is derived from the log.

All agents follow these. Where one is silent, the owning agent decides and writes it down.

## Protocol fixes from review round 29

| # | Topic | Decision |
|---|---|---|
| V1 | **A delegate carries the limits (U1)** | Define `carried(X)` = the union of `guardedFor` over X's open, **accepted** tasks (V4). While `carried(X)` is non-empty: a `task` X creates includes `carried(X)` in its `guardedFor`, which closes the parentless-subtask escape; and X may not approve, govern or steer others, the same Limits as a guarded author. It is derived from the log and ends when the tasks close. R-24's stricter mode states that HAN-native actions in a strict turn (`session.input`, `task`, `effect.approve`, `member.set` through HAN's tools) are proposals or refused. Vectors: the reviewer's three paths (parentless subtask, steer, approve). model_guard gains a delegate-chain property. |
| V2 | **The bounds hold (U2)** | Count the proposer and the executor unconditionally in the 19-agent bound at `effect.propose`, so later guarding can't push a claim past 19 guards. Re-derive `MAX_UNCONSUMED_APPROVALS` and `MAX_APPROVALS` and state the proof in P-08. fuzz_fold must reach the reviewer's counterexample (`p4_bounds.py`, `p4b_bounds55.py`) and show it now holds. |
| V3 | **The cap counts only usable approvals (U3)** | An open approval counts toward `MAX_OPEN_APPROVALS` only if its author is an E3 candidate now (matches the current rule, `separateApprover`, holds a role) and is not guarded. Reuse `approval_candidates`. Vectors: the rule changed to `["owner"]` after 16 member approvals; 16 approvers demoted to guest. |
| V4 | **Only accepted tasks carry guarding (U4)** | A task assigned to someone else adds its `guardedFor` to their effects (T1) and to `carried` (V1) only after they accept it (`task.update` accepted). A task whose assignee is its author, such as an agent claiming an issue, counts as accepted. Vectors: an unsolicited assignment to an owner burdens nothing; the 20-task denial needs accepted tasks. |
| V5 | **Forcing doesn't depend on every approver's server (U5)** | Record each approval's author `actorKind` at fold (it never changes) and judge forcing from it. Drop the extra pins of unconsumed approvers on `effect.approve`, and the quirk that forced approvals count again after forcing is switched off. |
| V6 | **Stale text (U6)** | Reword: §7.5's "mark every session" (the taint is per agent), §8.4's "while guarded" (now strict turns), and the README's "re-checked at every action" and §11's "confirmed human operators" under `allowAgents: "any"`. |
| V7 | **Sandbox boundary (U7)** | Define the boundary as the host process boundary, or the harness sandbox where one exists. State which local writes a strict turn allows (the session's worktree) and which become proposals. |
| V8 | **Timing-sensitive test (U8)** | Make `test_verify_follow.py::test_cli_follow_live_until_seq` deterministic, with no reliance on wall time under load. |

## Transport, identity and profiles (PLAN.md §10)

| # | Topic | Decision |
|---|---|---|
| V9 | **§8 rewrite** | §8.1 requirements; §8.2 the QUIC binding: dial by key, ALPN `han/1`, one stream per call, one-way streams for subscribe and watch, blobs as frames, JSON for signed authority, CBOR (integer keys, zstd batches, signed over the sent bytes) for bulk; §8.3 Matrix as an informative integration. Remove the WebSocket generic binding and the Matrix appservice binding. iroh is named as an implementation choice, never a requirement. |
| V10 | **Peer filter** | A node refuses connections from node ids that aren't registered devices of members of a space it serves. A one-time invite secret admits a joining device once. |
| V11 | **Devices and node ids** | One actor per person or named agent. Each device has its own key, added to the profile with `key.add` together with its node id. A lost device is revoked. Recovery uses a recovery key, or co-owners re-admit. |
| V12 | **Invite tickets** | Format and flow: sequencer node id, space id, one-time secret; `join` proves the secret, and an owner's `member.set` admits. Optional admission by GitHub org or team membership (gist-nonce proof) as a space rule. |
| V13 | **Profiles tier** | Create `spec/profiles/` with `han.status/1`, as defined in PLAN.md §5 with the round-10 fields. A profile is normative for interoperability between HAN apps and carries no authority. A task gains an external reference (issue URL). |
| V14 | **Witnesses** | `impl/` and `cleanroom-ts/` stay Policy witnesses (vectors and differential fuzz). Their WebSocket servers freeze at round 29 and leave the live interop. Interop evidence for the new binding comes with the Rust implementation. |

## Round 30 notes

### Protocol fixes (V1–V8)

- **One predicate.** `carried(X)` is the union of the `guardedFor` of the open tasks X accepted, in the order the tasks were created. It is one helper in policy.py (`carried`) and is used in three places: a task's `guardedFor` (the parent's, then `carried(author)`, then the author if guarded), an effect's (the named task's, then `carried(proposer)`, then the proposer and the executor if guarded), and the Limits (`limited` = guarded, or `carried` not empty). Round 29's "every open task listing the proposer" is gone; `carried` replaces it.
- **V1: the Limits.** A carrying author gets exactly the guarded author's Limits (P-16 step 8, one check). `policy.set` joins the list: it was left out in round 27 because only owners set the policy and owners are human, so a guarded author could never reach it, but an owner may now carry a guarded agent's limits, and setting the policy is governing. `agent.unguard`, redacting another's entry and `space.move` stay off the list: they don't decide who may act or approve. Leaving stays allowed. The vector `delegates-carry-the-limits` runs the reviewer's three paths: the parentless subtask, steering and approving. It also covers an owner who carries the limits after accepting a guarded agent's task.
- **V1: no exception for guards.** A guard that accepts its own agent's task carries that agent's limits and can't approve until the task closes. No exception was added (C2). The guard need not accept: an unaccepted assignment burdens nothing (V4), so it can work the task and close it with a final update.
- **V1: R-24.** The stricter mode now covers HAN's own actions taken through HAN's tools. A `task` is created only with the driving task as its `parent`. An `effect.approve`, a `member.set`, a `policy.set`, or a `session.input` into another's session is refused, whatever Policy allows. This is stricter than Policy for a strict turn whose agent has not accepted the task. R-24 also gains a host MUST: accept a task before the agent's first turn on it, or decline it. That makes acceptance (V4) a host duty for agents, so §7.5's residue shrinks to a human who works without accepting, and to a delegate that closes the task and works on.
- **V4: the state field.** Tasks gain `accepted: ActorId[]`. It is always present and is `[]` when no one has accepted. It is the smallest field that works, because acceptance is per assignee. The task's existing `state` is per task and is overwritten by every update, so using it would let one assignee's acceptance burden the others (U4 again). The field lists assignees in the order they accepted. An assignee accepts with its first non-final `task.update`, `accepted` or `progress`: an assignee reporting progress has taken the task. An author that assigns itself, like an agent taking an issue, has accepted at creation. Acceptance is never withdrawn. It ends with the task: a final update closes the task, and `declined` is one. The field is bounded by `to` (64). The §1.1 worked example already accepts before it proposes.
- **V2: the bound.** `over_bound` counts the entry's author, and an effect's proposer and executor, whether guarded or not. Tasks are now bounded too, with the author counted: `carried` lets a task's list grow beyond its chain's authors, so without a task bound the state would be unbounded. Both are the last check of their kind in P-16 (`denied`, "too many guarded tasks; close some"). The proof is in P-08: the possible guards of an effect are the operators of `guardedFor` ∪ {proposer, executor}, a set bounded by 19 at the proposal that never grows, because operators never change. So `MAX_UNCONSUMED_APPROVALS` = 16 + 1 + 19 = 36 and `MAX_APPROVALS` = 17 + 2 × 19 = 55 are unchanged. han.ts and policy.py comments now call `MAX_GUARDED_FOR` checked, not derived.
- **V2: fuzz_fold reaches the counterexample.** A new second phase (`bound_run`, one run per ten seeds) replays probes p4/p4b with random sizes (16–20 guarded agents), random acceptances and random approval orders. Of 200 runs, 46 proposals were refused at the bound and 31 claims needed exactly 19 guards after later guarding. The maximum held was 55 approvals (35 unconsumed), exactly the derived bound. Vector `later-guarding-stays-within-the-bound` is the reviewer's counterexample, now allowed only within the bound.
- **V3: the cap.** The cap counts `approval_candidates` (P-07's E3 list: unconsumed, unexpired, matching the current rule and `separateApprover`, author holding a role), minus the author's own and the guards'. "Not guarded" is not tested separately: the fold keeps no open approval by a guarded author (V5), and model_guard checks this as DeadApprovalsDropped. The bounds need one change. Rule changes and demotions make approvals non-countable without dropping them, so non-countable approvals could accumulate past 17. An approval's fold therefore drops every other open approval that is no E3 candidate, unless it is by one of the effect's guards. This generalizes round 29's "an approval drops the forced ones", which was one case of it. A guard's approval that stops being a guard's (its agent unguarded, and not in `guardedFor`) and is no candidate is dropped at the next approval. It could not count anyway, and the guard approves again if its agent is guarded again.
- **V5: the state field.** Each approval in the state records `actorKind`, copied from the profile pinned for the approval (the author is always pinned). An actor's kind never changes. This is the only approver fact forcing needs. With it, Policy no longer pins unconsumed approvers for an approve. A `policy.set` that turns forcing on now drops the open approvals of non-human authors, judged from the recorded kind; in round 29 its fold had no profile to judge with. A not-started outcome drops a returned approval whose author is forced. So switching forcing off brings nothing back, and the quirk is gone. E3's "usable" is now only a valid key. The round-28 mutations "guarded approvers count", "only a chosen guarding stops an approver" and "guarded candidates end the pins" are equivalent now and were retired from both suites. A consumed approval by an author who later becomes guarded still exists, so T7's cancel check (`guard_status` of the canceller) stays.
- **No other state or wire change.** `external` on a task body (V13) is not in the state. The two fields above change `StateView`. make_wire_vectors reports **STALE GOLDEN** for `11-approve.json` (step 2) and `21-redact.json` (step 22), both because of the approvals' `actorKind`. The oracle-built probes `probes-r22/23/24.json` were regenerated: their states now hold `accepted` and `actorKind`, and one byte-cut page in `probes-r24` moved. The witnesses' frozen round-29 servers (V14) fail those probes until they follow.
- **V6.** These texts were reworded:
  - §7.5: "mark every session" → "record per agent".
  - §8.4: "While guarded, file edits…" → "In a strict turn…", with V7's worktree rule.
  - §11 Accountability: the operator is named always and confirmed only under `allowAgents: "with-operator"`; the cost column says what `"any"` allows.
  - README's accountable-agents bullet says the same.

  The README no longer had the phrase "re-checked at every action" after the restructure; its bullet "who vouches for it" was the stale claim and is fixed.
- **V7.** R-24 gains a "sandbox boundary" bullet. The boundary is the harness's OS sandbox where one exists, and otherwise the host process boundary. A strict turn may write locally only inside the session's own worktree. Every other write, every network call, any other process and every outside message becomes a proposal or is refused. §8.4 follows: edits and Bash writes inside the worktree stay local, and anything else is refused and proposed.
- **V8: not done here.** `test_verify_follow.py` is in `witnesses/impl-py/tests/`, which this round may not edit. The fix is listed for the impl-py follow-up: wait for the follower's first output line instead of sleeping 1.5 s.
- **P-15 length.** It was 517 words before and is 558 after, counted from the P-15 heading to the `---` that ends §6.6. The additions are `carried`, the bound counting everyone, and `policy.set` in the Limits. "That is the cost", the §7.5 pointer and the duplicated cap wording were cut.
- **Vectors.** Three new ones: `delegates-carry-the-limits` (U1), `later-guarding-stays-within-the-bound` (U2) and `tasks-carry-an-external-reference` (V13).

  Recast:
  - `assignees-carry-their-tasks-guarding`: U4 (unaccepted assignments burden nothing), acceptance by `progress`, and the parentless subtask carrying the delegator.
  - `guarded-tasks-are-bounded`: the bound counts M and X though M is never guarded, and tasks are bounded too.
  - `cap-counts-only-countable-approvals`: U3's two repros (the rule changed to `["owner"]`; 16 approvers demoted to guest) and U5 (forcing drops at the `policy.set`, no approver pinned, nothing back after forcing goes off).
  - `guarded-approvals-do-not-count` and `guarded-approvals-grant-no-cancel`: forcing drops open approvals, so the cancel case uses a consumed approval.
  - `delegation-keeps-the-guard`: BOT3 accepts before its task-less work carries.

  Every approval `check` gets the author's `actorKind` from the vector's profiles (make_vectors fills it, an input, not a computed expectation). There are 159 vectors in all. Policy mutations went from 222 to 232, all caught: 3 retired as equivalent, 13 added, and anchors moved.
- **Model and fuzzer.**
  - The ghost tracks acceptances (`acc`), the rule (`rule`) and `carried`.
  - New moves: every assignee accepts every open task; BOT2's parentless subtask t6 and X's proposal pr9 under it; BOT2 approves.
  - The new property DelegateChain checks that anything created while carrying records what is carried. GuardedRestricted now covers carrying authors and `policy.set`.
  - CapLiveness counts candidates only. DeadApprovalsDropped includes forced authors.
  - The non-vacuity flag "guarded approver skipped" became "guarded approval dropped", since none can be skipped any more. New flags: "limited delegate" and "parentless delegated task".
  - Mutations: 51, all caught at depth 4. Added: unaccepted tasks carry, acceptance ignored, tasks ignore what the author carries, delegates carry no limits, forcing on keeps the agents' approvals, recorded approvals ignore forcing. Retired: the R2 pair and the forced-author cap mutation (equivalent). "An approval keeps what doesn't count" can't be reached with the model's rule and roles; the vectors catch it.
  - fuzz_fold checks acceptance, `carried`, the V2 refusals for tasks and proposals, the candidate-only cap, the recorded `actorKind` and forced dead approvals. It derives possible guards from the pinned operators instead of the fixed world's map, which `bound_run` needed.
  - model_check was re-run because the claim path changed: E3's "usable" and what an approval's fold keeps.

### Transport, identity and profiles (V9–V14)

- **Rule numbers.**
  - A new `N-` series for node rules: N-01 Devices and node ids (§4.3), N-02 Peer filter, N-03 Invite tickets, N-04 Admission by GitHub membership (§8.2).
  - G-01…G-11 keep their numbers. G-07, G-08, G-10 and G-11 are rewritten for QUIC; nothing is retired.
  - T-06 is renamed "Find the sequencer".
- **Node id (N-01).** A node id is the device's Ed25519 public key in base64url, the same string as the profile's `publicKey`, and the device's signing key is its QUIC endpoint key. So a registered device is a `valid` profile key, and registering one is a plain `key.add`, with no body or Policy change. A lost device is revoked with `key.revoke`. Recovery is a recovery key (a profile key kept offline, never a node), a server `key.add` with the existing warning, or a new actor that co-owners admit. An agent's key is held by its host, whose node uses it.
- **The hello proof is unchanged.** `dialed` stays the ServerName, never a node id, so the proof bytes and every wire vector stay valid. The client also checks that the node id it reached is one of `dialed`'s: listed in its `/.well-known/han`, or the `node` of a ticket for a space whose id names `dialed`.
- **Well-known.** `sequencer` keeps its name and becomes `NodeId[]` with at least one entry. The keys and `han-peers` are unchanged and still served over HTTPS.
- **Frames on QUIC (G-07).**
  - A frame is a 4-byte big-endian length followed by UTF-8 I-JSON, at most `maxFrameBytes`.
  - The server opens one unidirectional control stream, which carries only the bare `Challenge`.
  - Each call is one client-opened bidirectional stream: one request, then the reply, or `read`'s items and `end`.
  - `read` with `follow`, `subscribe` and `watch` deliver on a unidirectional stream the server opens for that call. They end by `cancel` or by resetting the call stream.
  - Call streams are handled in stream-id order, which keeps R-07's append order.
  - An over-limit peer is refused by closing with the application error `limit` (formerly HTTP 429).
  - CBOR (integer keys, zstd batches, signed over the exact bytes) carries only bulk data that a profile defines. Calls and authority entries are always JSON.
  - In test mode the peer filter admits everyone.
- **Peer filter (N-02).** A node accepts a connection only from:
  - a registered device of an actor that holds a role in a space it serves, or whose profile it serves;
  - a node id listed by a server it federates with;
  - a joining node, which may only `hello` and then make one `join`. It is closed on a failed join or on any other call. After a successful join it is accepted for 24 h, until an owner's `member.set` makes the first case hold.

  Revocations and role removals re-check open connections.
- **Invite tickets (N-03).**
  - A ticket is `{node, space, secret}`, the secret 32 random bytes in base64url. Its text form is `han1` + base64url(JCS(ticket)).
  - The inviter (an owner, or a member when `membersMayInvite`) registers `invite {space, hash: sha256(secret), expiresAt ≤ 7 days}` at the home. Only the hash is kept, and never in the log.
  - `join {space, secret}` follows `hello` and sends the secret itself: the connection is encrypted to the ticket's node id, and the secret is single-use.
  - On success the home signals the owners `{type: "join", actor}`. Admission is still an owner's `member.set`, which an owner's device MAY append automatically for its own ticket.
- **GitHub admission (N-04).** It is an owner-side procedure (a nonce, a gist, an API check, then `member.set`), not a `PolicyDoc` field, and it is marked "procedure, needs an implementation".
- **Matrix (§8.3, informative).** What remains is a room state event `org.han.space {spaceId, node}` that points to the space and the sequencer's node id, and a notifier bot that is a plain HAN actor. Removed from the normative spec: the application-service binding, the `/_han/v1/*` read, state, entry and challenge endpoints, session rooms, power levels, Megolm and the room-metadata trade-off. Removed from han.ts: the `Http*` request and response types and `MatrixEvents`. `Challenge` stays because G-08 uses it.
- **Legacy carrier.** The round-29 WebSocket rules are one informative paragraph in §8.2: the `/han` path, the upgrade, text and binary messages, HTTP 429 and D7. `ref/wire.py`, `run_wire.py` and `make_wire_vectors.py` still implement that carrier. They are frame-level oracles, unchanged apart from run_wire's largest-item bound (`accepted`, `actorKind`). conformance/wire-vectors/README.md says that the vectors are `han/1` frame vectors and that the carrier-level parts are informative.
- **`han.status/1`** is in `spec/profiles/han.status-1.md` with PLAN.md §5's fields. "The round-10 fields" was read as PLAN.md §5's schema; no other "round 10" field list exists in the repository. The item travels on the session stream as bulk data, so it reaches exactly the actors with the watch right. `stale` is derived by the face, and a claimed `stale` is treated as `idle`. `seq` per session decides freshness, unknown members are ignored, and `issue` mirrors the task's `external`.
- **V13: `external`.** The task body takes `external`: printable ASCII without spaces, 1–1024 characters, opaque to Policy (vector and mutation). It is not copied into the state, since faces read it from the entry. The grammar is wide enough for a URL and for the `github:owner/repo#123` form that `han.status/1` uses.
- **V14.** architecture.md §10 and the README say the witnesses are Policy witnesses (vectors and differential fuzz) and that their WebSocket servers are frozen at round 29. The recorded interop and wire-vector runs stay in the record as runs of the legacy carrier, and no implementation speaks QUIC yet. fuzz_diff against the TypeScript witness now diverges on every seed at the first new state field. It hits V4 (`tasks.<id>.accepted`, 150 seeds) and V5 (`approvals.<n>.actorKind`, 50 seeds); any later decision divergences (V1–V3) are hidden behind those until the witness follows.

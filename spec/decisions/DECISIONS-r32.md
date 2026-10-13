# Decisions for round 32

Inputs: the round-31 review (75.7, defects Y1–Y12) and the spec items from `research/security-requirements.md`.

Theme: **one fact for guarding.** Each round has found a new path through `carried` (the review's main finding), and each special-case fix cost simplicity. The owner chose to unify: while X carries a guarded agent's task, X **is guarded** by that agent's guard. This holds everywhere a guarded agent is checked: proposals, claims and the Limits. This removes predicates instead of adding them.

Stay inside recorded guarding (no holds). Everything is derived from the log. All agents follow these decisions; where they are silent, the owning agent decides and writes the choice down.

## Guarding (Policy)

| # | Topic | Decision |
|---|---|---|
| Z1 | **Carrying is guarding (Y1, Y3)** | `guardStatus(state, ctx, X)` returns `guards(X)`: X's own guard if X is guarded (by choice or forced), plus the guard of every agent in `carried(X)`. "X is guarded" means `guards(X)` is non-empty. Every check that today reads "guarded" or "limited" reads `guards`, at the proposal **and at the claim (E3)**: an effect needs, at the claim, the guards of its recorded `guardedFor`, plus `guards(proposer)` and `guards(executor)` as they stand at the claim. The Limits are the guarded Limits; `limited` and the separate carrying cases go. Closes Y1 (claim-time re-check) and Y3 (a carrying owner is a guarded author: no force-close of others' tasks, no `task.cancel` of others' tasks, no `space.move`, no governing). Vectors: the reviewer's Q1 and Q3 probes. model_guard: the DelegateChain property is replaced by one that checks claims against `guards()` computed independently of Policy. |
| Z2 | **Only a close ends a carry (Y2, Y9)** | An owner's `task.cancel` sets `cancelRequested` and leaves `accepted` alone (P-05 unchanged). A carry ends only on X's own final update, or on an owner's final update when the owner isn't an assignee (force-close). An owner may also clear `accepted` on a **closed** task with a final update (Y9 remedy), and an actor that leaves the space (role `none`) is removed from every `accepted` list. Vectors: Q2 (cancel doesn't free), Q5 (owner remedy), leaving. |
| Z3 | **A guard isn't trapped by its own agent (Y6)** | An approval whose author is a required guard of that effect is exempt from the guarded Limits, because approving is the guard's role. A guard therefore stays able to approve its own agent's effects even while it carries that agent's task. Its own effects under that task still need its own approval, which it may give, since a guard's approval of its own guarded work is allowed. Vector: the reviewer's `probe6.py`. |

## Network (§8, N-rules)

| # | Topic | Decision |
|---|---|---|
| Z4 | **Tickets are dial hints (Y4)** | A ticket's `node` and addresses are only hints. The client MUST confirm that the node id is listed for `server` in its `/.well-known/han` (fetched over HTTPS) before sending `hello`, as §8.3 already requires for Matrix hints. The hello proof's signed bytes add the server's node id (SR-13), binding the proof to the connection. Carrier vectors: a crafted ticket is refused; the hello bytes. |
| Z5 | **One bulk layering (Y5)** | A profile defines its own signed batch envelope and names a stream type. The core's G-07 bulk clause becomes generic framing only: a typed one-way stream carrying profile-defined frames, with no core `BulkBatch` signature. `han.timeline/1`'s TL-93 envelope is the batch format for stream type `han.timeline/1`, and TL-110 hello and answer travel on that typed stream. `spec/profiles/README.md` keeps a stream-type registry. Remove or rewrite `bulk.json` to test the framing only. |
| Z6 | **Stream-item bodies are opaque (Y8)** | G-05 validates only the `profile` stream item's shape (`name` grammar, body size); profile bodies are validated by faces, never by the server. Update `stream-items.json`. |
| Z7 | **The member tier is per space (Y10)** | `admit` judges the member tier against the space a call targets. A role in another space doesn't admit, and an open-join or guest role grants only what that role reads. Update the §7.5 stranger row. Carrier vectors. |
| Z8 | **Security-requirement spec items** | Make normative, or record as a deliberate SHOULD, each item `research/security-requirements.md` lists as a spec change: (1) hello bound to the node (Z4); (2) re-pinning revoked devices (done as X14; check wording); (3) timeline batch domain separation and replay context (TL-93, Z5); (4) the data envelope for untrusted text and `han.status/1` text field limits (SR-54, SR-59); (5) tasks imported by an integration count as outside instructions (SR-57, R-24); (6) layer-2 summaries need the read right, not just watch (SR-66, T-07); (7) whether a turn that reads other sessions must run strict (SR-56: decide and state); (8) §8.4 updated for MXC, with `status` carrying the sandbox backend and gate fidelity (SR-34, SR-99); (9) owner auto-admission under N-03(3) opt-in per ticket (SR-17); (10) a global limit on concurrent joining connections (SR-12). |

## Verification and words

| # | Topic | Decision |
|---|---|---|
| Z9 | **Timeline vectors get a runner (Y11)** | CI runs `conformance/timeline-vectors` through the Rust `han-timeline` crate (its `tests/vectors.rs`), and `run_all.py` gains a row calling `cargo test -p han-timeline --test vectors`. han-timeline is the reference runner for the profile. |
| Z10 | **Docs (Y12)** | Fix DECISIONS-r31's fuzz statement and §4.5's timing-model status, and add `cancel` and reads of unheld spaces to the §7.5 stranger row. Document the fuzz_diff runner protocol (vector directory in, summary out) in `ref/README.md`. |
| Z11 | **Rust follows** | han-core's Policy (M1) mirrors Z1–Z3 and Z7, and the Rust target joins the differential fuzz (`ref/fuzz_diff.py --target both`). |

## Round 32 notes

### Guarding (Z1–Z3)

- **`guards(X)` is a list of agents.** Each agent stands for its guard, the confirmed operator in the profile pinned for the entry. The list is X itself if it chose guarding (it is in the set `guarded`) or is forced, then `carried(X)` in task `seq` order, each once.
  - It lists agents, not operators, because `guardedFor` records agents and an agent with no confirmed operator must still make X guarded (so it can't be claimed for).
  - `guardStatus` returns `{guard?, guarded, forced, guards}`. `guarded` is kept for hosts (R-24) and now means `guards` is not empty. A carrying delegate is guarded, so its host runs it strict and refuses its steers.
  - `GuardInput` gains `tasks`. `state {agent}` (`AgentStateView`) gains `tasks`, cut to the tasks whose `accepted` lists the actor and whose `guardedFor` isn't empty, which is exactly what `carried` reads. This keeps the view a cut of the state, and keeps `guardStatus` one code path for the state and the view; a separate derived `carried` member would have needed two.
  - The view is still never paged. One that would not fit a frame (an agent in several hundred guarded tasks) is answered `limit`, and the host reads the paged state. `wire.py` used to assert this case away.
- **`limited` is gone.** Step 8 reads `guards(author)` (P-16: "the author is guarded and the entry is one the Limits forbid"). `policy.py` lost `limited`, `guarded_may_not` and `guarded_for`, and gained `guards`, `governs_others`, `claim_guards` and `unbounded_guards`.
- **Recording.** A task's `guardedFor` is its parent's list, then `guards(author)`. An effect's is its task's list, then `guards(proposer)`, then `guards(executor)`.
  - This replaces round 31's three special cases: `carried(author)` only without a parent (X2), `carried(proposer)` only without a task (X2), and `carried(executor)` always (X3). All three are now one rule.
  - **X2's "only linked work carries" is reversed.** A carrying actor's work under an unrelated parent or task records its guards too. This follows from Z1: X *is* guarded, and a guarded author's work has always recorded it. It closes the reviewer's Q4 (laundering under an unaccepted or unrelated task). The cost is round 30's W4 spread: the guarding now reaches whoever accepts a carrying lead's work, which §7.5 states. R-24's "the naming is part of the guarantee" sentence is gone; the naming still charges the right task.
  - **Order.** The author comes first, then what it carries. For a parentless task this is the reverse of round 31 (carried, then author); the vectors were re-recorded.
- **E3 at the claim (Y1).** The claim needs the guard of every agent in `guardedFor`, `guards(proposer)` and `guards(executor)`, computed at the claim. Z1 leaves open what happens when `guards(proposer)` or `guards(executor)` holds an agent the proposal never counted, meaning one in neither `guardedFor` nor the proposer or executor. **Decided: the claim is `denied` ("propose again"), before the guard checks.**
  - **Why.** Every guard a claim can need is then the operator of an agent the proposal bounded: `guardedFor` ∪ {proposer, executor}, at most 19. P-01's pin list (named actors, then those agents' guards), P-08's derived bounds (36 unconsumed, 55 in all) and the 19-guard bound stay exactly as they were.
  - **The alternative.** Accepting the new guard's approval on the old effect would have meant pinning `carried(proposer)` and `carried(executor)` (unbounded) on every approve, cancel and claim, and a guard set that moves after the proposal, so the approval bounds would no longer hold.
  - **The effect isn't dead.** It is claimable again once the carry ends, through the delegate's own final update (vector `carrying-executors-need-the-guard`, `c4`). Or the work is proposed again, and the new proposal records the guards.
  - A proposer or executor that is itself newly guarded at the claim is in {proposer, executor}, so its guard's approval satisfies the claim, as before (vector 105).
  - `guards_of(eff)` is the guard of each agent in `guardedFor`, then of each agent of `claim_guards` that is the proposer or executor. It is used at approve (the guard's exemption), cancel, claim and `relevantActors`, and needs no new pins.
- **The Limits (Y3).** A guarded author may not:
  - `effect.approve`, unless it is one of the effect's guards (Z3);
  - `member.set`, except leaving;
  - `policy.set` or `space.move`;
  - make a final `task.update` of a task it isn't assigned (a force-close);
  - `task.cancel` another's task;
  - `session.input` into another's session.

  If the task or session doesn't exist, the kind's own checks judge the entry. An approve of a missing proposal is still denied at step 8, as before. These stay allowed, as before: redacting others' entries as an owner, an owner's `effect.cancel`, abandon and resolve (facts about running work), `session.close` of another's session, and `agent.unguard` by a guard (the guard's role, like Z3). Nothing in Z1 names them, and none of them approves or loosens anything.
- **The cancel right reads `guards`** (Z1 taken literally). A carrying approver's standing approval no longer grants it a cancel right; it may still cancel as proposer, owner or guard.
- **X5 is kept.** The fold still drops open approvals only on an author's own guarding (its choice, forcing judged from the recorded `actorKind`) or a denied server (`approval_dead` is unchanged). An approval made before its author accepted a guarded task keeps counting. Dropping it at the accept would need the profiles of every effect's guards in the accept's fold, to spare a guard's own approvals (Z3), which the fold doesn't have. P-08 and §7.5 say so.
- **Z3.** A guard's exemption is judged by `guards_of(eff)` with the profiles pinned for the approve. A guard that carries its own agent's task approves that agent's effects. Its own work under the task records the agent and needs the guard's own approval, which it may give (vector `a-guard-is-not-trapped-by-its-agent`, the reviewer's `probe6`).
- **Z2.**
  - **Cancel.** `task.cancel`'s fold no longer touches `accepted`.
  - **Closed tasks.** The `task.update` check is now "D closed, unless the state is final". This is simpler than "the author is in `accepted`, or is an owner who isn't an assignee". On a closed task, an assignee's final update removes only itself (and changes nothing if it isn't there), and an owner who isn't an assignee clears the list (Y9's remedy). Nothing else on a closed task changes. A repeated final update is allowed and does nothing.
  - **Leaving.** The leaving trigger is "loses its readable role" (`none` or `banned`), the same one that drops its open approvals. Banning ends the carry too, because a banned actor can do nothing until an owner re-admits it, exactly like a removed one. Rejoining starts with no carry. This isn't an escape: the actor could always end it with its own final update.
- **P-15 length.** It went from 556 words to 512 (from the P-15 heading to the `---` that ends §6.6).
  - Cut: the separate "limited" definition, the per-assignee end of the limits (P-05 says it), the "only linked work carries" clause, the per-case carried recording, and the E3 approval details (P-07 says them).
  - Added: `guards`, the never-counted claim rule, and the extra Limits.
- **Vectors** (168).
  - Recast:
    - 142 `guard-status-takes-the-agent-view`: the view has `tasks`; a carrying agent and a carrying human.
    - 150: `q5` is now denied: work under t19 carries `guards(M)`.
    - 158 `closing-ends-only-the-closers-carry`: Q2 (a cancel ends nothing), Q5 (an owner clears a closed task), leaving and banning, and repeated final updates.
    - 160 `carrying-executors-need-the-guard`: Q1, the effect proposed before the accept, and its recovery.
    - `guardStatus` expectations gain `guards`.
  - Replaced: `only-linked-work-carries` by 159 `a-guarded-authors-work-all-carries` (`p4_cascade` and Q4).
  - New: 165 `carrying-is-guarding` (Q1 and Q3, `guardStatus` of a carrying human and agent) and 166 `a-guard-is-not-trapped-by-its-agent` (`probe6`).
- **Mutations** (`mutate_vectors.py`, 246 to 255): the P-15 anchors moved to the new functions.
  - New: guards omit what is carried; claims ignore the proposer's or the executor's guards now; claims accept guarding never counted; claims need the guards of agents never counted; a guard that carries its agent may not approve; carrying owners may move the space, force-close, or cancel others' tasks; guarded authors may not cancel their own tasks.
  - Z2: an owner's cancel force-closes; any cancel force-closes; a closed task takes only an accepted assignee's final update; a closed task takes any update; leaving keeps the carry; banning keeps the carry.
  - X2 and X3 restored as mutants: work under a task ignores the proposer's guards; subtasks ignore what the author carries; effects ignore what the executor carries.
  - Retired: "effects/tasks ignore carried(…)" (now covered by "guards omit what is carried"), "an owner's task.cancel force-closes nothing" (the rule now).
- **fuzz_fold** recomputes `guards()` from its own definitions and checks:
  - the recording rules;
  - the Limits, including Z3's exemption;
  - that an allowed claim had no never-counted guard and every guard's approval;
  - that a cancel changes no `accepted`;
  - that leaving removes the actor from every `accepted`;
  - that a closed task takes only final updates that change only `accepted`;
  - that `guardStatus` equals the definition.

  The generator lets an owner make 20 % of task updates and cancels, so force-closes, the Y9 remedy and refused force-closes by guarded owners occur. The fuzzer reports how often each Z path was taken.
- **model_guard.**
  - New moves: t9 (BOT tasks its own guard OP, so Z3 is reachable), BOT cancelling its own t7, O clearing a closed t7, and O force-closing t2 when it is closed too.
  - The ghost computes `guards()` and records per Z1. Its carry ends on a final update, a force-close or leaving, and never on a cancel.
  - **ClaimGuards replaces DelegateChain**, checking claims against the ghost's `guards(proposer)` and `guards(executor)` at the claim:
    - an allowed claim needs every such guard's unused approval, and none never counted;
    - a "never counted" refusal needs one.

    GuardedRestricted follows the new Limits.
  - New properties: ExactAcceptance (Policy's `accepted` equals the ghost's) and ClosedTasksTakeFinalUpdates.
  - New non-vacuity flags: claim refused as never counted, carrying guard approved, closed task cleared by an owner, leaving ended a carry.
  - Mutations go from 58 to 63.
  - Y3's owner limits are not reachable, because the model's one owner never carries. Vector `carrying-is-guarding` and mutate_vectors cover them.
- **model_check** was re-run (the claim path changed), with its mutations, into `ref/logs/model_check-r32.log`. It holds both runs, which also fixes §4.5's timing-model status line (Z10).

### Rust, CI and runners (Z9, Z11)

- **Z11.** han-core's Policy follows Z1–Z3 and Z7 (Z7 in `admit.rs`).
  - `GuardInput` gains `tasks`, `GuardStatus` gains `guards`, and `carried` reads a task map.
  - `decide` has `governs_others`, the closed-task rule and the never-counted claim check.
  - `fold` no longer clears on cancel, and removes a departing actor from `accepted`.
  - `han-conform` reads a view's `tasks`.
  - `tests/policy_vectors.rs` now counts the vector files instead of hard-coding 166.
- **fuzz_diff.** It rebuilds the default `han-conform` before every Rust run, so a stale binary is never compared. `run_all.py`'s differential row and the nightly `full.yml` run `--target both`, and `full.yml`'s spec job installs the Rust toolchain for it.
- **Z9.** `ci.yml` has a named step, `cargo test -p han-timeline --test vectors`; `cargo test --workspace` already ran it, unnamed. `run_all.py` gains the row "Timeline vectors … through han-timeline" and now runs every check from the repository root.

### Witnesses

- The TypeScript witness follows Z1–Z3: `policy.ts`, and `seq.ts`'s agent view with `tasks`. Its `han.ts` is again a copy of `spec/core/han.ts`, network changes included.
- impl-py uses `ref/policy.py`. Its tests were updated where they encoded the old meaning:
  - `guardStatus` results gain `guards`;
  - four host tests asserted that an accepting delegate is "not guarded"; under Z1 it is.
- `run_wire_vectors.py --launch py` and `--launch ts` pass. `probes-r25.json`'s agent-view replies gained `tasks`. These are generated probes, not golden files.

### Z10 (Policy and docs parts)

- DECISIONS-r31's Results note is corrected. Round 31's own `RESULTS.md` shows 0 divergent seeds; the listed divergences predated the witness update.
- `ref/README.md` has a `fuzz_diff.py` row with the runner protocol:
  - **In:** `<runner> <dir>`, where `dir` is a directory of vector files.
  - **Out:** one `<file>.json#<step>: <message>` line per failing step, then the summary line `steps pass N fail M; vectors pass A fail B`.
  - The exit code is not read.
- README and goal.md: an accepting delegate "is guarded the same way until the task closes for it". The README notes that the Rust core joins the differential fuzz.

### Network (Z4–Z8)

- **Z4, tickets are hints.**
  - A ticket's `node` and addresses, like a room's `org.han.space` hint, are only where to dial. Before `hello`, the client confirms the node is listed for the ticket's `server` in the `/.well-known/han` it fetches over HTTPS from that server. `carrier.dial_verdict(server, node, wellknown)` checks three things: the document is well formed, it names `server`, and it lists `node`.
  - `challenge_verdict`'s `nodes` is now always the well-known list. The r31 case "a ticket's node for its server" is gone. `control.json` has a new `dials` section, including the reviewer's K1 ticket.
- **The hello bytes on `han/1`** are `"han-hello-v2\n" ‖ challenge ‖ "\n" ‖ dialed ‖ "\n" ‖ node`. `node` is the server's node id as the client reached it, meaning the TLS key it authenticated, as 43 base64url characters.
  - The prefix moved to v2 so a v1 proof can never pass as a `han/1` proof (`control.json` has the case).
  - The legacy WebSocket carrier has no node ids. It keeps round 31's v1 bytes, so `wire.py`, the wire vectors and the witnesses are unchanged, and `make_wire_vectors` reports no STALE.
  - Rust: `sign::han1_hello_*` is new, and `hello_proof` stays as the legacy form.
- **SR-13, which Z4 cites, is also made normative in G-08.**
  - For a user actor, the public key of `keyId` MUST be the connection's node id.
  - For `@:S`, the node MUST be in S's `sequencer` and `keyId` among S's `keys`. Otherwise `denied`.
  - `carrier.hello_binding` checks this, with `control.json` `bindings`.
  - Together with the node in the signed bytes, a relayed proof fails twice.
- **Z5, typed streams.**
  - G-07's bulk clause is now framing only. A typed stream is a unidirectional stream that either end opens after `hello`.
  - Its first frame is the type name: 4-byte length, then UTF-8 in the profile-name grammar, at most 64 bytes.
  - Then come the profile's frames, each a 4-byte length (at most `maxFrameBytes`) and its bytes.
  - The core signs and checks nothing inside the frames. A bad header or an over-long frame resets the stream, and an unknown type is simply not read.
  - `BulkBatch` and `han/bulk/1` are gone, `bulk.json` is replaced by `typed-streams.json`, and han.ts has `TypedStream`.
  - `spec/profiles/README.md` has a stream-type registry. Its one row is `han.timeline/1`: TL-110 messages, one per frame, whose envelopes are TL-93's, with TL-113 deciding who may sync.
  - The TL-93 and TL-110 wording names the stream type and nothing else.
  - **For Team A** (`han.timeline/1`, no crate change made):
    - On the `han.timeline/1` stream, a TL-110 hello and answer share keys 1–5. They can be told apart only by the type of key 4 (an inventory or envelopes) and of key 5 (an int `maxBytes` or a bool `more`). A message-type key would be cleaner.
    - TL-111 pushes are worded as travelling inside an answer.
    - TL-93 binds the space and the history, but not the timeline, session or sender sequence that SR-89…SR-91 also ask for.
- **Z6, opaque bodies.**
  - G-05 checks a `profile` item's name grammar, Json depth (≤ 32) and the item size, never the body.
  - `han.status/1` bodies are checked by faces, which treat a failing body as absent. `status_body_verdict` is the face check, in the `bodies` section of `stream-items.json`.
  - "han.status/1 without seq" and "a state outside the seven" are now `ok` for the server.
- **Z7, member tier per space.** `admit` order:
  1. `hello`: open.
  2. No `hello`: denied.
  3. A vouched `@:S`: server.
  4. A vouched actor with a role (owner, member or guest) in the space the call names: member.
  5. A call naming no space (`blobPut`): member only for an owner or member of a served space whose `join` isn't `"open"`. A profile space counts, so an actor's own devices keep uploading.
  6. `join` or `cancel`: open.
  7. Any call naming a space not held here: open. This is new. Routing answers it with only `moved` or `not_found`, and it keeps clients able to follow moves now that a role elsewhere no longer admits them.
  8. Reads of public or profile spaces: open.
  9. Append to an open-join space: open.
  10. Otherwise denied.

  "An open-join or guest role grants only what that role reads" became two rules:
  - Such a role admits only calls on its own space, where the call's own rules judge.
  - It gives no standing for spaceless calls (no `blobPut`).

  A guest therefore cannot upload blobs. That is deliberate: uploads are a global resource.

  Rust `admit.rs` mirrors this exactly. A read call that names no space is now denied in both implementations; before, it was open.
- **Z8.** What each security-requirements item became, and where:
  1. Hello bound to the node: MUST, in G-08 (Z4 and SR-13 above).
  2. Re-pinning revoked devices: the X14 wording in N-02 was checked and is sufficient (re-pin at least every 300 s and on revoking receipts, and close the connection). No change.
  3. Timeline batch domain separation and replay context: these stay in the profile, since the core signs nothing (Z5). TL-93 already domain-separates (`han-timeline-batch-v1` and `han-timeline-sig-v1`) and binds space, history, actor and key. TL-101 verifies the signature (step 3) before it decompresses (step 5). Replays are TL-104 duplicates, and an envelope for another history fails step 2. Binding the timeline, session and sender sequence is left to Team A (see the note above).
  4. The data envelope and the `han.status/1` text: MUST, in R-24 (the `{han_data: {kind, source, trust: "untrusted", text}}` envelope and the plain-text `<han-data id=R>` wrapping). The text bounds are in `han.status-1.md`:
     - `step` ≤ 256 characters
     - `tool` ≤ 128 characters
     - `plan` ≤ 64 items, each `text` ≤ 256 characters

     These texts are untrusted. A host MUST NOT exceed the bounds, and a face treats an over-long body as failing. Carrier vectors cover them.
  5. Integration-imported tasks: MUST, in R-24. A task authored by an integration actor is outside; the imported text is data, and the instruction is a fixed template naming the task and its `external`.
  6. Layer-2 summaries: MUST, in T-07. A summary derived from a session's context is a `context` item with `what: "summary"`, under the read right. `han.status/1`'s `summary` member is withdrawn, and faces ignore it like an unknown member.
  7. SR-56 (turns that read other sessions): decided in R-24. Strict is MUST when the agent is guarded and a deliberate SHOULD otherwise. Such a read is newly recorded as taint (MUST), so later turns run strict once the agent is guarded. Under Z1 a carrying delegate is guarded, so the MUST covers it.
  8. §8.4 now names MXC (Windows 11 without admin, bwrap, seatbelt), the SDK sandbox and WSL2 or Podman. Every `han.status/1` item MUST carry `sandbox` (a backend name or `"none"`) and `gate` (`every_call` or `prompt_only`). A face MUST show both, and MUST mark a session as advisory when `sandbox` is `none`, `gate` is `prompt_only`, or either is missing (SR-99).
  9. Owner auto-admission: MUST, in N-03 (3). It is opt-in per ticket, off by default, and kept by the issuing device, never in the ticket or at the home. No wire change.
  10. Joining connections: MUST, in N-02. A global bound on concurrent connections whose actor is member or server in no space (strangers, joining devices included), refused with the `limit` close.
- **Carrier vectors:** 9 files and 175 cases (was 134), and 36 carrier mutations (was 24), all caught.

### Z10 (network parts)

- **§7.5 stranger row.** It now lists everything a stranger is admitted for:
  - `cancel`;
  - any call on a space the node doesn't hold, answered by routing alone;
  - the per-space member tier: a role elsewhere admits nothing, and a guest role or open join reaches only its own space;
  - the `blobPut` standing rule and the SR-12 global bound;
  - forged tickets, which are refused by the well-known check and by the node-bound proof.
- **§4.5 timing-model status.** `sync_docs.py` generates the line from the newest `model_check` log that holds both the property run and the mutation run. `ref/logs/model_check-r32.log` holds both, so the status line is current again.

### Results

- Every check passes. The run is in `ref/RESULTS.md`, and the logs are `ref/logs/*-r32.log`.
- fuzz_diff `--target both`, 1000 seeds × 250 steps: 300,107 vector steps, 0 seeds diverge across ref, TypeScript and Rust.
- Mutations, all caught:
  - policy: 255/255;
  - wire: 95/95 (two new Z1 mutations of the agent view);
  - carrier: 36/36;
  - guard model: 63/63.
- model_guard at depth 4: 1,046,930 transitions, every property holds.
- model_check: every property holds, and all 11 of its mutations are caught.

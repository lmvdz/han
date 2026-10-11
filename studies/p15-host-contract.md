# Study: P-15 as a host + executor contract

Status: design study (paper only), written against round 17. Nothing in this file is normative.

## 1. The design

**In one paragraph.** Policy keeps *authorization*: who may watch, read and steer, and who must approve what. The question "may an outside instruction reach the world without my operator?" moves to a **provenance contract**. The agent's own host code, never the model, tags every proposal `outside: true` if any instruction from someone other than the agent or its operator is unreleased in its context. The operator's choice (`auto` or `guarded`) lives in the agent's **profile space**, where space owners can't touch it. A single stateless claim check (E7) requires the operator's own approval for an outside-tagged effect of a guarded agent, and for an effect that an outsider aims at a guarded agent as executor. Holds, stickiness, releases with `seen`, cleared/outside bookkeeping, recorded guards and live/dead sessions all leave Policy. A verifier audit rule (V-1) turns a host that lies about provenance into a portable, signed proof after the fact.

### What stays in Policy

| Concept | State | Check |
|---|---|---|
| **Session rights** | `sessions[id] = {actor, open, task?}`; `policy.sessions = {watch, read, steer, requireGuarded}` | `sessionAccess`: the session's actor and its operator (while the operator holds a role) have all three rights; anyone else needs the minimum role. `session.input` needs steer. |
| **E7 Operator approval** (at claim) | none new; `claims[].operatorApprovals?` | Let `mode(A)` = `guarded` if `profile(A).steer = guarded` or `policy.sessions.requireGuarded`. The **required operators** are `op(proposer)` if `outside` and the proposer is a guarded agent, plus `op(executor)` if the executor is a guarded agent and the proposer ∉ {executor, op(executor)}. For each one, E3's walk must also find an unconsumed, unexpired approval **by that operator**, with a key valid at claim, while the operator holds a role. Operators may approve or cancel such effects whatever the rules say, and their approvals don't count toward the 16-approval cap. |

That's all. P-08 supersession, expiry and the cap are general approval rules and stay as they are.

### What the host must do (normative, replacing R-24's P-15 parts)

- **H-1 Provenance is computed by code.** An *outside instruction* is a `session.input`, `task` or `effect.propose`-naming-it whose author is neither the agent nor its operator. The host also counts as outside any context it fed from another session's stream or read (R-24 "data"). The host taints a model context when such input enters it. Taint is sticky until H-3. The model never sees the tag and has no tool that sets it.
- **H-2 Tag always, regardless of mode.** Every `effect.propose` sets `outside` from H-1, **in both modes**. The mode only decides, at claim (E7), whether the operator must approve. For auditability, H-1 is at least as coarse as V-1: after an unreleased outside instruction *in space S*, every proposal in S is tagged. A host MAY taint more (cross-space, reads), never less.
- **H-3 Only the operator releases.** The operator clears a context's taint through the host's own UI, which shows them every outside input. The host then appends `session.input {release: true}` by the operator, with `seen` set to the latest outside instruction it showed. A later outside instruction keeps the context tainted.
- **H-4 Tainted contexts instruct no one.** While tainted, the agent may talk, report, cancel, claim and propose. It MUST NOT append `task`, `effect.approve`, `session.input` into others' sessions, governance kinds or extensions. If the model asks for one, it becomes a proposal that waits for the operator.
- **H-5 Stricter turns.** A turn driven by an outside instruction, or in a tainted context, runs in the harness's stricter mode if `mode` is `guarded` (Claude Code: `default` permission mode with `canUseTool`, sandboxed egress, world actions as effects, as in §8.4). In `auto` the turn runs like the operator's own, but H-2 still tags it.

### What the executor must do

- **X-11** Before claiming, evaluate E7 with the pinned profiles. Don't claim an effect that would fail it, and re-check before launch (X-04). An executor that is itself a guarded agent applies E7's executor clause to proposals naming it. This is defence in depth: the sequencer enforces E7 anyway.

### Where the operator's choice lives

The agent's profile space gets `policy.set {policy: {steer: "auto" | "guarded"}}`, which **only the confirmed operator** may append. The default is `guarded`, so an unset or unconfirmed choice fails closed. The agent's own key can't change it, so a steered model can't un-guard itself. `ProfileView.steer` is pinned with the profile (P-01), as keys already are. Owners can only add caution, through `requireGuarded`.

## 2. Concept count and interface

| Today (P-15) | After |
|---|---|
| per-session modes + effective mode; live/dead sessions; guard recorded per session; sticky `guarded[X]`; owner-forced `guardedNow`; holds `held[X]`; release with `seen`; `cleared`/`outside`; recorded + derived per-effect guards; held-kind restrictions; departed-guard rules; guard cap exemption | **Policy:** session rights; E7 (operator approval for outside or executor-slot effects, cap-exempt). **Profile:** one field. **Host:** H-1…H-5. **Verifier:** V-1. |
| about 11 interacting concepts, 4 state maps, 1 derived predicate, a phase rearm row | **2 Policy concepts**, 0 new state maps, a stateless check |

Sketch of the han.ts changes:

```ts
"session.open":  { task?: EntryId };                       // steerMode removed
"session.input": { session: EntryId; release?: true };     // release: annotation for V-1; Policy ignores it
"effect.propose": { …; outside: boolean };                 // H-2, required (closed shapes)
policy.sessions: { watch; read; steer: SessionLevel; requireGuarded: boolean };   // replaces modes
type ProfilePolicy = { steer: SteerMode };                 // profile space; operator-only policy.set
ProfileView += { steer: SteerMode };                       // default "guarded"
SpaceState: − held, cleared, outside, guarded, sessions[].steerMode, sessions[].guard, sessions[].human, effects[].guards
claims[]: guardApprovals → operatorApprovals?: EntryId[]
Policy: unchanged signatures; guardedNow removed. Helper (pure): requiredOperators(state, effect, profiles) → ActorId[]
Verifier: VerifierStatus.audit: {kind: "provenance", agent, instruction, entry}[]
```

The §6.4 table loses its row "ready, when P-15 records a guard → awaiting-approval". P-01 pins at most two operators instead of two guards.

**V-1 Provenance audit (verifier, not decide).** In space S, if an agent's entry follows a non-operator instruction to it, with no operator `release` in between whose `seen` covers that instruction, and the entry is an `effect.propose` with `outside: false` or an H-4 kind, then the verifier reports `provenance`. The proof is three signed entries: the instruction, the offending entry and the receipts between them. This is the old hold predicate, but it lives in an audit and never gates anything.

## 3. Every attack from rounds 8–17

P = closed by Policy (E7, rights) · C = closed by the contract (honest host) and detectable by V-1 · T = trust assumption · N/A = the concept no longer exists.

| Rd | Attack | Now |
|---|---|---|
| 8 | The guard relied on the steered agent tagging its own proposals, so it could drop the tag | C: host code tags, the model can't. A host whose code lies = T (§4), and V-1 proves it. |
| 8 | The tag is laundered through another agent | C: H-4. A relay task from a tainted context is forbidden, and V-1 catches it. |
| 9 | Taint laundered through tasks the assignee didn't name | N/A: the host knows its own context. A task to X from a non-operator is outside (H-1). |
| 9 | Laundering through agents without a guard | C: H-4 (the tainted agent can't instruct an `auto` agent). An auto agent's operator accepted direct instructions from anyone with rights. |
| 9 | Approvals by tainted agents | C: H-4 + V-1. |
| 9 | Accidental clears | C: only the operator's release clears (H-3). Policy clears nothing. |
| 10 | Owners loosen open sessions' modes | P/N/A: the mode is in the profile. `requireGuarded` can only add caution. |
| 10 | Guard rights without a role; orphaned agents | P: E7 counts an operator's approval only while the operator holds a role. Owners may cancel. |
| 10/11 | `suggest` mode loosened Policy | N/A |
| 10 | Held agents can't cancel their own delegations | C: H-4 allows cancels. |
| 10/11 | Guard approvals crowded out by the cap or by expired approvals | P: operator approvals are cap-exempt. Expiry is general P-08. |
| 10 | Matrix stream keys not rotated on removal | unchanged (T-07, not steering) |
| 11 | Auto side-session as a side door | N/A + C: the mode is per agent (profile), and taint covers all of S. |
| 11 | Guarded session with its guard gone | P: no dead sessions. Outside effects wait for the operator (fail closed). |
| 12 | Auto session opened before the operator confirmed | P: the default is `guarded`, and unconfirmed agents fail accountability anyway. |
| 12 | Held agent used as executor | P: E7's executor clause, stateless. |
| 12 | Owner evicts the guard to approve as stand-in | P: only `op()` from the pinned profile counts. Nobody stands in. |
| 12 | Tasks as unheld instructions | C: H-1. A task from a non-operator taints. |
| 13 | Un-guarding from outside (close session, evict guard, toggle modes) | P/N/A: owners can't write the profile. Evicting the operator only blocks outside effects. |
| 13 | Naming as executor is an unheld instruction | P: E7. |
| 13 | Returned approvals no longer superseded | P: general P-08, unchanged. |
| 14 | Owner-forced guarding missed some paths | P: one `mode()`, used by E7 for both clauses. C: H-5. |
| 14 | Becoming guarded with outside instructions open | N/A: tags are set regardless of mode (H-2), and the mode is read at claim. |
| 14 | A held proposer's *older* proposals | N/A: same reason; outside-tagged proposals need the operator whenever they are claimed. |
| 14 | The agent can't always un-guard | changed: the agent never un-guards, the operator does (a deliberate restriction). |
| 15 | Becoming guarded held only via `session.open` | N/A (as round 14) |
| 15 | Effects aimed at a held executor lost their guard on release | P: E7 is evaluated at every claim. A release doesn't exist in Policy. |
| 16 | A stale release covered work the guard hadn't seen | C: the host releases exactly what it showed (H-3). V-1 checks `seen`. Per-effect approvals are bound to `planDigest`. |
| §7.5 | Prompt injection through read data | **better**: H-1 counts reads as outside, so they need the operator if guarded. Today, Policy can't see reads. |
| §7.5 | Held agent acts in another space | **better for honest hosts**: taint is per context, across spaces (H-2 MAY). |
| §7.5 | Local actions in the sandbox | T, unchanged |

New attacks this design creates: **(n1)** a host that mis-tags (T, with V-1 proof); **(n2)** the operator switching to `guarded` takes effect at the next profile pin (R-10: ≤ 300 s freshness, T). The host applies it locally at once. **(n3)** An evil executor that ignores tags is closed by E7 at the sequencer.

## 4. Threat model: what today's Policy-level P-15 actually buys

Take each adversary in turn, against agent X.

1. **Steerer, owner, other members, other agents; X's host honest.** Both designs protect. Today's guarantee *already* rests on the host: R-24 stricter mode, the §8.4 sandbox, and routing world actions through effects. Without these, "guarded is advisory" (§8.4). The contract adds one more host duty (tagging) on top of duties the host already has.
2. **Prompt-injected model.** Equal. Tags come from host code, which the model can't reach. The contract is better for reads (§3).
3. **Malicious host or stolen agent key.** **No difference.** The attacker *is* X. It doesn't need a steer: it proposes from "own" turns, and today Policy runs those without a guard. Holds constrain a held key, but nothing forces the attacker to get held.
4. **Malicious executor.** Equal with E7 (the sequencer denies the claim, and verifiers replay it). Without E7, an evil executor could run an outside effect unapproved. But it holds its own world access anyway. The plan's content is already readable by members, so it gains no exfiltration channel either.
5. **Malicious sequencer.** Equal. E7 is replayable.
6. **Honest but buggy host: sandbox and effects correct, provenance wrong.** **This is the only case where today's design is stronger.** Today the steer is in the log, so Policy holds X whatever the host computes. Under the contract, a mis-tagged effect runs, and V-1 detects it only afterwards. Today also *prevents*, rather than detects, H-4 relays by a buggy host.

**Conclusion.** Today's Policy-level P-15 protects against exactly one thing the contract doesn't: **a host that is correct about sandboxing and effects but wrong about provenance**, turned from prevention into detection. Two things make that gap small. V-1 is the old hold predicate, so a mis-tag yields a signed, portable proof against X's key and operator. And H-2's minimum taint rule is about 20 lines of code, testable against V-1 as an oracle (§6). Against every *adversarial* party except the host itself, the two designs are equivalent. Against the host itself, neither protects.

## 5. User requirements

- **"Auto mode, but someone else's steer is less permissive and risky tasks need approval; or allow auto from external steers."** The operator sets `steer: guarded` in X's profile: outside turns run in the stricter mode (H-5), and their world effects need the operator (E7). `steer: auto` lets outside steers run freely. That choice is the operator's alone and can't be flipped by space owners, which today's design couldn't fully guarantee.
- **Trust per space, set by owners.** Owners set `watch`/`read`/`steer` levels and `requireGuarded`. Small team: `{member, member, member}`, false. Large network: `{guest, owner, nobody}`, true.
- **Full A2A.** Agents read context (read right), message, task and subscribe, unchanged. Only a *tainted* context can't instruct others (H-4), which matches today's held state.
- **Rounds 8–17.** All addressed in §3: about 12 closed by Policy, 9 by the contract (each detectable by V-1), 6 dissolved, 1 explicit trust assumption (n1), and 1 unchanged non-steering item.

## 6. Testing in impl/PLAN.md

E7 replaces `ref/model_guard.py`. E7 is stateless, so ordinary vectors and mutations cover it. The host contract gets executable tests in `impl/tests/`:

- **Oracle property test (replaces the model checker).** Generate random interleavings of steers, tasks, executor namings, releases (by the operator, other actors and owners), mode changes and `ScriptedAgent` proposals and relays, across two agents and two spaces. Assert that every entry `han.host` emits satisfies V-1 (tags at least as coarse as the oracle, no H-4 kinds while tainted), and that `han.exec` never launches an E7-failing effect. V-1 is the oracle: the same predicate as the old ghost, now run against the real host.
- **Host mutations:** drop the tag; tag only when guarded; taint not sticky; release by a non-operator; release ignoring `seen`; allow approve or task while tainted; model-settable tag. The property must catch each one.
- **E2E (P-15/R-24 row):** a member steers a guarded Claude Code agent. Check that the push becomes a proposal with `outside: true`, the claim is denied until the operator approves, an owner who evicts the operator can't approve, and a doctored host that mis-tags is reported by `han-verify` with a provenance proof.
- **Vectors:** `e7-outside-needs-operator`, `e7-executor-slot`, `e7-mode-from-profile`, `profile-steer-operator-only`, `require-guarded-only-tightens`, `operator-approvals-cap-exempt`, `evicted-operator-blocks-not-bypasses`.

## 7. Risks and verdict

**Risks.**
1. The safety claim weakens from "prevented, given an honest sequencer" to "prevented, given an honest host; otherwise proven afterwards". The README and goal.md must say so plainly.
2. V-1 keeps release/`seen` bookkeeping, in the verifier rather than Policy. That bookkeeping is still complexity, but it is off the decision path and can't block anyone.
3. `outside: false` is self-declared, so a sloppy third-party host passes Policy. Conformance depends on the test suite, not on replay.
4. Operator-only un-guarding needs the operator's key at hand.

**Verdict.** It is simpler: Policy's P-15 shrinks from about 11 concepts and 4 state maps to session rights plus one stateless claim check. The decision path loses every concept the graders flagged. It is safe enough: against every party other than X's own host it matches today, and in two places (owner-proof mode, reads) it is stronger. The one regression is a buggy host, which gives up prevention for provable detection. Today's design also depends on that host for the sandbox, which matters more.

Self-score: simplicity **+8 to +10** on "simple interfaces" (about 76–78), safety **−2** (an honest-host assumption made explicit), net about **+1 to +2** overall. Confidence: medium. The 9 contract rows are only as good as the oracle test, so recommend this design only together with §6.

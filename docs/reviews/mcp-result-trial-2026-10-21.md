# MCP result visibility trial — 2026-10-21

The corrected Linux deployment exposes actual Codex MCP tool results in the shared
session: an intentional failed read, a subsequent successful read, and a proposal
receipt. The first deployment did **not** fix the live case; that failed trial led
to a correction of both the parser and its regression fixture.

This is an author-operated, bounded compatibility trial using an existing
subscription login. It is not fresh onboarding, independent team acceptance, an
authorization audit, or a landed task.

## Initial failure and correction

[PR 199](https://github.com/lmvdz/han/pull/199), source
`35c0a1e6afc7250f93306644a74ce3ca565868f9`, added text extraction from the adapter's
`rawOutput: {result, error}` envelope. Its tests passed, but the deployed trial
still produced three empty tool bodies. Task
`c93e83f6e43e714d65e7b9cb4ac864cc4f13b776bbe8e410fe7771aed0c9b795` and session
`131b84ff3cc997e6c19763645ca94943b4ffc5e25d28e8b8ec2f6008e3da338b` retain that failure.

The parser required `rawInput.server/tool` on completion. Inspection of the
installed adapter's reporting layer showed that it removes unchanged fields
before transmission. Tool identity arrives at the start but is normally omitted
from completion. The original fixture repeated that identity and missed this
behavior.

[PR 200](https://github.com/lmvdz/han/pull/200) recognizes the narrow result/error
envelope without repeated input metadata. The fixture now sends identity only at
the start. It retains typed text extraction, error messages, masking, size limits,
snapshot precedence and the adapter's status. Progress messages, structured
content, resource bodies and binary payloads are not added to the shared output.

The corrected source is `e8a9b43fff1bed478b18018037341b70af8d7fdf`, tree-equivalent
to main merge `6b7fefb19aa890932addc7372f38f33771ba0009`. The Linux artifact SHA256 is
`db9ed6ef5b0afbd942e22dfa7d9643a9ffe7d41e88f7c5d34e4af91f735f9a01`.
Retained captures verify that hash on the installed binary, all three normal
service executables, and the actual trial host, shim and MCP processes. Windows's
normal deployment was not updated by this trial.

## Corrected live observations

Task `bb3f2733e58f6639a1f59048e19c0fdef56b2fa07fe09e983be8c12e26ad9a47`, session
`90d9567edeb0b998e0476ffef463acea476b95f0561d9636168b91f6a5dba413`, made three calls
in order. These are observed Node API tool bodies, not only the agent's summary:

| Call | Observed status and output |
|---|---|
| `team_read` for an intentionally nonexistent session | `failed`; JSON error with `not_found` and the missing session name |
| `team_sessions(limit: 5)` | `completed`; a data envelope containing two session rows |
| `propose` for a reserved `.invalid` HTTP target | `completed`; receipt for `6db7acd45b161cd854395937e1348de1dee700adbb11e345e6bd93c418c1b817`, explicitly stating that acceptance does not confirm execution |

The failed read did not prevent the next calls in the same turn. This does not
establish recovery after a crash, quota exhaustion or an uncertain external effect.

The operator inspected ordinary owner cancellation eligibility and canceled the
proposal at sequence **47**, time **2026-10-21T06:00:01Z**. The final effect at head
**49** is done by cancellation, with zero approvals and zero claims. The reporting
task closed as `done`; no HTTP execution or landing is implied. The first trial's
proposal was also canceled normally, at sequence **40**.

The isolated trial executor stayed inactive. Zero claims therefore does not prove
policy refusal by an otherwise executable backend. The agent was stopped after
inspection; a subsequent status capture records three live spaces, zero outbox
and role backlogs, all three normal services active, and the trial executor inactive.

## Verification and limits

- All [PR 200 CI jobs](https://github.com/lmvdz/han/actions/runs/39551908524) passed
  at exact source `e8a9b43`, including the corrected integration fixture.
- Three targeted Windows parser/feed tests passed on the corrected source. The
  Linux release build passed. Fixtures cover split-secret masking, failure status,
  opaque-field omission and oversized-output suppression; the live calls use
  harmless data and do not independently certify those boundaries.
- On the preceding `35c0a1e` source, a Windows integration helper build failed with
  paging-capacity error 1455 before the behavioral test ran; a one-job retry passed.
  This historical result is not substituted for testing the corrected fixture.
- Completed calls delivered only as initial `tool_call` replay events still do not
  expose output. This correction covers completion updates, not every adapter or
  every replay/result shape.

[Selected evidence](evidence/mcp-result-trial-outcome.json) retains both trials'
tool bodies, final effects, task outcomes and owner cancellation receipts. Full
requests, Node API snapshots, deployment/process hashes and cleanup captures remain
in the author's `H:/HAN/test-runs/codex/mcp-*` and `deploy-mcp-*` artifacts for review.

# Trusted team automatic-landing trial

A real Codex task landed a README correction through HAN without a per-change
approval. [PR 182](https://github.com/lmvdz/han/pull/182) merged commit
`c65a18f814be950098374176b23f94b987432ff3` at `2026-10-21T00:05:09Z`.
The independent [round-5 review](beta-2026-10-20-round-5.md) verifies the positive
path and records its limits; the beta score remains 74/100.

## Setup and authority

The Linux home, node and executor used source `90d5e23`, whose tree matches
PR 181 merge `1f6c5d42baae86cfc4ab521e28fb988d5fc82a62`. The deployed Linux
binary SHA-256 was `da9023972654e20751b5fcea3cfbfdd228b967776211615b85f25b7344ff9216`.
The Windows person node used the matching implementation. Deployment hashes and
device confirmation are author observations, not independently reproduced by
the reviewer.

The production `han space create-trusted` command created a separate space,
`dddn17lj3s:runner.han.home.arpa`, scoped to `github:lmvdz/han`. Windows Hello
confirmed the grant and memberships. The runner service, agent and executor
were members; the human remained owner. An attempted owner role for the service
was refused. The original space's policy was not changed.

The trial executor reused the existing service's OS restrictions and repository
configuration, with separate state. Both its pre-landing `required` and
post-landing `full` check lists contained `quick`; backend was `github`.
No approval or presence setting was disabled.

## Recorded result

- Task: `ce9c4d5b37aa8679e04bc5ad9679080ac970663f97b863dedfad9bd9f054e820`.
- Session: `02f74d35e7078bc6dea726314bcea56200862728e49558831e67838d645933cf`.
- Proposal: `954f67c6916197dabd8c152b1b414d765e6c234304ea8459280edcd80fd9fc38`.
- Policy-derived tier: 1; `proposer_suffices: true`.
- Recorded effect: succeeded, one claim, no proposal approvals.
- Board: `land 954f67c69161: confirmed`.

The [outcome blob](evidence/trusted-landing-outcome.json) is 1,520 bytes with
SHA-256 `b39753383dc5598f8413fe2f853084d6932754c300bfe4102ef759838dcaa1ea`.
It binds the successful `quick` check to the exact landed commit. Its
[GitHub run](https://github.com/lmvdz/han/actions/runs/39519014683) ran scope and
spec checks; Rust tests, lint and doc-tests were skipped for this README-only
change. The nonrequired security scan was still running at claim time. This
does not demonstrate a full Rust test run before landing.

After confirmation, the session remained idle until the author stopped it
through `han agent stop`; the task then closed. The trial executor was stopped.
This is confirmed automatic landing, not evidence of autonomous session closure.

## Limits and follow-up

This used an existing account, installed adapter and author-operated setup. It
does not establish onboarding timing, independent two-person use, Windows Codex,
or live refusal/recovery coverage. The two-week multi-person acceptance target
remains unverified.

The trial exposed misleading proposal text and a transient board label that
assumed human approval was required. The follow-up distinguishes acceptance
from execution, uses the local effect view only as a presentation hint, and
leaves execution authority with the executor's current policy checks. It does
not infer that a proposer-authorized effect is free of guard requirements.

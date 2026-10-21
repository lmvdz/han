# Live terminal-output and headless-approval regression

A real Codex session now publishes its completed shell output in HAN's shared
transcript. The deployed approval screen also blocks signing on the headless
runner. This author-operated trial used an existing subscription login; it is
not fresh guided onboarding or independent team acceptance.

## Revision and deployment

[PR 196](https://github.com/lmvdz/han/pull/196) merged at
`4e99dc75a33a7a6507e2eacd157747ad4751e68a`. Its tree matches the tested source
`26bf6bdfe7b3915cf2a92a9253c6d3bc85561a4a`. All CI jobs passed at that source.
The Linux release build and 16 targeted Linux unit tests passed. Final-source
Windows transcript unit tests and the real host-to-node integration also passed;
the latter checks assembled masked output and test counts from a split summary.
An earlier Windows run used a stale assertion while its fixture changed and
failed; the final-source rerun passed in 12.81 seconds.

The deployed Linux binary SHA-256 is
`07156138a6abbc37004bf7f0dfabcb7bb201c80a742e443c5bd779e68e845c9a`.
Before replacement, the author checked all three services active, three spaces
live, zero pending outbox entries and zero agent roles. The installed file and
each service's `/proc/<pid>/exe` matched the new hash after restart. The first
node query ran before its socket was ready; a follow-up succeeded without
another restart. The previous binary was retained for rollback.

The source includes [PR 193](https://github.com/lmvdz/han/pull/193)'s headless
approval correction and [PR 192](https://github.com/lmvdz/han/pull/192)'s MCP
guidance correction. The Windows person node remains on its previous `ac85a15`
deployment. No presence configuration was changed.

## Transcript result

The installed Codex ACP adapter is version 2.2.2. Inspection of its installed
source showed that its default command-output channel is
`terminal_output_delta`; HAN previously ignored that field. Its installed
`dist/index.js` SHA-256 was
`6b1bb4e7e9caaf4aeca1042ae4aec2dc2433b0b0e4b1614b38a4d41699902aaa`.

The task requested metadata checks only, prohibited reading any credential
contents, and asked for a single reserved-address proposal for the UI check.
The completed shell tool row in the ordinary Node API contains:

```text
HAN_TOOL_OUTPUT_BEGIN
credential_readable=no
node_socket_present=no
HAN_TOOL_OUTPUT_END
```

These are observed tool-result bytes for `test -r` on the executor key path and
`test -S` on the operator socket path inside this session's sandbox. They are
stronger evidence than the prior trial's agent summary, but remain one bounded
metadata check under the host's observation. No credential contents were read.
This is not a general credential-isolation or exfiltration audit.

The `mcp.han.propose` tool row still has empty content in this capture. The
terminal-delta correction does not claim to populate every adapter tool-result
shape. Proposal acceptance is independently visible in the effect view.

## Approval screen and cleanup

The task submitted exactly one `http_request` proposal for
`https://han-permission-probe.invalid/headless-ui`. A retained PTY script opened
that exact target's card, observed the headless signing reason, pressed the
review/sign keys, and observed no signing panel. It exited intentionally after
4.41 seconds. This validates the deployed UI correction; it does not exercise a
signed approval at the home.

The owner cancelled through the Windows person's ordinary node API. Receipt
sequence 33 is timestamped `2026-10-21T05:19:18Z`. The final effect is done by
cancellation with zero approvals and zero claims. The reporting task closed as
done; that does not mean the proposed HTTP effect executed. The author stopped
the agent through `han agent stop`. Final checks show all three normal services
active, all three spaces live, no pending outbox entries and zero agent roles.
The separate trial executor remained inactive throughout this UI-only probe.

## Retained evidence and limits

- Task: `e5212ff47fd5e9d556d65add1fa2440c346005bd709e4aa9c09bff65e19c675c`.
- Session: `1865dbc09971edf2cd370d8e515a4613c934a74cead053167c18e8974d319362`.
- Proposal: `9d16a3443d8006d730d2fcec7378bf034229279d8fefbc49e40fd2269c53e91b`.
- [Selected ordinary API rows and UI script result](evidence/transcript-trial-outcome.json).
- Full local captures: `H:/HAN/test-runs/codex/transcript-*`,
  `deploy-deltas-*`, and `deltas-26bf6bd-build.log`.

The selected extract and deployment observations are author-retained evidence,
not an independent capture. Masking across chunks and oversized-output omission
are regression-test evidence using synthetic secrets; this live run intentionally
used only harmless metadata. Delta text is published after completion so masking
sees the assembled result; status and title updates remain live. Complete
snapshots replace deltas, and over-limit text receives an explicit omission
notice rather than a misleading concatenation across missing bytes.

No HTTP execution, signed non-owner rejection, revoked-authority test, new login,
landed source change or two-person acceptance was attempted in this trial. The
full guided login-to-landing path and unavailable independent testers remain
open requirements under the unchanged beta rubric.

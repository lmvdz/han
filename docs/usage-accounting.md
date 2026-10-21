# Usage and cost coverage

Token counts and cost are separate observations. A subscription session can
report tokens while its monetary cost remains unmeasured. The TUI session pane
shows **cost: unmeasured** in that case; measured zero is shown only when the host
has complete priced accounting and the original unit is available.

The API-key proxy counts provider usage and prices it with the session's price
table. A table alone does not establish complete accounting. An unpriced model,
missing usage, interrupted response, or outstanding request makes the cost
incomplete. An unmeasured completed call leaves a gap for the rest of that
session. Later priced calls cannot reconstruct it. A response must report both
input and output counts and end cleanly; supported SSE responses must also have
their terminal marker. Unsupported usage formats remain unmeasured.

The proxy remains the source of API-key accounting. Adapter metadata contributes
token counts for subscription sessions and does not overwrite proxy totals.
The host omits an effect proposal's cost when complete accounting is unavailable;
it never substitutes a partial amount or an invented zero. The executor and
policy still decide whether that proposal is permitted.

Existing budget behavior for unpriced models remains: those calls reserve no
priced amount. Once such a gap is observed, the budget status says that some
model calls have unmeasured cost. This is not a guarantee of a monetary cap for
unpriced calls or costs the provider does not report.

## API compatibility

`tokens.cost_measured` is an additive, default-false presentation extension to
`han.status/1`. The existing numeric `tokens.cost` remains for older readers;
updated readers must not treat it as a complete amount unless the flag is true
and the session's `budget.unit` is available. No status field grants authority.
Older payloads without the flag are conservatively unmeasured. Older clients
that ignore it can still display a misleading zero and should be updated.

Cost-view rows include `cost_measured`, `unmeasured_sessions`, and `subtotals`
grouped by original unit. A missing token report from an agent or unknown actor
is counted as unmeasured. A known service with no token report is omitted.
Changing the space's budget unit does not relabel historical session amounts.
Unlike units are never added into one monetary total. If all sessions are
measured but their units differ, the per-unit subtotals are complete while the
single `cost` field is not a meaningful total (`cost_measured: false`).
An aggregate that exceeds the numeric range also sets `cost_overflowed: true`.
Its saturated subtotals are lower bounds, including when multiple units are
present; they must not be presented as complete amounts even if every session
reported a measured cost.

These are host-reported accounting estimates, not a provider invoice. Counts,
coverage flags and units survive the normal status views; no private credential
or billing lookup is needed to inspect them.

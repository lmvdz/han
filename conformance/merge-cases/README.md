# Merge cases

Real three-way conflicts from HAN's own history, kept as a corpus for the landing pipeline (r33 land profile §5, conflicts as data): the deterministic merge, structural merge tools, candidate generators and the judged ranker are all measured against them. Nothing here is compiled; the files keep their `.rs` extension so language-aware tools pick the right grammar.

Each case is a directory:

| File | What it is |
|---|---|
| `base.rs` | the merge base |
| `ours.rs`, `theirs.rs` | the two sides, from the PR heads before either was rebased |
| `landed.rs` | the file as it landed on main: the resolution a person accepted |
| `git.rs` | `git merge-file` output (conflict markers) |
| `git3.rs` | the same in diff3 style |
| `mgf.rs` | `mergiraf merge` output (mergiraf 0.20.0) |
| `solved.rs` | `mergiraf solve` output on the diff3 file |

## Cases

| Case | Base | Ours | Theirs | Plain git | mergiraf 0.20.0 | What resolves it |
|---|---|---|---|---|---|---|
| `c99_sandbox` (`crates/han-host/src/sandbox/mod.rs`) | a46f596 | d9f9c17 (#99) | d52ce08 (#100) | conflict | not resolved (exit 1): both sides rewrote neighbouring rows of a doc-comment table | a line split: #99's MXC row with #100's bubblewrap row |
| `c92_bindings` (`crates/han-node/src/api/bindings.rs`) | 3c3546a | 160f476 (#92) | 7813c2e (#93) | conflict | **exit 0 but wrong**: two `use super::{…}` blocks, `cargo check` fails with E0252 | the union of both import lists |
| `c92_agent_start` (`crates/han-node/tests/agent_start.rs`) | 3c3546a | 160f476 (#92) | 7813c2e (#93) | conflict | resolved: both new tests kept, compiles | mergiraf's result |

The `c92_bindings` case is why a tool's clean exit is a claim to verify (build and test), never a fact.

mergiraf is GPL-3.0-only; HAN only ever runs it as an optional external tool found on PATH and never links or ships it. Its outputs here are recorded observations.

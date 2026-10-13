# Every task of the repository. `just --list` shows them.
# Needs: python 3.11+ with `cryptography`, node 22 with npm, a Rust toolchain (rust-toolchain.toml) with cargo-deny, and (test-py) the venv at witnesses/impl-py/.venv.
# To recreate the venv: python -m venv witnesses/impl-py/.venv, then install `witnesses/impl-py[dev]` into it (pip install -e "witnesses/impl-py[dev]").

set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]
set dotenv-load := false

export PYTHONIOENCODING := "utf-8"

default:
    @just --list

# the quick spec suite: tsc on han.ts, consistency, vectors, mutations, fuzz, wire, docs match (about 3 min)
check-spec: ts-build
    python ref/run_all.py --quick

# the Policy and wire conformance vectors; regenerating them must not change a file
vectors: ts-build
    python ref/run_vectors.py
    python ref/make_vectors.py
    git diff --exit-code -- conformance/vectors
    python ref/make_wire_vectors.py
    git diff --exit-code -- conformance/wire-vectors
    python ref/run_wire_vectors.py --launch py
    python ref/run_wire_vectors.py --launch ts

# differential fuzz, ref against the TypeScript witness
fuzz seeds="200": ts-build
    python ref/fuzz_diff.py --seeds {{seeds}}

# the cross-implementation matrix; writes conformance/interop/RESULTS.md
interop: ts-build
    python conformance/interop/run.py
    python conformance/interop/report.py

# the Python witness tests (uses its own venv)
[working-directory: 'witnesses/impl-py']
test-py:
    {{ if os() == "windows" { ".venv/Scripts/python.exe" } else { ".venv/bin/python" } }} -m pytest -q

# build the TypeScript witness and replay the Policy vectors
[working-directory: 'witnesses/cleanroom-ts']
test-ts: ts-build
    npm run vectors

[working-directory: 'witnesses/cleanroom-ts']
ts-build:
    npm install --no-audit --no-fund --no-package-lock
    npm run build

# build the Rust workspace
rs-build:
    cargo build --workspace --locked

# Rust: format and clippy checks, then every test (han-core replays the wire vectors and golden transcripts)
rs-test:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo test --workspace --locked

# Rust dependency policy: advisories, bans, licences, sources (needs `cargo install cargo-deny --locked`)
rs-deny:
    cargo deny check

# derive the counts in the docs from the checks (use `--check` to only compare)
docs-sync *args:
    python ref/sync_docs.py {{args}}

# deep checks that CI runs nightly: 1000-seed fuzz and the guard model mutations at depth 4
nightly: ts-build
    python ref/fuzz_diff.py --seeds 1000
    python ref/model_guard.py --depth 4 --all-mutations

# the full pull-request set
ci: check-spec vectors fuzz test-py test-ts interop rs-build rs-test rs-deny

# differential fuzz, ref against the Rust Policy (builds crates/han-core's han-conform)
fuzz-rust seeds="200":
    cargo build --release --locked -p han-core --bin han-conform
    python ref/fuzz_diff.py --target rust --seeds {{seeds}}

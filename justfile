# Every task of the repository. `just --list` shows them.
# Needs: python 3.11+ with `cryptography`, node 22 with npm, and (test-py) the venv at witnesses/impl-py/.venv.
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

# derive the counts in the docs from the checks (use `--check` to only compare)
docs-sync *args:
    python ref/sync_docs.py {{args}}

# deep checks that CI runs nightly: 1000-seed fuzz and the guard model mutations at depth 4
nightly: ts-build
    python ref/fuzz_diff.py --seeds 1000
    python ref/model_guard.py --depth 4 --all-mutations

# the full pull-request set
ci: check-spec vectors fuzz test-py test-ts interop

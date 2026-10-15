#!/usr/bin/env python3
"""What a pull request needs from the 'quick' job (workflow .github/workflows/ci.yml).

  python .github/scripts/ci_scope.py BASE      BASE is the sha the PR is based on; empty means a push (run everything)

Prints key=value lines for $GITHUB_OUTPUT:
  rust      true when the Rust steps (fmt, clippy, tests) must run at all
  packages  the `-p crate` arguments of the affected crates, or empty for the whole workspace
  doc_packages  the same for doc-tests, only crates with a library target (`cargo test --doc` refuses a bin-only
            crate), empty for the whole workspace, or `none` when no affected crate has a library
  spec      true when the doc-derived counts must be re-checked (ref/sync_docs.py --check): a Markdown file changed

Rust steps are skipped for a PR that only touches docs/, research/, top-level Markdown or spec/**/*.md: no Rust test
reads those (the vectors the tests read live in conformance/). The Python spec checks (vectors, consistency and
sync_docs, which verifies every <!--s:key--> marker in the spec text) always run.

Otherwise the affected crates are the ones with a changed file plus every workspace crate that depends on them
(normal, build and dev dependencies, from `cargo metadata`). Any change outside a crate directory (Cargo.lock, the root
Cargo.toml, rust-toolchain.toml, .cargo/, .github/, conformance/, ...) runs the whole workspace.
"""
import json
import re
import subprocess
import sys

DOC_ONLY = re.compile(r"^(docs|research)/|^[^/]+\.md$|^spec/.*\.md$")


def sh(*cmd):
    return subprocess.run(cmd, check=True, capture_output=True, text=True, encoding="utf-8").stdout


def emit(rust, packages, spec, doc_packages=""):
    print(f"rust={str(rust).lower()}")
    print(f"packages={packages}")
    print(f"doc_packages={doc_packages}")
    print(f"spec={str(spec).lower()}")


def main():
    base = sys.argv[1] if len(sys.argv) > 1 else ""
    if not base:
        emit(True, "", True)
        return
    files = [f for f in sh("git", "diff", "--name-only", f"{base}...HEAD").splitlines() if f]
    spec = any(f.endswith(".md") for f in files)
    code = [f for f in files if not DOC_ONLY.search(f)]
    if not code:
        sys.stderr.write("only documentation changed: no Rust steps\n")
        emit(False, "", spec)
        return

    meta = json.loads(sh("cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"))
    root = meta["workspace_root"].replace("\\", "/").rstrip("/") + "/"
    crates = {}  # directory (relative to the root, with a trailing slash) -> package name
    for p in meta["packages"]:
        d = p["manifest_path"].replace("\\", "/").removesuffix("Cargo.toml")
        assert d.startswith(root), d
        crates[d[len(root):]] = p["name"]
    names = set(crates.values())
    libs = {p["name"] for p in meta["packages"] if any(k in ("lib", "rlib", "proc-macro") for t in p["targets"] for k in t["kind"])}
    users = {n: set() for n in names}  # crate -> workspace crates that depend on it
    for p in meta["packages"]:
        for dep in p["dependencies"]:
            if dep["name"] in names and dep["name"] != p["name"]:
                users[dep["name"]].add(p["name"])

    changed = set()
    for f in code:
        hit = [d for d in crates if f.startswith(d)]
        if not hit:
            sys.stderr.write(f"{f} is outside every crate: the whole workspace\n")
            emit(True, "", spec)
            return
        changed.add(crates[max(hit, key=len)])

    affected, todo = set(), sorted(changed)
    while todo:
        n = todo.pop()
        if n not in affected:
            affected.add(n)
            todo.extend(users[n])
    sys.stderr.write(f"changed crates: {sorted(changed)}\naffected crates: {sorted(affected)}\n")
    if affected == names:
        emit(True, "", spec)
    else:
        docs = sorted(affected & libs)
        emit(True, " ".join(f"-p {n}" for n in sorted(affected)), spec, " ".join(f"-p {n}" for n in docs) or "none")


main()

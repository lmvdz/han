#!/usr/bin/env sh
# Copy the readable sources into docs/src for mdBook. Run from the repo root.
set -e
cp README.md PLAN.md docs/runner.md docs/src/
cp docs/getting-started.md docs/status.md docs/src/
cp spec/goal.md docs/src/
mkdir -p docs/src/core docs/src/profiles docs/src/decisions
cp spec/core/*.md docs/src/core/
cp spec/profiles/*.md docs/src/profiles/
cp spec/decisions/*.md docs/src/decisions/
mkdir -p docs/src/ui
cp spec/ui/*.md docs/src/ui/
cp spec/profiles/*.md docs/src/profiles/ 2>/dev/null || true

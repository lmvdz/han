#!/bin/sh
# Thin wrapper kept for old habits: `han runner setup` installs the executor's user, directories and unit
# (left disabled). Then `han executor add` makes its key, config and profile (docs/runner.md).
set -eu
exec han runner setup "$@"

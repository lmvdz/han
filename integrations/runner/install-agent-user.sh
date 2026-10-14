#!/bin/sh
# Thin wrapper kept for old habits: `han runner setup` does this (and more), idempotently.
# usage: install-agent-user.sh han-agent1 [node-user]   (run as root)
set -eu
user=${1:?usage: install-agent-user.sh han-agent<N> [node-user]}
node=${2:-han}
n=${user#han-agent}
case "$n" in ''|*[!0-9]*) echo "the polkit rule covers han-agent<N> only" >&2; exit 1;; esac
exec han runner setup --node-user "$node" --agents "$n"

#!/bin/sh
# Prepares one OS user to run one agent's host (SR-83): usage: install-agent-user.sh han-agent1 [node-user]
# Run as root, once per agent, after installing han-agent@.service and 50-han-agent.rules (docs/runner.md 7.1).
set -eu
[ "$(id -u)" = 0 ] || { echo "run as root" >&2; exit 1; }
user=${1:?usage: install-agent-user.sh <os-user> [node-user]}
node=${2:-han}
here=$(cd "$(dirname "$0")" && pwd)
case "$user" in han-agent[0-9]*) ;; *) echo "the polkit rule covers han-agent<N> only" >&2; exit 1;; esac
install -d -m 755 /var/lib/han-agents
id "$user" >/dev/null 2>&1 || useradd --system --create-home --home-dir "/var/lib/han-agents/$user" \
  --shell /usr/sbin/nologin "$user"
passwd -l "$user" >/dev/null
usermod -aG "$user" "$node"
chmod 2770 "/var/lib/han-agents/$user"
install -m 644 "$here/han-agent@.service" /etc/systemd/system/han-agent@.service
install -m 644 "$here/50-han-agent.rules" /etc/polkit-1/rules.d/50-han-agent.rules
systemctl daemon-reload
echo "ready: $user. Restart the node service so it picks up its new group."

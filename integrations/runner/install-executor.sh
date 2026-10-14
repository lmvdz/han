#!/bin/sh
# Installs the HAN executor on a Linux runner: the han-exec account, its directories and the systemd unit.
# Run as root. Afterwards: fill /etc/han/exec.env and /etc/han/exec.json, put the GitHub App's PEM at
# /var/lib/han-exec/github-app.pem (mode 600, owner han-exec), register and confirm @exec:<server>
# (docs/runner.md section 9), then `systemctl enable --now han-exec`.
set -eu
[ "$(id -u)" = 0 ] || { echo "run as root" >&2; exit 1; }
here=$(cd "$(dirname "$0")" && pwd)
id han-exec >/dev/null 2>&1 || useradd --system --create-home --home-dir /var/lib/han-exec \
  --shell /usr/sbin/nologin han-exec
install -d -o han-exec -g han-exec -m 700 /var/lib/han-exec/keys /var/lib/han-exec/state
install -d -m 755 /etc/han
[ -e /etc/han/exec.env ] || install -m 644 /dev/stdin /etc/han/exec.env <<'EOF'
HAN_SERVER=
HAN_SPACE=
HAN_RELAY=n0
HAN_HOME_TICKET=
EOF
[ -e /etc/han/exec.json ] || install -m 644 /dev/stdin /etc/han/exec.json <<'EOF'
{"repos": [], "branch_prefixes": ["han/"]}
EOF
install -m 644 "$here/han-exec.service" /etc/systemd/system/han-exec.service
systemctl daemon-reload
echo "installed; key: sudo -u han-exec han key /var/lib/han-exec/keys/keys/agent-exec-<server>.key"

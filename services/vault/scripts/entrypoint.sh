#!/bin/sh
# Starts OpenBao and, when configured, the backup loop (ADR 0005).
set -eu
mkdir -p /openbao/data/raft /openbao/data/audit
chmod 0700 /openbao/data/raft /openbao/data/audit
if [ -n "${BACKUP_INTERVAL_SECONDS:-}" ] && [ -n "${BAO_BACKUP_TOKEN:-}" ]; then
  /usr/local/bin/backup.sh loop &
fi
exec bao server -config=/openbao/config/dl-vault.hcl

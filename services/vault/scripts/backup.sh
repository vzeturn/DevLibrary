#!/bin/sh
# Raft snapshot -> R2 (ADR 0005). Runs inside the dl-vault machine.
#   backup.sh once    one snapshot now
#   backup.sh loop    every BACKUP_INTERVAL_SECONDS (renews the periodic backup token)
# Environment (Fly secrets; values never printed): BAO_BACKUP_TOKEN, CLOUDFLARE_API_TOKEN
# (R2 edit only), CF_ACCOUNT_ID; R2_BUCKET from fly.toml.
set -eu
: "${BAO_ADDR:=http://127.0.0.1:8200}"
export BAO_ADDR

log() { printf '{"ts":"%s","event":"%s","detail":"%s"}\n' "$(date -u +%FT%TZ)" "$1" "${2:-}"; }

once() {
  ts=$(date -u +%Y%m%dT%H%M%SZ)
  f=$(mktemp /tmp/dl-vault-XXXXXX)
  trap 'rm -f "$f"' EXIT
  if ! BAO_TOKEN=$BAO_BACKUP_TOKEN bao operator raft snapshot save "$f" >/dev/null 2>&1; then
    log backup.snapshot_failed "sealed or token expired"
    return 1
  fi
  url="https://api.cloudflare.com/client/v4/accounts/$CF_ACCOUNT_ID/r2/buckets/$R2_BUCKET/objects/dl-vault/$ts.snap"
  # The token goes through a curl config on stdin, never on the command line.
  if printf 'header = "Authorization: Bearer %s"\n' "$CLOUDFLARE_API_TOKEN" |
    curl -fsS -K - -X PUT --data-binary @"$f" "$url" >/dev/null; then
    log backup.uploaded "dl-vault/$ts.snap $(wc -c <"$f") bytes"
  else
    log backup.upload_failed "dl-vault/$ts.snap"
    return 1
  fi
  rm -f "$f"
  trap - EXIT
}

case "${1:-once}" in
once) once ;;
loop)
  sleep 120 # let the server start (and be unsealed) first
  while :; do
    BAO_TOKEN=$BAO_BACKUP_TOKEN bao token renew >/dev/null 2>&1 || log backup.renew_failed
    once || true
    sleep "${BACKUP_INTERVAL_SECONDS:-21600}"
  done
  ;;
*) echo "usage: backup.sh once|loop" >&2; exit 2 ;;
esac

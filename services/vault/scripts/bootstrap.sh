#!/usr/bin/env bash
# One-time bootstrap of a freshly deployed dl-vault (ADR 0005, ADR 0006), in two steps with the
# owner's unseal in between. Run from an operator machine with the bao (or vault) CLI and jq:
#
#   BAO_ADDR=https://dl-vault.fly.dev OWNER_PGP_KEYS=k1,k2,k3 bootstrap.sh init [state-dir]
#     -> the owner unseals with scripts/unseal.sh (two of the three shares)
#   BAO_ADDR=https://dl-vault.fly.dev bootstrap.sh configure [state-dir]
#
# OWNER_PGP_KEYS: three comma-separated paths to the owner's public key, base64 of
# `gpg --export <id>` (the same file three times is fine). The shares come back encrypted to
# it, so no plain share ever exists here. state-dir (default ~/.config/dl-vault, mode 0700):
#   owner-shares.json  the three encrypted shares, for the owner; nothing else
#   state.json         root token (until configure revokes it), then the AppRole and backup
#                      credentials for their clients
# Only names are printed, never values.
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
step=${1:-}
dir=${2:-$HOME/.config/dl-vault}
: "${BAO_ADDR:?set BAO_ADDR}"
export BAO_ADDR
bao=$(command -v bao || command -v vault) || { echo "need the bao or vault CLI" >&2; exit 2; }
umask 077; mkdir -p "$dir"; chmod 0700 "$dir"
state=$dir/state.json shares=$dir/owner-shares.json
# `status` exits 2 when sealed but still prints JSON; an empty answer means unreachable.
status() { local s; s=$("$bao" status -format=json || true); [ -n "$s" ] || { echo "cannot reach $BAO_ADDR" >&2; exit 2; }; printf '%s' "$s"; }

init() {
  : "${OWNER_PGP_KEYS:?set OWNER_PGP_KEYS to three comma-separated public key files}"
  [ ! -e "$state" ] && [ ! -e "$shares" ] || { echo "$dir already holds a bootstrap: refusing to overwrite" >&2; exit 2; }
  [ "$(status | jq -r .initialized)" = false ] || { echo "already initialised" >&2; exit 2; }
  IFS=, read -r -a keys <<<"$OWNER_PGP_KEYS"
  [ "${#keys[@]}" -eq 3 ] || { echo "OWNER_PGP_KEYS needs exactly 3 entries" >&2; exit 2; }
  for k in "${keys[@]}"; do [ -s "$k" ] || { echo "missing key file: $k" >&2; exit 2; }; done
  # If anything below fails, $raw is kept: it holds the root token and the encrypted shares.
  local raw; raw=$(mktemp "$dir/init.XXXXXX")
  "$bao" operator init -key-shares=3 -key-threshold=2 -pgp-keys="$OWNER_PGP_KEYS" -format=json >"$raw"
  # The shares in $raw are PGP messages for the owner; only the root token is plain.
  jq -e '(.unseal_keys_b64 | length == 3) and (.root_token | length > 0)' "$raw" >/dev/null \
    || { echo "init output incomplete; $raw kept for recovery" >&2; exit 1; }
  jq '{unseal_keys_b64_pgp: .unseal_keys_b64, threshold: 2}' "$raw" >"$shares"
  jq '{root_token}' "$raw" >"$state"
  rm -f "$raw"
  echo "initialised; encrypted shares for the owner in $shares, root token in $state"
  echo "next: the owner runs scripts/unseal.sh twice (two different shares), then: bootstrap.sh configure"
}

configure() {
  [ -s "$state" ] || { echo "no $state: run init first" >&2; exit 2; }
  [ "$(status | jq -r .sealed)" = false ] || { echo "vault is sealed: the owner unseals first (scripts/unseal.sh)" >&2; exit 2; }
  BAO_TOKEN=$(jq -er .root_token "$state") || { echo "no root token in $state (configure already ran?)" >&2; exit 2; }
  export BAO_TOKEN VAULT_TOKEN=$BAO_TOKEN
  # Every step below is safe to re-run after a partial failure: each one checks first.
  trap 'echo "configure stopped at line $LINENO. The root token is still in $state: fix the cause and run configure again; it skips what is done." >&2' ERR
  local audits auths mounts
  audits=$("$bao" audit list -format=json 2>/dev/null || echo '{}')
  jq -e 'has("file/")' <<<"$audits" >/dev/null || "$bao" audit enable file file_path=/openbao/data/audit/audit.log >/dev/null
  jq -e 'has("stdout/")' <<<"$audits" >/dev/null || "$bao" audit enable -path=stdout file file_path=stdout >/dev/null
  mounts=$("$bao" secrets list -format=json)
  jq -e 'has("kv/")' <<<"$mounts" >/dev/null || "$bao" secrets enable -path=kv -version=2 kv >/dev/null
  for p in hub-ai cloud-ai importer backup; do "$bao" policy write "$p" "$here/policies/$p.hcl" >/dev/null; done
  auths=$("$bao" auth list -format=json)
  jq -e 'has("approle/")' <<<"$auths" >/dev/null || "$bao" auth enable approle >/dev/null
  for r in hub-ai cloud-ai; do
    "$bao" write "auth/approle/role/$r" token_policies="$r" token_ttl=1h token_max_ttl=4h \
      secret_id_ttl=2160h >/dev/null
  done
  "$bao" write auth/approle/role/importer token_policies=importer token_ttl=1h token_max_ttl=1h \
    token_num_uses=0 secret_id_ttl=24h secret_id_num_uses=1 >/dev/null

  # Each value is checked before it is stored; the root token is revoked only after all are in $state.
  get() { local v; v=$("$@"); [ -n "$v" ] || { echo "empty value from: $*" >&2; return 1; }; printf '%s' "$v"; }
  local hr hs cr cs ir is bt tmp
  hr=$(get "$bao" read -field=role_id auth/approle/role/hub-ai/role-id)
  hs=$(get "$bao" write -f -field=secret_id auth/approle/role/hub-ai/secret-id)
  cr=$(get "$bao" read -field=role_id auth/approle/role/cloud-ai/role-id)
  cs=$(get "$bao" write -f -field=secret_id auth/approle/role/cloud-ai/secret-id)
  ir=$(get "$bao" read -field=role_id auth/approle/role/importer/role-id)
  is=$(get "$bao" write -f -field=secret_id auth/approle/role/importer/secret-id)
  bt=$(get "$bao" token create -orphan -policy=backup -period=24h -field=token)
  tmp=$(mktemp "$dir/state.XXXXXX")
  # Values reach jq through the environment, not argv; root_token is dropped in the same write.
  HR=$hr HS=$hs CR=$cr CS=$cs IR=$ir IS=$is BT=$bt jq \
    '{hub_ai:{role_id:env.HR,secret_id:env.HS}, cloud_ai:{role_id:env.CR,secret_id:env.CS},
      importer:{role_id:env.IR,secret_id:env.IS}, backup_token:env.BT, root_token:.root_token}' \
    "$state" >"$tmp"
  jq -e '[.hub_ai.secret_id, .cloud_ai.secret_id, .importer.secret_id, .backup_token] | all(length > 0)' "$tmp" >/dev/null \
    || { rm -f "$tmp"; echo "credential check failed; root token kept in $state" >&2; exit 1; }
  mv "$tmp" "$state"
  "$bao" token revoke -self >/dev/null
  tmp=$(mktemp "$dir/state.XXXXXX"); jq 'del(.root_token)' "$state" >"$tmp" && mv "$tmp" "$state"
  trap - ERR
  echo "audit on (file + stdout), kv-v2 at kv/, policies, AppRoles hub-ai cloud-ai importer, backup token; root token revoked"
  echo "credentials in $state: hub_ai, cloud_ai, importer (single use, 24 h), backup_token"
}

case $step in
  init) init ;;
  configure) configure ;;
  *) echo "usage: bootstrap.sh init|configure [state-dir]" >&2; exit 2 ;;
esac

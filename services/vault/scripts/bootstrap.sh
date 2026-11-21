#!/usr/bin/env bash
# One-time bootstrap of a freshly deployed dl-vault (ADR 0005). Run from an operator machine:
#
#   BAO_ADDR=https://dl-vault.fly.dev services/vault/scripts/bootstrap.sh [out-file]
#
# Writes every secret it creates (3 unseal shares, AppRole role/secret IDs, backup token)
# to out-file (default ~/.config/dl-vault/bootstrap.json, mode 0600) and prints only names.
# The root token is revoked at the end. Needs the bao (or vault) CLI and jq.
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-$HOME/.config/dl-vault/bootstrap.json}
: "${BAO_ADDR:?set BAO_ADDR}"
export BAO_ADDR
bao=$(command -v bao || command -v vault) || { echo "need the bao or vault CLI" >&2; exit 2; }
[ -e "$out" ] && { echo "$out exists: refusing to overwrite a previous bootstrap" >&2; exit 2; }
umask 077; mkdir -p "$(dirname "$out")"

[ "$("$bao" status -format=json | jq -r .initialized)" = false ] || { echo "already initialised" >&2; exit 2; }
"$bao" operator init -key-shares=3 -key-threshold=2 -format=json >"$out"
# From here a failure leaves an initialised vault: the shares and the root token are in $out.
trap 'echo "bootstrap stopped at line $LINENO. $out still holds the unseal shares and the root token: finish the remaining steps by hand with that root token (README, Setting it up), then revoke it." >&2' ERR

# Secrets go to the CLI on stdin ("-"), never on its command line.
for i in 0 1; do jq -r ".unseal_keys_b64[$i]" "$out" | "$bao" operator unseal - >/dev/null; done
BAO_TOKEN=$(jq -r .root_token "$out"); export BAO_TOKEN VAULT_TOKEN=$BAO_TOKEN

"$bao" audit enable file file_path=/openbao/data/audit/audit.log >/dev/null
"$bao" audit enable -path=stdout file file_path=stdout >/dev/null
"$bao" secrets enable -path=kv -version=2 kv >/dev/null
for p in hub-ai cloud-ai backup; do "$bao" policy write "$p" "$here/policies/$p.hcl" >/dev/null; done
"$bao" auth enable approle >/dev/null
for r in hub-ai cloud-ai; do
  "$bao" write "auth/approle/role/$r" token_policies="$r" token_ttl=1h token_max_ttl=4h \
    secret_id_ttl=2160h >/dev/null
done

# Each value is checked before it is stored; the root token is revoked only after all are in $out.
get() { local v; v=$("$@"); [ -n "$v" ] || { echo "empty value from: $*" >&2; return 1; }; printf '%s' "$v"; }
hr=$(get "$bao" read -field=role_id auth/approle/role/hub-ai/role-id)
hs=$(get "$bao" write -f -field=secret_id auth/approle/role/hub-ai/secret-id)
cr=$(get "$bao" read -field=role_id auth/approle/role/cloud-ai/role-id)
cs=$(get "$bao" write -f -field=secret_id auth/approle/role/cloud-ai/secret-id)
bt=$(get "$bao" token create -orphan -policy=backup -period=24h -field=token)
tmp=$(mktemp); trap 'rm -f "$tmp"' EXIT
# Values reach jq through the environment, not argv.
HR=$hr HS=$hs CR=$cr CS=$cs BT=$bt jq \
  '. + {hub_ai:{role_id:env.HR,secret_id:env.HS}, cloud_ai:{role_id:env.CR,secret_id:env.CS}, backup_token:env.BT}' \
  "$out" >"$tmp"
jq -e '[.hub_ai.secret_id, .cloud_ai.secret_id, .backup_token, (.unseal_keys_b64|length == 3)] | all' "$tmp" >/dev/null
mv "$tmp" "$out"

"$bao" token revoke -self >/dev/null
jq 'del(.root_token)' "$out" >"$tmp" && mv "$tmp" "$out"
trap - ERR
echo "initialised, unsealed, audit on (file + stdout), kv-v2 at kv/, AppRoles hub-ai and cloud-ai, backup token; root token revoked"
echo "secrets written to $out: unseal_keys_b64[0..2], hub_ai, cloud_ai, backup_token"

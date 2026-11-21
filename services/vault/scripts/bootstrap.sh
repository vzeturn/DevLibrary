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
mkdir -p "$(dirname "$out")"; umask 077

[ "$("$bao" status -format=json | jq -r .initialized)" = false ] || { echo "already initialised" >&2; exit 2; }
"$bao" operator init -key-shares=3 -key-threshold=2 -format=json >"$out"
for i in 0 1; do "$bao" operator unseal "$(jq -r ".unseal_keys_b64[$i]" "$out")" >/dev/null; done
BAO_TOKEN=$(jq -r .root_token "$out"); export BAO_TOKEN VAULT_TOKEN=$BAO_TOKEN

"$bao" audit enable file file_path=/openbao/data/audit/audit.log >/dev/null
"$bao" audit enable -path=stdout file file_path=stdout >/dev/null
"$bao" secrets enable -path=kv -version=2 kv >/dev/null
for p in hub-ai cloud-ai backup; do "$bao" policy write "$p" "$here/policies/$p.hcl" >/dev/null; done
"$bao" auth enable approle >/dev/null
for r in hub-ai cloud-ai; do
  "$bao" write "auth/approle/role/$r" token_policies="$r" token_ttl=1h token_max_ttl=4h \
    secret_id_ttl=2160h token_no_default_policy=false >/dev/null
done

tmp=$(mktemp); trap 'rm -f "$tmp"' EXIT
jq --arg hr "$("$bao" read -field=role_id auth/approle/role/hub-ai/role-id)" \
   --arg hs "$("$bao" write -f -field=secret_id auth/approle/role/hub-ai/secret-id)" \
   --arg cr "$("$bao" read -field=role_id auth/approle/role/cloud-ai/role-id)" \
   --arg cs "$("$bao" write -f -field=secret_id auth/approle/role/cloud-ai/secret-id)" \
   --arg bt "$("$bao" token create -orphan -policy=backup -period=24h -field=token)" \
   '. + {hub_ai:{role_id:$hr,secret_id:$hs}, cloud_ai:{role_id:$cr,secret_id:$cs}, backup_token:$bt}' \
   "$out" >"$tmp" && mv "$tmp" "$out"

"$bao" token revoke -self >/dev/null
jq 'del(.root_token)' "$out" >"$tmp" && mv "$tmp" "$out"
echo "initialised, unsealed, audit on (file + stdout), kv-v2 at kv/, AppRoles hub-ai and cloud-ai, backup token; root token revoked"
echo "secrets written to $out: unseal_keys_b64[0..2], hub_ai, cloud_ai, backup_token"

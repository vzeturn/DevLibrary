#!/usr/bin/env bash
# The owner's unseal step (ADR 0006): run on the owner's own machine, never by an AI session.
#
#   BAO_ADDR=https://dl-vault.fly.dev unseal.sh <owner-shares.json> <index 0|1|2>
#
# Decrypts one share with gpg (asks for the key's passphrase) and sends it as the JSON body of
# PUT /v1/sys/unseal through curl's stdin: the plain share is never written to disk, argv or the
# screen. (`bao operator unseal` reads a key only from a terminal or from argv, so it is not used.)
# Run it twice with two different indexes. Needs gpg, jq and curl.
set -euo pipefail
file=${1:?usage: unseal.sh <owner-shares.json> <index>} i=${2:?usage: unseal.sh <owner-shares.json> <index>}
: "${BAO_ADDR:?set BAO_ADDR}"
export BAO_ADDR
case $i in 0|1|2) ;; *) echo "index must be 0, 1 or 2" >&2; exit 2 ;; esac
res=$(jq -er ".unseal_keys_b64_pgp[$i]" "$file" | base64 -d | gpg --decrypt --quiet \
  | jq -Rc '{key: .}' | curl -fsS -X PUT --data-binary @- "$BAO_ADDR/v1/sys/unseal")
jq -r '"sealed: \(.sealed), progress: \(.progress)/\(.t)"' <<<"$res"

#!/usr/bin/env bash
# Release steps for dl-agent, from any machine with the repo (no Docker, no machine paths).
#
#   scripts/release/rel.sh fetch <version>            staged build (R2 dl-agent-builds/) -> dist/dl-agent/<version>/
#   scripts/release/rel.sh sign <version> <channel>   dist/dl-agent/<version>/<channel>.json (signed manifest)
#   scripts/release/rel.sh upload <version>           dist/dl-agent/<version>/<target>/ -> R2 dl-agent/<version>/
#   scripts/release/rel.sh canary <version>           PUT /admin/releases/dl-agent/<version> (canary manifest)
#   scripts/release/rel.sh gate <version>             GET …/gate
#   scripts/release/rel.sh promote <version>          POST …/promote with the stable manifest
#
# Environment (values never printed):
#   DL_AGENT_RELEASE_SEED       the Ed25519 release seed (base64); or DL_AGENT_RELEASE_SEED_FILE,
#                               default ~/.config/devlibrary-agent-hub/release.seed
#   DL_AGENT_RELEASE_KEY_ID     key id, default rel-2026
#   CLOUDFLARE_API_TOKEN        R2 edit on the hub's account (fetch, upload)
#   AGENT_HUB_ADMIN_TOKEN, AGENT_HUB_ACCESS_CLIENT_ID, AGENT_HUB_ACCESS_CLIENT_SECRET
#                               hub admin (canary, gate, promote); or ~/.config/devlibrary-agent-hub/admin.env
#   REL_TARGETS                 space-separated targets, default all five
#   MMEI_BUILD_DEFAULT_SQL_PASSWORD  sign refuses a binary without it compiled in; or
#                               ~/.config/devlibrary-agent-hub/einvoice-build.env (DL_BUILD_NO_DEFAULT_SQL_PASSWORD=1 skips)
set -euo pipefail
# shellcheck source=scripts/release/lib.sh
. "$(dirname "$0")/lib.sh"
cmd=${1:?usage: rel.sh fetch|sign|upload|canary|gate|promote <version> [channel]}
v=${2:?version}
[[ $v =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "version must be x.y.z"
d=$(dist "$v")

case $cmd in
fetch)
  for t in "${TARGETS[@]}"; do
    f=$(bin_name "$t"); mkdir -p "$d/$t"
    r2 GET "dl-agent-builds/$v/$t/$f" -o "$d/$t/$f" || die "no staged build for $t (run fly-build.sh $v first)"
    echo "$t $(sha256sum "$d/$t/$f" | cut -d' ' -f1)"
  done
  ;;
sign)
  ch=${3:?channel}
  case $ch in canary|stable) ;; *) die "channel must be canary or stable" ;; esac
  t="" u="" b="" bins=()
  for x in "${TARGETS[@]}"; do
    f=$(bin_name "$x"); [ -f "$d/$x/$f" ] || die "missing $d/$x/$f"
    t+="${t:+,}$x"; u+="${u:+,}$HUB/v1/artifacts/dl-agent/$v/$x/$f"; b+="${b:+,}$d/$x/$f"; bins+=("$d/$x/$f")
  done
  check_build_password "${bins[@]}"
  seed=$(mktemp); trap 'rm -f "$seed"' EXIT; chmod 600 "$seed"
  if [ -n "${DL_AGENT_RELEASE_SEED:-}" ]; then printf '%s' "$DL_AGENT_RELEASE_SEED" >"$seed"
  else cat "${DL_AGENT_RELEASE_SEED_FILE:-$CFG/release.seed}" >"$seed"; fi
  (cd "$root" && cargo run -q -p dl-agent --example release -- sign "$seed" "${DL_AGENT_RELEASE_KEY_ID:-rel-2026}" \
    dl-agent "$ch" "$v" "$t" "$u" "$b") >"$d/$ch.json"
  python3 -c "import json,sys;m=json.load(open(sys.argv[1]));print(m['channel'],m['version'],len(m['artifacts']),'artifacts signed')" "$d/$ch.json"
  ;;
upload)
  for t in "${TARGETS[@]}"; do
    f=$(bin_name "$t")
    r2 PUT "dl-agent/$v/$t/$f" --data-binary "@$d/$t/$f" -H 'content-type: application/octet-stream' -o /dev/null
    echo "uploaded dl-agent/$v/$t/$f"
  done
  ;;
canary)
  [ -f "$d/canary.json" ] || die "sign $v canary first"
  admin PUT "/admin/releases/dl-agent/$v" -H 'content-type: application/json' --data-binary "@$d/canary.json" -w ' HTTP %{http_code}\n'
  ;;
gate)
  admin GET "/admin/releases/dl-agent/$v/gate"; echo
  ;;
promote)
  [ -f "$d/stable.json" ] || die "sign $v stable first"
  python3 -c "import json,sys;print(json.dumps({'manifest':open(sys.argv[1]).read()}))" "$d/stable.json" >"$d/promote.json"
  admin POST "/admin/releases/dl-agent/$v/promote" -H 'content-type: application/json' --data-binary "@$d/promote.json" -w ' HTTP %{http_code}\n'
  ;;
*) die "unknown command $cmd" ;;
esac

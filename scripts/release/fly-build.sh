#!/usr/bin/env bash
# Builds dl-agent release binaries in a one-shot Fly Machine, from any machine with the repo
# and curl (no Docker daemon, no flyctl):
#
#   scripts/release/fly-build.sh <version> [targets...]
#
# The machine (auto_destroy, restart "no") downloads this repo at the pushed commit, installs
# the toolchain (scripts/xbuild/setup.sh, skipped when FLY_BUILD_IMAGE already has it), runs
# scripts/build-dl-agent.sh natively and stages the binaries, the log and the exit status in R2
# under dl-agent-builds/<version>/. It exits when done and Fly removes it; this script also
# destroys it on timeout, error or Ctrl-C, so no machine is left running. The release seed
# never leaves this machine: rel.sh fetch + sign run here.
#
# Environment (values never printed):
#   FLY_API_TOKEN         deploy token of FLY_BUILD_APP (create/delete machines)
#   FLY_BUILD_APP         Fly app that hosts build machines, default dl-xbuild
#   FLY_BUILD_REGION      default sin;  FLY_BUILD_SIZE  default performance-8x (cpus) / 16384 MB
#   FLY_BUILD_IMAGE       default rust:1-bookworm (toolchain installed on start, ~3 min)
#   FLY_BUILD_TIMEOUT     seconds, default 5400
#   DL_BUILD_REF          commit to build, default HEAD (must be on GitHub)
#   CLOUDFLARE_API_TOKEN  R2 edit on the hub's account; handed to the machine for staging
#   MMEI_BUILD_DEFAULT_SQL_PASSWORD  compiled into einvoice-enroll (optional, see build-dl-agent.sh)
#   DL_AGENT_RELEASE_KEYS public release keys, default ~/.config/devlibrary-agent-hub/release.pub
#   FLY_BUILD_DRY_RUN=1   print the machine config (secrets masked) and stop
set -euo pipefail
# shellcheck source=scripts/release/lib.sh
. "$(dirname "$0")/lib.sh"
v=${1:?usage: fly-build.sh <version> [targets...]}
shift
[[ $v =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "version must be x.y.z"
[ $# -gt 0 ] && TARGETS=("$@")
app=${FLY_BUILD_APP:-dl-xbuild}
api=https://api.machines.dev/v1/apps/$app/machines
ref=${DL_BUILD_REF:-$(git -C "$root" rev-parse HEAD)}
[[ $ref =~ ^[0-9a-f]{40}$ ]] || die "DL_BUILD_REF must be a full commit sha"
repo=${DL_BUILD_REPO:-$(git -C "$root" remote get-url origin | sed -E 's#^(https://github.com/|git@github.com:|.*/git/)##; s#\.git$##')}
[ -n "${DL_AGENT_RELEASE_KEYS:-}" ] || DL_AGENT_RELEASE_KEYS=$(cat "$CFG/release.pub")
export DL_AGENT_RELEASE_KEYS DL_BUILD_REF=$ref DL_BUILD_REPO=$repo DL_BUILD_VERSION=$v DL_BUILD_TARGETS="${TARGETS[*]}" \
  R2_ACCOUNT_ID R2_BUCKET FLY_BUILD_REGION=${FLY_BUILD_REGION:-sin} FLY_BUILD_IMAGE=${FLY_BUILD_IMAGE:-rust:1-bookworm} \
  FLY_BUILD_SIZE=${FLY_BUILD_SIZE:-performance-8x} FLY_BUILD_MEMORY=${FLY_BUILD_MEMORY:-16384}

config() { # machine config JSON on stdout; $1=mask hides secret values
  python3 - "$root/scripts/release/machine-build.sh" "${1:-}" <<'PY'
import base64, json, os, sys
mask = sys.argv[2] == "mask"
env = {k: os.environ.get(k, "") for k in ("DL_BUILD_REPO", "DL_BUILD_REF", "DL_BUILD_VERSION", "DL_BUILD_TARGETS",
       "DL_AGENT_RELEASE_KEYS", "R2_ACCOUNT_ID", "R2_BUCKET", "CLOUDFLARE_API_TOKEN", "MMEI_BUILD_DEFAULT_SQL_PASSWORD")}
for k in ("CLOUDFLARE_API_TOKEN", "MMEI_BUILD_DEFAULT_SQL_PASSWORD"):
    if mask and env[k]: env[k] = "***"
kind, _, cpus = os.environ["FLY_BUILD_SIZE"].partition("-")
print(json.dumps({
  "name": "xbuild-" + os.environ["DL_BUILD_VERSION"].replace(".", "-") + "-" + os.environ["DL_BUILD_REF"][:8],
  "region": os.environ["FLY_BUILD_REGION"],
  "config": {
    "image": os.environ["FLY_BUILD_IMAGE"],
    "auto_destroy": True,
    "restart": {"policy": "no"},
    "guest": {"cpu_kind": kind, "cpus": int(cpus.rstrip("x")), "memory_mb": int(os.environ["FLY_BUILD_MEMORY"])},
    "init": {"exec": ["/bin/bash", "/build.sh"]},
    "env": env,
    "files": [{"guest_path": "/build.sh", "raw_value": base64.b64encode(open(sys.argv[1], "rb").read()).decode()}],
    "metadata": {"purpose": "dl-agent-xbuild", "version": os.environ["DL_BUILD_VERSION"]},
  },
}, indent=1 if mask else None))
PY
}
if [ -n "${FLY_BUILD_DRY_RUN:-}" ]; then config mask; exit 0; fi
need FLY_API_TOKEN "deploy token of the Fly app $app"
need CLOUDFLARE_API_TOKEN "R2 edit on the hub's account"
git -C "$root" branch -r --contains "$ref" 2>/dev/null | grep -q . || [ -n "${DL_BUILD_REF:-}" ] \
  || die "commit $ref is not on the remote yet: push it first"

stage="dl-agent-builds/$v"
r2 DELETE "$stage/status" -o /dev/null 2>/dev/null || true
id=
cleanup() {
  [ -n "$id" ] || return 0
  bearer "$FLY_API_TOKEN" -o /dev/null -X DELETE "$api/$id?force=true" 2>/dev/null || true
  echo "machine $id destroyed"
}
trap cleanup EXIT INT TERM
resp=$(config | bearer "$FLY_API_TOKEN" -X POST "$api" -H 'content-type: application/json' --data-binary @- -w '\n%{http_code}')
code=${resp##*$'\n'}
[ "$code" = 200 ] || die "machine create failed: HTTP $code ${resp%$'\n'*}"
id=$(python3 -c "import json,sys;print(json.loads(sys.argv[1])['id'])" "${resp%$'\n'*}")
echo "machine $id ($FLY_BUILD_SIZE, $FLY_BUILD_REGION) building $v at ${ref:0:12}: ${TARGETS[*]}"

deadline=$(( $(date +%s) + ${FLY_BUILD_TIMEOUT:-5400} ))
state=
while :; do
  st=$(bearer "$FLY_API_TOKEN" -o - -w '\n%{http_code}' "$api/$id")
  case ${st##*$'\n'} in
    404) state=destroyed ;;
    200) state=$(python3 -c "import json,sys;print(json.loads(sys.argv[1]).get('state',''))" "${st%$'\n'*}") ;;
    *) state=unknown ;;
  esac
  case $state in destroyed|stopped|failed) break ;; esac
  [ "$(date +%s)" -lt "$deadline" ] || { echo "timeout after ${FLY_BUILD_TIMEOUT:-5400}s (state $state)" >&2; break; }
  sleep 20
done
[ "$state" = destroyed ] && id=  # Fly already removed it; nothing to clean up
rc=$(r2 GET "$stage/status" 2>/dev/null || echo missing)
echo "--- build log (tail) ---"
r2 GET "$stage/build.log" 2>/dev/null | tail -n "${FLY_BUILD_LOG_LINES:-40}" || echo "(no log staged)"
[ "$rc" = 0 ] || die "build failed (status $rc)"
echo "build $v staged in R2 $stage/; next: rel.sh fetch $v"

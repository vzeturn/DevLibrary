# shellcheck shell=bash disable=SC2034 # sourced: the variables are used by the callers
# Shared helpers for scripts/release/*.sh (sourced, not run). Secrets come from the
# environment (or the operator's ~/.config files) and never reach argv, a file in the repo or
# the output: curl reads every Authorization header from a process-substitution config.
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
CFG=${DL_AGENT_CONFIG_DIR:-$HOME/.config/devlibrary-agent-hub}
HUB=${DL_AGENT_HUB:-https://agent.pms.id.vn}
R2_BUCKET=${R2_BUCKET:-agent-hub-artifacts}
# The artifacts bucket lives in the account the hub Worker deploys to.
R2_ACCOUNT_ID=${R2_ACCOUNT_ID:-$(sed -n 's/.*"account_id": *"\([0-9a-f]*\)".*/\1/p' "$root/services/agent-hub/wrangler.jsonc" | head -1)}
read -r -a TARGETS <<<"${REL_TARGETS:-linux-x86_64 windows-x86_64 windows-x86 windows7-x86_64 windows7-x86}"

die() { echo "$*" >&2; exit 1; }
need() { [ -n "${!1:-}" ] || die "$1 is not set${2:+ ($2)}"; }
bin_name() { case $1 in linux-*) echo dl-agent ;; *) echo dl-agent.exe ;; esac; }
dist() { echo "$root/dist/dl-agent/$1"; }

# bearer <token> <curl args...>
bearer() { local t=$1; shift; curl -sS -K <(printf 'header = "Authorization: Bearer %s"\n' "$t") "$@"; }

# r2 <method> <key> [curl args...]: the Cloudflare R2 object API with CLOUDFLARE_API_TOKEN.
r2() {
  need CLOUDFLARE_API_TOKEN "an API token with R2 edit on the hub's account"
  local m=$1 k=$2; shift 2
  bearer "$CLOUDFLARE_API_TOKEN" -f -X "$m" \
    "https://api.cloudflare.com/client/v4/accounts/$R2_ACCOUNT_ID/r2/buckets/$R2_BUCKET/objects/$k" "$@"
}

# admin <method> <path> [curl args...]: the hub admin API behind Cloudflare Access.
admin() {
  if [ -z "${AGENT_HUB_ADMIN_TOKEN:-}" ] && [ -f "$CFG/admin.env" ]; then set -a; . "$CFG/admin.env"; set +a; fi
  need AGENT_HUB_ADMIN_TOKEN; need AGENT_HUB_ACCESS_CLIENT_ID; need AGENT_HUB_ACCESS_CLIENT_SECRET
  local m=$1 p=$2; shift 2
  curl -sS -X "$m" "$HUB$p" -K <(printf 'header = "Authorization: Bearer %s"\nheader = "CF-Access-Client-Id: %s"\nheader = "CF-Access-Client-Secret: %s"\n' \
    "$AGENT_HUB_ADMIN_TOKEN" "$AGENT_HUB_ACCESS_CLIENT_ID" "$AGENT_HUB_ACCESS_CLIENT_SECRET") "$@"
}

# The SMILE default SQL login password compiled into every release build (MMEI_BUILD_DEFAULT_SQL_PASSWORD,
# read with option_env!): pda-sync's grant self-repair and pda-enroll try it as `sql:smile`, and
# einvoice-enroll needs it to run without a prompt. build-dl-agent.sh only warns when it is unset,
# and 1.3.63, 1.3.64 and 1.3.66 shipped without it: Muine Bay could not apply the 1.3.66 grants.
# need_build_password loads it from $CFG/einvoice-build.env when the environment has none and
# exports it; DL_BUILD_NO_DEFAULT_SQL_PASSWORD=1 releases without it on purpose.
need_build_password() {
  if [ -z "${MMEI_BUILD_DEFAULT_SQL_PASSWORD:-}" ] && [ -f "$CFG/einvoice-build.env" ]; then
    set -a
    # shellcheck disable=SC1091 # operator file, outside the repo
    . "$CFG/einvoice-build.env"
    set +a
  fi
  if [ -n "${MMEI_BUILD_DEFAULT_SQL_PASSWORD:-}" ]; then export MMEI_BUILD_DEFAULT_SQL_PASSWORD; return 0; fi
  if [ "${DL_BUILD_NO_DEFAULT_SQL_PASSWORD:-}" = 1 ]; then
    echo "warning: releasing without the default SQL password (DL_BUILD_NO_DEFAULT_SQL_PASSWORD=1): pda-sync cannot apply new grants as sql:smile" >&2
    return 0
  fi
  die "MMEI_BUILD_DEFAULT_SQL_PASSWORD is not set and $CFG/einvoice-build.env has none: a release without it cannot apply pda_sync grants as the SMILE login. Set it, or DL_BUILD_NO_DEFAULT_SQL_PASSWORD=1 to release without it on purpose."
}

# check_build_password <binary...>: every binary has the default SQL password compiled in. The
# password reaches grep through a process-substitution pattern file, never argv or the output.
check_build_password() {
  need_build_password
  [ -n "${MMEI_BUILD_DEFAULT_SQL_PASSWORD:-}" ] || return 0
  local f
  for f in "$@"; do
    grep -a -F -q -f <(printf '%s\n' "$MMEI_BUILD_DEFAULT_SQL_PASSWORD") -- "$f" \
      || die "$f was built without MMEI_BUILD_DEFAULT_SQL_PASSWORD: build again with it (fly-build.sh loads it from $CFG/einvoice-build.env)"
  done
}

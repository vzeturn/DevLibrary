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

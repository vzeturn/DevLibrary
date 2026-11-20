#!/usr/bin/env bash
# Runs inside the one-shot Fly build machine that fly-build.sh starts (image rust:1-bookworm
# or a prebuilt xbuild image). It never sees the release seed: it builds, then stages the
# unsigned binaries and the build log in R2 under dl-agent-builds/<version>/, where
# rel.sh fetch picks them up for signing. The process exits either way and the machine,
# created with auto_destroy, is removed by Fly.
#
# Environment (set by fly-build.sh): DL_BUILD_REPO, DL_BUILD_REF, DL_BUILD_VERSION,
# DL_BUILD_TARGETS, DL_AGENT_RELEASE_KEYS (public), MMEI_BUILD_DEFAULT_SQL_PASSWORD,
# CLOUDFLARE_API_TOKEN, R2_ACCOUNT_ID, R2_BUCKET.
set -uo pipefail
v=$DL_BUILD_VERSION
log=/tmp/build.log
stage="dl-agent-builds/$v"
put() { # put <key> <file>
  curl -sS -f -X PUT --data-binary "@$2" -H 'content-type: application/octet-stream' -o /dev/null \
    -K <(printf 'header = "Authorization: Bearer %s"\n' "$CLOUDFLARE_API_TOKEN") \
    "https://api.cloudflare.com/client/v4/accounts/$R2_ACCOUNT_ID/r2/buckets/$R2_BUCKET/objects/$1"
}
build() {
  set -euo pipefail
  echo "build $v from $DL_BUILD_REPO@$DL_BUILD_REF targets: $DL_BUILD_TARGETS ($(nproc) cpu)"
  mkdir -p /src
  curl -fsSL "https://codeload.github.com/$DL_BUILD_REPO/tar.gz/$DL_BUILD_REF" | tar -xz -C /src --strip-components=1
  if ! command -v x86_64-w64-mingw32-gcc >/dev/null; then /src/scripts/xbuild/setup.sh >/tmp/setup.log 2>&1 || { tail -40 /tmp/setup.log; exit 1; }; fi
  cd /src
  # shellcheck disable=SC2086 # target list is word-split on purpose
  DL_XBUILD_NATIVE=1 scripts/build-dl-agent.sh "$v" "dist/dl-agent/$v" $DL_BUILD_TARGETS
  for t in $DL_BUILD_TARGETS; do
    f=dl-agent.exe; case $t in linux-*) f=dl-agent ;; esac
    put "$stage/$t/$f" "dist/dl-agent/$v/$t/$f"
    echo "staged $stage/$t/$f"
  done
}
start=$(date +%s)
( build ) 2>&1 | tee "$log"
rc=${PIPESTATUS[0]}
echo "exit $rc after $(( $(date +%s) - start ))s" | tee -a "$log"
echo "$rc" >/tmp/status
put "$stage/build.log" "$log" || echo "could not stage the log" >&2
put "$stage/status" /tmp/status || echo "could not stage the status" >&2
exit "$rc"

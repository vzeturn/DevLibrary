#!/usr/bin/env bash
# Client copy (RT-1107): a one-shot Windows EC2 machine (win-run: created, used, terminated)
# with an older SQL Server Express and a sample SMILE database. It installs the old dl-agent,
# upgrades it to the build about to be released, runs `dl-agent selftest` after each step and
# uninstalls it. No real data, and no smile/smilepms password is created or changed (Windows
# login only). The machine is destroyed by win-run whatever happens.
#
#   scripts/release/client-copy.sh <old-version> <new-target> [out-dir]
#
# <new-target> is canary (the usual pre-release gate), stable or a version. Needs win-run, the
# AWS keys of the cloud kit and the hub admin credentials of lib.sh. Exit 0 only if every step
# passed; the per-step result is out-dir/client-copy.json.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"
old=${1:?usage: client-copy.sh <old-version> <canary|stable|new-version> [out-dir]}
new=${2:?usage: client-copy.sh <old-version> <canary|stable|new-version> [out-dir]}
out=${3:-$root/win-run-out/client-copy-$(date +%Y%m%d-%H%M%S)}
# An older SQL Server Express (2017 by default); override with DL_CC_SQL_LINK.
sqllink=${DL_CC_SQL_LINK:-https://go.microsoft.com/fwlink/?linkid=853017}
tenant=client-copy
[[ $old =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] || die "old version '$old' is not a version"
[[ $new =~ ^(canary|stable|[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?)$ ]] || die "new target '$new' is not canary, stable or a version"
command -v win-run >/dev/null || die "win-run is not on PATH"

admin PUT "/admin/tenants/$tenant" -H 'content-type: application/json' \
  -d '{"name":"Client copy (disposable test machines)"}' -f >/dev/null || die "could not create the $tenant tenant on the hub"
resp=$(admin POST /admin/enroll-codes -H 'content-type: application/json' -f \
  -d "{\"tenant_id\":\"$tenant\",\"site_id\":\"cc-$(date +%s)\",\"ttl_seconds\":3600}") || die "could not mint an enrolment code"
code=$(printf '%s' "$resp" | sed -n 's/.*"code" *: *"\([^"]*\)".*/\1/p' | head -1)
[ -n "$code" ] || die "the hub answered without a code"

work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
chmod 700 "$work"
# The one-time code (1 h, one machine) is the only value written into the script that goes up.
sed -e "s|@OLD@|$old|g" -e "s|@NEW@|$new|g" -e "s|@CODE@|$code|g" -e "s|@HUB@|$HUB|g" -e "s|@SQLLINK@|$sqllink|g" \
  "$here/client-copy/verify.ps1" >"$work/verify.ps1"
mkdir -p "$out"
rc=0
win-run --repo "$root" --dotnet none --label rt-1107-cc --timeout-min 60 --out "$out" --cmd-file "$work/verify.ps1" || rc=$?
unset code resp
[ "$rc" -eq 0 ] || die "win-run exit $rc (the machine or its bucket may need a manual check)"
echo "win-run exit $rc; results in $out"
[ -f "$out/client-copy.json" ] || [ -n "$(find "$out" -name client-copy.json -print -quit)" ] || die "no client-copy.json came back"
res=$(find "$out" -name client-copy.json -print -quit)
if grep -q '"ok": *false' "$res"; then echo "client copy FAILED:"; grep -B1 -A2 '"ok": *false' "$res"; exit 1; fi
n=$(grep -c '"ok": *true' "$res")
[ "$n" -ge 8 ] || die "only $n steps passed; expected 8"
echo "client copy passed: $n steps"

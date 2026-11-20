#!/usr/bin/env bash
# One command from a cloud session (or any machine with the repo, curl and cargo; no Docker):
# build dl-agent <version> in a one-shot Fly Machine, sign it here for canary and stable,
# upload the artifacts and publish the canary manifest on the hub.
#
#   scripts/release/canary.sh <version> [targets...]
#
# Needs the environment of fly-build.sh and rel.sh (FLY_API_TOKEN, CLOUDFLARE_API_TOKEN, the
# release seed, the hub admin credentials). Afterwards: rel.sh gate <version>, then
# rel.sh promote <version> once the canary gate passes.
set -euo pipefail
here=$(dirname "$0")
v=${1:?usage: canary.sh <version> [targets...]}
shift
if [ $# -gt 0 ]; then export REL_TARGETS="$*"; fi
# shellcheck disable=SC2086 # REL_TARGETS is word-split on purpose
"$here/fly-build.sh" "$v" ${REL_TARGETS:-}
"$here/rel.sh" fetch "$v"
"$here/rel.sh" sign "$v" canary
"$here/rel.sh" sign "$v" stable
"$here/rel.sh" upload "$v"
"$here/rel.sh" canary "$v"
echo "dl-agent $v is on canary. Check: scripts/release/rel.sh gate $v"

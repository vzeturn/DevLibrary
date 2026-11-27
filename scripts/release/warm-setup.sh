#!/usr/bin/env bash
# One-time (and after a toolchain change) setup of the warm dl-agent build machine (RT-1340):
#
#   scripts/release/warm-setup.sh
#
# Builds the toolchain image (scripts/xbuild/Dockerfile: rust + mingw + wine + rustup targets,
# the ~3 min that a one-shot machine paid on every release) on Fly's remote builder, pushes it
# to registry.fly.io/<app>:<label> (label = hash of Dockerfile + setup.sh), makes a volume for
# the cargo registry/target cache and creates ONE stopped machine, dl-xbuild-warm, that mounts
# it at /cache. fly-build.sh then only starts it per release and stops it again, so an idle
# machine costs the volume only. Re-running it with a changed toolchain pushes a new image and
# points the machine at it; the volume (and its cache) is kept.
#
# Environment: FLY_API_TOKEN (deploy token of FLY_BUILD_APP), FLY_BUILD_APP (default dl-xbuild),
# FLY_BUILD_REGION (default sin), FLY_BUILD_VOLUME_GB (default 30), flyctl on PATH.
set -euo pipefail
# shellcheck source=scripts/release/lib.sh
. "$(dirname "$0")/lib.sh"
need FLY_API_TOKEN "deploy token of the Fly app"
app=${FLY_BUILD_APP:-dl-xbuild}
region=${FLY_BUILD_REGION:-sin}
api=https://api.machines.dev/v1/apps/$app
label=$(cat "$root/scripts/xbuild/Dockerfile" "$root/scripts/xbuild/setup.sh" | shasum -a 256 | cut -c1-12)
image=registry.fly.io/$app:tc-$label

tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
cp "$root/scripts/xbuild/Dockerfile" "$root/scripts/xbuild/setup.sh" "$tmp/"
printf 'app = "%s"\n[build]\n  dockerfile = "Dockerfile"\n' "$app" >"$tmp/fly.toml"
echo "building $image"
(cd "$tmp" && flyctl deploy --build-only --push --remote-only -a "$app" -c fly.toml \
  --image-label "tc-$label" >"$tmp/build.log" 2>&1) || { tail -30 "$tmp/build.log" >&2; die "image build failed"; }
# The Machines API does not resolve the bare tag of a --build-only push; the digest it prints does.
digest=$(grep -o "$app:tc-$label@sha256:[0-9a-f]\{64\}" "$tmp/build.log" | head -1 | sed 's/.*@//')
[ -n "$digest" ] || die "no image digest in the build output"
image=$image@$digest

machines=$(bearer "$FLY_API_TOKEN" -f "$api/machines")
id=$(python3 -c "import json,sys;m=[x for x in json.loads(sys.argv[1]) if x['name']=='dl-xbuild-warm'];print(m[0]['id'] if m else '')" "$machines")
vols=$(bearer "$FLY_API_TOKEN" -f "$api/volumes")
vol=$(python3 -c "import json,sys;v=[x for x in json.loads(sys.argv[1]) if x['name']=='dlxcache' and x['state']!='destroyed'];print(v[0]['id'] if v else '')" "$vols")
if [ -z "$vol" ]; then
  vol=$(bearer "$FLY_API_TOKEN" -f -X POST "$api/volumes" -H 'content-type: application/json' \
    -d "{\"name\":\"dlxcache\",\"region\":\"$region\",\"size_gb\":${FLY_BUILD_VOLUME_GB:-30}}" \
    | python3 -c "import json,sys;print(json.load(sys.stdin)['id'])")
  echo "volume $vol created"
fi
cfg=$(python3 - "$image" "$vol" "$region" <<'PY'
import json, sys
image, vol, region = sys.argv[1:4]
print(json.dumps({"name": "dl-xbuild-warm", "region": region, "skip_launch": True, "config": {
  "image": image, "restart": {"policy": "no"},
  "guest": {"cpu_kind": "performance", "cpus": 8, "memory_mb": 16384},
  "mounts": [{"volume": vol, "path": "/cache"}],
  "init": {"exec": ["/bin/bash", "-c", "echo warm machine idle"]},
  "metadata": {"purpose": "dl-agent-xbuild-warm", "toolchain": image}}}))
PY
)
if [ -z "$id" ]; then
  printf '%s' "$cfg" | bearer "$FLY_API_TOKEN" -f -X POST "$api/machines" -H 'content-type: application/json' --data-binary @- >/dev/null
  echo "machine dl-xbuild-warm created (stopped) on $image"
else
  python3 -c "import json,sys;c=json.loads(sys.argv[1]);print(json.dumps({'config':c['config'],'skip_launch':True}))" "$cfg" \
    | bearer "$FLY_API_TOKEN" -f -X POST "$api/machines/$id" -H 'content-type: application/json' --data-binary @- >/dev/null
  echo "machine $id now on $image (cache volume kept)"
fi

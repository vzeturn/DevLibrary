#!/usr/bin/env bash
# Builds dl-agent release binaries for every site target in a container.
#
#   scripts/build-dl-agent.sh <version> [out-dir] [targets...]
#
# Targets (release target name -> rust triple):
#   linux-x86_64     x86_64-unknown-linux-gnu
#   windows-x86_64   x86_64-pc-windows-gnu      Windows 10 / Server 2016+
#   windows-x86      i686-pc-windows-gnu        32-bit Windows 10 (x86-only SDKs)
#   windows7-x86_64  x86_64-win7-windows-gnu    Windows 7 / Server 2008 R2 / 2012 (nightly build-std)
#   windows7-x86     i686-win7-windows-gnu      32-bit Windows 7
# DL_AGENT_RELEASE_KEYS defaults to ~/.config/devlibrary-agent-hub/release.pub.
# Windows binaries are linked statically and may import only system DLLs (checked);
# the Win7 x64 build is smoke-tested under wine.
set -euo pipefail
version=${1:?usage: build-dl-agent.sh <version> [out-dir] [targets...]}
out=${2:-dist/dl-agent/$version}
shift $(( $# >= 2 ? 2 : 1 ))
targets=("$@")
[ ${#targets[@]} -eq 0 ] && targets=(linux-x86_64 windows-x86_64 windows-x86 windows7-x86_64 windows7-x86)
root=$(cd "$(dirname "$0")/.." && pwd)
keys=${DL_AGENT_RELEASE_KEYS:-$(cat "$HOME/.config/devlibrary-agent-hub/release.pub")}
image=dl-agent-xbuild:local
# The zero-prompt `einvoice-enroll` default SQL password is compiled in from this variable.
# Passed by name only, so docker takes the host value and nothing is echoed.
if [ -z "${MMEI_BUILD_DEFAULT_SQL_PASSWORD:-}" ]; then
  echo "warning: MMEI_BUILD_DEFAULT_SQL_PASSWORD is unset; einvoice-enroll then needs --sql-password (or --sql-prompt)" >&2
fi
docker image inspect "$image" >/dev/null 2>&1 || docker build -q -t "$image" "$root/scripts/xbuild" >/dev/null
mkdir -p "$root/$out"
for t in "${targets[@]}"; do
  case $t in
    linux-x86_64) triple=x86_64-unknown-linux-gnu; tc=+stable; std=; exe= ;;
    windows-x86_64) triple=x86_64-pc-windows-gnu; tc=+stable; std=; exe=.exe ;;
    windows-x86) triple=i686-pc-windows-gnu; tc=+stable; std=; exe=.exe ;;
    windows7-x86_64) triple=x86_64-win7-windows-gnu; tc=+nightly; std="-Zbuild-std=std,panic_abort"; exe=.exe ;;
    windows7-x86) triple=i686-win7-windows-gnu; tc=+nightly; std="-Zbuild-std=std,panic_abort"; exe=.exe ;;
    *) echo "unknown target $t" >&2; exit 2 ;;
  esac
  docker run --rm -v "$root:/src" -e DL_AGENT_VERSION="$version" -e DL_AGENT_RELEASE_KEYS="$keys" \
    -e MMEI_BUILD_DEFAULT_SQL_PASSWORD \
    -e CARGO_TARGET_DIR=/src/target/xbuild -e RUSTFLAGS="-C target-feature=+crt-static" "$image" \
    cargo $tc build -q --release -p dl-agent-bin --bin dl-agent --target "$triple" $std
  mkdir -p "$root/$out/$t"
  cp "$root/target/xbuild/$triple/release/dl-agent$exe" "$root/$out/$t/dl-agent$exe"
  if [ -n "$exe" ]; then
    # Only Windows system DLLs may be imported (no MinGW runtime DLLs to ship), and the
    # Win7 builds must not need bcryptprimitives.dll (Windows 8+).
    dlls=$(docker run --rm -v "$root/$out/$t:/b" "$image" x86_64-w64-mingw32-objdump -p /b/dl-agent.exe \
      | awk '/DLL Name:/ {print tolower($3)}' | sort -u | tr '\n' ' ')
    for d in $dlls; do
      case $d in kernel32.dll|advapi32.dll|ws2_32.dll|ntdll.dll|userenv.dll|secur32.dll|crypt32.dll|bcrypt.dll|api-ms-win-core-synch-l1-2-0.dll|msvcrt.dll|shell32.dll|ole32.dll|oleaut32.dll|user32.dll|iphlpapi.dll|netapi32.dll|version.dll|wtsapi32.dll) ;;
        bcryptprimitives.dll) case $t in windows7-*) echo "$t imports $d (Windows 8+)" >&2; exit 1 ;; esac ;;
        *) echo "$t imports non-system DLL $d" >&2; exit 1 ;;
      esac
    done
    echo "$t imports: $dlls"
  fi
  if [ "$t" = windows7-x86_64 ]; then
    # wine 8 lacks bcryptprimitives.dll, so only the Win7 build can run under it.
    got=$(docker run --rm -v "$root/$out/$t:/b" -e WINEDEBUG=-all "$image" /usr/lib/wine/wine64 /b/dl-agent.exe --version 2>/dev/null | tr -d '\r' || true)
    case $got in *"$version"*) echo "$t wine smoke: $got" ;; *) echo "wine smoke failed for $t: $got" >&2; exit 1 ;; esac
  fi
  echo "$t $(shasum -a 256 "$root/$out/$t/dl-agent$exe" | cut -d' ' -f1)"
done

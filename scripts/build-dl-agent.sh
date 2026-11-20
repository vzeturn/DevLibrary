#!/usr/bin/env bash
# Builds dl-agent release binaries for every site target in a container.
#
#   scripts/build-dl-agent.sh [--fault probation] <version> [out-dir] [targets...]
#
# --fault probation builds the rollback smoke test (crates/dl-agent/PROTOCOL.md §6.6): a
# binary that never passes probation, so every agent that takes it rolls back and reports
# `update_rolled_back`. Off unless given here; DL_AGENT_FAULT from the environment is ignored.
# The output goes to dist/dl-agent/<version>-fault-<fault> unless out-dir is given.
#
# Targets (release target name -> rust triple):
#   linux-x86_64     x86_64-unknown-linux-gnu
#   windows-x86_64   x86_64-pc-windows-gnu      Windows 10 / Server 2016+
#   windows-x86      i686-pc-windows-gnu        32-bit Windows 10 (x86-only SDKs)
#   windows7-x86_64  x86_64-win7-windows-gnu    Windows 7 / Server 2008 R2 / 2012 (nightly build-std)
#   windows7-x86     i686-win7-windows-gnu      32-bit Windows 7
# DL_AGENT_RELEASE_KEYS defaults to ~/.config/devlibrary-agent-hub/release.pub.
# DL_XBUILD_NATIVE=1 builds on this host instead of in the image: the host must have run
# scripts/xbuild/setup.sh (the one-shot Fly build machine does, see scripts/release/).
# Windows binaries are linked statically and may import only system DLLs (checked);
# the Win7 x64 build is smoke-tested under wine.
# The Linux binary links glibc dynamically (needs glibc >= 2.36, Debian 12 / Ubuntu 23.04+):
# dl-tls loads the system libssl with dlopen, and a static glibc next to the libc that
# libssl pulls in crashed in OpenSSL when two threads opened TLS at once (1.2.0 canary).
set -euo pipefail
fault=
if [ "${1:-}" = --fault ]; then
  case ${2:-} in
    probation) fault=probation ;;
    *) echo "unknown fault '${2:-}' (only: probation)" >&2; exit 2 ;;
  esac
  shift 2
  echo "warning: FAULT BUILD ($fault): never publish it to stable or to a customer site" >&2
fi
version=${1:?usage: build-dl-agent.sh [--fault probation] <version> [out-dir] [targets...]}
out=${2:-dist/dl-agent/$version${fault:+-fault-$fault}}
shift $(( $# >= 2 ? 2 : 1 ))
targets=("$@")
[ ${#targets[@]} -eq 0 ] && targets=(linux-x86_64 windows-x86_64 windows-x86 windows7-x86_64 windows7-x86)
root=$(cd "$(dirname "$0")/.." && pwd)
keys=${DL_AGENT_RELEASE_KEYS:-$(cat "$HOME/.config/devlibrary-agent-hub/release.pub")}
image=dl-agent-xbuild:local
native=${DL_XBUILD_NATIVE:-}
# The zero-prompt `einvoice-enroll` default SQL password is compiled in from this variable.
# Passed by name only, so docker takes the host value and nothing is echoed.
if [ -z "${MMEI_BUILD_DEFAULT_SQL_PASSWORD:-}" ]; then
  echo "warning: MMEI_BUILD_DEFAULT_SQL_PASSWORD is unset; einvoice-enroll then needs --sql-password (or --sql-prompt)" >&2
fi
if [ -n "$native" ]; then
  export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
    CARGO_TARGET_I686_PC_WINDOWS_GNU_LINKER=i686-w64-mingw32-gcc \
    CARGO_TARGET_X86_64_WIN7_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
    CARGO_TARGET_I686_WIN7_WINDOWS_GNU_LINKER=i686-w64-mingw32-gcc
else
  docker image inspect "$image" >/dev/null 2>&1 || docker build -q -t "$image" "$root/scripts/xbuild" >/dev/null
fi
# on_bin <cmd...>: runs a check on the built binary, whose folder is $bdir.
on_bin() {
  if [ -n "$native" ]; then WINEDEBUG=-all "$@"; else docker run --rm -v "$root/$out/$t:/b" -e WINEDEBUG=-all "$image" "$@"; fi
}
sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi; }
mkdir -p "$root/$out"
for t in "${targets[@]}"; do
  case $t in
    linux-x86_64) triple=x86_64-unknown-linux-gnu; tc=+stable; std=; exe=; rf= ;;
    windows-x86_64) triple=x86_64-pc-windows-gnu; tc=+stable; std=; exe=.exe; rf="-C target-feature=+crt-static" ;;
    windows-x86) triple=i686-pc-windows-gnu; tc=+stable; std=; exe=.exe; rf="-C target-feature=+crt-static" ;;
    windows7-x86_64) triple=x86_64-win7-windows-gnu; tc=+nightly; std="-Zbuild-std=std,panic_abort"; exe=.exe; rf="-C target-feature=+crt-static" ;;
    windows7-x86) triple=i686-win7-windows-gnu; tc=+nightly; std="-Zbuild-std=std,panic_abort"; exe=.exe; rf="-C target-feature=+crt-static" ;;
    *) echo "unknown target $t" >&2; exit 2 ;;
  esac
  if [ -n "$native" ]; then
    (cd "$root" && env DL_AGENT_VERSION="$version" DL_AGENT_RELEASE_KEYS="$keys" ${fault:+DL_AGENT_FAULT=$fault} \
      CARGO_TARGET_DIR="$root/target/xbuild" RUSTFLAGS="$rf" \
      cargo $tc build -q --release -p dl-agent-bin --bin dl-agent --target "$triple" $std)
    bdir=$root/$out/$t
  else
    docker run --rm -v "$root:/src" -e DL_AGENT_VERSION="$version" -e DL_AGENT_RELEASE_KEYS="$keys" \
      -e MMEI_BUILD_DEFAULT_SQL_PASSWORD ${fault:+-e DL_AGENT_FAULT=$fault} \
      -e CARGO_TARGET_DIR=/src/target/xbuild -e RUSTFLAGS="$rf" "$image" \
      cargo $tc build -q --release -p dl-agent-bin --bin dl-agent --target "$triple" $std
    bdir=/b
  fi
  mkdir -p "$root/$out/$t"
  cp "$root/target/xbuild/$triple/release/dl-agent$exe" "$root/$out/$t/dl-agent$exe"
  if [ -n "$exe" ]; then
    # Only Windows system DLLs may be imported (no MinGW runtime DLLs to ship; winspool.drv
    # is the print spooler the pms-device module prints through, present since NT), and the
    # Win7 builds must not need bcryptprimitives.dll (Windows 8+).
    dlls=$(on_bin x86_64-w64-mingw32-objdump -p "$bdir/dl-agent.exe" \
      | awk '/DLL Name:/ {print tolower($3)}' | sort -u | tr '\n' ' ')
    for d in $dlls; do
      case $d in kernel32.dll|advapi32.dll|ws2_32.dll|ntdll.dll|userenv.dll|secur32.dll|crypt32.dll|bcrypt.dll|api-ms-win-core-synch-l1-2-0.dll|msvcrt.dll|shell32.dll|ole32.dll|oleaut32.dll|user32.dll|iphlpapi.dll|netapi32.dll|version.dll|wtsapi32.dll|winspool.drv) ;;
        bcryptprimitives.dll) case $t in windows7-*) echo "$t imports $d (Windows 8+)" >&2; exit 1 ;; esac ;;
        *) echo "$t imports non-system DLL $d" >&2; exit 1 ;;
      esac
    done
    echo "$t imports: $dlls"
  fi
  if [ -z "$exe" ]; then
    # Must stay dynamically linked (see above) and start in the build image.
    on_bin readelf -l "$bdir/dl-agent" | grep -q 'program interpreter' \
      || { echo "$t is statically linked; dl-tls needs the dynamic glibc" >&2; exit 1; }
    got=$(on_bin "$bdir/dl-agent" --version 2>&1 || true)
    case $got in *"$version"*) echo "$t smoke: $got" ;; *) echo "smoke failed for $t: $got" >&2; exit 1 ;; esac
  fi
  if [ "$t" = windows7-x86_64 ]; then
    # wine 8 lacks bcryptprimitives.dll, so only the Win7 build can run under it.
    got=$(on_bin /usr/lib/wine/wine64 "$bdir/dl-agent.exe" --version 2>/dev/null | tr -d '\r' || true)
    case $got in *"$version"*) echo "$t wine smoke: $got" ;; *) echo "wine smoke failed for $t: $got" >&2; exit 1 ;; esac
  fi
  echo "$t $(sha256 "$root/$out/$t/dl-agent$exe" | cut -d' ' -f1)"
done

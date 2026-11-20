#!/usr/bin/env bash
# Installs the dl-agent cross-build toolchain on a Debian 12 host that already has rustup
# (the rust:1-bookworm image). Shared by the Dockerfile here and by the one-shot Fly build
# machine (scripts/release/machine-build.sh), so both build with the same tools.
# Stable targets: x86_64/i686-pc-windows-gnu (Windows 10 / Server 2016+), x86_64-unknown-linux-gnu.
# The repo pins `stable` in rust-toolchain.toml, so that toolchain carries the targets.
# Win7 / Server 2008 R2 targets are tier 3: built with nightly + build-std.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -q
apt-get install -y -q --no-install-recommends mingw-w64 wine64
rm -rf /var/lib/apt/lists/*
rustup toolchain install stable --profile minimal \
  --target x86_64-pc-windows-gnu,i686-pc-windows-gnu,wasm32-unknown-unknown --component rustfmt,clippy
rustup toolchain install nightly --profile minimal --component rust-src

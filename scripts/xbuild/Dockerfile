# Cross-build image for dl-agent release binaries (Linux host -> Windows/Linux targets).
# Stable targets: x86_64/i686-pc-windows-gnu (Windows 10 / Server 2016+), x86_64-unknown-linux-gnu.
# The repo pins `stable` in rust-toolchain.toml, so that toolchain carries the targets.
# Win7 / Server 2008 R2 targets are tier 3: built with nightly + build-std.
FROM rust:1-bookworm
RUN apt-get update \
 && apt-get install -y --no-install-recommends mingw-w64 wine64 \
 && rm -rf /var/lib/apt/lists/* \
 && rustup toolchain install stable --profile minimal \
      --target x86_64-pc-windows-gnu,i686-pc-windows-gnu,wasm32-unknown-unknown --component rustfmt,clippy \
 && rustup toolchain install nightly --profile minimal --component rust-src
ENV CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
    CARGO_TARGET_I686_PC_WINDOWS_GNU_LINKER=i686-w64-mingw32-gcc \
    CARGO_TARGET_X86_64_WIN7_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
    CARGO_TARGET_I686_WIN7_WINDOWS_GNU_LINKER=i686-w64-mingw32-gcc
WORKDIR /src

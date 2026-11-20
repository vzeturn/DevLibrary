# Cross-build image for dl-agent release binaries (Linux host -> Windows/Linux targets).
# The toolchain comes from setup.sh, which the one-shot Fly build machine runs too.
FROM rust:1-bookworm
COPY setup.sh /usr/local/bin/xbuild-setup.sh
RUN /usr/local/bin/xbuild-setup.sh
ENV CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
    CARGO_TARGET_I686_PC_WINDOWS_GNU_LINKER=i686-w64-mingw32-gcc \
    CARGO_TARGET_X86_64_WIN7_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
    CARGO_TARGET_I686_WIN7_WINDOWS_GNU_LINKER=i686-w64-mingw32-gcc
WORKDIR /src

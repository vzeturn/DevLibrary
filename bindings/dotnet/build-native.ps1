# Builds dl-ffi for the runtimes this machine can produce and copies the libraries to
# bindings/dotnet/runtimes/<rid>/native. CI builds every RID (see .github/workflows/release.yml).
#   win-x64    cargo (MSVC)
#   linux-x64  Docker rust:1-bookworm (glibc 2.36, loads on Debian 12+, Ubuntu 24.04, .NET images)
param([switch]$SkipLinux)
$ErrorActionPreference = "Stop"
$root = Resolve-Path "$PSScriptRoot\..\.."
$rt = "$PSScriptRoot\runtimes"
Push-Location $root
try {
    cargo build -p dl-ffi --profile min-size
    New-Item -ItemType Directory -Force "$rt\win-x64\native" | Out-Null
    Copy-Item target\min-size\dl_ffi.dll "$rt\win-x64\native\dl_ffi.dll" -Force
    if (-not $SkipLinux) {
        docker run --rm -v "${root}:/w" -w /w -e CARGO_TARGET_DIR=/w/target/linux rust:1-bookworm `
            cargo build -p dl-ffi --profile min-size
        New-Item -ItemType Directory -Force "$rt\linux-x64\native" | Out-Null
        Copy-Item target\linux\min-size\libdl_ffi.so "$rt\linux-x64\native\libdl_ffi.so" -Force
    }
    Get-ChildItem $rt -Recurse -File | ForEach-Object { "{0}  {1:N0} bytes" -f $_.FullName.Substring($rt.Length + 1), $_.Length }
}
finally { Pop-Location }

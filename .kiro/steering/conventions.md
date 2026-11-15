# DevLibrary conventions

DevLibrary is a Rust monorepo of zero-dependency libraries that replace the third-party
packages used across the vzeturn GitHub projects (C#, TypeScript, Go, Python). Targets:
Linux servers and the web (wasm32-unknown-unknown). Development happens on macOS and Windows.

## Hard rules
- No third-party crates, not even as dev-dependencies. Only `core`, `alloc`, `std` and other
  `dl-*` crates from `[workspace.dependencies]`.
- Every crate lives in `crates/dl-<name>/` and inherits `version`, `edition`, `rust-version`,
  `authors`, `publish` from the workspace, and has `[lints] workspace = true`.
- Pure computation crates are `#![no_std]` with `extern crate alloc;`. Tests may use std
  (`#[cfg(test)] extern crate std;`). Crates that do I/O (log, cli, rand OS entropy) use std.
- Every crate must build for `wasm32-unknown-unknown`. Platform code goes behind `cfg`.
- `unsafe` is denied workspace-wide. Allow it only on the smallest item that needs it, with a
  `// SAFETY:` comment explaining why it is sound.
- No panics on untrusted input. Parsers return `Result` with a typed error that carries
  position information when relevant. `unwrap()` is allowed only in tests and in code where
  the invariant is proven locally (leave a comment).
- Public items have rustdoc (`missing_docs` is on). Docs and code are in English.

## Quality bar
- Unit tests next to the code, with official test vectors (RFCs, NIST, etc.) where they exist.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all --check` pass.
- Benchmarks use the in-house `dl-bench` crate (`[[bench]] harness = false`).

## API style
- Small, explicit APIs. Prefer free functions and plain structs over builders unless the
  number of options makes a builder clearer.
- Errors: one `Error` enum per crate, `impl core::fmt::Display` and `core::error::Error`.
- Names follow Rust API guidelines (`to_*`, `as_*`, `into_*`, `try_from`, `FromStr`).

## Commands
- Test: `cargo test --workspace`
- Lint: `cargo check-all`
- Web build: `cargo build --workspace --exclude dl-bench --target wasm32-unknown-unknown`
- Linux test from Windows/macOS: `docker run --rm -v ${PWD}:/w -w /w rust:latest cargo test --workspace`

# Contributing to DevLibrary

Thank you for helping. Bug reports, test vectors, documentation fixes and new crates are
all welcome. Bug reports and questions can be written in English or Vietnamese.

## Reporting bugs

Open an [issue](https://github.com/vzeturn/devlibrary/issues) with:

- the crate and version (or commit),
- the smallest input that shows the problem,
- what you expected and what happened, and, if possible, what another implementation
  (for example .NET, Node.js or Python) returns for the same input.

Security problems: see [SECURITY.md](SECURITY.md) instead of opening a public issue.

## Development setup

- Rust 1.88+ via [rustup](https://rustup.rs); `rust-toolchain.toml` selects stable with
  `rustfmt`, `clippy` and the `wasm32-unknown-unknown` target.
- Python 3 only if you regenerate test vectors (scripts in each crate's `testdata/` or
  `tests/` folder).

Before opening a pull request, run:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --exclude dl-bench --target wasm32-unknown-unknown
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

Crates with a `std` feature must also pass `cargo test -p <crate> --no-default-features`.

## Rules

These are enforced in review and, where possible, by CI and the workspace lints.

### Dependencies

- **No third-party crates**, not even as dev-dependencies. Only `core`, `alloc`, `std`
  and other `dl-*` crates from `[workspace.dependencies]`.
- Python or Node.js packages may be used by scripts that *generate* test vectors. The
  generated data is committed; building and testing never needs them.

### Customers in docs

- The customers that run `dl-agent` are recorded in the Request Tracking (RT) customer book,
  not here. Take a customer's name, sites and products from RT's `get_customer` (MCP) and
  write the canonical name with its RT id, e.g. Muine Bay Resort (`muine-bay-resort`).
- Do not invent short or local spellings of a customer's name; quote an old spelling only
  when citing history.

### Crate layout

- Each crate lives in `crates/dl-<name>/` and inherits `version`, `edition`,
  `rust-version`, `authors`, `license`, `repository` and `publish` from the workspace,
  and sets `[lints] workspace = true`.
- Pure computation crates are `#![no_std]` with `extern crate alloc;`; tests may use
  `std`. Crates doing I/O use `std`.
- Every crate must build for `wasm32-unknown-unknown`. Put platform code behind `cfg`.
  `std::time::SystemTime::now()` panics on that target: provide an API that takes the
  time as an argument.
- `README.md` is the crate documentation (`#![doc = include_str!("../README.md")]`), so
  every example in it is a doctest.

### Correctness

- Expected values in tests come from a standard (RFC, NIST, ISO appendix), from an
  independent implementation, or from a reproducible generator script that is committed.
  **Never paste the output of the code under test as the expected value.**
- Cite the source of every rule in rustdoc (URL or document and section). If a fact
  cannot be verified, leave it out and say so.
- No panics on untrusted input. Parsers return `Result` with one `Error` enum per crate,
  implementing `Display` and `core::error::Error`, with byte offsets where useful.
  `unwrap()` only in tests or where the invariant is proven next to it (comment why).
- `unsafe` is denied. If it is truly needed (OS calls), allow it on the smallest item and
  write a `// SAFETY:` comment.

### API style

- Small explicit APIs; free functions and plain structs before builders, unless there are
  many options.
- Names follow the [Rust API guidelines](https://rust-lang.github.io/api-guidelines/)
  (`to_*`, `as_*`, `into_*`, `try_from`, `FromStr`).
- Every public item has rustdoc (`missing_docs` is on).

### Benchmarks

Use the in-house `dl-bench` crate (`[[bench]] harness = false`). When README numbers
change, state the machine and how noisy the run was.

## Pull requests

- One topic per pull request; describe what changed and how you verified it.
- Add tests for every bug fix (the failing input) and every new feature.
- Update the crate README and the root `CHANGELOG.md`.

## License of contributions

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in this project, as defined in the Apache-2.0 license, shall be dual licensed
under MIT OR Apache-2.0, without any additional terms or conditions.

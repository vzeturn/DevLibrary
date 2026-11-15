# dl-rand

Random numbers from two sources: the operating system's CSPRNG for secrets, and a fast
xoshiro256** generator for everything else. No dependencies, builds for Linux, macOS,
Windows, BSD, WASI and the browser.

Part of [DevLibrary](https://github.com/vzeturn/DevLibrary). Used by `dl-uuid` and
`dl-otp`.

## Replaces

| Ecosystem | Package |
|---|---|
| C# / .NET | `RandomNumberGenerator.Fill`, `RandomNumberGenerator.GetInt32`, `System.Random` / `Random.Shared` |
| TypeScript / JS | `crypto.getRandomValues`, `crypto.randomInt`, `Math.random` |
| Go | `crypto/rand`, `math/rand/v2` |
| Python | `secrets`, `random` |
| Rust | `rand`, `getrandom`, `fastrand` |

## Which one to use

| Need | Use | Why |
|---|---|---|
| Keys, tokens, salts, session IDs, OTP secrets, UUIDs | `fill`, `secure_bytes`, `secure_u64`, `secure_below` | Straight from the OS CSPRNG; unpredictable |
| Simulations, sampling, shuffling a playlist, retry jitter, tests | `Rng`, `u64()`, `range()`, `f64()`, `shuffle()` | Fast; reproducible with a seed |

`Rng` (xoshiro256**) is **not** cryptographically secure: a few outputs reveal its
state. Never use it for secrets.

## Installation

```toml
[dependencies]
dl-rand = { git = "https://github.com/vzeturn/DevLibrary" }
```

From a local checkout: `dl-rand = { path = "../DevLibrary/crates/dl-rand" }`. Inside the
workspace: `dl-rand.workspace = true`. Minimum Rust version: 1.88.

## Quick start

```rust
use dl_rand::Rng;

// Secrets from the OS.
let api_key: [u8; 32] = dl_rand::secure_bytes()?;
let pin = dl_rand::secure_below(1_000_000)?; // uniform 0..1_000_000, no modulo bias
assert!(pin < 1_000_000);
let mut salt = [0u8; 16];
dl_rand::fill(&mut salt)?;
assert_ne!(api_key, [0; 32]);

// Fast thread-local generator.
let dice = dl_rand::range(1..=6);
assert!((1..=6).contains(&dice));
let mut queue = vec!["a", "b", "c", "d"];
dl_rand::shuffle(&mut queue);
let jitter_ms = (dl_rand::f64() * 250.0) as u64;
assert!(jitter_ms < 250);

// Reproducible sequences for tests and simulations.
let mut a = Rng::from_seed(42);
let mut b = Rng::from_seed(42);
assert_eq!(a.next_u64(), b.next_u64());
assert!(a.range(-5i32..5) >= -5);
assert_eq!(Rng::from_seed(7).choose(&[10, 20, 30]).is_some(), true);
# Ok::<(), dl_rand::Error>(())
```

## Platforms

| Target | OS source |
|---|---|
| Linux, Android | `getrandom(2)` |
| macOS, iOS, FreeBSD, OpenBSD, NetBSD, DragonFly | `getentropy(2)` |
| Windows 10+ | `ProcessPrng` (bcryptprimitives.dll) |
| `wasm32-wasip1` | WASI `random_get` |
| `wasm32-unknown-unknown` | host import `dl_rand.fill(ptr, len)` |

In the browser, provide the import when instantiating the module:

```js
const imports = { dl_rand: { fill: (ptr, len) =>
  crypto.getRandomValues(new Uint8Array(instance.exports.memory.buffer, ptr, len)) } };
```

`len` is never more than 65 536 bytes, the `getRandomValues` limit.

## API overview

| Item | Purpose |
|---|---|
| `fill(&mut [u8])`, `secure_bytes::<N>()`, `secure_u64()`, `secure_below(n)` | OS CSPRNG; return `Result<_, Error>` |
| `Rng::from_seed(u64)`, `Rng::from_state([u64; 4])`, `Rng::from_os()` | Create a fast generator |
| `Rng::{next_u64, next_u32, next_f64, next_bool, fill_bytes, below, range, shuffle, choose}` | Fast generator methods |
| `u64()`, `f64()`, `range(r)`, `shuffle(&mut [T])`, `with_thread_rng(f)` | Thread-local fast generator, seeded from the OS |
| `Uniform` | Integer types accepted by `range` |
| `Error::{Os(code), Unsupported}` | OS failure |

## Performance

`cargo bench -p dl-rand`, Intel Core i7-9850H, Windows, rustc 1.100:

| Case | Time / throughput |
|---|---:|
| `Rng::next_u64` | 2.7 ns |
| `Rng::below(1000)` | 3.8 ns |
| `Rng::fill_bytes`, 64 KiB | 1.75 GiB/s |
| OS `fill`, 32 bytes | 341 ns |
| OS `fill`, 64 KiB | 1.4 GiB/s |

## Standards and references

- xoshiro256** 1.0 by Blackman and Vigna, <https://prng.di.unimi.it/>; seeds expanded
  with SplitMix64 as the authors recommend. Output is checked against the reference
  algorithm.
- Unbiased bounded integers with Lemire's method (`below`) and rejection sampling
  (`secure_below`).

## Limitations

- No floating-point distributions beyond uniform `[0, 1)` (no normal, exponential, ...).
- `unsafe` is allowed only in `src/os.rs`, for the OS calls, each with a `SAFETY:`
  comment.

## License

Licensed under either of [MIT](https://opensource.org/licenses/MIT) or
[Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0) at your option.

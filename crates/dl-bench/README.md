# dl-bench

A minimal benchmark harness used by every DevLibrary crate in place of `criterion`. Each
case is warmed up, then sampled for a fixed time budget; the median time per iteration
(and the throughput, when given a byte count) is printed.

Part of [DevLibrary](https://github.com/vzeturn/DevLibrary).

## Usage

`Cargo.toml` of the crate being benchmarked:

```toml
[dev-dependencies]
dl-bench = { git = "https://github.com/vzeturn/DevLibrary" }

[[bench]]
name = "my_bench"
harness = false
```

`benches/my_bench.rs`:

```rust,no_run
use dl_bench::Bench;

fn main() {
    let mut b = Bench::from_args();
    let data = vec![1u8; 64 * 1024];
    b.throughput("sum 64 KiB", data.len() as u64, || data.iter().map(|&x| x as u64).sum::<u64>());
    b.run("format a number", || format!("{}", 1234567));
}
```

```text
$ cargo bench -p my-crate
sum 64 KiB                                    9.80 µs     6377.3 MiB/s
format a number                               41.2 ns
$ cargo bench -p my-crate -- format          # only cases whose name contains "format"
```

Return the computed value from the closure (or wrap inputs in `std::hint::black_box`) so
the optimizer cannot remove the work.

## API

| Item | Purpose |
|---|---|
| `Bench::from_args()` | Runner; the first non-flag argument filters case names |
| `Bench::measure_time(Duration)` | Measurement budget per case (default 1 s, 30 samples, 200 ms warm-up) |
| `Bench::run(name, f)` | Time per iteration |
| `Bench::throughput(name, bytes, f)` | Time per iteration and MiB/s |
| `Report { median_ns, min_ns, iterations }` | Returned by `run` / `throughput` (`None` when filtered out) |

## Limitations

No statistics beyond median and minimum, no saved baselines or HTML reports. Compare
numbers from the same machine and run.

## License

Licensed under either of [MIT](https://opensource.org/licenses/MIT) or
[Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0) at your option.

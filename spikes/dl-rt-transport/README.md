# dl-rt-transport (spike, RT-1318)

Measures one long-lived duplex session over **QUIC** (quinn 0.11, rustls/ring) and over
**WebSocket + TLS 1.3 over TCP** (tokio-tungstenite + rustls), with an in-process network
impairment shim, as input to [ADR 0011](../../docs/decisions/0011-realtime-lan-first-sync.md).

Not production code. It is its own Cargo workspace on purpose: DevLibrary crates allow no
third-party dependencies (CONTRIBUTING.md), and this spike needs quinn/tokio/rustls to
measure them. The root workspace (`members = ["crates/*"]`) never builds it.

## Run

```sh
cd spikes/dl-rt-transport
cargo test --release
cargo run --release --bin rt-bench            # full run, about 3 min
cargo run --release --bin rt-bench -- --quick # 1/5 of the samples
```

## What it measures

- **Session:** two channels, `CONTROL` (1 KiB pings: a small capture batch or a command)
  and `BULK` (16 KiB frames). QUIC: one bidirectional stream per channel, control at a
  higher priority. WebSocket: one socket, frames tagged by channel, writer prefers control.
- **connect p50:** time to a usable session (QUIC handshake vs TCP + TLS + WS upgrade).
- **RTT idle:** sequential ping -> ack on a warm session.
- **RTT under bulk:** pings while 4 x 16 KiB bulk frames stay in flight.
- **bulk MB/s:** 16 KiB frames, 64 in flight, acked one by one.

## Result (2026-11-26, Intel iMac, macOS, full run)

| transport | link | connect p50 | RTT idle | RTT under bulk (4 x 16 KiB in flight) | bulk MB/s (64 in flight) |
|---|---|---|---|---|---|
| quic | loopback | 0.6 | 0.1 / 0.2 / 0.2 | 0.8 / 1.7 / 2.3 | 109.4 |
| ws+tls | loopback | 1.1 | 0.1 / 0.2 / 0.2 | 0.2 / 0.3 / 0.3 | 559.6 |
| quic | 30ms RTT, 50 Mbit/s | 34.1 | 33.2 / 34.6 / 35.1 | 34.7 / 36.2 / 36.9 | 5.6 |
| ws+tls | 30ms RTT, 50 Mbit/s | 66.7 | 33.1 / 34.2 / 34.7 | 35.3 / 36.6 / 37.1 | 6.1 |
| quic | 30ms RTT, 50 Mbit/s, 1% loss | 33.8 | 33.2 / 34.5 / 126.9 | 42.0 / 65.5 / 88.3 | 0.5 |
| ws+tls | 30ms RTT, 50 Mbit/s, 1% loss | 66.5 | 33.4 / 34.7 / 93.4 | 35.4 / 96.2 / 98.2 | 5.9 (TCP loss modelled, no congestion control) |

Reading and conclusions: ADR 0011, section D5.

## Shim (macOS has no `tc netem`)

`src/shim.rs` relays traffic through a per-direction FIFO with a fixed one-way delay, a
bandwidth cap and random loss (deterministic seed).

- UDP/QUIC: lost datagrams are really dropped; quinn's loss recovery and congestion
  control react as on a real network.
- TCP/WebSocket: bytes cannot be dropped above the kernel, so a lost segment is modelled
  as a 2 x RTT recovery delay (about Linux's tail-loss probe for sparse traffic) that
  every later byte waits behind (head-of-line). TCP congestion control is not modelled,
  so WebSocket throughput under loss is optimistic and marked as such.
- tokio timers have about 1 ms resolution on macOS, which adds up to ~1 ms per hop to
  both transports alike.

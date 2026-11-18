# dl-agent handoff (RT-0164, 2026-11-18)

Branch `feat/dl-agent` (WIP, the crate does **not** compile yet: `lib.rs` is missing).

## Done
- `PROTOCOL.md` v1 (identity/signing, endpoints, heartbeat, commands, channels + canary gate, update, reports, config).
- `INVENTORY.md`: all 9 agents, platform feature matrix, module requirements, risks, migration order.
- `src/error.rs`, `version.rs` (SemVer + tests), `channel.rs` (canary/stable, rollout bucket + tests),
  `manifest.rs` (signed manifest, key rotation + tests), `update.rs` (decide, UpdateState, Installer,
  probation/rollback; tests file `update_tests.rs` still to write; uses a `store` module still to write).
- Agent hub (Cloudflare Worker + D1 + R2) is being built on branch `feat/agent-hub` in worktree
  `~/Projects/DevLibrary-hub` (`services/agent-hub/`). Check its last commit; merge into `feat/dl-agent` after review.

## Remaining (in order)
1. `lib.rs` (mods + `pub use Error` + `current_target()`), `store.rs` (atomic `read_json`/`write_json`), `update_tests.rs`.
2. `report.rs`, `supervisor.rs` (Module trait), `identity.rs` (DPAPI/0600 seed, request signing §1),
   `hub.rs` (Transport trait over `dl_http::Client`), `config.rs` (last-known-good), `agent.rs` (loop), `main.rs` (CLI), built-in `sys` module.
3. Root `Cargo.toml` `[workspace.dependencies]` + README table + CHANGELOG; `cargo test -p dl-agent`,
   clippy `-D warnings`, fmt; check wasm32 handling like dl-http/dl-winsvc.
4. Review + merge hub; decide hub domain (needs owner) and deploy per repo rules.
5. Module migrations per `INVENTORY.md` §5 — tracked as tasks of RT-0164, not separate requests.

# dl-agent-hotellink module: report

Branch `feat/dl-agent-hotellink` (worktree `/Users/imac/Projects/DevLibrary-hl`). Not pushed or merged.

## What was ported

From SMILE-HLAPI `go/internal/*`, which was only read and not changed:

| Go | Rust (`crates/dl-agent-hotellink/src`) |
|---|---|
| `jobmodel` | `jobmodel/` (jobs, mapping and availability rows, Go-compatible JSON) |
| `pms` (repo, entities, insert, mapping, availability) | `pms/` on dl-tds through dl-agent's `sql::Pool`; parameterized SQL only |
| reconciler (new / modify / cancel / mapping, manual-conflict and user-cancel rules) | `reconcile/` (committed earlier on this branch as a89b249) |
| `store` job queue (jsonl) | `jobqueue/` (durable `queue.jsonl`, first-seen order, one-time legacy import with a marker) |
| hub client | `hubclient/` (fallback base URLs on network errors only; 401 → Unauthorized; 403 → Superseded; `X-Instance-Id` kept; `X-Agent-Version: dl-agent-<version>`) |
| breaker, liveness, policy cache | `breaker.rs`, `liveness.rs`, `policycache.rs` |
| agent run loops (poll, apply, ack, availability, mapping sync) | `run/` |
| config + DPAPI secrets | `config/` (same `config.json`, `enc:` prefix, `SMILEHL_AGENT_CONFIG` override, `%ProgramData%\SMILE-HL-Agent\config.json`) |
| install-script enrollment | `enroll.rs`: `dl-agent hl-enroll` |
| — | `module/`: the dl-agent `Module` (state, metrics, reports, commands, stop) |

Registered in `crates/dl-agent-bin/src/main.rs`: `.module(hotellink_module()).tool(hl_enroll_tool())`. The module is opt-in (`enabled_by_default = false`).

## Module behaviour

- **Config**: `modules.hotellink.{enabled, config_path}`. Without a usable config or token the module stays degraded with `not configured: …`, answers commands, checks the file again every 30 s, and does not send a problem report.
- **State**: in `<data_dir>/hotellink/`. The Go agent's `queue.jsonl`, next to `config.json`, is imported once (`legacy-queue-imported` marker).
- **Health**:
  - It becomes `running` after the first successful poll.
  - It becomes degraded on these signals, in priority order:
    1. superseded (403);
    2. 401;
    3. breaker open;
    4. hub fail streak ≥ 2 (past one back-off);
    5. stalled loop;
    6. SQL unreachable for more than 30 s.
- **Metrics**: jobs_polled, jobs_applied, jobs_acked, jobs_failed, manual_conflicts, availability_pushes, polls_ok, queue_depth, unacked, mapping_hash, mapping_count, last_sync_at, last_job_at, breaker_state.
- **Reports**:
  - Codes: hl_hub_failed, hl_auth_failed, hl_pms_failed, hl_reconcile_failed, hl_manual_conflict.
  - At most one per code per 15 min, with a `held_back` count.
  - auth, pms and reconcile are Error; the rest are Warning.
  - No guest names, SQL text or token.
- **Commands**: `hl.status` and `hl.sync-mapping`. The `hotellink.*` prefix is also accepted.
- **Stop**: the runner has its own thread because the 25 s long poll and SQL transactions cannot be interrupted. The module waits 5 s for the job in flight, then returns. A job that did not finish stays in the durable queue and is applied or acked on the next start.
- **hl-enroll**:
  - The token comes from `DL_AGENT_HL_TOKEN` or stdin, with echo hidden. The password comes from `DL_AGENT_HL_SQL_PASSWORD` or stdin. Neither is ever taken from argv.
  - A config that already has a token is kept unless `--force` is passed.
  - An unreadable config is an error unless `--force` is passed.

## Differences from Go and why

- **Dropped**: the service host, `update/` (no CheckUpdate, DownloadBlob or AckUpdate), the log file, and `cmd/agent`. dl-agent provides all of these. The self-update fields of the heartbeat are dropped for the same reason.
- **403 (superseded)**: the module stays degraded and sends one Critical `hl_auth_failed` report. The supervisor does not restart it, since restarting would fight the other install. Go behaves the same way: its Runner stops all loops and is not restarted. The only addition is the visible state and the report.
- **Go's ignored `_ =` errors**: these now log a warning, following the no-swallowed-errors rule. Behaviour does not change otherwise.
- **Time**: the local day and error timestamps use the fixed VN offset (+07:00), as Go did with `Asia/Ho_Chi_Minh`.
- **Jobs polled after a stop**: they are queued but not processed. They are processed on the next start.
- **Boot**: seeds `HLInventorySync` (the Go boot behaviour, kept).
- **Stuck runner**: after the grace the runner thread is detached rather than killed. Rust cannot kill a thread inside a blocking TDS or HTTP call.

## Gaps

- **`configui` is not ported (deferred)**: the local config web UI. `hl-enroll` covers first-time setup, and the file can be edited by hand.
- **Windows not run**:
  - Nothing ran on Windows: not DPAPI, not the `%ProgramData%` path, not echo hiding.
  - Only the `x86_64-pc-windows-gnu` clippy build checks that code compiles.
  - Off Windows, `protect`/`unprotect` pass bytes through as-is (base64 under `enc:`), so the config tests check the format, not the encryption.
- **SQL Server versions**: the SQL integration ran on SQL Server 2022 only. 2005–2019 were not run.

## Tests

- **Unit and fake tests**: 185 `#[test]` in the crate (plus 3 lifecycle tests in `tests/module_lifecycle.rs`). They use hand-written fakes only: `FakeRepo`, a fake hub server, and a temp-dir queue.
  - reconcile: 60 (ports of the Go reconciler tests)
  - run: 34
  - pms: 23 (14 of them are the SQL integration tests, which skip without the gate)
  - jobmodel: 12
  - module: 11
  - config: 8
  - hubclient: 8
  - jobqueue: 7
  - enroll: 5
  - liveness: 5
  - breaker: 3
  - policycache: 3
  - plus small helpers
- **Lifecycle under the real Supervisor**: opt-in; not configured → degraded with commands and a prompt stop; a config without a token. The in-crate module tests cover poll → running (and check the `X-Agent-Version` header), 401 → degraded, a stop with a job in flight (stops in under 2 s, no ack, the job stays queued), and the one-time Go queue import.
- **SQL integration**: `src/pms/sql_tests.rs`, gated by `HL_AGENT_TEST_SQL`, ports Go `repo_test.go` (plus one test for mapping insert, update and delete). It ran once against my own OrbStack container `hl-agent-sqltest` (`mcr.microsoft.com/mssql/server:2022-latest`) with `tests/fixtures/smile-schema.sql`, a copy of `go/deploy/smile-schema.sql`. Result: 14 passed (re-run by the main session with `encrypt=disable`, `--test-threads=1`; an `Encrypt=false;TrustServerCertificate=true` string fails because dl-tds expects `encrypt=disable`). The container was removed afterwards.

## Gate commands

Run by the main session after the implementer stopped before committing (it also fixed 6 clippy findings: a needless clone, `slice::from_ref`, three type aliases for complex tuple types, a collapsed `if let`):

- `cargo fmt --all`: clean
- `cargo test -p dl-agent-hotellink -p dl-agent-bin`: 185 + 3 passed (SQL tests skip without the gate)
- `cargo clippy -p dl-agent-hotellink -p dl-agent-bin --all-targets -- -D warnings`: clean, host and `x86_64-pc-windows-gnu`
- Largest file: `pms/repo.rs` 678 lines (≤ 800)

## Concerns

- The poll and transaction threads can outlive the module by up to one long poll (25 s) after a stop. They do no work once stopped, but they do hold one pooled connection until they end.
- The Windows-only paths (DPAPI, ProgramData, console echo) are compiled but have not been run.

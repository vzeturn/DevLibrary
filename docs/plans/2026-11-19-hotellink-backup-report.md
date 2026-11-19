# Hotel Link e2e and backup restore/tenant keys (RT-0259): report

Request: RT-0259 (part of RT-0164). Branch `cloud/rt-0259`, built in two streams (`-hl`, `-bk`). Nothing was
deployed.

## Acceptance → evidence (merged branch, cloud session, Linux, SQL Server 2022 container `TZ=Asia/Ho_Chi_Minh`)

| Criterion | Test / command | Real result |
|---|---|---|
| Hotel Link booking e2e with `HL_AGENT_TEST_SQL` | `HL_AGENT_TEST_SQL='server=…;Encrypt=false;TrustServerCertificate=true' cargo test -p dl-agent-hotellink --lib -- --test-threads=1`: `e2e::tests::booking_and_mapping_reach_sql_and_the_hub_gets_the_acks`, `ack_lost_at_a_crash_is_replayed_without_a_second_write`, `breaker_opens_on_sql_down_stops_hammering_and_recovers` | 199 passed, 0 failed. Booking: 1 Folio, 2 FolioRate, 1 AdditionalName, RoomTypeMapped r-200/STD, ack `ok:true` with the folio number. The replayed ack after a crash is byte-identical, with no second write. The breaker opens after 5 failures, does not hammer, and recovers. |
| `Encrypt=false;TrustServerCertificate=true` accepted | hotellink config unit tests (5 spellings); the e2e connects with that exact site string | pass |
| Poll/tx threads stop promptly | `dl_http::Cancel` + `Runner::stop`. A stop while the hub holds a 25 s poll returns in < 2 s (20 ms measured); 2 tests | pass |
| Restore `-Replace` with checksum match | `BACKUP_AGENT_TEST_SQL=… BACKUP_AGENT_TEST_DIR=… cargo test -p dl-agent-backup sql_tests -- --test-threads=1`: `replace::sealed_backup_restores_with_replace_and_to_a_new_name_with_matching_checksums`, `replace::an_unencrypted_backup_from_before_tenant_keys_still_restores` | 5 passed. Row counts and `CHECKSUM_AGG(BINARY_CHECKSUM(*))` per table match the values from before the backup, after REPLACE and after a restore to a new name (customers 2000 / orders 10000 / notes 500). |
| One key per tenant; an agent cannot read another tenant's data | `tenant_keys_tests::tenant_a_cannot_restore_a_backup_sealed_for_tenant_b`, `crypto::envelope::tests::another_tenants_agent_cannot_open_the_file`, `the_hub_never_gives_this_agent_another_tenants_key` (fake hub); live: restore with tenant B's key → `backup_credential_error`, DB unchanged | pass (agent side) |
| clippy/fmt | `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -D warnings`; `cargo clippy -p dl-agent-hotellink -p dl-agent-backup -p dl-http -p dl-agent-bin --all-targets --target x86_64-pc-windows-gnu -D warnings`; `cargo test -p dl-agent-hotellink -p dl-agent-backup -p dl-http -p dl-agent-bin` | clean; 554 passed, 0 failed |

## Notes

- **Hotel Link.** `Encrypt=false;TrustServerCertificate=true` already parsed. The refusals came from the hotellink
  config, which is now fixed. The breaker e2e found a busy loop: with the breaker open, the hub handed the held job
  back on every poll, and the poll rate reached about 2600/s against the fake hub. The Go agent has the same loop.
  The next poll now waits `breaker_poll_wait` (5 s).
- **Backup tenant keys.**
  - Backup files were never encrypted before. Both the Go agent and the port uploaded the plaintext `.bak`.
  - The module now seals every upload with the tenant's data key (format `DLBKENC1`: AES-256-GCM chunks, a
    per-file HKDF key, the key id in the header).
  - Restores pick the key named in the header. Plaintext backups from before this change still restore.
  - The key comes from `GET /api/agent/backup-key[?id=]`. The hub of the backup module is the **SRBackup hub**
    (another repo), and it does not serve that route yet.
  - Until it does, agents upload plaintext as before (404 fallback).
  - The hub change is specified in `docs/plans/2026-11-19-rt0259-backup-tenant-keys.md`: D1 table
    `tenant_backup_keys` wrapped under the master key, `backup_history.encryption_key_id`, the route returns 404 for a
    foreign id, rotation, and the vitest cases.
  - Software AES runs at about 33 MiB/s, so a 10 GiB backup takes about 5 min longer.
- **Windows / site only:**
  - a real `kill -9` of the service (the e2e simulates the crash with a stop while the ack is held);
  - DPAPI state, Windows-auth SQL, real cloud destinations and remote SQL download directories.

---

# Stream reports (verbatim)

## RT-0259 stream HL (dl-agent-hotellink): report

Branch `cloud/rt-0259-hl`, worktree `/home/user/wt/hl`, one commit `7adf397` on top of `f241daf`. Not pushed, not merged, not deployed.

## Summary

| Item | Result |
|---|---|
| 1. SQL e2e (booking → Folio/FolioRate/AdditionalName + RoomTypeMapped → ack; ack replay after a crash; PMS breaker) | 3 new gated tests in `src/e2e/`, all pass on SQL Server 2022 |
| 2. `Encrypt=false;TrustServerCertificate=true` | Accepted. It already parsed; the real refusals were elsewhere (see below). Fixed in the hotellink config with 5 unit tests |
| 3. Prompt stop of the poll/tx threads | `dl_http::Cancel` + `Runner::stop` cuts the long poll. A stop with a held 25 s poll returns in < 2 s (measured 20 ms in the e2e); 2 tests |
| Extra (found by the breaker e2e) | With the breaker open, the hub hands back the held job on every poll, so the Go agent and the port polled the hub with no wait (2617 polls/s against the fake). The next poll now waits `Timings::breaker_poll_wait` (5 s) |

## 1. End-to-end tests (gate `HL_AGENT_TEST_SQL`)

Files:
- `crates/dl-agent-hotellink/src/e2e/mod.rs`: the harness.
  - Database `SMILE_FO_E2E`: the schema fixture plus Go's `seedSmile`.
  - A TCP relay `Proxy` that can be switched down (it refuses new connections and cuts the live ones) and counts connections.
  - A fake hub that records every ack body, mapping-command ack and mapping push. It can hold acks and redeliver a job once.
  - The real module `run_configured`, started from a Go-format `config.json` (`connectionStringOverride` = the site spelling `…;Encrypt=false;TrustServerCertificate=true;` through the relay), with the real `SqlRepo` and reconciler.
- `crates/dl-agent-hotellink/src/e2e/tests.rs`: the three tests.

Command (SA password in a scratch file, never printed):
```
HL_AGENT_TEST_SQL="server=127.0.0.1,14361;user id=sa;password=…;database=master;Encrypt=false;TrustServerCertificate=true" \
CARGO_TARGET_DIR=/home/user/wt/target-hl cargo test -p dl-agent-hotellink --lib -- --test-threads=1 --nocapture
```
Container `rt259-hl` (`mcr.microsoft.com/mssql/server:2022-latest`, `TZ=Asia/Ho_Chi_Minh`, port 14361) was removed afterwards with its anonymous volume.

Result: **199 passed, 0 failed** (crate lib, gate on: 14 `pms::sql_tests` + 3 e2e + the rest). Each e2e run gives the same numbers.

- `e2e::tests::booking_and_mapping_reach_sql_and_the_hub_gets_the_acks`
  - The poll serves a booking (one DLX01 room, 2 nights, check-in today+30) and a mapping `insert` command (r-200 → STD).
  - In SQL: 1 Folio (DLX01, rate 1 500 000), 2 FolioRate, 1 AdditionalName, a RoomTypeMapped row r-200/STD/IsBooking=1.
  - Job ack body: `ok:true`, `error:""`, `recentErrors:null`, `pmsResult.action:"save"`, `completed:true`, `folios[0].folioNum` = the DB FolioNum, `roomTypeCode:"DLX01"`.
  - Mapping-command ack `ok:true`. Mapping pushes: 2 rows at boot, 3 after the command. The module metrics `mapping_hash` and `mapping_count=3` match the last push. An availability push arrived.
  - Measured: ack 101 ms after start, stop 20.7 ms, 2 SQL connections through the relay.
- `e2e::tests::ack_lost_at_a_crash_is_replayed_without_a_second_write`
  - The hub holds the ack. The folio is written, then the module stops: the stop cuts the held ack and returns in < 2 s.
  - The queue on disk has the job applied but not acked; no ack was delivered.
  - Restart on the same data dir:
    - `drainUnacked` re-acks before the first poll, and the replayed body is **byte for byte equal** to the lost one.
    - The hub then redelivers the same job. It is acked again (`save`) with **no second write**: same single FolioNum, 2 FolioRate, 0 cancelled.
  - Queue empty at the end.
- `e2e::tests::breaker_opens_on_sql_down_stops_hammering_and_recovers`
  - SQL down (relay) with a job waiting. The breaker opened after 502 ms on exactly 5 failures (threshold).
  - In the 2 s while open:
    - `jobs_failed` stayed 5, so no apply ran.
    - 1 SQL connection attempt, from the fail-open `GetHotelDate` of each poll, which the pool's own 1 s-doubling backoff limits. The test bound is ≤ 2.
    - 10 hub polls (the new 200 ms held-poll wait in the test timings).
    - No ack, 0 folios.
  - SQL back: after the 3 s test cooldown the half-open probe applied the job, recovered 652 ms after the relay came up. The breaker went back to `closed`, 1 folio, the ack is `ok:true` with an empty `error`.

Parity with Go (`/home/user/SMILE-HLAPI/go/`):
- `internal/agent/run.go:159` bookingLoop: drainUnacked → drainPending → poll → mapping cmds → GetHotelDate → enqueue + processJob. Same order in `run/booking.rs` `booking_loop`/`poll_once`.
- `run.go:207` processJob:
  - The breaker gate holds without an ack.
  - PMS down → `OnFailure`, no ack.
  - Otherwise `OnSuccess`, MarkApplied, ack.
  - Same in `run/booking.rs` `process_job`.
- `run.go:261` ackJob and `:269` drainUnacked: the re-ack sends `recentErrors` nil and the stored pmsResult. That is why the replayed body equals the lost one when the first ack's error ring was empty.
- `run.go:89` `breaker.New(5, 30s, 5min)`: now `Timings::default()` (5, 30 s, 300 s); the tests shorten it.
- `run.go:190` GetHotelDate runs on every poll and fails open. Same here, and it is the only SQL access while the breaker is open.
- `e2e/e2e_test.go:133` `TestE2EBookingAndAvailability` and `:390` `seedSmile`: same seed and the same kinds of assertions (Folio + FolioRate + AdditionalName counts, mapping push, availability).

Deviations and limits:
- "Process killed" is simulated by a module stop while the hub holds the ack. The on-disk state is the same as after a kill (applied, not acked) because `mark_applied` is written before the ack. A real `kill -9` of the service was not run.
- The PMS-down error lines go out with the heartbeat or with the next ack, whichever drains the ring first (Go `drainErrors`). The e2e does not assert where they go.
- The availability push is only checked as "arrived" (Go's e2e checks HL `saveInventory`, a hub concern).

## 2. Connection string `…;Encrypt=false;TrustServerCertificate=true`

Finding: that exact string already parses (`dl_tds::Encrypt::Off`, trust on) and logs in to SQL Server 2022. The whole SQL suite (14 + 3 e2e) passed with it as `HL_AGENT_TEST_SQL`, and the e2e module itself uses it. The earlier report's failure is not reproducible on this base.

Probing the spellings on dl-tds showed these real refusals:
- `TrustServerCertificate=yes` / `no` / `YES` → `invalid trust server certificate`. go-mssqldb's ParseBool rejects them; SqlClient accepts them.
- `Trust Server Certificate=true` (the SqlClient synonym) was silently ignored. The certificate was then checked and a SMILE self-signed certificate fails.
- `Encrypt=false` without `TrustServerCertificate` → `connect failed: TLS certificate is not trusted` against SQL 2022 (seen live). SqlClient never checks the certificate for login-only encryption.

Fix place: the hotellink config, not dl-tds.
- dl-tds documents go-mssqldb semantics, and every module shares it. The URL-form test even pins `trustservercertificate=yes` as an error.
- Arrival (`with_sqlclient_defaults`) and Membership (`normalize_connection_string`) already normalise SqlClient strings at module level.
- New `crates/dl-agent-hotellink/src/config/sqlclient.rs` `to_tds()`, used by `SqlConfig::conn_string()` for `connectionStringOverride` only. It rewrites only these keys and keeps everything else as written:
  - `TrustServerCertificate` / `Trust Server Certificate`: `true`/`yes` → `true`, `false`/`no` → `false`, in any case or spacing. Unknown values pass through so that dl-tds reports them; the error does not echo the password.
  - `Encrypt=false`/`no`/`optional`/`0` without a trust key adds `TrustServerCertificate=true`.
  - Quote-aware split (`Password="a;b"""`), empty parts dropped.
  - URL form and strings without `Encrypt` are unchanged, because Go read those with go-mssqldb, which dl-tds matches.
- Semantics, documented in the module docs and the README:
  - `Encrypt=false` maps to `Encrypt::Off`: LOGIN7 is encrypted if the server supports TLS, the whole session if the server requires it, and the certificate is not checked (as SqlClient).
  - An explicit `TrustServerCertificate=false` keeps the check on, which is stricter than SqlClient for login-only TLS.
- Tests in `config::sqlclient::tests` (5): site string from the install guide (also via `TdsConnector::parse`); 8 Encrypt spellings × 10 trust spellings (case, spaces, yes/no/true/false, synonym); other modes kept (`yes`, `True`, `Mandatory`, `disable`, `Strict`, absent); quoted parts untouched; URL and unknown values pass through.
- Command: `cargo test -p dl-agent-hotellink --lib config`: 20 passed.

## 3. Prompt stop

- `crates/dl-http/src/cancel.rs` (new):
  - `Cancel` is a cloneable handle. `Client::request_with_cancel` registers a clone of the exchange's socket.
  - `cancel()` shuts it down, so a blocking read ends at once, and makes every later request fail before connecting. The result is the new `Error::Cancelled` (`Error` is `#[non_exhaustive]`).
  - A cancelled exchange's connection is not returned to the pool.
  - A DNS lookup or TCP connect already started is not interrupted; it is bounded by `connect_timeout`.
  - Tests: 3, plus a doc test.
- `HubClient`:
  - It owns a `Cancel` and exposes `cancel()`.
  - A cancelled call returns at once without trying the fallback bases, so it can never fall over to the production `smile-hl-hub.fly.dev`.
  - `Runner::stop()` (and the 403 superseded path) calls it.
- A SQL transaction in flight is still not cut, on purpose: the module waits up to 5 s for it (`STOP_GRACE`) as before.
- Tests:
  - `run::tests::loops::stop_ends_a_held_long_poll_within_two_seconds`: runner with `poll_wait_sec: 25`, the hub holds the poll. `stop()` → `run()` returns within 2 s, and there is no second poll.
  - `module::tests::stop_cuts_the_long_poll_and_ends_the_runner_threads`: the real `run_configured` with a 10 s grace. Stop returns < 2 s, which proves the runner thread was joined rather than detached after the grace.
  - Both fail without the fix: 2.009 s "runner still running" and a 10.004 s stop. I checked this by disabling the `hub.cancel()` line temporarily.

## Extra: hub polling while the breaker holds jobs

- The Go hub's `ListPending` (`internal/store/file.go:180`) hands pending jobs back on every poll without a lease. With the PMS down and the breaker open, the Go agent (`run.go:159`) and the port re-polled with no wait: about 4800 polls in 2 s against the fake hub.
- Now `poll_once` returns `Held` when it got jobs and the breaker is open afterwards. The loop then sleeps `Timings::breaker_poll_wait` (default 5 s, e2e 200 ms) before polling again.
- This is a documented deviation from Go.
- Tests:
  - `run::tests::jobs::held_jobs_slow_the_poll_while_the_breaker_is_open`: 2..=8 polls in 1 s with a 200 ms wait. It fails with a zero wait: 2617 polls.
  - The e2e above (10 polls in 2 s).

## Gates (all on the final tree)

- `cargo fmt --all --check`: clean.
- `cargo clippy -p dl-http -p dl-agent-hotellink -p dl-agent-bin --all-targets -- -D warnings`: clean (host).
- Same with `--target x86_64-pc-windows-gnu`: clean.
- `cargo check --workspace --all-targets`: clean (the new `Error` variant breaks no caller).
- `cargo test -p dl-http -p dl-agent-hotellink -p dl-agent-bin` (no gate): all passed.
  - dl-agent-hotellink lib: 199; lifecycle: 3.
  - dl-http: 46 unit + 21 + 5 + 18 doc/integration.
- Gated run (above): dl-agent-hotellink lib 199 passed, including 14 SQL and 3 e2e.
- New files: `cancel.rs` 210, `sqlclient.rs` 199, `e2e/mod.rs` 514, `e2e/tests.rs` 265 lines (all ≤ 800).
  - `dl-http/src/client.rs` was already 1068 lines before this work and is 1102 now. The cancel logic itself went into the new file.
- `services/agent-hub` not touched. No new crates. SQL in the tests is parameterized (`@p1`); the schema and seed are fixed literals.

## Windows / site only (not run)

- Shutting the socket down on Windows to wake a blocking `recv` (`shutdown(SD_BOTH)`) is compiled for `x86_64-pc-windows-gnu` and not run. dl-http's existing `ShutdownHandle` for upgraded sockets relies on the same call.
- A real service kill during an ack, and SQL Server 2005–2019 / remote SQL logins on the transport ladder, were not run; only SQL Server 2022 in Docker on Linux was.
- DPAPI-encrypted `connectionStringOverride` decryption on Windows (off Windows `enc:` is base64 passthrough).

## Docs

- `CHANGELOG.md` (Unreleased/Added): dl-agent-hotellink and dl-http entries.
- `crates/dl-agent-hotellink/README.md`: the connection-string rules, stop behaviour, breaker wait, and how to run the e2e.

## RT-0259 stream BK: dl-agent-backup report

Branch `cloud/rt-0259-bk`, worktree `/home/user/wt/bk`, commit `91da79b`. No push, no deploy, no other repo modified.

## Findings before the work
- The hub for the backup module is the **SRBackup hub** (`/home/user/SRBackup/hub`, Worker + D1), not `services/agent-hub` (it has no backup routes). So hub side = **spec only** (`docs/plans/2026-11-19-rt0259-backup-tenant-keys.md`), not implemented.
- INVENTORY §4 "SRBackup hands every agent the same master key" is stale for **credentials**: SRBackup `f9ffb01` already derives a per-agent key (`hub/src/lib/agentKey.ts`, HKDF(master, `agent:<id>`)), and `GET /api/agent/master-key` returns that. Agent module unchanged there.
- **Backup files were never encrypted** (Go agent and dl-agent-backup uploaded the plaintext `.bak`). So "old backups under the shared key" do not exist; the legacy format is plaintext, and that is what the fallback keeps restorable. INVENTORY updated with this status.

## What was built (agent side)
- `src/crypto/envelope.rs` (format `DLBKENC1`): header = magic, key id, 32-byte random salt, chunk size; file key = HKDF-SHA256(tenant data key, salt, `dl-agent-backup/file/v1` ‖ header); 1 MiB chunks AES-256-GCM, nonce = chunk index, AAD = final flag (cut/reordered/extended/altered file refused). Decrypt writes `<out>.part`, renames only after every chunk authenticates; on failure removes it (no plaintext left).
- `HubClient::backup_key()` (`GET /api/agent/backup-key`; 404 → `None` = older hub → plaintext upload as before) and `backup_key_by_id(id)` (`?id=`; 403/404 → `Error::Credential` "…another tenant…", never `Auth`; checks the returned id matches).
- `Core` implements `BackupKeys`: current key cached 15 min, prefetched on each config poll, last answer kept through a hub outage (no answer ever + hub down → backup fails and is retried); keys by id cached in memory.
- Pipeline `run_with(.., keys, ..)`: step `encrypt` (done/skipped/error) before upload; uploads `<name>.bak.dlbk`; staging file removed; `BackupResult.key_id`; report gets `encryptionKeyId` (`hub::with_key_id`).
- Restore `restore_from_record_with(.., RecordIo{uploaders, keys, backup_dir}, ..)`: after download, magic → decrypt with key the header names (step `decrypt`), plaintext (no magic) → restored as is (`decrypt` skipped). Old `run` / `restore_from_record` kept (use `NoKeys`).
- Docs: crate README (encryption + live test section), CHANGELOG [Unreleased], INVENTORY §4 status, hub spec doc.

## Acceptance items
1. **Live RESTORE WITH REPLACE** — `backup::sql_tests::replace::sealed_backup_restores_with_replace_and_to_a_new_name_with_matching_checksums` and `…::an_unencrypted_backup_from_before_tenant_keys_still_restores` (gated on existing `BACKUP_AGENT_TEST_SQL` + `BACKUP_AGENT_TEST_DIR`).
   Container `rt259-bk` (mssql/server:2022-latest, Developer 16.0, `TZ=Asia/Ho_Chi_Minh`, port 14362, bind mount `/tmp/rt259bk`, random SA password in scratch file never printed). Command:
   `BACKUP_AGENT_TEST_DIR=/tmp/rt259bk BACKUP_AGENT_TEST_SQL='server=127.0.0.1,14362;user id=sa;password=…;encrypt=disable' cargo test -p dl-agent-backup sql_tests -- --nocapture --test-threads=1`
   Result: 5 passed (3 existing + 2 new), 6.14 s. Real numbers:
   - before backup: customers rows=2000 checksum=1243671687; orders rows=10000 checksum=-1164360969; notes rows=500 checksum=521743882
   - backup: 708608-byte .bak (COMPRESSION, CHECKSUM), key tenant-a:1, 610 ms; sealed copy at LocalFolder destination 708679 bytes
   - after damage: customers 2001 / 364438108; orders 6667 / -773609569; notes dropped
   - restore with tenant B's keys: `backup_credential_error`, DB unchanged
   - after RESTORE WITH REPLACE (869 ms): identical to before (all three tables)
   - restore to new name (820 ms): identical to before; source untouched
   - legacy plaintext backup: REPLACE (380 ms) and new name (367 ms) identical to before
   Container and volume removed afterwards (`docker ps -a`/`docker volume ls` empty), `/tmp/rt259bk` deleted.
2. **Per-tenant keys** (unit/integration, fake hub enforcing tenancy):
   - Tenant A cannot decrypt tenant B: `tenant_keys_tests::tenant_a_cannot_restore_a_backup_sealed_for_tenant_b` (restore Failed, message names key and "another tenant", no RESTORE issued, no files left, `backup_credential_error`), `crypto::envelope::tests::another_tenants_agent_cannot_open_the_file` (also with A's bytes under B's id → "authentication failed", no plaintext).
   - Key ids recorded: `a_backup_is_sealed_under_the_tenants_key_and_the_report_names_it` (report `encryptionKeyId`, `.bak.dlbk`, destination never sees plaintext), header key id via `key_id_of`.
   - Legacy restores: `a_backup_from_before_encryption_still_restores`, `plaintext_backups_are_recognised_as_legacy`, live legacy test above; older hub: `an_older_hub_without_keys_gets_unencrypted_backups_as_before`.
   - Hub never returns another tenant's key: `the_hub_never_gives_this_agent_another_tenants_key` checks the agent contract (foreign id → Credential, not Auth) against the fake hub. **The real hub unit test cannot be written here** (hub is in SRBackup); exact vitest cases are in the spec doc.
   - Also: tamper cases (`altered_cut_or_extended_files_are_refused`), outage (`a_cached_key_carries_backups_through_a_hub_outage`), key fetch failure (`a_backup_whose_key_cannot_be_fetched_fails_before_anything_is_uploaded`).

## Gates
- `cargo fmt --all --check`: OK
- `cargo clippy -p dl-agent-backup --all-targets -- -D warnings`: OK; same with `--target x86_64-pc-windows-gnu`: OK
- `cargo test -p dl-agent-backup`: 262 passed, 0 failed (live tests skip without env); `cargo check -p dl-agent-bin`: OK
- services/agent-hub not touched (no pnpm run needed).

## Deviations / notes
- Hub side not implemented (other repo): spec `docs/plans/2026-11-19-rt0259-backup-tenant-keys.md` — D1 table `tenant_backup_keys` (random 32-byte key wrapped under ENCRYPTION_MASTER_KEY, `retired_at`), `backup_history.encryption_key_id`, `GET /api/agent/backup-key[?id=]` (404 for foreign ids, never 403), report field validation, rotation, vitest cases.
- Until the hub ships the endpoint, agents upload plaintext as before (404 fallback); no `require_encryption` setting was added.
- Software AES throughput ≈ 33 MiB/s each way (release, 64 MiB): ~5 min extra for a 10 GiB backup.
- Local `.bak` on the SQL Server machine stays plaintext (written by SQL Server); only the destination copy is sealed.
- No standalone decrypt tool for manual disaster recovery outside the agent (follow-up suggestion).
- Windows/site-only: DPAPI state, Windows-auth SQL, remote SQL Server download dirs and real cloud providers were not exercised live; Windows build checked by clippy only.

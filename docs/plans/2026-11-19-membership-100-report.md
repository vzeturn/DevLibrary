# dl-agent-membership 100% (RT-0256): report

Request: RT-0256 (part of RT-0164, shared agent). Branch `cloud/rt-0256`, built in three parallel streams
(`cloud/rt-0256-earn`, `-bed`, `-dpapi`) merged into it. Goal: close the gaps of
`2026-11-19-membership-writes-report.md` so the Rust module can replace the .NET `MM.Membership.Agent`
at Victory (VIC-SGN, VICVT) with `read_only: false`.

Status: **done on Linux / SQL Server 2022; DPAPI, the Windows service switch and real SMILE data still need
one check on Windows / the SR bench** (list below). Nothing was released or deployed; no hotel config changed.

## What is closed

| Gap | Rust | .NET source (ported from) |
|---|---|---|
| EarnPoints job (gap 1) | `src/earn/*`, `src/hub/earn.rs`, `SmileWriter::report_earn` | `Jobs/JobDispatcher.cs:521-552` `HandleEarnAsync`, `Smile/FolioWriter.Revenue.cs`, `FolioWriter.StayClassification.cs`, `FolioWriter.CheckoutTime.cs`, `Sync/EarnPairSource.cs`, `EarnPairReader.cs`, `Hub/HubClient.cs:90-160,226-238,322-341` |
| Checkout scanner + walk-in earn + pms-cancelled + summary | `src/checkout/{scanner,walk_in,sql,tally,settings}.rs`, wired in `module/scan.rs` | `Worker/CheckoutScanner.cs`, `CheckoutScanTally.cs`, `WalkInEarnState.cs`, `Smile/FolioWriter.cs:390-697`, `FolioWriter.UserInfo2.cs`, `Smile/WalkInCheckout.cs` |
| Walk-in backfill (keyset cursor) | `src/checkout/backfill.rs`, tool `dl-agent membership-backfill-walkin` | `Worker/WalkInBackfillPlan.cs`, `WalkInBackfillState.cs`, `WalkInBackfillRunner.cs`, `WalkInBackfillExit.cs` |
| SMILE_SPECIAL extra-bed rows (gap 2) in Create/Modify | `writer/` extra-bed special + preflight, `src/terms/*` | `Smile/FolioWriter.ExtraBedSpecial.cs`, `Smile/ExtraBedPreflight.cs`, `FolioWriter.ExtraBed.cs`, `Sync/ExtraBedTermsSync.cs`, `ExtraBedTermsReader.cs`, `SpecialServiceCatalogReader.cs` |
| Hub sync-config `ExtraBedSpecialCode` override on the write side | `module/state.rs` `hub_special_code` | `EffectiveExtraBedSpecialCode` |
| Switch from the .NET agent keeping `config.dpapi`, with rollback | `src/enroll/{switch,legacy,service}.rs`, `membership-enroll --switch` / `--rollback` | `Configuration/AgentSecureConfig.cs:27-110`, `AgentConfigReader.cs`, `PortableAgentConfigStore.cs`, `AgentSecretStore.cs`, `Cli/EnrollCommand.cs:47-56` |

Note on "cursor confirmed by the hub": the .NET checkout scanner has no cursor of its own. The hub names the
bookings still pending (`GET bookings/pending-checkout`) and only moves them to CHECKED_OUT after the earn
POST succeeds, so the hub's pending list is the confirmed cursor; walk-ins are remembered in memory like .NET.
The only persisted cursor is the backfill's keyset state file (saved only while no group failed). Ported as is.

## Parity evidence (golden)

- **Earn / checkout report bodies:** captured from real .NET: a net10.0 console referencing
  `MM.Membership.Application` serialized the DTOs with `HubJson.Options` → `tests/fixtures/golden/earn_reports.txt`
  (8 bodies); the Rust writer matches them byte for byte.
- **Golden SQL** (recorder, `GOLDEN_SQL_WRITE=1`, compared statement by statement with the .NET source):
  `sql/earn_job.json`, `sql/checkout_bookings.json`, `sql/checkout_walk_in.json`, and the extra-bed fixtures of the
  bed stream (create + modify with `Folio_Special`). The .NET SQL texts for earn/checkout were extracted from the
  C# raw string literals programmatically.
- Recorder unit tests pin order, applocks, refusal reasons and the extra-bed fold reasons.

Deliberate differences (details in the stream sections below): datetime parameters cast from datetime2
(`DECLARE @Since datetime = CAST(@SinceP AS datetime)`); `IN @List` expanded as `@Codes0..`; walk-in
exclusion decimals without pinned precision (plan cache only); `checkout_scan_enabled` defaults to
`!read_only` so a read-only module beside the live .NET service does not double-report; the checkout scan pauses
with the other scans while SMILE shows a different database than the pinned one.

## Tests run on the merged branch (cloud session, Linux)

| Command | Result |
|---|---|
| `cargo fmt --all --check` | ok |
| `cargo clippy -p dl-agent-membership -p dl-agent-bin --all-targets -- -D warnings` | ok |
| same with `--target x86_64-pc-windows-gnu` | ok |
| `cargo test -p dl-agent-membership` | ok: lib 207, checkout_hub 3, earn_hub 3, enroll 5, fake_hub 9, golden 9, jobs_hub 7, module_lifecycle 5, switch 2 (live tests skip without the env var) |
| `MEMBERSHIP_AGENT_TEST_SQL=… cargo test -p dl-agent-membership --test sql_live --test sql_live_writes --test sql_live_earn --test sql_live_extra_bed` against SQL Server 2022 (`mcr.microsoft.com/mssql/server:2022-latest`, `TZ=Asia/Ho_Chi_Minh`) | all 4 passed |

`sql_live` fails with a UTC container between 17:00 and 24:00 UTC (seeds use the server's `GETDATE()`, the
probes the hotel date UTC+7); it failed that way on base `a2a2fca` too. Real SMILE servers run in local time, so
the README now says to start the container with `TZ=Asia/Ho_Chi_Minh`. The container and its password file were
removed after the run.

## Only verifiable on Windows / the SR bench / at the hotel

- Real DPAPI unsealing of a Victory `config.dpapi` (tests use the plain test sealer with plaintext samples written
  exactly as .NET `JsonSerializer.SerializeToUtf8Bytes`); the SCM calls of `--switch` / `--rollback` (disable +
  stop `MM.Membership.Agent`, restore start type + start). Both compile for `x86_64-pc-windows-gnu`, neither ran.
- The token probe against the production hub and a full rollback restarting the .NET service.
- Real SMILE_FO shapes: `Folio_Special` columns (nSeq identity, `SPNotes`, `CancelNum`), AdditionalName
  `MobileNum` vs `TeleNum`, `RmChgTrn` values; that Victory's extra-bed terms hash to the fingerprint on the hub
  (otherwise SMILE_SPECIAL folds with `FINGERPRINT_DRIFT`, which is safe); night audit posting the special row.
- The hub's real 202-held bodies and pending-checkout volume; checkout summary numbers vs the .NET agent on the
  same day; the backfill tool under the service account.

## Cutover and rollback (Victory)

1. On the hotel PC: `dl-agent membership-enroll --switch` (keeps HubBaseUrl / token / HotelId /
   SmileConnectionString / OriginCode from the .NET `config.dpapi`; re-enrolls with a fresh code only if the hub
   rejects the token, still keeping the SMILE connection string; disables then stops `MM.Membership.Agent`;
   records `<data>/modules/membership/enrolled.json`, no secret). Idempotent: safe on every installer run.
2. Remote config: `modules.membership = {enabled: true, read_only: false, pull_jobs: true}`, keep
   `extra_bed_write_mode` as the .NET `Smile:ExtraBedWriteMode`, copy `default_payment_code`,
   `default_breakfast_code`, `member_discount_code`, `smile_hotel_id` if SMILE has several hotels.
3. Check the hub: heartbeat `dl-agent/…`, queue drains, acks Success, earn arriving, no `PMS_IDENTITY_MISMATCH`.
4. **Rollback:** remote config `read_only: true` (or `enabled: false`), then `dl-agent membership-enroll --rollback`
   (restores the .NET service start type and starts it; restores the `.bak-*` config unless the switch re-enrolled,
   in which case the current file — same format, valid token — is kept; `--force` restores anyway).

Still not ported, no cutover effect found: the earn-pair 24 h drift check (log only), TCVN3 decoding of legacy
text, `SmileDiagnostics`, the schema probe diagnostics, the inventory fallback.

---

# Stream reports (verbatim)


## RT-0256 stream EARN: EarnPoints job + checkout scanner (report)

Worktree `/home/user/wt/earn`, branch `cloud/rt-0256-earn`. Not pushed.
Status: **DONE_WITH_CONCERNS** (concerns at the end).

## Commits
| SHA | What |
|---|---|
| `5319e79` | feat: EarnPoints job (`HandleEarnAsync`) + hub earn routes + earn bodies |
| `cb742d2` | feat: checkout scanner, walk-in earn, PMS-cancelled, summary, walk-in backfill tool, README/CHANGELOG |

## What was ported (.NET -> Rust)
Part 1 – EarnPoints
- `Jobs/JobDispatcher.cs:521-552` `HandleEarnAsync` -> `src/earn/job.rs::read` + `src/writer/smile_writer.rs` (`SmileWriter::report_earn`, `EarnPath::report`). SMILE reads run inside the writer session, the POST outside it. Ack semantics unchanged in `jobs/dispatch.rs` (Ok -> `Ack::ok`); `dispatch.rs` was NOT edited.
- Ambiguous room charge -> `WriteError::Coded{code:"UNKNOWN", message:"PMS_EARN_ROOM_CODE_AMBIGUOUS: không xác định được mã tiền phòng SMILE — chưa tích điểm {ext}."}` (the .NET `InvalidOperationException` classifies as UNKNOWN; summary = message).
- `Sync/EarnPairSource.cs` `ResolveAsync` -> `src/earn/pairs.rs` (override warning, ambiguous error -> None, log-on-change; drift check still not ported, as before). `EarnPairReader.ReadRoomChargeTrnAsync` (Bookings/WalkIns scopes, 500-chunks) -> `src/earn/sql.rs::room_charge_trn`. Catalog read refactored to `smile/folio_sql.rs::{CATALOG_SQL, catalog_params, catalog_from_sets}` (shared with the folio scanner).
- `Smile/FolioWriter.Revenue.cs` (`RevenueSumSql`, `RevenueLinesSql`, Scan/Sum for booking + walk-in scopes), `Smile/FolioRevenueScan.cs` (`ReadBookingRevenueAsync` fallback) -> `src/earn/sql.rs`.
- `Smile/FolioWriter.StayClassification.cs`, `Smile/FolioWriter.CheckoutTime.cs` (`ReadCheckedOutAtAsync`, `EarnCheckoutTime.Resolve`) -> `src/earn/sql.rs::{classification, checked_out_at, resolve_checkout_time}`.
- `Hub/HubClient.cs:127-160,90-125,226-238,322-341` (`ReportEarnAsync` 204/202-held, `ReportWalkInEarnAsync`, `GetPendingCheckoutAsync`, `ReportPmsCancelledAsync` null on 401/404/405, `ReportFolioRevenueAsync`, `ReportCheckoutScanSummaryAsync`, `HeldAsync`) -> `src/hub/earn.rs` (`EarnHub` trait, impl for `HubClient`; `hub/mod.rs` only got `pub mod earn;` + doc line).
- DTOs `Application/Sync/DTOs/AgentDtos.cs` (AgentEarnReport, AgentWalkInEarnReport/Result/Outcome, AgentPendingBooking, AgentPmsCancelReport/Result, AgentFolioRevenueReport/Line, AgentCheckoutScanSummaryReport), `AgentStayClassification` -> `src/earn/wire.rs`. `stj.rs` gained `J::Map` (dictionary with run-time keys, for `SkipReasons`).
- Fingerprint: .NET HubClient stamps `identity?.CurrentFingerprint`; here callers stamp it from `EarnEnv::fingerprint()` (`earn::ModuleEnv` = identity tracker + last sync-config, which also gives the hub's `ExtraBedSpecialCode` = `EffectiveExtraBedSpecialCode`).

Part 2 – Checkout scanner (all ported, walk-in included)
- `Worker/CheckoutScanner.cs` (ScanOnce/ScanCore, booking loop, TrackRevenue/zero alarm, PushSummary, ReportPmsCancelled, ScanWalkIns, ToRevenueLines, ReportFolioRevenue, MarkWalkInSeen/ForgetWalkInsBefore/BuildWalkInExclusion) -> `src/checkout/scanner.rs`, `src/checkout/walk_in.rs`.
- `CheckoutScanOptions` -> `src/checkout/settings.rs` (keys `checkout_scan_enabled`, `checkout_interval_seconds`=60, `checkout_max_per_scan`=100, `checkout_grace_days`=1 (0 allowed), `walk_in_earn_enabled`=true, `walk_in_lookback_days`=3, `walk_in_max_per_scan`=100; phone column = existing `guest_phone_column`). One field added to `settings.rs`.
- `Worker/CheckoutScanTally.cs` -> `src/checkout/tally.rs`; `Worker/WalkInEarnState.cs` -> `walk_in.rs` (shared via `module/state.rs` `Shared.walk_in`; `sync.rs` heartbeat now sends `WalkInEarnOk`).
- `Smile/FolioWriter.cs:390-697` (`ScanCheckedOutAsync`, `ScanCancelledAsync`, `ScanWalkInCheckoutsAsync`, `BuildPaddedExclusion`, `ScanWalkInCheckoutsPageAsync`, `SafeIdentifier`), `FolioWriter.UserInfo2.cs` `ParseUserInfo2`, `Smile/WalkInCheckout.cs` -> `src/checkout/sql.rs`.
- Wiring: `module/scan.rs` gets `CheckoutPass` + `ScanLoop::with_checkout` (own interval, `record_scan("checkout")`, failures via the existing `failed()` path); `module/mod.rs` builds it with its own `TdsSmile` when `checkout_scan_enabled`.
- `Worker/WalkInBackfillPlan.cs`, `WalkInBackfillState.cs`, `WalkInBackfillRunner.cs`, `WalkInBackfillExit.cs` -> `src/checkout/backfill.rs` + tool `dl-agent membership-backfill-walkin` (`src/checkout/tool.rs`, registered in `crates/dl-agent-bin/src/main.rs`, 4 lines). Same messages, 400-day cap, page clamp 1..500, cursor saved only while no group failed, state JSON field names as .NET, non-zero exit unless clean.

Note on the brief: the .NET checkout scanner has NO cursor (the hub names pending bookings; walk-ins are remembered in memory only). The only cursor is the backfill's keyset state file. Ported as such.

## Parity evidence
1. **Report JSON from real .NET**: I built a net10.0 console referencing `MM.Membership.Application` and serialized the actual DTOs with `HubJson.Options` (PascalCase, nulls written, WireEnumConverterFactory). Output saved as `tests/fixtures/golden/earn_reports.txt` (8 bodies: earn full/min, walk-in full/min, folio-revenue, pms-cancel, summary full/min). `earn::tests::report_bodies_match_the_dotnet_export` matches all 8 byte for byte. The same program confirmed walk-in result parsing (`"Parked"`, `"AWARDED"`, `4`).
2. **Golden SQL** (recorder, `GOLDEN_SQL_WRITE=1`): `sql/earn_job.json`, `sql/checkout_bookings.json`, `sql/checkout_walk_in.json`. The .NET SQL texts were extracted programmatically from the C# raw string literals (dedented as the compiler does) and compared statement by statement with: `FolioWriter.Revenue.cs` (RevenueLinesSql/RevenueSumSql + CheckedOutChargeFilter), `FolioWriter.StayClassification.cs`, `FolioWriter.CheckoutTime.cs`, `EarnPairReader.cs` (QueryRmAsync, CatalogSql), `FolioWriter.cs` (ScanCheckedOut, ScanCancelled, ScanWalkInCheckouts, page query). Parameter order and types match Dapper (nvarchar for strings, decimal, varchar for ExtraBedCode/EarnC/EarnS). Differences listed below.
3. **Live SQL Server 2022** (`tests/sql_live_earn.rs`, container `rt256-earn`, port 14341): seeds TransactionCode, 2-room checked-out booking (+ a `'B'` line that must not count, + minibar not in pairs), a cancelled booking, a walk-in group with phone on TeleNum only. Runs the EarnPoints job through the real `SmileWriter` + `SyncWorker` + fake hub (ack Success, revenue `1500000.00`, 3 lines, checkout `2026-11-18T04:05:06+00:00` PMS_CHECKOUT_TIME), the checkout scan (booking earn, walk-in `CONF:9001` 400000.00 at +07:00, pms-cancelled MM-LIVE-C, second scan excludes 9001 in SQL), and the backfill (1 group, clean). PASSED.

## Deliberate differences
- `datetime` parameters: dl_tds binds datetime2, so the walk-in and page queries start with `DECLARE @Since datetime = CAST(@SinceP AS datetime);` (same pattern as the existing folio scan). Statement text differs by that prefix only.
- `IN @List` expanded by hand as `@Codes0..`/`@Keys0..` (Dapper uses `@Codes1..`); walk-in exclusion params are plain `decimal` (dl_tds picks precision), .NET pins `decimal(18,0)` (plan-cache only).
- Checkout scan default: .NET always runs it; here `checkout_scan_enabled` defaults to `!read_only` so a read-only module beside the live .NET service does not double-report. Explicit `true/false` wins.
- The checkout scan runs on the scan thread, so it pauses with the other scans while SMILE shows another database than the pinned one (.NET keeps reporting and the hub holds with 202). Nothing is lost: the hub keeps the bookings pending.
- `ExtraBedTermsSync` (runs in the .NET scan's finally block) is NOT ported (separate gap, other stream/later).
- Backfill tool cannot read the remote module config: phone column / extra-bed code / discount code / charge codes are CLI options with the .NET defaults.
- Per-booking failures are logged with the error text (Rust has no exception object); no PII beyond what .NET logs (booking id, code, folio ref).

## Tests and gates (CARGO_TARGET_DIR=/home/user/wt/target-earn)
| Command | Result |
|---|---|
| `cargo fmt --all --check` | ok |
| `cargo clippy -p dl-agent-membership --all-targets -- -D warnings` | ok |
| `cargo clippy -p dl-agent-membership --all-targets --target x86_64-pc-windows-gnu -- -D warnings` | ok |
| `cargo clippy -p dl-agent-bin -- -D warnings` | ok |
| `cargo test -p dl-agent-membership` | all ok: lib 146, checkout_hub 3, earn_hub 3, enroll 5, fake_hub 9, golden 9, jobs_hub 5, module_lifecycle 5, sql_live*/ skipped without env |
| `MEMBERSHIP_AGENT_TEST_SQL=... cargo test -p dl-agent-membership --test sql_live_earn --test sql_live_writes` | both PASSED against SQL Server 2022 |
| same with `--test sql_live` | FAILS at `tests/sql_live.rs:188` (`probe.folio_count >= 2`, got 1). Pre-existing: reproduced identically on the base commit `a2a2fca` in a separate worktree; looks time-of-day dependent (occupancy window vs seed dates); not touched by this stream. |

New tests: `earn::tests` (5 incl. .NET JSON golden + golden SQL), `earn::wire::tests` (2), `checkout::tests` (7 + golden SQL), `checkout::tally`, `checkout::walk_in` (2), `checkout::settings`, `checkout::backfill::tests` (4), `tests/earn_hub.rs` (pull EarnPoints -> exact earn body -> ack Success; 202 held -> Success; 500 -> NETWORK/TRANSIENT), `tests/checkout_hub.rs` (3: full scan over real HubClient, older hub 404/405/401 on pms-cancelled, failed pending-checkout touches no SMILE), `tests/sql_live_earn.rs`.

Docker: container `rt256-earn` created with a random SA password in a scratch file (never printed), removed after the run; password file deleted. (An anonymous data volume of that container may remain; I did not touch other Docker resources.)

## Only verifiable on Windows / at the hotel
- Real SMILE_FO data shapes (Victory: AdditionalName MobileNum vs TeleNum, RmChgTrn values, AutoPost uniqueness), the hub's real responses (202 held bodies, pending-checkout volume), the backfill tool under the Windows service account (DPAPI config.dpapi read, ProgramData write), and parity of the checkout summary numbers vs. the .NET agent on the same day. Run one site side by side only with the .NET agent stopped (or `checkout_scan_enabled:false` on the Rust side) to avoid double reports.

## Concerns
- Shared files touched (small, additive): `hub/mod.rs`, `lib.rs`, `settings.rs` (+1 field), `module/state.rs` (+1 field), `module/scan.rs`, `module/mod.rs` (hub client kept as `Arc<HubClient>`, `dispatcher()` signature +1 param, `checkout_pass`, `earn_settings`), `sync.rs` (1 line), `wire/mod.rs` (doc), `stj.rs` (`J::Map`), `writer/session.rs` (`Rec::opt_datetime`, `Rec::first`), `smile/mod.rs` (`folio_sql` now `pub(crate)`), `CHANGELOG.md`, README, `dl-agent-bin/src/main.rs`. Expect trivial merge conflicts with other streams in `module/mod.rs` / `settings.rs`.
- `sql_live` pre-existing failure above.
- ExtraBedTermsSync not ported; EarnPair drift check not ported (unchanged).

## Stream BED report: SMILE_SPECIAL extra-bed rows (RT-0256)

Worktree `/home/user/wt/bed`, branch `cloud/rt-0256-bed`. Nothing pushed.

Status: **DONE_WITH_CONCERNS** (concerns at the end; none blocks the extra-bed path).

## Commits

| SHA | What |
|---|---|
| `cf199db` | SMILE_SPECIAL rows on create/modify: preflight, terms reader, Folio_Special reconcile, the hub code override, goldens, recorder/dispatcher/fake-hub/live tests |
| `5abe81b` | Terms sync and SpecialService catalogue (hub reports), README, CHANGELOG |

## What was ported (.NET → Rust)

| .NET (MM-MEMBERSHIP `src/MM.Membership.Agent/`) | Rust (`crates/dl-agent-membership/src/`) |
|---|---|
| `Smile/ExtraBedPreflight.cs` (`SmileSpecialPrice.From` :172-178, `ExtraBedPreflightInputs` :185, `Decide` :195-234, `Check` :237-254, `Bands` :261-285) | `jobs/preflight.rs` (`special_price`, `PreflightInputs`, `decide`, `check`, `bands`) |
| `Smile/FolioWriter.ExtraBedSpecial.cs` `DecideExtraBed` :38 | `jobs/preflight.rs::decide_extra_bed` |
| `FolioWriter.ExtraBedSpecial.cs` `ReadExtraBedInputsAsync` :41-66 | `writer/special_rows.rs::read_extra_bed_inputs` (+ `PmsWriter::read_extra_bed_inputs`, default `Ok(None)` like the .NET interface default) |
| `FolioWriter.ExtraBedSpecial.cs` `WriteMmBedRowsInTxAsync` :73-131, `NSeqLiteralAsync` :137-147 | `writer/special_rows.rs::write_mm_bed_rows_in_tx`, `nseq_literal` (replaces the fold-only version that returned `NotPorted`) |
| `FolioWriter.ExtraBed.cs` `ExtraBedWriteDecision` :23-36 (`SpecialCode`, `NativeSurchargeActive`), `SpecialRowMarker` :191 | `jobs/beds.rs` (`BedDecision` fields, fold reason constants), `writer/special_rows.rs::special_row_marker` |
| `FolioWriter.Reservation.cs` :115-116 (create writes rows after rates), :291-293 (update) ; `FolioWriter.Atomic.cs` :106 (`Decide` with `plan.ExtraBed`) | `writer/folio.rs` (`write_booking` decides with `plan.extra_bed`; `create_in_tx` writes the rows) ; `jobs/plan.rs` `BookingPlan.extra_bed` |
| `Jobs/JobDispatcher.cs` :145-151 (create), :177-215 (modify), `FolioMap`/`Decide` :656-669 | `jobs/dispatch.rs` (`bed_inputs`, `decide(room, inputs)` with `FOLD_LEGACY`, `folio_map` takes inputs) |
| `Configuration/SmileWriteOptions.cs` `HubExtraBedSpecialCode` / `SetHubExtraBedSpecialCode` / `EffectiveExtraBedSpecialCode` :100-118, `SmileHotelId` :78 | `writer/special_rows.rs::HubSpecialCode`, `FolioWriter::effective_special_code`; `module/state.rs::Shared.hub_special_code` set from every sync-config; `settings.rs` `smile_hotel_id`; `FolioWriter.ReadSpecialBedRowsAsync` foreign code now uses the effective code (`writer/lookup.rs`) |
| `Sync/ExtraBedTerms.cs` (record, `SpecialUsable`, `SpecialPriceable`, `AsPmsPriced`, `ExtraBedTermsHashing` all three hashes) | `jobs/bed_terms.rs` |
| `Sync/ExtraBedTermsReader.cs` (all SQL constants, `ReadAsync`, `ParseSurcharge`, stock, special, tax codes, rates) | `writer/bed_terms_reader.rs` (SQL text byte-for-byte with the C# raw-literal indentation removed; `IN @Names` expanded like Dapper) |
| `Sync/ExtraBedTermsSync.cs` + `.Catalog.cs` (`RunIfDueAsync`, `DecideMode`, `SpecialEarnCovered`, catalogue report) | `terms/mod.rs` (`TermsSync::run_if_due`, `decide_mode`, `special_earn_covered`, `TermsRequest`/`CatalogRequest` bodies) |
| `Sync/SpecialServiceCatalogReader.cs` (reader, `SpecialServiceBedPrice`, `SpecialServiceCatalogHashing`) | `terms/catalog.rs` |
| `HubClient.ReportExtraBedTermsAsync` / `ReportSpecialServicesAsync` (404/405 ⇒ false) | `hub/mod.rs` `HubApi::post_extra_bed_terms` / `post_special_services` (trait defaults `Ok(false)`, `HubClient::post_optional`) |

Small fix found on the way: `ColumnList::has_param` compared against `@name` but `dl_tds::Param` stores names without `@`, so it never matched (it had no caller before). Fixed in `writer/schema.rs`.

Ack classes: a missing/too-narrow `SPNotes` with rows wanted is `InvalidOperationException` in .NET (classifier ⇒ `UNKNOWN` / Unknown); Rust returns `WriteError::Coded{UNKNOWN}` ⇒ same code and class. All preflight failures fold (no failure ack), as in .NET. `EXTRA_BED_LOCKED` (locked folio with special bed) was already ported in room_diff and is now reachable because decisions can be special.

## Parity evidence

1. **Golden SQL** (`tests/fixtures/golden/sql/`, generated with `GOLDEN_SQL_WRITE=1` then compared by hand):
   - `create_booking_extra_bed.json` (32 statements): `ReadExtraBedInputsAsync` (Folio_Special probe, `sys.columns`, HotelID probe, parameters with `IN (@Names1..6)`, stock, special, code tax, room tax, hotel date, SC and VAT rates — order of `ExtraBedTermsReader.ReadAsync` :151-197), then `WriteBookingAsync` (probes, lock, replay lookup, duplicate guard, counters, Folio INSERT with `RateAmount = NetExclBed` and `ExtraBed = 1`, FolioRate × 2 at NetExclBed, the UPDLOCK select, `COLUMNPROPERTY` nSeq probe, `INSERT INTO Folio_Special (FolioNum, SpecialCode, Qty, Price, FromDay, ToDay, SPNotes, CancelNum)` with AnsiString code/marker, Decimal price 453600, Int32 qty/days, then AdditionalName, COMMIT). Compared with `FolioWriter.Reservation.cs` :108-125, `FolioWriter.ExtraBedSpecial.cs` :88-130, `ExtraBedTermsReader.cs` :55-210.
   - `modify_booking_extra_bed.json` (33 statements): UPDATE Folio, FolioRate rewrite, AdditionalName, select of the agent rows, `DELETE ... SPNotes LIKE @Marker`, nSeq probe, INSERT, note read. Compared with `FolioWriter.Reservation.cs` :248-293 and `ExtraBedSpecial.cs` :99-130.
   - Caveat as before: hand-verified against .NET source, not captured from .NET traffic.
2. **Fingerprint and hashes computed outside Rust**: Python `hashlib.sha256` over the `extra-bed-fingerprint:v3` / `extra-bed-content:v2` / `special-services:v1` canonical texts (written from `ExtraBedTermsHashing` / `SpecialServiceCatalogHashing`). The Rust reader reproduces them in the recorder test and, for the fingerprint, **live against SQL Server** with the .NET `SmileFixture` Victory seed (`872409c5…1d33`). This is the value a hub's verified fingerprint is compared with, so a mismatch would fold every SMILE_SPECIAL write.
3. **Unit tests mirroring .NET tests**: every case of `ExtraBedPreflightTests.cs` (price gross-up table incl. 333,333 → 378,000, bands, short per-night list, rule 13, no beds, the 12 failing-check rows, case/space-insensitive match, PMS-priced pricing/no-proof/no-rates/fingerprint structure, agent-priced amount coverage); recorder tests for rows already as wanted (no write), cancelled or differently coded rows replaced, non-identity nSeq literal, FOLD_LEGACY reads nothing and deletes agent rows, Folio_Special without the columns ⇒ `FINGERPRINT_DRIFT`, SPNotes missing ⇒ `UNKNOWN`, payload/hub code ⇒ PMS-priced terms for that code; terms reader skip reasons, surcharge parsing, rates only on "++".
4. **Dispatcher / fake hub**: `jobs/dispatch_bed_tests.rs` (inputs read with the payload fingerprint and code, carried by the plan, ack `ExtraBedWritten`/`ExtraBedSpecialQtyWritten`, unchanged modify reports `…Found`, a switch to fold rewrites); `tests/jobs_hub.rs` over the real `HubClient` (a SMILE_SPECIAL create acked with the special quantity; sync-config `ExtraBedSpecialCode` reaches `Shared.hub_special_code`; `pms/extra-bed-terms` 404 ⇒ false, 204 ⇒ true, 500 ⇒ error); `terms/terms_tests.rs` (300 s throttle, post only on change, full snapshot after 24 h, hub pick ⇒ PMS-priced repost, older hub and SMILE failure never fail the cycle, body property names).
5. **Live SQL Server 2022** (`tests/sql_live_extra_bed.rs`, new; mirrors `FolioWriterExtraBedSpecialTests`): Victory terms seeded like `SmileFixture.SeedExtraBedSpecialAsync` + `SeedSmileRatesAsync` (TaxDef/ServiceCharge created in the test, as .NET does; `smile_schema.sql` already had `Folio_Special`, `SpecialService`, `GlobalParameter`, `ExtraBedInv`, `HotelParameter`, `TransactionCode` with tax columns — nothing to add). Checks: live fingerprint, catalogue read; create ⇒ one agent row (EB, 1, 453600, 1–2, CancelNum 0, SalesPerson NULL), `Folio.ExtraBed = 1`, rates 1,300,000, no note; replay create and unchanged modify keep the same single row (same nSeq); a front-desk EB row survives a 2-bed modify that rewrites only the agent row (found = 2); switch to FOLD_LEGACY deletes the agent row, keeps the desk row, writes the bed note and ExtraBed 0; cancel.

## Deliberate differences

- `RefreshHubSpecialCodeAsync` is not duplicated inside the terms sync: the sync worker already reads sync-config every 60 s (.NET: every 300 s from the terms cycle) and sets `HubSpecialCode`; it keeps the last value on a failed read, like .NET.
- The terms/catalogue reports run on the sync thread and **only with writes on** (`read_only: false`), so a shadow Rust agent next to the live .NET one does not post terms derived from a different config (mode/earn codes would flap the wire hash). .NET rides the checkout scanner cycle, which is not ported. They also pause while scans are paused (wrong database).
- `special_price` / `bed_price` use checked arithmetic: an overflow (impossible for real VND amounts) folds as `SPECIAL_UNUSABLE` / returns no price instead of throwing.
- `DispatchSettings.extra_bed_mode` is now unused by the dispatcher (it decides with `FOLD_LEGACY` when there are no inputs, as `JobDispatcher.Decide` does); left in place to keep the shared struct stable.
- The fake `Pms` in `jobs/fakes.rs` no longer mentions SMILE_SPECIAL in its `NotPorted` message ("an unported write"); it gained `bed_inputs`/`last_plan`. The fake `Hub` gained `pms_posts`/`has_pms_endpoints`.

## Tests and gates (CARGO_TARGET_DIR=/home/user/wt/target-bed)

| Command | Result |
|---|---|
| `cargo fmt --all --check` | exit 0 |
| `cargo clippy -p dl-agent-membership --all-targets -- -D warnings` | exit 0 |
| `cargo clippy -p dl-agent-membership --all-targets --target x86_64-pc-windows-gnu -- -D warnings` | exit 0 |
| `cargo test -p dl-agent-membership` | 205 passed, 0 failed (lib 167, enroll 5, fake_hub 9, golden 9, jobs_hub 7, module_lifecycle 5, sql_live 1, sql_live_extra_bed 1, sql_live_writes 1 — the three live ones skip without the env var) |
| `MEMBERSHIP_AGENT_TEST_SQL='Data Source=127.0.0.1,14342;User ID=sa;Password=…;Encrypt=disable' cargo test -p dl-agent-membership --test sql_live_extra_bed --test sql_live_writes` | both passed |
| same with `--test sql_live` | **FAILED** at `tests/sql_live.rs:188` (`probe.folio_count >= 2`, got 1). Pre-existing: it fails identically on the base commit `a2a2fca` with my changes stashed. Read side, not touched by this stream (looks date/time-of-day dependent: `hotel_now()` vs the seeded stays). |

Live runs used my own container `rt256-bed` (`mcr.microsoft.com/mssql/server:2022-latest`, port 14342, random SA password in a scratch file, never printed). Container removed and password file deleted afterwards.

## Only verifiable on Windows / at the hotel

- The real SMILE_FO `Folio_Special` schema (nSeq identity or not, `SPNotes` width/type, `CancelNum`) — the fixture columns follow the .NET spec, which itself says "schema SMILE thật chưa đối chiếu". Both nSeq branches are covered by the recorder.
- That Victory's live terms hash to the fingerprint staff recorded on the hub (the VM proof); otherwise SMILE_SPECIAL folds with `FINGERPRINT_DRIFT` (safe).
- Night audit posting the agent row (`NAPostSpecial_Service`) — SMILE behaviour, not testable here.
- `TRY_CONVERT` in the reader needs SQL Server 2012+ (same as .NET).

## Concerns

- `sql_live` read test fails on the base commit too (see above); someone owning the read side should look.
- Shared files touched (small, additive): `jobs/dispatch.rs` (inputs + `#[path] mod bed_tests`), `settings.rs` (`smile_hotel_id`), `lib.rs` (`pub mod terms`), `hub/mod.rs` (two trait methods with defaults + `post_optional`), `module/mod.rs`, `module/state.rs`, `sync.rs` (`with_terms`), `README.md`, `CHANGELOG.md`, `jobs/fakes.rs`, `tests/jobs_hub.rs`. `wire/mod.rs` untouched (bodies live in `terms/`).
- Earn reporting is still the other cutover blocker (not this stream).

## Stream DPAPI report (RT-0256): switch from the .NET agent keeping config.dpapi, with rollback

Status: DONE_WITH_CONCERNS (only concern: real DPAPI and the SCM calls can only run on Windows; they are compile-checked, not run)
Branch: cloud/rt-0256-dpapi, worktree /home/user/wt/dpapi, commit 94a369b

## Places the .NET agent keeps its config (audit) -> Rust
| .NET source | Format | Rust |
|---|---|---|
| Configuration/AgentSecureConfig.cs:27-110 (`BlobPath`, `TryReadBlob`, `WriteBlobCore`, `Save` .bak-*) | `%ProgramData%\MM.Membership.Agent\config.dpapi`, DPAPI LocalMachine, entropy `MM.Membership.Agent.v1`, STJ JSON of `AgentConfig` | already in `src/config.rs` (unchanged); new parity tests in `src/enroll/legacy_tests.rs` |
| AgentSecureConfig.Load:34-48 + AgentConfigReader.cs:23-45 + Program.cs:20-41 (content root = exe folder; appsettings.json < appsettings.{Env}.json < env < appsettings.hotel.json < env) | seed `"Agent"` section, `ConnectionStrings:SmileFo` fallback only when the key is absent (`??`), TrimEnd('/'), token Trim, OriginCode default "Membership", bad GUID = error | `src/enroll/legacy.rs` `Section`, `read_seed`, `Places::detect` (service exe folder from the SCM, then `%ProgramFiles%\MM.Membership.Agent`; `DOTNET_ENVIRONMENT` default Production). Case-insensitive keys, comments and trailing commas accepted (`strip_jsonc`) like JsonConfigurationProvider |
| PortableAgentConfigStore.cs (File mode, Linux/macOS only; AgentSecretStore.cs `DefaultPortablePath`, `Agent:SecretStorePath`) | plain `{"Agent":{...}}` (WriteIndented), used alone | `read_seed` with `Places.portable` (off Windows only, like `AgentSecretStoreSelector.Resolve`) |
| Env mode (AgentSecretStore.cs) | `Agent__*` env only | the env layer of `read_seed` |
| PreHostConfigStoreFactory.cs | selects the above | n/a (covered) |
| Cli/EnrollCommand.cs:47-56 (keep SmileConnectionString when re-enrolling) | | `enroll_and_save` (extracted from `run_enroll` in `src/enroll/mod.rs`) + switch keeps the old SMILE string |
No older format exists: MM-MEMBERSHIP history is squashed, only config.dpapi is ever written; `secrets.dpapi` appears only in an install.ps1 `-Purge` help comment, no code writes it.

## What was added
- `src/enroll/legacy.rs` (+ `legacy_tests.rs`): seed/portable discovery described above, `exe_dir` (service command line -> install folder).
- `src/enroll/service.rs`: `LegacyService` trait; `Scm` = `#[cfg(windows)]` dl_winsvc (open `MM.Membership.Agent`; set Disabled first so the .NET failure-restart actions cannot bring it back, then `stop_and_wait` 60 s; rollback sets the recorded start type and starts it unless disabled), `#[cfg(not(windows))]` stub (NotWindows). Same dl_winsvc::scm calls as dl-agent cli_install_win/cli_uninstall.
- `src/enroll/switch.rs` (+ `switch_tests.rs`): `run_switch`, `run_rollback`, `hub_probe` (GET api/agent/sync-config via HubClient), record `<data>/modules/membership/enrolled.json` (state, hotel_id, source, config_path, rewrote, reenrolled, backup, service, service_was_running, service_start_before, at; no secret).
- `src/enroll/tool.rs`: `--switch`, `--rollback` (mutually exclusive), `--hub-url` now required only for the plain enrolment.
- tests/switch.rs (fake hub), README "Switching from the .NET agent" + "Rollback", CHANGELOG Unreleased line.

## Switch behaviour
1. record says rolled_back and no --force -> nothing. Record says switched for the hotel config.dpapi holds -> keep, only re-ensure the service is stopped/disabled (no probe, record not rewritten so the original start type/backup stay).
2. config.dpapi readable -> kept byte-for-byte. Missing -> seed config sealed into config.dpapi (what .NET's first start does). Unreadable -> enroll needed (SMILE hint from the seed).
3. Probe token: ok -> keep; `AGENT_TOKEN_REJECTED` -> enroll with code (DL_AGENT_MEMBERSHIP_TOKEN/stdin) keeping the old SMILE connection (old file > DL_AGENT_MEMBERSHIP_SMILE > seed), backup `.bak-*` recorded; other hub error -> keep with a warning. Missing code -> error before anything changes (service untouched, no record).
4. Stop + disable service; failure -> error after recording ("stop it by hand before turning writes on").
5. Warns (no change) when the .NET config names another hub origin than --hub-url.

## Rollback
Restores start type + starts `MM.Membership.Agent`; restores the `.bak-*` (current file kept as `config.dpapi.switch-<stamp>Z`) — but when the switch re-enrolled, the backup's token was replaced at the hub, so by default it keeps the current file (same format, valid token, readable by .NET) and says so; `--force` restores anyway. Marks the record rolled_back and prints the remote-config step (`read_only: true` or `enabled: false`), which the tool cannot do itself.

## Deliberate differences
- Blob property names matched case-insensitively and missing OriginCode -> "" (STJ is case-sensitive; files are always written by STJ PascalCase, so harmless).
- Seed JSON null values are treated as absent.
- The portable store is read whenever its file exists off Windows (.NET reads it only with Agent:SecretStore=File).

## Parity evidence
- `reads_and_writes_the_dotnet_blob_byte_for_byte`: STJ default-encoder plaintext (escaped `+ & ' Đ`, `\\`, `/` kept) loads and `AgentConfig::to_json` reproduces it byte for byte (compared with AgentSecureConfig.WriteBlobCore `JsonSerializer.SerializeToUtf8Bytes(cfg)` and the record AgentConfig declaration order, AgentConfigReader.cs:4-9).
- other order + unknown properties + BOM + upper-case GUID + no OriginCode; packaged appsettings.json (empty strings) is not a config; hotel file with comments/trailing commas/any-case keys normalised like AgentConfigReader.cs:39-44; `""` hides SmileFo while an absent key falls back; env wins; bad GUID error without the value; portable WriteIndented file (PortableAgentConfigStore.Save) read alone.
- No golden SQL fixtures apply (no SQL in this stream).

## Tests (CARGO_TARGET_DIR=/home/user/wt/target-dpapi, in /home/user/wt/dpapi)
- `cargo fmt --all --check` -> ok
- `cargo clippy -p dl-agent-membership --all-targets -- -D warnings` -> ok
- `cargo clippy -p dl-agent-membership --all-targets --target x86_64-pc-windows-gnu -- -D warnings` -> ok (compiles the `#[cfg(windows)]` SCM code)
- `cargo test -p dl-agent-membership` -> all pass: lib 140 (incl. 10 legacy + 7 switch new), integration binaries all ok incl. new tests/switch.rs 2/2. No failures.
- SQL live test / docker: not needed for this stream (no SQL); not run.

## Only verifiable on Windows / at the hotel
- Real DPAPI unseal of the hotel's config.dpapi (LocalMachine + entropy) from the dl-agent service account.
- SCM: stop/disable/restore/start of `MM.Membership.Agent` (needs elevation; dl-agent runs as LocalSystem), and `exe_dir` from the real service binPath.
- The token probe against the production hub (AGENT_TOKEN_REJECTED header) and an end-to-end rollback restarting the .NET service.

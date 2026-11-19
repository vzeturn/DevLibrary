# dl-agent core 100% for SMILE (RT-0258): report

Request: RT-0258 (task 645 of RT-0164). Branch `cloud/rt-0258`, built in two streams (`-tds`, `-core`) merged into
it. Each item below first audits what already existed, then lists what was added. Nothing was deployed.

## Acceptance → evidence (cloud session, Linux)

| Criterion | Test (command) | Result |
|---|---|---|
| `Server=HOST\INSTANCE` connects through a fake SQL Browser | `dl-tds` `client_legacy_tests` (`cargo test -p dl-tds --lib legacy`): fake UDP browser answers `INST` → connects to the fake TDS server on that port and runs login + batch; explicit `\INST,port` skips the browser; an unknown instance is a browser error before any TCP | pass |
| Windows login creates the correct NTLM NEGOTIATE / AUTHENTICATE messages (standard vectors) | `dl-tds` `ntlm_tests::ms_nlmp_4_2_4_ntlmv2_vectors` (MS-NLMP §4.2.4 NTOWFv2, NTProofStr, SessionBaseKey, LMv2 byte for byte), `negotiate_message_layout`, MIC tests; end to end `domain_user_logs_in_with_ntlmv2_and_a_mic` on the fake TDS server; `dl-hash` MD4 RFC 1320 vectors | pass |
| TDS 7.3 login + TLS 1.0 through a fake TDS server | `sql_server_2008_r2_answers_tds_73b`, `sql_server_2008_answers_tds_73a` (LOGINACK 7.3, then int/nvarchar/datetime2/date/decimal and a row count), `tls10_only_sql_2008_r2_errors_are_tls_errors_until_unencrypted`, plus reset-on-hello cases; optional live `legacy_tls` 6/6 against SQL 2022 limited to TLS 1.0 | pass |
| `collect_diagnostics` contains the logs, survey and config, with secrets scrubbed | `dl-agent` `diag::tests::the_bundle_has_every_source_and_no_planted_secret_survives` (opens the zip; 14 planted secrets + a JWT found nowhere), `dl-log` `redact::tests::secret_shapes_table` (14 shapes) | pass |
| Hub failover + rollback trial (`--fault probation`) automated on Linux | `hub::failover_tests::primary_down_moves_to_the_fallback_sticks_and_returns_when_the_fallback_fails` (two loopback HTTP hubs); `agent::rollback_tests::a_fault_release_is_installed_rolled_back_at_the_end_of_probation_and_reported` (install → fault boot → rollback → hub receives `update_rolled_back`) | pass |
| `cargo test -p dl-tds -p dl-agent` green; clippy host + windows-gnu; fmt | `cargo test -p dl-tds -p dl-hash -p dl-agent -p dl-log -p dl-http`: 597 passed, 0 failed. `cargo clippy --workspace --all-targets -D warnings` clean. `cargo clippy -p dl-tds -p dl-hash -p dl-agent -p dl-log -p dl-agent-bin --all-targets --target x86_64-pc-windows-gnu -D warnings` clean. `cargo fmt --all --check` clean | pass |

`cargo test --workspace --no-fail-fast`: 3599 passed, 3 failed. The failures are `dl-agent-pms` `fake_cloud` (1) and
`dl-tls` `network` (2). Both fail the same way on `main` in this sandbox, and this request does not touch them.

## Design notes

- **NTLM.** NTLM is portable NTLMv2 (`dl-tds/src/ntlm.rs`). It is used for `User ID=DOMAIN\user;Password=…` on any
  OS. `Integrated Security=SSPI` behaves differently by platform:
  - on Windows, SSPI Negotiate is still used first (Kerberos, or NTLM inside SSPI);
  - off Windows, without domain credentials, it remains `Unsupported` with a clear message.

  The client does not use KEY_EXCH, SIGN or SEAL, which TDS does not need, and sends no channel bindings. So a
  server set to Extended Protection = Required will refuse it.
- **SQL Server 2008 / TDS 7.3.** The client still asks for TDS 7.4. A server that answers 7.3 is accepted and
  nothing is gated on 7.4, so the client needed no change. The TLS ladder (TLS 1.2 → TLS 1.0 → unencrypted, with
  `sql_tls_unavailable` raised for remote servers) already lives in `dl-agent/src/sql/transport.rs`. dl-tls has no
  server side, so the fake server emulates a TLS 1.0 peer at the protocol level.
- **Diagnostics.**
  - The bundle now includes `survey.json` and `selftest.last.json`.
  - Modules can add a log folder (`add_diagnostic_folder`, hook H2 of the pda-sync plan). The pda-sync module still
    has to call it for its log folder (follow-up).
  - The scrub now also masks fields whose names contain token, secret or password. A few non-secret fields may be
    masked too; that trade-off is accepted.

## Only on real Windows / real SQL 2008 / SR bench (for the iMac or the bench)

1. On Windows:
   - `cargo test -p dl-tds --lib` (SSPI unit tests);
   - `DL_TDS_SSPI_SERVER=<server> cargo test -p dl-tds --test sspi`.
2. NTLM against a real Windows SQL Server (domain or local account), run from Linux/macOS and from Windows:
   - Connect with `Server=<host>;User ID=<DOMAIN>\<user>;Password=…;TrustServerCertificate=true`.
   - `SELECT SUSER_SNAME(), auth_scheme FROM sys.dm_exec_connections WHERE session_id=@@SPID` must return
     `DOMAIN\user` and `NTLM`.
   - Extended Protection = Allowed must work; Required is expected to refuse.
3. Mũi Né pilot (RT-0252, SQL 2008 through the relay host SERVER_SMILE_MN):
   - **Build:** `SELECT @@VERSION, SERVERPROPERTY('ProductVersion')`. Check whether the TLS 1.2 update is installed,
     and that `tds_version` reports 0x730A0003 or 0x730B0003.
   - **Browser:** check that UDP 1434 reaches the server through the relay. If it does not, use `HOST\INST,port`
     with the static TCP port.
   - **TLS rung:** check which rung of the ladder the connection settles on, and that `sql_tls_unavailable` is
     raised instead of a silent clear-text SQL login.
   - **Diagnostics:** check `survey.json` in the diagnostics bundle.
4. Rollback trial on a Windows canary:
   - Build with `scripts/build-dl-agent.sh --fault probation X` and publish it.
   - The SCM must restart the service with exit code 75.
   - The running `dl-agent.exe` must move to `previous\`.
   - The hub must show `update_rolled_back`, and the heartbeat `update.state=rolled_back`.
   - Then publish X+1 and see it commit.
5. Failover against the real hosts:
   - Block `agent.pms.id.vn` on the bench: the heartbeat must move to the fallback through the site proxy (the
     WinHTTP proxy path is Windows only).
   - Then block the fallback: the heartbeat must come back to the primary.
6. `collect_diagnostics` from the hub on a SMILE site: download the zip and grep it for the SQL password and the
   tokens.

---

# Stream reports (verbatim)

## RT-0258 stream TDS — report

Branch `cloud/rt-0258-tds`, worktree `/home/user/wt/tds`. Commits: `8b12a34` feat(dl-hash): add MD4 for NTLM;
`3e1e553` feat(dl-tds): NTLMv2 for DOMAIN\user logins, SMILE legacy server tests (final). Not pushed.

## Gates (CARGO_TARGET_DIR=/home/user/wt/target-tds)
- `cargo fmt --all --check` — OK
- `cargo clippy -p dl-tds -p dl-hash --all-targets --all-features -- -D warnings` — OK
- same with `--target x86_64-pc-windows-gnu` — OK (compiles the cfg(windows) SSPI paths too)
- `RUSTDOCFLAGS="-D warnings" cargo doc -p dl-tds -p dl-hash --no-deps` — OK
- `cargo test -p dl-tds -p dl-hash` — all green: dl-tds lib 94 (was 82; +20 new, 12 legacy + 8 NTLM), dl-hash lib 18
  (+3 MD4), integration/doc/other targets green (live tests skip without env).
- Extra: `cargo test -p dl-agent --lib sql::` (uses `dl_tds::fake`) — 69 passed, 2 ignored.

## Item 1 — Windows login (NTLM)
Already present: SSPI Negotiate on Windows only — `crates/dl-tds/src/sspi.rs` (Negotiate, SPN rules `spn()` :202),
`src/sspi_windows.rs` (secur32 FFI), token loop `src/client.rs:372 finish_login`, LOGIN7 sends empty user/password +
fIntSecurity when an SSPI token is present (`src/login.rs:64`), fake SSPI package test `client_tests.rs
sspi_token_flow_answers_each_challenge_until_loginack`. Off Windows: `Error::Unsupported`. No NTLM, no MD4.

Added:
- `crates/dl-hash/src/md4.rs` — `Md4` (RFC 1320); tests `md4::tests::{rfc1320_vectors, streaming_matches_one_shot,
  nt_hash_of_password}` (NTOWFv1("Password") = a4f49c40... from MS-NLMP 4.2.2.1.2). README/Cargo description updated.
- `crates/dl-tds/src/ntlm.rs` — portable NTLMv2 (MS-NLMP): NEGOTIATE (flags UNICODE|REQUEST_TARGET|NTLM|ALWAYS_SIGN|ESS|
  TARGET_INFO|VERSION|128|56, no KEY_EXCH/SIGN/SEAL), CHALLENGE parsing (bounds-checked, AV pairs), AUTHENTICATE with
  NTOWFv2 (HMAC-MD5(MD4(pw), UPPER(user)+domain)), NTLMv2 response, target info echoed; when the server sends
  `MsvAvTimestamp`: server time used, LM = Z(24), `MsvAvFlags |= 0x2` and MIC = HMAC-MD5(SessionBaseKey,
  NEG||CHAL||AUTH) per 3.1.5.1.2 (no key exchange: ExportedSessionKey = SessionBaseKey); otherwise LMv2. Client
  challenge from `dl_rand::secure_bytes` (dl-tds now depends on workspace crates dl-hash + dl-rand; no third-party).
  New `Error::Ntlm(&'static str)` (Error is #[non_exhaustive]); messages never contain credentials/tokens.
- Selection (`sspi.rs:80 Negotiate::for_config`, documented in the module doc + README):
  * `Integrated Security=SSPI` on Windows → SSPI current account, always first (user/password in the string ignored,
    as SqlClient).
  * `Integrated Security=SSPI` off Windows → NTLM if `User ID=DOMAIN\user` + password given, else `Unsupported`
    (message now: "integrated authentication needs Windows (SSPI), or User ID=DOMAIN\user with a password for NTLM").
  * No integrated security + `User ID=DOMAIN\user` → NTLM on every OS (go-mssqldb behaviour; also on Windows, with
    the explicit credentials, not SSPI).
  * Anything else → SQL login (unchanged).
- Tests, `src/ntlm_tests.rs` (`cargo test -p dl-tds --lib ntlm`):
  * `ms_nlmp_4_2_4_ntlmv2_vectors` — User/Domain/Password, server chal 0123456789abcdef, client chal aa*8, time 0, the
    spec's CHALLENGE_MESSAGE 4.2.4.3 parsed byte for byte: NTOWFv2 0c868a403bfd7a93a3001ef22ef02e3f, NTProofStr
    68cd0ab851e51c96aabc927bebef6a1c, SessionBaseKey 8de40ccadbc14a82f15cb0ad0de95ca3, LMv2 86c35097...aaaa — all pass.
    (The spec's EncryptedRandomSessionKey vector needs KEY_EXCH/RC4, which this client does not do.)
  * `spec_example_authenticate_verifies`, `negotiate_message_layout`, `timestamp_means_zero_lm_mic_and_flags` (MIC
    verified; tampering AUTH or NEGOTIATE breaks it; wrong password fails NTProofStr; 2nd CHALLENGE refused),
    `existing_av_flags_keep_their_bits`, `user_name_case_is_ignored_domain_case_is_not`, `windows_login_spellings`,
    `malformed_challenges_are_ntlm_errors`. A server-side verifier (`ntlm_tests::verify`) recomputes NTProofStr, the
    echoed target info and the MIC.
- End-to-end on the fake TDS server, `src/client_legacy_tests.rs` (`cargo test -p dl-tds --lib legacy`):
  `domain_user_logs_in_with_ntlmv2_and_a_mic` (fake answers LOGIN7's NEGOTIATE with a CHALLENGE incl. timestamp, checks
  the AUTHENTICATE: domain/user/workstation, NTProofStr, MIC; LOGIN7 user empty; password never in any token),
  `ntlm_without_server_timestamp_uses_lmv2`, `a_bad_challenge_is_an_ntlm_error_not_a_panic`,
  `integrated_security_off_windows_needs_domain_credentials` (non-Windows), `a_plain_sql_login_never_starts_ntlm`.
  Results: all pass.

## Item 2 — Named instance
Already present: parsing `host\INST[,port]`, URL path (`config.rs:348`, tests config.rs:775-807, :1005 "explicit
port wins"); Browser request/response + unit tests with a fake UDP browser (`browser.rs:19,74,157`); dial resolves only
when no port (`client.rs:177 dial`); `.`/`(local)`/`localhost` → 127.0.0.1 (`client.rs dial_host`).
Gap: nothing tied Browser → connect end to end. Added:
- `browser.rs` test-only `test_port` thread-local so `resolve` can hit a fake browser on a random UDP port (prod path
  unchanged, cfg(test) only).
- Fake server records the PRELOGIN INSTOPT (`Attempt::instance`).
- Tests: `named_instance_resolves_through_the_browser_then_logs_in` (`127.0.0.1\SMILE`, `.\SMILE`, `(local)\smile`,
  `localhost\SMILE`, `sqlserver://smile:x@127.0.0.1/SMILE` → one Browser request each, connect to the returned port,
  login + batch; INSTOPT = SMILE), `an_explicit_port_wins_over_the_instance_and_skips_the_browser` (`\SMILE,port`
  forms incl. `tcp:`; browser points to a dead port and must receive nothing), `an_unknown_instance_is_a_browser_error_
  before_any_tcp`. All pass.

## Item 3 — SQL Server 2008 / 2008 R2
Audit: LOGIN7 asks TDS 7.4 (`client.rs:306`), no FEATUREEXT is sent (`login.rs` only with features; client passes
none), LOGINACK version parsed and only reported (`token.rs:223`, `Client::tds_version`); no code path is gated on
7.4; date/time/datetime2/datetimeoffset decoding exists (`types.rs:592-620`) and NBCROW is accepted but not required.
So a 7.3 answer is accepted; nothing needed changing in the client. TLS ladder already exists in dl-agent
(`crates/dl-agent/src/sql/transport.rs:120 should_step_down`, unit `sql_2008_r2_rtm_ends_unencrypted` :259; fake-based
`dl-agent/src/sql/tds_tests.rs:64 the_ladder_steps_from_tls12_to_tls10_to_unencrypted`); dl-tds maps every handshake
failure to `Error::Tls` (`client.rs handshake_error`).
Added to `dl_tds::fake` (feature `fake-server`, public fields with `..Script::default()`): `Script::tds_version`
(consts `TDS_74`, `TDS_73B`=0x730B0003, `TDS_73A`=0x730A0003), `server_version` (PRELOGIN VERSION + LOGINACK),
`responses` (scripted token streams per request), `tls: TlsPeer {DropAfterHello (old default), ResetOnHello,
Tls10Only}`; `Attempt::{instance, tls_alert, login_tds_version}`; LOGINACK is now recorded before it is sent (removes a
race in tests that check `logged_in` right after connect). dl-tls has no server side, so a TLS 1.0 handshake cannot
complete in the fake: it is emulated at the protocol level (TLS 1.0 ServerHello record; RST by closing with the hello
unread).
Tests:
- `sql_server_2008_r2_answers_tds_73b` / `sql_server_2008_answers_tds_73a`: LOGINACK 7.3 + 10.50.1600 / 10.0.1600,
  then a result set int / nvarchar ("Mũi") / datetime2(7) / date / decimal(18,2) incl. NULL row, then an UPDATE row
  count; client asked 7.4. Pass.
- `tls10_only_sql_2008_r2_errors_are_tls_errors_until_unencrypted`: default transport → "TLS handshake failed
  (0xa000102): error:0A000102:SSL routines::unsupported protocol" (`Error::Tls(Handshake)`, client sends alert 70
  protocol_version); TLS 1.0 rung → "TLS I/O error: Connection reset by peer" (`Error::Tls(Io)`); `Encrypt=disable` →
  LOGINACK 7.3. Both errors are step-down errors for `should_step_down`. Pass (Linux/OpenSSL; cfg skips macOS).
- `a_reset_during_the_handshake_is_a_tls_io_error`: default, `Encrypt=true`, `TLSMin=1.0` → `Error::Tls(Io)`, no
  login, no clear-text fallback inside dl-tds. Pass.
- Live (optional) done: SQL Server 2022 container `rt258-tds` with `network.tlsprotocols=1.0`,
  `tlsciphers=DEFAULT:@SECLEVEL=0`, self-signed cert; `DL_TDS_TEST_TLS10_URL=sqlserver://sa:***@127.0.0.1:14358
  cargo test -p dl-tds --test legacy_tls` → 6/6 pass (default fails in handshake, TLS 1.0 login, whole session over
  TLS 1.0, URL tlsmin, unencrypted, wrong password is a server error). `DL_TDS_TEST_URL=...?encrypt=disable cargo test
  -p dl-tds --test integration` against the same container: 17 pass, `encrypted_connections` fails only because that
  container refuses TLS 1.2 by configuration (expected, not a regression). Container and scratch password removed.

## Only checkable on real Windows / real SQL 2008 / SR bench
1. Windows: `cargo test -p dl-tds --lib` (sspi `windows_negotiate_first_token`, `windows_local_name`) and
   `DL_TDS_SSPI_SERVER=<server> cargo test -p dl-tds --test sspi` (SSPI still first with `Integrated Security=SSPI`).
2. NTLM against a real Windows SQL Server with a domain or local account (needs AD or a Windows host; not possible in
   the Linux container): connection string `Server=<host>;User ID=<DOMAIN or MACHINE>\<user>;Password=...;
   TrustServerCertificate=true`, then `SELECT SUSER_SNAME(), auth_scheme FROM sys.dm_exec_connections WHERE
   session_id=@@SPID` must return `DOMAIN\user`, `NTLM`. Run from Linux/macOS and Windows. Also check a server with
   Extended Protection = Allowed (works) vs Required (expected refusal: no channel bindings).
3. Mũi Né pilot (RT-0252, SQL 2008 via relay host SERVER_SMILE_MN) — bench must check:
   - exact build: `SELECT @@VERSION, SERVERPROPERTY('ProductVersion')` (10.0.x = 2008, 10.50.x = 2008 R2; SP/TLS 1.2
     update present?), and `Client::tds_version()` / agent describe reports 0x730A0003 or 0x730B0003;
   - named instance: is SQL Browser (UDP 1434) reachable from the agent host to SERVER_SMILE_MN through the relay?
     If UDP is not relayed, use `host\INST,port` (explicit port skips the Browser) — confirm the static TCP port;
   - which ladder rung settles (as_configured / tls1.0 / unencrypted) and that `sql_tls_unavailable` is raised (not a
     silent clear-text SQL login) when the server is remote and only the unencrypted rung works; whether the reset
     happens after the TLS 1.0 handshake (field case 2026-11-17) or at the hello;
   - if the site uses a Windows account: `User ID=DOMAIN\user` NTLM login works through the relay (TCP only), and
     `auth_scheme` = NTLM; with SSPI on the agent machine, the SPN for the relay name (`ServerSPN` may be needed, else
     NTLM fallback);
   - datetime/datetime2/decimal/nvarchar columns of SMILE_FO read the same as the old agent (spot-check a folio).
4. dl-agent follow-up (not in this stream's crates): `dl-agent/src/sql/secret.rs:59 AuthMode::of` classifies a
   `DOMAIN\user` string as `sql` (describe shows `auth: sql`, ladder treats it as a SQL login: no clear rung to a
   remote host — conservative, NTLM never sends the password, but the label is misleading). Consider a `windows-ntlm`
   mode there.

## Files
New: crates/dl-hash/src/md4.rs, crates/dl-tds/src/ntlm.rs (390 lines), src/ntlm_tests.rs (327),
src/client_legacy_tests.rs (431). Changed: dl-hash lib.rs/README/Cargo.toml; dl-tds Cargo.toml (dl-hash, dl-rand deps;
dl-rand moved from dev-deps), sspi.rs, client.rs (+test module, 751 lines), error.rs (`Error::Ntlm`), browser.rs
(test hook), fake.rs (419), client_tests.rs, tests/sspi.rs (message), README.md; CHANGELOG.md; Cargo.lock.

## RT-0258 stream CORE — report

Worktree `/home/user/wt/core`, branch `cloud/rt-0258-core`, final commit `5a3dbaa` (3 commits on top of `2dc320c`, not pushed).

Commits:
- `b3d00c4` feat(dl-log): scrub URL credentials, JWTs and more secret field names
- `637f245` feat(dl-agent): survey, last selftest and module log folders in the diagnostics bundle
- `5a3dbaa` test(dl-agent): hub failover over loopback HTTP and the fault rollback trial end to end (+ README/PROTOCOL/CHANGELOG)

Crates touched: `dl-agent`, `dl-log` (`scrub` lives in `dl-log/src/redact.rs`, so the scrub gaps had to be closed there). `dl-http` not touched.

## Item 4 — diagnostics bundle

### Already present (audit)
- `collect_diagnostics` + zip: `crates/dl-agent/src/agent_diag.rs` (bundle, upload, 8 MB cap with halving log budget).
- Logs: `logs/<file>`, newest first, every file in `data/logs` (so rotated files too), tail of each within a 6 MB budget.
- State files `state/*` (update/config/heartbeat/watchdog/reports/sql-installs/sql-setup.last), `identity.key` and `sql.json` never included.
- Module files: `ModuleContext::add_diagnostic_file` (`supervisor.rs:259`), name allowlist `diagnostic_file_allowed`.
- Status: `diagnostics.json` already has version, update state, module statuses, hub urls, clock skew, config version.
- Scrubbing: `dl_log::scrub` handled `Password=`/`Pwd=` with ADO.NET quoting (case-insensitive), Bearer/Basic, names ending in password/pwd/secret/token/key/authorization/_sealed.
- Tests already there: `agent::diag::tests::{bundle_scrubs_secrets_and_leaves_out_key_files, logs_take_the_newest_tail_within_the_budget, collect_diagnostics_uploads_the_zip_and_answers_with_its_key, a_failed_upload_fails_the_command_but_keeps_the_diagnostics, module_files_go_in_scrubbed_and_secret_files_never}`.

### Gaps found and added
- **Survey missing** from the bundle → `survey.json` (`{host, databases: SqlPools::surveys()}`), `agent_diag.rs:263` `bundle_inputs`.
- **Selftest missing** (runs were never kept) → `selftest::save_last` (`selftest/execute.rs:119`) writes `selftest.last.json` after every service `selftest` command and every CLI `dl-agent selftest`; added to `STATE_FILES` (`agent_diag.rs:23`).
- **Scrub gaps** (`dl-log/src/redact.rs`): URL userinfo (`scheme://user:[redacted]@host`, token-as-user masked whole) `url_userinfo` :88; JWT-looking strings `eyJ….….…` `jwts` :64; field names *containing* `password`/`passwd`/`secret`, or with a `token`/`pwd`/`apikey` `_`/`-` part (`refresh_token_enc`, `password_hash`, `client_secret_value`) `is_secret_name` :49. `key` stays "ends with" so `key_id`/`kid`/`r2_key` stay readable (existing behaviour kept).
- **H2 rolling log folder**: `ModuleContext::add_diagnostic_folder` (`supervisor.rs:277`, new `ModuleStatus::diagnostic_folders`), folder-name check `diagnostic_folder_allowed` (`agent_diag.rs:57`, refuses key/secret/identity/password/cert), bundled as `modules/<module>/<folder>/<file>` newest first, files filtered by the same file allowlist, budget min(2 MiB, log_budget/4) per folder (shrinks with the halving loop), missing folder noted in `bundle.json`. Refactor: shared `add_folder` (:126) for agent logs and module folders; `bundle`/`bundle_with` now take an `Inputs` struct. pda-sync still has to call `add_diagnostic_folder` (B5) — plan H2 line updated.

### Tests (all pass)
- `dl-log` `redact::tests::secret_shapes_table` (table-driven: 14 cases — Password/PWD any case + quoting, URL creds, token-as-user, JWT bare/quoted, secret/password/token-part names, x-api-key) + extended `plain_text_is_borrowed` (tokenizer/tokens, plain URL, e-mail, "eyJ" word stay untouched).
- `agent::diag::tests::the_bundle_has_every_source_and_no_planted_secret_survives` — full agent + module with file + folder; opens the uploaded zip and asserts entries `diagnostics.json, survey.json, logs/agent.log, logs/agent.log.1 (rotated), state/config.json, state/selftest.last.json, modules/pda-sync/status.json, modules/pda-sync/logs/<2 files>, bundle.json`, no `identity.key`/`sql.json`/key-named files, and 14 planted secrets + a JWT (in logs, rotated log, config, sql.json, identity.key, selftest, module file, module folder, refused folder, survey) appear nowhere (names or contents) nor in the command output.
- `agent::diag::tests::module_folders_take_the_newest_files_within_their_share` — refusals, dedupe, budget (newest whole, next tail, oldest skipped), missing folder.
- Command: `CARGO_TARGET_DIR=/home/user/wt/target-core cargo test -p dl-agent -p dl-log` → 356 passed, 0 failed.

## Item 5 — hub failover and rollback trial

### Already present (audit)
- Failover logic: `hub.rs:333` `call_as` (sticky preferred index, move down on transport error/5xx, wrap around), `hub.rs:250` `set_hub_urls` (keeps host in use). Design (PROTOCOL §2.1): "sends each call to the host that last worked" — i.e. it returns to the primary only when the fallback fails (wrap-around) or the hub's list drops the fallback; there is no timed probe back to the primary.
- Tests (scripted fake transport): `hub_tests.rs:172 falls_back_to_next_hub_and_remembers_it`, `:293 a_new_host_list_keeps_the_host_in_use_and_falls_back_over_it`, `agent_hub_tests.rs:15 a_hub_list_is_kept_and_used_when_the_primary_is_down`.
- Rollback: `agent.rs:245 boot` (boot-count rollback), `agent.rs:416 watch_probation` (fault build never commits), `agent_update.rs:140 roll_back` (restore, failure count, `update_rolled_back` critical report). Unit tests: `update_tests.rs:222/244`, `agent_tests.rs:241/310`, `agent_drain_tests.rs:109 a_fault_build_rolls_back_at_the_end_of_probation_and_reports_it`, `agent_lane_tests.rs canary_probation_is_short_and_still_rolls_a_bad_build_back`. Missing: a real-HTTP failover test and a whole-cycle (install → fault boot → rollback → hub sees report) test.

### Added
- `hub::failover_tests::primary_down_moves_to_the_fallback_sticks_and_returns_when_the_fallback_fails` (`src/hub_failover_tests.rs`): two loopback HTTP hubs with the real `HttpTransport`; primary port refusing → call lands on fallback and it is kept; primary back up → still 0 requests to primary while fallback answers; fallback down → next calls go to primary and stay; both down → `Error::Transport`, url list unchanged.
- `agent::rollback_tests::a_fault_release_is_installed_rolled_back_at_the_end_of_probation_and_reported` (`src/agent_rollback_tests.rs`): three agent lives on one temp install. Life 1 (old build) takes a signed canary manifest for 99.0.0 and installs the downloaded "fault build" bytes (exe swapped, `previous/` keeps old). Life 2 (version 99.0.0, `fault = probation`) boots on probation, links to hub, never commits, at `probation_until` restores the old binary → `Exit::Restart`. Life 3 (old version) boots normal, failure of 99.0.0 recorded, next heartbeat flushes reports: hub receives `update_rolled_back` (message contains `DL_AGENT_FAULT=probation`) and the heartbeat says `version = old`, `update.state = "rolled_back"`.
- Docs: PROTOCOL §4.1 (bundle contents, scrub rules) and §6.6 (what the Linux test covers), README (modules diag hooks, test coverage), CHANGELOG, plan H2.

## Gates
- `cargo fmt --all --check` → ok
- `cargo clippy -p dl-agent -p dl-log --all-targets -- -D warnings` → 0 warnings
- `cargo clippy -p dl-agent -p dl-log --all-targets --target x86_64-pc-windows-gnu -- -D warnings` → 0 warnings
- `cargo test -p dl-agent -p dl-log` → 356 passed, 0 failed (2 ignored = live SQL tests)
- Regression check of crates using scrub/log output: `cargo test --no-fail-fast -p dl-agent-backup -p dl-agent-pms -p dl-agent-pda-sync -p dl-agent-arrival` → all ok except the known pre-existing `dl-agent-pms --test fake_cloud` failure (`enrolls_runs_ops_and_reconnects_after_a_close`, fails on main in this sandbox, not touched).
- New files ≤ 800 lines (agent.rs now 787). No new crates, no unsafe.

## Only on real Windows / bench (precise list)
1. Rollback trial on a canary Windows machine (PROTOCOL §6.6 steps 1–4): `scripts/build-dl-agent.sh --fault probation X`, publish to canary, check: the SCM restarts the service on exit code 75 after install and after rollback (recovery actions), renaming the running `dl-agent.exe` to `previous\` works, `GET /admin/reports?severity=critical&version=X` shows `update_rolled_back`, heartbeat `update.state=rolled_back`, selftest row `core/update_state` lists X; then publish X+1 and see it commit.
2. Failover against the real hosts: block `agent.pms.id.vn` on the bench (hosts file / firewall) and check the heartbeat moves to `agent-hub.vutq-sense.workers.dev` through the site proxy (WinHTTP/IE proxy path is Windows-only), and comes back after the fallback is blocked instead.
3. `collect_diagnostics` from the hub on a SMILE site: download the zip from R2 and grep for the SQL password, enrol token and pda-sync token; check `survey.json` from SQL 2008 (Mũi Né relay SERVER_SMILE_MN) and that `selftest.last.json` appears after one `selftest` command (needs the service account to write the data dir — a write failure is only logged).
4. pda-sync B5: call `ctx.add_diagnostic_folder(<pda log dir>)` in the module (not done here: module crate out of this stream's scope) and confirm the folder lands under `modules/pda-sync/logs/`.

## Notes / behaviour changes to be aware of
- `scrub` now also masks values of fields like `token_count`, `password_policy`, `secret_*` (any name with a token part or containing password/secret). Over-masking of a few non-secret log fields is the accepted trade-off.
- `ModuleStatus` got a new public field `diagnostic_folders` (no external struct literals exist).

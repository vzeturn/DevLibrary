# RT-0164 BENCH — report

Status: **DONE_WITH_CONCERNS** (built and tested locally, never run on a real Windows PC).
Branch `feat/hub-bench` (from origin/main 11ac477), feature commit `627b9c1`. Not pushed, merged or
deployed; no prod D1 or secret was touched.

## What was built

### Routes

| Route | Auth | What |
|---|---|---|
| `POST /admin/bench` | admin | Body `{tenant_id, site_id, label, steps:[{setup, args?, env?}], sql_databases?, sql_fixups?, expires_hours (1–72, default 24), max_fetches (1–100, default 10), config?}` → `201 {id, irm, expires_at}` |
| `GET /admin/bench` | admin | Bundles with `status` (`active`/`expired`/`revoked`/`used_up`), `fetch_count`, `code_used_at`; never `steps_enc` |
| `DELETE /admin/bench/:id` | admin | Revokes the bundle and its enrolment code if still unused; 404 `bench_not_found` |
| `GET /bench/:token` | public | `text/plain; charset=utf-8`, `Cache-Control: no-store`; atomic `fetch_count + 1` with `UPDATE … RETURNING`; one identical 404 for unknown, expired, revoked, used up or malformed |
| `POST /v1/agent/selftest` | agent signature (same middleware as `/v1/reports`) | Body per brief; 413 `selftest_too_large` over 256 KB; zod caps 500 checks; retried `run_id` replaces its run; newest 20 per agent kept (same batch) |
| `GET /admin/agents/:id/selftest` | admin | `{agent_id, latest: {…, summary, checks} or null, history: [summaries newest first]}`; 404 `agent_not_found` |
| `GET /admin` | admin | New section "Tự kiểm tra": latest run per agent, đạt/cảnh báo/lỗi/bỏ qua counts, failed checks (module · title: detail → remedy), HTML-escaped |

`POST /admin/commands {"agent_id", "kind": "selftest", "args": {"trigger": "command"}}` works through
the generic path (kind regex `^[a-z][a-z0-9_.]{0,63}$`), delivered on the next heartbeat. Covered by a test,
documented in the README. The hub has no special case for it.

### Migration

`services/agent-hub/migrations/0004_bench_selftest.sql` (next free number after 0003):
`bench_bundles` and `selftest_runs` with the brief's columns, index `selftest_runs_agent (agent_id, received_at)`.

### Secret to set (owner / main, not done here)

- `BENCH_KEY`: base64 of 32 random bytes. Set it with `openssl rand -base64 32 | wrangler secret put BENCH_KEY`.
  - Without it every bench call answers `503 bench_key_missing`.
  - Rotating it makes existing bundles unreadable (`500 bench_unreadable`, logged as `bench_unseal_failed`).
- Deploy order: `pnpm db:migrate` (remote) → `wrangler secret put BENCH_KEY` → `pnpm deploy`.
- Check that Cloudflare Access covers only `/admin*`, not `/bench/*`. A PC running `irm` has no Access token.

### Files

- New:
  - `migrations/0004_bench_selftest.sql`
  - `src/domain/bench.ts` (zod schemas, placeholder rules)
  - `src/repo/bench.ts`
  - `src/repo/selftest.ts`
  - `src/services/bench.ts`
  - `src/services/selftest.ts`
  - `src/routes/bench.ts`
  - `src/install/benchScript.ts` (PowerShell renderer, 442 lines)
  - `test/bench.test.ts`
  - `test/selftest.test.ts`
- Changed:
  - `src/lib/crypto.ts`: `toBase64`, `randomToken`, `importAesKey`, `seal`/`unseal` (AES-256-GCM, 12-byte IV, AAD)
  - `src/env.ts`: `BENCH_KEY?`
  - `src/domain/schemas.ts`: `selftestBody`, `MAX_SELFTEST_BYTES`
  - `src/routes/admin.ts`, `src/routes/agent.ts`, `src/routes/statusPage.ts`, `src/index.ts`
  - `wrangler.jsonc`: secrets comment
  - `README.md`: endpoint table, self-test note, section "Bench (test PC)", BENCH_KEY in Production
  - root `CHANGELOG.md`: two entries under Unreleased/Added
- The brief `docs/plans/2026-11-19-bench-brief.md` is committed with the work.

## The bench script (what runs on the PC)

The script runs these steps in order:

1. **Elevation check:** without Administrator it prints a clear message and exits before anything else.
2. **TLS 1.2:** turned on; a warning is printed if it is not available.
3. **Log file:** `%ProgramData%\dl-agent\bench-yyyyMMdd-HHmmss.log`.
4. **Older agents (addendum 1):** read-only listing of Windows services matching `LEGACY_SERVICE_PATTERN` against name, display name and path. It prints name, state, display name and path.
   - Pattern: `mmei|e-?invoice|smile ?pms|smile ?hl|mm\.arrival|mm\.membership|arrival\.agent|membership\.agent|srbackup|pda ?sync`, matched case-insensitively.
   - Uses Get-CimInstance, or Get-WmiObject where CIM is missing.
   - Nothing stops, removes or replaces them: `install.ps1` is called without `-Replace`, and a test asserts the token never appears.
   - At the end their state is compared again; any change is a `warn` row.
5. **SQL Server:** one prompt (`.` default, SQL login with a SecureString password, or Windows), unless `-SqlConnection` is passed.
   - Tested through SqlClient against master.
   - `DB_ID()` is checked for every database the bundle touches.
   - Windows auth prints a LocalSystem warning.
6. **SQL fixups (addendum 2):**
   - Each entry is `{name?, database, sql_url | sql_inline, sha256? (with sql_url only)}`.
   - Runs once per script run, before install. The order is fixups, then install, then the module steps.
   - Each fixup runs in its own transaction, with `GO` lines splitting batches.
   - Inline SQL travels base64 inside the sealed payload. Only name, database, batch count and rows changed are printed.
   - A failed fixup rolls back and stops the bench before install: the code is not spent, and nothing runs against a copy that may still talk to live services.
7. **Install:** `install.ps1 -Code <code> -Channel canary`. The result is checked through the exe, `identity.json` and the service.
8. **`sql-add` / `sql-test`:** run for each `sql_databases` entry. The connection is passed only in `DL_AGENT_SQL_CONNECTION`.
9. **Steps:** `dl-agent <setup> <args…> --data %ProgramData%\dl-agent`, with env set for that process and restored or removed in `finally`.
   - `{{SQL:<Db>}}` (env only) becomes that database's connection string.
   - `{{BENCH_DIR}}` (args and env) becomes `%ProgramData%\dl-agent\bench`, created after install. Steps carry `--install-dir/--config-dir/--state-dir {{BENCH_DIR}}\<module>` so modules never adopt the older agents' directories.
   - Unknown placeholders, or SQL placeholders in args, are refused at `POST /admin/bench`.
   - A failed step is printed and the next one runs.
10. **Restart:** the service is restarted, then the script waits up to 90 s for `hub_at` in `heartbeat.json`.
11. **Self test:** `dl-agent selftest --trigger install`. If `--help` does not list it, the script prints a message and records "skipped".
12. **Summary:** a table of step, result and note, plus the count of failed steps.

Secrets (code, env values, SQL password, connection strings, fixup URLs) are kept in a list. Every console and log line is masked with it. Env values exist only for the step's process.

## Deviations from the brief (deliberate)

- **The `irm` link carries a token, not the bundle `id`.** The link token has 256 bits and is base64url, so 43 characters. D1 stores `id = sha256(token)` (hex). That `id` is what `GET`/`DELETE /admin/bench` use. This matches how enrolment codes and product keys are stored: a D1 leak does not reveal live links.
- **Extra column `bench_bundles.label`.** It is needed for the listing and the script banner.
- **Extra unique index `selftest_runs (agent_id, run_id)`.** It makes a retried upload idempotent.
- **An existing tenant with `demo = 0` gets `409 tenant_not_demo`** instead of being flipped. Flipping would move a live tenant's agents to canary.
- **Optional `config` pass-through to the enrolment code.** It sets `modules.<name>.enabled` so steps' modules actually run.
- **Addendum extensions:** an optional `sha256` pin for `sql_url`, and an optional fixup `name` (default `fixup N`).
- **The log name includes the time** (`bench-yyyyMMdd-HHmmss.log`), so re-runs on the same day do not interleave.

## Tests and gates (exact commands, in `services/agent-hub`)

- `pnpm typecheck`: clean (src + test).
- `HEAVY_LOCK=/tmp/heavy-dl-bench ~/.claude/scripts/heavy.sh pnpm -s vitest run`: 11 files, 110 tests passed. 39 of them are new: `bench.test.ts` 27 and `selftest.test.ts` 12.
  - **Bench:** create, list and revoke; the fetch count and its limit; expiry; every 404 case; `no-store`; 401 without the admin token; 503 without the key; 409 for a non-demo tenant; 15 validation rejections.
  - **Encryption at rest:** the D1 dump does not contain the token, a step secret, a setup name or the fixup SQL. The AAD binding means a sealed blob moved to another row gives 500 plus a logged error. Seal/unseal fails with a wrong key, a wrong AAD or malformed input.
  - **Enrolment:** the code from the rendered script enrols an agent on the demo tenant with channel canary.
  - **Script content:** `psQuote` handles typographic quotes; the legacy pattern matches the seven agent names and not `dl-agent`, `Spooler` or `MSSQLSERVER`; there is no `-Replace`.
  - **PowerShell, pwsh 7.6.6:** the rendered script parses with 0 errors using `Parser::ParseFile`. The data block round-trips tricky values (quotes, `‘’`, `$()`, a backtick, Vietnamese text, CRLF). The real `Split-Batches` function, lifted from the script, splits `GO`/`go` lines and leaves `GOTO` alone.
  - **Self test:** signed upload and summary; a forged signature gives 401; 5 validation rejections; 413 over 256 KB; keep-20 pruning and per-agent isolation; replacement on a retried `run_id`; 404 for an unknown agent; the status page section (empty state, the latest run only, escaping); the `selftest` command through `/admin/commands` delivered on the heartbeat.
- `HEAVY_LOCK=/tmp/heavy-dl-bench ~/.claude/scripts/heavy.sh pnpm -s exec wrangler deploy --dry-run --outdir <scratchpad>`: builds, 952 KiB (gzip 168 KiB).
- All files are under 800 lines; the largest is `benchScript.ts` at 442.

## Not run, and why

- **No run on Windows PowerShell 5.1 or 2.0.** There is no Windows machine here. The parse check used pwsh 7.6.6 on macOS. The script avoids PS 3+ syntax (no `::new()`, `[ordered]` or JSON cmdlets), but 2.0 parsing and runtime behaviour are unverified: WebClient, SqlClient, the transaction, Get-WmiObject, Restart-Service, and the elevation check are all untested.
- **No `wrangler d1 migrations apply --local`.** The vitest fake D1 runs the real migration files on node:sqlite, including 0004.
- **No push, deploy, remote migration or `wrangler secret`.** These were out of scope by instruction.

## Concerns

1. **The older agents keep running against the same restored database**, by owner rule. The fixups only cover what main's SQL covers (triggers, `HLInventorySync`). An old SmileHLAgent or PdaSyncAgent pointed at that copy could still talk to live services; the bench only lists them.
2. **Fixups run once per script run, and every fetch re-runs them,** so main's SQL must be idempotent. Each fixup runs inside one transaction, so statements that cannot run in a user transaction fail and roll back. That includes `ALTER DATABASE … SET` and some full-text DDL.
3. **Step arguments like `--install-dir`, `--config-dir` and `--state-dir` are passed through as written.** If a module's setup sub-command does not accept them, that step fails, and the run moves on to the next step. `--data %ProgramData%\dl-agent` is always appended after the args.
4. **The legacy service pattern is inferred from the product names.** The exact Windows service names of the E-Invoice and SRBackup Go agents were not confirmed. A miss only means the service is not listed.
5. **Secrets embedded in the script body may be recorded.** The code and env values could land in the event log if PowerShell script block logging is on. `-SqlConnection` typed at the console ends up in PSReadLine history; the README and script header say so.
6. **A non-elevated run still consumes one fetch.** The code is single-use. A re-run on an enrolled PC relies on `identity.json` (install.ps1 keeps the identity). Another PC needs a new bundle.
7. **Windows auth runs the agent as LocalSystem,** so `NT AUTHORITY\SYSTEM` needs a SQL login. The script warns about this.
8. **`irm` itself needs TLS 1.2 before the script can set it.** On an old .NET that may require `[Net.ServicePointManager]::SecurityProtocol = 3072` first. This is not handled, since nothing can run before `irm` returns.
9. **The agent side (brief ST) must post to `/v1/agent/selftest` with this exact body shape.** The trigger regex is `^[a-z][a-z0-9_-]{0,31}$` and the `run_id` regex is `^[A-Za-z0-9._:-]{1,64}$`.

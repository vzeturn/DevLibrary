# Brief BENCH: hub "bench" bundles — one `irm` that installs dl-agent with every module on a test PC

RT-0164. The owner has a Windows test PC with SQL Server and a copy of hotel SMILE databases.
They want one PowerShell `irm ... | iex` line that installs dl-agent on the **canary** channel
for a dedicated demo tenant, enrolls every module against each product's demo tenant, then runs
the self test and uploads the result. Hub: `services/agent-hub` (Cloudflare Worker + D1, Hono).
Read `src/install/script.ts` (current install.ps1: `-Code -Channel -SqlPrompt -Setup -Replace`,
env `DL_AGENT_SETUP_ARGS`), `src/routes/admin.ts`, `src/services/enroll.ts`, `src/lib/crypto.ts`
and the migrations folder first.

## What to build (worktree `~/Projects/DevLibrary-bench`, branch `feat/hub-bench`)

1. **Migration** (next free number): `bench_bundles(id, tenant_id, site_id, enroll_code_id,
   steps_enc, created_at, expires_at, fetch_count, max_fetches, revoked_at)` and
   `selftest_runs(id, agent_id, run_id, trigger, started_at, finished_at, agent_version,
   summary_json, checks_json, received_at)` + index on (agent_id, received_at).
2. **Admin** (behind the existing admin auth): `POST /admin/bench` with
   `{ tenant_id, site_id, label, steps: [{ setup, args?: string[], env?: {NAME: value} }],
   sql_databases?: string[], expires_hours (default 24, max 72), max_fetches (default 10) }`.
   It ensures the tenant exists with `demo = true` (canary), mints an enroll code for that
   tenant/site (reuse the existing code minting), stores the steps **encrypted at rest** (AES-GCM
   with a key from a Worker secret, e.g. `BENCH_KEY`; reuse `lib/crypto.ts` helpers) and returns
   `{ id, irm: "irm https://<host>/bench/<id> | iex", expires_at }`. `GET /admin/bench`,
   `DELETE /admin/bench/:id` (revoke). `id` is long and random (≥128 bits, URL-safe).
3. **Public** `GET /bench/:id`: `text/plain`, `Cache-Control: no-store`; 404 for unknown,
   expired, revoked or over `max_fetches` (increment atomically). Returns a PowerShell 5.1
   script (also parse-clean on 2.0 where practical) that:
   - requires elevation (clear message otherwise), sets TLS 1.2;
   - asks once for the SQL Server (default `.`/`localhost`, Windows auth or SQL login, password
     read as SecureString) unless `-SqlConnection` given, tests it, and registers each name in
     `sql_databases` with `dl-agent sql-add` (connection only in env, never argv/log);
   - downloads and runs the hub `install.ps1` with `-Code <code> -Channel canary`;
   - runs each step `dl-agent <setup> <args>` with its `env` set for that process only and
     removed in `finally`; env values may contain `{{SQL:<DbName>}}` which the script replaces
     with the connection string for that database built from the prompted server/login;
   - a failed step is printed and the script continues with the next one (each module is
     independent), collecting a summary;
   - finally runs `dl-agent selftest --trigger install` (built in parallel, brief ST; if the
     binary lacks `selftest`, print that and continue) and prints the step summary + selftest
     table, writing a transcript to `%ProgramData%\dl-agent\bench-<date>.log` **without
     secrets**.
   Never echo env values, codes or passwords.
4. **Agent** `POST /v1/agent/selftest` (signed exactly like `/reports`, same prefix): body
   `{ run_id, trigger, started_at, finished_at, agent_version, os, hostname, checks: [{ module,
   id, title, status: pass|warn|fail|skip, detail, remedy?, duration_ms }] }`; validate with the
   repo's schema lib, cap size (e.g. 256 KB / 500 checks), store, keep the last 20 runs per agent.
   Admin: `GET /admin/agents/:id/selftest` (latest + history summaries) and a "Tự kiểm tra"
   section on the `/admin` status page (latest run per agent: counts pass/warn/fail/skip, failed
   checks listed). Admin `POST /admin/commands` with type `selftest` must already work through
   the generic command path — confirm and document.
5. **Docs**: `services/agent-hub/README.md` section "Bench (test PC)"; CHANGELOG.

## Checks before commit

`pnpm typecheck` (or the repo's command) + the hub test suite for touched files (vitest),
`pwsh` parse of a rendered bench script if `pwsh` exists, `wrangler deploy --dry-run`.
Heavy jobs via `HEAVY_LOCK=/tmp/heavy-dl-bench ~/.claude/scripts/heavy.sh`. Files ≤ 800 lines.

## Rules

Only this worktree. Conventional Commits in English with trailer
`Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and
`Claude-Session: https://claude.ai/code/session_01DAn3zia3PHZ2jf2uZQSp3k`.
No push, merge, deploy, no wrangler secret/migration against prod. Never print secrets. Report to
`docs/plans/2026-11-19-bench-report.md`: routes, migration number, secret names to set, test
counts, gaps (no real Windows run).

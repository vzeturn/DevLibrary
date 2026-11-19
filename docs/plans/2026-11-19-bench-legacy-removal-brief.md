# Brief BL: bench removes old agents first + SQL part placeholders + einvoice token env

RT-0164. The owner does not remember which old per-product agents run on the test PC and said:
"cứ thực hiện remove hết trước khi cài mới" (remove them all before installing). Merged bench code:
`services/agent-hub/src/install/benchScript.ts`, `src/domain/bench.ts`, `src/services/bench.ts`,
tests `test/bench.test.ts`; report `docs/plans/2026-11-19-bench-report.md`.

## 1. Hub bench: `remove_legacy` (default true for new bundles)

Before SQL fixups/install, when `remove_legacy` is true, for every old agent found:
- Windows services matching the existing `LEGACY_SERVICE_PATTERN` (never `dl-agent`): stop
  (wait ≤ 30 s, then kill the service process by PID), `sc.exe delete`.
- Scheduled tasks of those agents (watchdog/updater tasks that could reinstall or restart them):
  match by task name/path/action against the same pattern, `Unregister-ScheduledTask` (fallback
  `schtasks /Delete /F`).
- Their data/identity directories: **rename, never delete**, to `<dir>.bench-old-<yyyyMMddHHmmss>`.
  Find the exact directories from each product's own installer (read-only in the repos:
  `~/Projects/SRBackup` (read-only git show origin/main, never touch its working tree),
  `~/Projects/SMILE-HLAPI`, `~/Projects/MM-EINVOICE`, `~/Projects/SmileCheckin`,
  `~/Projects/MM-MEMBERSHIP`, `~/Projects/SMILE-PMS`, `~/Projects/SML-PDAOrder` — install scripts
  under `deploy/`, `scripts/`, `wwwroot/downloads/`), and the dl-agent enroll tools' adoption paths
  (`crates/dl-agent-*/src/**/enroll*`, e.g. `C:\ProgramData\srbackup\agent.json`,
  `%ProgramData%\SMILE-HL-Agent`, `%ProgramData%\MM.Membership.Agent`). Put the list in one
  table in the script with a comment per entry citing the source file.
- Print every action (service, task, dir → new name) and add each to the summary; write the
  list to `%ProgramData%\dl-agent\bench-removed-<ts>.json` and print a one-line restore hint.
- Any failure to stop/delete stops the bench before install (code not spent), with a clear message.
- Never touch SQL Server services, `dl-agent`, or anything not matched.
Drop the "compare legacy state at the end" warn logic for removed services (keep it for the
`remove_legacy: false` path).

## 2. Hub bench: SQL part placeholders (env only)

Add `{{SQL_SERVER}}`, `{{SQL_USER}}`, `{{SQL_PASSWORD}}`, `{{SQL_AUTH}}` (`sql`|`windows`) resolved
from the single SQL prompt, for tools that take separate fields (E-Invoice uses `MMEI_SQL_HOST`,
`MMEI_SQL_USER`, `MMEI_SQL_PASSWORD`; check `crates/dl-agent-einvoice/src/enroll/tool.rs` for the
exact names and Windows-auth support). Validation in `domain/bench.ts` (env only, refused in args),
masking (password is a secret), tests.

## 3. dl-agent `einvoice-enroll`: token from env

`crates/dl-agent-einvoice/src/enroll/tool.rs`: accept the token from env `DL_AGENT_EINVOICE_TOKEN`
when `--token` is absent (argv still wins; document both). If Windows auth (no SQL user) is not
supported by the tool, add it only if the collector code already supports integrated auth;
otherwise say so in the report. Tests for the env path.

## Checks

Hub: `pnpm typecheck`, vitest for touched tests, pwsh parse of a rendered script (with
remove_legacy on). Rust: `cargo fmt --check`, `cargo test -p dl-agent-einvoice`, clippy host +
windows-gnu for that crate. Heavy jobs via `HEAVY_LOCK=/tmp/heavy-dl-bl ~/.claude/scripts/heavy.sh`.
Files ≤ 800 lines (benchScript.ts is 442 — split the legacy-removal PowerShell into its own
module file if it would pass ~700). CHANGELOG entries.

## Rules

Worktree `~/Projects/DevLibrary-bl`, branch `feat/bench-remove-legacy` from origin/main (79aa03f+).
Conventional Commits, English, trailer
`Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and
`Claude-Session: https://claude.ai/code/session_01DAn3zia3PHZ2jf2uZQSp3k`.
No push/merge/deploy. Never print secrets. Report `docs/plans/2026-11-19-bench-legacy-removal-report.md`
(directory table with sources, what is unverified on real Windows, test counts). Final message ≤ 10 lines.

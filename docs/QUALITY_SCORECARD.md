# Quality scorecard

## Re-score 2026-11-25 (RT-1180)

Rubric: `~/.claude/docs/quality-rubric-100.md` (12 dimensions, 100/50/0 % per criterion).

- Total: **56.0 / 100** (raw 51.5 / 92, D10 N/A and re-weighted by 100/92). Band: **< 70, not ready for wide rollout**.
- Previous score: none (no earlier docs/QUALITY_SCORECARD.md). Delta: n/a, no regression.
- Commit scored: `5de5f4e` (main).
- Caps: no load test and no restore drill ever run, so the max is 89; not binding at this score. No Critical money/security gap was found, so the 79 cap is not applied.
- N/A: D10 (8 pts). The repo ships libraries, a Windows agent and a Cloudflare hub API, with no end-user UI; the rest is re-weighted.
- Gate results (run in cloud, Linux):
  - `cargo fmt --all --check`: pass.
  - `cargo clippy --workspace --all-targets --offline -- -D warnings`: pass, 0 warnings.
  - `cargo test -p dl-decimal`: pass (23 doctests plus unit tests).
  - Full `cargo test --workspace` not run in cloud: too long for the 5-minute budget, and `crates/dl-agent/tests/sql_live.rs` / `sql_setup_live.rs` need a real SQL Server. About 3690 `#[test]` functions were counted by grep, not executed.
  - agent-hub vitest suite (`services/agent-hub/test/`): not run in cloud, dependencies not installed.

| Dim | Score | Max | Evidence |
|---|---:|---:|---|
| D1 Business fit | 9 | 12 | `docs/plans/2026-11-20-dl-agent-parity-matrix.md`, module reports in `docs/plans/`; exceptions partial; pilot only via `scripts/release/client-copy.sh` (chưa kiểm chứng at a real site) |
| D2 Money/data integrity | 6.5 | 12 | `crates/dl-decimal` (7 rounding modes, tests); run-once restores in `docs/plans/2026-11-19-backup-module-report.md`; `services/agent-hub/migrations/0007_diagnostics_audit.sql` + `test/adminAudit.test.ts`; idempotency not proven for every write (chưa kiểm chứng) |
| D3 Security | 6 | 12 | `unsafe` denied in `Cargo.toml`; `dl-jwt`, `dl-otp`, `dl-hash`; `docs/decisions/0005-central-vault.md`, `0006-vault-owner-held-seal.md`; `SECURITY.md`; zero third-party deps + `Cargo.lock`; no SAST, no cargo-audit/SBOM in `.github/workflows/ci.yml`, no threat model or pentest |
| D4 Reliability | 6.5 | 10 | `crates/dl-agent-arrival/src/agent/journal.rs` (outbox/journal), problem reports (`migrations/0012_report_command.sql`), back-off in backup syncloop; backup exists but no restore drill record, no RPO/RTO |
| D5 Architecture | 5.5 | 8 | Cargo workspace of 40+ crates; 9 ADRs in `docs/decisions/`; numbered migrations `0001`..`0013`; `crates/dl-agent-pda-sync/tests/commands_executor.rs` is 1687 lines (over 800); no layering check |
| D6 Testing | 3 | 9 | ~3690 `#[test]`, `services/agent-hub/test/*.test.ts`, doctests per README; live-SQL tests not run in cloud; no coverage ratchet, no property/mutation/chaos |
| D7 Performance | 1.5 | 7 | `crates/dl-bench`, `docs/plans/2026-11-19-bench-report.md` (client measurement, not a load test); no p95 budget, no load test, no query-plan review |
| D8 Observability | 3.5 | 8 | `crates/dl-log` (structured logs); heartbeat + selftest (`docs/plans/2026-11-19-selftest-report.md`); no metrics dashboards, no SLO alerts, no postmortem process |
| D9 Release | 4.5 | 6 | `.github/workflows/ci.yml`, `release.yml`; `scripts/release/canary.sh`; `migrations/0005_update_lanes.sql`, `0006_kill_switch.sql`; `CHANGELOG.md` (Keep a Changelog); no documented migration rollback |
| D10 UX/a11y/i18n | N/A | N/A | No end-user UI in this repo |
| D11 Compliance | 2 | 4 | `crates/dl-vn` (tax code, amount in words, VietQR), `crates/dl-agent-einvoice`; PCI scope and PDPL not documented (chưa kiểm chứng) |
| D12 Docs | 3.5 | 4 | `README.md`, per-crate READMEs with compiled examples, `CONTRIBUTING.md`, `services/agent-hub/README.md`, ADRs + CHANGELOG; no dedicated ops/backup/incident doc |

## Top 5 gaps (points / effort)

1. Add `cargo audit`/`cargo deny` and an SBOM step to CI (+0.5 to +1 D3, small effort).
2. Write a restore drill record (restore a dl-agent-backup file on a test SQL Server) and publish RPO/RTO (+2 to +3 D4, lifts the 89 cap, medium effort).
3. Define p95 budgets and run one load test of agent-hub (`wrangler dev` + k6) with numbers in docs (+3 to +4 D7, medium effort).
4. Add a coverage threshold (cargo-llvm-cov ratchet) and property tests for `dl-decimal` / `dl-json` (+2 to +3 D6, medium effort).
5. Add `docs/OPERATIONS.md` (runbook, alert recipients, incident and postmortem steps, SLOs) and split files over 800 lines (+2 across D8/D5, medium effort).

Next re-score due: 2026-12-09.

# dl-agent handoff (RT-0164, 2026-11-18)

## Done
- `PROTOCOL.md` v1 and `INVENTORY.md` (9 agents, feature matrix, risks, migration order).
- Crate complete: identity/signing, hub client (clock skew, fallback, revoke), signed update with
  probation/rollback/blacklist, supervisor + `sys` module, reports spool, remote config with
  last-known-good, commands, CLI (`enroll`, `run`, `install`, `uninstall`, `status`) with
  Windows service and systemd install. `cargo test -p dl-agent`, clippy `-D warnings`, fmt, `cargo wasm` green.
- Hub `services/agent-hub` (branch `feat/agent-hub`): review items settled — per-product release line
  (`product` in heartbeat), `disabled` modules allowed by the gate (PROTOCOL wording), first release
  passes without force when canary is clean, revoke via `401 revoked` (agent wipes identity),
  Cloudflare Access for `/admin` documented in its README.

- Hub in production at https://agent.pms.id.vn (PR #6); release key offline under
  `~/.config/devlibrary-agent-hub/`; dl-agent 1.0.1 on canary, promoted after the soak.
- SQL Server bridge (task 577, PROTOCOL §9): pools per database from `sql.json`, `sql` module,
  `ctx.sql(name)`, `sql-add`/`sql-test`. Live tests: `DL_AGENT_SQL_LIVE=… cargo test -p dl-agent
  --test sql_live -- --test-threads=1` against `mcr.microsoft.com/mssql/server:2022-latest`
  (macOS has no TLS backend: use `Encrypt=disable` locally).

## Remaining
1. Module migrations per `INVENTORY.md` §5 — tasks of RT-0164 (task 557), each on a demo tenant first.

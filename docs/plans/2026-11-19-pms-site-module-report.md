# Report P1: `pms-site` module (RT-0164 task 557, step 2)

- **Status:** DONE. Commit `181583d` on `feat/dl-agent-pms` (not pushed): crate `crates/dl-agent-pms`, registered in `dl-agent-bin`, with `pms-enroll` and `pms-migrate` and docs (crate README, dl-agent Modules table, CHANGELOG).
- **Tests:** `cargo test -p dl-agent-pms -p dl-agent-bin` passes 77 tests: 58 unit, plus `bundle_golden`, `fake_cloud`, `module_lifecycle`, `ops_contract`, `wire_fixtures` and the drift test. The drift test against SMILE-PMS's manifest also passes. `sql_integration` passed 4/4 against a `mssql/server:2022-latest` container, since removed. fmt and clippy are clean (`-D warnings`, on the host and on `x86_64-pc-windows-gnu`).
- **Gap, `ctx.sql(name)`:** to use it, swap `db::Pool` (behind the executor, migration runner and retention) for an adapter over dl-agent's `sql` pools. The `MM_PMS` / `SMILE_*` connection strings would move from `databases.json` into the hub's sql config. The adapter must keep the per-op timeouts, deadlock retry and result caps. The backup and migration steps need one dedicated connection.
- **Gap, DPAPI:** `databases.json` is still plaintext (0600 file, ACL on the state dir). DPAPI for it is out of scope.
- **Deviations from the port:**
  - Tool flags use two dashes (dl-cli).
  - A tool failure exits with dl-agent's EXIT_CONFIG code, not 1.
  - The `hello` `agent_version` is clipped to 32 characters.
  - There is an extra report code, `pms_config_invalid`.
  - A terminal stop parks the module degraded with the next step, instead of exiting.
  - `update` frames are logged and ignored.
  - Nothing is written under `ctx.data_dir()`, which is already `modules/pms-site`.

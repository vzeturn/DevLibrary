# Brief ST: `dl-agent selftest` — on-site self check of every module, uploaded to the hub

RT-0164. The owner will install dl-agent (canary) on a real Windows test PC that has SQL Server
with a copy of hotel SMILE databases, with **every module enabled at once**, each module enrolled
against that product's demo tenant. They want the agent to test itself completely on that machine
and send the result to the hub, so we can fix every problem without them copying logs.

## What to build (worktree `~/Projects/DevLibrary-st`, branch `feat/dl-agent-selftest`)

1. **Contract in `dl-agent`**: a self-test check model
   `Check { module, id, title, status: pass|warn|fail|skip, detail, remedy: Option, duration_ms }`
   and an optional hook on `Module` (default impl returns the module's last health as one check)
   or a separate per-module `SelfTest` registration on the app builder — pick what fits the
   existing `supervisor.rs` / `tool.rs` design without breaking current modules. A module whose
   config is absent reports `skip` with the reason ("not enrolled"), never `fail`.
2. **Core checks** (dl-agent itself): binary version + channel; identity present; data dir
   permissions (0600 files on Linux, ACL owner on Windows); Windows service installed + running +
   recovery actions; watchdog scheduled task present; clock skew vs hub; hub reachable and the
   signed heartbeat accepted; update manifest fetch + signature verify for the current channel
   (no install); report spool writable and flushable; every database in `sql.json`: connect,
   survey warnings (reuse C3), schema probe against each enabled module's requirements (reuse C4).
3. **Module checks** — one function per crate, reusing existing code paths, read-only unless
   noted, each bounded by a timeout:
   - `einvoice`: config.enc decrypts; cloud auth ping; FO and POS schema probes; one collector
     read of pending rows (count only, no push).
   - `pms-site` / `pms-device`: enroll config present; databases.json present + each DB connects;
     cloud reachable; run op `pos.checkVersion.get` locally.
   - `hotellink`: config decrypts (DPAPI on Windows); hub reachable with the agent token; queue
     readable; SQL queries the module uses parse/run with `SET FMTONLY`-free read-only probes.
   - `backup`: each destination: write + read back + delete a small probe object; SQL BACKUP
     permission; COMPRESSION supported or correctly disabled (Express/Web).
   - `arrival`: identity, tenant key, vault readable; cloud reachable **over HTTP/1.1** (report
     the negotiated protocol); PMS read query runs.
   - `membership`: read-side queries run against the SMILE DBs; write side reported `skip`
     while `read_only` is true.
   Check `crates/dl-agent-*/` for the real names of configs and functions; do not invent new
   config files.
4. **Entry points**: CLI `dl-agent selftest [--data] [--module <name>] [--json] [--no-upload]`
   prints a readable table (status, module, title, detail, remedy) and exits non-zero on any
   `fail`; remote command `selftest` (sys module) runs the same in the service and returns the
   result as the command result. Both upload the run to the hub.
5. **Upload**: signed `POST /v1/agent/selftest` (look at how `/reports` and heartbeat are signed
   in `hub.rs`; use the same path prefix the agent uses for those) with body
   `{ run_id, trigger: "cli"|"command"|"install", started_at, finished_at, agent_version, os,
   hostname, checks: [Check...] }`. The hub side is built in parallel (brief BENCH) with exactly
   this shape; a 404 from an older hub is logged and ignored. Never put secrets, connection
   strings or tokens in `detail`.

## Checks before commit

`cargo fmt --check`, `cargo test` for the touched crates, `cargo clippy -D warnings` host and
Windows target (see how earlier slices ran it: `docs/plans/*report.md`), `cargo wasm` alias still
passes. Heavy jobs via `HEAVY_LOCK=/tmp/heavy-dl-st ~/.claude/scripts/heavy.sh`. Files ≤ 800 lines.
CHANGELOG entry.

## Rules

Only this worktree. Conventional Commits in English with trailer
`Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and
`Claude-Session: https://claude.ai/code/session_01DAn3zia3PHZ2jf2uZQSp3k`.
No push, merge, deploy. Never print secrets. Report to
`docs/plans/2026-11-19-selftest-report.md`: checks per module, what is only verified on Linux,
test counts, open gaps.

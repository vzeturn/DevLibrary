# Brief B2c: `backup-enroll` adopts the Go agent's identity (RT-0164 task 557, step 4)

## Why

The SRBackup hub now hands out the dl-agent installer (SRBackup PR #4,
`docs/dl-agent-installer.md` there): `install.ps1 -Setup backup-enroll -Replace SRBackupAgent`, enroll
key in env `DL_AGENT_BACKUP_KEY`, `--hub-url <origin>` in `DL_AGENT_SETUP_ARGS`. Today
`backup-enroll` always calls `POST /api/agent/self-register`, which creates a **new** Pending agent.
On a machine that already runs the Go agent, the hub's SQL servers, schedules and history are tied
to the old `agent_id` (`hub/src/db/schema.ts`, `agentId` references), so an upgrade would orphan
them. The switch must be in place: the dl-agent must continue as the same hub agent.

## What to build (crate `crates/dl-agent-backup`, module `enroll/`)

1. **Read the Go agent config** (read only; reference `~/Projects/sr-backup/agent/internal/config/config.go`,
   `internal/secret/secret.go`, `internal/hwid/hwid.go` — read only, the SRBackup main checkout has
   another session's uncommitted work, never write there):
   - Path search exactly as Go: `%ProgramData%\srbackup\agent.json`, then
     `C:\ProgramData\srbackup\agent.json`, then `%APPDATA%\srbackup\agent.json` (check Go's order
     and conditions); an override for tests.
   - Fields: `server_url`, `agent_id`, `api_key` (sealed `enc:v1:` + base64(nonce‖ciphertext‖tag),
     AES-256-GCM), `master_key` (sealed the same way, see `secret.Sealed`). Plaintext `api_key`
     (no prefix) is accepted as Go does (`ErrNotEncrypted` path).
   - Key = HKDF-SHA256 extract (zero salt, IKM = machine id) + one expand block with info
     `srbackup-agent-at-rest-v1`. Machine id = Go `hwid.Get()`: Windows `MachineGuid` from
     `HKLM\SOFTWARE\Microsoft\Cryptography` (Go shells out to `reg query`; read the registry
     directly via `dl-winsvc`/windows APIs already in the workspace if available, else the same
     `reg query` parse), fallback = hex SHA-256 of the hostname — copy Go's exact fallback format.
     Off Windows: a test seam supplies the machine id.
   - Use crypto crates already in the workspace (check `Cargo.toml`; repo dependency policy, no new
     third-party crates). If AES-GCM/HKDF are not available, stop and say so in the report.
2. **Adopt in `backup-enroll`**: when no dl enrollment exists (or `--force`), and a Go config is
   found whose `server_url` has the same origin as `--hub-url`, and `agent_id` + `api_key` open:
   verify them against the hub (`GET /api/agent/config` with the Go agent's auth — copy how the Go
   agent and `crates/dl-agent-backup` hubclient authenticate), and on 2xx store them in
   `enrollment.enc` exactly like a fresh enrollment (keep the master-key handling the module
   already does; if the module fetches the master key itself, do not import Go's). Print
   `adopted agent <id> from the Go agent`. Do **not** call self-register in that case.
   - Hub rejects (401/403) or config unreadable/different origin → fall back to self-register with
     the enroll key as today, and say why in one line (no secret in it).
   - Never modify or delete the Go `agent.json` (the installer's `-Replace` only removes the
     service; the file stays as the rollback path).
   - The hwid sent later (if any) must not make the hub treat it as a different machine — check
     whether the hub uses `hwid` after registration; report what you found.
3. README of the crate: an "Upgrading from the Go agent" section.

## Tests (hand-written fakes, mirror source structure)

- Golden: a Go program in your scratch dir (not committed to SRBackup) that seals a known
  `api_key`/`master_key` with a fixed machine id via the real `secret` package (copy the package
  into the scratch program or `go run` against a temp copy) → fixture JSON committed in the crate;
  Rust opens it byte-for-byte. Include a plaintext `api_key` case and a wrong-machine-id case
  (must fail cleanly, then fall back).
- Fake hub on a local socket: adopt (config 200 → no self-register call), adopt rejected (401 →
  self-register), different origin → self-register, existing dl enrollment without `--force` →
  untouched.
- Never print the key, api_key or master key; assert an error string does not contain them.

## Checks before commit

- `~/.cargo/bin/cargo fmt --all`
- `~/.cargo/bin/cargo test -p dl-agent-backup -p dl-agent-bin`
- `~/.cargo/bin/cargo clippy -p dl-agent-backup -p dl-agent-bin --all-targets -- -D warnings`, and
  the same with `--target x86_64-pc-windows-gnu`.
- Every file ≤ 800 lines. No swallowed errors. Heavy builds via
  `HEAVY_LOCK=/tmp/heavy-dl-bk3 ~/.claude/scripts/heavy.sh` (never kill another session's job).

## Rules

- Work only in worktree `~/Projects/DevLibrary-bk3`, branch `feat/dl-agent-backup-adopt`.
  Conventional Commits in English, trailer:
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01DAn3zia3PHZ2jf2uZQSp3k`.
  **Do not push, merge or deploy.** Do not modify SRBackup. Never print secrets.
- Report to `docs/plans/2026-11-19-backup-adopt-report.md`: what was built, the machine-id source on
  Windows, the hwid finding, crypto crates used, gaps (no real Windows run), test counts.

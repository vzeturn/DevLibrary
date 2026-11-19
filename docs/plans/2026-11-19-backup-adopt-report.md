# Report B2c: `backup-enroll` adopts the Go agent's identity (RT-0164 task 557, step 4)

Status: DONE_WITH_CONCERNS (no run on a real Windows machine; see Gaps).
Branch `feat/dl-agent-backup-adopt` in worktree `~/Projects/DevLibrary-bk3`, not pushed.

## What was built

- `crates/dl-agent-backup/src/enroll/goagent.rs` (new): read-only port of the Go agent's
  `internal/config` discovery, `internal/secret` and `internal/hwid`.
  - `candidates()`: Go's Windows order `%PROGRAMDATA%\srbackup\agent.json` (or
    `C:\ProgramData\srbackup\agent.json` only when `PROGRAMDATA` is unset, exactly Go's
    if/else), `%APPDATA%\srbackup\agent.json`, `%LOCALAPPDATA%\srbackup\agent.json`. Go's third
    candidate (the Go executable's own directory) is left out: this binary cannot know where the
    Go exe was; the installer keeps the file in ProgramData. Off Windows Go's unix list
    (`~/.srbackup/agent.json`, `/etc/srbackup/agent.json`). Tests pass paths explicitly
    (`GoSource { paths, machine }`).
  - `find()`: first candidate that reads, parses and has a non-empty `server_url` (Go's
    auto-discovery rule). Missing files are skipped silently; unreadable/unparseable ones are
    skipped and reported if nothing usable is found.
  - `open_value()`: `enc:v1:` + base64 std (nonce 12 ‖ ciphertext ‖ tag), AES-256-GCM; key =
    HKDF-SHA256 extract (32 zero bytes salt, IKM = machine id) + one expand block, info
    `srbackup-agent-at-rest-v1`. No prefix (or bare `enc:v1:`, as Go's `IsEncrypted`) =
    plaintext, as Go's `MustDecrypt`. Errors never contain the value or the machine id.
  - `MachineId::current()`: Windows `MachineGuid` read directly with
    `dl_winos::registry` (`HKLM\SOFTWARE\Microsoft\Cryptography`, `View::Wow64_64` = what
    the amd64 Go agent's `reg query` sees); fallback `host-` + hex of the first 8 bytes of
    SHA-256(hostname), Go's exact format (`fmt.Sprintf("host-%x", sum[:8])`), with the reason
    kept so a decrypt failure says the fallback was used.
  - `same_origin()`: scheme, host (case-insensitive) and effective port equal, no path.
- `enroll/mod.rs`: `enroll(..., go: &GoSource)`. When no dl enrollment exists (or `--force`):
  Go config found + same origin + key opens → `HubClient::check_token()` (`GET
  /api/agent/config` with `Authorization: Bearer <api_key>`, same as the Go hubclient and the
  crate's client) → on 2xx store `enrollment.enc` (hub url, Go agent id, Go api key, hwid =
  previous dl hwid or a fresh random id, as before) → `Outcome::Adopted`, prints
  `adopted agent <id> from the Go agent`; no self-register.
  - 401/403, config unreadable/sealed elsewhere, different origin → self-register as today,
    plus one line `not adopting the Go agent: <reason>` (no secret; the other origin is shown
    as `scheme://host:port` only).
  - Hub unreachable / 5xx while checking → error "nothing changed" and no self-register
    (a new registration would orphan the site). This is a choice beyond the brief's letter;
    the brief only listed 401/403 as fallback.
  - The Go `master_key` is not imported: the module already fetches `/master-key` itself.
  - The Go `agent.json` is only read, never written or deleted (test asserts bytes unchanged).
- `enroll/tool.rs`: `run_args` takes `&Machine { name, go }` (keeps clippy's argument limit);
  the real tool passes `GoSource::current()`.
- `hub/mod.rs`: `HubClient::check_token()`.
- `README.md` of the crate: "Upgrading from the Go agent"; the "Differences" bullet updated.
  `CHANGELOG.md`: Unreleased/Added entry.

## Machine id source on Windows

Registry API (`dl_winos::registry::Key::open(Hive::LocalMachine,
"SOFTWARE\Microsoft\Cryptography", View::Wow64_64).string("MachineGuid")`), not `reg query`.

## hwid finding

The hub stores `hwid` only at registration (`routes/agent.ts` `/register` and
`/self-register` insert it; `db/schema.ts` column `hwid notNull`) and only displays it
(`routes/agentLogs.ts`). `requireAgentAuth` identifies the agent by the token hash alone; no
route matches or compares `hwid` after registration. Adoption makes no register call, so the
hub's agent row (and its hwid) is unchanged; the dl enrollment keeps its own random hwid, only
used if a later `--force` falls back to self-register.

Observation (not changed): the Go agent sends `hwid.Get()` (the MachineGuid) to the hub as
`hwid`, and that same value is the only input of its at-rest key, so anyone with the hub's
`agents` table and a copy of `agent.json` can open the Go api_key. dl-agent does not repeat
this (random hwid; DPAPI for `enrollment.enc`).

## Crypto

No new crates. AES-256-GCM: the crate's own `crypto::gcm` (already checked against FIPS 197,
the GCM test cases and hub fixtures). HKDF: `crypto::hkdf_sha256` on `dl_hash::Hmac<Sha256>`.
SHA-256: `dl_hash`. Base64: `dl_encoding::base64::STANDARD`.

## Tests

- Golden: a scratch Go module with verbatim copies of the agent's `internal/secret` and
  `internal/config` and a stub `internal/hwid` (`FAKE_MACHINE_ID`) wrote `agent.json` through
  the real `config.Save` (and round-tripped it through `config.Load`). Committed:
  `crates/dl-agent-backup/tests/fixtures/go-agent/{sealed.json,plaintext.json,make-go-agent.go.txt}`
  (test-only values). SRBackup was not modified.
- `enroll/goagent_tests.rs` (10): opens Go-sealed api_key and master_key byte-for-byte; wrong
  machine id fails without leaking; plaintext and edge values; hostname fallback equals Go's
  printed `host-90b07b80f2ff6f69`; discovery order; wrong id error names the fallback;
  origin matching; candidate order.
- `enroll/adopt_tests.rs` (6, fake hub on a local socket): adopt (config 200 → 0
  self-register, file bytes unchanged); 401 → self-register; other origin → self-register and
  the Go key never sent; config sealed elsewhere → self-register; existing dl enrollment
  without `--force` untouched, with `--force` adopted keeping the hwid; 503 → error, nothing
  stored, no self-register. Every output/error asserted free of api_key, machine id, enroll
  key and new token.
- Existing callers (`enroll/tests.rs`, `module/tests.rs`, `sync/tests.rs`) pass
  `GoSource::none()`.

Commands (all green):
- `~/.cargo/bin/cargo fmt --all` (and `--check`)
- `HEAVY_LOCK=/tmp/heavy-dl-bk3 ~/.claude/scripts/heavy.sh ~/.cargo/bin/cargo test -p dl-agent-backup -p dl-agent-bin`
  → dl-agent-backup 237 passed, 0 failed (16 new); dl-agent-bin 0 tests.
- `... cargo clippy -p dl-agent-backup -p dl-agent-bin --all-targets -- -D warnings` → clean.
- `... cargo clippy ... --all-targets --target x86_64-pc-windows-gnu -- -D warnings` → clean.
- All touched files ≤ 274 lines.

## Gaps / concerns

- No run on real Windows: the registry read, the `%PROGRAMDATA%` path and an actual Go
  `agent.json` sealed with a real MachineGuid are untested end to end (Windows code is
  clippy-checked only). Suggest one manual upgrade on a test site.
- Hostname fallback: Go's `os.Hostname()` on Windows is `ComputerNamePhysicalDnsHostname`;
  this port uses `dl_agent::hub::hostname()` (`COMPUTERNAME`, NetBIOS, upper case). Only
  matters when MachineGuid cannot be read; then adoption fails cleanly and falls back to
  self-register (the message names the fallback).
- Go applies env overrides (`SRBACKUP_SERVER`, `SRBACKUP_API_KEY`, `SRBACKUP_AGENT_ID`) on
  top of the file; they are not read here (the installer runs with its own env, and the Go
  service env is not visible to it).
- Go fails a whole config whose `master_key` does not open; this port ignores `master_key`, so
  a file with a good api_key but a broken master_key is still adopted (harmless: the module
  fetches the master key from the hub).
- Go's exe-directory candidate is skipped (see above).

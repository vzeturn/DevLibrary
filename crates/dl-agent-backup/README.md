# dl-agent-backup

SQL Server backups for the SRBackup hub as a [dl-agent](../dl-agent/README.md) module
(`backup`). It does the work of the SRBackup Go agent (`agent/`: `hubclient`, `syncloop`,
`scheduler`, `executor`, `queue`, `backup`, `uploader`, `retention`, `discovery`,
`jobhistory`) inside the shared agent: it polls the hub's config, runs the schedules that are
due (BACKUP → checksum → upload → cleanup), applies retention, runs restore requests and
hub commands, and reports every outcome back.

The Go agent's service host, updater, logger, health endpoint, metrics, hwid, secret store
and `cmd/agent` are not ported: dl-agent provides them.

## Turning it on

`dl-agent` (package `dl-agent-bin`) registers the module. It is opt-in: it stays `disabled`
until the remote config enables it for the site.

```json
{"modules": {"backup": {
    "enabled": true,
    "backup_dir": "D:\\SQLBackups",
    "concurrency": 1,
    "stop_grace_seconds": 10,
    "poll_interval_seconds": 60
}}}
```

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `false` | Run the module on this site. |
| `backup_dir` | the instance's backup directory (`InstanceDefaultBackupPath`) | Where SQL Server writes the `.bak` files (a path on the SQL Server machine). |
| `concurrency` | `1` (Go `DefaultConcurrency`), at most 8 | Backups running at once; never two on one instance. |
| `stop_grace_seconds` | `10`, at most 60 | How long a running `BACKUP` may go on after stop before it is cancelled. |
| `poll_interval_seconds` | `60` | How often the hub's config is read; failures back off to 5 minutes. |

The hub URL and agent token come from the enrollment, never from the remote config.

## Enrolling a site

```sh
DL_AGENT_BACKUP_KEY=… dl-agent backup-enroll --hub-url https://backupapi.example [--force]
# or: echo "$KEY" | dl-agent backup-enroll --hub-url https://backupapi.example
```

The tenant enroll key comes from `DL_AGENT_BACKUP_KEY` or one line of standard input, never
from the command line (argv is visible to every local user and lands in installer logs). The
tool calls `POST /api/agent/self-register` and stores the agent id and token sealed in
`<data>/modules/backup/backup/enrollment.enc`. Without `--force` an existing enrollment for
the same hub is kept and one for another hub is refused. A running module picks up a new
enrollment within 30 s, without a restart.

## Upgrading from the Go agent

The hub ties a site's SQL servers, schedules and history to its agent id, so an upgrade must
not register a new agent. The installer
(`install.ps1 -Setup backup-enroll -Replace SRBackupAgent`) removes the Go service and runs
`backup-enroll`, which first looks for the Go agent's `agent.json` in the Go agent's own order:
`%PROGRAMDATA%\srbackup\agent.json` (or `C:\ProgramData\srbackup\agent.json` when the
variable is unset), `%APPDATA%\srbackup\agent.json`, `%LOCALAPPDATA%\srbackup\agent.json`
(Go also looks next to its own executable, which this binary cannot know). The first file that
parses and has a `server_url` is used.

Its `api_key` is opened the way the Go agent seals it (`enc:v1:` + AES-256-GCM under
HKDF-SHA256 of the Windows `MachineGuid`, or of `host-<hash>` when the registry cannot be
read); a plaintext key from an older agent is used as it is. When `server_url` is the same
origin as `--hub-url` and the hub accepts the key on `GET /api/agent/config`, the agent id and
key are stored in `enrollment.enc` and the tool prints `adopted agent <id> from the Go agent`;
no self-registration happens. The Go agent's `master_key` is not imported: the module fetches
the per-agent key from the hub as it does after a fresh enrollment.

A rejected key (401/403), a config that does not open (another machine, altered) or another
hub origin falls back to self-registration with the enroll key, and the tool says why in one
line. A hub that cannot be reached or answers 5xx stops the tool with nothing changed, since a
new registration would orphan the site. The Go `agent.json` is never written or deleted: it
stays the rollback path (reinstall the Go service and it carries on as the same agent).

## How it runs

- **Not enrolled**: `degraded` with `not enrolled`; it waits for an enrollment or stop.
- **Sync thread** (Go `syncloop`): `GET /api/agent/config`; due schedules (cron in the
  schedule's time zone) are queued; restore requests and commands run once each; discovery
  (`POST /api/agent/discover`) on the first poll and every tenth; retention by age and count
  (destination copy, local file, history entry). The module turns `running` after the first
  successful poll and `degraded` when the hub is still unreachable after one back-off or
  rejects the agent (401/403, with a hint to run `backup-enroll --force`).
- **Workers** (Go `executor`): take jobs off the durable queue, one backup per SQL Server
  instance at a time, renew the job's lease while it runs, retry a failed job up to three
  attempts and report a job out of attempts once (`abandoned after 3/3 attempts: …`).
- **Credentials**: the hub seals server passwords and destination tokens under the agent's
  master key (`GET /api/agent/master-key`, AES-256-GCM under HKDF-SHA256, byte-compatible with
  the hub's `encryptCredential`). The key is kept in memory only; when a credential does not
  open it is fetched once more (key rotation). A credential the hub flags `credentialError`
  (sent as `""`) fails that schedule, restore or command with the reason, and the rest run.
- **Backup encryption** (per tenant): before an upload the module asks the hub for the
  tenant's current backup data key (`GET /api/agent/backup-key` → `{"keyId", "key"}`, 32 bytes
  base64url; fetched with each config poll, cached in memory for 15 minutes, and the last
  answer is kept through a hub outage). The copy sent to the destination is `<name>.bak.dlbk`:
  magic `DLBKENC1`, key id, random salt, chunk size, then 1 MiB chunks of AES-256-GCM under
  HKDF-SHA256(data key, salt, `dl-agent-backup/file/v1` ‖ header), nonce = chunk index, the
  final chunk flagged in the AAD (a cut, reordered or extended file is refused). The report
  carries `encryptionKeyId`. A restore reads the key id from the downloaded file and asks
  `GET /api/agent/backup-key?id=<keyId>`; the hub answers only for the agent's own tenant, so
  another tenant's backup fails with `backup_credential_error` before SQL Server is asked,
  and the plaintext is written to `….part` and renamed only after every chunk checked out. A
  hub that answers 404 (no per-tenant keys yet) gets the plaintext `.bak` uploaded as before
  (step `encrypt` = `skipped`); a downloaded file without the magic (every backup made before
  encryption) is restored as it is. The `.bak` the server writes locally stays plaintext, as
  SQL Server writes it. Throughput of the built-in AES: about 33 MiB/s each way (release,
  64 MiB, Linux x86-64), so a 10 GiB backup spends about five minutes encrypting.
- **Reports** (`POST /api/agent/report`) go through a sealed outbox: written to disk first,
  removed once the hub accepts them, sent again after every successful poll. A hub outage
  loses none.
- **Stop**: workers take no new job; a running `BACKUP` gets `stop_grace_seconds`, then its
  statement is cancelled with a TDS attention and the job goes back to the queue without
  spending an attempt.

State lives under `<data>/modules/backup/backup/` (`queue.enc`, `outbox.enc`, `history.enc`,
`runs.enc`, `enrollment.enc`), sealed with DPAPI in machine scope on Windows (owner-only files
elsewhere, for development).

## SQL Server

dl-tds connects with SQL logins or, for `authType` Windows, SSPI as the service account.
Database names and file paths are statement parameters; identifiers that must be spliced
(`MOVE` logical names, `RESTORE … WITH`) are validated and quoted. The connection probes the
edition and version once and adds `COMPRESSION` only where the edition supports it (not
Express, Web, Workgroup, 2005, or 2008 outside Enterprise/Developer) and `CHECKSUM` from SQL
Server 2005 on; the choice is written into the job's `backup` step. A log backup of a database
in SIMPLE recovery fails with `backup_recovery_model` before SQL Server is asked. The `.bak`
file is written by the SQL Server service; when that server is remote and the agent cannot read
the file, the error says so ("written by SQL Server on … but is not reachable from the agent").

## Destinations

| Provider | Notes |
|---|---|
| `LocalFolder` | Copy to a local or mapped folder (`remoteFolderPath`). |
| `AWSS3`, `S3`, `CloudflareR2`, `R2` | SigV4, multipart above 16 MiB, `endpoint` for R2/MinIO (https only). |
| `AzureBlob` | `connectionString` (`conn:` Shared Key / SAS, or `sas:` URL), block blobs above 32 MiB; the stored path carries no SAS token. |
| `GoogleDrive` | OAuth refresh token (`clientId`, `clientSecret`, `refreshToken`, optional `accessToken` / `tokenExpiresAt`), resumable upload in 32 MiB chunks (multiples of 256 KiB, an interrupted chunk is resumed), path `gdrive://<folder>/<name>`. |
| `OneDrive` | Same OAuth fields, Graph: simple PUT under 4 MiB, upload session in 10 MiB chunks (multiples of 320 KiB), above 20 MiB up to four parallel parts plus a `.odmanifest`; the redirect download carries no bearer token; path `onedrive://<path>`. |

An unknown provider still fails with `backup_destination_unsupported`. A refreshed or rotated access token lives in memory only, as in the Go agent. A revoked grant is `backup_credential_error`.

## Reports, metrics, commands

Problem reports are rate-limited (one per code and subject per 15 minutes) and scrubbed of
passwords, keys, signatures and tokens: `backup_hub_failed`, `backup_auth_failed`,
`backup_sql_failed`, `backup_upload_failed`, `backup_credential_error`,
`backup_destination_unsupported`, `backup_recovery_model` (plus `backup_schedule_invalid`,
`backup_io_failed`).

Metrics: `queue_depth`, `outbox_depth`, `jobs_ok`, `jobs_failed`, `dead_lettered`,
`bytes_uploaded`, `last_success` (per database, Unix ms).

Commands: `backup.status` (the metrics and the last outcomes) and
`backup.run-now {"scheduleId": "…"}` (queued on the next poll, which it brings forward).

## Live SQL Server tests

`BACKUP_AGENT_TEST_SQL` (`server=host,port;user id=sa;password=…;encrypt=disable`) and
`BACKUP_AGENT_TEST_DIR` (a directory the server and the test see at the same path, a bind
mount for a container) turn on `backup::sql_tests`, including `sql_tests::replace`: a database
with three tables goes through the real backup path (sealed upload to a `LocalFolder`
destination), is damaged, and comes back through the real restore path with
`RESTORE … WITH REPLACE` and under a new name; per-table row counts and
`CHECKSUM_AGG(BINARY_CHECKSUM(*))` must equal the values taken before the backup.

```sh
docker run -d --name bk-sql -e ACCEPT_EULA=Y -e MSSQL_SA_PASSWORD=… -p 14362:1433 \
  -v /tmp/bkdir:/tmp/bkdir mcr.microsoft.com/mssql/server:2022-latest
BACKUP_AGENT_TEST_DIR=/tmp/bkdir BACKUP_AGENT_TEST_SQL='server=127.0.0.1,14362;user id=sa;password=…;encrypt=disable' \
  cargo test -p dl-agent-backup sql_tests -- --test-threads=1
```

## Differences from the Go agent

- `COMPRESSION` / `CHECKSUM` only where supported (Go always sent `COMPRESSION`, which fails on
  Express and Web).
- Statement parameters instead of spliced names and paths.
- Uploaded backups are encrypted under a per-tenant key when the hub serves one (the Go agent
  uploaded the plaintext `.bak`).
- A failure no retry can fix (SIMPLE recovery for a log backup, an unsupported destination, a
  credential the hub could not open, a bad schedule) is reported and dropped at once instead of
  being tried three times.
- A `credentialError` or undecryptable credential is reported to the hub as a Failed backup
  with the reason; Go only logged it.
- Restores and commands run once per request; Go ran them on every poll until the hub saw the
  report.
- A 401/403 keeps polling (degraded) so a re-enrollment takes effect without a restart.
- The Azure cloud path no longer stores the SAS token.
- Time zones come from a small built-in table (zones without daylight saving such as
  `Asia/Ho_Chi_Minh`, fixed offsets like `+07:00` / `Etc/GMT-7`, and UTC) instead of the IANA
  database; another zone fails its schedule with `backup_schedule_invalid`.
- Only the Go agent's identity (agent id, API key) is imported; its queue and history are
  not: a site moving from the Go agent starts with an empty queue and history (the hub keeps the backup history; restores of older backups use
  their cloud copy).

## Tests

`cargo test -p dl-agent-backup` runs the ports of the Go tests, a fake hub on a local socket
(enroll → config → job → report, master key rotation, `credentialError`, hub down → outbox),
SigV4 against the AWS test vectors, Azure Shared Key / SAS against fixed vectors, credential
fixtures produced by the hub's `encryptCredential`, and the module lifecycle.

`BACKUP_AGENT_TEST_SQL` (`server=host,port;user id=sa;password=…`, a disposable SQL Server;
add `;encrypt=disable` on macOS and Linux, where dl-tds has no TLS backend) with `BACKUP_AGENT_TEST_DIR` (a directory the server sees at the same path, e.g. a container
bind mount) enables `src/backup/sql_tests.rs`: full backups with and without compression
(checked in `msdb.dbo.backupset`), a log backup on FULL and one on SIMPLE recovery, and
`RESTORE … WITH REPLACE` to a new name next to its online source.

```sh
docker run -d --name bk-sqltest -e ACCEPT_EULA=Y -e MSSQL_SA_PASSWORD="$PW" -p 14334:1433 \
  -v "$DIR:$DIR" mcr.microsoft.com/mssql/server:2022-latest
BACKUP_AGENT_TEST_SQL="server=127.0.0.1,14334;user id=sa;password=$PW;encrypt=disable" \
  BACKUP_AGENT_TEST_DIR="$DIR" cargo test -p dl-agent-backup sql_tests
```

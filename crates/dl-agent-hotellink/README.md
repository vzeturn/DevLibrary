# dl-agent-hotellink

SMILE Hotel Link as a [dl-agent](../dl-agent/README.md) module (`hotellink`). It does the
work of the SMILE-HLAPI Go agent (`go/internal/...`) inside the shared agent:

- long-polls the HL hub for booking jobs (new, modify, cancel, mapping) and keeps them in a
  durable queue (`queue.jsonl`) until the hub acknowledges them;
- reconciles each job into the hotel's `SMILE_FO` database (Folio, FolioRate,
  AdditionalName) in one transaction, with the Go agent's duplicate, manual-change and
  user-cancel rules;
- pushes room-type mapping and availability back to the hub, behind a circuit breaker.

The Go service host, self-update, log file and `cmd/agent` are not ported: dl-agent
supplies the service, updates and logs. The local config UI (`configui`) is not ported yet.

## Turning it on

The `dl-agent` executable (package `dl-agent-bin`) registers the module. It is opt-in and
stays `disabled` until the hub's remote config enables it:

```json
{"modules": {"hotellink": {"enabled": true}}}
```

`config_path` overrides the HL config file. By default the module reads
`%ProgramData%\SMILE-HL-Agent\config.json`, or the path in `SMILEHL_AGENT_CONFIG`. This is
the Go agent's file: token and SQL password are DPAPI-encrypted (`enc:` prefix). Without
a usable file or token the module stays `degraded` ("not configured: …") and checks the
file again every 30 s.

State lives in `<data dir>/hotellink/`. On first start the Go agent's unfinished
`queue.jsonl` (next to `config.json`) is imported once.

## Enrolling

```
set DL_AGENT_HL_TOKEN=...            (or type it on standard input)
dl-agent hl-enroll --hub-url https://... --hotel-id H1 --instance-id I1 [--sql-user U] [--force]
```

With `--sql-user`, the password comes from `DL_AGENT_HL_SQL_PASSWORD` or the next line of
standard input. Secrets are never accepted on the command line. A config that already
has a token is kept unless `--force` is passed.

## Health, metrics, commands

- `running` after the first successful poll; `degraded` on hub 401, another install
  taking over (403), an open breaker, the hub unreachable for more than one back-off,
  a stalled loop, or SQL unreachable for more than 30 s.
- Metrics: `jobs_polled`, `jobs_applied`, `jobs_acked`, `jobs_failed`, `manual_conflicts`,
  `availability_pushes`, `polls_ok`, `queue_depth`, `unacked`, `mapping_hash`,
  `mapping_count`, `last_sync_at`, `last_job_at`, `breaker_state`.
- Reports (at most one per code per 15 min): `hl_hub_failed`, `hl_auth_failed`,
  `hl_pms_failed`, `hl_reconcile_failed`, `hl_manual_conflict`. Reports carry no guest
  names, SQL text or token.
- Commands: `hl.status`, `hl.sync-mapping` (also as `hotellink.*`).

## Tests

```
cargo test -p dl-agent-hotellink
HL_AGENT_TEST_SQL="server=127.0.0.1,14333;user id=sa;password=...;database=master;encrypt=disable" \
  cargo test -p dl-agent-hotellink --lib pms::sql_tests
```

The SQL tests create `SMILE_FO` and load `tests/fixtures/smile-schema.sql` (a copy of
SMILE-HLAPI `go/deploy/smile-schema.sql`).

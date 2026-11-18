# Brief E2c: E-Invoice console installs the shared dl-agent (RT-0164 task 557)

Context: `docs/plans/2026-11-19-einvoice-e2-plan.md` (DevLibrary). Read decisions 4–6 there.
Repo: `~/Projects/MM-EINVOICE` ("Azolo E-Invoice"). Read its `CLAUDE.md` Gotchas first.

## Goal

The platform console (`/platform` → tenant → Agent tab → mint enroll token) can hand out a
one-liner that installs **dl-agent** (DevLibrary shared agent, module `einvoice`) instead of the
Go `mmei-agent`. The legacy path stays the default and is untouched; the new one is behind a
switch, because no dl-agent build with the `einvoice` module is on the stable channel yet.

## What exists

- `internal/webapi/enroll.go`: public `/agent/bootstrap/{token}[/remote]` serves a PowerShell
  script (text/template) that removes `*MM-EInvoice*` services and runs `mmei-agent -enroll`.
- `internal/webapi/app_platform_agent.go`: `POST /platform/tenants/{id}/agent/enroll-token`
  → `{token, expiresAt}` via `PlatformAgentService.MintEnrollToken`.
- `web/src/components/platform-tenant/AgentTab.tsx` `EnrollTokenBanner`: two commands
  (`irm <bootstrap> | iex`, and `/remote` for SQL Server on another machine).
- DL hub (already live): `POST https://agent.pms.id.vn/products/einvoice/enroll-codes`,
  header `Authorization: Bearer <product key>`, JSON body
  `{external_id, name, site_id?: "main", ttl_seconds?: 60..604800 (default 86400), demo?: false}`
  → `201 {id, code, tenant_id, site_id, expires_at (unix s), install_command}`;
  `external_id` must match `^[A-Za-z0-9._-]{1,64}$` and `einvoice-<external_id>` ≤ 64 chars
  (a UUID fits). Errors: 401 `unauthorized`, 400 with `{error, ...}`.
- DL `install.ps1` (served by the hub, PowerShell 2.0 safe) takes `-Code`, `-Channel
  stable|canary`, `-Setup <sub-command>`, `-Replace '<service pattern>'`. `-Setup` runs after
  enrolment with extra args from env `DL_AGENT_SETUP_ARGS` (whitespace separated; the installer
  removes the variable afterwards). `-Replace` deletes the old services only after setup works.

## Build

1. **Config** (wired in `cmd/server`): `MMEI_AGENT_INSTALLER` (`legacy` default, `dl` turns the
   new path on), `MMEI_DL_AGENT_HUB` (default `https://agent.pms.id.vn`), `MMEI_DL_AGENT_KEY`
   (secret; never logged, never in git). `dl` without a key: log an error at startup and stay
   on legacy. Document them where the repo documents env vars.
2. **`internal/dlagent`** (new, small): a client with `MintCode(ctx, tenantID uuid.UUID, name
   string, demo bool) (Code{Code string; ExpiresAt time.Time}, error)`; 10 s timeout; errors
   carry the HTTP status and the hub's `error` field, never the key. Port interface in webapi
   (no driver types leaking). Tests with `httptest.Server` (hand-written fakes, no mocking libs):
   success, 401, 400, timeout, key in header and not in the error text.
3. **Mint**: `mintPlatformEnrollToken` also mints a DL code when the switch is on: external id =
   tenant UUID, name = the tenant's display name (use an existing tenant lookup in the handler's
   services; fall back to the UUID only if none exists), demo = the tenant's `IsTest`
   (`tenantcfg.AgentConfig`). Response gains `dlCode`, `dlChannel` (`canary` when demo, else
   `stable`) or `dlError` (short message) when the hub call fails. A hub failure never fails the
   request: the MMEI token is still returned and the legacy commands still work; log it
   structured (tenant id, status, hub error code). The admin API `mintEnrollToken`
   (`admin_agent.go`) stays legacy.
4. **Wrapper route** (public, same handler as bootstrap, outside auth):
   `GET /agent/bootstrap/{token}/dl/{code}` and `.../dl/{code}/remote`, `?channel=canary`
   optional. **Validate** before templating: token and code must match strict charsets
   (look at how tokens and DL codes are generated: DL codes look like `XXXX-XXXX`, letters,
   digits and `-`); otherwise 404 with no script. Also apply the token check to the existing
   bootstrap routes (a crafted path must never put PowerShell into a script from our domain).
   Script (PowerShell 2.0 safe: WebClient, no `irm`, no `Invoke-WebRequest`):
   - TLS 1.2 on (`[Net.ServicePointManager]::SecurityProtocol -bor 3072`).
   - `$env:DL_AGENT_SETUP_ARGS = '--token <token> --platform-url <public base URL>[ --sql-prompt]'`
     (`--sql-prompt` for `/remote`).
   - Run the hub installer:
     `& ([scriptblock]::Create((New-Object Net.WebClient).DownloadString('<hub>/install.ps1'))) -Code '<code>' -Setup einvoice-enroll -Replace '*MM-EInvoice*'[ -Channel canary]`.
   - Remove `DL_AGENT_SETUP_ARGS` in a `finally`.
   - Public base URL as the existing bootstrap computes it (`publicBaseURL`).
   - Headers like the existing bootstrap (`text/plain`, `no-store`). Never validates the
     token against the DB (no oracle), same as today.
5. **UI** (`AgentTab.tsx`, npm, never run prettier on existing files): when the response has
   `dlCode`, show the two DL commands as the primary options, PowerShell 2.0 safe:
   `[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor 3072; iex ((New-Object Net.WebClient).DownloadString('<origin>/agent/bootstrap/<token>/dl/<code>[/remote][?channel=canary]'))`.
   Hints in Vietnamese: works on Windows 7 / Server 2008 R2 too; re-running keeps the site's
   E-Invoice credential (an existing `config.enc` is reused). Keep the legacy commands under a
   small "Cách cũ (agent Go)" disclosure. With `dlError`, show the legacy commands plus a muted
   note that the new installer is unavailable right now. Update the route test
   (`platform.tenants.$id.test.tsx`) for both shapes.
6. Docs: a runbook note (`docs/runbooks/`, the agent install one if it exists) and `CHANGELOG.md`
   if the repo has one. An ADR only if the repo's ADR practice calls for it for this size.

## Do not

- Change `internal/agentbuild`, the Go agent, `agentbin`, or anything MISA / DXQN related.
- Touch tenant data or run anything against production. No `fly` commands.
- Push, merge or deploy (a merge to master is the production deploy).

## Checks

Change-scoped while working (`go test ./internal/webapi/... ./internal/dlagent/...`, `go vet`,
web `npm run typecheck` + the touched test). Then **`make gate` once at the end** via
`~/.claude/scripts/heavy.sh make gate`; read its final `EXIT=` line. Commit in English,
Conventional Commits, with the trailer:

```
Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01DAn3zia3PHZ2jf2uZQSp3k
```

Report (≤ 10 lines) to `docs/plans/2026-11-19-einvoice-e2c-report.md` in the worktree: status,
SHA, the gate's EXIT line, gaps.

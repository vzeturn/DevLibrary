# E-Invoice on dl-agent: E2 (install and enroll) and E3 (canary)

RT-0164 task 557, step 1. Follows E1 (`einvoice` module, brief `2026-11-19-einvoice-module-brief.md`).

## Where things stand

- The E-Invoice console hands an admin a one-liner `iex (irm <platform>/agent/bootstrap/<token>[/remote])`
  (`MM-EINVOICE/internal/webapi/enroll.go`). That script removes old `*MM-EInvoice*` services,
  downloads the Go exe embedded in the platform, and runs `mmei-agent -enroll`. The agent then
  exchanges the token for a device-bound API key and seals `config.enc` (DPAPI LocalMachine).
- The E1 module reads that `config.enc` from the Go agent's directory. A site that already runs
  the Go agent therefore needs no new E-Invoice credential. It only needs dl-agent installed
  with `modules.einvoice.enabled`, and the Go service removed.

## Decisions (taken; reasons inline)

1. **The E-Invoice platform asks the DL hub for a dl-agent enroll code using a product key, not
   the admin token.**
   - New hub table `product_keys`: hashed key, product, created/revoked.
   - New route `POST /products/:product/enroll-codes` (Bearer product key; outside `/v1`, whose routes need a signed agent, and outside CF Access).
     It upserts tenant `<product>-<external id>` and sets `modules.<product>.enabled = true`
     in that tenant's config. It returns `{code, expires_at, install_command}`.
   - Admin routes create and revoke product keys.
   - Why: the platform must not hold the admin token plus the CF Access service token. A
     product key can only mint codes and enable its own module in its own tenant namespace.
2. **New-site enroll moves into the module as a CLI sub-command:**
   `dl-agent einvoice-enroll --token … --platform-url … [--sql-prompt]`.
   - Port `rust/agent/src/enroll.rs`: token exchange, SQL detection and prompt, `config.enc`
     seal. Go stays the reference.
   - `App` gets a hook for modules to add sub-commands, so this stays generic.
   - No default SQL password in source. The build secret works as in the Rust port, or the
     command prompts for it.
3. **`install.ps1` gains two generic switches:**
   - `-Replace '<service pattern>'`: stop and delete old services and processes before install.
     Their config is kept.
   - `-Setup '<sub-command>'`: run after enroll, before `install`. Its arguments come from
     `DL_AGENT_SETUP_ARGS`, and the env var is removed afterwards.
4. **The E-Invoice bootstrap script becomes a thin wrapper.** It fetches the DL code from the
   DL hub when the E-Invoice token is created in the console, never on GET, so there is no
   minting oracle. It then runs:

   ```
   install.ps1 -Code <dl code> -Replace '*MM-EInvoice*' -Setup einvoice-enroll
   ```

   - The `einvoice-enroll` step is skipped when `config.enc` already exists and is valid
     (migration of a live site), unless the console asked for re-enroll.
   - Switch: env `MMEI_AGENT_INSTALLER` (see 6).
   - Merging to MM-EINVOICE master deploys prod, so `make gate` runs first.
5. **No platform change for updates.** The Go agent updates itself in-process (ADR 0017 of
   MM-EINVOICE): once `-Replace` deletes its service nothing runs it again, and the `einvoice`
   module ignores update hints (dl-agent owns updates).
6. **The new installer is off by default** (`MMEI_AGENT_INSTALLER=dl` turns it on). It needs a
   dl-agent build with the `einvoice` module on the stable channel; until then the console keeps
   the legacy one-liner. The setup contract is
   `DL_AGENT_SETUP_ARGS='--token <t> --platform-url <u> [--sql-prompt]'` +
   `install.ps1 -Code <dl code> -Setup einvoice-enroll -Replace '*MM-EInvoice*'` (the release
   product stays `dl-agent`: one binary for every product).

## Slices

| Slice | Repo | What | Verify |
|---|---|---|---|
| E2a | DevLibrary hub | product keys + `/products/:p/enroll-codes` + tenant module config | vitest, tsc, deploy, curl on prod with a test key |
| E2b | DevLibrary agent | `App` sub-command hook (PR #19), `einvoice-enroll` port, `install.ps1` `-Replace`/`-Setup` (PR #18) | cargo test/clippy host + windows-gnu, pwsh parse |
| E2c | MM-EINVOICE | console mints DL code; bootstrap wrapper; installer switch (off by default) | `make gate`, merge = prod deploy, ops MCP check |
| E3 | both | demo tenant canary: Linux dl-agent with module + local SQL 2022 holding SMILE fixtures → E-Invoice demo tenant; gate; promote | hub gate pass, items visible on the platform for the demo tenant |

## Not verifiable on this machine

Real Windows (DPAPI seal, service, scheduled-task watchdog), Windows 7, and SQL 2005–2012.
Deliver says so.

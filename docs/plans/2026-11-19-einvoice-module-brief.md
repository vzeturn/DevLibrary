# Brief E1: `einvoice` module in dl-agent (RT-0164 task 557, step 1)

## Goal

E-Invoice (MM-EINVOICE, display name "Azolo E-Invoice") moves its on-prem agent onto the shared
`dl-agent`. This slice builds the **module only**, inside DevLibrary. It does not change
MM-EINVOICE, release anything, or deploy.

## Source to port

- `~/Projects/MM-EINVOICE/rust/agent/`: the Rust port of the Go agent (`cmd/agent`). It is about
  8.4k lines and uses DevLibrary crates only (pinned to an old rev). Read its README first.
  It covers collection (FO/POS procs, VAT apportion, TCVN3, void pairs), SQL proc install with
  the 2008 rewrite, the platform client (push, heartbeat, config, trn-types, write-back and ack,
  backoff), the outbox, the runner, and `config.enc` (Go format, DPAPI LocalMachine).
- **Missing from the Rust port and required:** the provider egress relay (ADR 0016,
  `~/Projects/MM-EINVOICE/docs/adr/0016-provider-egress-through-tenant-agent.md`). Go code:
  `internal/agentcollector/relay.go`, `internal/egressrelay/wire.go`, `CheckURL` in
  `internal/egressrelay`. Port it with exactly the behaviour in the ADR:
  - Long-poll `/relay/next`.
  - Answer on `/relay/{id}/result`.
  - The agent-side allowlist: https, port 443, no userinfo, host under `.vnpt-invoice.com.vn`.
  - Only the `Content-Type`, `SOAPAction` and `Accept` headers are forwarded. No redirects.
  - Timeouts default to 30 s, with 60 s max.
  - Backoff from 5 s to 60 s; wait 5 min when the hub has no relay route.
  - One call at a time.
  - Never log bodies: log only id, host, status and duration.
- The Go agent itself (`cmd/agent`, `internal/agentcollector`) is the reference whenever the
  Rust port and Go disagree. Go is what runs live.

## Target

- New crate `crates/dl-agent-einvoice`: a library exporting `einvoice_module() -> Factory`, or
  an equivalent. Add it to the workspace with the same lint and edition settings as the other
  crates. Depend on workspace crates by path (current main), not on the old git rev.
- Register the module in the `dl-agent` binary (`crates/dl-agent/src/main.rs`, `App::module`).
  This is one binary for every product. The module stays `Disabled` unless the remote config
  has `modules.einvoice.enabled = true`.
- Module config section `modules.einvoice`:
  - `enabled`.
  - `config_dir`: optional. The default is the Go agent's directory, the same as
    `default_config_dir()` in `rust/agent/src/main.rs`, so a site that already has `config.enc`
    switches over with no re-enroll.
  - Optional overrides of poll intervals, if the port has any.
  - The module's state (outbox, etc.) lives under `ctx.data_dir()/einvoice/`.
- Behaviour inside `Module::run`:
  - Load the sealed config. If it is missing, call `set_degraded("not enrolled: no config.enc in …")`
    and wait on `ctx.stop()`. This is not an error loop.
  - Otherwise run the collector loop and the relay worker on separate threads. Both stop
    promptly on `ctx.stop()`.
  - Call `set_running` once both are up.
  - Publish metrics: pending outbox, pushed total, last push time, relay calls and failures.
  - Report problems with `ctx.report` and stable codes: `einvoice_push_failed`,
    `einvoice_sql_failed`, `einvoice_relay_refused` and so on.
  - The heartbeat `agent_version` sent to the E-Invoice platform is `dl-agent-<version>`.
- No self-update code in the module, because dl-agent owns updates. Ignore any update hints
  from the platform.
- Keep the TCVN3 and money code byte-identical to Go for this slice. The golden tests decide.
  - `dl-vn` (C6) is a better TCVN3 decoder, but switching would change the pushed items.
  - Do not switch. Note in the report which golden cases would differ.
- No default SQL password in source. Enrolling a new site is slice E2 and is out of scope. It
  includes the zero-prompt bootstrap and `-sql-prompt`. Port only what reading an existing
  `config.enc` needs.
- Remote commands, nice to have if cheap:
  - `einvoice.status` returns the counters as JSON.
  - `einvoice.collect_now` triggers one cycle.

## Tests (mirror source structure, hand-written fakes, no mocking crates)

- Port the unit tests and the golden tests (`tests/golden.rs` and `tests/legacy_golden.rs`, plus
  their `testdata/`), so push items stay byte-identical to Go.
- Relay tests:
  - Allowlist accept and refuse: a LAN address, http, another port, userinfo and redirects are
    all refused.
  - Header filtering, the timeout cap, and the unsupported-hub wait.
  - A fake hub on a local socket. Use whatever local-server helper dl-agent tests already use.
- Module lifecycle: disabled; no `config.enc` gives degraded; stop is prompt.
- Live SQL tests stay `#[ignore]`/env-gated like the source.

## Checks before commit

- `~/.cargo/bin/cargo fmt --all`
- `~/.cargo/bin/cargo test -p dl-agent-einvoice -p dl-agent`
- `~/.cargo/bin/cargo clippy -p dl-agent-einvoice -p dl-agent --all-targets -- -D warnings`
- The same clippy run with `--target x86_64-pc-windows-gnu`.
- Every file stays at 800 lines or fewer, 200–400 typical. Split by responsibility.
- No swallowed errors.

## Rules

- Work only in the given worktree, on branch `feat/dl-agent-einvoice`. Commit with Conventional
  Commits, in English, with the trailer below. **Do not push, merge or deploy.** Do not
  touch MM-EINVOICE.
- Run heavy builds through `~/.claude/scripts/heavy.sh <cmd>`. Do not dump logs.
- Update `crates/dl-agent/README.md` (a short "Modules" note), `CHANGELOG.md` (Unreleased
  → Added), and the module's own README: scope, config, and differences from the Go agent.
- Report (at most 10 lines) to the report path: status, SHA, one test line, gaps.

Commit trailer:

```
Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01DAn3zia3PHZ2jf2uZQSp3k
```

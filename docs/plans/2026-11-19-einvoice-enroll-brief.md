# Brief E2b: `einvoice-enroll` setup sub-command (RT-0164 task 557)

Context: `docs/plans/2026-11-19-einvoice-e2-plan.md` decisions 2, 4 and 6. Hooks exist already:
`dl_agent::tool::Tool` + `App::tool` (PR #19, see `crates/dl-agent/src/tool.rs` and the README
"Building a product agent" note) and the `einvoice` module crate `crates/dl-agent-einvoice`
(PR #21). The executable is `crates/dl-agent-bin/src/main.rs`.

## Goal

`dl-agent einvoice-enroll --data <dir> --token <t> --platform-url <u> [--sql-prompt] …`
enrolls a site with the Azolo E-Invoice platform and seals `config.enc` exactly like
`mmei-agent -enroll`, so the `einvoice` module can run. `install.ps1 -Setup einvoice-enroll`
calls it after the dl-agent enrolment, with the arguments from env `DL_AGENT_SETUP_ARGS`.

## Port

Source: `~/Projects/azolo-einvoice/rust/agent/src/enroll.rs` (850 lines with tests) and the
`-enroll` flag handling in its `main.rs` (read only; do not touch MM-EINVOICE). Go
(`internal/agentcollector/enroll*.go`) is the reference where they disagree.

- Put it in `crates/dl-agent-einvoice/src/enroll/` (split by responsibility: prompt, ping /
  transport fallback, the enroll flow; each file ≤ 400 lines typical, never > 800).
- Export `einvoice_enroll_tool() -> dl_agent::tool::Tool` and register it in
  `dl-agent-bin` with `.tool(...)`.
- Flags (dl_cli grammar, long form): `--token` (required), `--platform-url` (required),
  `--sql-host`, `--sql-user`, `--sql-password`, `--sql-prompt`, `--collect-from YYYY-MM-DD`,
  `--config-dir` (default: the same directory the module reads by default, use the module's
  own default function, do not duplicate the path), `--force`.
- **Skip when already enrolled**: if `config.enc` in the config dir opens and parses (the
  module's loader), print one line ("da co cau hinh E-Invoice, giu nguyen") and exit 0 without
  spending the token, unless `--force`. A present but unreadable `config.enc` is an error
  (exit non-zero, say why), never silently overwritten.
- `install_service`: a no-op here (dl-agent installs its own service after setup).
- The exchange reports `agent_version` as `dl-agent-<dl_agent::agent_version()>`.
- **Default SQL password**: `option_env!("MMEI_BUILD_DEFAULT_SQL_PASSWORD")`, same as the
  Rust port; never a literal in source, tests or docs. Without it and without
  `--sql-password` / `MMEI_SQL_PASSWORD` / `--sql-prompt`, refuse before the exchange (port
  the existing test).
- Prompt texts stay Vietnamese without diacritics (legacy console code page). Password input
  with echo off on Windows (port `HideInput`; on other OS keep what the port does).
- Exit codes: 0 ok / skipped, non-zero with a one-line reason on stderr. No secret (token,
  password, API key) in any output or error text; add a test that the token and password do
  not appear in the output.

## Build script

`scripts/build-dl-agent.sh`: pass `-e MMEI_BUILD_DEFAULT_SQL_PASSWORD` (name only, so docker
takes the host value when set and nothing is echoed) to the cargo build container. Print a
warning (not the value) when it is unset: zero-prompt enroll then needs `--sql-password`.

## Tests (mirror source, hand-written fakes, no mocking crates)

Port the enroll tests (prompt flow with scripted input, refuse without password, ping
fallback, nothing persisted on error) plus: skip when `config.enc` is valid; error when it is
corrupt; `--force` re-enrolls; `DL_AGENT_SETUP_ARGS` words reach the tool (via
`dl_agent::tool::with_setup_args`); secrets absent from output. Live SQL tests stay
`#[ignore]` / env-gated.

## Checks before commit

`~/.cargo/bin/cargo fmt --all`; `cargo test -p dl-agent-einvoice -p dl-agent-bin`;
`cargo clippy -p dl-agent-einvoice -p dl-agent-bin --all-targets -- -D warnings`, and again
with `--target x86_64-pc-windows-gnu`; `cargo wasm` still builds. Heavy builds through
`~/.claude/scripts/heavy.sh <cmd>`. Update the crate README (enroll section), `CHANGELOG.md`
(Unreleased → Added).

## Rules

Work only in the worktree given, on branch `feat/einvoice-enroll`. Conventional Commits in
English, trailer below. Do not push, merge, release or deploy. Report ≤ 10 lines to the report
path: status, SHA, one test line, gaps.

```
Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01DAn3zia3PHZ2jf2uZQSp3k
```

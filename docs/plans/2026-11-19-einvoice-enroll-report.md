# Report: einvoice-enroll (RT-0164 task 557)

Status: DONE_WITH_CONCERNS. Branch feat/einvoice-enroll (not pushed).

- Port in `crates/dl-agent-einvoice/src/enroll/` (mod, ping, prompt, exchange, tool, tests); `einvoice_enroll_tool()` registered in `dl-agent-bin`. Deps added: dl-cli, dl-winos (hide_input).
- Skip when config.enc opens; corrupt = error naming file and `--force`; `--force` re-enrolls; install_service removed (no-op); default password via `option_env!`, never read or written.
- `scripts/build-dl-agent.sh`: `-e MMEI_BUILD_DEFAULT_SQL_PASSWORD` by name + warning when unset. README and CHANGELOG updated.
- Tests: `cargo test -p dl-agent-einvoice -p dl-agent-bin` all pass (85 lib tests incl. 11 enroll flow, 7 tool). Clippy -D warnings clean on host and x86_64-pc-windows-gnu; `cargo wasm` builds.
- Not run: real SQL / live platform, docker build-script run (only `bash -n`).

Concerns:
- `agent_version` is sent as the `X-Agent-Version` header, not in the JSON body: the Go platform decodes the exchange body with DisallowUnknownFields. The server does not read the header yet.
- `--config-dir` defaults to the module default; a custom `config_dir` from remote config must be passed explicitly.

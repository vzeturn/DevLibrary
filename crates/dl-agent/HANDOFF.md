# dl-agent handoff (RT-0164, 2026-11-18)

## Done
- `PROTOCOL.md` v1 and `INVENTORY.md` (9 agents, feature matrix, risks, migration order).
- Crate complete: identity/signing, hub client (clock skew, fallback, revoke), signed update with
  probation/rollback/blacklist, supervisor + `sys` module, reports spool, remote config with
  last-known-good, commands, CLI (`enroll`, `run`, `install`, `uninstall`, `status`) with
  Windows service and systemd install. `cargo test -p dl-agent`, clippy `-D warnings`, fmt, `cargo wasm` green.
- Hub `services/agent-hub` (branch `feat/agent-hub`): review items settled — per-product release line
  (`product` in heartbeat), `disabled` modules allowed by the gate (PROTOCOL wording), first release
  passes without force when canary is clean, revoke via `401 revoked` (agent wipes identity),
  Cloudflare Access for `/admin` documented in its README.

## Remaining
1. Owner: hub domain + production deploy (D1, R2, Access, secrets) — see `services/agent-hub/README.md`.
2. Release signing key (offline Ed25519) and `DL_AGENT_RELEASE_KEYS` build setting.
3. Module migrations per `INVENTORY.md` §5 — tasks of RT-0164 (task 557), each on a demo tenant first.

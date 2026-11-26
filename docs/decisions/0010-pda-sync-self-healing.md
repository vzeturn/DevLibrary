# 0010. pda-sync heals itself: any private LAN printer, grants as the SMILE login, clean store text

- Status: accepted
- Date: 2026-11-26
- Scope: `crates/dl-agent-pda-sync` (`printrelay`, `commands/grants*`, `util/store_text`,
  `commands/channel`); RT-1309

## Decision

1. **Print relay targets.** An empty `modules.pda-sync.PrintRelay.PrinterSubnets` allows every
   private LAN range the agent accepts (`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`,
   `fc00::/7`), including networks routed beyond the server's own adapters. A listed subnet
   still narrows the targets, and must itself be private. The port allow-list (default
   `9100`) stays, and public, loopback and link-local addresses are always refused, so the
   relay is never an open TCP proxy: the cloud can only make the agent open a raw printer port
   on the store's private LAN. A target that is allowed but does not answer is a normal
   retryable connect error naming `host:port` and the OS error, never a refusal (owner
   decision 2026-11-26: "allow all, do not refuse; only a failed connection is an error").
2. **Grants self-repair identities.** When `pda_sync` lacks its order grants, the service runs
   the same idempotent bootstrap as `pda-enroll`, as the identities `pda-enroll` tries, in
   its order: the service's Windows identity, then the SMILE default SQL login. The next
   identity is tried only after an access refusal (`SQL_NO_ACCESS`, `SQL_LOGIN_FAILED`,
   `SQL_DATABASE_NOT_FOUND`). Hard rule: the bootstrap only creates and grants objects for
   `pda_sync`; it never alters the SMILE login or its password. Site events name the
   identity and its outcome code, never a password. `SQL_GRANTS_MISSING` is sent only when
   every identity failed (once per episode); the service tries again every 15 minutes while
   the grants stay missing, and at once when an order command was refused by SQL. A refused
   command only probes: it never waits for or runs a repair, so its answer beats the lease.
3. **Store text at the read boundary.** Every text value read from SMILE for a command result
   or a capture row goes through one helper (`util::store_text::clean`): NUL characters
   are removed and trailing U+0020 padding is trimmed. PostgreSQL `jsonb`/`text` refuse
   NUL (22P05), which made the cloud drop an `order.item_post` result and let the command
   end `lease_expired` although the agent had applied it. Writes back to SMILE are unchanged.
4. **Unreadable commands.** A command frame the agent cannot read but whose `command_id` is
   readable is answered `rejected` (`invalid_payload: ...`) instead of being skipped.

## Consequences

- A store whose printers sit on another routed private subnet prints without any hub
  config; operators narrow it with `PrinterSubnets` when they want to.
- A store where the service account is not a SQL administrator heals its grants through the
  SMILE login baked into the release build (or `DL_AGENT_PDA_SQL_DEFAULT_PASSWORD`); a
  build without it records that identity as skipped and still reports
  `SQL_GRANTS_MISSING` with the `dl-agent pda-grants` instruction.
- The bootstrap's login step (`010_login.sql`) resets the `pda_sync` password to the
  stored secret under either identity, as the Windows repair did before.
- Capture rows that already reached the cloud are not resent for this change
  (`TEXT_REVISION` unchanged): a row with a NUL never ingested, and padded codes were
  already trimmed (ADR 0123 of the C# agent).

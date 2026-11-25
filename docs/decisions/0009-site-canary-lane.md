# 0009. Site canary lane: watched first stop on stable, pulled back on bad signs

- Status: accepted
- Date: 2026-11-25
- Scope: `services/agent-hub` (stable offer, admin API, migration 0011)

## Decision

An admin lists site canaries (`POST /admin/site-canaries {tenant_id, site_id?}`; site `*` is
the whole tenant). For every non-major stable promotion made after the list was created:

1. Only the listed sites are offered the release (by the manifest's `rollout_percent`).
2. The hub judges those sites on every stable heartbeat (`domain/watch.ts`), for
   `SITE_CANARY_HOURS` (default 4) after promotion:
   - **bad at once**: a site reported a rollback of the version, a site on the version is silent
     for more than `SITE_CANARY_SILENT_SECONDS` (default 900), its latest self-test on the
     version has failed checks, a module is not running, or an error/critical fingerprint
     appears that the previous version never had.
   - **good**: the watch time passed with at least one site on the version and no fault, or no
     site canary agent is enrolled for the product (an empty lane never strands a release).
3. The verdict is stored once per promotion (`release_watch`). `good` hands the release to the
   normal ramp (ADR 0001). `bad` pauses the release (`release_pauses`, reason kept), so the
   manifest endpoint serves the previous stable again and nobody else is offered the bad one.

## Consequences

- The pull-back is a pause: sites already on the bad version go back only through the agent's
  own probation rollback, or an older signed manifest with `allow_downgrade`. The hub cannot
  sign a downgrade.
- Resuming a pulled-back release clears its verdict, so the lane judges it again.
- While the watch waits (no listed site has the version yet), other sites hold. Remove the list
  entry or pause/resume the release to unblock.
- Promotions made before the list existed are never judged.

# 0004. Customers on stable, canary only for our machines and consented pilots

- Status: accepted
- Date: 2026-11-20
- Scope: `services/agent-hub` (channel resolution, admin agent list, release gate), operator procedure

The brief for RT-0375 asked for the number 0002; that number and 0003 were already taken, so
this is the next free one.

## Context

The owner's rule (RT-0375): a real customer machine must not receive untested builds. Until
now the channel of an agent followed its tenant's `demo` flag (`resolveChannel`, `src/domain/channel.ts`):
`demo=1` means canary, otherwise stable, unless the agent carries a `channel_override`.
Muine Bay Resort (tenant `arrival-muine-bay-resort`, agent on SERVER_SMILE_MN, running 1.3.21)
was on canary only because its tenant had `demo=1`. Stable was 1.3.15, older than the
machine, so simply switching the tenant to stable would have offered a downgrade.

Also, after a test round the revoked test agents stayed in `GET /admin/agents`, so the list
did not show only live machines.

Gate results run on 2026-11-20 for dl-agent 1.3.16 to 1.3.21 (all patch bumps against stable
1.3.15, `min_agents` 1, soak 2 h):

| Version | Result | Failing checks |
|---|---|---|
| 1.3.16 | pass | none; promoted to stable (artifact hashes matched canary) |
| 1.3.17 | fail | min_agents 0/1, soak 0/1, new error fingerprint f31092d82b794d26; rolled back on dl-bench |
| 1.3.18 | fail | no_new_errors (10 new fingerprints vs 1.3.15) |
| 1.3.19 | fail | soak 0/1 agents soaked 2 h (superseded before the soak finished) |
| 1.3.20 | fail | no_new_errors (3 new fingerprints) |
| 1.3.21 | fail | soak 0/1 (published about 1 h 46 m before the check), modules_running (dl-bench pms-device degraded), no_new_errors (5 new fingerprints, all from Muine Bay: SQL Server error 18461 during a server maintenance window, then `sql_recovered`; `arrival_pms_failed` "connection was lost"; pda-sync `SYNC_BATCH_QUARANTINED` FK 23503 rejected by the PDAOrder cloud) |

## Decision

- Real customer tenants are created with `demo=false`, so their agents follow stable.
- Canary is for our own machines (dl-bench, demo tenants) and for pilot machines whose
  project owner agreed. A pilot machine is pinned with `PUT /admin/agents/:id/channel
  {"channel":"canary"}` and the reason is recorded (ticket or audit note).
- A customer machine already ahead of stable is pinned to canary and never downgraded.
  When a release at or above its version reaches stable, the operator removes the pin with
  `PUT /admin/agents/:id/channel {"channel":"stable"}`. Muine Bay is in this state: tenant
  `demo=false`, agent pinned to canary (running 1.3.21, stable 1.3.16).
- Offline test and demo agents are revoked after a test round. `GET /admin/agents` and the
  status page list only live machines; `include_revoked=1` shows the rest, and
  `GET /admin/agents/:id` still finds a revoked agent.
- 1.3.10 stays paused on purpose. The recorded reason is "fault build (rollback smoke B6) -
  never offer": it is the deliberately faulty build of the rollback smoke test, and resuming
  it would offer a broken build.

## Consequences

- A customer sees a release only after it has passed the gate and been promoted.
- Pins are manual state: someone must remove them once stable catches up, or the machine
  stays on the fast lane.
- Revoked agents disappear from the default list; tools that audit revoked machines must ask
  for them.

## Alternatives considered

- Keep `demo=1` for pilots: ties a customer machine to the tenant flag and to every demo
  tenant default; rejected, the per-agent override says why a machine is on canary.
- Allow the downgrade to stable: rejected, a working machine must not be moved to an older build.
- Delete revoked agents: rejected, the audit trail and `GET /admin/agents/:id` need the rows.

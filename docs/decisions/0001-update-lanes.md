# 0001. Update lanes: canary fast lane, stable lanes by semver bump

- Status: accepted
- Date: 2026-11-19
- Scope: `crates/dl-agent` (update loop), `services/agent-hub` (heartbeat, release gate)

## Context

The owner's rule (2026-11-19): while we are testing, the agent must update very fast so
faults are fixed quickly and thoroughly; once a stable release exists, apply rules by major /
minor / patch.

Before this decision both channels moved at the same pace. A canary agent beat every 60 s,
checked for an update at most every 5 minutes, waited for the site's update window, honoured
`auto: false`, and stayed on probation for 10 minutes. A fix published for a test PC could
take most of an hour to reach it and be judged. On stable every promotion went to every agent
(within the manifest's `rollout_percent`) after the same 2 h soak, whether it was a one-line
fix or a new major version.

## Decision

**Canary is the fast lane** (PROTOCOL.md §5.1, §6.4). Canary agents are test machines
(demo tenants, agents marked canary).

- The hub answers a canary heartbeat with `min(agent interval, CANARY_HEARTBEAT_SECONDS)`
  (default 15 s), and 5 s while the agent reports an update on probation.
- On canary the agent checks for an update on every heartbeat with `update_check`, ignores
  the site window and `auto: false`, and keeps probation to 120 s.
- The boot count, the failure cap and the rollback are unchanged: a fast lane must still
  stop a bad build.

So publish → install takes one heartbeat (≤ 15 s) plus the download.

**Stable rolls out by semver bump** (PROTOCOL.md §5.2). On promotion the hub compares the
version with the current stable and stores the bump on the promotion (migration 0005).

| Bump | Soak | Min canary agents | Offered to stable agents |
|---|---|---|---|
| patch (and `initial`, the first stable) | 2 h | `GATE_MIN_AGENTS` | all at once; the window holds it unless urgent |
| minor | 24 h | `GATE_MIN_AGENTS` | 10 % → 50 % → 100 % at 0 / 24 / 48 h after promotion |
| major | 72 h | max(`GATE_MIN_AGENTS`, 2) | only opted-in tenants or sites, or `update_now` |

- The hub applies the ramp through `update_check`. It uses the same bucket as the agent
  (`sha256(agent_id|product|version)[0..2] % 100`), so the cohort only grows. The signed
  `rollout_percent` stays the ceiling, and the agent applies it again itself.
- A major is opted in per tenant (every site) or per site with
  `POST /admin/releases/:product/:version/allow`. `urgent` and `required` do not bypass this,
  but `update_now` does, since it fetches the stable manifest directly.
- The hotfix lane stays: an `urgent` patch skips the window, and `force` promotes past a
  failing gate.

## Consequences

- Agents from before the change (1.3.x) keep working. They see the faster canary interval,
  flooring 5 s at 10 s, but still space their checks 5 minutes apart and keep a 10-minute
  probation. The heartbeat answer keeps its shape.
- Promotions recorded before migration 0005 have no bump and behave as a patch, as before.
  The old `GATE_SOAK_HOURS` remains the fallback for the patch soak.
- The manifest endpoint serves only the latest stable release. While a major, or a minor
  still ramping, is the latest stable, a stable agent that is not offered it gets no update
  at all, not even a later patch of the previous line. A patch for the old line needs a
  release line of its own, which is not built yet.
- A canary agent whose failure cap defers a version still fetches the manifest on every
  heartbeat (every 15 s) until the deferral ends. The download itself is not repeated.
- Canary load on the hub rises (4× the heartbeats of a stable agent), but canary agents are
  a handful of test PCs.

## Alternatives considered

- **Separate soak per bump only, no ramp.** This was rejected because a minor reaching every
  site at once is exactly what the owner's rule asks to avoid.
- **Ramp on the agent (rewriting `rollout_percent` over time).** This was rejected because
  the manifest is signed offline, so changing the percentage would need a new signature at
  every step. The hub's `update_check` can widen the cohort without one.
- **Allow-list majors by agent id.** This was rejected because majors go out with the
  customer, site by site, so a tenant or site is the unit an admin decides on.

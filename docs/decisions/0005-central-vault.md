# 0005. One OpenBao vault on Fly.io for the keys of Hub AI and Cloud AI

- Status: accepted; decision 4 and the `hub-ai` part of decision 5 superseded by
  [ADR 0006](0006-vault-owner-held-seal.md)
- Date: 2026-11-21
- Scope: `services/vault` (new Fly app `dl-vault`), the Request Tracking hub (Hub AI) and cloud
  sessions (Cloud AI) as clients
- Request: RT-0584 (the owner left the choices to best practice)

## Context

The keys and credentials the two AIs use live in several places: the hub's cloud kit
(`cloud_secrets`, one bundle per project, RT-0514), files under `~/.config/*` on the iMac,
Fly and Worker secrets per app, and the `SECRETS_BUNDLE_TGZ_B64` tarball. There is no single
place to see what exists, who may read it, or when it was read, and no backup of the kit
itself outside the hub's database.

The request asks for one vault on Fly.io, HTTPS only, a separate least-privilege identity for
each AI, an audit log and backups, and for the secrets to move into it.

## Decision

1. **Software: OpenBao** (`openbao/openbao`, the Linux Foundation fork of Vault, MPL-2.0).
   Vault's own licence is BSL. OpenBao keeps the Vault API, so `vault`-compatible clients and
   the `bao` CLI both work.
2. **One Fly machine in `sin`** (shared-cpu-1x, 512 MB) with a 1 GB volume. Storage is the
   integrated Raft backend on the volume (single node). `auto_stop_machines = "off"`. The
   expected cost is about 3 to 6 USD a month, including R2 for backups.
3. **HTTPS only.** The public service has a single port, 443, with TLS ending at the Fly
   edge (certificate for `dl-vault.fly.dev`). There is no port 80 handler. Inside the
   machine the listener is plain HTTP on 8200. Inside the org's private network (6PN) the
   listener can be reached without TLS; no other app in the org needs it, and every request
   still needs a token.
4. **Seal: Shamir, 3 shares, threshold 2.** Share 1 is held by the owner offline. Share 2
   is held in the hub's kit (cloud project kit `dev-library`, name `DL_VAULT_UNSEAL_2`).
   Share 3 is on the iMac (`~/.config/dl-vault/unseal-3`). Two shares unseal the vault, so
   the hub and the iMac together can bring it back after a restart without the owner, and
   no single place holds enough to unseal. A restarted machine stays sealed until then;
   clients fall back to the kit during the move (S3), so a sealed vault is an outage of the
   new path only. The root token is revoked at the end of bootstrap; a new one needs two
   shares (`bao operator generate-root`).
5. **Identities: AppRole, one role per AI, tokens of 1 h (renewable up to 4 h).**
   - `hub-ai`: reads and writes `kv/data/hub/*`, `kv/data/projects/*`, `kv/data/shared/*`.
     The hub remains the component that hands a cloud session the keys of one project
     (RT-0514), now reading them from the vault.
   - `cloud-ai`: read-only on `kv/data/cloud/*` (tools every cloud session needs) and
     `kv/data/shared/*`. It cannot list or read project kits; those still come only through
     the hub's pass.
   - `backup`: one periodic token (24 h, renewed by the backup loop) that can only take a
     Raft snapshot.
   Secret IDs expire after 90 days and are rotated by the operator.
6. **Audit:** two audit devices, a file on the volume (`/openbao/data/audit/audit.log`) and
   stdout (Fly logs). Values are HMAC'd by OpenBao, never logged in clear. If no audit
   device can write, OpenBao refuses the request, so access is never silently unlogged.
7. **Backup:** every 6 h the machine takes a Raft snapshot and uploads it to R2
   (`dl-vault-backups/dl-vault/<UTC timestamp>.snap`, Cloudflare API with an R2-only token).
   A snapshot stays encrypted by the vault's barrier key: restoring it needs two unseal
   shares. A restore drill into a throwaway machine is part of S2 and is logged in the README.
8. **Rollout in three slices:** S1 is the config and scripts in this repo. S2 creates and
   deploys the app, bootstraps it, enables audit, takes and restores a backup, and issues
   the AppRole credentials. S3 moves the secrets group by group. Hub AI and Cloud AI then
   read from the vault, and the kit copies are removed only after the AIs read from the
   vault successfully. S3 changes the Request Tracking hub, so it has its own request there.

## Consequences

- One more always-on machine to watch: a sealed or unreachable vault blocks only clients
  that have already moved, and the kit stays the fallback until S3 completes.
- The AppRole secret IDs and the backup token are themselves secrets. They live as Fly or
  hub secrets of their client and nowhere else.
- Anyone with two shares can read everything. Share holders are listed above, and moving a
  share is a new ADR.

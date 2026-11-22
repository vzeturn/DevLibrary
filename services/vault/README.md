# dl-vault

The central secret store of Hub AI and Cloud AI: OpenBao on one Fly.io machine, Raft storage
on a volume, HTTPS only at the Fly edge, AppRole identities, audit to file and stdout,
snapshots to R2 every 6 h. The decision and the reasons: [ADR 0005](../../docs/decisions/0005-central-vault.md);
who holds the unseal shares and what each AI may do: [ADR 0006](../../docs/decisions/0006-vault-owner-held-seal.md).

| File | Purpose |
|---|---|
| `fly.toml` | app `dl-vault` (`sin`), volume `vault_data` at `/openbao/data`, port 443 only, health check that tolerates a sealed vault |
| `Dockerfile` | `openbao/openbao:2.2.0` plus `curl`/`jq`, the config and the scripts |
| `config.hcl` | Raft at `/openbao/data/raft`, listener `:8200` (TLS at the edge), `api_addr` `https://dl-vault.fly.dev` |
| `policies/*.hcl` | `hub-ai` (read `hub/`, `projects/`, `shared/`), `cloud-ai` (read `cloud/`, `shared/`), `importer` (create/update only, no read), `backup` (snapshot only), `hub-admin` (root-equivalent for the RT hub, accident denies: seal/rekey/generate-root/raw/audit/unmount kv/approle, its own grant; may rotate only its own secret ID) |
| `scripts/entrypoint.sh` | starts the server, and the backup loop when `BAO_BACKUP_TOKEN` is set |
| `scripts/backup.sh` | `once` / `loop`: Raft snapshot, then upload to R2 `dl-vault-backups/dl-vault/<UTC>.snap` |
| `scripts/bootstrap.sh` | one-time, two steps: `init` (3 shares, threshold 2, encrypted to the owner's PGP key) and, after the owner's unseal, `configure` (audit, kv-v2 at `kv/`, policies, AppRoles, backup token, revoke root); safe to re-run after a partial failure |
| `scripts/unseal.sh` | the owner's step on the owner's machine: decrypt one share with gpg and send it to `sys/unseal` via stdin |

## Setting it up (S2)

```bash
fly apps create dl-vault -o mm-151
fly volumes create vault_data -a dl-vault -r sin -s 1 --yes
cd services/vault && fly deploy -a dl-vault --remote-only --depot=false
export BAO_ADDR=https://dl-vault.fly.dev                       # bao CLI + jq on the operator machine
# The owner's public key, three times (base64 of `gpg --export <id>`); shares come back encrypted to it.
OWNER_PGP_KEYS=owner.b64,owner.b64,owner.b64 scripts/bootstrap.sh init
#   -> hand ~/.config/dl-vault/owner-shares.json to the owner (it holds only encrypted shares)
#   -> the owner, on their own machine: scripts/unseal.sh owner-shares.json 0; ... 1
scripts/bootstrap.sh configure                                 # revokes the root token at the end
# backup loop: an R2-only Cloudflare token, the account id, backup_token from state.json
fly secrets set -a dl-vault --stage CLOUDFLARE_API_TOKEN=... CF_ACCOUNT_ID=... BAO_BACKUP_TOKEN=...
fly deploy -a dl-vault --remote-only --depot=false             # reseals: the owner unseals again
fly ssh console -a dl-vault -C '/usr/local/bin/backup.sh once'  # or wait for the loop; fly logs shows backup.uploaded
```

Bucket `dl-vault-backups` gets an R2 lifecycle rule that deletes objects after 30 days (about
120 snapshots). The backup loop runs in the background of the machine's entrypoint. If it
dies, `fly logs` stops showing `backup.uploaded`; a machine restart starts it again. Hub
monitoring of the newest snapshot's age is part of S3.

No plain unseal share ever exists outside the owner's machine. The owner keeps the PGP
private key and `owner-shares.json` offline, in two separate places; losing both loses the
vault. If `configure` stops part way, the root token is still in `state.json`: fix the cause
and run it again (it skips what is done). Afterwards `state.json` holds only the client
credentials: `hub_ai` and `cloud_ai` go to their clients as secrets, `importer` is for the
first approved batch, `backup_token` is the Fly secret above. Delete the file once every
value has been placed.

## Operating it

- **Sealed after a restart or deploy**: the owner runs `scripts/unseal.sh owner-shares.json
  <i>` with two different indexes (`BAO_ADDR=https://dl-vault.fly.dev`; needs gpg, jq, curl).
  `GET /v1/sys/health` answers `sealed: true` until then. `bao operator unseal` is not used:
  it reads a key only from a terminal or argv.
- **Login**: `bao write auth/approle/login role_id=... secret_id=-` (secret ID on stdin)
  returns a 1 h token (renewable to 4 h). Secret IDs expire after 90 days. To rotate one:
  `bao write -f auth/approle/role/<role>/secret-id`, then destroy the old accessor.
- **Admin session** (the owner): `bao operator generate-root -init -pgp-key=owner.b64`, then
  `generate-root` with two decrypted shares, decrypt the token, do the work, `bao token
  revoke -self`. There is no standing admin account and no web UI.
- **Moving secrets in**: only items on a list the owner approved. An admin session issues an
  importer secret ID (`bao write -f auth/approle/role/importer/secret-id`, single use, 24 h);
  the importer token can create and update but not read, so a wrong write is fixed by an
  admin from the kv-v2 version history.
- **Audit**: `fly logs -a dl-vault` (stdout device) and `/openbao/data/audit/audit.log` on
  the volume. Request and response values are HMAC'd.
- **Restore**: on a throwaway machine with an empty volume, init and unseal it with
  throwaway keys, log in with its root token and run `bao operator raft snapshot restore
  -force <file>`. The node then carries the original data and keyring, so the owner unseals
  it with two of the original shares.

## Execution log

| Date | What | Result |
|---|---|---|
| 2026-11-21 | S1: config, policies, scripts (RT-0584) | in the repo, not deployed yet; the image and scripts are exercised for the first time in S2 |
| 2026-11-21 | S1b: owner-held encrypted shares, read-only `hub-ai`, `importer`, two-step bootstrap (ADR 0006) | local run against OpenBao 2.2.0 (Raft, 127.0.0.1): init with a test PGP key, `unseal.sh` x2, `configure`, re-run refused; policy checks (hub-ai read yes/write no, importer write yes/read no/second login no, cloud-ai no `projects/`, backup token snapshot yes/read no); no plain value in audit or server log; snapshot restored into a second node and unsealed with two original shares, value read back |
| 2026-11-22 | S2 from the iMac (RT-0584): volume, deploy, `init` (shares to owner key 27B0…4D1A), unseal, owner-approved admin import, `configure`, R2 backup | live at https://dl-vault.fly.dev; all three shares unseal; root token revoked; hub-ai/cloud-ai log in and get 403 on `kv/admin/*`; every iMac key file (41 entries, 208 files) under `kv/admin/imac/{home,repo}/…` plus `kv/admin/dl-vault/{state,r2-backup-token}`, written by root before `configure` and by a generate-root session after; bucket `dl-vault-backups` on the R2 account (30-day lifecycle), R2-only token `dl-vault-backup-r2`, first `backup.uploaded`; restore drill on a throwaway local node: snapshot from R2 restored, unsealed with original shares 0+2, hub-ai login works, admin still 403 |
| 2026-11-22 | S3a from a cloud session (RT-0584): first importer batch | importer AppRole login once (policies `importer`), three writes HTTP 200 version 1: `kv/projects/dev-library/kit` (25 kit keys; the secrets bundle, which carries the release seed, and the Vault's own AppRole credentials left out: they stay under `kv/admin/` only), `kv/shared/owner-pgp-public` (owner's public key, fingerprint, format), `kv/cloud/github` (`GH_TOKEN`); importer read 403, `revoke-self` 204, second login with the same secret ID 400; cloud-ai reads `cloud/github` and `shared/owner-pgp-public` (values match the kit by SHA-256, never printed) and gets 403 on `projects/dev-library/kit` and `admin/`; audit lines for each path in `fly logs -a dl-vault`. Hub side (hub-ai credentials in the RT Worker, every project's kit, `cloud_secrets` reading from the Vault, D1 copies removed) is RT-0653 |
| 2026-11-22 | `hub-admin` AppRole for the RT hub (owner: the hub mints kit keys and grants itself; no iMac in the loop) | applied live by a generate-root session from the iMac; role-id + secret-id (365 d, hub rotates it) in the `request-tracking` kit as `VAULT_HUB_ADMIN_*`; login checked: create/read/delete a test policy + test AppRole yes, `sys/seal`, `sys/audit`, own role config 403; root token revoked |

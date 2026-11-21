# dl-vault

The central secret store of Hub AI and Cloud AI: OpenBao on one Fly.io machine, Raft storage
on a volume, HTTPS only at the Fly edge, AppRole identities, audit to file and stdout,
snapshots to R2 every 6 h. The decision and the reasons: [ADR 0005](../../docs/decisions/0005-central-vault.md).

| File | Purpose |
|---|---|
| `fly.toml` | app `dl-vault` (`sin`), volume `vault_data` at `/openbao/data`, port 443 only, health check that tolerates a sealed vault |
| `Dockerfile` | `openbao/openbao:2.2.0` plus `curl`/`jq`, the config and the scripts |
| `config.hcl` | Raft at `/openbao/data/raft`, listener `:8200` (TLS at the edge), `api_addr` `https://dl-vault.fly.dev` |
| `policies/*.hcl` | `hub-ai` (rw `hub/`, `projects/`, `shared/`), `cloud-ai` (read `cloud/`, `shared/`), `backup` (snapshot only) |
| `scripts/entrypoint.sh` | starts the server, and the backup loop when `BAO_BACKUP_TOKEN` is set |
| `scripts/backup.sh` | `once` / `loop`: Raft snapshot, then upload to R2 `dl-vault-backups/dl-vault/<UTC>.snap` |
| `scripts/bootstrap.sh` | one-time: init (3 shares, threshold 2), unseal, audit, kv-v2 at `kv/`, policies, AppRoles, backup token, revoke root; secrets only go to a 0600 file |

## Setting it up (S2)

```bash
fly apps create dl-vault -o mm-151
fly volumes create vault_data -a dl-vault -r sin -s 1 --yes
fly deploy -a dl-vault -c services/vault/fly.toml --remote-only --depot=false   # from services/vault
BAO_ADDR=https://dl-vault.fly.dev services/vault/scripts/bootstrap.sh            # bao CLI + jq
# backup loop: an R2-only Cloudflare token, the account id, the backup token from bootstrap.json
fly secrets set -a dl-vault --stage CLOUDFLARE_API_TOKEN=... CF_ACCOUNT_ID=... BAO_BACKUP_TOKEN=...
fly deploy -a dl-vault -c services/vault/fly.toml --remote-only --depot=false
# every deploy or restart reseals the vault: unseal with two shares (stdin, not argv)
jq -r '.unseal_keys_b64[0]' ~/.config/dl-vault/bootstrap.json | bao operator unseal -
jq -r '.unseal_keys_b64[1]' ~/.config/dl-vault/bootstrap.json | bao operator unseal -
fly ssh console -a dl-vault -C '/usr/local/bin/backup.sh once'   # inside the machine (its secrets); or wait for the loop, then fly logs shows backup.uploaded
```

Bucket `dl-vault-backups` gets an R2 lifecycle rule that deletes objects after 30 days (about
120 snapshots). The backup loop runs in the background of the machine's entrypoint. If it
dies, `fly logs` stops showing `backup.uploaded`; a machine restart starts it again. Hub
monitoring of the newest snapshot's age is part of S3.

If `bootstrap.sh` stops after `operator init`, the file still holds the shares and the root
token. Finish the remaining steps by hand with that token, then `bao token revoke -self` and
remove `root_token` from the file.

Then, from `bootstrap.json`: share 1 goes to the owner (offline), share 2 to the hub kit as
`DL_VAULT_UNSEAL_2`, share 3 to the iMac at `~/.config/dl-vault/unseal-3`. The `hub_ai` and
`cloud_ai` role/secret IDs go to their clients as secrets. Delete the file once every value
has been placed.

## Operating it

- **Sealed after a restart or deploy**: `bao operator unseal -` twice, piping in two
  different shares (`BAO_ADDR=https://dl-vault.fly.dev`). `GET /v1/sys/health` answers `sealed: true` until then.
- **Login**: `bao write auth/approle/login role_id=... secret_id=...` returns a 1 h token
  (renewable to 4 h). Secret IDs expire after 90 days. To rotate one:
  `bao write -f auth/approle/role/<role>/secret-id`, then destroy the old accessor.
- **Audit**: `fly logs -a dl-vault` (stdout device) and `/openbao/data/audit/audit.log` on
  the volume. Request and response values are HMAC'd.
- **Restore**: on a throwaway machine with an empty volume, init and unseal it with
  throwaway keys, log in with its root token and run `bao operator raft snapshot restore
  -force <file>`. The node then carries the original data and keyring, so unseal it with two
  shares of the original vault.

## Execution log

| Date | What | Result |
|---|---|---|
| 2026-11-21 | S1: config, policies, scripts (RT-0584) | in the repo, not deployed yet; the image and scripts are exercised for the first time in S2 |

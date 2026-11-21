# 0006. dl-vault: the owner holds every unseal share, the AIs only read

- Status: accepted
- Date: 2026-11-21
- Supersedes: ADR 0005 decision 4 (seal shares) and the `hub-ai` part of decision 5
- Scope: `services/vault`
- Request: RT-0584 (the owner's compliance conditions on approval, comment 1486)

## Context

The owner approved RT-0584 with conditions (ISO 27001, ISO 42001, OWASP ASVS L2, ND 356
Article 9). Out of scope: giving Hub AI or Cloud AI read access to the whole vault or a root
token; letting an AI hold an unseal share, the root key or a release signing key; storing
root or unseal material in a repository, a Fly environment or a log; an admin interface on
the public Internet without MFA. Required: one least-privilege identity per AI with
short-lived renewable tokens, the root token revoked after setup, unseal shares held offline
by the owner, human admin access with more than one factor, and secrets moved in only from
a list the owner approved item by item, without their values reaching a log.

ADR 0005 put share 2 in the hub's kit and share 3 on the iMac, where an AI session can read
them, and gave `hub-ai` write access to every path it reads. Neither meets the conditions.
Nothing had been deployed when this was found.

## Decision

1. **Shares are encrypted to the owner's PGP key at init.** `bootstrap.sh init` runs
   `bao operator init -key-shares=3 -key-threshold=2 -pgp-keys=<owner keys>`. OpenBao returns
   each share already encrypted, so the plain shares never exist on the machine that runs the
   bootstrap. The owner keeps the private key (with its passphrase) and the three encrypted
   shares offline. No share goes to a kit, a Fly secret, the iMac or the repository.
2. **Unsealing is the owner's step.** After a deploy or a restart the vault stays sealed
   until the owner runs `scripts/unseal.sh` twice on their own machine (it decrypts one
   share with `gpg` and sends it as the body of `PUT /v1/sys/unseal` through curl's stdin;
   `bao operator unseal` reads a key only from a terminal or argv). Clients keep the kit as a
   fallback until RT-0584 S3 ends, so a sealed vault is an outage of the new path only.
3. **The root token lives only for the setup.** `init` stores it in a 0600 file on the
   bootstrap machine while the vault is sealed (it can do nothing then). `bootstrap.sh
   configure` runs after the owner's unseal, sets everything up and revokes it.
4. **Human admin access is `bao operator generate-root` with two owner shares.** That needs
   the encrypted shares and the PGP private key with its passphrase (possession plus
   knowledge), produces a root token for one session, and the admin revokes it at the end.
   There is no standing admin account and the web UI stays off (`ui = false`).
5. **`hub-ai` is read-only.** It reads `kv/data/hub/*`, `kv/data/projects/*` and
   `kv/data/shared/*` and lists their metadata, because the hub hands one project's keys to a
   cloud session (RT-0514). It cannot write, delete, read `kv/data/cloud/*`, or touch `sys/`
   or `auth/`. `cloud-ai` stays as in ADR 0005 (read `cloud/` and `shared/`).
6. **Writes go through `importer`, one batch at a time.** The AppRole `importer` may only
   create and update `kv/data/{hub,projects,shared,cloud}/*`: no read, no list, no delete.
   Its secret IDs are single use and live 24 h, its token 1 h, not renewable. `configure`
   issues the first one for the first approved batch; each later batch gets a new one from an
   admin session (decision 4). kv-v2 keeps old versions, so a wrong write can be rolled back
   by an admin. The importer writes only the items on the list the owner approved, reads the
   values from their current place and never prints them.

## Consequences

- Every deploy or host restart needs the owner to unseal. Deploys of `dl-vault` are rare
  (image bumps), and the vault is not on any critical path until S3 moves a client.
- Losing the PGP private key, or two of the three encrypted shares, loses the vault. The
  backups (ADR 0005 decision 7) do not help then, since a snapshot needs the same shares.
  The owner keeps the key and the shares in two separate offline places.
- The AI that runs the bootstrap holds a root token from `init` until `configure` revokes it
  as its last step. Until the owner unseals, that token can do nothing; between the owner's
  unseal and the end of `configure` (minutes, with no secrets stored yet) it is live. This ADR
  reads the owner's "root key" exclusion as the material that unseals or regenerates root
  (the shares and the PGP key), which no AI ever holds; the owner can instead run `configure`
  themselves to keep even this short-lived token away from an AI.
- The restore drill needs the owner to unseal the restored node with the original shares.

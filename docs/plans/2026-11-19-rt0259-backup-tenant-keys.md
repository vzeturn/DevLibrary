# RT-0259: per-tenant backup keys, SRBackup hub side (spec)

The agent side is in `crates/dl-agent-backup` (`crypto/envelope.rs`, `hub::HubClient::backup_key*`,
`core.rs`). The hub that serves the backup module is the SRBackup hub (`/home/user/SRBackup/hub`,
Cloudflare Worker + D1), not `services/agent-hub` in this repo, so the hub change below is a spec
for that repository. Nothing here is deployed.

## Where things stand

- Credentials (server passwords, destination tokens) are already sealed per agent: SRBackup
  `f9ffb01` derives `deriveAgentKey(ENCRYPTION_MASTER_KEY, agentId)` (`hub/src/lib/agentKey.ts`)
  and `GET /api/agent/master-key` returns that derived key, never the master key. The
  INVENTORY §4 risk ("every agent gets the same master key") is closed for credentials.
- Backup files were never encrypted: the Go agent and dl-agent-backup before RT-0259 upload the
  plaintext `.bak`. "Old backups" are therefore plaintext; the agent restores them unchanged.

## Hub change

1. **Migration** (additive, next free number after `0008`):

   ```sql
   CREATE TABLE tenant_backup_keys (
     id          TEXT PRIMARY KEY,            -- key id, e.g. 'tbk_<uuid>' ([A-Za-z0-9._:-]{1,64})
     tenant_id   TEXT NOT NULL REFERENCES tenants(id),
     wrapped_key TEXT NOT NULL,               -- encryptCredential(base64url(32 random bytes), ENCRYPTION_MASTER_KEY)
     created_at  INTEGER NOT NULL,
     retired_at  INTEGER                       -- set on rotation; retired keys still open old backups
   );
   CREATE INDEX idx_tenant_backup_keys_tenant ON tenant_backup_keys(tenant_id, retired_at);
   ALTER TABLE backup_history ADD COLUMN encryption_key_id TEXT;
   ```

   Random keys stored wrapped (rather than HKDF of the master key) so a master-key rotation
   re-wraps rows instead of making every sealed backup unreadable.

2. **`GET /api/agent/backup-key`** (`requireAgentAuth`): the newest row with
   `tenant_id = agent.tenantId AND retired_at IS NULL`; when none exists, create one
   (32 bytes from `crypto.getRandomValues`, wrapped) inside the request. Answer
   `{"keyId": id, "key": base64url(unwrapped)}` with `Cache-Control: no-store`. Audit
   `agent.backup_key.fetched` with `{agentId, keyId}`.

3. **`GET /api/agent/backup-key?id=<keyId>`**: the row `WHERE id = ? AND tenant_id = agent.tenantId`
   (retired or not). Any other id, including one that exists for another tenant, is **404**
   with the same body as an unknown id (no existence oracle). Never 403 here: the agent treats
   401/403 on other routes as "agent rejected". Audit `agent.backup_key.fetched` with the id,
   and `agent.backup_key.denied` on a 404 for an id that belongs to another tenant.

4. **`POST /api/agent/report`**: accept optional `encryptionKeyId` on `type: 'backup'` and store
   it in `backup_history.encryption_key_id`; reject (400) an id that is not one of the agent's
   tenant's keys.

5. **Rotation** (admin route or script): set `retired_at` on the current row; the next
   `GET /backup-key` creates a new one. Agents pick it up within 15 minutes (their cache TTL).
   Retired rows are kept as long as any `backup_history` row names them.

## Hub tests (vitest, `hub/test/agent.backup-key.test.ts`)

- Agent of tenant A gets a key; a second call returns the same id and bytes.
- Agent of tenant A asking `?id=` of tenant B's key gets 404, and the body holds no key bytes.
- Two agents of the same tenant get the same key; agents of two tenants get different keys.
- A retired key is still returned by `?id=` to its own tenant, never as the current key.
- The response never contains `ENCRYPTION_MASTER_KEY` or a wrapped value.
- Report with a foreign `encryptionKeyId` is refused; with its own it is stored.

## Agent behaviour against an older hub

`GET /api/agent/backup-key` answering 404 means "no per-tenant keys": the agent uploads the
plaintext `.bak` (step `encrypt` = `skipped`) exactly as before, so the agent can ship before
the hub. Once the hub serves keys, new backups are sealed; old plaintext backups still restore.

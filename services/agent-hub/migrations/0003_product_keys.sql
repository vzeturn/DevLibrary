-- Product keys (RT-0164 E2a): a product platform (e.g. E-Invoice) mints dl-agent enroll codes for
-- its own tenants without holding the admin token. Only the SHA-256 of a key is stored.
CREATE TABLE product_keys (
  id         TEXT PRIMARY KEY,
  product    TEXT NOT NULL,
  key_hash   TEXT NOT NULL UNIQUE,
  label      TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  last_used  INTEGER,
  revoked_at INTEGER
);
CREATE INDEX product_keys_product ON product_keys (product, created_at);

-- Initial remote config an agent enrolled with this code starts from (JSON; NULL = built-in default).
ALTER TABLE enroll_codes ADD COLUMN config TEXT;
-- Product whose key minted the code (NULL = admin).
ALTER TABLE enroll_codes ADD COLUMN minted_by TEXT;

# Importer: writes one owner-approved batch of secrets. Create and update only: it cannot read,
# list or delete, so it never sees a stored value (ADR 0006).
path "kv/data/hub/*"      { capabilities = ["create", "update"] }
path "kv/data/projects/*" { capabilities = ["create", "update"] }
path "kv/data/shared/*"   { capabilities = ["create", "update"] }
path "kv/data/cloud/*"    { capabilities = ["create", "update"] }

# Hub admin (owner 2026-11-22): the RT hub runs Vault itself — mints AppRoles, secret IDs,
# policies and kit keys without the iMac. Root-equivalent for day-to-day work; a root token is
# never kept anywhere. The denies below stop accidents (seal, rekey, audit, unmounting the
# stores, rewriting this grant); they are not a hard wall, since the holder can write policies.
path "*" { capabilities = ["create", "read", "update", "delete", "list", "patch", "sudo"] }

path "sys/seal"                  { capabilities = ["deny"] }
path "sys/step-down"             { capabilities = ["deny"] }
path "sys/generate-root*"        { capabilities = ["deny"] }
path "sys/rekey*"                { capabilities = ["deny"] }
path "sys/rekey-recovery-key*"   { capabilities = ["deny"] }
path "sys/raw*"                  { capabilities = ["deny"] }
path "sys/audit*"                { capabilities = ["deny"] }
path "sys/mounts/kv"             { capabilities = ["deny"] }
path "sys/auth/approle"          { capabilities = ["deny"] }
path "sys/policies/acl/hub-admin" { capabilities = ["deny"] }
path "sys/policy/hub-admin"      { capabilities = ["deny"] }
path "kv/data/admin/dl-vault/*"  { capabilities = ["read", "list"] }
path "kv/metadata/admin/dl-vault/*" { capabilities = ["read", "list"] }

# Its own role stays as the iMac set it; the hub may only rotate its own secret ID.
path "auth/approle/role/hub-admin"   { capabilities = ["deny"] }
path "auth/approle/role/hub-admin/*" { capabilities = ["deny"] }
path "auth/approle/role/hub-admin/role-id"                    { capabilities = ["read"] }
path "auth/approle/role/hub-admin/secret-id"                  { capabilities = ["create", "update", "list"] }
path "auth/approle/role/hub-admin/secret-id/lookup"           { capabilities = ["update"] }
path "auth/approle/role/hub-admin/secret-id/destroy"          { capabilities = ["update"] }
path "auth/approle/role/hub-admin/secret-id-accessor/lookup"  { capabilities = ["update"] }
path "auth/approle/role/hub-admin/secret-id-accessor/destroy" { capabilities = ["update"] }

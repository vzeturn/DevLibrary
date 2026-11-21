# Hub AI (Request Tracking hub): owns the project kits and shared keys (ADR 0005).
path "kv/data/hub/*"          { capabilities = ["create", "read", "update", "delete"] }
path "kv/data/projects/*"     { capabilities = ["create", "read", "update", "delete"] }
path "kv/data/shared/*"       { capabilities = ["create", "read", "update", "delete"] }
path "kv/metadata/hub/*"      { capabilities = ["read", "list"] }
path "kv/metadata/projects/*" { capabilities = ["read", "list"] }
path "kv/metadata/shared/*"   { capabilities = ["read", "list"] }

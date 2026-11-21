# Hub AI (Request Tracking hub): read-only on its own keys, the project kits it hands to cloud
# sessions (RT-0514) and the shared keys. No writes, no cloud/, sys/ or auth/ (ADR 0006).
path "kv/data/hub/*"          { capabilities = ["read"] }
path "kv/data/projects/*"     { capabilities = ["read"] }
path "kv/data/shared/*"       { capabilities = ["read"] }
path "kv/metadata/hub/*"      { capabilities = ["list"] }
path "kv/metadata/projects/*" { capabilities = ["list"] }
path "kv/metadata/shared/*"   { capabilities = ["list"] }

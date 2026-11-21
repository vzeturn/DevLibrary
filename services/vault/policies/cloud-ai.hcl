# Cloud AI (cloud sessions): read-only on cloud tooling and shared keys. Project kits still
# come only through the hub's per-request pass (RT-0514, ADR 0005).
path "kv/data/cloud/*"  { capabilities = ["read"] }
path "kv/data/shared/*" { capabilities = ["read"] }

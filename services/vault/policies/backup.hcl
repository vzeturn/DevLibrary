# Backup loop: take a Raft snapshot, nothing else (ADR 0005).
path "sys/storage/raft/snapshot" { capabilities = ["read"] }

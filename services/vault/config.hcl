# OpenBao server config for dl-vault (ADR 0005).
ui            = false
disable_mlock = true # recommended with integrated storage; the machine has no swap

storage "raft" {
  path    = "/openbao/data/raft"
  node_id = "dl-vault-1"
}

# TLS ends at the Fly edge (port 443 only, see fly.toml).
listener "tcp" {
  address     = "[::]:8200"
  tls_disable = true
}

api_addr     = "https://dl-vault.fly.dev"
cluster_addr = "http://127.0.0.1:8201"

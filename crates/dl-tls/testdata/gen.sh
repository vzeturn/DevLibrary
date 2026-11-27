#!/usr/bin/env bash
# Regenerates the TEST-ONLY certificates and private keys of dl-tls's server tests with the
# OpenSSL command line (an implementation independent of dl-tls). These keys are public in
# this repository: never use them for anything but these tests.
#
#   crates/dl-tls/testdata/gen.sh        (OpenSSL 3; the repo's xbuild image has it)
#
# Files (DER unless named .pem; keys are unencrypted PKCS#8, RFC 5208):
#   ec-cert.der / ec-key.p8.der      self-signed P-256, CN=localhost, SAN localhost + 127.0.0.1
#   rsa-cert.der / rsa-key.p8.der    self-signed RSA 2048, same names
#   root-cert.pem                    test root CA (P-256), the trust anchor for curl/s_client
#   inter-cert.der                   intermediate CA signed by the root
#   leaf-cert.der / leaf-key.p8.der  P-256 leaf for localhost signed by the intermediate
#   p384-key.p8.der                  P-384 key, only to check that unsupported curves are refused
set -euo pipefail
cd "$(dirname "$0")"
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
days=36500
san="subjectAltName=DNS:localhost,IP:127.0.0.1"

der_key() { openssl pkcs8 -topk8 -nocrypt -in "$1" -outform DER -out "$2"; }
der_cert() { openssl x509 -in "$1" -outform DER -out "$2"; }

# Self-signed leaf certificates.
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days $days \
  -subj "/O=dl-tls test only/CN=localhost" -addext "$san" \
  -keyout "$tmp/ec-key.pem" -out "$tmp/ec-cert.pem" 2>/dev/null
der_key "$tmp/ec-key.pem" ec-key.p8.der
der_cert "$tmp/ec-cert.pem" ec-cert.der

openssl req -x509 -newkey rsa:2048 -nodes -days $days \
  -subj "/O=dl-tls test only/CN=localhost" -addext "$san" \
  -keyout "$tmp/rsa-key.pem" -out "$tmp/rsa-cert.pem" 2>/dev/null
der_key "$tmp/rsa-key.pem" rsa-key.p8.der
der_cert "$tmp/rsa-cert.pem" rsa-cert.der

# Root -> intermediate -> leaf.
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days $days \
  -subj "/O=dl-tls test only/CN=dl-tls test root" \
  -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" \
  -keyout "$tmp/root-key.pem" -out root-cert.pem 2>/dev/null

openssl req -new -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -subj "/O=dl-tls test only/CN=dl-tls test intermediate" \
  -keyout "$tmp/inter-key.pem" -out "$tmp/inter.csr" 2>/dev/null
printf '%s\n' "basicConstraints=critical,CA:TRUE,pathlen:0" "keyUsage=critical,keyCertSign,cRLSign" >"$tmp/inter.ext"
openssl x509 -req -in "$tmp/inter.csr" -CA root-cert.pem -CAkey "$tmp/root-key.pem" -CAserial "$tmp/ca.srl" -CAcreateserial \
  -days $days -extfile "$tmp/inter.ext" -out "$tmp/inter-cert.pem" 2>/dev/null
der_cert "$tmp/inter-cert.pem" inter-cert.der

openssl req -new -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -subj "/O=dl-tls test only/CN=localhost" \
  -keyout "$tmp/leaf-key.pem" -out "$tmp/leaf.csr" 2>/dev/null
printf '%s\n' "basicConstraints=critical,CA:FALSE" "keyUsage=critical,digitalSignature" \
  "extendedKeyUsage=serverAuth" "$san" >"$tmp/leaf.ext"
openssl x509 -req -in "$tmp/leaf.csr" -CA "$tmp/inter-cert.pem" -CAkey "$tmp/inter-key.pem" -CAserial "$tmp/ca.srl" -CAcreateserial \
  -days $days -extfile "$tmp/leaf.ext" -out "$tmp/leaf-cert.pem" 2>/dev/null
der_key "$tmp/leaf-key.pem" leaf-key.p8.der
der_cert "$tmp/leaf-cert.pem" leaf-cert.der

openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-384 -out "$tmp/p384.pem" 2>/dev/null
der_key "$tmp/p384.pem" p384-key.p8.der

# The chain must verify with the OpenSSL verifier before the tests rely on it.
openssl verify -CAfile root-cert.pem -untrusted "$tmp/inter-cert.pem" "$tmp/leaf-cert.pem" >/dev/null
echo "testdata regenerated"

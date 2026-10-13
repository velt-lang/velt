#!/bin/sh
# Regenerates the chatbench TEST-ONLY TLS material: a CA (ca.pem) and a server certificate
# (cert.pem, key.pem) for 127.0.0.1 and localhost, signed by it. The keys are public; never use
# them for anything but this benchmark. Key identifiers are included because strict verifiers
# (Python 3.13+'s default VERIFY_X509_STRICT) require them.
set -eu
cd "$(dirname "$0")"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 3650 \
  -subj "/CN=chatbench test CA" \
  -addext "basicConstraints=critical,CA:TRUE" \
  -addext "keyUsage=critical,keyCertSign,cRLSign" \
  -addext "subjectKeyIdentifier=hash" \
  -keyout "$tmp/ca-key.pem" -out ca.pem

openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
  -subj "/CN=localhost" -keyout "$tmp/key.pem" -out "$tmp/server.csr"
# PKCS#8 ("BEGIN PRIVATE KEY"), which every server here reads.
openssl pkcs8 -topk8 -nocrypt -in "$tmp/key.pem" -out key.pem

cat > "$tmp/ext.cnf" <<EXT
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature
extendedKeyUsage=serverAuth
subjectAltName=IP:127.0.0.1,DNS:localhost
subjectKeyIdentifier=hash
authorityKeyIdentifier=keyid,issuer
EXT
openssl x509 -req -in "$tmp/server.csr" -CA ca.pem -CAkey "$tmp/ca-key.pem" -CAcreateserial \
  -CAserial "$tmp/ca.srl" -days 3650 -extfile "$tmp/ext.cnf" -out cert.pem

openssl verify -CAfile ca.pem cert.pem

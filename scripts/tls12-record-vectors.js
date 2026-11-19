// Prints TLS 1.2 AEAD record vectors (RFC 5246 §6.2.3.3 with RFC 5288's explicit nonce for AES-GCM
// and RFC 7905's nonce for ChaCha20-Poly1305), computed with Node's crypto, for
// std/net/tls/record12.test.velt. Usage: node scripts/tls12-record-vectors.js
const crypto = require("crypto");

const seqBytes = (seq) => {
  const b = Buffer.alloc(8);
  b.writeBigUInt64BE(BigInt(seq));
  return b;
};

function record(alg, key, iv, seq, type, plain, nonceSeq = seq) {
  const header = Buffer.from([type, 3, 3]);
  const len = Buffer.alloc(2);
  len.writeUInt16BE(plain.length);
  const aad = Buffer.concat([seqBytes(seq), header, len]);
  let nonce;
  let explicit = Buffer.alloc(0);
  if (alg === "chacha20-poly1305") {
    nonce = Buffer.from(iv);
    const s = seqBytes(seq);
    for (let i = 0; i < 8; i++) nonce[4 + i] ^= s[i];
  } else {
    explicit = seqBytes(nonceSeq);
    nonce = Buffer.concat([iv, explicit]);
  }
  const c = crypto.createCipheriv(alg, key, nonce, { authTagLength: 16 });
  c.setAAD(aad);
  const body = Buffer.concat([explicit, c.update(plain), c.final(), c.getAuthTag()]);
  const blen = Buffer.alloc(2);
  blen.writeUInt16BE(body.length);
  return Buffer.concat([header, blen, body]).toString("hex");
}

const fill = (n, start) => Buffer.from(Array.from({ length: n }, (_, i) => (start + i) & 255));
const plain = Buffer.from("hello, TLS 1.2");
const cases = [
  ["AES-128-GCM", "aes-128-gcm", fill(16, 1), fill(4, 0xa1)],
  ["AES-256-GCM", "aes-256-gcm", fill(32, 0x40), fill(4, 0xb1)],
  ["ChaCha20-Poly1305", "chacha20-poly1305", fill(32, 0x80), fill(12, 0xc1)],
];
for (const [name, alg, key, iv] of cases) {
  console.log(`${name} key ${key.toString("hex")} iv ${iv.toString("hex")}`);
  console.log(`  seq 0 type 23: ${record(alg, key, iv, 0, 23, plain)}`);
  console.log(`  seq 1 type 23: ${record(alg, key, iv, 1, 23, plain)}`);
  console.log(`  seq 2 type 21: ${record(alg, key, iv, 2, 21, Buffer.from([1, 0]))}`);
  console.log(`  seq 0x0102030405060708 type 22: ${record(alg, key, iv, 0x0102030405060708n, 22, plain)}`);
}
// A peer may choose any explicit nonce (RFC 5288 §3): sequence number 0, explicit nonce
// 0x1122334455667788.
const [, alg, key, iv] = cases[0];
console.log(`AES-128-GCM seq 0, explicit nonce 1122334455667788: ${record(alg, key, iv, 0, 23, plain, 0x1122334455667788n)}`);

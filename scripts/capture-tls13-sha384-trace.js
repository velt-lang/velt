// Captures a TLS 1.3 handshake trace from a real server forced to TLS_AES_256_GCM_SHA384:
// sends a fixed ClientHello (fixed random, session id and X25519 key), answers a
// HelloRetryRequest with a fixed P-256 share, and writes the hellos and the server's records as
// JSON (std/net/tls/openssl_sha384.test.velt's traces). The expected secrets come from the
// server's key log. Usage, with a P-256 certificate:
//   openssl s_server -accept 44311 -cert cert.pem -key key.pem -tls1_3 \
//     -ciphersuites TLS_AES_256_GCM_SHA384 -groups X25519:P-256 -keylogfile keylog.txt -naccept 1
//   node scripts/capture-tls13-sha384-trace.js 44311 trace.json
// (`-groups P-256` for the HelloRetryRequest trace.)
const net = require("net");
const crypto = require("crypto");
const fs = require("fs");

const port = Number(process.argv[2]);
const out = process.argv[3];

const X25519_PRIVATE = Buffer.from("a0".repeat(16) + "b1".repeat(16), "hex");
const P256_PRIVATE = Buffer.from("11".repeat(31) + "07", "hex");

function x25519Public(priv) {
  // PKCS#8 DER for an X25519 private key: fixed prefix + 32 bytes.
  const der = Buffer.concat([Buffer.from("302e020100300506032b656e04220420", "hex"), priv]);
  const key = crypto.createPrivateKey({ key: der, format: "der", type: "pkcs8" });
  const spki = crypto.createPublicKey(key).export({ format: "der", type: "spki" });
  return spki.subarray(spki.length - 32);
}

function p256Public(priv) {
  const ecdh = crypto.createECDH("prime256v1");
  ecdh.setPrivateKey(priv);
  return ecdh.getPublicKey();
}

function u16(n) {
  return Buffer.from([(n >> 8) & 255, n & 255]);
}
function vec(lenBytes, body) {
  const l = Buffer.alloc(lenBytes);
  l.writeUIntBE(body.length, 0, lenBytes);
  return Buffer.concat([l, body]);
}
function ext(type, body) {
  return Buffer.concat([u16(type), vec(2, body)]);
}

function clientHello(group, share, cookie) {
  const exts = [
    ext(0, vec(2, Buffer.concat([Buffer.from([0]), vec(2, Buffer.from("localhost"))]))),
    ext(43, vec(1, u16(0x0304))),
    ext(10, vec(2, Buffer.concat([u16(0x001d), u16(0x0017)]))),
    ext(13, vec(2, Buffer.concat([0x0403, 0x0503, 0x0804, 0x0805, 0x0806, 0x0401, 0x0501, 0x0601].map(u16)))),
    ext(51, vec(2, Buffer.concat([u16(group), vec(2, share)]))),
  ];
  if (cookie) exts.push(ext(44, vec(2, cookie)));
  const body = Buffer.concat([
    u16(0x0303),
    Buffer.alloc(32, 0x5a),
    vec(1, Buffer.alloc(32, 0x3c)),
    vec(2, u16(0x1302)),
    Buffer.from([1, 0]),
    vec(2, Buffer.concat(exts)),
  ]);
  return Buffer.concat([Buffer.from([1]), vec(3, body)]);
}

function record(type, body) {
  return Buffer.concat([Buffer.from([type, 3, 1]), vec(2, body)]);
}

const ch1 = clientHello(0x001d, x25519Public(X25519_PRIVATE), null);
let buf = Buffer.alloc(0);
let ch2 = null;
const records = [];
let sock = null;
let tries = 0;
function attempt() {
  sock = net.connect(port, "127.0.0.1");
  sock.on("error", () => {
    if (++tries < 100) setTimeout(attempt, 100);
  });
  sock.on("connect", () => {
    sock.write(record(22, ch1));
    setTimeout(finish, 1500);
  });
  sock.on("data", onData);
}
attempt();
function onData(d) {
  buf = Buffer.concat([buf, d]);
  while (buf.length >= 5) {
    const len = buf.readUInt16BE(3);
    if (buf.length < 5 + len) break;
    const rec = buf.subarray(0, 5 + len);
    buf = buf.subarray(5 + len);
    records.push(rec.toString("hex"));
    if (rec[0] == 22 && rec[5] == 2 && ch2 == null) {
      const random = rec.subarray(11, 43).toString("hex");
      if (random.startsWith("cf21ad74")) {
        // A HelloRetryRequest: find a cookie, answer with a P-256 share.
        let cookie = null;
        const sidLen = rec[43];
        let p = 44 + sidLen + 3;
        const end = p + 2 + rec.readUInt16BE(p);
        p += 2;
        while (p < end) {
          const t = rec.readUInt16BE(p);
          const l = rec.readUInt16BE(p + 2);
          if (t == 44) cookie = rec.subarray(p + 6, p + 4 + l);
          p += 4 + l;
        }
        ch2 = clientHello(0x0017, p256Public(P256_PRIVATE), cookie);
        sock.write(record(22, ch2));
      }
    }
  }
}
function finish() {
  fs.writeFileSync(
    out,
    JSON.stringify(
      {
        x25519Private: X25519_PRIVATE.toString("hex"),
        p256Private: P256_PRIVATE.toString("hex"),
        clientHello1: ch1.toString("hex"),
        clientHello2: ch2 ? ch2.toString("hex") : null,
        records,
      },
      null,
      1,
    ),
  );
  sock.destroy();
  process.exit(0);
}

// Captures a TLS 1.2 handshake and a request from a real server, for
// std/net/tls/openssl_tls12.test.velt: sends a fixed ClientHello (random 5a.., session id 3c..,
// the X25519 key below; TLS 1.3's suites and TLS 1.2's six, extended_master_secret,
// renegotiation_info, ec_point_formats), answers the server's flight with a ClientKeyExchange
// (that X25519 key, or the P-256 key below), a ChangeCipherSpec and its Finished, sends a request,
// and writes what each side sent as JSON. The key schedule is checked against the server's key
// log (-keylogfile), and the server's Finished against this script's own PRF. Usage:
//   openssl s_server -accept 44312 -cert cert.pem -key key.pem -tls1_2 \
//     -cipher ECDHE-ECDSA-AES128-GCM-SHA256 -groups X25519 -keylogfile keylog.txt -www -naccept 1
//   node scripts/capture-tls12-trace.js 44312 keylog.txt trace.json [noems]
const net = require("net");
const crypto = require("crypto");
const fs = require("fs");

// A fourth argument, "noems", leaves extended_master_secret out of the ClientHello.
const [port, keylogFile, out, option] = process.argv.slice(2);
const offerEms = option !== "noems";

const X25519_PRIVATE = Buffer.from("a0".repeat(16) + "b1".repeat(16), "hex");
const P256_PRIVATE = Buffer.from("11".repeat(31) + "07", "hex");
const CLIENT_RANDOM = Buffer.alloc(32, 0x5a);

function x25519Key(priv) {
  const der = Buffer.concat([Buffer.from("302e020100300506032b656e04220420", "hex"), priv]);
  return crypto.createPrivateKey({ key: der, format: "der", type: "pkcs8" });
}
function x25519Public(priv) {
  const spki = crypto.createPublicKey(x25519Key(priv)).export({ format: "der", type: "spki" });
  return spki.subarray(spki.length - 32);
}
function x25519Shared(priv, peer) {
  const spki = Buffer.concat([Buffer.from("302a300506032b656e032100", "hex"), peer]);
  const pub = crypto.createPublicKey({ key: spki, format: "der", type: "spki" });
  return crypto.diffieHellman({ privateKey: x25519Key(priv), publicKey: pub });
}

const u16 = (n) => Buffer.from([(n >> 8) & 255, n & 255]);
function vec(lenBytes, body) {
  const l = Buffer.alloc(lenBytes);
  l.writeUIntBE(body.length, 0, lenBytes);
  return Buffer.concat([l, body]);
}
const ext = (type, body) => Buffer.concat([u16(type), vec(2, body)]);
const msg = (type, body) => Buffer.concat([Buffer.from([type]), vec(3, body)]);
const record = (type, minor, body) => Buffer.concat([Buffer.from([type, 3, minor]), vec(2, body)]);

const SUITES = [0x1301, 0x1302, 0x1303, 0xc02b, 0xc02f, 0xc02c, 0xc030, 0xcca9, 0xcca8];
const clientHello = msg(
  1,
  Buffer.concat([
    u16(0x0303),
    CLIENT_RANDOM,
    vec(1, Buffer.alloc(32, 0x3c)),
    vec(2, Buffer.concat(SUITES.map(u16))),
    Buffer.from([1, 0]),
    vec(
      2,
      Buffer.concat([
        ext(0, vec(2, Buffer.concat([Buffer.from([0]), vec(2, Buffer.from("localhost"))]))),
        ext(43, vec(1, Buffer.concat([u16(0x0304), u16(0x0303)]))),
        ...(offerEms ? [ext(23, Buffer.alloc(0))] : []),
        ext(0xff01, Buffer.from([0])),
        ext(11, Buffer.from([1, 0])),
        ext(10, vec(2, Buffer.concat([u16(0x001d), u16(0x0017)]))),
        ext(13, vec(2, Buffer.concat([0x0403, 0x0503, 0x0804, 0x0805, 0x0806, 0x0807, 0x0401, 0x0501, 0x0601].map(u16)))),
        ext(51, vec(2, Buffer.concat([u16(0x001d), vec(2, x25519Public(X25519_PRIVATE))]))),
      ]),
    ),
  ]),
);

function prf(hash, secret, label, seed, n) {
  const ls = Buffer.concat([Buffer.from(label), seed]);
  let a = ls;
  const parts = [];
  let len = 0;
  while (len < n) {
    a = crypto.createHmac(hash, secret).update(a).digest();
    const b = crypto.createHmac(hash, secret).update(Buffer.concat([a, ls])).digest();
    parts.push(b);
    len += b.length;
  }
  return Buffer.concat(parts).subarray(0, n);
}

// The AEAD of `suite`, sealing or opening TLS 1.2 records.
function aead(suite, key, iv) {
  const chacha = suite === 0xcca8 || suite === 0xcca9;
  const alg = chacha ? "chacha20-poly1305" : key.length === 16 ? "aes-128-gcm" : "aes-256-gcm";
  let seq = 0n;
  const aad = (type, n) => {
    const s = Buffer.alloc(8);
    s.writeBigUInt64BE(seq);
    return Buffer.concat([s, Buffer.from([type, 3, 3]), u16(n)]);
  };
  const nonce = (explicit) => {
    if (!chacha) return Buffer.concat([iv, explicit]);
    const s = Buffer.alloc(8);
    s.writeBigUInt64BE(seq);
    const n = Buffer.from(iv);
    for (let i = 0; i < 8; i++) n[4 + i] ^= s[i];
    return n;
  };
  return {
    seal(type, plain) {
      const explicit = Buffer.alloc(chacha ? 0 : 8);
      if (!chacha) explicit.writeBigUInt64BE(seq);
      const c = crypto.createCipheriv(alg, key, nonce(explicit), { authTagLength: 16 });
      c.setAAD(aad(type, plain.length));
      const body = Buffer.concat([explicit, c.update(plain), c.final(), c.getAuthTag()]);
      seq++;
      return record(type, 3, body);
    },
    open(rec) {
      const type = rec[0];
      const body = rec.subarray(5);
      const pre = chacha ? 0 : 8;
      const n = body.length - pre - 16;
      const d = crypto.createDecipheriv(alg, key, nonce(body.subarray(0, pre)), { authTagLength: 16 });
      d.setAAD(aad(type, n));
      d.setAuthTag(body.subarray(body.length - 16));
      const plain = Buffer.concat([d.update(body.subarray(pre, pre + n)), d.final()]);
      seq++;
      return plain;
    },
  };
}

const serverRecords = [];
const clientSent = [];
let buf = Buffer.alloc(0);
let hsBuf = Buffer.alloc(0);
const transcript = [clientHello];
let state = "flight";
let suite = 0;
let hash = "sha256";
let serverRandom = null;
let ems = false;
let group = 0;
let serverPoint = null;
let master = null;
let keys = null;
let reader = null;
let response = Buffer.alloc(0);

function handshakeMessages() {
  const msgs = [];
  while (hsBuf.length >= 4) {
    const n = hsBuf.readUIntBE(1, 3);
    if (hsBuf.length < 4 + n) break;
    msgs.push(hsBuf.subarray(0, 4 + n));
    hsBuf = hsBuf.subarray(4 + n);
  }
  return msgs;
}

function onServerMessage(m, sock) {
  const type = m[0];
  if (type === 2) {
    serverRandom = m.subarray(6, 38);
    const sidLen = m[38];
    suite = m.readUInt16BE(39 + sidLen);
    hash = suite === 0xc02c || suite === 0xc030 ? "sha384" : "sha256";
    let p = 39 + sidLen + 3;
    const end = p + 2 + m.readUInt16BE(p);
    p += 2;
    while (p < end) {
      const t = m.readUInt16BE(p);
      const l = m.readUInt16BE(p + 2);
      if (t === 23) ems = true;
      p += 4 + l;
    }
  } else if (type === 12) {
    group = m.readUInt16BE(5);
    serverPoint = m.subarray(8, 8 + m[7]);
  }
  transcript.push(m);
  if (type !== 14) return;
  // ServerHelloDone: the client's flight.
  let ours;
  let shared;
  if (group === 0x001d) {
    ours = x25519Public(X25519_PRIVATE);
    shared = x25519Shared(X25519_PRIVATE, serverPoint);
  } else {
    const e = crypto.createECDH("prime256v1");
    e.setPrivateKey(P256_PRIVATE);
    ours = e.getPublicKey();
    shared = e.computeSecret(serverPoint);
  }
  const cke = msg(16, vec(1, ours));
  transcript.push(cke);
  const th = () => crypto.createHash(hash).update(Buffer.concat(transcript)).digest();
  master = ems
    ? prf(hash, shared, "extended master secret", th(), 48)
    : prf(hash, shared, "master secret", Buffer.concat([CLIENT_RANDOM, serverRandom]), 48);
  const chacha = suite === 0xcca8 || suite === 0xcca9;
  const keyLen = suite === 0xc02b || suite === 0xc02f ? 16 : 32;
  const ivLen = chacha ? 12 : 4;
  const kb = prf(hash, master, "key expansion", Buffer.concat([serverRandom, CLIENT_RANDOM]), 2 * keyLen + 2 * ivLen);
  keys = {
    ck: kb.subarray(0, keyLen),
    sk: kb.subarray(keyLen, 2 * keyLen),
    civ: kb.subarray(2 * keyLen, 2 * keyLen + ivLen),
    siv: kb.subarray(2 * keyLen + ivLen),
  };
  const fin = msg(20, prf(hash, master, "client finished", th(), 12));
  transcript.push(fin);
  const writer = aead(suite, keys.ck, keys.civ);
  const flight = Buffer.concat([
    record(22, 3, cke),
    record(20, 3, Buffer.from([1])),
    writer.seal(22, fin),
    writer.seal(23, Buffer.from("GET / HTTP/1.0\r\n\r\n")),
  ]);
  clientSent.push(flight.toString("hex"));
  sock.write(flight);
  state = "ccs";
}

function onRecord(rec, sock) {
  serverRecords.push(rec.toString("hex"));
  const type = rec[0];
  if (state === "flight") {
    if (type !== 22) throw new Error(`record ${type} in the server's flight`);
    hsBuf = Buffer.concat([hsBuf, rec.subarray(5)]);
    for (const m of handshakeMessages()) onServerMessage(m, sock);
  } else if (state === "ccs") {
    if (type !== 20) throw new Error(`record ${type} instead of ChangeCipherSpec`);
    reader = aead(suite, keys.sk, keys.siv);
    state = "finished";
  } else if (state === "finished") {
    const fin = reader.open(rec);
    const th = crypto.createHash(hash).update(Buffer.concat(transcript)).digest();
    const want = msg(20, prf(hash, master, "server finished", th, 12));
    if (!fin.equals(want)) throw new Error("the server's Finished does not match");
    state = "data";
  } else {
    const plain = reader.open(rec);
    if (type === 23) response = Buffer.concat([response, plain]);
  }
}

const sock = net.connect(Number(port), "127.0.0.1", () => {
  const ch = record(22, 1, clientHello);
  sock.write(ch);
});
sock.on("data", (d) => {
  buf = Buffer.concat([buf, d]);
  while (buf.length >= 5) {
    const n = buf.readUInt16BE(3);
    if (buf.length < 5 + n) break;
    onRecord(buf.subarray(0, 5 + n), sock);
    buf = buf.subarray(5 + n);
  }
});
sock.on("close", () => {
  const log = fs.readFileSync(keylogFile, "utf8");
  const line = log.split("\n").find((l) => l.startsWith(`CLIENT_RANDOM ${CLIENT_RANDOM.toString("hex")}`));
  const logged = line ? line.trim().split(" ")[2] : "";
  if (logged !== master.toString("hex")) throw new Error(`master ${master.toString("hex")} != key log ${logged}`);
  fs.writeFileSync(
    out,
    JSON.stringify(
      {
        suite: suite.toString(16),
        group: group.toString(16),
        ems,
        clientHello: clientHello.toString("hex"),
        serverRecords,
        clientRecords: clientSent,
        master: master.toString("hex"),
        response: response.toString("latin1").split("\r\n")[0],
      },
      null,
      1,
    ),
  );
  console.log(`suite ${suite.toString(16)} group ${group.toString(16)} ems ${ems} master ok, ${serverRecords.length} server records, response ${JSON.stringify(response.toString("latin1").split("\r\n")[0])}`);
});

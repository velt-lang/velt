// The client's side of std/net/tls/server_tls12.test.velt, computed with Node's crypto: a fixed
// TLS 1.2-only ClientHello (as OpenSSL 1.1.1's s_client -tls1_2 sends one: no
// supported_versions, extended_master_secret, renegotiation_info, ec_point_formats, ALPN), and,
// given the flight the server answered with, the ServerKeyExchange's signature checked against
// the certificate, then the client's records (ClientKeyExchange with the fixed X25519 key,
// ChangeCipherSpec, Finished, "ping") and the server records it expects back (ChangeCipherSpec,
// Finished, "pong").
//   node scripts/tls12-server-trace.js hello <suite hex>[-noems]
//   node scripts/tls12-server-trace.js client <suite hex> <server flight records hex>
const crypto = require("crypto");

const X25519_PRIVATE = Buffer.from("c4".repeat(31) + "45", "hex");
const CLIENT_RANDOM = Buffer.alloc(32, 0x6b);

const u16 = (n) => Buffer.from([(n >> 8) & 255, n & 255]);
function vec(lenBytes, body) {
  const l = Buffer.alloc(lenBytes);
  l.writeUIntBE(body.length, 0, lenBytes);
  return Buffer.concat([l, body]);
}
const ext = (type, body) => Buffer.concat([u16(type), vec(2, body)]);
const msg = (type, body) => Buffer.concat([Buffer.from([type]), vec(3, body)]);
const record = (type, body) => Buffer.concat([Buffer.from([type, 3, 3]), vec(2, body)]);

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

function prf(hash, secret, label, seed, n) {
  const ls = Buffer.concat([Buffer.from(label), seed]);
  let a = ls;
  const parts = [];
  let len = 0;
  while (len < n) {
    a = crypto.createHmac(hash, secret).update(a).digest();
    parts.push(crypto.createHmac(hash, secret).update(Buffer.concat([a, ls])).digest());
    len += 32;
  }
  return Buffer.concat(parts).subarray(0, n);
}

function sealer(suite, key, iv) {
  const chacha = suite === 0xcca9;
  const alg = chacha ? "chacha20-poly1305" : key.length === 16 ? "aes-128-gcm" : "aes-256-gcm";
  let seq = 0n;
  return (type, plain) => {
    const s = Buffer.alloc(8);
    s.writeBigUInt64BE(seq);
    let nonce;
    let explicit = Buffer.alloc(0);
    if (chacha) {
      nonce = Buffer.from(iv);
      for (let i = 0; i < 8; i++) nonce[4 + i] ^= s[i];
    } else {
      explicit = s;
      nonce = Buffer.concat([iv, s]);
    }
    const c = crypto.createCipheriv(alg, key, nonce, { authTagLength: 16 });
    c.setAAD(Buffer.concat([s, Buffer.from([type, 3, 3]), u16(plain.length)]));
    seq++;
    return record(type, Buffer.concat([explicit, c.update(plain), c.final(), c.getAuthTag()]));
  };
}

function clientHello(suite) {
  return msg(
    1,
    Buffer.concat([
      u16(0x0303),
      CLIENT_RANDOM,
      vec(1, Buffer.alloc(0)),
      vec(2, Buffer.concat([suite, 0xc02f, 0x00ff].map(u16))),
      Buffer.from([1, 0]),
      vec(
        2,
        Buffer.concat([
          ext(0, vec(2, Buffer.concat([Buffer.from([0]), vec(2, Buffer.from("localhost"))]))),
          ext(11, Buffer.from([3, 0, 1, 2])),
          ext(10, vec(2, Buffer.concat([u16(0x001d), u16(0x0017), u16(0x0018)]))),
          ext(16, vec(2, vec(1, Buffer.from("http/1.1")))),
          ...(ems ? [ext(23, Buffer.alloc(0))] : []),
          ext(13, vec(2, Buffer.concat([0x0403, 0x0503, 0x0804, 0x0401].map(u16)))),
        ]),
      ),
    ]),
  );
}

const [mode, suiteHex, flightHex] = process.argv.slice(2);
const suite = parseInt(suiteHex, 16);
// "c02b-noems": without extended_master_secret, so the master secret comes from the randoms.
const ems = !suiteHex.endsWith("-noems");
const hello = clientHello(suite);
if (mode === "hello") {
  console.log(hello.toString("hex"));
  process.exit(0);
}
// The server's handshake messages from its records.
const flight = Buffer.from(flightHex, "hex");
let hs = Buffer.alloc(0);
for (let p = 0; p < flight.length; ) {
  const n = flight.readUInt16BE(p + 3);
  if (flight[p] !== 22) throw new Error("a record that is not handshake in the flight");
  hs = Buffer.concat([hs, flight.subarray(p + 5, p + 5 + n)]);
  p += 5 + n;
}
const msgs = [];
for (let p = 0; p < hs.length; ) {
  const n = hs.readUIntBE(p + 1, 3);
  msgs.push(hs.subarray(p, p + 4 + n));
  p += 4 + n;
}
const [sh, cert, ske, shd] = msgs;
if (msgs.length !== 4 || sh[0] !== 2 || cert[0] !== 11 || ske[0] !== 12 || shd[0] !== 14) {
  throw new Error(`unexpected flight ${msgs.map((m) => m[0])}`);
}
const serverRandom = sh.subarray(6, 38);
if (sh.readUInt16BE(39 + sh[38]) !== suite) throw new Error("another suite");
// The ServerKeyExchange's signature, against the leaf certificate's key.
const leaf = cert.subarray(10, 10 + cert.readUIntBE(7, 3));
const pub = new crypto.X509Certificate(leaf).publicKey;
const paramsLen = 4 + ske[7];
const params = ske.subarray(4, 4 + paramsLen);
const scheme = ske.readUInt16BE(4 + paramsLen);
const sig = ske.subarray(4 + paramsLen + 4);
const signed = Buffer.concat([CLIENT_RANDOM, serverRandom, params]);
const ok = crypto.verify("sha256", signed, pub, sig);
if (scheme !== 0x0403 || !ok) throw new Error(`the ServerKeyExchange signature does not verify (scheme ${scheme})`);
const group = params.readUInt16BE(1);
if (group !== 0x001d) throw new Error("not X25519");
const shared = x25519Shared(X25519_PRIVATE, params.subarray(4));
const hash = suite === 0xc02c ? "sha384" : "sha256";
const transcript = [hello, sh, cert, ske, shd];
const cke = msg(16, vec(1, x25519Public(X25519_PRIVATE)));
transcript.push(cke);
const th = () => crypto.createHash(hash).update(Buffer.concat(transcript)).digest();
const master = ems
  ? prf(hash, shared, "extended master secret", th(), 48)
  : prf(hash, shared, "master secret", Buffer.concat([CLIENT_RANDOM, serverRandom]), 48);
const chacha = suite === 0xcca9;
const keyLen = suite === 0xc02b ? 16 : 32;
const ivLen = chacha ? 12 : 4;
const kb = prf(hash, master, "key expansion", Buffer.concat([serverRandom, CLIENT_RANDOM]), 2 * keyLen + 2 * ivLen);
const ck = kb.subarray(0, keyLen);
const sk = kb.subarray(keyLen, 2 * keyLen);
const civ = kb.subarray(2 * keyLen, 2 * keyLen + ivLen);
const siv = kb.subarray(2 * keyLen + ivLen);
const cfin = msg(20, prf(hash, master, "client finished", th(), 12));
transcript.push(cfin);
const sfin = msg(20, prf(hash, master, "server finished", th(), 12));
const cw = sealer(suite, ck, civ);
const sw = sealer(suite, sk, siv);
const client = Buffer.concat([
  record(22, cke),
  record(20, Buffer.from([1])),
  cw(22, cfin),
  cw(23, Buffer.from("ping")),
]);
const server = Buffer.concat([record(20, Buffer.from([1])), sw(22, sfin), sw(23, Buffer.from("pong"))]);
console.log(`signature ok; client ${client.toString("hex")}`);
console.log(`server ${server.toString("hex")}`);
// A client Finished with a wrong verify_data, protected as the right one is.
const bad = Buffer.from(cfin);
bad[bad.length - 1] ^= 1;
const badRecords = Buffer.concat([record(22, cke), record(20, Buffer.from([1])), sealer(suite, ck, civ)(22, bad)]);
console.log(`badfinished ${badRecords.toString("hex")}`);
// A renegotiation: the ClientHello again, protected, after "ping".
console.log(`renegotiation ${cw(22, hello).toString("hex")}`);

// Writes the interop corpus to the directory in argv[2]: the edges of a stream (empty, one byte),
// random bytes, repetitive bytes, text and JSON-like text, at sizes around DEFLATE's window
// (32 KiB), a stored block's limit (64 KiB) and a block's symbol limit, and past Brotli's
// meta-block (1 MiB), its longest match (1 MiB) and near its window (4 MiB). Deterministic (a fixed xorshift),
// so ratios compare between runs.
import { mkdirSync, writeFileSync } from "node:fs";

const dir = process.argv[2];
mkdirSync(dir, { recursive: true });

let state = 0x2075;
function next() {
  state ^= state << 13;
  state ^= state >>> 17;
  state ^= state << 5;
  state >>>= 0;
  return state;
}

function random(n) {
  const out = Buffer.alloc(n);
  for (let i = 0; i < n; i++) out[i] = next() & 255;
  return out;
}

const WORDS =
  "the quick brown fox jumps over a lazy dog while streams carry compressed bytes through windows of memory and chains of hashes find matches for every prefix they have seen before".split(" ");

function text(n) {
  let s = "";
  while (s.length < n) {
    s += WORDS[next() % WORDS.length] + (next() % 11 == 0 ? ".\n" : " ");
  }
  return Buffer.from(s.slice(0, n));
}

function json(n) {
  let s = "[";
  for (let i = 0; s.length < n; i++) {
    s += JSON.stringify({ id: i, name: WORDS[next() % WORDS.length], score: next() % 1000, ok: i % 3 == 0 }) + ",";
  }
  return Buffer.from(s.slice(0, n));
}

const K = 1024;
const SIZES = [2, 3, 257, 258, 259, 262, 1000, 32 * K - 262, 32 * K - 1, 32 * K, 32 * K + 1, 64 * K - 1, 64 * K, 64 * K + 1, 100 * K, 1024 * K];
const put = (name, bytes) => writeFileSync(`${dir}/${name}`, bytes);
put("empty", Buffer.alloc(0));
put("one", Buffer.from([65]));
for (const n of SIZES) {
  put(`random-${n}`, random(n));
  put(`zeros-${n}`, Buffer.alloc(n));
  put(`abc-${n}`, Buffer.alloc(n, "abc"));
  put(`text-${n}`, text(n));
}
put("json-1048576", json(1024 * K));
// Half compressible, half not: a stored block next to Huffman blocks.
put("mixed-300000", Buffer.concat([text(100000), random(100000), text(100000)]));
// Matches at the far end of the window: a block of random bytes repeated.
const far = random(32000);
put("far-96000", Buffer.concat([far, far, far]));
// Brotli's limits: several meta-blocks of text, a run longer than the longest match, and a block
// repeated 4 MiB later, just inside the window, then again just outside it.
put("text-3145728", text(3 * 1024 * K));
put("zeros-3145728", Buffer.alloc(3 * 1024 * K));
const block = random(64 * K);
put("far4m", Buffer.concat([block, text(4 * 1024 * K - 64 * K - 16), block, text(64 * K), block]));

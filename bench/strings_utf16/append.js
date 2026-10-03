// Append loop (same workload as append.vlt): 20 strings of about 190k characters, each built by
// 100k `s += piece` appends of non-ASCII pieces, with a strided charCodeAt checksum over each.
class Rng {
  seed = 42;

  next(n) {
    this.seed = (this.seed * 48271) % 2147483647;
    return this.seed % n;
  }

  pick(xs) {
    return xs[this.next(xs.length)];
  }
}

const PIECES = "é|ab|日本|😀| |ça|👍🏽|x|€|🚀z";

const rng = new Rng();
const pieces = PIECES.split("|");
let total = 0;
let h = 0;
let s = "";
for (let round = 0; round < 20; round++) {
  s = "";
  for (let i = 0; i < 100000; i++) {
    s += rng.pick(pieces);
  }
  total += s.length;
  for (let i = 0; i < s.length; i += 97) {
    h = (h * 31 + s.charCodeAt(i)) % 1000000007;
  }
}
console.log(`total=${total} length=${s.length} hash=${h} tail=${s.slice(s.length - 6)}`);

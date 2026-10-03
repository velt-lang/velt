// Sort (same workload as sort.vlt): `sort()` without a comparator on 200k generated words that
// mix ASCII, accented Latin, CJK and emoji.
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

const ALPHABET = "a b c d e k m s z A Z é è ü ç ñ ß Å 日 本 語 中 文 한 글 😀 🎉 🚀 🔥 👍🏽 ﬁ €";

const rng = new Rng();
const alphabet = ALPHABET.split(" ");
const words = [];
for (let i = 0; i < 200000; i++) {
  const len = 2 + rng.next(7);
  let w = "";
  for (let j = 0; j < len; j++) {
    w += rng.pick(alphabet);
  }
  words.push(w);
}
words.sort();
let h = 0;
for (const w of words) {
  h = (h * 31 + w.length) % 1000000007;
}
console.log(
  `words=${words.length} first=${words[0]} mid=${words[100000]} last=${words[199999]} hash=${h}`,
);

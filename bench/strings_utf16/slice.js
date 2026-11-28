// slice / substring / charAt / at (same workload as slice.velt) over each of three 400k-character
// texts: Latin with ~5% accented letters, CJK, and emoji-heavy: every unit's `charAt`, `at` from
// the end, short `slice`s and long `substring`s, and a parser consuming text from the front.
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

const ASCII = "abcdefghijklmnopqrstuvwxyz";
const ACCENTS = "é è à ü ö ç ñ å ø ß";
const CJK = "的 一 是 不 了 人 我 在 有 他 这 中 大 来 上 国 个 到 说 们 为 子 和 你 地 出 道 也 时 年 得 就";
const EMOJI = "😀 🎉 🚀 🌍 🔥 💡 🐍 🍕 ❤️ 👍🏽";

function latin(rng, chars) {
  const ascii = ASCII.split("");
  const accents = ACCENTS.split(" ");
  const parts = [];
  for (let i = 0; i < chars; i++) {
    if (rng.next(8) == 0) {
      parts.push(" ");
    } else if (rng.next(20) == 0) {
      parts.push(rng.pick(accents));
    } else {
      parts.push(rng.pick(ascii));
    }
  }
  return parts.join("");
}

function cjk(rng, chars) {
  const cjk = CJK.split(" ");
  const parts = [];
  for (let i = 0; i < chars; i++) {
    parts.push(rng.next(16) == 0 ? "。" : rng.pick(cjk));
  }
  return parts.join("");
}

function emoji(rng, chars) {
  const ascii = ASCII.split("");
  const emoji = EMOJI.split(" ");
  const parts = [];
  for (let i = 0; i < chars; i++) {
    parts.push(rng.next(3) == 0 ? rng.pick(emoji) : rng.pick(ascii));
  }
  return parts.join("");
}

function scan(name, s) {
  let h = 0;
  let units = 0;
  const n = s.length;
  // `charAt` of every unit, and `at` from the end.
  for (let i = 0; i < n; i++) {
    const c = s.charAt(i);
    h = (h * 31 + (c.length) + (c.charCodeAt(0))) % 1000000007;
  }
  for (let i = 1; i <= n; i += 3) {
    const c = s.at(-i) ?? "";
    h = (h * 37 + (c.charCodeAt(0))) % 1000000007;
  }
  // Short pieces with `slice`, longer ones with `substring`.
  for (let i = 0; i < n; i += 3) {
    const p = s.slice(i, i + 5);
    units += p.length;
    h = (h * 41 + (p.charCodeAt(p.length - 1))) % 1000000007;
  }
  for (let i = 0; i < n; i += 97) {
    const p = s.substring(i + 300, i);
    units += p.length;
    h = (h * 43 + (p.charCodeAt(0))) % 1000000007;
  }
  // A parser consuming 100k units from the front.
  let rest = s.slice(0, 100000);
  let steps = 0;
  while (rest.length > 0) {
    rest = rest.slice(7);
    steps++;
  }
  console.log(`${name} length=${s.length} units=${units} steps=${steps} hash=${h}`);
}

const rng = new Rng();
// A generated 50k-character block repeated 8 times keeps generation out of the measurement.
scan("latin", latin(rng, 50000).repeat(8));
scan("cjk", cjk(rng, 50000).repeat(8));
scan("emoji", emoji(rng, 50000).repeat(8));

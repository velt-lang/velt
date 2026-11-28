// for-of scan (same workload as for_of.velt): a `for (const c of s)` and a walk through the
// string iterator (a generic iterable) over each of three 400k-character texts: Latin with ~5%
// accented letters, CJK, and emoji-heavy.
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

// The characters through the string's iterator (a generic iterable), not the `for` hook.
function walk(it) {
  let h = 0;
  for (const c of it) {
    h = (h * 31 + c.length + (c > "~" ? 1 : 0)) % 1000000007;
  }
  return h;
}

function scan(name, s) {
  let h = 0;
  let chars = 0;
  let wide = 0;
  for (const c of s) {
    h = (h * 31 + c.length) % 1000000007;
    chars++;
    if (c > "~") {
      wide++;
    }
  }
  const w = walk(s);
  console.log(`${name} length=${s.length} chars=${chars} wide=${wide} hash=${h} walk=${w}`);
}

const rng = new Rng();
// A generated 50k-character block repeated 8 times keeps generation out of the measurement.
scan("latin", latin(rng, 50000).repeat(8));
scan("cjk", cjk(rng, 50000).repeat(8));
scan("emoji", emoji(rng, 50000).repeat(8));

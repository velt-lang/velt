// Node twin of char_code_at_translated.velt: the types go, `main` is a block, and an
// out-of-range `charCodeAt` (NaN) counts as Velt's -1.

const BRAND = Symbol("brand");

/** Has `v` no member keyed by `BRAND` (a symbol the program made)? */
function noBrand(v) {
  return v[BRAND] === undefined;
}

/** A checksum of the code units `s.charCodeAt(i)` gives at each `i` of `order`. */
function sum(s, order) {
  let h = 7;
  for (const i of order) {
    const c = s.charCodeAt(i);
    h = (h * 31 + (Number.isNaN(c) ? -1 : c) + 2) % 1000000007;
  }
  return h;
}

function forward(n) {
  const out = [];
  for (let i = -2; i < n + 2; i++) {
    out.push(i);
  }
  return out;
}

{
  const hi = String.fromCharCode(0xd83d);
  const lo = String.fromCharCode(0xde00);
  const pieces = ["é", "日本", "😀", "a", hi + "x", "y" + lo, lo + hi, "𝄞z"];
  let heap = "";
  for (let i = 0; i < 400; i++) {
    heap += pieces[i % pieces.length];
  }
  const literal = "naïve café — 日本語のテキスト 😀🎉 𝄞 and some ASCII past the stride of breadcrumbs, again: naïve café — 日本語のテキスト 😀🎉 𝄞";
  for (const s of [heap, literal, heap.slice(3), heap.slice(5, 300), literal.slice(1)]) {
    const n = s.length;
    const fwd = forward(n);
    const back = fwd.slice().reverse();
    const jumps = [];
    for (let k = 0; k < n; k++) {
      jumps.push((k * 97) % n);
    }
    console.log(n, sum(s, fwd), sum(s, back), sum(s, jumps));
    console.log(s.codePointAt(0), s.codePointAt(n - 1), s.codePointAt(Math.floor(n / 2)));
  }
  // Strings made and freed while another is read: the translation of each stays right.
  let total = 0;
  for (let round = 0; round < 20; round++) {
    let t = "";
    for (let i = 0; i < 100 + round; i++) {
      t += pieces[(i + round) % pieces.length];
    }
    for (let i = 0; i < t.length; i += 3) {
      total = (total * 7 + t.charCodeAt(i)) % 1000003;
    }
    total = (total + heap.charCodeAt(round * 13)) % 1000003;
  }
  console.log(total);
  // Keys through `any`: a digit first is an index; a non-ASCII first character is not.
  const arr = [5, 6];
  const keys = ["0", "1", "é", "日", "😀", hi, "", "1é"];
  console.log(keys.map((k) => k in arr).join(","));
  const o = { "1": "one", "é": "e", "😀": "smile" };
  console.log(keys.map((k) => k in o).join(","));
  // A symbol's key on values without fields: the glue tests its first character.
  const tup = [1, "é"];
  console.log(noBrand(tup), noBrand((x) => x + 1), noBrand(true), noBrand(3.5));
}

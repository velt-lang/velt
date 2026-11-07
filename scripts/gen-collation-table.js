// Generates std/lang/string/collate_table.velt (the weights `localeCompare` uses) from the CLDR
// root collation as Node's ICU ships it (`Intl.Collator("und")`), and the letters and digits
// outside the table from Unicode 17.0 (`ALNUM_UNICODE`, what Rust 1.99's `char::is_alphanumeric`
// used when velt_rt had this collation), which Node 24 does not have yet, so in two steps:
//   bun scripts/gen-collation-table.js --alnum > alnum.txt      (any engine with Unicode 17.0)
//   node scripts/gen-collation-table.js alnum.txt > std/lang/string/collate_table.velt
// The weights are those of ICU 77.1 (Node 24); a newer ICU orders some characters differently.
const ALNUM_UNICODE = "17.0";
const base = new Intl.Collator("und", { sensitivity: "base" });
const accent = new Intl.Collator("und", { sensitivity: "accent" });
const variant = new Intl.Collator("und", { sensitivity: "variant" });
// Basic Latin through Latin Extended-B, Greek and Coptic, Cyrillic, Latin Extended Additional,
// General Punctuation, Currency Symbols.
const BLOCKS = [
  [0x20, 0x24f], [0x370, 0x3ff], [0x400, 0x4ff], [0x1e00, 0x1eff], [0x2000, 0x206f], [0x20a0, 0x20cf],
];
const chars = [];
for (const [lo, hi] of BLOCKS) for (let c = lo; c <= hi; c++) chars.push(String.fromCodePoint(c));
const ignorable = (c) => base.compare("x" + c, "x") === 0 && accent.compare("x" + c, "x") === 0 && variant.compare("x" + c, "x") === 0;
const letters = "abcdefghijklmnopqrstuvwxyz";
// Two-character expansions: letter pairs (`æ` = `ae`) and punctuation pairs (`‼` = `!!`).
const pairs = [];
for (const x of letters) for (const y of letters) pairs.push(x + y);
const puncts = "!?.";
for (const x of puncts) for (const y of puncts) pairs.push(x + y);
const info = new Map();
const singles = [];
const marks = [];
for (const c of chars) {
  if (ignorable(c)) { info.set(c, { ignorable: true }); continue; }
  // Combining marks: no primary weight, only an accent (secondary) weight.
  if (base.compare("x" + c, "x") === 0) { info.set(c, { mark: true }); marks.push(c); continue; }
  // Expansion to two ASCII letters?
  let exp = null;
  if (!letters.split("").some((x) => base.compare(c, x) === 0)) {
    for (const p of pairs) if (base.compare(c, p) === 0) { exp = p; break; }
  }
  if (exp) info.set(c, { exp });
  else singles.push(c);
}
// Primary classes of single characters.
singles.sort((a, b) => base.compare(a, b));
let cls = 0;
for (let i = 0; i < singles.length; i++) {
  if (i === 0 || base.compare(singles[i - 1], singles[i]) !== 0) cls++;
  info.set(singles[i], { p: cls });
}
if (cls >= 4095) throw new Error("too many classes");
const primaryOf = (s) => [...s].map((c) => info.get(c).p);
// Secondary/tertiary ranks within each group sharing the same primaries (expansions join the
// group of their two-letter base, whose plain form has rank 0).
const groups = new Map();
for (const c of chars) {
  const e = info.get(c);
  if (e.ignorable || e.mark) continue;
  const ps = e.exp ? primaryOf(e.exp) : [e.p];
  e.ps = ps;
  const key = ps.join(",");
  if (!groups.has(key)) groups.set(key, []);
  groups.get(key).push(c);
}
for (const [key, members] of groups) {
  const isExp = key.includes(",");
  const ref = isExp ? [...members].map((c) => info.get(c).exp)[0] : null;
  const all = isExp ? [ref, ...members] : members;
  const bySec = [...all].sort((a, b) => accent.compare(a, b));
  let sec = 0;
  const secOf = new Map();
  for (let i = 0; i < bySec.length; i++) {
    if (i > 0 && accent.compare(bySec[i - 1], bySec[i]) !== 0) sec++;
    secOf.set(bySec[i], sec);
  }
  for (const c of members) {
    const e = info.get(c);
    e.sec = secOf.get(c);
    const peers = all.filter((x) => secOf.get(x) === e.sec).sort((a, b) => variant.compare(a, b));
    let ter = 0;
    for (let i = 0; i < peers.length; i++) {
      if (i > 0 && variant.compare(peers[i - 1], peers[i]) !== 0) ter++;
      if (peers[i] === c) break;
    }
    e.ter = ter;
    if (e.sec > 255 || e.ter > 15) throw new Error("rank overflow " + c);
  }
}
// Marks rank by the accent they give a letter, after rank 0 (no accent).
marks.sort((a, b) => accent.compare("a" + a, "a" + b));
for (let i = 0, rank = 1; i < marks.length; i++) {
  if (i > 0 && accent.compare("a" + marks[i - 1], "a" + marks[i]) !== 0) rank++;
  info.get(marks[i]).sec = Math.min(rank, 255);
}
const word = (c) => {
  const e = info.get(c);
  if (e.ignorable) return "0x0";
  if (e.mark) return "0x" + (BigInt(e.sec) << 24n).toString(16);
  // collate.velt's orderAt reads a character whose first primary is 0 as ignorable at the first
  // level, second element included.
  if (!e.ps[0] && e.ps[1]) {
    throw new Error(`${hex(c.codePointAt(0))}: a second primary without a first`);
  }
  const v = BigInt(e.ps[0]) | (BigInt(e.ps[1] ?? 0) << 12n) | (BigInt(e.sec) << 24n) | (BigInt(e.ter) << 32n);
  return "0x" + v.toString(16);
};
const v = process.versions;
// The letters and digits outside the table, as an inversion list: a code point is one when an
// odd number of boundaries are at or below it. Code points whose state doesn't matter (the
// table's, the marks, the three scripts ranked by range in `CLASSES`) take the state of the one
// before, so they add no boundaries.
const skipped = [
  [0x300, 0x36f], [0x1100, 0x11ff], [0x3040, 0x30ff], [0x3130, 0x318f], [0x31f0, 0x31ff],
  [0x3400, 0x4dbf], [0x4e00, 0x9fff], [0xac00, 0xd7af], [0xf900, 0xfaff], [0x20000, 0x323af],
  ...BLOCKS,
];
// One per line: the Unicode version, then the boundaries.
const alnumLines = () => {
  if (process.versions.unicode !== ALNUM_UNICODE) {
    throw new Error(`the letters and digits need Unicode ${ALNUM_UNICODE}, this engine has ${process.versions.unicode}`);
  }
  const alnum = /^[\p{Alphabetic}\p{N}]$/u;
  const out = [ALNUM_UNICODE];
  let inside = false;
  for (let c = 0x20; c <= 0x10ffff; c++) {
    if (c >= 0xd800 && c <= 0xdfff) continue;
    if (skipped.some(([lo, hi]) => c >= lo && c <= hi)) continue;
    if (alnum.test(String.fromCodePoint(c)) !== inside) {
      out.push("0x" + c.toString(16));
      inside = !inside;
    }
  }
  return out;
};
if (process.argv[2] === "--alnum") {
  console.log(alnumLines().join("\n"));
  process.exit(0);
}
const [alnumUnicode, ...boundaries] = process.argv[2]
  ? require("fs").readFileSync(process.argv[2], "utf8").trim().split(/\r?\n/)
  : alnumLines();
if (alnumUnicode !== ALNUM_UNICODE) throw new Error(`${process.argv[2]} is from Unicode ${alnumUnicode}`);
const hex = (n) => "0x" + n.toString(16);
// The tables are strings of 7-bit values (ASCII), which a program reads byte by byte with no
// bounds check and which cost the compiler a literal each, not an array of numbers.
const literal = (bytes) => {
  let s = "";
  for (const b of bytes) {
    if (b < 0 || b > 0x7f) throw new Error("not 7 bits: " + b);
    const c = String.fromCharCode(b);
    s += b < 0x20 || b === 0x7f || c === '"' || c === "\\" ? "\\x" + b.toString(16).padStart(2, "0") : c;
  }
  return `"${s}"`;
};
// `n` bytes of 7 bits per value, the low bits first.
const septets = (values, n) => values.flatMap((x) => {
  const v = BigInt(x);
  if (v >> BigInt(7 * n)) throw new Error(`${x} needs more than ${7 * n} bits`);
  return Array.from({ length: n }, (_, k) => Number((v >> BigInt(7 * k)) & 0x7fn));
});
// As `velt fmt` lays out a constant too long for one line.
const constant = (doc, name, type, value) => {
  const one = `export const ${name}: ${type} = ${value};`;
  console.log(doc);
  console.log(one.length <= 100 ? one : `export const ${name}: ${type} =\n  ${value};`);
};
console.log(`// velt/lang/string (internal): the weights of \`localeCompare\` (std/lang/string/collate.velt),
// generated by scripts/gen-collation-table.js: do not edit.
//
// From the CLDR root collation (\`Intl.Collator("und")\`; ICU ${v.icu}, Unicode ${v.unicode}, CLDR ${v.cldr}), for
// U+0020..=U+024F (Basic Latin through Latin Extended-B), U+0370..=U+04FF (Greek and Coptic,
// Cyrillic), U+1E00..=U+1EFF (Latin Extended Additional), U+2000..=U+206F (General Punctuation)
// and U+20A0..=U+20CF (Currency Symbols). One word per character packs primary 1 (bits 0..12),
// primary 2 (12..24, for expansions such as \`æ\` = \`ae\`), secondary (24..32) and tertiary
// (32..36) ranks. A zero word is an ignorable character (controls); a combining mark has no
// primary, only a secondary rank. The tables are strings of 7-bit values, a value \`n\` bytes of
// 7 bits each, the low bits first.
//
// Derived from Unicode CLDR root collation data, © Unicode, Inc., Unicode License v3
// (https://www.unicode.org/license.txt).
`);
constant("/** The highest primary rank in the table. */", "LAST_PRIMARY", "u64", cls);
console.log();
constant("/** The lowest digit's primary rank (`0`). */", "ZERO_PRIMARY", "u64", info.get("0").ps[0]);
console.log();
// The words of the blocks, in order, and per 16 code points (a page) where its words start.
const words = [];
const end = BLOCKS[BLOCKS.length - 1][1] + 1;
const pages = new Array(end / 16).fill(0);
for (const [lo, hi] of BLOCKS) {
  if (lo % 16 || (hi + 1) % 16) throw new Error(`block ${hex(lo)} is not whole pages`);
  for (let c = lo; c <= hi; c++) {
    if (c % 16 === 0) pages[c / 16] = words.length / 16 + 1;
    words.push(word(String.fromCodePoint(c)));
  }
}
constant("/** The code points below this one have a page in `PAGES`. */", "PAGES_END", "u64", hex(end));
console.log();
constant(`/**
 * Per page of 16 code points: 0 if the table doesn't hold it, else 1 + where its words start in
 * \`WEIGHTS\`, over 16.
 */`, "PAGES", "string", literal(pages));
console.log();
constant("/** The words, 5 bytes each. */", "WEIGHTS", "string", literal(septets(words, 5)));
console.log();
// The classes of the code points outside the table, as runs: 0 for a symbol, 1 for a letter or
// digit (from the inversion list `boundaries`), then the scripts the root collation orders last:
// 2 for Hangul, 3 for kana, 4 for Han. Code points never looked up (the table's, the marks, the
// controls) continue the run before, so they add no runs.
const SCRIPTS = [
  [0x1100, 0x11ff, 2], [0x3130, 0x318f, 2], [0xac00, 0xd7af, 2], [0x3040, 0x30ff, 3], [0x31f0, 0x31ff, 3],
  [0x3400, 0x4dbf, 4], [0x4e00, 0x9fff, 4], [0xf900, 0xfaff, 4], [0x20000, 0x323af, 4],
];
const bounds = boundaries.map((x) => parseInt(x, 16));
const runs = [];
let inside = false, next = 0, last = -1;
for (let c = 0x20; c <= 0x10ffff; c++) {
  while (next < bounds.length && bounds[next] <= c) { inside = !inside; next++; }
  if (c >= 0xd800 && c <= 0xdfff) continue;
  if (c >= 0x300 && c <= 0x36f) continue;
  if (BLOCKS.some(([lo, hi]) => c >= lo && c <= hi)) continue;
  const script = SCRIPTS.find(([lo, hi]) => c >= lo && c <= hi);
  const k = script ? script[2] : inside ? 1 : 0;
  if (k !== last) { runs.push(c * 8 + k); last = k; }
}
constant("/** How many runs `CLASSES` holds. */", "CLASS_RUNS", "u64", runs.length);
console.log();
constant(`/**
 * The classes of the code points outside the table, as runs, 3 bytes each: a run's first code
 * point times 8 plus its class, which holds up to the next run's: 0 for a symbol, 1 for a letter
 * or digit (Unicode ${alnumUnicode}), then the scripts the root collation orders last, 2 for Hangul,
 * 3 for kana and 4 for Han.
 */`, "CLASSES", "string", literal(septets(runs, 3)));

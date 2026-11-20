// Generates tests/std/text/regex_node_cases.velt: patterns and subjects with what Node finds,
// for the differential test tests/std/text/regex_node.test.velt.
//
//   node scripts/gen-regex-cases.js > tests/std/text/regex_node_cases.velt
//
// A case is a line `pattern flags subject matches split`: the pattern and the subject as the hex
// of their UTF-16 code units (`-` for none), the flags (`-` for none), every match of
// `subject.matchAll` with the `g` flag as `index-end` followed by `|` and a group's hex (`~`
// for one that did not take part), `;` between matches, and the pieces of `subject.split` as
// hex with `,` between them (`~` for a group that did not take part). The patterns are a fixed
// list and ones drawn from a small grammar by a seeded generator, without lookaround and back
// references, which Velt does not support.
let seed = 0x2f6e2b1;
function random(n) {
  seed = (Math.imul(seed, 1103515245) + 12345) >>> 0;
  return Math.floor((seed / 0x100000000) * n);
}
const pick = (xs) => xs[random(xs.length)];

const CHARS = ["a", "b", "c", "x", "A", "B", "1", "2", " ", "-", "_", "\n", "é", "É", "ſ", "\u{1F600}", " ", "k", "K"];
const ATOMS = ["a", "b", "c", "x", "A", "1", " ", "-", "é", "\u{1F600}", ".", "\\d", "\\w", "\\s", "\\D", "\\W", "\\S",
  "[ab]", "[^a]", "[a-c]", "[^\\d]", "[\\w-]", "[x\\s]", "[é-ÿ]", "[^]", "[]", "\\u0041", "\\x62", "\\n", "\\-", "k", "ſ"];
const ASSERTS = ["^", "$", "\\b", "\\B"];
const QUANTS = ["*", "+", "?", "{2}", "{1,2}", "{0,1}", "{2,}", "*?", "+?", "??", "{1,2}?"];

let named = 0;
function atom(depth) {
  const r = random(10);
  if (depth > 0 && r < 2) return `(${alt(depth - 1)})`;
  if (depth > 0 && r < 3) return `(?:${alt(depth - 1)})`;
  if (depth > 0 && r < 4) return `(?<n${named++}>${alt(depth - 1)})`;
  return pick(ATOMS);
}
function term(depth) {
  if (random(12) === 0) return pick(ASSERTS);
  const a = atom(depth);
  return random(3) === 0 ? a + pick(QUANTS) : a;
}
function seq(depth) {
  let s = "";
  for (let n = random(4); n >= 0; n--) s += term(depth);
  return s;
}
function alt(depth) {
  let s = seq(depth);
  for (let n = random(6) === 0 ? 1 + random(2) : 0; n > 0; n--) s += "|" + (random(8) === 0 ? "" : seq(depth));
  return s;
}
function subject() {
  let s = "";
  for (let n = random(13); n > 0; n--) s += pick(CHARS);
  return s;
}

const FIXED = [
  ["(a*)*", "", ["b", "aab"]], ["(a*)+", "", ["b", "aab"]], ["(?:(a)|b)*", "", ["ab", "ba", "abab"]],
  ["(a|ab)(c|bcd)(d*)", "", ["abcd"]], ["(a+)(b+)?", "", ["aab", "aac"]], ["x*", "", ["axxb", "", "\u{1F600}"]],
  ["", "", ["abc", "", "\u{1F600}é"]], ["(?:)", "u", ["\u{1F600}é"]], ["\\b", "", ["ab cd", "é a"]],
  ["^", "m", ["a\nb\r\nc d"]], ["$", "m", ["a\nb\r\nc d"]], [".", "", ["\u{1F600}", "a\n\r b"]],
  [".", "s", ["a\n"]], [".", "u", ["\u{1F600}\ud83dx"]], ["[\\uD800-\\uDBFF]", "", ["\u{1F600}a"]],
  ["[\\uDC00-\\uDFFF]", "", ["\u{1F600}a\udc00"]], ["\\uD83D", "u", ["\u{1F600}\ud83d"]],
  ["\\uD83D\\uDE00", "u", ["\u{1F600}"]], ["\\u{1F600}", "u", ["a\u{1F600}"]], ["\u{1F600}+", "", ["\u{1F600}\ude00"]],
  ["[^a]", "", ["\u{1F600}"]], ["[\u{1F600}]", "", ["\u{1F600}"]], ["[\u{1F600}]", "u", ["\u{1F600}"]],
  ["\\w+", "iu", ["aſKb"]], ["\\w+", "i", ["aſKb"]], ["[k-l]+", "iu", ["KKkL"]], ["\\W+", "iu", ["ſ- K"]],
  ["\\p{Lu}+", "u", ["ABcÉ"]], ["\\P{L}+", "u", ["ab12 é"]], ["[\\p{Nd}_]+", "u", ["a1_٣b"]], ["\\p{Lu}", "iu", ["a"]],
  ["\\p{sc=Grek}+", "u", ["abγδε"]], ["straSSe", "i", ["STRASSE strasse"]], ["σ+", "iu", ["ΣσςΣ"]], ["σ+", "i", ["ΣσςΣ"]],
  ["a{2,3}", "", ["aaaaaaa"]], ["a{2,3}?", "", ["aaaaaaa"]], ["(a){0}b", "", ["ab"]], ["(?:a|b)+?c", "", ["abac"]],
  ["\\d{4}-\\d\\d", "", ["on 2024-02-29 and 2025-01"]], ["(?<y>\\d{4})-(?<m>\\d\\d)", "", ["2024-02"]],
  ["\\s*[,;]\\s*", "", ["a , b;c ,d"]], ["a|ab|abc", "", ["abc"]], ["abc|ab|a", "", ["abc"]], ["(a|b)*c|(a|ab)*c", "", ["abc"]],
  ["x{", "", ["x{"]], ["a{,3}", "", ["a{,3}"]], ["\\k", "", ["k"]], ["\\1", "", ["\u0001"]], ["\\8", "", ["8"]],
  ["\\c", "", ["\\c"]], ["[\\c_]", "", ["\u001f"]], ["\\ca", "", ["\u0001"]], ["[\\b]", "", ["\b"]], ["\\0", "", ["\u0000"]],
  ["\\x4", "", ["x4"]], ["\\u00", "", ["u00"]], ["]", "", ["]"]], ["[a-\\d]", "", ["a-1"]], ["\\z", "", ["z"]],
  ["agggtaaa|tttaccct", "", ["ttagggtaaatttaccctag"]], ["[cgt]gggtaaa|tttaccc[acg]", "i", ["TGGGTAAAtttacccg"]],
  ["foo|foobar|ba", "", ["xfoobar ba"]], ["ab|a", "", ["\u{1F600}ab\u{1F600}a"]], ["é|e", "", ["eée"]],
];

const hex = (s) => (s === "" ? "-" : [...Array(s.length)].map((_, i) => s.charCodeAt(i).toString(16)).join("."));
const lines = [];
const seen = new Set();
function add(pattern, flags, s) {
  let re;
  try {
    re = new RegExp(pattern, flags + "g");
  } catch {
    return;
  }
  const key = `${pattern}/${flags}/${s}`;
  if (seen.has(key)) return;
  seen.add(key);
  const matches = [...s.matchAll(re)].map((m) =>
    `${m.index}-${m.index + m[0].length}` + m.slice(1).map((g) => "|" + (g === undefined ? "~" : hex(g))).join(""));
  const pieces = s.split(new RegExp(pattern, flags)).map((p) => (p === undefined ? "~" : hex(p)));
  lines.push([hex(pattern), flags || "-", hex(s), matches.join(";") || "-", pieces.join(",") || "-"].join(" "));
}
for (const [pattern, flags, subjects] of FIXED) for (const s of subjects) add(pattern, flags, s);
while (lines.length < 1500) {
  named = 0;
  const pattern = alt(2);
  const flags = ["", "", "i", "m", "s", "u", "iu", "im", "su", "imsu"][random(10)];
  for (let n = 0; n < 2; n++) add(pattern, flags, subject());
}

// Long subjects, which Velt matches with another matcher than short ones: a short subject
// repeated, and of its matches only how many there are and a checksum of where (and of the
// length of each group). A pattern V8 backtracks on for more than a second is left out.
const vm = require("vm");
function digest(re, s) {
  let h = 7;
  let count = 0;
  for (const m of s.matchAll(re)) {
    count++;
    h = (h * 31 + m.index) % 1000000007;
    h = (h * 31 + m[0].length) % 1000000007;
    for (let g = 1; g < m.length; g++) h = (h * 31 + (m[g] === undefined ? 1 : m[g].length + 2)) % 1000000007;
  }
  return `${count} ${h}`;
}
const long = [];
const REPEAT = 4000;
while (long.length < 120) {
  named = 0;
  const pattern = alt(2);
  const flags = ["", "", "i", "m", "s", "u", "iu", "im", "su", "imsu"][random(10)];
  let s = subject();
  while (s.length < 4) s += subject();
  let re;
  try {
    re = new RegExp(pattern, flags + "g");
  } catch {
    continue;
  }
  try {
    const found = vm.runInNewContext("digest(re, s.repeat(n))", { digest, re, s, n: REPEAT }, { timeout: 1000 });
    long.push([hex(pattern), flags || "-", hex(s), found].join(" "));
  } catch {
    continue;
  }
}

function literal(text) {
  const chunks = [];
  for (let i = 0; i < text.length; ) {
    let end = Math.min(i + 92, text.length);
    if (text[end - 1] === "\\") end--;
    chunks.push(text.slice(i, end));
    i = end;
  }
  return chunks.join("\\\n");
}
process.stdout.write(`// Cases of tests/std/text/regex_node.test.velt, generated by scripts/gen-regex-cases.js (which
// says how a case is written) with Node ${process.version}: do not edit.

/** The cases, one per line. */
export const CASES: string =
  "${literal(lines.join("\\n"))}";

/** How many times a long case repeats its subject. */
export const REPEAT: number = ${REPEAT};

/** The cases with long subjects, one per line: \`pattern flags subject matches checksum\`. */
export const LONG_CASES: string =
  "${literal(long.join("\\n"))}";
`);

// Generates std/text/regex/unicode_tables.velt: the sets of the Unicode property escapes
// (`\p{L}`, `\p{Script=Greek}`) and the characters that match each other under the `i` flag.
//
//   node scripts/gen-regex-tables.js PropertyValueAliases.txt CaseFolding.txt \
//     > std/text/regex/unicode_tables.velt
//
// The two files are the Unicode Character Database's, of the Unicode version of the Node that
// runs this (`process.versions.unicode`; https://www.unicode.org/Public/<version>/ucd/). They
// give the names (General_Category and Script values with their aliases) and simple case
// folding. Each set is what Node's own regular expressions match, asked one code point at a
// time, so the tables are Node's by construction.
const fs = require("fs");
if (process.argv.length < 4) {
  console.error("usage: node scripts/gen-regex-tables.js PropertyValueAliases.txt CaseFolding.txt");
  process.exit(2);
}
const aliasText = fs.readFileSync(process.argv[2], "utf8");
const foldText = fs.readFileSync(process.argv[3], "utf8");
const version = /-(\d+\.\d+)\.\d+\.txt/.exec(aliasText)[1];
if (version !== process.versions.unicode) {
  console.error(`the files are Unicode ${version}, this Node has ${process.versions.unicode}`);
  process.exit(1);
}

// ECMA-262's table of binary Unicode properties.
const BINARY = `ASCII ASCII_Hex_Digit|AHex Alphabetic|Alpha Any Assigned Bidi_Control|Bidi_C
Bidi_Mirrored|Bidi_M Case_Ignorable|CI Cased Changes_When_Case_Folded|CWCF
Changes_When_Case_Mapped|CWCM Changes_When_Lowercased|CWL Changes_When_NFKC_Casefolded|CWKCF
Changes_When_Titlecased|CWT Changes_When_Uppercased|CWU Dash Default_Ignorable_Code_Point|DI
Deprecated|Dep Diacritic|Dia Emoji Emoji_Component|EComp Emoji_Modifier|EMod
Emoji_Modifier_Base|EBase Emoji_Presentation|EPres Extended_Pictographic|ExtPict Extender|Ext
Grapheme_Base|Gr_Base Grapheme_Extend|Gr_Ext Hex_Digit|Hex IDS_Binary_Operator|IDSB
IDS_Trinary_Operator|IDST ID_Continue|IDC ID_Start|IDS Ideographic|Ideo Join_Control|Join_C
Logical_Order_Exception|LOE Lowercase|Lower Math Noncharacter_Code_Point|NChar
Pattern_Syntax|Pat_Syn Pattern_White_Space|Pat_WS Quotation_Mark|QMark Radical
Regional_Indicator|RI Sentence_Terminal|STerm Soft_Dotted|SD Terminal_Punctuation|Term
Unified_Ideograph|UIdeo Uppercase|Upper Variation_Selector|VS White_Space|space
XID_Continue|XIDC XID_Start|XIDS`.split(/\s+/);

/** The `property ; alias ; alias…` lines of `property`, as lists of names. */
function aliases(property) {
  const out = [];
  for (const line of aliasText.split("\n")) {
    const fields = line.split("#")[0].split(";").map((f) => f.trim());
    if (fields[0] === property && fields.length >= 3) {
      out.push([...new Set(fields.slice(1))]);
    }
  }
  return out;
}

/** The code points `/\p{expr}/u` matches, as `[lo, hi]` ranges; null when Node has no such
 * property. */
function ranges(expr) {
  let re;
  try {
    re = new RegExp(`^\\p{${expr}}$`, "u");
  } catch {
    return null;
  }
  const out = [];
  let lo = -1;
  for (let c = 0; c <= 0x110000; c++) {
    const hit = c <= 0x10ffff && re.test(String.fromCodePoint(c));
    if (hit && lo < 0) lo = c;
    if (!hit && lo >= 0) {
      out.push([lo, c - 1]);
      lo = -1;
    }
  }
  return out;
}

// A number is base-32 digits, the low ones first: `#`..`B` for a digit more follow, `]`..`|`
// for the last one. Neither run holds a quote or a backslash.
function number(n) {
  let s = "";
  while (n >= 32) {
    s += String.fromCharCode(0x23 + (n & 31));
    n = Math.floor(n / 32);
  }
  return s + String.fromCharCode(0x5d + n);
}

/** Ranges as numbers: each one's distance from the end of the one before, and its length. */
function encodeRanges(list) {
  let s = "";
  let next = 0;
  for (const [lo, hi] of list) {
    s += number(lo - next) + number(hi - lo);
    next = hi + 1;
  }
  return s;
}

const names = { gc: [], binary: [], sc: [] };
const tables = [];
for (const group of aliases("gc")) {
  const r = ranges(`gc=${group[0]}`);
  if (r) {
    names.gc.push(group.join("|"));
    tables.push(encodeRanges(r));
  }
}
for (const entry of BINARY) {
  const r = ranges(entry.split("|")[0]);
  if (r) {
    names.binary.push(entry);
    tables.push(encodeRanges(r));
  }
}
const scripts = aliases("sc").filter((group) => ranges(`sc=${group[0]}`) !== null);
for (const group of scripts) {
  names.sc.push(group.join("|"));
  tables.push(encodeRanges(ranges(`sc=${group[0]}`)));
}
// Script_Extensions of a script: what it adds to the script's own characters (`+`), or, when
// it does not hold them all (Common, Inherited), the whole set.
for (const group of scripts) {
  const own = ranges(`sc=${group[0]}`);
  const ext = ranges(`scx=${group[0]}`);
  const inExt = new Set();
  for (const [lo, hi] of ext) for (let c = lo; c <= hi; c++) inExt.add(c);
  const inOwn = new Set();
  for (const [lo, hi] of own) for (let c = lo; c <= hi; c++) inOwn.add(c);
  if (![...inOwn].every((c) => inExt.has(c))) {
    tables.push(encodeRanges(ext));
    continue;
  }
  const extra = [];
  for (const c of [...inExt].sort((a, b) => a - b)) {
    if (inOwn.has(c)) continue;
    if (extra.length && extra[extra.length - 1][1] === c - 1) extra[extra.length - 1][1] = c;
    else extra.push([c, c]);
  }
  tables.push("+" + encodeRanges(extra));
}

// Case: the classes of characters with one canonical form. With the `u` flag that is simple
// case folding; without, `toUpperCase` of one code unit when the result is one code unit and
// does not turn a character past ASCII into an ASCII one.
const simpleFold = new Map();
for (const line of foldText.split("\n")) {
  const m = /^([0-9A-F]+); [CS]; ([0-9A-F]+);/.exec(line);
  if (m) simpleFold.set(parseInt(m[1], 16), parseInt(m[2], 16));
}
function unitCanon(c) {
  const u = String.fromCharCode(c).toUpperCase();
  if (u.length !== 1) return c;
  const cu = u.charCodeAt(0);
  return c >= 128 && cu < 128 ? c : cu;
}
/** For every character with others of the same canonical form, `[c, other]` per other one. */
function relation(canon, max) {
  const classes = new Map();
  for (let c = 0; c <= max; c++) {
    const k = canon(c);
    if (!classes.has(k)) classes.set(k, []);
    classes.get(k).push(c);
  }
  const pairs = [];
  for (const members of classes.values()) {
    for (const a of members) for (const b of members) if (a !== b) pairs.push([a, b]);
  }
  return pairs.sort((x, y) => x[0] - y[0] || x[1] - y[1]);
}
/** Checks a relation against Node's own matching, pair by pair. */
function check(pairs, flags) {
  for (const [a, b] of pairs) {
    const hex = a.toString(16).padStart(4, "0");
    const re = new RegExp(flags.includes("u") ? `^\\u{${hex}}$` : `^\\u${hex}$`, flags);
    if (!re.test(String.fromCodePoint(b))) {
      throw new Error(`U+${hex} and U+${b.toString(16)} do not match with /${flags}`);
    }
  }
}
/** Pairs as runs `lo, length - 1, delta`: the characters `lo..` each match the one `delta`
 * away (zigzag: the sign in the low bit); `lo` as its distance from the run before. */
function encodePairs(pairs) {
  const runs = [];
  for (const [a, b] of pairs) {
    const last = runs[runs.length - 1];
    if (last && last.delta === b - a && last.lo + last.len === a) last.len++;
    else runs.push({ lo: a, len: 1, delta: b - a });
  }
  let s = "";
  let prev = 0;
  for (const r of runs) {
    s += number(r.lo - prev) + number(r.len - 1) + number(r.delta < 0 ? -2 * r.delta - 1 : 2 * r.delta);
    prev = r.lo;
  }
  return s;
}
const unitPairs = relation(unitCanon, 0xffff);
const pointPairs = relation((c) => simpleFold.get(c) ?? c, 0x10ffff);
check(unitPairs, "i");
check(pointPairs, "iu");

/** A string constant, split over lines of at most 96 characters. */
function constant(name, doc, text) {
  // The doc comment, wrapped as `velt fmt` leaves it.
  const words = doc.split(" ");
  const docLines = ["/**"];
  for (const w of words) {
    if ((docLines[docLines.length - 1] + " " + w).length > 96) docLines.push(" *");
    docLines[docLines.length - 1] += " " + w;
  }
  let out = `${docLines.join("\n")} */\nexport const ${name}: string =\n`;
  const lines = [];
  for (let i = 0; i < text.length || i === 0; i += 92) lines.push(text.slice(i, i + 92));
  out += lines.map((l, i) => `  "${l}${i + 1 < lines.length ? "\\" : '";'}`).join("\n");
  // A continued line must not start with spaces that would be part of the string.
  return out.replace(/\\\n {2}"/g, "\\\n");
}

const out = [];
out.push(`// velt/text (internal): the sets of Unicode property escapes (\`\\p{L}\`) and the characters that
// match each other under the \`i\` flag, generated by scripts/gen-regex-tables.js: do not edit.
//
// Unicode ${version}, as Node ${process.version} (ICU ${process.versions.icu}) matches. A number is base-32 digits,
// the low ones first: \`#\`..\`B\` for a digit more follow, \`]\`..\`|\` for the last.
// unicode_props.velt and case_fold.velt read them.
//
// Derived from the Unicode Character Database, © Unicode, Inc., Unicode License v3
// (https://www.unicode.org/license.txt).`);
out.push(constant("GC_NAMES", "The values of General_Category, `,` between them, `|` between a value's names; table `i` of `TABLES` is value `i`'s.", names.gc.join(",")));
out.push(constant("BINARY_NAMES", "The binary properties, as `GC_NAMES`; their tables follow those of General_Category.", names.binary.join(",")));
out.push(constant("SCRIPT_NAMES", "The values of Script, as `GC_NAMES`; their tables follow the binary properties', and are followed by as many for Script_Extensions.", names.sc.join(",")));
out.push(`/** How many values \`GC_NAMES\`, \`BINARY_NAMES\` and \`SCRIPT_NAMES\` hold. */
export const GC_COUNT: usize = ${names.gc.length};
export const BINARY_COUNT: usize = ${names.binary.length};
export const SCRIPT_COUNT: usize = ${names.sc.length};`);
out.push(constant("TABLES", "The tables, each ended by `!`: a set's ranges in order, each as its distance from the end of the one before and its length less one. A Script_Extensions table that starts with `+` holds what the script's own table lacks.", tables.map((t) => t + "!").join("")));
out.push(constant("UNIT_CASE", "The UTF-16 code units that match another under the `i` flag without the `u` flag: runs `lo, length - 1, delta`, each character of `lo..` matching the one `delta` away (the sign in the low bit), `lo` as its distance from the run before.", encodePairs(unitPairs)));
out.push(constant("POINT_CASE", "As `UNIT_CASE`, the code points that match another under the `i` and `u` flags.", encodePairs(pointPairs)));
process.stdout.write(out.join("\n\n") + "\n");

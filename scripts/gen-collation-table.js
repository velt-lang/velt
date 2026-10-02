// Generates crates/velt_rt/src/str_ops/collate_table.rs (the weights `localeCompare` uses) from
// ICU's root collation as Node ships it:
//   node scripts/gen-collation-table.js > crates/velt_rt/src/str_ops/collate_table.rs
const base = new Intl.Collator("und", { sensitivity: "base" });
const accent = new Intl.Collator("und", { sensitivity: "accent" });
const variant = new Intl.Collator("und", { sensitivity: "variant" });
const LO = 0x20, HI = 0x24f;
const chars = [];
for (let c = LO; c <= HI; c++) chars.push(String.fromCodePoint(c));
const ignorable = (c) => base.compare("x" + c, "x") === 0 && accent.compare("x" + c, "x") === 0 && variant.compare("x" + c, "x") === 0;
const letters = "abcdefghijklmnopqrstuvwxyz";
const pairs = [];
for (const x of letters) for (const y of letters) pairs.push(x + y);
const info = new Map();
const singles = [];
for (const c of chars) {
  if (ignorable(c)) { info.set(c, { ignorable: true }); continue; }
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
  if (e.ignorable) continue;
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
const words = [];
for (const c of chars) {
  const e = info.get(c);
  let v = 0n;
  if (!e.ignorable) {
    v = BigInt(e.ps[0]) | (BigInt(e.ps[1] ?? 0) << 12n) | (BigInt(e.sec) << 24n) | (BigInt(e.ter) << 32n);
  }
  words.push("0x" + v.toString(16).padStart(9, "0"));
}
const maxPrimary = cls;
console.log(`//! Collation weights of U+0020..=U+024F (Basic Latin through Latin Extended-B), generated from
//! ICU's root collation (Node's \`Intl.Collator("und")\`) by scripts/gen-collation-table.js: one
//! word per character, packing primary 1 (bits 0..12), primary 2 (12..24, for expansions such as
//! \`æ\` = \`ae\`), secondary (24..32) and tertiary (32..36) ranks. A zero word is an ignorable
//! character (controls).
`);
console.log(`pub(super) const LAST_PRIMARY: u32 = ${maxPrimary};`);
console.log("// Six words per line (rustfmt would put one per line).");
console.log("#[rustfmt::skip]");
console.log(`pub(super) const TABLE: [u64; ${words.length}] = [`);
for (let i = 0; i < words.length; i += 6) console.log("    " + words.slice(i, i + 6).join(", ") + ",");
console.log("];");

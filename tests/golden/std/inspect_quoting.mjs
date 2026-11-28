// The Node twin of tests/golden/std/inspect_quoting.velt.
const strs = [
  "plain", "", "it's", "say \"hi\"", "both ' and \"", "all ' \" `", "tpl ' \" ${x}", "back\\slash",
  "tab\there", "nl\nx", "cr\rx", "bs\bx", "ff\fx", "nul\u0000x", "esc\u001bx", "del\u007fx",
  "c1\u0085x\u009fy", "é ü 日本語 😀", " nbsp", "lone " + String.fromCharCode(0xd800) + " hi", "lo " + String.fromCharCode(0xdfff), "pair 😀",
  String.fromCharCode(0xd83d) + "\u0001", "ab'cd'ef'gh'ij'kl'mn'op", "aaaaaaaabbbbbbbb\"cccccccc", "x'`\"$",
  "1234567890123456789012345", "  ", "\u0080",
];
console.log(strs);
const keys = {};
const ks = ["a", "_x1", "A_9", "$", "$a", "a b", "1", "1a", "é", "", "it's", "a\nb", "Z", "_"];
for (let i = 0; i < ks.length; i++) {
  keys[ks[i]] = i;
}
console.log(keys);
const m = new Map();
m.set("k'1", "v\"1");
m.set("k\t2", "v`2");
console.log(m);
console.log([["deep", ["x'y"]]]);

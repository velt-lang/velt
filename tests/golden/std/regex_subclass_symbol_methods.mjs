// The Node twin of tests/golden/std/regex_subclass_symbol_methods.velt.
class S extends RegExp {
  [Symbol.split](s, limit) { return ["custom", s]; }
  [Symbol.match](s) { return ["custom-match"]; }
  [Symbol.replace](s, r) { return "custom-replace:" + r; }
  [Symbol.search](s) { return 42; }
}
class Plain extends RegExp {}
class Below extends Plain {
  [Symbol.matchAll](s) { return []; }
}
class Deeper extends S {}
const s = new S("a", "g");
console.log(JSON.stringify("xax".split(s)), JSON.stringify("xax".match(s)), "xax".replace(s, "-"), "xax".search(s));
console.log("xax".replaceAll(s, "+"), JSON.stringify("x,y".split(new Deeper(",", "g"))));
const held = new Deeper("a", "g");
console.log(JSON.stringify("banana".split(held)), "banana".search(held), "banana".replace(held, "o"));
console.log([..."aXbXc".matchAll(new Below("X", "g"))].length, [..."aXbXc".matchAll(new Plain("X", "g"))].length);
const p = new Plain("a", "g");
console.log(JSON.stringify("banana".split(p)), JSON.stringify("banana".match(p)), "banana".search(p), "banana".replace(p, "o"));
const re = /b/g;
console.log(JSON.stringify(re[Symbol.split]("abcabc")), re[Symbol.search]("xxb"), re[Symbol.replace]("abc", "B"));
console.log(JSON.stringify(re[Symbol.match]("abcb")));

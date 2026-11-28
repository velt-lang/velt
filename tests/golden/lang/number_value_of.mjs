// The Node twin of tests/golden/lang/number_value_of.velt; its .out is what Node 22 prints.
const n = 2.5;
console.log(n.valueOf(), (4).valueOf(), (-0.5).valueOf() + 1);
const k = 3;
const s = 200;
const w = -7;
const f = 1.5;
console.log(k.valueOf(), s.valueOf(), w.valueOf() * 2, f.valueOf());
const lit = 2;
console.log(lit.valueOf(), n.valueOf() === n, typeof n.valueOf());
const a = 2.5;
console.log(a.valueOf(), [1, 2, 3].map((x) => x.valueOf() * 10).join(","));
console.log(NaN.valueOf(), Infinity.valueOf(), (0.1 + 0.2).valueOf());

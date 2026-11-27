// The Node twin of tests/golden/lang/math_pow_table.velt. Its .out is Node 24's output (V8's
// `--use-std-math-pow`, on by default there, calls the C library's `pow`). Node 22's V8 has its
// own `pow` (fdlibm's), which differs in the last place in 34 cells: `2 ** 2.5` is
// 5.65685424949238 there, 5.656854249492381 in Node 24 and Velt.

const BASES = [
  2, 3, 7, 10, 0.5, 1.5, 2.5, 100, 1e-3, -2, -0.5, -3, 0, -0, 1, -1, NaN, Infinity, -Infinity,
];
const EXPONENTS = [
  0, -0, 1, 2, 3, 10, -1, -2, -10, 0.5, 1.5, 2.5, 3.5, 10.5, -0.5, -1.5, -2.5, 0.25, 0.3,
  1 / 3, -0.75, 1e-3, 52.5, NaN, Infinity, -Infinity,
];

function main() {
  console.log("e:", EXPONENTS.join(" "));
  for (const x of BASES) {
    let s = "";
    let differs = "";
    for (const e of EXPONENTS) {
      const r = x ** e;
      s += ` ${Object.is(r, -0) ? "-0" : `${r}`}`;
      if (!Object.is(r, Math.pow(x, e))) {
        differs += ` ${e}`;
      }
    }
    console.log(`${Object.is(x, -0) ? "-0" : `${x}`}:${s}`);
    if (differs != "") {
      console.log("  Math.pow differs at", differs);
    }
  }
}

main();

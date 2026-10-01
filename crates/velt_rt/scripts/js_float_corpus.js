// Emits "<f64 bits hex>\t<String(x)>" lines from a real JS engine, used by the ignored
// `fmt::tests::js_corpus` test to check velt_rt's float formatting. Usage:
//   node crates/velt_rt/scripts/js_float_corpus.js [count] > corpus.txt
const count = Number(process.argv[2] || 300000);
const buf = new DataView(new ArrayBuffer(8));
let s = 0x9e3779b97f4a7c15n;
const mask = (1n << 64n) - 1n;
function next() {
  s ^= (s << 13n) & mask;
  s ^= s >> 7n;
  s ^= (s << 17n) & mask;
  return s;
}
const out = [];
function emit(x) {
  buf.setFloat64(0, x);
  out.push(buf.getBigUint64(0).toString(16) + "\t" + String(x));
}
for (let i = 0; i < count; i++) {
  const r = next();
  switch (i % 4) {
    case 0: // arbitrary bit patterns
      buf.setBigUint64(0, r);
      emit(buf.getFloat64(0));
      break;
    case 1: // "human" decimals around the layout boundaries (1e-7 .. 1e22)
      emit(Number(r % 100000n) * Math.pow(10, Number(r % 30n) - 12));
      break;
    case 2: // integers of all magnitudes
      emit(Number(r >> BigInt(Number(r % 64n))));
      break;
    default: // results of arithmetic
      emit(Number(r % 1000n) / Number((r >> 20n) % 997n + 1n));
  }
}
for (const x of [0, -0, NaN, Infinity, -Infinity, 1e21, 1e-7, 1e-6, 5e-324, Number.MAX_VALUE,
  999999999999999900000, 123456789012345680000, 0.1 + 0.2]) emit(x);
process.stdout.write(out.join("\n") + "\n");

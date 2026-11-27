// The Node twin of tests/golden/std/stdout_lone_surrogates.velt: lone surrogates written as
// U+FFFD on stdout and stderr.
function main() {
  const hi = String.fromCharCode(0xd800);
  const lo = String.fromCharCode(0xdfff);
  const pair = String.fromCharCode(0xd83d, 0xde00);
  console.log("a" + hi + "b");
  console.log(lo + "x" + hi);
  console.log(hi + lo + hi);
  console.log(pair, hi, lo, pair);
  console.log("ends with " + hi);
  console.log("é" + hi + "✓" + lo + lo);
  process.stdout.write(hi + "\n");
  process.stdout.write("x" + lo);
  process.stdout.write("\n");
  console.error("err " + lo + hi + "!");
  console.error(hi);
  let big = "";
  for (let i = 0; i < 8300; i++) {
    big += "012345" + (i % 1000 === 0 ? pair : hi);
  }
  console.log(big.length);
  console.log(big);
  console.log("done");
}

main();

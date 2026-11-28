// The Node twin of tests/golden/lang/array_in_place_result.velt.
function sorted(xs) { return xs.sort(); }
function show(xs) { console.log(xs); }
function holder() { return { items: [2, 1] }; }
function main() {
  const xs = [3, 1, 2];
  show(xs.reverse());
  console.log(xs.sort());
  console.log(sorted(["b", "a"]), [1, 2].sort().reverse());
  console.log(holder().items.reverse());
  const ys = xs.reverse();
  ys.push(9);
  console.log(xs, ys, xs.length);
  const zs = [1, 2, 3, 4].copyWithin(0, 2);
  console.log(zs);
  console.log(zs.copyWithin(1, 0));
  const filled = new Array(3).fill(7);
  console.log(filled);
  console.log(filled.fill(0, 1).join("-"));
  const h = holder();
  const kept = h.items.sort((a, b) => b - a);
  kept.push(0);
  console.log(h.items, kept === h.items);
  const words = "c,a,b".split(",").sort().reverse();
  console.log(words);
  const bytes = [1, 2, 3, 4];
  console.log(bytes.fill(9, 2), bytes.copyWithin(0, 2).length);
}
main();

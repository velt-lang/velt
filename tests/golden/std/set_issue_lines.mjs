// The lines of #3820: a Map as the set algebra's other, a set changed during its for...of, and
// -0 stored as +0.
function main() {
  const a = new Set([1, 2, 3]);
  const m = new Map([
    [2, "two"],
    [7, "seven"],
  ]);
  console.log(
    [...a.union(m)].join(","),
    [...a.intersection(m)].join(","),
    a.isSubsetOf(m),
    a.isDisjointFrom(m),
  );
  const s = new Set([1, 2, 3]);
  const seen = [];
  for (const x of s) {
    seen.push(x);
    if (x == 1) {
      s.delete(2);
      s.add(7);
    }
  }
  console.log(seen.join(","));
  console.log(
    1 / [...new Set([-0])][0],
    1 / [...new Map([[-0, 1]]).keys()][0],
  );
}
main();

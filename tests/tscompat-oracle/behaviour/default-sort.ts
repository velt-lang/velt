// JavaScript's `sort()` compares numbers as text: Velt prints [ 1, 9, 10 ], JavaScript
// [ 1, 10, 9 ]. The fix passes a comparator.

export function main() {
  const xs = [10, 9, 1];
  xs.sort();
  console.log(xs);
}
